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

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use ruffle_core::backend::audio::{
    swf, AudioBackend, AudioMixer, DecodeError, RegisterError, SoundHandle, SoundInstanceHandle,
    SoundStreamInfo, SoundTransform,
};
use ruffle_core::backend::locale::LocaleBackend;
use ruffle_core::backend::log::LogBackend;
use ruffle_core::compatibility_rules::CompatibilityRules;
use ruffle_core::{LoadBehavior, PlayerRuntime};
use ruffle_render::quality::StageQuality;
use ruffle_core::impl_audio_mixer_backend;
use ruffle_core::events::{KeyDescriptor, KeyLocation, LogicalKey};
use ruffle_core::limits::ExecutionLimit;
use ruffle_core::tag_utils::SwfMovie;
use ruffle_core::{FloatDuration, Player, PlayerBuilder, PlayerEvent};

mod input_table;
mod navigator;
mod renderer;
use input_table::{Btn, BUTTONS, BUTTON_COUNT, SHIFT_LEFT, SHIFT_RIGHT};
use navigator::{read_whole, run_tasks, GuestNavigator, Tasks};
use renderer::GuestRenderer;


/// Ruffle says a great deal about what it could not do - an unimplemented
/// method, a SharedObject it refused to open, an asset it would not decode -
/// and it says all of it through `tracing`. A guest with no subscriber
/// installed drops every word, which is why a game that half-works in this core
/// has been so hard to explain: the player is telling you, and nothing is
/// listening. This is the smallest subscriber that keeps those words, pointed
/// at the same stderr the rest of this core's diagnostics already use.
struct WarnToStderr;

struct MsgVisitor(String);
impl tracing::field::Visit for MsgVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn core::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{:?}", value);
        } else {
            if !self.0.is_empty() {
                self.0.push(' ');
            }
            self.0.push_str(&format!("{}={:?}", field.name(), value));
        }
    }
}

impl tracing::Subscriber for WarnToStderr {
    fn enabled(&self, meta: &tracing::Metadata<'_>) -> bool {
        *meta.level() <= tracing::Level::WARN
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let mut v = MsgVisitor(String::new());
        event.record(&mut v);
        eprintln!("ruffle {}: {}", event.metadata().level(), v.0);
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}


/// The machine's wall clock, as a movie reads it through `Date`.
///
/// The sandbox answers clock_gettime with a constant, so ruffle's default
/// locale backend - which is `Utc::now()` - is FROZEN in here. getTimer() was
/// given a moving clock long ago (advance_virtual_time, below); `Date` was not,
/// so a movie asking `new Date().getTime()` got the same millisecond forever.
///
/// That does not look like a clock bug from the outside. New Star Soccer draws
/// its language menu, follows the pointer with its own cursor, and ignores
/// every click - because its Monkey X runtime measures a tap in elapsed
/// milliseconds, and none ever elapse. Proved by freezing the NATIVE
/// reference's date (run-native --frozen-date): the same click that works there
/// stops working, which is exactly what the sandbox does.
///
/// So `Date` follows the same virtual clock `getTimer()` does. Deterministic,
/// because a movie has to replay identically on every machine - the epoch is
/// fixed and the step is one host frame - and MOVING, which is what frozen was
/// not. A movie that prints the date prints the same date everywhere.
struct WaterboxLocale {
    millis: Rc<Cell<i64>>,
}

impl LocaleBackend for WaterboxLocale {
    fn get_current_date_time(&self) -> chrono::DateTime<chrono::Utc> {
        // 2001-01-01T00:00:00Z, and not "now" for any value of now
        chrono::DateTime::from_timestamp(978_307_200, 0)
            .unwrap()
            .checked_add_signed(chrono::TimeDelta::milliseconds(self.millis.get()))
            .unwrap()
    }

    fn get_timezone(&self) -> chrono::FixedOffset {
        chrono::FixedOffset::east_opt(0).unwrap()
    }
}

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
    /* the movie's own warnings go where ruffle's do: nothing else was keeping
     * them, and a movie complaining about itself is exactly what somebody
     * looking at a half-working game needs to read */
    fn avm_warning(&self, message: &str) { eprintln!("ruffle avm: {}", message); }
}

/// Ruffle's software mixer, driven a frame at a time - the same shape as the
/// TestAudioBackend ruffle's own harness uses, so the corpus's amplitude
/// assertions are a valid oracle. The player sizes the buffer through
/// set_frame_rate when the movie loads, and tick() mixes one frame into it.
/// The buffer is shared with the machine, which hands it to the host as i16.
struct WaterboxAudio {
    mixer: AudioMixer,
    buffer: Rc<RefCell<Vec<f32>>>,
    /// The rate a frame of audio is mixed FOR, when it is not the movie's.
    /// Sound is mixed once per HOST frame, so at a raised frame rate the
    /// buffer has to be the host's frame worth of samples or every second of
    /// movie would mix several seconds of sound. None leaves the movie's own
    /// rate alone, which is what the player sets and what every default run
    /// has always used.
    forced_rate: Option<f64>,
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
        let frame_rate = self.forced_rate.unwrap_or(frame_rate);
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
    /// One HOST frame's worth of time - the frame the frontend counts, the one
    /// a movie's input log has a line for. Equal to the movie's own frame at
    /// the default rate, and a fraction of it above that.
    frame_time: FloatDuration,
    /// What `Date` reads: milliseconds since this machine's fixed epoch, moved
    /// one host frame at a time (see WaterboxLocale).
    clock_millis: Rc<Cell<i64>>,
    /// The movie's own frame rate, as the SWF stores it: rate = rate_256/256.
    /// Movie frames are due against this and nothing else, so raising the host
    /// rate gives more input, not a faster movie.
    rate_256: u64,
    /// The host frame rate as a ratio (num/den): the movie's own rate by
    /// default, the fps setting when it is set.
    host_num: u64,
    host_den: u64,
    /// Movie frames run so far. A movie frame is due on the first host frame
    /// that reaches it, and the host frames after it carry input and timers
    /// into the movie frame that is already running.
    movie_frames: u64,
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
    /// The pointer is a person's, arriving through the axis, not a recorded
    /// stream of exact positions. A person's pointer must be seen to MOVE
    /// before it clicks; a recorded one must not (see inject_edges).
    live_pointer: bool,
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
    /// This frame's picture, BGRA, read back out of the offscreen GL target.
    /// GetVideoBgra hands out a pointer into it, so it must not move during
    /// the host's read.
    video: Vec<u8>,
    video_w: u32,
    video_h: u32,
    /// The movie's declared frame rate, as a ratio the engine can clock from.
    vsync_num: i32,
    vsync_den: i32,
    /// The host GL context the renderer's objects were built against. wgpu owns
    /// those objects opaquely as names a particular context handed out, and a
    /// whole-machine savestate carries the names into a session where that
    /// context is gone (a fresh process gets a new one). This id, restored with
    /// the rest of guest memory, is what lets FrameAdvance notice: a mismatch
    /// with the live context means rebuild the backend before drawing. Zero
    /// means the bridge cannot tell (no GPU), so nothing ever moves.
    gl_context: u64,
}

// One machine per sandbox, driven by one thread (vsched runs guest threads one
// at a time), so a static is the honest representation.
static mut MACHINE: Option<Machine> = None;
static mut LOAD_ERROR: [u8; 256] = [0; 256];
static mut SPOOF_URL: [u8; 512] = [0; 512];

/// The settings channel: the engine mounts the effective settings as a flat
/// JSON object under "settings" (always, even when empty, so the ABI is
/// uniform). Read once at Init; anything unreadable is treated as unset rather
/// than as a failure, because a movie that would have run is not worth refusing
/// over a setting.
struct Settings(serde_json::Value);

impl Settings {
    fn load() -> Self {
        Settings(
            // read_whole, never std::fs: an empty read here loses every setting
            // in silence, which is what made the spoofed URL, the quality knob
            // and font substitution all appear to do nothing.
            read_whole("settings")
                .ok()
                .and_then(|raw| serde_json::from_slice(&raw).ok())
                .unwrap_or(serde_json::Value::Null),
        )
    }

    /// A non-blank string, or None. Whitespace is not a value.
    fn str(&self, name: &str) -> Option<String> {
        let s = self.0.get(name)?.as_str()?.trim();
        if s.is_empty() { None } else { Some(s.to_owned()) }
    }

    /// An enum arrives as its name; matching is case-insensitive so that
    /// "High8x8" and "high8x8" mean the same, and an unknown name falls back to
    /// the default rather than failing the load.
    fn choice<T: Copy>(&self, name: &str, table: &[(&str, T)], fallback: T) -> T {
        match self.str(name) {
            Some(v) => table
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(&v))
                .map(|(_, t)| *t)
                .unwrap_or(fallback),
            None => fallback,
        }
    }

    fn bool(&self, name: &str, fallback: bool) -> bool {
        self.0.get(name).and_then(|v| v.as_bool()).unwrap_or(fallback)
    }

    fn int(&self, name: &str, fallback: i64) -> i64 {
        self.0.get(name).and_then(|v| v.as_i64()).unwrap_or(fallback)
    }
}

/// The URL the movie believes it was loaded from (Flash's domain checks, and
/// where relative loads resolve). Set before Init; empty means file:///.
///
/// The frontend has no reason to call this - it declares spoofUrl as a setting
/// and the engine mounts it - but the gate's own runner has no settings channel,
/// so this stays as the direct route, and it wins when both are given.
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
    // The movie arrives one of two ways: handed over directly through AllocSwf
    // (the gate's runner does that), or mounted into the guest's filesystem
    // under the name waterbox.config calls romFile, which is how the engine
    // loads a game.
    let handed: &[u8] = unsafe { &*core::ptr::addr_of!(SWF) };
    let mounted;
    let data: &[u8] = if !handed.is_empty() {
        handed
    } else {
        // read_whole, never std::fs. This is the ENGINE's route into the core -
        // the frontend mounts the movie as "game" - so an empty read here is a
        // project that opens to nothing.
        match read_whole("game") {
            Ok(bytes) if !bytes.is_empty() => { mounted = bytes; &mounted }
            _ => {
                set_load_error("no SWF: nothing was handed over and no movie is mounted as 'game'");
                return 0;
            }
        }
    };
    let movie = match SwfMovie::from_data(data, "file:///game.swf".to_string(), None, None) {
        Ok(m) => m,
        Err(e) => {
            set_load_error(&format!("not a SWF: {e}"));
            return 0;
        }
    };
    let cfg = Settings::load();
    let frame_rate = movie.frame_rate().to_f64();
    // The SWF stores its rate as 8.8 fixed point, so this is exact.
    let rate_256 = (frame_rate * 256.0).round().max(1.0) as u64;

    // How often the machine is stepped. A Flash movie has its own frame rate
    // and that is what its timeline runs at; this is how often the HOST gets a
    // frame, which is how often a person (or a movie file) can change what the
    // input says. Unset, the two are the same and this is exactly what the core
    // has always done. Raised, the movie still runs at its own rate and the
    // extra frames carry input, timers and a picture.
    let fps = cfg.int("fps", 0);
    let (host_num, host_den) = if fps > 0 { (fps as u64, 1u64) } else { (rate_256, 256u64) };
    // host period = den/num seconds. At the default this is 1000.0/frame_rate
    // to the last bit: both are the correctly rounded value of the same exact
    // ratio, since a fixed-8 rate is exact in a double.
    let frame_time = FloatDuration::from_millis(1000.0 * host_den as f64 / host_num as f64);
    // The viewport IS the stage, at scale 1, so a pointer coordinate the host
    // sends is a stage pixel. Any other size scales the stage to fit and every
    // click lands somewhere else: the per-object hit tests silently miss while
    // the root-level handlers still fire. (Same rule as ruffle's own runner.)
    let (vw, vh) = (movie.width().to_pixels() as u32, movie.height().to_pixels() as u32);

    // What the frontend runs at: the host rate, which is the movie's own unless
    // the fps setting raised it. A rate like 23.976 has to survive as a ratio;
    // a whole number stays whole.
    let host_rate = host_num as f64 / host_den as f64;
    let (vsync_num, vsync_den) = if (host_rate - host_rate.round()).abs() < 1e-6 {
        (host_rate.round() as i32, 1)
    } else {
        ((host_rate * 1000.0).round() as i32, 1000)
    };

    let _ = tracing::subscriber::set_global_default(WarnToStderr);
    let trace = Rc::new(RefCell::new(Vec::new()));
    let clock_millis = Rc::new(Cell::new(0i64));
    let audio_f32 = Rc::new(RefCell::new(Vec::new()));
    let tasks: Tasks = std::rc::Rc::new(RefCell::new(Vec::new()));
    // A real GL renderer, in the sandbox, on software: see renderer.rs. It is
    // required, not optional - a core that quietly ran without a picture would
    // still pass a trace gate and be useless for a TAS.
    let render_backend = match renderer::build(vw, vh) {
        Ok(r) => r,
        Err(e) => {
            set_load_error(&format!("no renderer: {e}"));
            return 0;
        }
    };

    let mut builder = PlayerBuilder::new()
        .with_movie(movie)
        .with_renderer(render_backend)
        .with_log(CaptureLog { out: trace.clone() })
        .with_locale(WaterboxLocale { millis: clock_millis.clone() })
        .with_audio(WaterboxAudio {
            mixer: AudioMixer::new(WaterboxAudio::CHANNELS, WaterboxAudio::RATE),
            buffer: audio_f32.clone(),
            // only when the rate was raised: at the default the player's own
            // call is already the movie's rate, and nothing changes
            forced_rate: if fps > 0 { Some(host_rate) } else { None },
        })
        .with_navigator(GuestNavigator { tasks: tasks.clone() })
        .with_autoplay(true)
        .with_viewport_dimensions(vw.max(1), vh.max(1), 1.0);

    // Where the movie thinks it is. Two separate addresses, because Flash asks
    // two separate questions: the movie's own URL (a sponsor lock reading
    // loaderInfo.url, and what relative loads resolve against) and the address
    // of the page it is embedded in (Security.pageDomain).
    let spoof = unsafe {
        let raw = &*core::ptr::addr_of!(SPOOF_URL);
        let n = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
        core::str::from_utf8(&raw[..n]).ok().filter(|s| !s.is_empty()).map(|s| s.to_owned())
    }.or_else(|| cfg.str("spoofUrl"));
    if let Some(u) = spoof {
        builder = builder.with_spoofed_url(Some(u));
    }
    if let Some(u) = cfg.str("pageUrl") {
        builder = builder.with_page_url(Some(u));
    }

    // What the movie thinks it is running on. 0 means "whatever the SWF asks
    // for", which is ruffle's own behaviour.
    let ver = cfg.int("playerVersion", 0);
    if (6..=32).contains(&ver) {
        builder = builder.with_player_version(Some(ver as u8));
    }
    builder = builder.with_player_runtime(cfg.choice(
        "playerRuntime",
        &[("flashPlayer", PlayerRuntime::FlashPlayer), ("air", PlayerRuntime::AIR)],
        PlayerRuntime::FlashPlayer,
    ));
    builder = builder.with_quality(cfg.choice(
        "quality",
        &[
            ("low", StageQuality::Low),
            ("medium", StageQuality::Medium),
            ("high", StageQuality::High),
            ("best", StageQuality::Best),
            ("high8x8", StageQuality::High8x8),
            ("high8x8linear", StageQuality::High8x8Linear),
            ("high16x16", StageQuality::High16x16),
            ("high16x16linear", StageQuality::High16x16Linear),
        ],
        StageQuality::High,
    ));
    builder = builder.with_load_behavior(cfg.choice(
        "loadBehavior",
        &[
            ("streaming", LoadBehavior::Streaming),
            ("delayed", LoadBehavior::Delayed),
            ("blocking", LoadBehavior::Blocking),
        ],
        LoadBehavior::Streaming,
    ));
    // Ruffle's per-site fixups. Off is what this core has always done, and what
    // a preservation run wants; on is what ruffle's own players do.
    builder = builder.with_compatibility_rules(if cfg.bool("compatibilityRules", false) {
        CompatibilityRules::builtin_rules()
    } else {
        CompatibilityRules::empty()
    });
    builder = builder.with_default_font(cfg.bool("defaultFont", true));
    let player = builder.build();

    player.lock().unwrap().preload(&mut ExecutionLimit::exhausted());

    unsafe {
        *core::ptr::addr_of_mut!(MACHINE) =
            Some(Machine {
                player, frame_time, clock_millis, rate_256, host_num, host_den, movie_frames: 0,
                trace, tty: Vec::new(), frames: 0,
                buttons: [0; BUTTON_COUNT], prev_buttons: [0; BUTTON_COUNT],
                mouse: (0, 0), prev_mouse: (0, 0), live_pointer: false, text_input: true,
                audio_f32, audio_i16: Vec::new(), tasks,
                video: Vec::new(), video_w: vw.max(1), video_h: vh.max(1),
                vsync_num, vsync_den,
                gl_context: 0,
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
        // The clock moves one frame's worth, every frame: the sandbox has no
        // wall clock (it answers clock_gettime with a constant, so a movie
        // replays the same everywhere), and without this getTimer() answers the
        // same number forever. Buttons still work that way - they are events -
        // but everything a game drives from elapsed time stops dead, which is
        // why Zuma reached its menu and then no ball ever rolled.
        p.advance_virtual_time(m.frame_time.to_std());
        // the same step for the same reason, so `Date` and getTimer() agree
        m.clock_millis
            .set(m.clock_millis.get() + m.frame_time.as_millis().round() as i64);
        // How many movie frames should have run by the end of this host frame:
        // ceil(host frames so far * movie rate / host rate), in integers so it
        // cannot drift over a long run and cannot differ between two machines.
        // At the default rate this is exactly one per host frame, which is what
        // it has always been; above it, the movie frame lands on the FIRST host
        // frame that reaches it and the host frames after it belong to the
        // movie frame already running.
        let due = {
            let n = (m.frames + 1) * m.rate_256 * m.host_den;
            let d = 256 * m.host_num;
            (n + d - 1) / d
        };
        while m.movie_frames < due {
            // BEFORE the frame, every frame - not once at startup. preload is what
            // processes a movie whose bytes have just arrived, so a child SWF that
            // loadMovie fetched is parsed here and nowhere else. Preloading only at
            // startup means the fetch completes, the data is delivered, and the
            // child then sits there: "Loading movie" traces and "Child movie
            // loaded!" never does, with nothing anywhere reporting a failure.
            // ExecutionLimit::exhausted is upstream's own choice here: no budget,
            // so it finishes rather than spreading the work over later frames,
            // which is what keeps this deterministic.
            p.preload(&mut ExecutionLimit::exhausted());
            p.run_frame();
            m.movie_frames += 1;
        }
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
        // The renderer's objects belong to a host GL context, and this frame
        // may be the first after a savestate was loaded into a fresh process:
        // the names wgpu holds are then another context's, every call on them is
        // refused without a word, and the picture comes back blank. Ask which
        // context the calls land on now; if it is a different one than the
        // objects were built against, build a fresh backend on the new context
        // and swap it in, then bump the render epoch so ruffle_core's own
        // GPU-handle caches re-register from the display list and library rather
        // than draw with the dangling handles they still hold. When the context
        // is stable - every ordinary frame, and a state reloaded in the same
        // process - live equals the saved id and this does nothing, so the
        // machine stays exactly what it was. Zero is "cannot tell" (no bridge),
        // and never triggers a rebuild.
        let live = renderer::context_id();
        if live != 0 && m.gl_context != 0 && live != m.gl_context {
            match renderer::build(m.video_w, m.video_h) {
                Ok(rb) => {
                    p.set_renderer(Box::new(rb));
                    p.set_viewport_dimensions(ruffle_render::backend::ViewportDimensions {
                        width: m.video_w,
                        height: m.video_h,
                        scale_factor: 1.0,
                    });
                    // The stage's quality lives in the PLAYER and is pushed down
                    // to the renderer only when it is set (Stage::set_quality
                    // ends in renderer.set_quality). A backend built just now
                    // starts at its own default instead, and for wgpu that
                    // decides the MSAA sample count - so the picture comes back
                    // with different edges, which is not a crash and not a
                    // desync and is easy to miss. Measured on a movie at the
                    // default 'high': 11.6% of pixels differed after a reopen,
                    // and 0% at 'low', where there is no anti-aliasing to lose.
                    // Reading it back and setting it again is what re-pushes it.
                    let quality = p.quality();
                    p.set_quality(quality);
                    ruffle_render::bump_render_epoch();
                    eprintln!(
                        "ruffle: host GL context changed ({} -> {}); rebuilt the renderer",
                        m.gl_context, live
                    );
                }
                Err(e) => eprintln!("ruffle: could not rebuild the renderer after a context change: {e}"),
            }
        }
        if live != 0 {
            m.gl_context = live;
        }
        inject_edges(&mut p, &mut m.buttons, &mut m.prev_buttons, m.mouse, &mut m.prev_mouse, m.live_pointer, m.text_input);
        p.render();
        // Read the frame back out of the offscreen target. ruffle's own image
        // tests capture exactly here, through the same downcast.
        if let Some(rb) = (p.renderer_mut() as &mut dyn std::any::Any).downcast_mut::<GuestRenderer>() {
            if let Some(img) = rb.capture_frame() {
                m.video_w = img.width();
                m.video_h = img.height();
                let src = img.as_raw();
                m.video.clear();
                m.video.reserve(src.len());
                // ruffle gives RGBA; every chimera host reads BGRA
                for px in src.chunks_exact(4) {
                    m.video.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
                }
            }
        }
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
    live_pointer: bool,
    text_input: bool,
) {
    let (x, y) = (mouse.0 as f64, mouse.1 as f64);
    // A pointer that moved AND pressed in the same frame gets one event, the
    // press, which carries the position: Ruffle's own protocol sends a bare
    // MouseDown at a position, and a preceding synthetic move would fire hover
    // events the scripted stream never had. A move on its own is a MouseMove.
    let mouse_edge = (0..3).any(|i| buttons[i] != prev[i]);
    if mouse != *prev_mouse {
        // A person's pointer gets the move BEFORE the press, always - that is
        // what a browser does, and a movie believes it. Flash tracks what the
        // pointer is over, and a bare MouseDown somewhere it was never seen to
        // travel is tested against a stale target: the click lands on whatever
        // was under the pointer last, or on nothing. It goes wrong exactly when
        // someone moves and clicks in one motion, which is what makes it look
        // like the odd click is being dropped.
        //
        // A RECORDED pointer must not get one. ruffle's own protocol sends a
        // bare MouseDown carrying its position, and a synthetic move ahead of it
        // fires rollover events the recording never had.
        if !mouse_edge || live_pointer {
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
                // Say it. A click that does nothing is either not arriving, or
                // arriving somewhere the movie has nothing under - and those
                // need opposite fixes. One line settles which.
                eprintln!("ruffle: mouse {} {:?} at {},{}",
                    if down { "down" } else { "up" }, button, mouse.0, mouse.1);
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

/// The pointer, as the frontend sends it: a position normalised across the
/// range waterbox.config declares, not stage pixels. A movie decides its own
/// stage size, and the config cannot know it, so the scaling belongs here -
/// the same shape as a light gun's screen axes on the other cores.
const MOUSE_AXIS_MAX: i32 = 8191;

#[no_mangle]
pub extern "C" fn SetAxis(index: i32, value: i32) {
    unsafe {
        if let Some(m) = &mut *core::ptr::addr_of_mut!(MACHINE) {
            let v = value.clamp(0, MOUSE_AXIS_MAX) as i64;
            m.live_pointer = true;
            match index {
                0 => m.mouse.0 = ((v * (m.video_w as i64 - 1)) / MOUSE_AXIS_MAX as i64) as i32,
                1 => m.mouse.1 = ((v * (m.video_h as i64 - 1)) / MOUSE_AXIS_MAX as i64) as i32,
                _ => {}
            }
        }
    }
}

/// The pointer in exact stage pixels. The gate replays ruffle's own recorded
/// mouse positions, which are stage coordinates, and must not go through the
/// normalised axis and back.
#[no_mangle]
pub extern "C" fn SetMousePixels(x: i32, y: i32) {
    unsafe {
        if let Some(m) = &mut *core::ptr::addr_of_mut!(MACHINE) {
            m.mouse = (x, y);
            m.live_pointer = false;
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

/// Memory domains: a Flash movie has none worth naming.
///
/// The other cores expose a machine's flat RAM here, which is what a RAM search
/// or a watch is for. Flash has no such thing: a movie's state is a garbage
/// collected object graph inside the AVM, moved and recycled as it runs, and
/// there is no address that means the same thing from one frame to the next.
/// Publishing the guest's heap as "RAM" would be worse than publishing nothing,
/// because it would look searchable and quietly lie. So the count is zero and
/// the rest of the contract is answered honestly.
#[no_mangle]
pub extern "C" fn GetMemoryDomainCount() -> i32 { 0 }

#[no_mangle]
pub extern "C" fn GetMemoryDomainName(_i: i32) -> *const u8 { core::ptr::null() }

#[no_mangle]
pub extern "C" fn GetMemoryDomainPtr(_i: i32) -> *const u8 { core::ptr::null() }

#[no_mangle]
pub extern "C" fn GetMemoryDomainSize(_i: i32) -> i64 { 0 }

#[no_mangle]
pub extern "C" fn GetMemoryDomainWritable(_i: i32) -> i32 { 0 }

/// The frame's picture, BGRA, as the chimera video contract wants it: a pointer
/// into guest memory the host reads GetVideoWidth x GetVideoHeight pixels from.
/// The host's GL callback, handed over before Init. Without it there is no
/// renderer and Init refuses, rather than running blind: a core that quietly
/// produced no picture would still pass a trace gate and be useless for a TAS.
#[no_mangle]
pub extern "C" fn SetGpuBridge(addr: u64) {
    renderer::set_bridge(addr);
}

#[no_mangle]
pub extern "C" fn GetVideoBgra() -> *const u8 {
    unsafe {
        match &*core::ptr::addr_of!(MACHINE) {
            Some(m) => m.video.as_ptr(),
            None => core::ptr::null(),
        }
    }
}

/// Flash movies carry their own frame rate, and it is not always a whole
/// number, so the engine is told a ratio rather than a rounded figure.
#[no_mangle]
pub extern "C" fn GetVsyncNumerator() -> i32 {
    unsafe {
        match &*core::ptr::addr_of!(MACHINE) { Some(m) => m.vsync_num, None => 60 }
    }
}

#[no_mangle]
pub extern "C" fn GetVsyncDenominator() -> i32 {
    unsafe {
        match &*core::ptr::addr_of!(MACHINE) { Some(m) => m.vsync_den, None => 1 }
    }
}

#[no_mangle]
pub extern "C" fn GetVideoWidth() -> i32 {
    unsafe {
        match &*core::ptr::addr_of!(MACHINE) { Some(m) => m.video_w as i32, None => 0 }
    }
}

#[no_mangle]
pub extern "C" fn GetVideoHeight() -> i32 {
    unsafe {
        match &*core::ptr::addr_of!(MACHINE) { Some(m) => m.video_h as i32, None => 0 }
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
