/* Frontend-free runner for the Ruffle waterbox core.
 *
 * Mounts a .swf into the guest VFS, boots it, advances N frames, and prints the
 * machine's TTY - which for this core is the SWF's own trace() output - plus a
 * digest. The native reference (waterbox/run-native) prints the same trace, and
 * ruffle's corpus ships the expected output.txt next to each test.swf, so all
 * three can be compared with cmp.
 *
 * Usage: run-wbx <core.wbx> <file.swf> --frames <n> [--quiet] [--input <moves>]
 *               [--audio-out <raw i16 stereo>] [--audio-peaks <one per frame>]
 *               [--file <vfsname>=<hostpath>]... [--spoof-url <url>]
 *   --input replays a moves file (tests/input2moves.py): one line per frame of
 *   "A<axis>=<v>" / "B<button>=<0|1>" tokens, applied before that frame.
 */
#include "minibox.h"
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <stdio.h>
#include <time.h>

/* --rerecord saves and reloads the machine before every single frame. If any
 * of the machine lives outside the sandbox's memory - or the core keeps a
 * pointer across a load - the run diverges from an ordinary one. */
struct statebuf { uint8_t *p; size_t len, cap, pos; };
static struct statebuf g_state;
static int32_t state_write(uintptr_t ud, const uint8_t *d, uintptr_t n)
{
	struct statebuf *b = (struct statebuf *)ud;
	if (b->len + n > b->cap) {
		size_t want = (b->len + n) * 2;
		uint8_t *q = (uint8_t *)realloc(b->p, want);
		if (!q) return -1;
		b->p = q; b->cap = want;
	}
	memcpy(b->p + b->len, d, n);
	b->len += n;
	return 0;
}
static intptr_t state_read(uintptr_t ud, uint8_t *d, uintptr_t n)
{
	struct statebuf *b = (struct statebuf *)ud;
	size_t left = b->len - b->pos;
	if (n > left) n = left;
	memcpy(d, b->p + b->pos, n);
	b->pos += n;
	return (intptr_t)n;
}

/* the host's end of the GPU bridge (waterbox/gl-host.c) */
int chimera_gl_host_init(char *err, int errlen);
const char *chimera_gl_host_description(void);
uintptr_t chimera_gl_host_dispatch(uintptr_t op, uintptr_t a, uintptr_t b,
                                   uintptr_t c, uintptr_t d, uintptr_t e);
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <string.h>

typedef struct { FILE *f; } freader;
static intptr_t file_read(uintptr_t ud, uint8_t *d, uintptr_t n) {
	return (intptr_t)fread(d, 1, n, ((freader *)ud)->f);
}

static uintptr_t proc(mb_host *h, const char *n) {
	mb_return r;
	wbx_get_proc_addr(h, n, &r);
	if (r.error_message[0]) { fprintf(stderr, "proc %s: %s\n", n, r.error_message); exit(2); }
	return r.data;
}

int main(int argc, char **argv) {
	if (argc < 3) { fprintf(stderr, "usage: run-wbx <core.wbx> <file.swf> --frames <n> [--quiet] [--no-gpu]\n"); return 2; }
	const char *corepath = argv[1], *swfpath = argv[2];
	long frames = 1;
	int quiet = 0;
	const char *movespath = NULL, *audiopath = NULL, *peakspath = NULL, *spoofurl = NULL;
	const char *videopath = NULL;
	int rerecord = 0;
	long benchCrossings = 0;
	int noRender = 0;
	int from_vfs = 0;
	int noGpu = 0;
	const char *files[64][2]; int nfiles = 0;
	for (int i = 3; i < argc; i++) {
		if (!strcmp(argv[i], "--frames") && i + 1 < argc) frames = atol(argv[++i]);
		else if (!strcmp(argv[i], "--quiet")) quiet = 1;
		else if (!strcmp(argv[i], "--input") && i + 1 < argc) movespath = argv[++i];
		else if (!strcmp(argv[i], "--audio-out") && i + 1 < argc) audiopath = argv[++i];
		else if (!strcmp(argv[i], "--audio-peaks") && i + 1 < argc) peakspath = argv[++i];
		else if (!strcmp(argv[i], "--spoof-url") && i + 1 < argc) spoofurl = argv[++i];
		else if (!strcmp(argv[i], "--video-out") && i + 1 < argc) videopath = argv[++i];
		else if (!strcmp(argv[i], "--rerecord")) rerecord = 1;
		else if (!strcmp(argv[i], "--bench-crossings") && i + 1 < argc) benchCrossings = atol(argv[++i]);
		/* what a seek or a turbo run looks like: the frames still happen, nobody
		 * looks at the picture */
		else if (!strcmp(argv[i], "--no-render")) noRender = 1;
		/* skip AllocSwf and let Init find the movie in the guest's filesystem,
		 * which is how the engine loads a game (waterbox.config: romFile) */
		else if (!strcmp(argv[i], "--from-vfs")) from_vfs = 1;
		/* offer no bridge at all, which is what a Chimera without one looks
		 * like. The software renderer has to draw anyway; the hardware one has
		 * to refuse. Both are things the gate should be able to ask for. */
		else if (!strcmp(argv[i], "--no-gpu")) noGpu = 1;
		else if (!strcmp(argv[i], "--file") && i + 1 < argc && nfiles < 64) {
			char *eq = strchr(argv[++i], '=');
			if (eq) { *eq = 0; files[nfiles][0] = argv[i]; files[nfiles][1] = eq + 1; nfiles++; }
		}
	}
	FILE *moves = movespath ? fopen(movespath, "r") : NULL;
	if (movespath && !moves) { perror(movespath); return 1; }

	FILE *cf = fopen(corepath, "rb");
	if (!cf) { perror(corepath); return 1; }
	/* Ruffle is a Rust guest: std's allocator wants a real heap, and a movie's
	 * display list plus its decoded assets live on it. */
	mb_memory_layout_template layout = { 256u << 20, 16u << 20, 16u << 20, 64u << 20, 512u << 20 };
	freader fr = { cf };
	mb_return r;
	wbx_create_host(&layout, "core.wbx", file_read, (uintptr_t)&fr, &r);
	fclose(cf);
	if (r.error_message[0]) { fprintf(stderr, "create: %s\n", r.error_message); return 1; }
	mb_host *h = (mb_host *)r.data;

	wbx_activate_host(h, &r);

	/* Hand the movie over directly: ask the guest for a buffer and fill it. The
	 * host can write guest memory while the block is active, and it avoids the
	 * guest-side file IO bug (see docs/PLAN.md). */
	if (!from_vfs) {
		uint8_t *(*AllocSwf)(uint64_t) = (uint8_t *(*)(uint64_t))proc(h, "AllocSwf");
		FILE *sf = fopen(swfpath, "rb");
		if (!sf) { perror(swfpath); return 1; }
		fseek(sf, 0, SEEK_END); long swflen = ftell(sf); fseek(sf, 0, SEEK_SET);
		uint8_t *dst = AllocSwf((uint64_t)swflen);
		if (!dst) { fprintf(stderr, "guest refused a %ld byte buffer\n", swflen); return 1; }
		if (fread(dst, 1, (size_t)swflen, sf) != (size_t)swflen) { fprintf(stderr, "short read\n"); return 1; }
		fclose(sf);
	} else {
		/* the engine's route: the movie is a mounted file called romFile */
		mb_return mr;
		wbx_mount_file_path(h, "game", swfpath, &mr);
		if (mr.error_message[0]) { fprintf(stderr, "mount game: %s\n", mr.error_message); return 1; }
	}

	/* associated files a movie may load (loadMovie/loadSound): mount each into
	 * the guest VFS under the name the movie asks for. The navigator reads them
	 * with std::fs. */
	for (int k = 0; k < nfiles; k++) {
		wbx_mount_file_path(h, files[k][0], files[k][1], &r);
		if (r.error_message[0]) { fprintf(stderr, "mount %s: %s\n", files[k][0], r.error_message); return 1; }
	}
	if (spoofurl) {
		void (*SetSpoofUrl)(const char *, int32_t) = (void (*)(const char *, int32_t))proc(h, "SetSpoofUrl");
		SetSpoofUrl(spoofurl, (int32_t)strlen(spoofurl));
	}

	/* The GPU bridge: bring up a real GL context on this side and hand the
	 * guest the one callback it is allowed to call. This must happen before
	 * Init, which builds the renderer through it. */
	if (!noGpu) {
		char glerr[256] = {0};
		if (chimera_gl_host_init(glerr, sizeof glerr) != 0) {
			/* Not fatal any more. The core falls back to the Mesa
			 * inside the sandbox, which needs nothing from this side, and a
			 * machine with no GL - or one whose EGL has run out of something -
			 * should still be able to run the picture legs. A core that was
			 * told to draw on the GPU fails its own Init instead, with a
			 * message that names the setting. */
			fprintf(stderr, "gpu bridge: no GL context (%s); the core must draw in software\n", glerr);
			goto no_bridge;
		}
		void (*SetGpuBridge)(uint64_t) = (void (*)(uint64_t))proc(h, "SetGpuBridge");
		mb_return cb;
		wbx_get_callback_addr(h, (mb_external_callback)chimera_gl_host_dispatch, 0, &cb);
		if (cb.error_message[0]) {
			fprintf(stderr, "gpu bridge: %s\n", cb.error_message);
			return 1;
		}
		SetGpuBridge(cb.data);
		if (!quiet)
			fprintf(stderr, "gpu bridge: %s\n", chimera_gl_host_description());
	}
no_bridge:
	;

	int (*Init)(void) = (int (*)(void))proc(h, "Init");
	void (*FrameAdvance)(uint64_t) = (void (*)(uint64_t))proc(h, "FrameAdvance");
	const uint8_t *(*GetTty)(void) = (const uint8_t *(*)(void))proc(h, "GetTty");
	int64_t (*GetTtySize)(void) = (int64_t (*)(void))proc(h, "GetTtySize");
	uint64_t (*GetTraceDigest)(void) = (uint64_t (*)(void))proc(h, "GetTraceDigest");
	void (*SetButton)(int32_t, int32_t) = (void (*)(int32_t, int32_t))proc(h, "SetButton");
	void (*SetAxis)(int32_t, int32_t) = (void (*)(int32_t, int32_t))proc(h, "SetAxis");
	/* the gate replays ruffle's recorded stage coordinates exactly, so it uses
	 * the pixel entry rather than the frontend's normalised axis */
	void (*SetMousePixels)(int32_t, int32_t) = (void (*)(int32_t, int32_t))proc(h, "SetMousePixels");
	int32_t mx = 0, my = 0;
	void (*SetTextInput)(int32_t) = (void (*)(int32_t))proc(h, "SetTextInput");
	const int16_t *(*GetAudio)(void) = (const int16_t *(*)(void))proc(h, "GetAudio");
	int32_t (*GetAudioSampleCount)(void) = (int32_t (*)(void))proc(h, "GetAudioSampleCount");
	const uint8_t *(*GetVideoBgra)(void) = (const uint8_t *(*)(void))proc(h, "GetVideoBgra");
	int32_t (*GetVideoWidth)(void) = (int32_t (*)(void))proc(h, "GetVideoWidth");
	int32_t (*GetVideoHeight)(void) = (int32_t (*)(void))proc(h, "GetVideoHeight");
	int32_t (*GetVsyncNumerator)(void) = (int32_t (*)(void))proc(h, "GetVsyncNumerator");
	int32_t (*GetVsyncDenominator)(void) = (int32_t (*)(void))proc(h, "GetVsyncDenominator");
	FILE *audiof = audiopath ? fopen(audiopath, "wb") : NULL;
	FILE *peaksf = peakspath ? fopen(peakspath, "w") : NULL;
	uint64_t ah = 1469598103934665603ull; uint64_t audio_bytes = 0;
	uint64_t vh_hash = 1469598103934665603ull, lit = 0;
	int32_t vw = 0, vh = 0;
	const uint8_t *last_px = NULL;

	/* --bench-crossings N: how long one GL crossing costs. Nothing else in
	 * this runner needs it; it is here because the answer decides whether
	 * batching the bridge is worth building. Run before Init so no renderer
	 * exists and the number is the boundary and nothing else. */
	if (benchCrossings > 0) {
		uint64_t (*Bench)(uint64_t) = (uint64_t (*)(uint64_t))proc(h, "BenchGlCrossings");
		/* warm the path once, then time it */
		Bench(1000);
		struct timespec t0, t1;
		clock_gettime(CLOCK_MONOTONIC, &t0);
		uint64_t sum = Bench((uint64_t)benchCrossings);
		clock_gettime(CLOCK_MONOTONIC, &t1);
		double secs = (double)(t1.tv_sec - t0.tv_sec) + (double)(t1.tv_nsec - t0.tv_nsec) / 1e9;
		printf("crossings %ld in %.6f s = %.1f ns each (sum %llu)\n",
			benchCrossings, secs, secs * 1e9 / (double)benchCrossings,
			(unsigned long long)sum);
		return 0;
	}

	if (!Init()) {
		const char *(*GetLoadError)(void) = (const char *(*)(void))proc(h, "GetLoadError");
		fprintf(stderr, "init failed: %s\n", GetLoadError());
		return 1;
	}

	/* Sealed once the movie is loaded: the savestate is a diff from here, and
	 * everything the SWF brought with it is baseline rather than state. */
	{
		mb_return sr;
		fprintf(stderr, "[seal] starting\n"); fflush(stderr);
		wbx_seal(h, &sr);
		fprintf(stderr, "[seal] done\n"); fflush(stderr);
		if (sr.error_message[0]) { fprintf(stderr, "seal: %s\n", sr.error_message); return 1; }
	}
	if (noRender) {
		void (*SetRenderingEnabled)(int32_t) =
			(void (*)(int32_t))proc(h, "SetRenderingEnabled");
		SetRenderingEnabled(0);
	}
	/* the corpus protocol scripts TextInput as its own event, so a replayed
	 * moves file must not have key presses type characters on top */
	if (moves) SetTextInput(0);
	char line[4096];
	for (long i = 0; i < frames; i++) {
		if (moves && fgets(line, sizeof line, moves)) {
			for (char *tok = strtok(line, " \n"); tok; tok = strtok(NULL, " \n")) {
				long idx, val;
				if (sscanf(tok, "A%ld=%ld", &idx, &val) == 2) {
					if (idx == 0) mx = (int32_t)val; else if (idx == 1) my = (int32_t)val;
					SetMousePixels(mx, my);
					(void)SetAxis;
				}
				else if (sscanf(tok, "B%ld=%ld", &idx, &val) == 2) SetButton((int32_t)idx, (int32_t)val);
			}
		}
		if (rerecord) {
			mb_return sr;
			g_state.len = 0;
			wbx_save_state(h, state_write, (uintptr_t)&g_state, &sr);
			if (sr.error_message[0]) { fprintf(stderr, "save_state: %s\n", sr.error_message); return 1; }
			g_state.pos = 0;
			wbx_load_state(h, state_read, (uintptr_t)&g_state, &sr);
			if (sr.error_message[0]) { fprintf(stderr, "load_state: %s\n", sr.error_message); return 1; }
		}
		FrameAdvance(0);
		/* this frame's audio: digest it (and dump it) the way the native reference does */
		int32_t n = GetAudioSampleCount();
		const int16_t *a = GetAudio();
		int32_t peak = 0;
		for (int32_t k = 0; k < n * 2; k++) {
			const uint8_t *b = (const uint8_t *)&a[k];
			ah ^= b[0]; ah *= 1099511628211ull; ah ^= b[1]; ah *= 1099511628211ull;
			int32_t v = a[k] < 0 ? -(int32_t)a[k] : a[k]; if (v > peak) peak = v;
		}
		audio_bytes += (uint64_t)n * 4;
		if (audiof && n > 0) fwrite(a, 4, (size_t)n, audiof);
		if (peaksf) fprintf(peaksf, "%.6f\n", peak / 32767.0);

		/* this frame's picture: digest every pixel, and count the ones that are
		 * neither black nor transparent. A digest alone cannot tell a real
		 * frame from a uniformly blank one, and a blank frame is exactly what a
		 * broken renderer produces. */
		/* --no-render is a frontend that is not going to LOOK at the frame, and
		 * that includes this runner: digesting the picture here is two passes
		 * over a megabyte, which is more than the frame costs and would drown
		 * out what the flag is being measured for. */
		vw = GetVideoWidth(); vh = GetVideoHeight();
		const uint8_t *px = noRender ? NULL : GetVideoBgra();
		if (px && vw > 0 && vh > 0) {
			size_t bytes = (size_t)vw * (size_t)vh * 4;
			lit = 0;
			for (size_t k = 0; k < bytes; k++) { vh_hash ^= px[k]; vh_hash *= 1099511628211ull; }
			for (size_t k = 0; k + 3 < bytes; k += 4)
				if (px[k] || px[k+1] || px[k+2]) lit++;
			last_px = px;
		}
	}
	if (videopath && last_px && vw > 0 && vh > 0) {
		/* PPM: no encoder needed, and any tool can read it */
		FILE *vf = fopen(videopath, "wb");
		if (vf) {
			fprintf(vf, "P6\n%d %d\n255\n", vw, vh);
			for (size_t k = 0; k < (size_t)vw * (size_t)vh; k++) {
				unsigned char rgb[3] = { last_px[k*4+2], last_px[k*4+1], last_px[k*4+0] };
				fwrite(rgb, 1, 3, vf);
			}
			fclose(vf);
		}
	}
	if (audiof) fclose(audiof);
	if (peaksf) fclose(peaksf);

	int64_t n = GetTtySize();
	const uint8_t *tty = GetTty();
	if (!quiet && n > 0) fwrite(tty, 1, (size_t)n, stdout);
	fprintf(stderr, "ruffle: frames=%ld traceBytes=%lld traceDigest=%016llx audioBytes=%llu audioDigest=%016llx\n",
	        frames, (long long)n, (unsigned long long)GetTraceDigest(),
	        (unsigned long long)audio_bytes, (unsigned long long)ah);
	fprintf(stderr, "ruffle: video=%dx%d videoDigest=%016llx litPixels=%llu\n",
	        vw, vh, (unsigned long long)vh_hash, (unsigned long long)lit);
	/* the rate the machine runs at: the movie's own, or whatever the fps
	 * setting raised it to - the gate reads this back */
	fprintf(stderr, "ruffle: vsync=%d/%d\n", GetVsyncNumerator(), GetVsyncDenominator());
	if (last_px && vw > 0 && vh > 0) {
		unsigned long long sb=0,sg=0,sr=0,sa=0; unsigned char mb=0,mg=0,mr=0,ma=0;
		size_t n2 = (size_t)vw*(size_t)vh;
		for (size_t k=0;k<n2;k++) {
			unsigned char b=last_px[k*4],g=last_px[k*4+1],r=last_px[k*4+2],a=last_px[k*4+3];
			sb+=b; sg+=g; sr+=r; sa+=a;
			if(b>mb)mb=b; if(g>mg)mg=g; if(r>mr)mr=r; if(a>ma)ma=a;
		}
		fprintf(stderr, "ruffle: channel means B=%.1f G=%.1f R=%.1f A=%.1f  max B=%u G=%u R=%u A=%u\n",
		        (double)sb/n2,(double)sg/n2,(double)sr/n2,(double)sa/n2,mb,mg,mr,ma);
	}
	return 0;
}
