/* An OpenGL that never leaves the sandbox.
 *
 * ruffle draws with its own wgpu renderer either way; the only question this
 * file answers is whose OpenGL wgpu is talking to. waterbox/gl-host.c hands the
 * calls to the machine's GPU through the bridge, which is fast and is outside
 * the savestate. Here instead the OpenGL IS code we compiled into the guest:
 * Mesa's softpipe behind the OSMesa front end (waterbox/setup-mesa.sh), plain C
 * with no JIT and no runtime dispatch on host CPU features. Nothing crosses the
 * sandbox boundary, so the picture is decided entirely by the machine's own
 * state and is the same on every machine.
 *
 * OSMesa is the front end that renders into a memory buffer rather than a
 * window, which is the shape a core wants: there is nothing to swap into, and
 * ruffle renders into an offscreen texture of its own and reads that back.
 * The default framebuffer OSMesa insists on therefore only has to exist.
 *
 * Compiled either way. CHIMERA_GUEST_MESA says a guest Mesa was built and
 * linked (waterbox/setup-mesa.sh); without it this file is two refusals, so a
 * machine that cannot build Mesa still gets a core - one that can only draw
 * through the bridge, and says so when a project asks for software.
 */
#include <glad/gl.h>
#include <cstdio>
#include <cstdlib>
#include <cstring>

/* OSMesa's entry points, declared rather than included.
 *
 * <GL/osmesa.h> pulls in Mesa's own <GL/gl.h>, and this file already has
 * glad's, which declares the same types and the same several thousand enums.
 * The OSMesa API is four functions wide, so it is cheaper to say what they are
 * than to referee two headers. */
extern "C"
{
	typedef struct osmesa_context *OSMesaContext;
	typedef void (*OSMESAproc)();

	OSMesaContext OSMesaCreateContextExt(GLenum format, GLint depthBits,
		GLint stencilBits, GLint accumBits, OSMesaContext sharelist);
	GLboolean OSMesaMakeCurrent(OSMesaContext ctx, void *buffer, GLenum type,
		GLsizei width, GLsizei height);
	OSMESAproc OSMesaGetProcAddress(const char *funcName);
}

#define OSMESA_RGBA 0x1908

/* gl-map.cpp owns the one list of extensions this core must not believe in,
 * because the rule is the same whichever OpenGL is underneath. It needs to know
 * which one that is to ask it anything. */
extern "C" void chimera_gl_set_real_loader(void *(*resolve)(const char *));
extern "C" void *chimera_gl_shared_override(const char *name);

#ifdef CHIMERA_GUEST_MESA

namespace {

OSMesaContext g_context;

/* The default framebuffer, which nothing draws into. ruffle renders into a
 * texture it owns and captures that, so this exists only because
 * OSMesaMakeCurrent will not take a null buffer. It is kept alive for the
 * life of the context: Mesa writes into it whenever something touches
 * framebuffer zero, and a freed pointer there would be a write into the heap. */
unsigned char *g_surface;
const int kSurfaceWidth = 64;
const int kSurfaceHeight = 64;

void *resolve(const char *name)
{
	return (void *)OSMesaGetProcAddress(name);
}

} // namespace

/* Brings the in-sandbox OpenGL up. Returns 0 if Mesa will not start, and the
 * caller then fails the load with something a person can act on rather than
 * running on with no picture. */
extern "C" int chimera_gl_software_init(void)
{
	if (g_context != nullptr)
		return 1;

	/* 24 bit depth and 8 bit stencil: ruffle's renderer uses the stencil for
	 * masks, and a context without one loses every masked object silently. */
	g_context = OSMesaCreateContextExt(OSMESA_RGBA, 24, 8, 0, nullptr);
	if (g_context == nullptr) {
		std::fprintf(stderr, "ruffle: OSMesaCreateContext failed\n");
		return 0;
	}

	g_surface = (unsigned char *)std::calloc((size_t)kSurfaceWidth * kSurfaceHeight, 4);
	if (g_surface == nullptr) {
		std::fprintf(stderr, "ruffle: no memory for the OSMesa surface\n");
		return 0;
	}
	if (!OSMesaMakeCurrent(g_context, g_surface, GL_UNSIGNED_BYTE,
			kSurfaceWidth, kSurfaceHeight)) {
		std::fprintf(stderr, "ruffle: OSMesaMakeCurrent failed\n");
		return 0;
	}

	chimera_gl_set_real_loader(resolve);
	return 1;
}

/* The loader wgpu is handed. Everything comes from the Mesa linked in beside
 * us, except the few entry points gl-map.cpp answers for itself - the same ones
 * it answers across the bridge, for the same reasons, which is why they are
 * written down once and asked for here. */
extern "C" void *chimera_gl_lookup_software(const char *name)
{
	if (name == nullptr)
		return nullptr;
	if (void *shared = chimera_gl_shared_override(name))
		return shared;
	return resolve(name);
}

#else /* no guest Mesa in this build */

extern "C" int chimera_gl_software_init(void)
{
	std::fprintf(stderr, "ruffle: this core was built without a guest Mesa; "
		"run waterbox/setup-mesa.sh and build again\n");
	return 0;
}

extern "C" void *chimera_gl_lookup_software(const char *name)
{
	(void)name;
	return nullptr;
}

#endif
