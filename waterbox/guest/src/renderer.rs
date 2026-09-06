//! A real GPU, from inside the sandbox.
//!
//! ruffle's own wgpu renderer runs in the guest, but the OpenGL it draws with
//! lives on the host: every GL call leaves through the one callback miniBox
//! allows a guest, and the driver executes it in the same address space, so the
//! vertex data and textures are read where they already are. See
//! `waterbox/gl-bridge.h` for the protocol and `tools/gen-gl-bridge.py` for the
//! 709 entry points both sides are generated from.
//!
//! WHAT THIS COSTS, said plainly: the GPU is outside the sandbox, so it is
//! outside the savestate and different on every machine. The machine's own
//! state stays deterministic - the frame is computed from the display list, not
//! read back from it - with one exception worth naming: `BitmapData.draw()`
//! rasterises display objects into a bitmap that ActionScript can then read,
//! and there the picture does feed the machine. A movie that does that is not
//! guaranteed to replay across machines.
//!
//! (The alternative, a software rasteriser inside the sandbox, was built and
//! rejected: Mesa's softpipe cannot draw ruffle's frames - it reads ruffle's
//! second uniform block as zeros and every triangle collapses - and llvmpipe,
//! which does work, would mean carrying LLVM in the guest.)

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
}

/// The id of the host GL context these calls reach, or 0 when it cannot be told
/// (no bridge, or a host too old to answer). A change between two frames means a
/// savestate was loaded into a fresh process and the backend's objects are gone.
pub fn context_id() -> u64 {
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

/// Build the renderer for a stage `w` x `h`. The host has already made its
/// context current on this thread; wgpu adopts it through the loader and never
/// learns there is a sandbox in the way.
pub fn build(w: u32, h: u32) -> Result<GuestRenderer, String> {
    let (w, h) = (w.max(1), h.max(1));
    if !have_bridge() {
        return Err("no GPU bridge: the host did not offer a GL context".to_string());
    }

    let exposed = unsafe {
        wgpu::hal::gles::Adapter::new_external(
            |sym| {
                match CString::new(sym) {
                    Ok(c) => unsafe { chimera_gl_lookup_guest(c.as_ptr()) },
                    Err(_) => std::ptr::null(),
                }
            },
            Default::default(),
        )
    }
    .ok_or_else(|| "wgpu would not accept the host's GL context".to_string())?;

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
