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
	return chimera_gl_lookup(name);
}
