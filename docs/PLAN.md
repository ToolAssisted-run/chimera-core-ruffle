# Chimera core: Ruffle (Flash / .swf)

Chosen by the user 2026-09-03, after RPCS3. Ruffle (github.com/ruffle-rs/ruffle,
pinned dc8c838a3998) is the Rust Flash Player emulator. ROMs are `.swf` files
plus any associated files; input is keyboard + mouse; no firmware.

## Why this fits chimera

Ruffle is **not deterministic by design**. TASers today
(tasvideos.org/EmulatorResources/Ruffle) force determinism onto the stock
desktop binary with **libTAS** (Linux syscall hooking), and it is fragile:
desyncs from lag frames, leftover `.sol` save data, a runtime libopenh264
download, and needing `clock_gettime` monotonic hooked. The waterbox sandbox
provides all of that by construction (every syscall trapped; one guest thread
at a time under vsched), so chimera gives Ruffle the determinism libTAS only
approximates. That is the pitch.

## The two hard parts

1. **Rust guest toolchain.** Every core so far is C/C++ on the musl-GCC guest
   toolchain. Ruffle is Rust/Cargo. We need `ruffle_core` (+ deps) built for
   `x86_64-unknown-linux-musl` (installed; rustc 1.97), static, linked against
   the miniBox guest ABI / syscall shim - the Rust analogue of the C++ guest
   toolchain. Main unknown.
2. **Rendering.** Ruffle renders via `render/wgpu` (also `render/canvas`,
   `render/webgl`); no obvious CPU rasteriser. For a deterministic TAS the
   picture must land in guest memory: wgpu on the GL backend over the guest
   Mesa softpipe + GPU bridge, or a software path. **M0 needs no renderer.**

## Key in-tree facts (from ~/ruffle-src)

- `core` is backend-agnostic: Renderer/Audio/Navigator/Ui/Log are traits the
  frontend supplies. `render/src/backend/null.rs` is a NullRenderer; core ships
  null audio/navigator/ui/log too. All-null = a headless deterministic core.
- Driving a movie (tests/framework/src/runner.rs):
  `PlayerBuilder::new().with_movie(m).with_autoplay(true)` -> build ->
  `player.preload(&mut ExecutionLimit::exhausted())` -> per frame
  `player.tick(frame_time); player.run_frame(); player.audio_mut().tick();`
  -> `player.render()`.
- **Oracle for free:** each of the 4869 tests in `tests/tests/swfs/` ships
  `test.swf` + `output.txt` (the SWF's `trace()` output captured by a log
  backend) + `test.toml` (`num_frames`/`num_ticks`). Reproducing `output.txt`
  proves determinism AND correctness, with no copyrighted content.
- Input model (`tests/input-format`): `AutomatedEvent::{Wait (=frame boundary),
  MouseMove{pos}, MouseDown/Up{pos,btn}, key events}` - maps onto chimera's
  frame-advance + mouse-as-2-axes + buttons + keyboard.

## Milestones (native reference first, like every core)

- **M0 DONE (2026-09-03).** `waterbox/run-native` drives ruffle_core with
  null backends + a trace-capturing log, loads a SWF, runs `num_frames` at the
  movie's own rate, prints the trace + a summary (`traceSha1`). The native gate
  (`waterbox/run-gate.sh`) is **120/120**: every test in
  `tests/oracle-list.txt` reproduces ruffle's committed `output.txt` byte for
  byte AND is identical across three runs. Determinism separately spot-checked
  93/93 SWFs. The ~26% of simple corpus tests that null backends cannot serve
  (navigator/network, input injection, fonts) are deferred to M2/M3/M5, not on
  the list. Java: a portable JDK lives at ~/.local/jdk (asc.jar needs it);
  `build-native.sh` puts it on PATH.
- **M1 DONE (2026-09-03).** The Rust-in-waterbox toolchain is PROVEN (2026-09-03):
  a trivial Rust guest (std Vec/sort/hash) runs inside miniBox byte-identical
  to native (digest 885242c4a6382a93) and savestate round-trips. Recipe
  (waterbox/build-guest.sh, waterbox/guest/): nightly `-Z build-std`
  recompiles std for x86_64-unknown-linux-musl with LARGE code model + static
  reloc (the waterbox base) against our musl; `panic = "immediate-abort"`
  (cargo-features opt-in) strips the unwinder; inert abort() stubs
  (unwind-stubs.c) satisfy std's leftover libgcc `_Unwind_*` refs instead of
  the host's small-model glibc libgcc_eh; `musl-gcc` links with emulibc +
  cxxglue + linkscript.T, exports forced with `-u`. FINDINGS (recon done): ruffle_core
  compiles AND links AND runs for the musl guest with ZERO undefined symbols -
  the big dep tree is not the problem. It runs past getrandom(318) (std's
  HashMap seed), now implemented deterministically in miniBox (cb7bed6).
  **Flash now runs inside the waterbox.** The gate is 120/120 on all three
  properties: the trace matches ruffle's own committed output.txt, three runs
  agree, and the SANDBOX trace is byte-identical to the native one. Needed
  (a) guest %fs TLS (miniBox 62ca831) and (b) fcntl's descriptor-flag commands
  (miniBox 7e3d82e). The export surface is waterbox/guest/src/lib.rs
  (AllocSwf/Init/FrameAdvance/GetTty/GetTtySize/GetTraceDigest/GetFrameCount/
  GetLoadError/IsRunning) driven by waterbox/run-wbx.c.

  **KNOWN BUG, deferred: Rust std file IO inside the sandbox.** musl's open
  returns a good fd (3) but Rust's File reports a corrupted one
  (as_raw_fd gave 16728), so std::fs::read fails; reproducible in a ten-line
  guest, and NOT caused by the %fs work (it happens with the swap forced off).
  M1 sidesteps it - the host hands the movie over through AllocSwf, writing
  guest memory directly - but M5's navigator (associated files, URL spoofing)
  will need it fixed. Was: guest %fs TLS - the wbx has .tdata/.tbss and 149 %fs:
  accesses, and Init faults at the first thread_local read (HashMap
  RandomState) because the guest thread pointer is not established (arch_prctl
  158 absent from miniBox's dispatch; host-context %fs switching unwired).
  NEXT: make guest TLS work, then wire the WaterboxCore exports
  (Init/FrameAdvance/memory domains/GetTty for trace) and reproduce M0's trace
  digests in the sandbox.
- **M2 DONE (2026-09-03): input.** Mouse as two axes (stage pixels) plus
  Left/Middle/Right, and a 100-key keyboard, as LEVELS in waterbox.config's
  wire order (generated by waterbox/input-table.py, the one source of truth
  for guest/src/input_table.rs and the config). The guest turns level changes
  into Ruffle's edge events at the END of each frame step, exactly where ruffle's runner injects: `run_frame;
  update_timers(frame_time); audio.tick; inject; render()`. Two hard-won
  facts: (1) ruffle's frame-counted runner never calls `tick()` - that is the
  wall-clock path with its own accumulator and AVM2 catch-up, and calling it
  too doubled frame work in exactly the timer-/input-sensitive movies (the
  trace-only corpus tolerated it, which hid the bug through M0/M1); (2) the
  viewport must be the movie's own stage size at scale 1, or pointer pixels
  are not stage pixels and per-object hit tests silently miss while root
  handlers still fire. Printable keys also type their character (TextInput)
  by default, the way an OS delivers typing; `SetTextInput(0)` reproduces
  ruffle's scripted protocol, which the oracle needs. Gate: the 17 corpus
  input.json streams the level model can express (tests/input2moves.py, block
  k -> frame k) replay 17/17 trace-identical; the full gate stays 120/120.
  Not mapped yet: MouseWheel, TextControl, IME, clipboard, focus; a press AND
  release inside one frame is inexpressible as levels (every core's limit).
  **M3 DONE (2026-09-03): audio.** Ruffle's own software mixer (AudioMixer,
  44.1 kHz stereo) driven a frame at a time - the same backend shape as
  ruffle's TestAudioBackend, via `impl_audio_mixer_backend!`, so the corpus's
  amplitude assertions are a valid oracle. The player sizes the buffer itself
  through set_frame_rate; tick() mixes one frame; the guest hands it to the
  host as i16 through GetAudio/GetAudioSampleCount (capacity 44100 frames, a
  1 fps movie). ruffle_core is built with the harness's feature set
  (audio, mp3, aac, default_font) in both the guest and run-native.
  ONE FACT THAT COST A ROUND: ruffle's harness rounds the INTERLEAVED sample
  count, which is odd at 24 fps (88200/24 = 3675, a lone left sample); native
  wrote it, the sandbox's len/2 dropped it, and every test's audio digest
  differed by two bytes. Both sides now round the FRAME count (whole stereo
  frames - the only thing a host can take, and the mixer never sees half a
  frame). Gate: every one of the 120 oracle tests now also requires native
  and sandbox audio digests to be identical; the audio leg (tests/audio-list.txt,
  tests/audio-assert.py mirroring the harness's test_audio) requires ruffle's
  amplitude assertions to hold, native == sandbox byte for byte, and reruns
  identical. Three assertion tests are excluded with reasons: their sound
  reaches the movie through the navigator (loadMovie/loadSound) and native
  is just as silent - M5, not audio.
- **M4** rendering (wgpu/Mesa-softpipe over the GPU bridge, or software).
- **M5 DONE (2026-09-03): navigator.** A movie's associated files
  (loadMovie/loadSound/loadVariables/URLLoader/getURL of a relative file) are
  served from the guest VFS the host mounts (run-wbx --file name=path); the
  guest navigator (waterbox/guest/src/navigator.rs) ports ruffle's
  TestNavigatorBackend - resolve relative to file:///, read the file - but reads
  through std::fs (now that guest file IO works) and runs the load futures on an
  IN-HOUSE executor. futures::executor::LocalPool parks a thread and never polls
  in the single-threaded sandbox, so the navigator queues futures and the
  machine polls them each frame with a no-op waker (deterministic: no wall
  clock, no external wakeups). URL spoofing: SetSpoofUrl before Init sets the
  movie's base URL (Flash domain checks; where relative loads resolve). Gate:
  94/106 navigator-candidate corpus tests (tests/navigator-list.txt) load their
  files with the trace ruffle expects, reruns identical. The 12 excluded (with
  reasons on file) either need ruffle's TEST navigator to LOG its fetches into
  the trace (a harness oracle detail; the file still loads), or fonts/image
  decode (M4), or are the audio-via-navigator tests (M3's amplitude leg).
  Also fixed here: the guest std file-IO bug was a STALE musl sysroot - the
  arg-clobber fix (91b3e30) never recompiled; meson now rebuilds musl on source
  change (miniBox af5d43a).
- **M6** savestates. **M7** frontend leg + package + keybinds.

## Rules

Never push without explicit say-so. No copyrighted SWFs in the repo (ruffle's
own MIT test corpus only). Witness gate before chimera commits. Gates run one
at a time, never concurrent.


## M4 - the picture (DONE)

ruffle's own wgpu renderer runs in the guest; the OpenGL it draws with belongs
to the host. Every GL call leaves through the single callback miniBox allows a
guest, and the driver executes it in the same address space, so vertex data and
textures are read where they already are. 709 entry points, both sides
generated from glad's declarations by `tools/gen-gl-bridge.py`.

**Gate:** `ruffle image gate: 197/197` - each movie's frame compared against
ruffle's own `output.expected.png` (8 per channel, 99% of pixels; ruffle's own
harness allows up to 128 for the same reason) and rendered twice to show the
frame does not wander. The other legs are unchanged and still green: 120/120
trace, 92/92 navigator, 17/17 input, 2/2 audio.

**What this costs, plainly:** the GPU is outside the sandbox, so it is outside
the savestate and different on every machine. The machine's own state stays
deterministic - the frame is computed from the display list, not read back from
it - with one exception worth naming: `BitmapData.draw()` rasterises display
objects into a bitmap ActionScript can then read, and there the picture does
feed the machine.

### What was tried first, and why it was abandoned

A software rasteriser INSIDE the sandbox would have made the picture
deterministic too. Mesa's **softpipe** was built for the guest and proven to
draw (the smoke test's `hash=a2962dc5`), but it cannot draw ruffle's frames: it
reads ruffle's second uniform block as zeros, every vertex collapses, and
nothing rasterises - with no GL error anywhere. This is not a sandbox problem;
it reproduces natively with the system Mesa under `GALLIUM_DRIVER=softpipe`,
while llvmpipe renders the same code correctly. Every individual primitive
(clears, uploads, shader draws, indexed draws, sampling, render-then-sample,
stencil attachments, uniform buffers with and without dynamic offsets) passes
on softpipe in isolation, so no minimal trigger was found.

llvmpipe does work, but it is llvmpipe because it JITs through LLVM, and that
means carrying LLVM in the guest.

### Three bugs this milestone found

- **miniBox** ran host callbacks on the guest's `%fs` (fixed there, 9b3fa9d).
- **wgpu's GL backend claims 2x/4x MSAA whatever `GL_MAX_SAMPLES` says**, so
  ruffle asked for a multisampled stencil buffer softpipe cannot allocate.
- **`panic = "immediate-abort"` while std still unwinds** is undefined, and it
  showed up only once the core did real work. The guest now uses `abort`.
