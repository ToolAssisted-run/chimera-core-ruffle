/* Frontend-free runner for the Ruffle waterbox core.
 *
 * Mounts a .swf into the guest VFS, boots it, advances N frames, and prints the
 * machine's TTY - which for this core is the SWF's own trace() output - plus a
 * digest. The native reference (waterbox/run-native) prints the same trace, and
 * ruffle's corpus ships the expected output.txt next to each test.swf, so all
 * three can be compared with cmp.
 *
 * Usage: run-wbx <core.wbx> <file.swf> --frames <n> [--quiet]
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
	for (int i = 3; i < argc; i++) {
		if (!strcmp(argv[i], "--frames") && i + 1 < argc) frames = atol(argv[++i]);
		else if (!strcmp(argv[i], "--quiet")) quiet = 1;
	}

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

	int (*Init)(void) = (int (*)(void))proc(h, "Init");
	void (*FrameAdvance)(uint64_t) = (void (*)(uint64_t))proc(h, "FrameAdvance");
	const uint8_t *(*GetTty)(void) = (const uint8_t *(*)(void))proc(h, "GetTty");
	int64_t (*GetTtySize)(void) = (int64_t (*)(void))proc(h, "GetTtySize");
	uint64_t (*GetTraceDigest)(void) = (uint64_t (*)(void))proc(h, "GetTraceDigest");

	if (!Init()) {
		const char *(*GetLoadError)(void) = (const char *(*)(void))proc(h, "GetLoadError");
		fprintf(stderr, "init failed: %s\n", GetLoadError());
		return 1;
	}
	for (long i = 0; i < frames; i++) FrameAdvance(0);

	int64_t n = GetTtySize();
	const uint8_t *tty = GetTty();
	if (!quiet && n > 0) fwrite(tty, 1, (size_t)n, stdout);
	fprintf(stderr, "ruffle: frames=%ld traceBytes=%lld traceDigest=%016llx\n",
	        frames, (long long)n, (unsigned long long)GetTraceDigest());
	return 0;
}
