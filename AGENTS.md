# AGENTS.md - Ruffle core for Chimera

This repository builds Ruffle (Flash, `.swf`) as a sandboxed guest for
[Chimera](https://github.com/ToolAssisted-run/chimera), a frontend for
tool-assisted speedruns, and packs it into one file, `ruffle.chimeraCore`.
Upstream Ruffle is a pinned submodule; everything here is the adapter around
it, the build scripts and the gate. The core is Rust. Builds run on Linux, and
the one package works on Linux and on Windows.

## Layout

- `extern/ruffle` - upstream Ruffle, a submodule pinned by commit. Its test
  suite (`tests/tests/swfs`) is the gate's content.
- `patches/` - the numbered patch series applied to `extern/ruffle` by
  `waterbox/apply-patches.sh`, which refuses a partly patched tree.
- `waterbox/guest/` - the guest crate (`src/lib.rs` is the export surface),
  its `.cargo/config.toml` and its target, `waterbox-guest.json`.
- `waterbox/run-native/` - the native reference: `ruffle_core` on the host.
  `waterbox/run-wbx.c`, `gl-host.c` - the runner that loads `core.wbx`.
- `waterbox/setup-mesa.sh`, `build-native.sh`, `build-guest.sh`,
  `build-package.sh` - the builds.
- `waterbox/run-gate.sh` - the core gate. `tests/run-frontend.sh` - the
  frontend gate. `tests/*-list.txt` - which upstream tests each leg runs.
- `waterbox/waterbox.config`, `file_slots.json`, `default_keybinds.json`,
  `package-licenses.json` - what the package declares.
- `waterbox/input-table.py` - the one source of the input wire order. It
  writes `waterbox/guest/src/input_table.rs`; with `--json` it prints the
  `input` section for `waterbox.config`. Edit the table, not its outputs.
- `waterbox/generated/` - the GPU bridge's generated halves. Never edit them
  by hand. `waterbox/gl-entry-points.txt` lists the entry points used.
- `docs/PLAN.md` - the design log. `.github/workflows/chimera.yml` - CI, the
  authoritative build recipe. `build/`, `waterbox/build/`, `*/target/` and
  `tests/work/` are build output.

## Set up the build environment

`<chimera>` is a Chimera checkout and `<minibox>` is
`<chimera>/extern/chimera-common-minibox`.

```sh
sudo apt-get update
sudo apt-get install -y --no-install-recommends meson ninja-build build-essential cmake pkg-config python3 bison flex python3-mako python3-packaging default-jdk libegl-dev libegl-mesa0 libgl1-mesa-dri
rustup toolchain install nightly --profile minimal -c rust-src
rustup default stable
git submodule update --init
git clone https://github.com/ToolAssisted-run/chimera <chimera>
git -C <chimera> submodule update --init extern/chimera-common-minibox
mb=<minibox>
[ -f "$mb/build/meson-linux/build.ninja" ] || meson setup "$mb/build/meson-linux" "$mb"
meson compile -C "$mb/build/meson-linux"
[ -f "$mb/build/meson-cpp/build.ninja" ] || meson setup "$mb/build/meson-cpp" "$mb" -Dguest_cpp=true
meson compile -C "$mb/build/meson-cpp"
```

The frontend gate also needs Chimera itself built, .NET 8.0 and
`mono-complete`: see `docs/BUILDING.md`.

## Build

```sh
bash waterbox/setup-mesa.sh -m <minibox>   # guest Mesa, once; fetches a pinned tarball
./waterbox/build-native.sh                 # the native reference (stable cargo)
./waterbox/build-package.sh -m <minibox> -r <chimera>
```

`build-package.sh` runs `build-guest.sh` (which applies the patches and runs
`cargo +nightly build --release` in `waterbox/guest`) and writes
`<chimera>/build/Cores/ruffle.chimeraCore`. To build only the guest
(`waterbox/build/core.wbx` and `waterbox/build/run-wbx`), then check that the
core runs at all:

```sh
MINIBOX_DIR=<minibox> ./waterbox/build-guest.sh
# must print what extern/ruffle/tests/tests/swfs/avm1/add/output.txt holds
./waterbox/build/run-wbx waterbox/build/core.wbx extern/ruffle/tests/tests/swfs/avm1/add/test.swf --frames 1
```

Pass `-m` and `-r`: without them the scripts guess (`$HOME/chimera`).
Skipping `setup-mesa.sh` does not fail the build: it gives a core that can
only draw through the GPU bridge, and the gate's sandbox legs then fail.

## Install the core into Chimera

Chimera ships no cores and downloads nothing: a package is put in its `Cores`
folder by hand. In a source checkout `build-package.sh -r <chimera>` already
wrote it there, as `<chimera>/build/Cores/ruffle.chimeraCore`. For a release
bundle, copy the file into the `Cores` folder beside `Chimera.exe`, or into
the folder chosen in File > Core Manager > Change folder... File > Core
Manager lists the folder; Refresh List rescans it.

A hand-built package is stamped `<commit>+local` (`-dirty` when the tree has
changes) and is for testing. CI stamps the commit and publishes the `dev` and
`nightly-YYYY-MM-DD` releases.

## Test before you commit

```sh
./waterbox/run-gate.sh
# also, with Chimera built, when the package or what the frontend sees changed:
./tests/run-frontend.sh --chimera-root <chimera>
cd <chimera> && CHIMERA_CORES_DIR=<chimera>/build/Cores dotnet test source/gui/Chimera.Tests.Client.Common/Chimera.Tests.Client.Common.csproj -c Release --nologo --filter "FullyQualifiedName~InstalledCorePackagesTests|FullyQualifiedName~MnemonicUniquenessTests"
```

`run-gate.sh` needs the native reference, `waterbox/build/core.wbx` and
`waterbox/build/run-wbx`, and no game or firmware: the content is upstream's
test suite. Every summary line must report 0 failures and the script must exit
0. If the last line says `sandbox SKIPPED`, the guest was not built and the
gate proved little. Run one gate at a time, never two at once. In
`run-frontend.sh` a SKIP names what was missing; never report a skipped leg as
passed.

## Rules of this repository

- Never commit inside `extern/ruffle`. A change to upstream is a numbered
  file in `patches/`, applied at the start of every build. `git status` shows
  `extern/ruffle` as modified once the series is applied. That is normal: do
  not stage it, reset it or clean it to tidy up.
- The series is judged as a whole: an edit in `extern/ruffle` that is not in
  a patch stops the next build as "partly patched". To add a patch, edit the
  patched tree, write the change against the tree as the earlier patches
  leave it (a plain `git diff` is against HEAD) as the next
  `NNNN-chimera-<what-is-now-true>.patch`, then run
  `waterbox/apply-patches.sh` and see `already applied: all N patches`.
- Determinism is the product. The guest must not read host time, host
  randomness, the network or anything else that differs between runs, and a
  savestate must round-trip. The gate checks it; a change that breaks it is a
  bug. The one stated exception is the picture on the `opengl-hw` renderer,
  which a host GPU draws; the movie's own state stays deterministic even then.
- Run the gate before committing. A new leg needs a negative control: break
  the thing it checks, see the leg fail, and say so in the commit message.
  Read Chimera's `docs/gates.md` before writing a leg.
- Never commit game files. The only Flash content allowed is upstream's test
  suite in the submodule and the movies the gate makes (`tests/make-*-swf.py`).
- Never add network access. The guest's navigator serves the files the
  project mounts and connects no socket.
- CI runs the shell scripts directly. Keep them executable (git mode 100755).
- Documentation prose is plain ASCII.
- Commit messages follow the log: `type(scope): a sentence saying what is now
  true`, for example `fix(gate): the image leg was resetting the trace leg's
  failure count`. Types in use: `feat`, `fix`, `perf`, `build`, `ci`, `docs`.
  A Chimera issue is cited as `(chimera#N)`. The body says what changed, why,
  what was measured, the gate's tally and the negative control.
- Do not edit `.github/workflows` unless the task is the workflow.
- Problems with this core are reported in Chimera's issue tracker, not here.

## Where to read more

- `docs/BUILDING.md` - requirements, every script's options, how to add a
  patch, the gates' legs, troubleshooting.
- `docs/PLAN.md` - why the core is built the way it is. It is long: search it.
- In the Chimera checkout: `README.md` ("Building"),
  `docs/porting-a-core.md`, `docs/core-manager.md`, `docs/gates.md`.
