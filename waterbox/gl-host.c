/* The host's side of the GPU bridge: a real GL context, and the dispatcher a
 * sandboxed guest calls into.
 *
 * This is the ONLY place in this core where something outside the sandbox does
 * work the machine depends on. Everything about it is deliberately visible:
 * the context is created here, the opcodes are answered here, and a build
 * without CHIMERA_GL_BRIDGE does not link any of it.
 *
 * TWO PLATFORMS, ONE DISPATCHER.
 *
 * The context is headless on both. On Linux that is EGL's surfaceless
 * platform, which Mesa provides with or without a GPU. On Windows it is a
 * window that is never shown: WGL has no headless path, and a pixel format has
 * to come from somewhere, so it comes from a window nobody sees.
 *
 * Above the context, both platforms load their entry points through glad, and
 * that is not tidiness for its own sake. Windows' opengl32.dll exports OpenGL
 * 1.1 and nothing else: every call this renderer makes that is newer than that
 * - shaders, buffers, framebuffers, all of it - has to be fetched at runtime
 * through wglGetProcAddress. Loading the same way on Linux keeps the
 * dispatcher below identical on both, which is the part that must not drift.
 */
#ifdef CHIMERA_GL_BRIDGE

/* glad brings the types and the function pointers; nothing here includes a
 * system GL header, so the two platforms cannot disagree about either. */
#include <glad/gl.h>

#include "gl-bridge.h"
#include "generated/gl-bridge-ops.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#ifdef _WIN32
#include <windows.h>
#else
#include <EGL/egl.h>
#include <EGL/eglext.h>
#ifndef EGL_PLATFORM_SURFACELESS_MESA
#define EGL_PLATFORM_SURFACELESS_MESA 0x31DD
#endif
#endif

static int s_version;

/* ---------------------------------------------------------------------------
 * Windows: a context from a window nobody sees.
 */
#ifdef _WIN32

static HWND s_window;
static HDC s_dc;
static HGLRC s_context;
static HMODULE s_opengl32;

/* wglGetProcAddress answers for the modern entry points and returns null - or
 * one of several unhelpful small integers - for the 1.1 ones, which live in
 * opengl32.dll itself. Every GL loader on Windows carries this same small
 * piece of ugliness; here it is, once. */
static GLADapiproc chimera_gl_loader(const char *name)
{
	PROC p = wglGetProcAddress(name);
	if (p == NULL || p == (PROC)1 || p == (PROC)2 || p == (PROC)3 || p == (PROC)-1)
		p = GetProcAddress(s_opengl32, name);
	return (GLADapiproc)p;
}

static int create_context(char *err, int errlen)
{
	WNDCLASSA wc;
	memset(&wc, 0, sizeof wc);
	wc.lpfnWndProc = DefWindowProcA;
	wc.hInstance = GetModuleHandleA(NULL);
	wc.lpszClassName = "ChimeraGpuBridge";
	RegisterClassA(&wc);

	s_window = CreateWindowExA(0, "ChimeraGpuBridge", "", WS_OVERLAPPEDWINDOW,
		0, 0, 64, 64, NULL, NULL, wc.hInstance, NULL);
	if (s_window == NULL)
	{
		snprintf(err, errlen, "could not create the hidden window");
		return -1;
	}

	s_dc = GetDC(s_window);

	PIXELFORMATDESCRIPTOR pfd;
	memset(&pfd, 0, sizeof pfd);
	pfd.nSize = sizeof pfd;
	pfd.nVersion = 1;
	pfd.dwFlags = PFD_DRAW_TO_WINDOW | PFD_SUPPORT_OPENGL | PFD_DOUBLEBUFFER;
	pfd.iPixelType = PFD_TYPE_RGBA;
	pfd.cColorBits = 32;
	pfd.cDepthBits = 24;
	pfd.cStencilBits = 8;

	const int format = ChoosePixelFormat(s_dc, &pfd);
	if (format == 0 || !SetPixelFormat(s_dc, format, &pfd))
	{
		snprintf(err, errlen, "no usable pixel format");
		return -1;
	}

	s_context = wglCreateContext(s_dc);
	if (s_context == NULL || !wglMakeCurrent(s_dc, s_context))
	{
		snprintf(err, errlen, "wglCreateContext failed");
		return -1;
	}

	s_opengl32 = LoadLibraryA("opengl32.dll");
	if (s_opengl32 == NULL)
	{
		snprintf(err, errlen, "opengl32.dll would not load");
		return -1;
	}

	return 0;
}

#else

/* ---------------------------------------------------------------------------
 * Linux: EGL's surfaceless platform, with a pbuffer to draw into.
 */
static EGLDisplay s_display = EGL_NO_DISPLAY;
static EGLContext s_context = EGL_NO_CONTEXT;
static EGLSurface s_surface = EGL_NO_SURFACE;

static GLADapiproc chimera_gl_loader(const char *name)
{
	return (GLADapiproc)eglGetProcAddress(name);
}

static int create_context(char *err, int errlen)
{
	s_display = eglGetPlatformDisplay(EGL_PLATFORM_SURFACELESS_MESA, EGL_DEFAULT_DISPLAY, NULL);
	if (s_display == EGL_NO_DISPLAY)
	{
		snprintf(err, errlen, "no surfaceless EGL display");
		return -1;
	}

	EGLint major = 0, minor = 0;
	if (!eglInitialize(s_display, &major, &minor))
	{
		snprintf(err, errlen, "eglInitialize failed");
		return -1;
	}

	const EGLint config_attr[] = {
		EGL_SURFACE_TYPE, EGL_PBUFFER_BIT,
		EGL_RENDERABLE_TYPE, EGL_OPENGL_BIT,
		EGL_RED_SIZE, 8, EGL_GREEN_SIZE, 8, EGL_BLUE_SIZE, 8, EGL_ALPHA_SIZE, 8,
		EGL_DEPTH_SIZE, 24,
		EGL_NONE
	};
	EGLConfig config;
	EGLint count = 0;
	if (!eglChooseConfig(s_display, config_attr, &config, 1, &count) || count < 1)
	{
		snprintf(err, errlen, "no usable EGL config");
		return -1;
	}

	/* A surface, even though nothing will ever look at it.
	 *
	 * A renderer draws into framebuffer 0 unless it is told otherwise, and a
	 * context made current with no surface at all HAS no framebuffer 0 - every
	 * draw would be discarded as incomplete. A pbuffer is the smallest way to
	 * give it one: off-screen memory the driver owns, large enough for any
	 * resolution a Dreamcast scans out. */
	const EGLint pbuffer_attr[] = { EGL_WIDTH, 1024, EGL_HEIGHT, 1024, EGL_NONE };

	eglBindAPI(EGL_OPENGL_API);
	s_surface = eglCreatePbufferSurface(s_display, config, pbuffer_attr);
	if (s_surface == EGL_NO_SURFACE)
	{
		snprintf(err, errlen, "eglCreatePbufferSurface failed");
		return -1;
	}

	s_context = eglCreateContext(s_display, config, EGL_NO_CONTEXT, NULL);
	if (s_context == EGL_NO_CONTEXT)
	{
		snprintf(err, errlen, "eglCreateContext failed");
		return -1;
	}
	if (!eglMakeCurrent(s_display, s_surface, s_surface, s_context))
	{
		snprintf(err, errlen, "eglMakeCurrent failed");
		return -1;
	}

	return 0;
}

#endif

/* Brings up a headless GL context and loads its entry points. Returns 0 on
 * success; the caller decides what to do without one, and this core's answer
 * is to use the software rasteriser, which is what it does anyway. */
/* Progress, said out loud and flushed.
 *
 * This path exists on two operating systems and can only be tested on one of
 * them at a time, so when it fails on the other the first question is always
 * "how far did it get". Answering that costs four lines of output and saves a
 * round trip measured in hours. */
const char *chimera_gl_host_description(void);

static void step(const char *what)
{
	fprintf(stderr, "gpu bridge: %s\n", what);
	fflush(stderr);
}

int chimera_gl_host_init(char *err, int errlen)
{
	step("creating a context");
	if (create_context(err, errlen) != 0)
		return -1;

	step("context created, loading entry points");
	s_version = gladLoadGL(chimera_gl_loader);
	if (s_version == 0)
	{
		snprintf(err, errlen, "could not load the GL entry points");
		return -1;
	}

	/* A driver that only offers the old fixed-function pipeline cannot run
	 * this renderer, and saying so here is far kinder than a null call in the
	 * middle of a shader compile. */
	step(chimera_gl_host_description());

	if (GLAD_VERSION_MAJOR(s_version) < 3)
	{
		snprintf(err, errlen, "the driver offers OpenGL %d.%d, and this needs 3.1",
			GLAD_VERSION_MAJOR(s_version), GLAD_VERSION_MINOR(s_version));
		return -1;
	}

	return 0;
}

const char *chimera_gl_host_description(void)
{
	static char described[256];
	const char *renderer = (const char *)glGetString(GL_RENDERER);
	const char *version = (const char *)glGetString(GL_VERSION);
	snprintf(described, sizeof described, "%s on %s",
		version ? version : "?", renderer ? renderer : "?");
	return described;
}

/* ---------------------------------------------------------------------------
 * The dispatcher.
 *
 * Entered from guest code, so it must be sysv64 on every host (MB_GUEST_ABI in
 * miniBox's header says the same thing from the other side). On Windows that
 * is not cosmetic: a win64 callee would spill its shadow space over the
 * caller's stack. The argument block is in guest memory, which is this
 * process's memory too - that is the whole trick - so it is read in place.
 */
#if defined(_WIN32) && defined(__GNUC__)
#define BRIDGE_ABI __attribute__((sysv_abi))
#else
#define BRIDGE_ABI
#endif

static uintptr_t chimera_gl_dispatch_inner(uintptr_t op, uintptr_t a, uintptr_t b,
                                           uintptr_t c, uintptr_t d, uintptr_t e);

/* Extensions the guest must not be told about.
 *
 * buffer_storage makes a buffer IMMUTABLE and is the gateway to persistent
 * mapping - a pointer into the driver's memory, held for the buffer's life.
 * That pointer is the one thing this bridge can never carry. Worse, wgpu takes
 * the extension as licence to allocate with glBufferStorage and then still
 * write through glBufferSubData, which an immutable buffer rejects: every
 * upload fails with GL_INVALID_OPERATION and the frame comes out empty, with
 * nothing anywhere saying why.
 *
 * Hiding the extension is the honest answer rather than a workaround: across
 * this seam the driver genuinely cannot offer it, so we describe a driver that
 * does not. wgpu then allocates mutable buffers and uploads with sub-data,
 * which crosses perfectly well. */
static void chimera_gl_hide_unsupportable(uintptr_t op, uintptr_t buf)
{
	if (op != CHIMERA_GL_OP_glGetStringi || !buf)
		return;
	char *s = (char *)buf;
	/* Renamed, not blanked: an empty string makes the guest's wrapper hand
	 * back NULL, and the caller runs strlen on it. The guest sees an
	 * extension nobody looks for. */
	if (strcmp(s, "GL_ARB_buffer_storage") == 0 || strcmp(s, "GL_EXT_buffer_storage") == 0)
		snprintf(s, 22, "GL_CHIMERA_withheld");
}

/* CHIMERA_GL_CHECK=1 asks the driver, after every crossing, whether that call
 * upset it. A bridge hides GL errors otherwise: the guest never sees them and
 * a wrong frame looks like no frame at all. */
uintptr_t BRIDGE_ABI chimera_gl_host_dispatch(uintptr_t op, uintptr_t a, uintptr_t b,
                                              uintptr_t c, uintptr_t d, uintptr_t e)
{
	static int check = -1;
	if (check < 0) { const char *v = getenv("CHIMERA_GL_CHECK"); check = v && *v && *v != '0'; }
	if (!check) {
		uintptr_t rv = chimera_gl_dispatch_inner(op, a, b, c, d, e);
		chimera_gl_hide_unsupportable(op, b);
		return rv;
	}
	while (glGetError() != GL_NO_ERROR) { }
	uintptr_t r = chimera_gl_dispatch_inner(op, a, b, c, d, e);
	chimera_gl_hide_unsupportable(op, b);
	GLenum err = glGetError();
	if (err != GL_NO_ERROR) {
		fprintf(stderr, "[gl!] op=%lu raised %#x\n", (unsigned long)op, err);
		fflush(stderr);
	}
	return r;
}

static uintptr_t chimera_gl_dispatch_inner(uintptr_t op, uintptr_t a, uintptr_t b,
                                           uintptr_t c, uintptr_t d, uintptr_t e)
{
	(void)d; (void)e;

	/* CHIMERA_GL_TRACE=1 prints every crossing, so a core that dies inside the
	 * driver says which call it died on. */
	static unsigned long s_prev;
	if (getenv("CHIMERA_GL_TRACE")) {
		fprintf(stderr, "[gl] %lu returned; entering op=%lu\n", s_prev, (unsigned long)op);
		fflush(stderr);
		s_prev = (unsigned long)op;
	}

	switch (op)
	{
		case GL_OP_VERSION:
		{
			/* Never hand a host pointer back: the guest cannot read host
			 * memory, and the sandbox is right to stop it. The string is
			 * copied into the buffer the guest supplied. */
			struct GlVersionArgs *args = (struct GlVersionArgs *)a;
			if (!args->out || args->size == 0)
				return 0;
			snprintf((char *)(uintptr_t)args->out, args->size, "%s",
				chimera_gl_host_description());
			return 1;
		}

		case GL_OP_CLEAR_TEST:
		{
			/* Real work on the real device, and the result handed back: clear
			 * to a colour the guest chose and read one pixel of it. If this
			 * comes back right, the boundary carries data both ways. */
			struct GlClearTestArgs *args = (struct GlClearTestArgs *)a;
			GLuint fbo = 0, tex = 0;
			glGenFramebuffers(1, &fbo);
			glGenTextures(1, &tex);
			glBindTexture(GL_TEXTURE_2D, tex);
			glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA8, 8, 8, 0, GL_RGBA, GL_UNSIGNED_BYTE, NULL);
			glBindFramebuffer(GL_FRAMEBUFFER, fbo);
			glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, tex, 0);
			glViewport(0, 0, 8, 8);
			glClearColor(args->r, args->g, args->b, 1.0f);
			glClear(GL_COLOR_BUFFER_BIT);

			uint32_t pixel = 0;
			glReadPixels(0, 0, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, &pixel);
			if (args->pixel_out)
				*(uint32_t *)(uintptr_t)args->pixel_out = pixel;

			glBindFramebuffer(GL_FRAMEBUFFER, 0);
			glDeleteTextures(1, &tex);
			glDeleteFramebuffers(1, &fbo);
			return 1;
		}

		/* the generated cases: one per GL entry point the renderer names */
#include "generated/gl-bridge-host.inc"

		default:
			fprintf(stderr, "gpu bridge: opcode %ld has no case\n", (long)op);
			return 0;
	}
}

#endif /* CHIMERA_GL_BRIDGE */
