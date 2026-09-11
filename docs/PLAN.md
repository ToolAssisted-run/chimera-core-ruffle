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

## Key in-tree facts (from extern/ruffle)

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

**Overturned 2026-09-11** - see "The software renderer" below. softpipe was
never unable to draw ruffle; wgpu was handing it shaders that could not carry
their own bindings, and any GL 3.3 driver draws the same black frame.

### Three bugs this milestone found

- **miniBox** ran host callbacks on the guest's `%fs` (fixed there, 9b3fa9d).
- **wgpu's GL backend claims 2x/4x MSAA whatever `GL_MAX_SAMPLES` says**, so
  ruffle asked for a multisampled stencil buffer softpipe cannot allocate.
- **`panic = "immediate-abort"` while std still unwinds** is undefined, and it
  showed up only once the core did real work. The guest now uses `abort`.


## M6 - savestates (DONE)

`ruffle state gate: 30/30`. Each movie runs twice: once normally, once with the
whole machine saved and reloaded before EVERY frame. Both runs agree on the
trace, the audio AND the picture, byte for byte - across trace movies, movies
that take input, and movies that draw.

The picture surviving is worth a note, because the GPU is outside the sandbox
and therefore outside the savestate: it survives because ruffle redraws each
frame from the display list rather than accumulating it, so a reloaded machine
draws the same frame again from the same state.

Getting here needed a fix in miniBox (96514d3): page snapshots were taken with
malloc inside the SIGSEGV handler, which is not async-signal-safe. It survived
while few pages were taken and stopped surviving the moment a core was sealed -
sealing marks every dirty page clean, so the next frame takes thousands of
snapshots at once while the graphics driver is itself allocating, and the
handler re-enters the allocator on a held lock. The failure was mute (a fault
inside the handler is delivered with SIGSEGV blocked, so the process is killed
with no diagnosis). Snapshots now come from pages miniBox maps itself.


## M7 - the core in Chimera (DONE)

`ruffle.chimeraCore` builds, loads in the frontend's engine, and draws.

`tests/run-frontend.sh`, 3/3:
  * **package** - core.wbx, waterbox.config, default_keybinds.json,
    file_slots.json, licences, provenance; packaged deterministically, so the
    SHA1 is the core's identity and a movie can cite it.
  * **keybinds** - all 103 buttons and both axes have a default binding.
    Flash is played with the keyboard and mouse a desktop already has, so the
    default is the identity: every key drives the key of the same name.
  * **engine:picture** - chimera-run (the same session the GUI drives) plays a
    movie and writes a frame that matches ruffle's own expected PNG to 0.356%
    of pixels - the same figure the core's own image gate reports.

What the engine needed that the core did not have:

  * **the movie from the VFS.** The engine mounts the game as a file named by
    `romFile`; Init now reads it, and AllocSwf remains for the gate's runner.
  * **the shared opcode table.** The core generated its own numbering at
    first, which meant nothing to the engine: opcodes live in miniBox
    (source/gl) precisely because a package is a guest binary while the host
    half lives in whatever runs it. 169 entry points a wgpu renderer names
    were appended there (append-only), and the core now generates against it.
  * **the frame rate per movie.** A SWF carries its own, and it is not always
    whole, so GetVsyncNumerator/Denominator report a ratio.
  * **the pointer, normalised.** The frontend sends a position across the
    range the config declares and the core scales it to the movie's stage,
    which is the same shape as a light gun's screen axes on other cores. The
    gate keeps exact stage pixels through SetMousePixels.
  * **memory domains, honestly empty.** A movie's state is a garbage collected
    object graph; there is no address that means the same thing twice, and
    publishing the guest heap as "RAM" would look searchable and quietly lie.

And one bug the engine found that the core's own gate could not: the GL
extension filter (buffer_storage) had been implemented in the gate's host, so
it did not apply under the engine's context and every upload failed there.
It now lives in the guest, where it belongs - the core knows it cannot use a
persistent mapping across the bridge, whoever is hosting it.

## What the crossings actually cost (2026-09-11)

The bridge was the suspect: ruffle's wgpu backend makes about six thousand GL
calls in an ordinary frame and fifty thousand in a heavy one, and every one of
them leaves the sandbox through a single callback. So batching them looked like
the next thing to build.

It is not, and the reason is a number. `run-wbx --bench-crossings N` makes N
crossings of GL_OP_CONTEXT_ID - the opcode the host answers from a variable,
touching no driver - and divides:

| | |
| --- | --- |
| one crossing | **4.0 ns** |
| six thousand of them | 24 us |
| fifty thousand of them | 200 us |

A fifth of a millisecond in the worst frame there is. The boundary is not what
those six thousand calls cost; the DRIVER is, and batching them would move the
same work to the same place. Written down rather than built.

The bench is kept because the next person will have the same suspicion, and
because a change to miniBox's callback path would show up here first.

Two real things came out of the same look:

* **the runner's own dispatcher asked the environment on every crossing.**
  `getenv("CHIMERA_GL_TRACE")`, per GL call - the same mistake the engine's
  bridge made and had fixed (chimera e00b19c); this copy was still there. Read
  once now. It never affected a shipped core, because Chimera's engine is the
  host in a real session and this file is only the gate's, but it made every
  measurement taken with the gate's runner a measurement of getenv.
* **GL_OP_CONTEXT_ID had no case here at all**, so it fell through to the
  default, printed a complaint, and told the guest "cannot tell" - which is the
  answer that disables the renderer-rebuild check. Answered now.

## SetRenderingEnabled (2026-09-11)

A seek replays hundreds of frames nobody sees and a turbo run replays them as
fast as the machine will go. Every core that can tell the difference is told,
through `SetRenderingEnabled`, and this one exported none - so it drew, read the
picture back off the GPU and converted it, for frames that were thrown away.

**What is skipped is the READBACK and nothing else.** `Player::render` still
runs, because it is not only drawing: it broadcasts `Event.RENDER` to the
display list, updates the bitmap caches and sweeps the font caches, and all of
that is machine state a movie depends on. Skipping it would be a desync wearing
an optimisation's clothes. The readback is pure output - a whole-frame copy off
the GPU, which blocks until the GPU has finished, and then a per-pixel RGBA to
BGRA pass over three hundred thousand pixels - so leaving it out changes nothing
the machine can observe. The buffer keeps whatever was last drawn into it, so a
host that asks for the picture anyway gets the last real one rather than
garbage.

Measured with `run-wbx --no-render`, 300 frames of Corporate Climber under
llvmpipe:

| | seconds |
| --- | --- |
| drawing and read back | 5.32 |
| read back, digest skipped | 4.82 |
| neither | 3.00 |

So the readback and its conversion are 1.82 s of 300 frames - about 6 ms a
frame - and the runner's own picture digest is the other half a second.

**That 6 ms is llvmpipe's, and it does not transfer.** Measured afterwards on a
GTX 1060 through `chimera-run --gpu` (frames differenced to remove startup):
18.59 ms a frame drawing, 17.05 ms a frame not - so **1.5 ms**, not 6. On a
software rasteriser the readback is a copy out of system memory that the CPU
just wrote; on a real GPU it is a transfer that the driver has already had to
wait for anyway. Worth having, and a quarter of what the first measurement
suggested. Never quote a GPU number taken on llvmpipe.

That middle row is the lesson, not a detail. The first attempt measured
`--no-render` as 25% SLOWER, repeatably. The cause was the harness: the runner
digests every pixel of every frame, and with the readback skipped it was
digesting a stale pointer into an empty vector. A flag that says "nobody is
going to look at this frame" has to mean it in the runner too.

### One cache the render epoch does not reach (found 2026-09-11, not fixed)

The renderer-rebuild path bumps `ruffle_render::render_epoch` and says, in a
comment, that this makes "ruffle_core's own GPU-handle caches re-register from
the display list and library rather than draw with the dangling handles they
still hold". That is true of every cache that stamps the epoch - `BitmapData`,
`Character`, `Drawing`, glyphs, morph shapes, the graphic shape cache - and it
is NOT true of `BitmapCache`, the one behind `cacheAsBitmap` and filters. Its
struct (core/src/display_object.rs) has no epoch field at all: it decides
staleness from the matrix and the source size, so a handle made by a previous
backend survives the bump and is drawn with.

What it would take: the same four lines `bitmap_data.rs` already has - a
`handle_epoch` set in `BitmapCache::update`, and `is_dirty` returning true when
it does not match `render_epoch()`. Not done here because nothing in the gate
can see it: the state leg saves and reloads in the SAME process, where the
context never changes, and the image leg never rebuilds a backend. A test has to
come first, and it has to be a second process.

Who it bites: a project reopened from disk, on a movie that uses cacheAsBitmap
or a filter. Not a desync - the machine is unaffected - but a wrong picture,
which the frontend will happily record.

## Where a New Star Soccer frame actually goes (GTX 1060, 2026-09-11)

The user's own report - Flash is slow in Chimera and not in vanilla Ruffle -
finally measured on the machine it was reported from, with `CHIMERA_GL_PROFILE`
(chimera's engine): every crossing timed and counted by opcode.

`chimera-run --gpu --render-every-frame`, frames differenced to remove startup
and the 7MB SWF parse:

| | ms a frame |
| --- | --- |
| the whole machine frame | 18.59 |
| ... of which inside the GL driver | 13.50 |
| the same frame, not drawing | 17.05 |

So **73% of the frame is inside the driver**, and this is what it is made of:

| call | a frame | ms | share of the driver |
| --- | --- | --- | --- |
| `glGetSynciv` | 56 | 7.22 | 53% |
| `glGenBuffers` | 140 | 1.65 | 12% |
| `glClientWaitSync` | 1 | 0.96 | 7% |
| `glGenTextures` | 101 | 0.93 | 7% |
| `glReadPixels` | 1 | 0.81 | 6% |
| `glGetBufferSubData` | 1 | 0.42 | 3% |
| **all 40,000 others** | 39,898 | **~1.5** | 11% |

Read that last row twice. **Forty thousand GL calls cost a millisecond and a
half between them.** `glDisable` alone is 10,610 calls for 0.30 ms. The call
COUNT was never the problem, which is the same conclusion the 4 ns crossing
measurement reached from the other end, now confirmed against the driver itself.

What IS the problem is two things that are the same thing:

- **8.2 ms a frame waiting on GPU fences.** 56 `glGetSynciv` calls averaging
  129 microseconds each is not a status poll, it is a block. The pipeline is
  being drained every frame rather than allowed to run ahead.
- **140 buffers and 101 textures CREATED every frame**, 2.6 ms. A renderer that
  allocates fresh resources each frame has to wait for the GPU to finish with
  the last ones, which is where the fences come from.

That is the signature of wgpu's OpenGL backend, which this core is on because
the bridge speaks GL and nothing else. Vanilla Ruffle on Windows runs the same
wgpu on DX12 or Vulkan, where the same frame recycles its buffers and does not
stall - which is the honest answer to "why is it slower here".

Two ways out, both real work and neither started: stop the per-frame resource
churn (upstream-shaped, in wgpu-hal's GL backend or in how ruffle drives it), or
teach the bridge a second backend so the guest is not forced onto GL. Written
down rather than guessed at.

**The tools are kept**, because the next person will ask the same question:
`CHIMERA_GL_TIME=1` adds the driver total to the per-frame line, and
`CHIMERA_GL_PROFILE=1` dumps calls and milliseconds per opcode every 300 frames
and again at exit. Opcodes are the master list's order (miniBox
`source/gl/gl-entry-points.txt`, first entry is 100). `chimera-run
--render-every-frame` is the other half of the A/B: without it the runner draws
only the frames a screenshot asks for, which makes it a measurement of a seek.

## The software renderer (2026-09-11)

The hardware path was too unstable for the user, so the core now draws in
software by default: ruffle's same wgpu renderer, on a Mesa softpipe compiled
into the guest behind OSMesa. Nothing crosses the sandbox boundary to draw. A
`renderer` setting chooses: `software` (default) or `opengl-hw` (the old path,
renamed from `wgpu-hw` so the wizard labels it "Hardware (OpenGL)" like every
other core).

### What renderers actually exist in the pinned ruffle

`render/` has three backends and one of them can run here. `render/canvas` and
`render/webgl` are browser backends (web-sys; no canvas or WebGL exists outside
a browser). `render/wgpu` is the only native one, and wgpu has no CPU backend of
its own - its software options are a software DRIVER underneath (llvmpipe or
lavapipe, both LLVM JITs) or Mesa's softpipe on the GL backend. There is no
CPU rasteriser crate in the tree; writing a `RenderBackend` on something like
tiny-skia would be a new renderer, not a setting. So: wgpu on softpipe, which
is what M4 tried and gave up on.

### Why M4's softpipe drew black

Reproduced first on the host, with the old core: `GALLIUM_DRIVER=softpipe` on
run-wbx's EGL context gives `litPixels=0` for avm1/color. Two separate bugs,
found in order:

1. **wgpu claims multisampling the driver does not have.** Its GL backend
   reports 2x and 4x as supported whenever `GL_MAX_SAMPLES` is below 8 (it reads
   a low answer as an iOS Safari quirk). softpipe's is 1. ruffle asks for 4 at
   the default quality, `glRenderbufferStorageMultisample(samples=4)` fails, the
   framebuffer is incomplete, and every clear and draw after it is refused.
   Seen with `MESA_DEBUG=1`; M4 had noted this one, then worked around it.
2. **The real black frame: bindings a GLSL 3.30 shader cannot write.** With the
   sample count forced to 1 there are no GL errors at all, and still nothing
   draws. A probe in a scratch copy of gl-host.c that reads the framebuffer back
   after every draw showed the clear lands and every draw - including the final
   full-screen copy - changes nothing. The decisive A/B: **llvmpipe told to
   report GLSL 3.30** (`MESA_GL_VERSION_OVERRIDE=3.3
   MESA_GLSL_VERSION_OVERRIDE=330`) draws the exact same black frame, digest for
   digest (13ab10433231a583). So it was never softpipe.

   naga writes `layout(binding = N)` only from desktop GLSL 4.20 up; below that
   the bindings have to be set after linking with `glUniformBlockBinding`. wgpu
   does that only when it believes the shader could not, and it decides that
   from `GL_ARB_compute_shader` rather than from the GLSL version
   (`SHADER_BINDING_LAYOUT = supports_compute`, wgpu-hal 30.0.1
   gles/adapter.rs). softpipe is GL 3.3 AND offers compute shaders, so every
   uniform block landed on binding 0 and ruffle's second one - the transforms -
   read as zeros. Every vertex collapsed. `MESA_EXTENSION_OVERRIDE=-GL_ARB_compute_shader`
   on the host's softpipe then drew **ef37bc5c264bfd83, byte-identical to
   llvmpipe**.

Both fixes live in `waterbox/gl-map.cpp`, in the extension filter that already
withheld buffer_storage, and both loaders go through them:
`GL_ARB_compute_shader` is withheld exactly when the context's GLSL is below
4.20, and the three calls that allocate multisampled storage clamp their sample
count to `GL_MAX_SAMPLES`. Neither is softpipe-specific - a GL 3.3 host GPU
across the bridge had the same two bugs - and neither touches `extern/`, so the
patch series is unchanged (verified: a fresh worktree at the pin plus 0001 and
0002 differs from extern/ruffle in 0 files).

### What was built

- `waterbox/setup-mesa.sh` - Flycast's recipe (Mesa 24.0.9, softpipe + OSMesa,
  static, no LLVM), with one fix: the cross file points pkg-config at an empty
  directory. On this machine a host libdrm has appeared since Flycast's Mesa
  was built; Mesa found it, turned on externalobjects.c, and stopped at
  `<linux/types.h>`, which the guest sysroot does not have. Flycast's copy has
  the same latent problem.
- `waterbox/gl-osmesa.cpp` - the OSMesa context and the software loader; a
  build without a guest Mesa compiles it as two refusals, so the core still
  builds bridge-only on a machine that cannot build Mesa.
- `renderer.rs` / `lib.rs` - `renderer::build(w, h, Which)`. The software path
  answers `context_id() = 0`, so the rebuild-on-context-change check never fires
  on it: its GL objects are guest memory and the savestate already has them.
  `opengl-hw` with no bridge refuses to start, naming the setting, rather than
  quietly drawing in software - the two draw different pixels.
- `run-wbx --no-gpu`, and a host with no GL context is no longer fatal to
  run-wbx (the software renderer does not need one).
- Mesa added to `package-licenses.json` (MIT). core.wbx is 116 MB, the package
  44 MB - in line with Flycast, which carries the same Mesa.

Proof there is no bridge in the software path: `CHIMERA_GL_TRACE=1` counts
**one** crossing for a whole run (the opcode-list handshake at SetGpuBridge,
before the renderer exists), and `run-wbx --no-gpu` draws the same
ef37bc5c264bfd83.

### The price: no anti-aliasing

softpipe cannot multisample at all, so every edge is hard. The image leg, run on
both renderers against ruffle's own expected pictures at 8 per channel:

| | within 1% of pixels | worst movie |
| --- | --- | --- |
| opengl-hw (host llvmpipe, 4x MSAA) | 197/197 | - |
| software (guest softpipe, 1x) | 190/197 | 2.4% |

The five past 1% are all text and gradient edges
(define_font_glyph_table_order 2.4%, overlay_onto_stage 2.1%,
edittext_selection_font_size 1.5%, acid-text-2 1.4%, acid-color-2 1.0%). The
gate now runs the image leg twice, software with `--no-gpu` at a stated 3%
budget and hardware at the old 1%; the per-channel tolerance is 8 in both, so a
pixel of the wrong colour still fails either. Of the seven software failures
at 1%, five are those edges; the other two were NO FRAME and an empty second
digest (bitmapdata_applyfilter_colormatrix, blend_scroll). mcl_target_gif89a,
which failed the same way in an earlier pass, drew identically three times
running alone, so these read as run-wbx exiting when its host EGL context failed
- which it no longer does - rather than as the renderer. That is an inference;
the gate run below is the measurement.

### What it costs: speed

New Star Soccer (nss102.swf, spoofed to kongregate.com), a blank 300-frame
movie, `chimera-run` on this WSL box (20 cores, no GPU), one run at a time.
Frames 0-60 are the loader and nearly free on either renderer, so the rate is
taken over frames 60-300 - the language screen, which ruffle redraws in full
every frame (`Player::render` always runs; see SetRenderingEnabled above).

Seek mode (the runner's default: frames drawn, not read back):

| | 60 frames | 300 frames | frames 60-300 |
| --- | --- | --- | --- |
| software, run A | 3.15 s | 285.0 s | **1174 ms a frame** (0.85 fps) |
| software, run B | | 293.1 s | 1208 ms a frame |
| opengl-hw on host llvmpipe, run A | 1.49 s | 70.7 s | **288 ms a frame** (3.5 fps) |
| opengl-hw on host llvmpipe, run B | | 71.0 s | 290 ms a frame |

So on the same box and the same frames the software renderer is **about 4x
slower than the bridge on llvmpipe**. That comparison is the only fair one this
machine can make, and it flatters the hardware path's absence: llvmpipe is
itself a software rasteriser, JIT-compiled and multithreaded. Against a real
GPU the gap is far wider - the GTX 1060 figure above is 18.59 ms for a New Star
Soccer frame, which would make softpipe on the order of 60x slower - but that was
measured on another machine, on another part of the game, and the rule above
about quoting llvmpipe as a GPU applies in reverse: this is an order of
magnitude, not a measurement.

Play mode (`--render-every-frame`: every frame read back, as a person watching):

| | 60 frames | 300 frames | frames 60-300 |
| --- | --- | --- | --- |
| software | 3.20 s | 279.5 s | **1151 ms a frame** (0.87 fps) |
| opengl-hw on host llvmpipe | 1.47 s | 70.4 s | **287 ms a frame** (3.5 fps) |

Reading the picture back costs the software renderer nothing measurable -
play mode is not slower than seek mode, within run-to-run noise - because the
"readback" is a copy out of memory the rasteriser has just written. All the time
is softpipe drawing. The ratio is 4.0x in both modes.

### Determinism

Same movie, same renderer, two separate processes, savestate at frame 250 and
screenshots at 150 and at the end:

| | pictures A vs B | savestate A vs B (101 MB / 89 MB) |
| --- | --- | --- |
| software | identical (f8f3604423e5a432) | 5,476 bytes differ |
| opengl-hw (llvmpipe) | identical (d65925b6624280ef) | 6,497 bytes differ |

The pictures agree on both. This is a static screen, which makes that the weaker
half of the evidence - and the final screenshot of a turbo run is not proof of
anything by itself, because chimera-run reads a frame back only when a
screenshot asks for that frame, so a final picture can be the last one read. The
reload runs below, which ask for frames 10 and 49 with every frame read back,
show the language screen really is unchanged. The savestates are the interesting half, because they differ
for **different reasons**:

- **software: every one of the 5,476 bytes is one host address.** The value
  0x5db878f27d88 in run A and 0x59ba6841ad88 in run B - a host heap address
  under ASLR (same low twelve bits, different base) - stored 1,043 times, plus a
  handful of tagged and shifted copies of the same page base. With it masked the
  two states are byte-identical. It sits in one repeated guest structure
  (guest pointer, 0, HOST, guest pointer, guest pointer; 936 copies) whose type
  words point into the first heap allocations after `_end` and have no symbol.
  The obvious suspect is ruled out as far as a static look can rule it out:
  the only guest code that reads `%gs:0x18` (the one host-owned slot a guest
  can see) is musl's own threading and locale code, every site going through
  `__pthread_self`, which dereferences the slot to a guest pointer rather than
  keeping it. Who does write the address is **not found**; that needs an
  instrumented guest. It does not appear in the hardware state at all, so it
  arrived with the in-guest Mesa stack or with something only that path
  exercises.
- **opengl-hw: none of its 6,497 bytes is a host address.** They are real data
  (the first is a double, 144.77 in one run and 114.28 in the other) - state
  the machine computed differently because the frames came back from a driver
  outside the sandbox at different moments.

So the claim that can be made with numbers: the software renderer's machine
state is identical run to run **except for one leaked host address**, while the
GPU path's state differs in the values themselves. Byte-identical savestates
cannot be claimed.

What the leak does NOT do, measured: break a state reopened in another process.
Saved at frame 250 in one chimera-run, loaded into a fresh one, every frame read
back:

| | straight through | reopened in a new process, frames 10 and 49 later |
| --- | --- | --- |
| software | f8f3604423e5a432 | **f8f3604423e5a432, both** |
| opengl-hw (llvmpipe) | d65925b6624280ef | 3aaaa5bc94aa9fc8, both - rebuilt its renderer, drew something else |
| software, avm1/color, saved at 5 | 883c227ffe3b3acc | 883c227ffe3b3acc |

That is the determinism result that matters for a TAS, and it is the software
renderer's: a greenzone reopened from disk draws what it drew, where the GPU
path, even on the same machine and driver, does not. (Which of the rebuilt
backend's caches is responsible on the GPU side was not chased; the BitmapCache
epoch gap above is a candidate.)

A first attempt at this test asked only for `--final-screenshot` and got a
blank 1920x1080 frame from BOTH renderers - the runner's picture when nothing
was read back, not a broken state. Worth knowing before trusting a turbo run's
final screenshot.

Two controls, with the pre-change package still installed on the Windows side
(ruffle-fef5fcf36163-dirty+local, hardware only, no guest Mesa), same movie,
same box:

- **the old core's savestates were never identical either.** Two runs to
  frame 250 did not even agree on the SIZE of the state (94,673,726 and
  92,023,614 bytes). Byte-identical savestates across processes is not a
  property this core had and lost; it is one the software renderer is now one
  leaked address away from.
- **the hardware path's picture is unchanged by this work.** Frame 150 through
  the old package, the new package's opengl-hw, and a second run of each: all
  d65925b6624280ef. On llvmpipe (GLSL 4.50, 4 samples) neither gl-map.cpp fix
  triggers, which is the point of keying them on what the driver says.

### The gate

`waterbox/run-gate.sh`, one run, alone, 1904 s:

| leg | result |
| --- | --- |
| trace / determinism / sandbox | 197/197 |
| image, software (--no-gpu, 97%) | **197/197** |
| image, opengl-hw (99%) | 197/197 |
| state (save and reload before every frame, now on software) | 30/30 |
| navigator | 92/92 |
| audio | 2/2 |
| input | 17/17 |
| sub-frame | 11/11 |
| settings | **16/17** |

The one failure was this work's, and it was the test's premise that had to
change, not its budget. `quality:low` must draw a different frame from the
default - and on the default renderer it cannot, because softpipe has one
sample and low and high both mean one sample. The quality checks now run on
opengl-hw against a base of their own (where they still prove the setting
reaches the renderer), and two assertions were added that are true of the new
default: on software, `quality:low` draws the SAME frame as the default (if that
ever fails, softpipe has learned to multisample and the 3% image budget should
come down), and `renderer=software` at its declared default is inert. That leg
alone was rerun after the edit: 14/14 picture checks, and its five address
checks were untouched and had passed. **The whole gate was not rerun after that
edit.**

The two NO FRAME results from the earlier standalone software pass did not
recur in the gate's software pass.
