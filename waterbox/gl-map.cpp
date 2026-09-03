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

extern "C" void *chimera_gl_lookup(const char *name);

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
