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
    /// Whether the frontend is going to look at the frame (SetRenderingEnabled).
    /// Output only: it gates the readback and nothing the machine can observe.
    rendering: bool,
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

/// ZERO MEANT TWO THINGS, AND ONE OF THEM WAS A LIE (chimera issue 126).
///
/// `Machine::gl_context` starts at 0 - Init builds the backend against the live
/// context and never records which one - and is first written at the bottom of
/// `FrameAdvance`, below the rebuild test. So there is one state in every
/// session that carries 0 while the backend's GL objects already exist: the
/// greenzone's frame-0 anchor, taken right after Init and before the first frame
/// advance. Loading it read that 0 as "the bridge cannot tell, nothing moved",
/// when what it really means is "this state was taken before anyone looked, so
/// it cannot vouch for the objects the driver is holding NOW" - and those are
/// whatever the frames after the anchor left behind.
///
/// It reaches a person because TAStudio goes to a frame by loading the state
/// BEFORE it and emulating one forward, so frames 0 and 1 both load that anchor
/// and frame 2 is the first that does not. Reported on PCSX2 and Maximo: Ghosts
/// to Glory as a corrupt picture from frame 0 or 1 and a clean one from frame 2.
/// The same code was in every bridged core.
///
/// So the host's word for it is kept instead of guessed from the number: the
/// engine tells every core when the machine's memory has been replaced (the
/// StateLoaded export below), and after one of those the stored 0 cannot be
/// trusted. Recording the id in Init instead would put it in the SEALED
/// baseline, where no state carries it as a delta, and the cross-session rebuild
/// the test exists for would stop happening; a non-zero "never seen" sentinel
/// fails identically, because the anchor carries whatever the initial value is.
///
/// This lives outside `Machine` on purpose: it is a fact about what the HOST did
/// a moment ago rather than part of the machine. A state carries it like any
/// other guest byte, which is harmless because it is set AFTER the load - the
/// load cannot wipe it - and cleared the moment it has been read.
static mut STATE_LOADED: bool = false;
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
    // Which OpenGL the picture is drawn on. 'opengl-hw' is the machine's GPU
    // across the bridge: several times faster, and a picture that belongs to a
    // driver rather than to the movie. It is what the PACKAGE declares as its
    // default, so it is what a project gets. 'software' is Mesa's softpipe
    // compiled into this binary: slower, but inside the savestate and the same
    // on every machine. Both run ruffle's one wgpu renderer; see renderer.rs.
    //
    // The fallback below answers a different question - what to draw on when
    // nothing was asked for at all, which only happens to a headless caller
    // that passes no settings. Software is the answer there because it is the
    // one that needs no host GL: falling back to hardware would refuse to
    // start on a machine with no bridge, rather than quietly drawing nothing.
    let which = cfg.choice(
        "renderer",
        &[("software", renderer::Which::Software), ("opengl-hw", renderer::Which::Hardware)],
        renderer::Which::Software,
    );
    // A real GL renderer: see renderer.rs. It is required, not optional - a
    // core that quietly ran without a picture would still pass a trace gate and
    // be useless for a TAS.
    let render_backend = match renderer::build(vw, vh, which) {
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
                rendering: true,
                vsync_num, vsync_den,
                gl_context: 0,
            });
    }
    1
}

/// Told by the engine after every load of the machine - a savestate, a branch
/// file, a greenzone restore - with the machine stopped and before it runs
/// again. See STATE_LOADED above: the only thing this core keeps that a load
/// invalidates is the renderer's claim about which host GL context its objects
/// came from.
#[no_mangle]
pub extern "C" fn StateLoaded() {
    unsafe { *core::ptr::addr_of_mut!(STATE_LOADED) = true };
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
        // First, before the frame's scripts: a movie's ActionScript reaches the
        // renderer too - BitmapData.draw, applyFilter and every getPixel that
        // reads a GPU-drawn bitmap back. Checked after run_frame, as it was, the
        // first frame after a load ran those against the dead backend: reads
        // through names that are gone or reissued came back as zeros or as
        // another object's bytes and were kept as the bitmap's pixels, and a
        // pending GPU copy queued on the old device could panic the frame.
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
        // Taken and cleared whatever the ids say: it has been read into this
        // decision, and a flag left standing would rebuild at some later context
        // change for a load that is long past.
        let after_load = core::mem::replace(&mut *core::ptr::addr_of_mut!(STATE_LOADED), false);
        if live != 0 && live != m.gl_context && (m.gl_context != 0 || after_load) {
            // Every GL name the old backend made becomes stale here. Dropping a
            // handle deletes its objects by name, and a context's names are
            // handed out again: in a new process from 1, and after a rewind the
            // ones freed since. The backend itself goes right below, but
            // ruffle_core's caches let go of their old shapes and bitmaps one by
            // one over the next frames, long after the new backend has been
            // built - and a drop whose number the new backend had been given took
            // the new backend's program or buffer with it: GL_INVALID_VALUE on
            // its next use and parts of the picture gone. gl-map.cpp keeps the
            // new backend off every stale number, so an old drop can only ever
            // free what is old. The null renderer in between lets the old
            // backend's own memory go before the new one is built.
            // Every other bridged core says this on stderr when it happens, and
            // it is the only direct witness that the decision above went the way
            // it did - a rebuild is otherwise just a frame that cost more.
            eprintln!(
                "ruffle: GL objects came from context {}, now {}; rebuilding",
                m.gl_context, live
            );
            renderer::new_gl_generation();
            p.set_renderer(Box::new(ruffle_render::backend::null::NullRenderer::new(
                ruffle_render::backend::ViewportDimensions {
                    width: m.video_w,
                    height: m.video_h,
                    scale_factor: 1.0,
                },
            )));
            match renderer::build(m.video_w, m.video_h, renderer::Which::Hardware) {
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
        inject_edges(&mut p, &mut m.buttons, &mut m.prev_buttons, m.mouse, &mut m.prev_mouse, m.live_pointer, m.text_input);
        p.render();
        // Read the frame back out of the offscreen target. ruffle's own image
        // tests capture exactly here, through the same downcast.
        //
        // Not when nobody is looking: see SetRenderingEnabled. This is the one
        // part of the frame that is pure output, so it is the one part that can
        // be left out without changing the machine.
        if m.rendering {
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

/// Memory domains: a Flash movie has no machine RAM to name.
///
/// The other cores expose a machine's flat RAM here, which is what a RAM search
/// or a watch is for. Flash has no such thing: a movie's state is a garbage
/// collected object graph inside the AVM. So the count stays zero, and what
/// there is to look at is offered as a bus instead - see GetBusCount below.
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

/// The heap, as a bus (chimera#216; user-decided, 2026-10-07).
///
/// Until that day this core published nothing, on the reasoning that the heap
/// of an emulator is not a machine's RAM and would "look searchable and
/// quietly lie". The decision reverses that with its eyes open: it is the only
/// place a movie's values are, and being able to find one, watch it, poke it
/// and read it from a script is worth more than the tidiness of refusing.
///
/// What a person is looking at, then, is RUFFLE's memory and not Flash's:
/// - a number a movie keeps is an f64, eight bytes, wherever the AVM put it;
///   a clip's position is in twentieths of a pixel in a 32-bit integer;
/// - an address is good for as long as the object lives. Objects are freed
///   and their memory reused, and a table that grows moves;
/// - addresses are the same from run to run of one build of this core (the
///   machine is deterministic, the allocator with it) and move with any
///   other build. A project pins its build.
///
/// It is the program break's heap: `sbrk` from its arena's start to the break,
/// which is where the allocator keeps everything under about 230 KiB - the
/// AVM's objects and their tables. Bigger buffers (bitmaps, long arrays) are
/// mapped elsewhere and are not here. The bus is as long as the arena can
/// ever get, because a bus's length is read once and the heap grows; past the
/// break it reads as zeros and ignores writes.
///
/// A BUS and not a domain, for that reason: a domain is a pointer and a
/// length the host reads by itself, and the pages past the break are not
/// there to be read. Reading changes nothing in the machine: a run inside the
/// heap is answered with a pointer to it, and the one buffer used for a run
/// that straddles the break is invisible to savestates.
mod heap_bus {
    #[repr(C)]
    struct Range { start: u64, size: u64 }
    #[repr(C)]
    struct Layout { elf: Range, main_thread: Range, alt_thread: Range, sbrk: Range, sealed: Range, invis: Range, plain: Range, mmap: Range }
    extern "C" {
        // the sandbox's layout, written into the guest when it is loaded (emulibc)
        static __wbxsysinfo: Layout;
        fn sbrk(increment: isize) -> *mut core::ffi::c_void;
    }
    pub const RUN: usize = 65536; // CE_BUS_READ_CHUNK: the most the engine asks for at once
    #[link_section = ".ldata.invis"]
    static mut SCRATCH: [u8; RUN] = [0; RUN];

    pub fn capacity() -> i64 { unsafe { __wbxsysinfo.sbrk.size as i64 } }
    fn start() -> *mut u8 { unsafe { __wbxsysinfo.sbrk.start as *mut u8 } }
    /// How much of the arena is heap right now.
    pub fn live() -> i64 {
        let brk = unsafe { sbrk(0) } as i64;
        (brk - start() as i64).clamp(0, capacity())
    }
    pub fn peek(addr: i64) -> i32 {
        if addr < 0 || addr >= live() { return 0; }
        unsafe { *start().add(addr as usize) as i32 }
    }
    pub fn poke(addr: i64, value: u8) {
        if addr < 0 || addr >= live() { return; }
        unsafe { *start().add(addr as usize) = value; }
    }
    /// Where on the bus an address in this process is, when all `len` bytes of
    /// it are heap right now (`live`, asked once by the caller: it is a call
    /// out of the sandbox).
    pub fn offset_of(addr: usize, len: usize, live: i64) -> Option<i64> {
        let offset = (addr as i64).checked_sub(start() as i64)?;
        if len == 0 || offset < 0 || offset + len as i64 > live { return None; }
        Some(offset)
    }
    pub fn read(addr: i64, len: i32) -> *const u8 {
        let len = (len.max(0) as usize).min(RUN);
        let live = live();
        if addr >= 0 && addr + len as i64 <= live {
            return unsafe { start().add(addr as usize) };
        }
        unsafe {
            let scratch = core::ptr::addr_of_mut!(SCRATCH) as *mut u8;
            core::ptr::write_bytes(scratch, 0, len);
            if addr >= 0 && addr < live {
                core::ptr::copy_nonoverlapping(start().add(addr as usize), scratch, (live - addr) as usize);
            }
            scratch
        }
    }
}

#[no_mangle]
pub extern "C" fn GetBusCount() -> i32 { 1 }

#[no_mangle]
pub extern "C" fn GetBusName(_i: i32) -> *const u8 { b"Heap\0".as_ptr() }

#[no_mangle]
pub extern "C" fn GetBusSize(_i: i32) -> i64 { heap_bus::capacity() }

#[no_mangle]
pub extern "C" fn GetBusWritable(_i: i32) -> i32 { 1 }

#[no_mangle]
pub extern "C" fn PeekBus(bus: i32, addr: i32) -> i32 {
    if bus != 0 { 0 } else { heap_bus::peek(addr as i64) }
}

#[no_mangle]
pub extern "C" fn PokeBus(bus: i32, addr: i32, value: i32) {
    if bus == 0 { heap_bus::poke(addr as i64, value as u8) }
}

/// Up to 64 KiB of the bus at once (chimera engine.h, ce_session_bus_read).
#[no_mangle]
pub extern "C" fn ReadBus(bus: i32, addr: i64, len: i32) -> *const u8 {
    heap_bus::read(if bus != 0 { -1 } else { addr }, len)
}

/// How much of the Heap bus is heap at this moment (the rest reads as zeros).
/// Diagnostic: the gate reads it; nothing in a session does.
#[no_mangle]
pub extern "C" fn GetHeapBytes() -> i64 { heap_bus::live() }

/// A movie's variables as game properties (chimera#216): every ActionScript
/// 1/2 variable that has a place in memory, by the name a script would write
/// (`_root.hero.hp`), as an entry on the Heap bus. The table is DYNAMIC - a
/// variable's slot moves when its object gains a property and is gone when the
/// object is - so the engine asks for the whole list when the user wants to
/// choose, and for one name (GetGameProperty) every time it is about to read.
///
/// Both answers are made WITHOUT TOUCHING THE HEAP. The heap is the machine's
/// state: a `String` built here and dropped would leave the allocator's free
/// lists in another order, and a run where the list was opened would part
/// ways with one where it was not. So the walk (ruffle's chimera_vars)
/// allocates nothing, and what it finds is written into memory the sandbox
/// keeps out of states (.ldata.invis). ALLOCATIONS counts every call the Rust
/// side makes to the allocator, so the gate can see that the count does not
/// move across a listing rather than take it on trust.
mod variables {
    use super::heap_bus;
    use ruffle_core::chimera_vars::{Kind, Place};
    use std::alloc::{GlobalAlloc, Layout, System};

    #[link_section = ".ldata.invis"]
    static mut ALLOCATIONS: u64 = 0;
    #[link_section = ".ldata.invis"]
    static mut LAST_COST: u64 = 0;

    pub struct Counting;
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            ALLOCATIONS += 1;
            System.alloc(layout)
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            ALLOCATIONS += 1;
            System.dealloc(ptr, layout)
        }
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            ALLOCATIONS += 1;
            System.alloc_zeroed(layout)
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            ALLOCATIONS += 1;
            System.realloc(ptr, layout, new_size)
        }
    }

    pub fn allocations() -> u64 { unsafe { ALLOCATIONS } }
    /// How many times the last listing or lookup went to the allocator: 0.
    pub fn last_cost() -> u64 { unsafe { LAST_COST } }

    const LIST_BYTES: usize = 4 << 20;
    const ONE_BYTES: usize = 4096;
    const SEEN_SLOTS: usize = 1 << 16;
    /// The longest string listed; a longer one is listed by its start.
    const STRING_MAX: usize = 4096;
    #[link_section = ".ldata.invis"]
    static mut LIST: [u8; LIST_BYTES] = [0; LIST_BYTES];
    #[link_section = ".ldata.invis"]
    static mut ONE: [u8; ONE_BYTES] = [0; ONE_BYTES];
    #[link_section = ".ldata.invis"]
    static mut SEEN: [usize; SEEN_SLOTS] = [0; SEEN_SLOTS];

    struct Out { buf: *mut u8, cap: usize, len: usize, full: bool, live: i64 }
    impl Out {
        fn put(&mut self, bytes: &[u8]) {
            // one byte is always kept for the NUL
            if self.len + bytes.len() >= self.cap { self.full = true; return; }
            unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), self.buf.add(self.len), bytes.len()); }
            self.len += bytes.len();
        }
        fn text(&mut self, s: &str) {
            self.put(b"\"");
            for b in s.bytes() {
                match b {
                    b'"' => self.put(b"\\\""),
                    b'\\' => self.put(b"\\\\"),
                    _ => self.put(&[b]),
                }
            }
            self.put(b"\"");
        }
        fn number(&mut self, mut n: u64) {
            let mut digits = [0u8; 20];
            let mut at = digits.len();
            loop {
                at -= 1;
                digits[at] = b'0' + (n % 10) as u8;
                n /= 10;
                if n == 0 { break; }
            }
            self.put(&digits[at..]);
        }
        fn end(&mut self) { unsafe { *self.buf.add(self.len) = 0; } }
    }

    /// One entry of the table, or nothing when the place is not on the Heap
    /// bus (a constant in the program, a string too big for the small heap).
    fn entry(out: &mut Out, place: &Place<'_>, first: bool) -> bool {
        let len = if matches!(place.kind, Kind::Latin1 | Kind::Utf16) { place.len.min(STRING_MAX) & !(matches!(place.kind, Kind::Utf16) as usize) } else { place.len };
        let Some(offset) = heap_bus::offset_of(place.addr, len, out.live) else { return false };
        let mark = out.len;
        if !first { out.put(b","); }
        out.put(b"{\"name\":");
        out.text(place.name);
        out.put(b",\"group\":");
        out.text(place.group);
        out.put(b",\"domain\":\"Heap\",\"offset\":");
        out.number(offset as u64);
        match place.kind {
            Kind::Number => out.put(b",\"type\":\"f64\""),
            Kind::Bool => out.put(b",\"type\":\"bool\""),
            Kind::Int => out.put(b",\"type\":\"s32\""),
            Kind::Twips => out.put(b",\"type\":\"s32\",\"description\":\"in twips: 20 to a pixel\""),
            Kind::Frame => out.put(b",\"type\":\"u16\""),
            Kind::Latin1 | Kind::Utf16 => {
                out.put(b",\"type\":\"string\",\"encoding\":");
                out.put(if place.kind == Kind::Latin1 { b"\"latin1\"" } else { b"\"utf16le\"" });
                out.put(b",\"length\":");
                out.number(len as u64);
            }
        }
        if !place.writable { out.put(b",\"writable\":false"); }
        out.put(b"}");
        if out.full { out.len = mark; return false; }
        true
    }

    /// The whole table, as the engine reads one (docs/game-cores.md).
    pub fn list(player: Option<&ruffle_core::Player>) -> *const u8 {
        unsafe {
            let before = ALLOCATIONS;
            let mut out = Out { buf: core::ptr::addr_of_mut!(LIST) as *mut u8, cap: LIST_BYTES - 2, len: 0, full: false, live: heap_bus::live() };
            out.put(b"{\"dynamic\":true,\"properties\":[");
            if let Some(player) = player {
                let seen = &mut *core::ptr::addr_of_mut!(SEEN);
                seen.fill(0);
                let mut count = 0usize;
                player.chimera_variables(seen, &mut |place| {
                    if entry(&mut out, place, count == 0) { count += 1; }
                    !out.full
                });
            }
            // the two bytes kept back: the list is closed even when it was cut short
            out.full = false;
            out.cap = LIST_BYTES;
            out.put(b"]}");
            out.end();
            LAST_COST = ALLOCATIONS - before;
            out.buf
        }
    }

    /// One entry, by name, or an empty string when the name has no place now.
    pub fn one(player: Option<&ruffle_core::Player>, name: &str) -> *const u8 {
        unsafe {
            let before = ALLOCATIONS;
            let mut out = Out { buf: core::ptr::addr_of_mut!(ONE) as *mut u8, cap: ONE_BYTES, len: 0, full: false, live: heap_bus::live() };
            if let Some(player) = player {
                player.chimera_variable(name, &mut |place| { entry(&mut out, place, true); });
            }
            out.end();
            LAST_COST = ALLOCATIONS - before;
            out.buf
        }
    }
}

#[global_allocator]
static ALLOCATOR: variables::Counting = variables::Counting;

#[no_mangle]
pub extern "C" fn GetGameProperties() -> *const u8 {
    unsafe {
        match &*core::ptr::addr_of!(MACHINE) {
            Some(m) => match m.player.try_lock() {
                Ok(player) => variables::list(Some(&player)),
                Err(_) => variables::list(None),
            },
            None => variables::list(None),
        }
    }
}

/// `name` is a NUL-terminated UTF-8 string in the caller's memory.
#[no_mangle]
pub extern "C" fn GetGameProperty(name: *const u8) -> *const u8 {
    unsafe {
        let mut len = 0usize;
        while !name.is_null() && len < 1024 && *name.add(len) != 0 { len += 1; }
        let text = if name.is_null() { "" } else { core::str::from_utf8(core::slice::from_raw_parts(name, len)).unwrap_or("") };
        match &*core::ptr::addr_of!(MACHINE) {
            Some(m) => match m.player.try_lock() {
                Ok(player) => variables::one(Some(&player), text),
                Err(_) => variables::one(None, text),
            },
            None => variables::one(None, text),
        }
    }
}

/// How many times the last GetGameProperties or GetGameProperty went to the
/// allocator. It is 0; the gate checks that it is.
#[no_mangle]
pub extern "C" fn GetVariableListCost() -> i64 { variables::last_cost() as i64 }

/// Every call the Rust side has made to the allocator so far. Diagnostic: the
/// gate's control reads it to see the counter is alive.
#[no_mangle]
pub extern "C" fn GetAllocationCount() -> i64 { variables::allocations() as i64 }

/// The frame's picture, BGRA, as the chimera video contract wants it: a pointer
/// into guest memory the host reads GetVideoWidth x GetVideoHeight pixels from.
/// The host's GL callback, handed over before Init. Without it there is no
/// renderer and Init refuses, rather than running blind: a core that quietly
/// produced no picture would still pass a trace gate and be useless for a TAS.
/// Whether the frontend is going to LOOK at the next frame.
///
/// A seek replays hundreds of frames nobody sees, and a turbo run replays them
/// as fast as the machine will go. Every core that can tell the difference is
/// told, and until now this one could not: it drew, read the picture back off
/// the GPU and converted it, for frames that were thrown away.
///
/// What is skipped is the READBACK and nothing else. `Player::render` still
/// runs, because it is not only drawing: it broadcasts `Event.RENDER` to the
/// display list, updates bitmap caches and sweeps the font caches, and all of
/// that is machine state a movie depends on. Skipping it would be a desync
/// rather than an optimisation. The readback is pure output - a whole-frame
/// copy off the GPU, which blocks until the GPU has finished, and then a
/// per-pixel RGBA to BGRA pass - so leaving it out changes nothing the machine
/// can observe.
///
/// The buffer keeps whatever was last drawn into it, so a frontend that asks
/// for the picture anyway gets the last real one rather than garbage.
#[no_mangle]
pub extern "C" fn SetRenderingEnabled(on: i32) {
    unsafe {
        if let Some(m) = &mut *core::ptr::addr_of_mut!(MACHINE) {
            m.rendering = on != 0;
        }
    }
}

#[no_mangle]
pub extern "C" fn SetGpuBridge(addr: u64) {
    renderer::set_bridge(addr);
}

/// How long a GL crossing costs, measured rather than guessed.
///
/// ruffle's wgpu backend makes about six thousand GL calls in an ordinary
/// frame and fifty thousand in a heavy one, and every one of them leaves the
/// sandbox through a single callback. Whether that is worth batching depends
/// entirely on what one crossing costs, and the only honest way to know is to
/// make a lot of them and divide.
///
/// GL_OP_CONTEXT_ID is the crossing with nothing in it: the host answers from
/// a variable and touches no driver. So this measures the boundary itself and
/// nothing else. The host times the call; the sum is returned so that nothing
/// here can be optimised away.
#[no_mangle]
pub extern "C" fn BenchGlCrossings(count: u64) -> u64 {
    let mut sum = 0u64;
    for _ in 0..count {
        sum = sum.wrapping_add(renderer::context_id());
    }
    sum
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
