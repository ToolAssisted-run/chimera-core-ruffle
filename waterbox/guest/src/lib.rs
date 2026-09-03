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

use ruffle_core::backend::log::LogBackend;
use ruffle_core::limits::ExecutionLimit;
use ruffle_core::tag_utils::SwfMovie;
use ruffle_core::{FloatDuration, Player, PlayerBuilder};

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

struct Machine {
    player: Arc<Mutex<Player>>,
    frame_time: FloatDuration,
    trace: Rc<RefCell<Vec<u8>>>,
    /// The trace flattened for the host: GetTty hands out a pointer into this,
    /// so it must outlive the call and not move while the host reads it.
    tty: Vec<u8>,
    frames: u64,
}

// One machine per sandbox, driven by one thread (vsched runs guest threads one
// at a time), so a static is the honest representation.
static mut MACHINE: Option<Machine> = None;
static mut LOAD_ERROR: [u8; 256] = [0; 256];

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

    let trace = Rc::new(RefCell::new(Vec::new()));
    let player = PlayerBuilder::new()
        .with_movie(movie)
        .with_log(CaptureLog { out: trace.clone() })
        .with_autoplay(true)
        .with_viewport_dimensions(550, 400, 1.0)
        .build();

    player.lock().unwrap().preload(&mut ExecutionLimit::exhausted());

    unsafe {
        *core::ptr::addr_of_mut!(MACHINE) =
            Some(Machine { player, frame_time, trace, tty: Vec::new(), frames: 0 });
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
        let mut p = m.player.lock().unwrap();
        p.tick(m.frame_time);
        p.run_frame();
        p.audio_mut().tick();
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
