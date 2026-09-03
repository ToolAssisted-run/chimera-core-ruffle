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
//                    [--width <px>] [--height <px>]
// Prints the trace to stdout and, on stderr, one summary line:
//   ruffle: frames=<n> traceBytes=<k> traceSha1=<hex> lastFrame=<cur>

use std::cell::RefCell;
use std::rc::Rc;

use ruffle_core::backend::log::LogBackend;
use ruffle_core::tag_utils::SwfMovie;
use ruffle_core::limits::ExecutionLimit;
use ruffle_core::{FloatDuration, PlayerBuilder};

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
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--frames" => { frames = args[i + 1].parse().unwrap(); i += 2; }
            "--fps" => { fps = Some(args[i + 1].parse().unwrap()); i += 2; }
            "--spoof-url" => { spoof_url = Some(args[i + 1].clone()); i += 2; }
            "--width" => { width = args[i + 1].parse().unwrap(); i += 2; }
            "--height" => { height = args[i + 1].parse().unwrap(); i += 2; }
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
    let mut builder = PlayerBuilder::new()
        .with_movie(movie)
        .with_log(log.clone())
        .with_autoplay(true)
        .with_viewport_dimensions(width, height, 1.0);
    if let Some(url) = spoof_url {
        builder = builder.with_spoofed_url(Some(url));
    }
    let player = builder.build();

    player.lock().unwrap().preload(&mut ExecutionLimit::exhausted());
    let mut last_frame = 0u16;
    for _ in 0..frames {
        let mut p = player.lock().unwrap();
        // exactly ruffle's runner for a frame-counted test (not tick(): see the guest)
        p.run_frame();
        p.update_timers(frame_time);
        p.audio_mut().tick();
        p.render(); // ruffle's runner renders every frame; it has display-list side effects
        last_frame = p.current_frame().unwrap_or(last_frame);
    }

    let trace = log.take();
    print!("{trace}");
    eprintln!(
        "ruffle: frames={frames} traceBytes={} traceSha1={} lastFrame={last_frame}",
        trace.len(),
        sha1_hex(trace.as_bytes())
    );
}
