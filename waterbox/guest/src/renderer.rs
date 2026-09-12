//! Whose OpenGL ruffle draws on.
//!
//! ruffle's own wgpu renderer runs in the guest either way; this file decides
//! which OpenGL it is talking to, and that is the whole of the difference
//! between the core's two renderers.
//!
//! **software** is Mesa's softpipe, compiled into the guest
//! behind the OSMesa front end (`waterbox/setup-mesa.sh`,
//! `waterbox/gl-osmesa.cpp`). Nothing leaves the sandbox: the OpenGL is code we
//! compiled, plain C with no JIT and no dispatch on host CPU features, so the
//! picture is decided by the machine's state alone and is the same on every
//! machine. It is slower than a GPU by a large factor, and that is the trade.
//!
//! **opengl-hw** is the host's GPU across the GPU bridge: every GL call leaves
//! through the one callback miniBox allows a guest, and the driver executes it
//! in the same address space, so vertex data and textures are read where they
//! already are. See `waterbox/gl-bridge.h` for the protocol and
//! `tools/gen-gl-bridge.py` for the 709 entry points both sides are generated
//! from. WHAT IT COSTS, said plainly: the GPU is outside the sandbox, so it is
//! outside the savestate and different on every machine. The machine's own
//! state stays deterministic - the frame is computed from the display list, not
//! read back from it - with one exception worth naming: `BitmapData.draw()`
//! rasterises display objects into a bitmap that ActionScript can then read,
//! and there the picture does feed the machine. A movie that does that is not
//! guaranteed to replay across machines.
//!
//! softpipe was tried first, in M4, and written off then as unable to draw
//! ruffle's frames at all. That was wrong, and the reason is in
//! `gl-map.cpp`'s `glsl_carries_explicit_bindings`: the black frame was wgpu
//! trusting a GLSL 3.30 shader to carry bindings it cannot write, which any GL
//! 3.3 driver reproduces - llvmpipe included, once it is told to report 3.30.

use std::ffi::{c_void, CString};
use std::os::raw::c_char;
use std::sync::Arc;

use ruffle_render_wgpu::backend::WgpuRenderBackend;
use ruffle_render_wgpu::descriptors::Descriptors;
use ruffle_render_wgpu::target::TextureTarget;

pub type GuestRenderer = WgpuRenderBackend<TextureTarget>;

extern "C" {
    fn chimera_gl_install(bridge: u64) -> bool;
    /// the shared table, with buffer mapping displaced by gl-map.cpp
    fn chimera_gl_lookup_guest(name: *const c_char) -> *const c_void;
    /// Which host GL context the bridged calls are landing on (GL_OP_CONTEXT_ID).
    /// Every object wgpu holds is a name that context handed out; when this
    /// number changes under a loaded savestate, those names are another
    /// context's and the backend must be rebuilt. Zero means "cannot tell".
    fn chimera_gl_context_id() -> u64;
    /// Brings the guest's own OpenGL up (waterbox/gl-osmesa.cpp). Zero when
    /// this core was built without a guest Mesa, or Mesa refused to start.
    fn chimera_gl_software_init() -> i32;
    /// the guest Mesa's entry points, with the same extension filter
    fn chimera_gl_lookup_software(name: *const c_char) -> *const c_void;
}

/// Which renderer a project asked for. Two names for one wgpu renderer, and
/// the choice is only which OpenGL it draws on.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Which {
    /// Mesa softpipe, inside the sandbox. Deterministic, slow.
    Software,
    /// the machine's GPU, across the bridge. Fast, outside the savestate.
    Hardware,
}

/// What the live backend is drawing on. Read by `context_id`, which must not
/// report a host context while the picture is not coming from one.
static mut IN_USE: Option<Which> = None;

/// Which OpenGL the live backend was built on, once one has been built.
pub fn in_use() -> Option<Which> {
    unsafe { *core::ptr::addr_of!(IN_USE) }
}

/// The id of the host GL context these calls reach, or 0 when it cannot be told
/// (no bridge, or a host too old to answer). A change between two frames means a
/// savestate was loaded into a fresh process and the backend's objects are gone.
pub fn context_id() -> u64 {
    // A software renderer has no host context to change under it: its GL
    // objects are guest memory and a savestate carries them like any other
    // machine state. Answering the bridge's id here would be answering a
    // question about something else, and the caller would rebuild a backend
    // that never went anywhere.
    if unsafe { *core::ptr::addr_of!(IN_USE) } == Some(Which::Software) {
        return 0;
    }
    unsafe { chimera_gl_context_id() }
}

/// The host's callback address, handed over by SetGpuBridge before Init.
static mut BRIDGE: u64 = 0;

pub fn set_bridge(addr: u64) {
    unsafe {
        // install() asks the host how long its opcode list is and refuses a
        // host that is behind this core; a refusal leaves BRIDGE clear, and
        // Init then fails with something a person can act on.
        if chimera_gl_install(addr) {
            *core::ptr::addr_of_mut!(BRIDGE) = addr;
        }
    }
}

pub fn have_bridge() -> bool {
    unsafe { *core::ptr::addr_of!(BRIDGE) != 0 }
}

/// request_device's future is ready immediately on the GL backend. Polling it
/// with an inert waker keeps the executor out of the core: `futures`' LocalPool
/// parks a thread and never polls at all in a single-threaded sandbox (the same
/// trap the navigator hit).
fn block<F: std::future::Future>(mut f: F) -> F::Output {
    use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
    unsafe fn nop(_: *const ()) {}
    unsafe fn clone(p: *const ()) -> RawWaker { RawWaker::new(p, &VT) }
    static VT: RawWakerVTable = RawWakerVTable::new(clone, nop, nop, nop);
    let w = unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VT)) };
    let mut cx = Context::from_waker(&w);
    let mut f = unsafe { std::pin::Pin::new_unchecked(&mut f) };
    for _ in 0..1_000_000 {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) { return v; }
    }
    panic!("renderer: a device future never became ready");
}

/// Build the renderer for a stage `w` x `h` on the OpenGL `which` names.
///
/// For `Hardware` the host has already made its context current on this thread;
/// for `Software` the context is made here, out of the Mesa linked into this
/// binary. Either way wgpu adopts what it is given through the loader and never
/// learns there is a sandbox in the way.
pub fn build(w: u32, h: u32, which: Which) -> Result<GuestRenderer, String> {
    use Which::{Hardware, Software};
    let (w, h) = (w.max(1), h.max(1));
    let loader: fn(&str) -> *const c_void = match which {
        Software => {
            if unsafe { chimera_gl_software_init() } == 0 {
                return Err("the guest's own OpenGL would not start".to_string());
            }
            |sym| match CString::new(sym) {
                Ok(c) => unsafe { chimera_gl_lookup_software(c.as_ptr()) },
                Err(_) => std::ptr::null(),
            }
        }
        Hardware => {
            if !have_bridge() {
                // Not a silent fall back to software. The two renderers draw
                // different pixels, and a run that asked for the GPU and
                // quietly got the softpipe would be a movie that says one
                // thing and was made under another.
                return Err("no GPU bridge: this Chimera did not offer a GL context. \
                    Set the renderer setting to 'software' to draw inside the sandbox instead"
                    .to_string());
            }
            |sym| match CString::new(sym) {
                Ok(c) => unsafe { chimera_gl_lookup_guest(c.as_ptr()) },
                Err(_) => std::ptr::null(),
            }
        }
    };
    // Before the first GL call goes anywhere, so that gl-map.cpp's extension
    // filter asks the right driver. A build that fails after this leaves the
    // flag where the failure was, which is what a later context_id should say.
    unsafe { *core::ptr::addr_of_mut!(IN_USE) = Some(which) };

    let exposed = unsafe {
        wgpu::hal::gles::Adapter::new_external(loader, Default::default())
    }
    .ok_or_else(|| "wgpu would not accept this GL context".to_string())?;

    let mut idesc = wgpu::InstanceDescriptor::new_without_display_handle();
    idesc.backends = wgpu::Backends::GL;
    // wgpu validates indirect draw/dispatch buffers with a COMPUTE shader it
    // builds at device creation. Flash has no indirect draws for it to check,
    // and requiring compute would rule out any GL 3.3 host.
    idesc.flags.remove(wgpu::InstanceFlags::VALIDATION_INDIRECT_CALL);
    let instance = wgpu::Instance::new(idesc);

    let adapter = unsafe { instance.create_adapter_from_hal(exposed) };
    let limits = adapter.limits();
    let (device, queue) = block(adapter.request_device(&wgpu::DeviceDescriptor {
        label: None,
        required_features: wgpu::Features::empty(),
        required_limits: limits,
        memory_hints: Default::default(),
        trace: Default::default(),
        experimental_features: Default::default(),
    }))
    .map_err(|e| format!("no wgpu device on the host's GL: {e:?}"))?;

    let descriptors = Arc::new(Descriptors::new(instance, adapter, device, queue));
    let target = TextureTarget::new(&descriptors.device, (w, h))
        .map_err(|e| format!("offscreen target {w}x{h}: {e:?}"))?;
    WgpuRenderBackend::new(descriptors, target).map_err(|e| format!("ruffle wgpu backend: {e:?}"))
}
