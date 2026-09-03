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
- **M1** Rust guest toolchain: `core` as one static musl blob linked to the
  miniBox guest ABI; boot + frame advance + main-memory domain; native ==
  sandbox digest.
- **M2** input (mouse 2 axes + buttons + keyboard). **M3** audio.
- **M4** rendering (wgpu/Mesa-softpipe over the GPU bridge, or software).
- **M5** URL spoofing + associated-file navigator backed by the sandbox FS.
- **M6** savestates. **M7** frontend leg + package + keybinds.

## Rules

Never push without explicit say-so. No copyrighted SWFs in the repo (ruffle's
own MIT test corpus only). Witness gate before chimera commits. Gates run one
at a time, never concurrent.
