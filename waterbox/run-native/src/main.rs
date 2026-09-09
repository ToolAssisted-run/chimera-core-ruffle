// M0 native reference for the chimera Ruffle core.
//
// Drive ruffle_core the way ruffle's own test framework does: null backends, a
// log backend that captures ActionScript trace(), autoplay, a fixed viewport.
// Load a .swf, preload it, run N frames at the movie's own frame rate, and
// print the captured trace. Ruffle's test corpus ships each test.swf next to
// the output.txt this must reproduce, which is the determinism-and-correctness
// oracle - no console, no rendering, no copyrighted content.
//
// Usage: run-native <file.swf> --frames <n> [--fps <f>] [--spoof-url <url>]
//                    [--click <frame>:<x>:<y>[,...]] [--hover <n>] [--hold <n>]
//                    [--width <px>] [--height <px>] [--audio-out <raw i16 stereo>]
//                    [--audio-peaks <one max-amplitude per frame>]
// Prints the trace to stdout and, on stderr, one summary line:
//   ruffle: frames=<n> traceBytes=<k> traceSha1=<hex> lastFrame=<cur>

use std::cell::RefCell;
use std::rc::Rc;

use ruffle_core::backend::audio::{
    swf, AudioBackend, AudioMixer, DecodeError, RegisterError, SoundHandle, SoundInstanceHandle,
    SoundStreamInfo, SoundTransform,
};
use ruffle_core::backend::log::LogBackend;
use ruffle_core::impl_audio_mixer_backend;

/// Ruffle's software mixer a frame at a time - identical to the guest's, so the
/// two produce the same samples or the gate says so.
struct NativeAudio { mixer: AudioMixer, buffer: Rc<RefCell<Vec<f32>>> }
impl AudioBackend for NativeAudio {
    impl_audio_mixer_backend!(mixer);
    fn play(&mut self) {}
    fn pause(&mut self) {}
    fn set_frame_rate(&mut self, frame_rate: f64) {
        // whole stereo frames, exactly as the guest (see its comment)
        let frames = (44100f64 / frame_rate).round() as usize;
        self.buffer.borrow_mut().resize(frames * 2, 0.0);
    }
    fn tick(&mut self) {
        let mut b = self.buffer.borrow_mut();
        if !b.is_empty() { self.mixer.mix::<f32>(b.as_mut()); }
    }
}
use ruffle_core::tag_utils::SwfMovie;
use ruffle_core::limits::ExecutionLimit;
use ruffle_core::{FloatDuration, PlayerBuilder, PlayerEvent};

#[derive(Clone)]
struct CaptureLog {
    out: Rc<RefCell<String>>,
}
impl CaptureLog {
    fn new() -> Self {
        Self { out: Rc::new(RefCell::new(String::new())) }
    }
    fn take(&self) -> String {
        self.out.take()
    }
}
impl LogBackend for CaptureLog {
    fn avm_trace(&self, message: &str) {
        self.out.borrow_mut().push_str(message);
        self.out.borrow_mut().push('\n');
    }
    fn avm_warning(&self, _message: &str) {}
}

// A tiny, dependency-free SHA1 so the summary needs no crates beyond ruffle.
fn sha1_hex(data: &[u8]) -> String {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let ml = (data.len() as u64) * 8;
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&ml.to_be_bytes());
    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([chunk[i * 4], chunk[i * 4 + 1], chunk[i * 4 + 2], chunk[i * 4 + 3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let t = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(wi);
            e = d; d = c; c = b.rotate_left(30); b = a; a = t;
        }
        h[0] = h[0].wrapping_add(a); h[1] = h[1].wrapping_add(b); h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d); h[4] = h[4].wrapping_add(e);
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: run-native <file.swf> --frames <n> [--fps <f>] [--spoof-url <url>] [--width <px>] [--height <px>]");
        std::process::exit(2);
    }
    let swf_path = args[1].clone();
    let mut frames: u32 = 1;
    let mut fps: Option<f64> = None;
    let mut spoof_url: Option<String> = None;
    let mut width: u32 = 0; // 0 = the movie's own stage size, like ruffle's runner
    let mut height: u32 = 0;
    let mut audio_out: Option<String> = None;
    let mut audio_peaks: Option<String> = None;
    let mut clicks: Vec<(u32, f64, f64)> = Vec::new();
    let mut hover: u32 = 0;
    let mut hold: u32 = 2;
    let mut virtual_time = false;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--frames" => { frames = args[i + 1].parse().unwrap(); i += 2; }
            "--fps" => { fps = Some(args[i + 1].parse().unwrap()); i += 2; }
            "--spoof-url" => { spoof_url = Some(args[i + 1].clone()); i += 2; }
            "--width" => { width = args[i + 1].parse().unwrap(); i += 2; }
            "--height" => { height = args[i + 1].parse().unwrap(); i += 2; }
            "--audio-out" => { audio_out = Some(args[i + 1].clone()); i += 2; }
            "--audio-peaks" => { audio_peaks = Some(args[i + 1].clone()); i += 2; }
            // A scripted pointer, so the native reference can answer the one
            // question the image and audio oracles cannot: whether a movie that
            // ignores a click ignores it HERE too. Same shape as the guest's:
            // the pointer arrives, then presses, then releases, and the events
            // land in the same place in the frame.
            "--click" => {
                for part in args[i + 1].split(',') {
                    let f: Vec<&str> = part.split(':').collect();
                    if f.len() == 3 {
                        clicks.push((
                            f[0].parse::<u32>().unwrap(),
                            f[1].parse::<f64>().unwrap(),
                            f[2].parse::<f64>().unwrap(),
                        ));
                    }
                }
                i += 2;
            }
            "--hover" => { hover = args[i + 1].parse().unwrap(); i += 2; }
            "--hold" => { hold = args[i + 1].parse().unwrap(); i += 2; }
            // the guest advances a virtual clock every frame because the sandbox
            // has none; this makes the reference do the same, so that difference
            // can be tested rather than assumed
            "--virtual-time" => { virtual_time = true; i += 1; }
            other => { eprintln!("unknown arg: {other}"); std::process::exit(2); }
        }
    }

    let data = std::fs::read(&swf_path).expect("read swf");
    let movie = SwfMovie::from_data(&data, format!("file://{swf_path}"), None, None)
        .expect("parse swf");
    let frame_rate = fps.unwrap_or_else(|| movie.frame_rate().to_f64());
    if width == 0 { width = (movie.width().to_pixels() as u32).max(1); }
    if height == 0 { height = (movie.height().to_pixels() as u32).max(1); }
    let frame_time = FloatDuration::from_millis(1000.0 / frame_rate);

    let log = CaptureLog::new();
    let audio_buf = Rc::new(RefCell::new(Vec::new()));
    let mut builder = PlayerBuilder::new()
        .with_movie(movie)
        .with_log(log.clone())
        .with_audio(NativeAudio { mixer: AudioMixer::new(2, 44100), buffer: audio_buf.clone() })
        .with_autoplay(true)
        .with_viewport_dimensions(width, height, 1.0);
    if let Some(url) = spoof_url {
        builder = builder.with_spoofed_url(Some(url));
    }
    let player = builder.build();

    player.lock().unwrap().preload(&mut ExecutionLimit::exhausted());
    let mut last_frame = 0u16;
    let mut audio_all: Vec<u8> = Vec::new();
    let mut peaks: Vec<f32> = Vec::new();
    for frame_i in 0..frames {
        let mut p = player.lock().unwrap();
        if virtual_time { p.advance_virtual_time(frame_time.to_std()); }
        // exactly ruffle's runner for a frame-counted test (not tick(): see the guest)
        p.run_frame();
        p.update_timers(frame_time);
        p.audio_mut().tick();
        // input goes exactly where the guest puts it: after the frame, before
        // the render, so the two sides are comparable frame for frame
        for &(cf, x, y) in &clicks {
            if frame_i == cf {
                p.handle_event(PlayerEvent::MouseMove { x, y });
            }
            if frame_i == cf + hover {
                p.handle_event(PlayerEvent::MouseDown { x, y, button: ruffle_core::events::MouseButton::Left, index: Some(0) });
            }
            if frame_i == cf + hover + hold {
                p.handle_event(PlayerEvent::MouseUp { x, y, button: ruffle_core::events::MouseButton::Left });
            }
        }
        p.render(); // ruffle's runner renders every frame; it has display-list side effects
        last_frame = p.current_frame().unwrap_or(last_frame);
        // the same f32 -> i16 the guest does; peaks from the i16 so both sides agree
        let f = audio_buf.borrow();
        let mut peak = 0i32;
        for v in f.iter() {
            let s16 = (v.clamp(-1.0, 1.0) * 32767.0) as i16;
            audio_all.extend_from_slice(&s16.to_le_bytes());
            peak = peak.max((s16 as i32).abs());
        }
        peaks.push(peak as f32 / 32767.0);
    }

    let trace = log.take();
    print!("{trace}");
    if let Some(pth) = audio_out { std::fs::write(pth, &audio_all).expect("write audio"); }
    if let Some(pth) = audio_peaks {
        std::fs::write(pth, peaks.iter().map(|p| format!("{p:.6}\n")).collect::<String>()).expect("write peaks");
    }
    let mut ah: u64 = 1469598103934665603;
    for b in &audio_all { ah ^= *b as u64; ah = ah.wrapping_mul(1099511628211); }
    eprintln!(
        "ruffle: frames={frames} traceBytes={} traceSha1={} lastFrame={last_frame} audioBytes={} audioDigest={ah:016x}",
        trace.len(),
        sha1_hex(trace.as_bytes()),
        audio_all.len()
    );
}
