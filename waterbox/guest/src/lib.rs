// The Ruffle waterbox guest (M1): ruffle_core driven from inside the sandbox
// through the chimera export surface.
//
// Same shape as the native reference (waterbox/run-native): null backends, a
// log backend that captures ActionScript trace(), autoplay, the movie's own
// frame rate. The point of the milestone is that the trace produced here is
// byte-identical to the native one - determinism the sandbox enforces rather
// than merely hopes for.
//
// The SWF is handed over directly: the host asks for a buffer (AllocSwf) and
// copies the bytes into guest memory, which it can do while activated. Reading
// it through the VFS with std::fs would be the natural thing, but Rust's std
// file IO currently comes back with a corrupted descriptor inside the sandbox
// (musl's open returns 3, File::as_raw_fd reports garbage; reproducible in a
// ten-line guest, unrelated to ruffle). That is its own bug to chase - see
// docs/PLAN.md - and nothing in M1 needs a filesystem.
// The captured trace is exposed as the machine's TTY (GetTty/GetTtySize), the
// way every chimera core hands text back to the frontend.
#![no_main]

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use ruffle_core::backend::audio::{
    swf, AudioBackend, AudioMixer, DecodeError, RegisterError, SoundHandle, SoundInstanceHandle,
    SoundStreamInfo, SoundTransform,
};
use ruffle_core::backend::log::LogBackend;
use ruffle_core::impl_audio_mixer_backend;
use ruffle_core::events::{KeyDescriptor, KeyLocation, LogicalKey};
use ruffle_core::limits::ExecutionLimit;
use ruffle_core::tag_utils::SwfMovie;
use ruffle_core::{FloatDuration, Player, PlayerBuilder, PlayerEvent};

mod input_table;
mod navigator;
use input_table::{Btn, BUTTONS, BUTTON_COUNT, SHIFT_LEFT, SHIFT_RIGHT};
use navigator::{run_tasks, GuestNavigator, Tasks};

/// Captures ActionScript trace() into a buffer the host can read back.
#[derive(Clone)]
struct CaptureLog {
    out: Rc<RefCell<Vec<u8>>>,
}
impl LogBackend for CaptureLog {
    fn avm_trace(&self, message: &str) {
        let mut b = self.out.borrow_mut();
        b.extend_from_slice(message.as_bytes());
        b.push(b'\n');
    }
    fn avm_warning(&self, _message: &str) {}
}

/// Ruffle's software mixer, driven a frame at a time - the same shape as the
/// TestAudioBackend ruffle's own harness uses, so the corpus's amplitude
/// assertions are a valid oracle. The player sizes the buffer through
/// set_frame_rate when the movie loads, and tick() mixes one frame into it.
/// The buffer is shared with the machine, which hands it to the host as i16.
struct WaterboxAudio {
    mixer: AudioMixer,
    buffer: Rc<RefCell<Vec<f32>>>,
}
impl WaterboxAudio {
    const CHANNELS: u8 = 2;
    const RATE: u32 = 44100;
}
impl AudioBackend for WaterboxAudio {
    impl_audio_mixer_backend!(mixer);
    fn play(&mut self) {}
    fn pause(&mut self) {}
    fn set_frame_rate(&mut self, frame_rate: f64) {
        // Whole stereo frames. Ruffle's own harness rounds the INTERLEAVED
        // sample count, which can be odd (24 fps: 88200/24 = 3675, a lone left
        // sample); a host can only take whole frames, and the mixer should not
        // be handed half of one either. Rounding the frame count is at most one
        // sample per frame away from the harness and identical on both sides.
        let frames = (Self::RATE as f64 / frame_rate).round() as usize;
        self.buffer.borrow_mut().resize(frames * Self::CHANNELS as usize, 0.0);
    }
    fn tick(&mut self) {
        let mut b = self.buffer.borrow_mut();
        if !b.is_empty() {
            self.mixer.mix::<f32>(b.as_mut());
        }
    }
}

struct Machine {
    player: Arc<Mutex<Player>>,
    frame_time: FloatDuration,
    trace: Rc<RefCell<Vec<u8>>>,
    /// The trace flattened for the host: GetTty hands out a pointer into this,
    /// so it must outlive the call and not move while the host reads it.
    tty: Vec<u8>,
    frames: u64,
    /// Levels the host set for this frame (SetButton/SetAxis) and what the
    /// guest last injected, so a change becomes exactly one edge event.
    buttons: [u8; BUTTON_COUNT],
    prev_buttons: [u8; BUTTON_COUNT],
    mouse: (i32, i32),
    prev_mouse: (i32, i32),
    /// A printable key press also types its character (TextInput), the way an
    /// OS delivers typing to a real player. Off reproduces ruffle's own test
    /// protocol, which sends the two as separate scripted events.
    text_input: bool,
    /// This frame's mixed audio: the mixer's f32 buffer, and the i16 stereo
    /// copy the host reads through GetAudio (it must not move during the read).
    audio_f32: Rc<RefCell<Vec<f32>>>,
    audio_i16: Vec<i16>,
    /// The navigator's load futures, drained each frame after the movie runs -
    /// where ruffle's own runner drains its executor.
    tasks: Tasks,
}

// One machine per sandbox, driven by one thread (vsched runs guest threads one
// at a time), so a static is the honest representation.
static mut MACHINE: Option<Machine> = None;
static mut LOAD_ERROR: [u8; 256] = [0; 256];
static mut SPOOF_URL: [u8; 512] = [0; 512];

/// The URL the movie believes it was loaded from (Flash's domain checks, and
/// where relative loads resolve). Set before Init; empty means file:///.
#[no_mangle]
pub extern "C" fn SetSpoofUrl(ptr: *const u8, len: i32) {
    unsafe {
        let n = (len as usize).min(SPOOF_URL.len() - 1);
        let src = core::slice::from_raw_parts(ptr, n);
        let e = &mut *core::ptr::addr_of_mut!(SPOOF_URL);
        e[..n].copy_from_slice(src);
        e[n] = 0;
    }
}

fn set_load_error(msg: &str) {
    unsafe {
        let e = &mut *core::ptr::addr_of_mut!(LOAD_ERROR);
        let n = msg.len().min(e.len() - 1);
        e[..n].copy_from_slice(&msg.as_bytes()[..n]);
        e[n] = 0;
    }
}

#[no_mangle]
pub extern "C" fn GetLoadError() -> *const u8 {
    core::ptr::addr_of!(LOAD_ERROR) as *const u8
}

static mut SWF: Vec<u8> = Vec::new();

/// The host asks for room for the movie, fills it, then calls Init.
#[no_mangle]
pub extern "C" fn AllocSwf(len: u64) -> *mut u8 {
    unsafe {
        let b = &mut *core::ptr::addr_of_mut!(SWF);
        b.clear();
        b.resize(len as usize, 0);
        b.as_mut_ptr()
    }
}

#[no_mangle]
pub extern "C" fn Init() -> i32 {
    let data: &[u8] = unsafe { &*core::ptr::addr_of!(SWF) };
    if data.is_empty() {
        set_load_error("no SWF was handed over (call AllocSwf first)");
        return 0;
    }
    let movie = match SwfMovie::from_data(data, "file:///game.swf".to_string(), None, None) {
        Ok(m) => m,
        Err(e) => {
            set_load_error(&format!("not a SWF: {e}"));
            return 0;
        }
    };
    let frame_rate = movie.frame_rate().to_f64();
    let frame_time = FloatDuration::from_millis(1000.0 / frame_rate);
    // The viewport IS the stage, at scale 1, so a pointer coordinate the host
    // sends is a stage pixel. Any other size scales the stage to fit and every
    // click lands somewhere else: the per-object hit tests silently miss while
    // the root-level handlers still fire. (Same rule as ruffle's own runner.)
    let (vw, vh) = (movie.width().to_pixels() as u32, movie.height().to_pixels() as u32);

    let trace = Rc::new(RefCell::new(Vec::new()));
    let audio_f32 = Rc::new(RefCell::new(Vec::new()));
    let tasks: Tasks = std::rc::Rc::new(RefCell::new(Vec::new()));
    let mut builder = PlayerBuilder::new()
        .with_movie(movie)
        .with_log(CaptureLog { out: trace.clone() })
        .with_audio(WaterboxAudio { mixer: AudioMixer::new(WaterboxAudio::CHANNELS, WaterboxAudio::RATE), buffer: audio_f32.clone() })
        .with_navigator(GuestNavigator { tasks: tasks.clone() })
        .with_autoplay(true)
        .with_viewport_dimensions(vw.max(1), vh.max(1), 1.0);
    let spoof = unsafe {
        let raw = &*core::ptr::addr_of!(SPOOF_URL);
        let n = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
        core::str::from_utf8(&raw[..n]).ok().filter(|s| !s.is_empty()).map(|s| s.to_owned())
    };
    if let Some(u) = spoof {
        builder = builder.with_spoofed_url(Some(u));
    }
    let player = builder.build();

    player.lock().unwrap().preload(&mut ExecutionLimit::exhausted());

    unsafe {
        *core::ptr::addr_of_mut!(MACHINE) =
            Some(Machine {
                player, frame_time, trace, tty: Vec::new(), frames: 0,
                buttons: [0; BUTTON_COUNT], prev_buttons: [0; BUTTON_COUNT],
                mouse: (0, 0), prev_mouse: (0, 0), text_input: true,
                audio_f32, audio_i16: Vec::new(), tasks,
            });
    }
    1
}

#[no_mangle]
pub extern "C" fn FrameAdvance(_input: u64) {
    unsafe {
        let m = match &mut *core::ptr::addr_of_mut!(MACHINE) {
            Some(m) => m,
            None => return,
        };
        // Ruffle's own test runner, for a frame-counted test, does EXACTLY:
        // run_frame, update_timers(frame_time), audio.tick, THEN inject this
        // frame's input, THEN render (which has display-list side effects). Not
        // tick(): that is the wall-clock path with its own frame accumulator and
        // AVM2 catch-up logic, and calling it as well doubles frame work in
        // ways only timer- and input-sensitive movies notice. Same order here,
        // so a movie's frame-k input lands exactly where ruffle's block k does
        // and the corpus is a valid oracle.
        let mut p = m.player.lock().unwrap();
        p.run_frame();
        p.update_timers(m.frame_time);
        p.audio_mut().tick();
        drop(p);
        run_tasks(&m.tasks); // resolve any loadMovie/loadSound the frame kicked off
        let mut p = m.player.lock().unwrap();
        {
            // f32 -> i16, the conversion every host expects; clamp, never wrap
            let f = m.audio_f32.borrow();
            m.audio_i16.clear();
            m.audio_i16.extend(f.iter().map(|v| (v.clamp(-1.0, 1.0) * 32767.0) as i16));
        }
        inject_edges(&mut p, &mut m.buttons, &mut m.prev_buttons, m.mouse, &mut m.prev_mouse, m.text_input);
        p.render();
        drop(p);
        m.frames += 1;
    }
}

#[no_mangle]
pub extern "C" fn GetTty() -> *const u8 {
    unsafe {
        let m = match &mut *core::ptr::addr_of_mut!(MACHINE) {
            Some(m) => m,
            None => return core::ptr::null(),
        };
        m.tty = m.trace.borrow().clone();
        m.tty.as_ptr()
    }
}

#[no_mangle]
pub extern "C" fn GetTtySize() -> i64 {
    unsafe {
        match &*core::ptr::addr_of!(MACHINE) {
            Some(m) => m.trace.borrow().len() as i64,
            None => 0,
        }
    }
}

/// FNV-1a over the trace: the one number the native reference and the sandbox
/// must agree on, frame for frame.
#[no_mangle]
pub extern "C" fn GetTraceDigest() -> u64 {
    unsafe {
        let m = match &*core::ptr::addr_of!(MACHINE) {
            Some(m) => m,
            None => return 0,
        };
        let mut h: u64 = 1469598103934665603;
        for b in m.trace.borrow().iter() {
            h ^= *b as u64;
            h = h.wrapping_mul(1099511628211);
        }
        h
    }
}

#[no_mangle]
pub extern "C" fn GetFrameCount() -> u64 {
    unsafe {
        match &*core::ptr::addr_of!(MACHINE) {
            Some(m) => m.frames,
            None => 0,
        }
    }
}

#[no_mangle]
pub extern "C" fn IsRunning() -> i32 {
    unsafe { if (*core::ptr::addr_of!(MACHINE)).is_some() { 1 } else { 0 } }
}

/// Turn the host's LEVELS into Ruffle's EDGES: one MouseMove if the pointer
/// moved, then one Down/Up per button whose level changed, in wire order (a
/// fixed order is what makes two frames with the same levels identical).
fn inject_edges(
    p: &mut Player,
    buttons: &mut [u8; BUTTON_COUNT],
    prev: &mut [u8; BUTTON_COUNT],
    mouse: (i32, i32),
    prev_mouse: &mut (i32, i32),
    text_input: bool,
) {
    let (x, y) = (mouse.0 as f64, mouse.1 as f64);
    // A pointer that moved AND pressed in the same frame gets one event, the
    // press, which carries the position: Ruffle's own protocol sends a bare
    // MouseDown at a position, and a preceding synthetic move would fire hover
    // events the scripted stream never had. A move on its own is a MouseMove.
    let mouse_edge = (0..3).any(|i| buttons[i] != prev[i]);
    if mouse != *prev_mouse {
        if !mouse_edge {
            p.handle_event(PlayerEvent::MouseMove { x, y });
        }
        *prev_mouse = mouse;
    }
    let shift = buttons[SHIFT_LEFT] != 0 || buttons[SHIFT_RIGHT] != 0;
    for i in 0..BUTTON_COUNT {
        if buttons[i] == prev[i] {
            continue;
        }
        let down = buttons[i] != 0;
        match &BUTTONS[i] {
            Btn::Mouse(b) => {
                let button = *b;
                if down {
                    // index: Some(0) is what ruffle's own harness sends - click
                    // counting from wall-clock time is exactly what a TAS must not have
                    p.handle_event(PlayerEvent::MouseDown { x, y, button, index: Some(0) });
                } else {
                    p.handle_event(PlayerEvent::MouseUp { x, y, button });
                }
            }
            Btn::Char(lo, hi, pk) => {
                let ch = if shift { *hi } else { *lo };
                let key = KeyDescriptor { physical_key: *pk, logical_key: LogicalKey::Character(ch), key_location: KeyLocation::Standard };
                if down {
                    p.handle_event(PlayerEvent::KeyDown { key });
                    if text_input { p.handle_event(PlayerEvent::TextInput { codepoint: ch }); }
                } else {
                    p.handle_event(PlayerEvent::KeyUp { key });
                }
            }
            Btn::Named(nk, pk, loc) => {
                let key = KeyDescriptor { physical_key: *pk, logical_key: LogicalKey::Named(*nk), key_location: *loc };
                if down { p.handle_event(PlayerEvent::KeyDown { key }); } else { p.handle_event(PlayerEvent::KeyUp { key }); }
            }
            Btn::NumChar(ch, pk) => {
                let key = KeyDescriptor { physical_key: *pk, logical_key: LogicalKey::Character(*ch), key_location: KeyLocation::Numpad };
                if down {
                    p.handle_event(PlayerEvent::KeyDown { key });
                    if text_input { p.handle_event(PlayerEvent::TextInput { codepoint: *ch }); }
                } else {
                    p.handle_event(PlayerEvent::KeyUp { key });
                }
            }
        }
        prev[i] = buttons[i];
    }
}

#[no_mangle]
pub extern "C" fn SetButton(index: i32, state: i32) {
    unsafe {
        if let Some(m) = &mut *core::ptr::addr_of_mut!(MACHINE) {
            if index >= 0 && (index as usize) < BUTTON_COUNT {
                m.buttons[index as usize] = if state != 0 { 1 } else { 0 };
            }
        }
    }
}

#[no_mangle]
pub extern "C" fn SetAxis(index: i32, value: i32) {
    unsafe {
        if let Some(m) = &mut *core::ptr::addr_of_mut!(MACHINE) {
            match index {
                0 => m.mouse.0 = value,
                1 => m.mouse.1 = value,
                _ => {}
            }
        }
    }
}

/// Whether a printable key press also types its character. On by default
/// (what a player at a real keyboard gets); the corpus oracle runs with it
/// off, because ruffle's test protocol scripts TextInput separately.
#[no_mangle]
pub extern "C" fn SetTextInput(on: i32) {
    unsafe {
        if let Some(m) = &mut *core::ptr::addr_of_mut!(MACHINE) {
            m.text_input = on != 0;
        }
    }
}

#[no_mangle]
pub extern "C" fn GetAudio() -> *const i16 {
    unsafe {
        match &*core::ptr::addr_of!(MACHINE) {
            Some(m) => m.audio_i16.as_ptr(),
            None => core::ptr::null(),
        }
    }
}

/// Stereo frames this frame (interleaved L R), at 44100 Hz.
#[no_mangle]
pub extern "C" fn GetAudioSampleCount() -> i32 {
    unsafe {
        match &*core::ptr::addr_of!(MACHINE) {
            Some(m) => (m.audio_i16.len() / 2) as i32,
            None => 0,
        }
    }
}
