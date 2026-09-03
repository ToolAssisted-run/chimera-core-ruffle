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
#include <stdio.h>
#include <stdlib.h>
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
	if (argc < 3) { fprintf(stderr, "usage: run-wbx <core.wbx> <file.swf> --frames <n> [--quiet]\n"); return 2; }
	const char *corepath = argv[1], *swfpath = argv[2];
	long frames = 1;
	int quiet = 0;
	const char *movespath = NULL, *audiopath = NULL, *peakspath = NULL, *spoofurl = NULL;
	const char *files[64][2]; int nfiles = 0;
	for (int i = 3; i < argc; i++) {
		if (!strcmp(argv[i], "--frames") && i + 1 < argc) frames = atol(argv[++i]);
		else if (!strcmp(argv[i], "--quiet")) quiet = 1;
		else if (!strcmp(argv[i], "--input") && i + 1 < argc) movespath = argv[++i];
		else if (!strcmp(argv[i], "--audio-out") && i + 1 < argc) audiopath = argv[++i];
		else if (!strcmp(argv[i], "--audio-peaks") && i + 1 < argc) peakspath = argv[++i];
		else if (!strcmp(argv[i], "--spoof-url") && i + 1 < argc) spoofurl = argv[++i];
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
	uint8_t *(*AllocSwf)(uint64_t) = (uint8_t *(*)(uint64_t))proc(h, "AllocSwf");
	FILE *sf = fopen(swfpath, "rb");
	if (!sf) { perror(swfpath); return 1; }
	fseek(sf, 0, SEEK_END); long swflen = ftell(sf); fseek(sf, 0, SEEK_SET);
	uint8_t *dst = AllocSwf((uint64_t)swflen);
	if (!dst) { fprintf(stderr, "guest refused a %ld byte buffer\n", swflen); return 1; }
	if (fread(dst, 1, (size_t)swflen, sf) != (size_t)swflen) { fprintf(stderr, "short read\n"); return 1; }
	fclose(sf);

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

	int (*Init)(void) = (int (*)(void))proc(h, "Init");
	void (*FrameAdvance)(uint64_t) = (void (*)(uint64_t))proc(h, "FrameAdvance");
	const uint8_t *(*GetTty)(void) = (const uint8_t *(*)(void))proc(h, "GetTty");
	int64_t (*GetTtySize)(void) = (int64_t (*)(void))proc(h, "GetTtySize");
	uint64_t (*GetTraceDigest)(void) = (uint64_t (*)(void))proc(h, "GetTraceDigest");
	void (*SetButton)(int32_t, int32_t) = (void (*)(int32_t, int32_t))proc(h, "SetButton");
	void (*SetAxis)(int32_t, int32_t) = (void (*)(int32_t, int32_t))proc(h, "SetAxis");
	void (*SetTextInput)(int32_t) = (void (*)(int32_t))proc(h, "SetTextInput");
	const int16_t *(*GetAudio)(void) = (const int16_t *(*)(void))proc(h, "GetAudio");
	int32_t (*GetAudioSampleCount)(void) = (int32_t (*)(void))proc(h, "GetAudioSampleCount");
	FILE *audiof = audiopath ? fopen(audiopath, "wb") : NULL;
	FILE *peaksf = peakspath ? fopen(peakspath, "w") : NULL;
	uint64_t ah = 1469598103934665603ull; uint64_t audio_bytes = 0;

	if (!Init()) {
		const char *(*GetLoadError)(void) = (const char *(*)(void))proc(h, "GetLoadError");
		fprintf(stderr, "init failed: %s\n", GetLoadError());
		return 1;
	}
	/* the corpus protocol scripts TextInput as its own event, so a replayed
	 * moves file must not have key presses type characters on top */
	if (moves) SetTextInput(0);
	char line[4096];
	for (long i = 0; i < frames; i++) {
		if (moves && fgets(line, sizeof line, moves)) {
			for (char *tok = strtok(line, " \n"); tok; tok = strtok(NULL, " \n")) {
				long idx, val;
				if (sscanf(tok, "A%ld=%ld", &idx, &val) == 2) SetAxis((int32_t)idx, (int32_t)val);
				else if (sscanf(tok, "B%ld=%ld", &idx, &val) == 2) SetButton((int32_t)idx, (int32_t)val);
			}
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
	}
	if (audiof) fclose(audiof);
	if (peaksf) fclose(peaksf);

	int64_t n = GetTtySize();
	const uint8_t *tty = GetTty();
	if (!quiet && n > 0) fwrite(tty, 1, (size_t)n, stdout);
	fprintf(stderr, "ruffle: frames=%ld traceBytes=%lld traceDigest=%016llx audioBytes=%llu audioDigest=%016llx\n",
	        frames, (long long)n, (unsigned long long)GetTraceDigest(),
	        (unsigned long long)audio_bytes, (unsigned long long)ah);
	return 0;
}
