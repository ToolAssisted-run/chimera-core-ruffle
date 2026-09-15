/* Buffer mapping, across a boundary a pointer cannot cross.
 *
 * glMapBufferRange hands back a pointer into the DRIVER's memory. That is the
 * one thing this bridge can never carry: the guest is not allowed to read host
 * memory, and the sandbox is right to stop it. wgpu maps a buffer for every
 * upload and every readback, so "just don't map" is not an option either.
 *
 * So mapping is emulated on the guest's side. A map allocates ordinary guest
 * memory and, when the caller intends to read (or to write only part of the
 * range), fills it from the real buffer first. An unmap - or an explicit flush
 * - writes it back. Both directions travel as ordinary glGetBufferSubData /
 * glBufferSubData calls, which carry guest pointers and therefore cross
 * happily. The driver never sees a guest address it did not already handle,
 * and wgpu never learns that its pointer was not the driver's.
 *
 * The cost is a copy per map, which is what any remote GL has to pay.
 */
#include <glad/gl.h>
#include <cstdlib>
#include <cstring>
#include <cstdio>
#include <vector>

extern "C" void *chimera_gl_lookup(const char *name);   /* the generated table */

namespace {

struct Mapping {
	GLuint buffer;   /* a map belongs to the BUFFER, not to the target it */
	GLenum target;   /* happened to be bound to when it was made */
	GLintptr offset;
	GLsizeiptr length;
	GLbitfield access;
	void *staging;
};

/* One live mapping per buffer target is all GL allows, and wgpu uses a couple
 * of targets at a time. */
Mapping g_maps[16];

/* Which buffer is bound to a target right now. A map belongs to the buffer:
 * GL lets the caller bind something else to the same target while a mapping is
 * live, and keying by target would then write one buffer's staging into
 * another's - or free a pointer its owner is still using. */
GLenum binding_of(GLenum target)
{
	switch (target) {
	case GL_ARRAY_BUFFER:              return GL_ARRAY_BUFFER_BINDING;
	case GL_ELEMENT_ARRAY_BUFFER:      return GL_ELEMENT_ARRAY_BUFFER_BINDING;
	case GL_COPY_READ_BUFFER:          return GL_COPY_READ_BUFFER_BINDING;
	case GL_COPY_WRITE_BUFFER:         return GL_COPY_WRITE_BUFFER_BINDING;
	case GL_PIXEL_PACK_BUFFER:         return GL_PIXEL_PACK_BUFFER_BINDING;
	case GL_PIXEL_UNPACK_BUFFER:       return GL_PIXEL_UNPACK_BUFFER_BINDING;
	case GL_UNIFORM_BUFFER:            return GL_UNIFORM_BUFFER_BINDING;
	case GL_TRANSFORM_FEEDBACK_BUFFER: return GL_TRANSFORM_FEEDBACK_BUFFER_BINDING;
	case GL_SHADER_STORAGE_BUFFER:     return GL_SHADER_STORAGE_BUFFER_BINDING;
	case GL_DRAW_INDIRECT_BUFFER:      return GL_DRAW_INDIRECT_BUFFER_BINDING;
	case GL_ATOMIC_COUNTER_BUFFER:     return GL_ATOMIC_COUNTER_BUFFER_BINDING;
	case GL_TEXTURE_BUFFER:            return GL_TEXTURE_BUFFER_BINDING;
	default:                           return 0;
	}
}

GLuint bound_buffer(GLenum target);

Mapping *find(GLenum target)
{
	GLuint buf = bound_buffer(target);
	for (auto &m : g_maps)
		if (m.staging && m.buffer == buf)
			return &m;
	return nullptr;
}

Mapping *slot()
{
	for (auto &m : g_maps)
		if (!m.staging)
			return &m;
	return nullptr;
}

template <typename Fn> Fn gl(const char *name)
{
	return (Fn)chimera_gl_lookup(name);
}

GLuint bound_buffer(GLenum target)
{
	GLenum pname = binding_of(target);
	if (!pname)
		return 0;
	auto getiv = gl<void (*)(GLenum, GLint *)>("glGetIntegerv");
	GLint id = 0;
	if (getiv)
		getiv(pname, &id);
	return (GLuint)id;
}

} // namespace

extern "C" void *chimera_gl_map_buffer_range(GLenum target, GLintptr offset,
                                             GLsizeiptr length, GLbitfield access)
{
	Mapping *m = slot();
	if (!m || length <= 0)
		return nullptr;
	void *staging = calloc(1, (size_t)length);
	if (!staging)
		return nullptr;

	/* A read map obviously needs the current contents; so does a write map
	 * that has not promised to overwrite the whole range, because the bytes it
	 * leaves alone must survive the write-back. */
	const bool wants_existing =
		(access & GL_MAP_READ_BIT) ||
		!(access & (GL_MAP_INVALIDATE_RANGE_BIT | GL_MAP_INVALIDATE_BUFFER_BIT));
	if (wants_existing) {
		auto get = gl<void (*)(GLenum, GLintptr, GLsizeiptr, void *)>("glGetBufferSubData");
		if (get)
			get(target, offset, length, staging);
	}

	if (getenv("CHIMERA_GL_MAPTRACE")) {
		const unsigned char *b = (const unsigned char *)staging;
		fprintf(stderr, "[map] target=%#x buf=%u off=%ld len=%ld access=%#x read=%d first=%02x%02x%02x%02x\n",
		        target, bound_buffer(target), (long)offset, (long)length, access,
		        (int)wants_existing, b[0], b[1], b[2], b[3]);
		fflush(stderr);
	}
	m->buffer = bound_buffer(target);
	m->target = target;
	m->offset = offset;
	m->length = length;
	m->access = access;
	m->staging = staging;
	return staging;
}

extern "C" void *chimera_gl_map_buffer(GLenum target, GLenum access)
{
	auto param = gl<void (*)(GLenum, GLenum, GLint *)>("glGetBufferParameteriv");
	GLint size = 0;
	if (param)
		param(target, GL_BUFFER_SIZE, &size);
	if (size <= 0)
		return nullptr;
	GLbitfield bits = 0;
	if (access == GL_READ_ONLY || access == GL_READ_WRITE) bits |= GL_MAP_READ_BIT;
	if (access == GL_WRITE_ONLY || access == GL_READ_WRITE) bits |= GL_MAP_WRITE_BIT;
	return chimera_gl_map_buffer_range(target, 0, size, bits);
}

extern "C" void chimera_gl_flush_mapped_buffer_range(GLenum target, GLintptr offset,
                                                     GLsizeiptr length)
{
	Mapping *m = find(target);
	if (!m || length <= 0)
		return;
	auto sub = gl<void (*)(GLenum, GLintptr, GLsizeiptr, const void *)>("glBufferSubData");
	if (sub)
		sub(target, m->offset + offset, length, (const char *)m->staging + offset);
}

extern "C" GLboolean chimera_gl_unmap_buffer(GLenum target)
{
	Mapping *m = find(target);
	if (!m)
		return GL_FALSE;
	/* With FLUSH_EXPLICIT the caller has already said which parts matter and
	 * flushed them; writing the whole range again would only undo nothing. */
	if ((m->access & GL_MAP_WRITE_BIT) && !(m->access & GL_MAP_FLUSH_EXPLICIT_BIT)) {
		auto sub = gl<void (*)(GLenum, GLintptr, GLsizeiptr, const void *)>("glBufferSubData");
		if (sub)
			sub(target, m->offset, m->length, m->staging);
	}
	free(m->staging);
	m->staging = nullptr;
	return GL_TRUE;
}

extern "C" void chimera_gl_get_buffer_pointerv(GLenum target, GLenum pname, void **params)
{
	if (!params)
		return;
	Mapping *m = find(target);
	*params = (pname == GL_BUFFER_MAP_POINTER && m) ? m->staging : nullptr;
}


/* Which OpenGL the wrappers below ask.
 *
 * The bridge's generated table by default - the machine's own GPU, on the far
 * side of the one callback a guest gets. A project that asked for the software
 * renderer instead puts the guest Mesa here (waterbox/gl-osmesa.cpp), and
 * everything below is then asking code that never leaves the sandbox. The
 * questions, and the answers this file refuses to pass on, are the same either
 * way, which is why there is one copy of them and not two.
 */
static void *(*g_real_loader)(const char *) = chimera_gl_lookup;

extern "C" void chimera_gl_set_real_loader(void *(*resolve)(const char *))
{
	if (resolve != nullptr)
		g_real_loader = resolve;
}

/* Whether this OpenGL's shading language can carry `layout(binding = N)`.
 *
 * This decides a question that is not about the extension it is asked through.
 * naga writes explicit bindings into the GLSL it generates only from desktop
 * GLSL 4.20 (or GLES 3.10) up; below that the bindings have to be assigned
 * after the link, with glUniformBlockBinding and glUniform1i. wgpu knows how to
 * do that - and decides WHETHER to from whether the driver has compute shaders,
 * which is a different question with a different answer on exactly the drivers
 * that matter here. Mesa's softpipe is GL 3.3 (GLSL 3.30) AND offers
 * GL_ARB_compute_shader, so wgpu leaves the bindings to a shader that could not
 * write them: every uniform block lands on binding 0, ruffle's second block
 * reads as zeros, every vertex collapses to the origin and the frame comes back
 * black with no GL error anywhere. Any GL 3.3 host GPU does the same across the
 * bridge.
 *
 * So the answer is the version, asked once. A context is never swapped under a
 * live loader - a new one means a new backend - so once is enough.
 */
static bool glsl_carries_explicit_bindings()
{
	static int answer = -1;
	if (answer >= 0)
		return answer != 0;
	auto get_string = (const GLubyte *(*)(GLenum))g_real_loader("glGetString");
	const GLubyte *v = get_string ? get_string(GL_SHADING_LANGUAGE_VERSION) : nullptr;
	/* "4.50" or "3.30", possibly with a vendor suffix. Unreadable means
	 * "assume not": the post-link path is correct on every version, only
	 * slightly more work, while guessing the other way draws nothing. */
	int major = 0, minor = 0;
	answer = (v && std::sscanf((const char *)v, "%d.%d", &major, &minor) == 2
		&& (major * 100 + minor) >= 420) ? 1 : 0;
	return answer != 0;
}

/* Extensions this core must not believe in, whoever is hosting it.
 *
 * buffer_storage makes a buffer IMMUTABLE and is the gateway to persistent
 * mapping - a pointer into the driver's memory, held for the buffer's life,
 * which is the one thing this bridge can never carry. Worse, wgpu takes the
 * extension as licence to allocate with glBufferStorage and then still write
 * through glBufferSubData, which an immutable buffer rejects: every upload
 * fails with GL_INVALID_OPERATION and the frame comes out empty with nothing
 * anywhere saying why. That second half is true of the guest Mesa too, which
 * offers the extension and is not across any seam, so this one is withheld
 * there as well.
 *
 * compute_shader is withheld only where the shading language cannot carry
 * explicit bindings, and only because wgpu reads it as the answer to that
 * question (see above). This core never runs a compute shader: ruffle's
 * renderer has none, and wgpu's only use for one is validating indirect draws,
 * which renderer.rs already turns off.
 *
 * The filtering belongs HERE rather than in a host, because every host would
 * otherwise have to know this core's business. The core is told a truth about
 * itself: renamed rather than blanked, because an empty string makes the
 * generated wrapper answer NULL and the caller runs strlen on it.
 */
static bool withheld(const char *ext)
{
	if (std::strcmp(ext, "GL_ARB_buffer_storage") == 0
	    || std::strcmp(ext, "GL_EXT_buffer_storage") == 0)
		return true;
	if (std::strcmp(ext, "GL_ARB_compute_shader") == 0)
		return !glsl_carries_explicit_bindings();
	return false;
}

extern "C" const GLubyte *chimera_gl_get_stringi(GLenum name, GLuint index)
{
	auto real = (const GLubyte *(*)(GLenum, GLuint))g_real_loader("glGetStringi");
	const GLubyte *s = real ? real(name, index) : nullptr;
	if (s && name == GL_EXTENSIONS && withheld((const char *)s))
		return (const GLubyte *)"GL_CHIMERA_withheld";
	return s;
}


/* Multisampling, asked for in a quantity the driver may not have.
 *
 * wgpu's GL backend reports 2x and 4x multisampling as available on every
 * driver whose GL_MAX_SAMPLES is below 8 - it reads a low answer as an iOS
 * Safari quirk and overrides it. ruffle then asks its surface for the sample
 * count its quality setting names (4 at the default 'high'), and on a driver
 * that has none - Mesa's softpipe has exactly one sample - allocating the
 * renderbuffer fails with GL_INVALID_OPERATION, the framebuffer is incomplete
 * from then on, and every clear and every draw is refused. The frame comes back
 * black; the only trace is one GL error nobody is reading.
 *
 * So the count is clamped here, where the driver's real answer is available.
 * Multisampling in GL is a property of the framebuffer's attachments and not of
 * the pipeline, so clamping every attachment leaves a consistent, complete,
 * single-sampled framebuffer: the resolve blit becomes a copy and the picture
 * comes out without anti-aliasing rather than not at all. That is the honest
 * trade for a rasteriser that cannot multisample, and it is written down in the
 * renderer setting's description.
 */
static GLsizei clamped_samples(GLsizei samples)
{
	static GLint max = -1;
	if (max < 0) {
		auto get_integerv = (void (*)(GLenum, GLint *))g_real_loader("glGetIntegerv");
		max = 0;
		if (get_integerv)
			get_integerv(GL_MAX_SAMPLES, &max);
		if (max < 1)
			max = 1;
	}
	return samples > max ? (GLsizei)max : samples;
}

extern "C" void chimera_gl_renderbuffer_storage_multisample(GLenum target,
	GLsizei samples, GLenum internalformat, GLsizei width, GLsizei height)
{
	auto real = (void (*)(GLenum, GLsizei, GLenum, GLsizei, GLsizei))
		g_real_loader("glRenderbufferStorageMultisample");
	if (real)
		real(target, clamped_samples(samples), internalformat, width, height);
}

extern "C" void chimera_gl_tex_storage_2d_multisample(GLenum target, GLsizei samples,
	GLenum internalformat, GLsizei width, GLsizei height, GLboolean fixedsamplelocations)
{
	auto real = (void (*)(GLenum, GLsizei, GLenum, GLsizei, GLsizei, GLboolean))
		g_real_loader("glTexStorage2DMultisample");
	if (real)
		real(target, clamped_samples(samples), internalformat, width, height,
			fixedsamplelocations);
}

extern "C" void chimera_gl_tex_image_2d_multisample(GLenum target, GLsizei samples,
	GLenum internalformat, GLsizei width, GLsizei height, GLboolean fixedsamplelocations)
{
	auto real = (void (*)(GLenum, GLsizei, GLenum, GLsizei, GLsizei, GLboolean))
		g_real_loader("glTexImage2DMultisample");
	if (real)
		real(target, clamped_samples(samples), internalformat, width, height,
			fixedsamplelocations);
}

/* What both loaders answer for themselves, whichever OpenGL is underneath:
 * the extension string this core must not believe, and the three entry points
 * that allocate multisampled storage. */
extern "C" void *chimera_gl_shared_override(const char *name)
{
	if (name == nullptr)
		return nullptr;
	if (std::strcmp(name, "glGetStringi") == 0)
		return (void *)chimera_gl_get_stringi;
	if (std::strcmp(name, "glRenderbufferStorageMultisample") == 0)
		return (void *)chimera_gl_renderbuffer_storage_multisample;
	if (std::strcmp(name, "glTexStorage2DMultisample") == 0)
		return (void *)chimera_gl_tex_storage_2d_multisample;
	if (std::strcmp(name, "glTexImage2DMultisample") == 0)
		return (void *)chimera_gl_tex_image_2d_multisample;
	return nullptr;
}

/* Names from a context the renderer has already given up.
 *
 * A GL name is a small integer the context hands out, and hands out AGAIN once
 * it is free. When the host context changes under a loaded savestate - a
 * rewind, a reopen - the core builds a fresh backend (see lib.rs), but
 * ruffle_core still holds shapes, bitmaps and glyphs registered with the old
 * one. Those are let go lazily, as each cache notices the render epoch moved,
 * and letting go of one deletes its buffers and textures by name. If the new
 * backend has been handed one of those numbers in the meantime, the old
 * handle's drop deletes the new backend's object: its next glBufferSubData is
 * refused with GL_INVALID_VALUE, and parts of the picture stop drawing.
 * Measured on a rewind: most of the background gone, and a run of refused
 * uploads after every rebuild.
 *
 * Telling the two apart at the delete is impossible - it is the same number -
 * so the numbers are kept from ever being the same. Every name is noted when it
 * is made. At a new generation the names still noted become STALE, and from
 * then on a gen that the driver answers with a stale number keeps that number
 * back and asks again: the new backend only ever holds numbers no old handle
 * holds. A delete of a stale name is the old handle letting go, and goes to the
 * driver - freeing what the old context left there, or the blank name held back
 * - after which the number is free for anyone. A delete of a name that is
 * neither is a double delete, and stops here.
 *
 * Programs and shaders share one namespace in GL, so they share one table. The
 * tables are guest memory, so a savestate carries them. */
namespace {

enum NameKind { kBuffer, kTexture, kFramebuffer, kRenderbuffer, kVertexArray,
	kSampler, kQuery, kProgramOrShader, kProgramPipeline, kTransformFeedback, kNameKinds };

enum : unsigned char { kFree = 0, kCurrent = 1, kStale = 2 };

std::vector<unsigned char> g_names[kNameKinds];

unsigned char &name_state(NameKind k, GLuint name)
{
	if (g_names[k].size() <= name) g_names[k].resize((size_t)name + 1024, kFree);
	return g_names[k][name];
}

unsigned char state_of(NameKind k, GLuint name)
{
	return g_names[k].size() > name ? g_names[k][name] : kFree;
}

/* After the driver filled `names`: note the fresh ones, and swap each stale one
 * for another from `again` (which makes one name at a time). The stale number
 * stays stale; its old holder's delete is what frees it. */
template <typename Again> void note_made(NameKind k, GLsizei n, GLuint *names, Again again)
{
	if (!names) return;
	for (GLsizei i = 0; i < n; i++) {
		int tries = 0;
		while (names[i] != 0 && state_of(k, names[i]) == kStale && tries++ < 1 << 20)
			names[i] = again();
		if (names[i] != 0) name_state(k, names[i]) = kCurrent;
	}
}

/* Whether a delete of this name should reach the driver, and the bookkeeping
 * for it: a current or stale name is freed, anything else was never ours. */
bool release(NameKind k, GLuint name)
{
	if (name == 0 || state_of(k, name) == kFree) return false;
	g_names[k][name] = kFree;
	return true;
}

template <typename Del> void delete_ours(NameKind k, GLsizei n, const GLuint *names, const char *entry)
{
	auto real = (Del)chimera_gl_lookup(entry);
	if (!real || !names || n <= 0) return;
	GLuint small[16];
	std::vector<GLuint> big;
	GLuint *keep = small;
	if (n > 16) { big.resize((size_t)n); keep = big.data(); }
	GLsizei kept = 0;
	for (GLsizei i = 0; i < n; i++)
		if (release(k, names[i])) keep[kept++] = names[i];
	if (kept) real(kept, keep);
}

} // namespace

extern "C" void chimera_gl_new_generation(void)
{
	for (auto &v : g_names)
		for (auto &st : v)
			if (st == kCurrent) st = kStale;
}

#define CHIMERA_GEN_WRAP(kind, gen, del)                                         \
	static void GLAD_API_PTR chimera_##gen(GLsizei n, GLuint *names)             \
	{                                                                            \
		auto real = (void (GLAD_API_PTR *)(GLsizei, GLuint *))chimera_gl_lookup(#gen); \
		if (!real) return;                                                       \
		real(n, names);                                                          \
		note_made(kind, n, names, [real] { GLuint x = 0; real(1, &x); return x; }); \
	}                                                                            \
	static void GLAD_API_PTR chimera_##del(GLsizei n, const GLuint *names)       \
	{                                                                            \
		delete_ours<void (GLAD_API_PTR *)(GLsizei, const GLuint *)>(kind, n, names, #del); \
	}

CHIMERA_GEN_WRAP(kBuffer, glGenBuffers, glDeleteBuffers)
CHIMERA_GEN_WRAP(kTexture, glGenTextures, glDeleteTextures)
CHIMERA_GEN_WRAP(kFramebuffer, glGenFramebuffers, glDeleteFramebuffers)
CHIMERA_GEN_WRAP(kRenderbuffer, glGenRenderbuffers, glDeleteRenderbuffers)
CHIMERA_GEN_WRAP(kVertexArray, glGenVertexArrays, glDeleteVertexArrays)
CHIMERA_GEN_WRAP(kSampler, glGenSamplers, glDeleteSamplers)
CHIMERA_GEN_WRAP(kQuery, glGenQueries, glDeleteQueries)
CHIMERA_GEN_WRAP(kProgramPipeline, glGenProgramPipelines, glDeleteProgramPipelines)
CHIMERA_GEN_WRAP(kTransformFeedback, glGenTransformFeedbacks, glDeleteTransformFeedbacks)

/* The direct-state-access makers hand out the same names as their glGen twins. */
#define CHIMERA_CREATE_WRAP(kind, create)                                        \
	static void GLAD_API_PTR chimera_##create(GLsizei n, GLuint *names)          \
	{                                                                            \
		auto real = (void (GLAD_API_PTR *)(GLsizei, GLuint *))chimera_gl_lookup(#create); \
		if (!real) return;                                                       \
		real(n, names);                                                          \
		note_made(kind, n, names, [real] { GLuint x = 0; real(1, &x); return x; }); \
	}
CHIMERA_CREATE_WRAP(kBuffer, glCreateBuffers)
CHIMERA_CREATE_WRAP(kFramebuffer, glCreateFramebuffers)
CHIMERA_CREATE_WRAP(kRenderbuffer, glCreateRenderbuffers)
CHIMERA_CREATE_WRAP(kVertexArray, glCreateVertexArrays)
CHIMERA_CREATE_WRAP(kSampler, glCreateSamplers)
CHIMERA_CREATE_WRAP(kProgramPipeline, glCreateProgramPipelines)
CHIMERA_CREATE_WRAP(kTransformFeedback, glCreateTransformFeedbacks)

static void GLAD_API_PTR chimera_glCreateTextures(GLenum target, GLsizei n, GLuint *names)
{
	auto real = (void (GLAD_API_PTR *)(GLenum, GLsizei, GLuint *))chimera_gl_lookup("glCreateTextures");
	if (!real) return;
	real(target, n, names);
	note_made(kTexture, n, names, [real, target] { GLuint x = 0; real(target, 1, &x); return x; });
}

static void GLAD_API_PTR chimera_glCreateQueries(GLenum target, GLsizei n, GLuint *names)
{
	auto real = (void (GLAD_API_PTR *)(GLenum, GLsizei, GLuint *))chimera_gl_lookup("glCreateQueries");
	if (!real) return;
	real(target, n, names);
	note_made(kQuery, n, names, [real, target] { GLuint x = 0; real(target, 1, &x); return x; });
}

static GLuint GLAD_API_PTR chimera_glCreateProgram(void)
{
	auto real = (GLuint (GLAD_API_PTR *)(void))chimera_gl_lookup("glCreateProgram");
	GLuint name = real ? real() : 0;
	note_made(kProgramOrShader, 1, &name, [real] { return real(); });
	return name;
}

static GLuint GLAD_API_PTR chimera_glCreateShader(GLenum type)
{
	auto real = (GLuint (GLAD_API_PTR *)(GLenum))chimera_gl_lookup("glCreateShader");
	GLuint name = real ? real(type) : 0;
	note_made(kProgramOrShader, 1, &name, [real, type] { return real(type); });
	return name;
}

static void GLAD_API_PTR chimera_glDeleteProgram(GLuint name)
{
	auto real = (void (GLAD_API_PTR *)(GLuint))chimera_gl_lookup("glDeleteProgram");
	auto is = (GLboolean (GLAD_API_PTR *)(GLuint))chimera_gl_lookup("glIsProgram");
	const bool stale = state_of(kProgramOrShader, name) == kStale;
	if (!real || !release(kProgramOrShader, name)) return;
	/* Unlike a buffer or a texture, a program or shader name the driver does not
	 * know is an error (GL_INVALID_VALUE), and after a reopen none of the old
	 * ones exist. Only a stale name is asked about; this generation's are real. */
	if (stale && is && !is(name)) return;
	real(name);
}

static void GLAD_API_PTR chimera_glDeleteShader(GLuint name)
{
	auto real = (void (GLAD_API_PTR *)(GLuint))chimera_gl_lookup("glDeleteShader");
	auto is = (GLboolean (GLAD_API_PTR *)(GLuint))chimera_gl_lookup("glIsShader");
	const bool stale = state_of(kProgramOrShader, name) == kStale;
	if (!real || !release(kProgramOrShader, name)) return;
	/* Unlike a buffer or a texture, a program or shader name the driver does not
	 * know is an error (GL_INVALID_VALUE), and after a reopen none of the old
	 * ones exist. Only a stale name is asked about; this generation's are real. */
	if (stale && is && !is(name)) return;
	real(name);
}

static void *generation_override(const char *name)
{
	static const struct { const char *name; void *fn; } table[] = {
		{ "glGenBuffers", (void *)chimera_glGenBuffers },
		{ "glDeleteBuffers", (void *)chimera_glDeleteBuffers },
		{ "glGenTextures", (void *)chimera_glGenTextures },
		{ "glDeleteTextures", (void *)chimera_glDeleteTextures },
		{ "glGenFramebuffers", (void *)chimera_glGenFramebuffers },
		{ "glDeleteFramebuffers", (void *)chimera_glDeleteFramebuffers },
		{ "glGenRenderbuffers", (void *)chimera_glGenRenderbuffers },
		{ "glDeleteRenderbuffers", (void *)chimera_glDeleteRenderbuffers },
		{ "glGenVertexArrays", (void *)chimera_glGenVertexArrays },
		{ "glDeleteVertexArrays", (void *)chimera_glDeleteVertexArrays },
		{ "glGenSamplers", (void *)chimera_glGenSamplers },
		{ "glDeleteSamplers", (void *)chimera_glDeleteSamplers },
		{ "glGenQueries", (void *)chimera_glGenQueries },
		{ "glDeleteQueries", (void *)chimera_glDeleteQueries },
		{ "glGenProgramPipelines", (void *)chimera_glGenProgramPipelines },
		{ "glDeleteProgramPipelines", (void *)chimera_glDeleteProgramPipelines },
		{ "glGenTransformFeedbacks", (void *)chimera_glGenTransformFeedbacks },
		{ "glDeleteTransformFeedbacks", (void *)chimera_glDeleteTransformFeedbacks },
		{ "glCreateBuffers", (void *)chimera_glCreateBuffers },
		{ "glCreateFramebuffers", (void *)chimera_glCreateFramebuffers },
		{ "glCreateRenderbuffers", (void *)chimera_glCreateRenderbuffers },
		{ "glCreateVertexArrays", (void *)chimera_glCreateVertexArrays },
		{ "glCreateSamplers", (void *)chimera_glCreateSamplers },
		{ "glCreateProgramPipelines", (void *)chimera_glCreateProgramPipelines },
		{ "glCreateTransformFeedbacks", (void *)chimera_glCreateTransformFeedbacks },
		{ "glCreateTextures", (void *)chimera_glCreateTextures },
		{ "glCreateQueries", (void *)chimera_glCreateQueries },
		{ "glCreateProgram", (void *)chimera_glCreateProgram },
		{ "glCreateShader", (void *)chimera_glCreateShader },
		{ "glDeleteProgram", (void *)chimera_glDeleteProgram },
		{ "glDeleteShader", (void *)chimera_glDeleteShader },
	};
	for (const auto &e : table)
		if (std::strcmp(name, e.name) == 0)
			return e.fn;
	return nullptr;
}

/* CHIMERA_GL_STALETRACE: is anything still USING a name from before the
 * rebuild? A bind of a name the current generation did not make is an old
 * handle drawing - one a ruffle_core cache kept past the render epoch. Printed
 * once per name, with its kind. Only wired in when the variable is set at the
 * time the backend looks its entry points up, so it costs nothing otherwise. */
namespace {

const char *state_word(unsigned char st)
{
	return st == kStale ? "stale (old handle not yet dropped)" : "free (old handle already dropped)";
}

void stale_use(NameKind k, GLuint name, const char *entry)
{
	if (name == 0) return;
	unsigned char st = state_of(k, name);
	if (st == kCurrent) return;
	static std::vector<unsigned char> told[kNameKinds];
	if (told[k].size() <= name) told[k].resize((size_t)name + 1024, 0);
	if (told[k][name]) return;
	told[k][name] = 1;
	fprintf(stderr, "[gl-stale] %s(%u): %s\n", entry, name, state_word(st));
	fflush(stderr);
}

} // namespace

static void GLAD_API_PTR trace_glBindTexture(GLenum target, GLuint name)
{
	static auto real = (void (GLAD_API_PTR *)(GLenum, GLuint))chimera_gl_lookup("glBindTexture");
	stale_use(kTexture, name, "glBindTexture");
	real(target, name);
}
static void GLAD_API_PTR trace_glBindBuffer(GLenum target, GLuint name)
{
	static auto real = (void (GLAD_API_PTR *)(GLenum, GLuint))chimera_gl_lookup("glBindBuffer");
	stale_use(kBuffer, name, "glBindBuffer");
	real(target, name);
}
static void GLAD_API_PTR trace_glBindBufferBase(GLenum target, GLuint index, GLuint name)
{
	static auto real = (void (GLAD_API_PTR *)(GLenum, GLuint, GLuint))chimera_gl_lookup("glBindBufferBase");
	stale_use(kBuffer, name, "glBindBufferBase");
	real(target, index, name);
}
static void GLAD_API_PTR trace_glBindBufferRange(GLenum target, GLuint index, GLuint name, GLintptr off, GLsizeiptr size)
{
	static auto real = (void (GLAD_API_PTR *)(GLenum, GLuint, GLuint, GLintptr, GLsizeiptr))chimera_gl_lookup("glBindBufferRange");
	stale_use(kBuffer, name, "glBindBufferRange");
	real(target, index, name, off, size);
}
static void GLAD_API_PTR trace_glBindSampler(GLuint unit, GLuint name)
{
	static auto real = (void (GLAD_API_PTR *)(GLuint, GLuint))chimera_gl_lookup("glBindSampler");
	stale_use(kSampler, name, "glBindSampler");
	real(unit, name);
}
static void GLAD_API_PTR trace_glUseProgram(GLuint name)
{
	static auto real = (void (GLAD_API_PTR *)(GLuint))chimera_gl_lookup("glUseProgram");
	stale_use(kProgramOrShader, name, "glUseProgram");
	real(name);
}
static void GLAD_API_PTR trace_glBindVertexArray(GLuint name)
{
	static auto real = (void (GLAD_API_PTR *)(GLuint))chimera_gl_lookup("glBindVertexArray");
	stale_use(kVertexArray, name, "glBindVertexArray");
	real(name);
}
static void GLAD_API_PTR trace_glBindFramebuffer(GLenum target, GLuint name)
{
	static auto real = (void (GLAD_API_PTR *)(GLenum, GLuint))chimera_gl_lookup("glBindFramebuffer");
	stale_use(kFramebuffer, name, "glBindFramebuffer");
	real(target, name);
}
static void GLAD_API_PTR trace_glBindRenderbuffer(GLenum target, GLuint name)
{
	static auto real = (void (GLAD_API_PTR *)(GLenum, GLuint))chimera_gl_lookup("glBindRenderbuffer");
	stale_use(kRenderbuffer, name, "glBindRenderbuffer");
	real(target, name);
}
static void GLAD_API_PTR trace_glFramebufferTexture2D(GLenum target, GLenum att, GLenum textarget, GLuint name, GLint level)
{
	static auto real = (void (GLAD_API_PTR *)(GLenum, GLenum, GLenum, GLuint, GLint))chimera_gl_lookup("glFramebufferTexture2D");
	stale_use(kTexture, name, "glFramebufferTexture2D");
	real(target, att, textarget, name, level);
}
static void GLAD_API_PTR trace_glFramebufferTextureLayer(GLenum target, GLenum att, GLuint name, GLint level, GLint layer)
{
	static auto real = (void (GLAD_API_PTR *)(GLenum, GLenum, GLuint, GLint, GLint))chimera_gl_lookup("glFramebufferTextureLayer");
	stale_use(kTexture, name, "glFramebufferTextureLayer");
	real(target, att, name, level, layer);
}
static void GLAD_API_PTR trace_glFramebufferRenderbuffer(GLenum target, GLenum att, GLenum rbtarget, GLuint name)
{
	static auto real = (void (GLAD_API_PTR *)(GLenum, GLenum, GLenum, GLuint))chimera_gl_lookup("glFramebufferRenderbuffer");
	stale_use(kRenderbuffer, name, "glFramebufferRenderbuffer");
	real(target, att, rbtarget, name);
}

static void *stale_trace_override(const char *name)
{
	static const bool on = getenv("CHIMERA_GL_STALETRACE") != nullptr;
	if (!on) return nullptr;
	static const struct { const char *name; void *fn; } table[] = {
		{ "glBindTexture", (void *)trace_glBindTexture },
		{ "glBindBuffer", (void *)trace_glBindBuffer },
		{ "glBindBufferBase", (void *)trace_glBindBufferBase },
		{ "glBindBufferRange", (void *)trace_glBindBufferRange },
		{ "glBindSampler", (void *)trace_glBindSampler },
		{ "glUseProgram", (void *)trace_glUseProgram },
		{ "glBindVertexArray", (void *)trace_glBindVertexArray },
		{ "glBindFramebuffer", (void *)trace_glBindFramebuffer },
		{ "glBindRenderbuffer", (void *)trace_glBindRenderbuffer },
		{ "glFramebufferTexture2D", (void *)trace_glFramebufferTexture2D },
		{ "glFramebufferTextureLayer", (void *)trace_glFramebufferTextureLayer },
		{ "glFramebufferRenderbuffer", (void *)trace_glFramebufferRenderbuffer },
	};
	for (const auto &e : table)
		if (std::strcmp(name, e.name) == 0)
			return e.fn;
	return nullptr;
}

/* The loader the renderer is actually handed.
 *
 * Buffer mapping is answered here rather than across the bridge, so it has to
 * displace the generated wrappers for those five names. Everything else falls
 * through to the shared table unchanged. */
extern "C" void *chimera_gl_lookup_guest(const char *name)
{
	if (!name)
		return nullptr;
	if (std::strcmp(name, "glMapBufferRange") == 0)         return (void *)chimera_gl_map_buffer_range;
	if (std::strcmp(name, "glMapBuffer") == 0)              return (void *)chimera_gl_map_buffer;
	if (std::strcmp(name, "glUnmapBuffer") == 0)            return (void *)chimera_gl_unmap_buffer;
	if (std::strcmp(name, "glFlushMappedBufferRange") == 0) return (void *)chimera_gl_flush_mapped_buffer_range;
	if (std::strcmp(name, "glGetBufferPointerv") == 0)      return (void *)chimera_gl_get_buffer_pointerv;
	if (void *shared = chimera_gl_shared_override(name))    return shared;
	if (void *gen = generation_override(name))              return gen;
	if (void *trace = stale_trace_override(name))           return trace;
	return chimera_gl_lookup(name);
}
