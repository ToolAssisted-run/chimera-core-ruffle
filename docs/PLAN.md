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
- **M5** URL spoofing + associated-file navigator backed by the sandbox FS.
- **M6** savestates. **M7** frontend leg + package + keybinds.

## Rules

Never push without explicit say-so. No copyrighted SWFs in the repo (ruffle's
own MIT test corpus only). Witness gate before chimera commits. Gates run one
at a time, never concurrent.
