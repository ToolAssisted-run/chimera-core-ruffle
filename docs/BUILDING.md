# Building the Ruffle core

This repository builds Ruffle (Flash, `.swf`) as a sandboxed guest for
[Chimera](https://github.com/ToolAssisted-run/chimera) and packs it into one
file, `ruffle.chimeraCore`. The steps below are the ones
`.github/workflows/chimera.yml` runs on a fresh clone on a public runner; where
the workflow uses a GitHub Action, the manual equivalent is given. Cores are
built on Linux. The same package file works on Linux and on Windows, because
the guest inside it is run by Chimera's sandbox (miniBox) on either.

Placeholders used below:

- `<core>` - the checkout of this repository.
- `<chimera>` - a checkout of https://github.com/ToolAssisted-run/chimera.
- `<minibox>` - `<chimera>/extern/chimera-common-minibox`, a git submodule of
  Chimera: the sandbox host and the guest toolchain.

Commands are run from `<core>` unless a `cd` says otherwise.

## Requirements

**Operating system.** Linux, x86-64. CI runs on GitHub's `ubuntu-latest`.

**apt packages.** The workflow has two jobs and each installs its own list.

The core gate job (builds the core and runs `waterbox/run-gate.sh`):

```sh
sudo apt-get update
sudo apt-get install -y --no-install-recommends meson ninja-build build-essential cmake pkg-config python3 bison flex python3-mako python3-packaging default-jdk libegl-dev libegl-mesa0 libgl1-mesa-dri
```

The frontend gate job (builds Chimera and the package, runs
`tests/run-frontend.sh` and Chimera's contract tests):

```sh
sudo apt-get update
sudo apt-get install -y --no-install-recommends meson ninja-build build-essential cmake pkg-config python3 bison flex python3-mako python3-packaging default-jdk mono-complete xvfb libgl1-mesa-dev libx11-dev libxext-dev libasound2-dev
```

A machine that does both wants both lists. What the less obvious ones are for:

- `default-jdk` - `ruffle_core`'s build script compiles its ActionScript 3
  `playerglobal` with a JVM. The build scripts also put `~/.local/jdk/bin` on
  `PATH` when that directory exists.
- `bison flex python3-mako python3-packaging` - the guest Mesa build
  (`waterbox/setup-mesa.sh` uses the system meson only when
  `python3 -c "import mako, packaging"` works).
- `libegl-dev libegl-mesa0 libgl1-mesa-dri` - `run-wbx`, the gate's runner, is
  the host end of the GPU bridge. It compiles against EGL and needs a driver at
  run time. On a machine with no GPU that driver is Mesa's software rasteriser,
  reached through EGL's surfaceless platform.
- `mono-complete` - `tests/run-frontend.sh` compiles a probe with `mcs` and
  runs it with `mono`.

**Rust.** Through rustup. The workflow runs:

```sh
rustup toolchain install nightly --profile minimal -c rust-src
rustup default stable
```

- Channels: `stable` is the default toolchain and builds the native reference.
  `nightly` builds the guest. The workflow pins no version of either: each is
  what rustup resolves on the day. There is no `rust-toolchain` file.
- Profile and components: `nightly` is installed with `--profile minimal` plus
  the `rust-src` component. The guest is built with `-Z build-std`, which
  recompiles `std` from that source.
- Targets: none is added with rustup. The guest target is a custom target
  specification in this repository, `waterbox/guest/waterbox-guest.json`.
- Crate versions are pinned by the committed `Cargo.lock` files
  (`waterbox/guest/Cargo.lock`, `waterbox/run-native/Cargo.lock`).
- rustup itself is not installed by the workflow: the runner image has it.

**.NET SDK 8.0** (frontend gate only). The workflow uses
`actions/setup-dotnet@v4` with `dotnet-version: '8.0'`. Chimera's README gives
the manual install:

```sh
curl -sSL https://dot.net/v1/dotnet-install.sh | bash -s -- --channel 8.0
```

**What the scripts fetch or build themselves.**

- `waterbox/setup-mesa.sh` downloads Mesa 24.0.9
  (`https://archive.mesa3d.org/mesa-24.0.9.tar.xz`) with `curl` into
  `build/deps/`, checks its SHA256, unpacks it into `build/mesa/` and
  cross-builds it for the guest. `MESA_TARBALL=<path>` uses a tarball already
  on the machine.
- cargo downloads the crates named in the lock files.
- Nothing else is downloaded. The test content is upstream Ruffle's own test
  suite, which is inside the `extern/ruffle` submodule.

## Get the sources

This repository, with its submodule (`actions/checkout@v6` with
`submodules: true`):

```sh
git clone https://github.com/ToolAssisted-run/chimera-core-ruffle <core>
cd <core>
git submodule update --init
```

`extern/ruffle` is upstream Ruffle, pinned by commit. It has no submodules of
its own.

Chimera (`actions/checkout@v6` of `ToolAssisted-run/chimera` at `main`). The
core gate job needs only miniBox from it:

```sh
git clone https://github.com/ToolAssisted-run/chimera <chimera>
cd <chimera>
git submodule update --init extern/chimera-common-minibox
```

The frontend gate job builds Chimera itself and checks it out with
`submodules: recursive`:

```sh
cd <chimera>
git submodule update --init --recursive
```

Where the scripts look for these:

- `waterbox/build-package.sh` takes the Chimera checkout from `-r`, and
  `tests/run-frontend.sh` from `--chimera-root`. Without the option each tries
  `<core>/../chimera`, then `$HOME/chimera`.
- miniBox comes from `-m <dir>` or `MINIBOX_DIR`. Without either,
  `build-package.sh` uses `<chimera>/extern/chimera-common-minibox`, while
  `build-guest.sh`, `setup-mesa.sh` and `tests/run-frontend.sh` fall back to
  `$HOME/chimera/extern/chimera-common-minibox`. Pass it explicitly unless
  Chimera really is at `$HOME/chimera`.

## Build miniBox

The sandbox host and the guest toolchain. The workflow's commands:

```sh
mb=<minibox>
[ -f "$mb/build/meson-linux/build.ninja" ] || meson setup "$mb/build/meson-linux" "$mb"
meson compile -C "$mb/build/meson-linux"
[ -f "$mb/build/meson-cpp/build.ninja" ] || meson setup "$mb/build/meson-cpp" "$mb" -Dguest_cpp=true
meson compile -C "$mb/build/meson-cpp"
```

- `build/meson-linux` holds the sandbox host (`source/host/libminiboxhost.so`)
  and the C guest kit (`musl-gcc`, `source/guest/emulibc.c.o`).
- `build/meson-cpp` holds the C++ guest sysroot (`guest-sysroot/`, with
  libstdc++ and its headers). Mesa and the GPU bridge need it.

CI caches those two directories with `actions/cache@v4`, keyed on the miniBox
commit and its `meson.build`.

## Build the core

The workflow's order is: guest Mesa, native reference, guest core.

### 1. The guest Mesa

```sh
bash waterbox/setup-mesa.sh -m <minibox>
```

`bash`, not `sh`: the script uses `set -o pipefail`. It builds Mesa's softpipe
driver behind OSMesa, static, without LLVM, for the guest. This is the core's
`software` renderer.

- Options: `-m <miniBox dir>`, `-j N`.
- Environment: `MESA_TARBALL=<path>`; `MESA_BUILD_CONFIGURE_ONLY=1` stops after
  configuring; `CHIMERA_DEPS_DIR` moves the download cache;
  `MESA_BUILD_VENV` names the Python venv used when the system meson lacks
  Mako.
- Output: `build/mesa/build-guest2/` (static archives and the osmesa
  `target.c.o`). A second run prints `mesa: already built` and exits.
- Mesa's own last step, linking a shared `libOSMesa`, fails for a guest. That
  is expected: the script tolerates it and then checks the archives and the
  target object exist.

Without a guest Mesa `build-guest.sh` still produces a core and prints a NOTE,
but that core can only draw through the GPU bridge, and every sandbox leg of
the gate then fails with no frame.

### 2. The patches

Upstream is not modified in git. The changes this core needs are the numbered
files in `patches/`, applied to the working tree of `extern/ruffle` by
`waterbox/apply-patches.sh`. Both build scripts run it first, so it rarely
needs running by hand. After a build `git status` shows `extern/ruffle` as
modified. That is the applied series, and it is normal.

What the script does, in order:

1. It checks that `extern/ruffle` is a checked-out repository of its own. If
   not, it prints the `git submodule update` command and exits 1.
2. It lists every file the series touches and copies those files, as the
   submodule's HEAD has them, into a scratch directory.
3. It applies every patch, in order, to the scratch copy with `git apply`. If
   one does not apply it stops with `the series does not apply to the
   submodule's HEAD at <patch>`. That means the submodule was moved without
   rebasing the patches. The real tree is not touched.
4. If the working tree is pristine (every touched file equals HEAD), it applies
   each patch to the tree and prints `applied: <patch>`.
5. Otherwise the tree must be exactly what the whole series leaves behind. If
   it is, the script prints `already applied: all N patches` and exits 0.
6. If it is neither, the script names each file that differs
   (`not as the series leaves it: <file>`), says
   `extern/ruffle is partly patched`, and exits 1. It applies nothing.

The series is judged as a whole, never one patch at a time. A tree with some
patches applied, or with one file reverted by hand, or with an edit that is not
in any patch, is "partly patched" and stops the build. To start again from the
submodule's HEAD (this discards edits made in the tree):

```sh
git -C extern/ruffle reset --hard && git -C extern/ruffle clean -fd && waterbox/apply-patches.sh
```

`RUFFLE_TREE=<dir>` points the script at another checkout. It exists to test
the script without touching the tree a build depends on.

To add a patch:

1. Start from a fully patched tree: `waterbox/apply-patches.sh` prints
   `already applied`.
2. Edit the files under `extern/ruffle`. Do not commit there.
3. Write the change as the next numbered file in `patches/`, named like the
   others (`NNNN-chimera-<what-is-now-true>.patch`). It is a unified diff with
   `--- a/<path>` and `+++ b/<path>` headers, paths relative to the submodule
   root, as `git apply` reads them. It must hold only the new change, measured
   against the tree as the earlier patches leave it, not against HEAD. A plain
   `git diff` in the submodule is against HEAD: for a file an earlier patch
   already touches it would contain that patch too. Chimera's
   `docs/porting-a-core.md` describes the trap: snapshot such a file before
   editing and `diff -u` against the snapshot.
4. Run `waterbox/apply-patches.sh` again. With the new file in `patches/` and
   your edit still in the tree, it must print `already applied: all N patches`
   with N one higher. `not as the series leaves it: <file>` means the patch
   does not reproduce your edit.
5. Build and run the gate.

When the submodule pin moves, every patch must still apply in order to the new
HEAD. Step 3 of the script is what tells you.

### 3. The native reference

```sh
./waterbox/build-native.sh
```

It applies the patches and runs `cargo build --release` in
`waterbox/run-native` with the default toolchain (stable). It needs `java` and
`cargo` on `PATH` and says so if either is missing.

- Output: `waterbox/run-native/target/release/run-native`.
- What it is for: `run-native` drives `ruffle_core` on the host with null
  backends and no sandbox. The gate compares its trace with the `output.txt`
  upstream ships beside each test movie, and then compares the sandboxed core
  with it. It is also where debugging is done.

### 4. The guest core

```sh
MINIBOX_DIR=<minibox> ./waterbox/build-guest.sh
```

What it does:

1. Applies the patches.
2. Runs `cargo +nightly build --release` from `waterbox/guest`. The directory's
   `.cargo/config.toml` sets `build-std = ["std"]`,
   `target = "waterbox-guest.json"` and the rustflags
   `-C code-model=large -C relocation-model=static -C target-feature=+crt-static`.
   `waterbox-guest.json` is the stock `x86_64-unknown-linux-musl` target with
   `has-thread-local` off and `disable-redzone` on. The result is
   `waterbox/guest/target/waterbox-guest/release/libruffle_guest.a`.
3. Compiles the C and C++ pieces with miniBox's `musl-gcc`: the unwind stubs,
   the libgcc builtins, the generated GPU bridge
   (`waterbox/generated/gl-bridge-guest.cpp`), glad, `gl-map.cpp` and
   `gl-osmesa.cpp`.
4. Links everything, with the guest Mesa when it is there, into
   `waterbox/build/core.wbx`.
5. Builds `waterbox/build/run-wbx`, the runner that loads `core.wbx` through
   the miniBox host without the frontend. This needs
   `<minibox>/build/meson-linux/source/host/libminiboxhost.so`.

Environment: `MESA_GUEST_DIR=<path>` uses a guest Mesa built elsewhere;
`MESA_GUEST_DIR=` (set but empty) deliberately builds the bridge-only core.

### 5. A smoke test

The workflow runs one test before the gate, with stderr visible:

```sh
./waterbox/build/run-wbx waterbox/build/core.wbx extern/ruffle/tests/tests/swfs/avm1/add/test.swf --frames 1
```

What it prints must equal
`extern/ruffle/tests/tests/swfs/avm1/add/output.txt`. If it cannot, nothing in
the gate will pass.

## Build the package

```sh
./waterbox/build-package.sh -m <minibox> -r <chimera>
```

Options: `-m <miniBox dir>` (or `MINIBOX_DIR`) and `-r <chimera root>`. There
is no `-o`: the output location follows `-r`.

What it does:

1. Runs `build-guest.sh`, so the patches are applied and the guest is rebuilt.
   It does not run `setup-mesa.sh` or `build-native.sh`. Build the guest Mesa
   first or the package is the bridge-only core.
2. Runs miniBox's `source/guest/check-wbx.sh` on `core.wbx`.
3. Stages `core.wbx`, `waterbox.config`, `default_keybinds.json` and
   `file_slots.json` in `build/package-staging/`, with the licence texts that
   miniBox's `package-licenses.py` gathers from
   `waterbox/package-licenses.json`.
4. Stamps the version into the staged `waterbox.config` and writes
   `build.json`, which records what built the package.
5. Zips the staging directory deterministically, twice, and fails if the two
   SHA1s differ. It prints `package sha1 <hash>`.
6. Writes `<chimera>/build/Cores/ruffle.chimeraCore`, removes any
   `<chimera>/build/CoreCache/ruffle-*` directory, and prints
   `packaged -> <path>`.

**The version stamp.** A package's version is the commit it was built from. CI
sets `CORE_VERSION` to the commit (`${{ github.sha }}`) for this step. Without
`CORE_VERSION` the script stamps `<commit>+local`, where `<commit>` is the
12-character short hash, and `<commit>-dirty+local` when `git diff --quiet
HEAD` reports changes. The applied patches count as a change to
`extern/ruffle`, so a hand build normally carries `-dirty`. The commit's date,
in UTC, is stamped beside it as `versionDate`. Hand-built packages are for
testing: Chimera's publishing script refuses a version that carries `+local` or
`-dirty`.

**Releases.** On every green push to `main` the workflow's `publish` job
replaces the rolling `dev` release. The scheduled run (cron `0 4 * * *`)
publishes a dated `nightly-YYYY-MM-DD` release, only when `main` moved since
the last one. Nothing is published from a pull request.

## Install it into Chimera

Chimera ships no cores and downloads nothing: it has no network code. A user
downloads a core's `.chimeraCore` package from the core repository's Releases
page, or builds it, and puts it in Chimera's `Cores` folder.

- In a Chimera source checkout the cores folder is `<chimera>/build/Cores/`.
  `waterbox/build-package.sh -r <chimera>` writes the package straight there,
  so there is nothing more to do.
- In a release bundle it is the `Cores` folder beside `Chimera.exe`, or another
  folder chosen in File > Core Manager > Change folder... Copy
  `ruffle.chimeraCore` into it.
- File > Core Manager lists what is in that folder. Refresh List rescans it.

Published packages are at
https://github.com/ToolAssisted-run/chimera-core-ruffle/releases.

## Run the gates

Run one gate at a time, never two at once (`docs/PLAN.md`, "Rules").

### The core gate

```sh
./waterbox/run-gate.sh
```

Options: `--ruffle <ruffle checkout>` (or `RUFFLE_SRC`) and
`--bin <run-native>`.

It needs the native reference (it exits 1 without it) and the submodule's test
suite, `extern/ruffle/tests/tests/swfs`. It needs no game and no firmware, so
every leg runs in CI. The sandbox legs need `waterbox/build/core.wbx` and
`waterbox/build/run-wbx`. Without them every sandbox leg is skipped and the
last line says `sandbox SKIPPED`.

The legs, each with the list in `tests/` that drives it:

- trace (`oracle-list.txt`) - the native reference reproduces upstream's
  `output.txt` byte for byte; repeated runs give the same trace digest; the
  sandboxed core prints the same trace and the same audio digest as native.
- input (`input-list.txt`) - upstream's `input.json` streams, replayed as
  per-frame levels, give upstream's trace.
- sub-frame (`subframe-input-list.txt`) - a raised `fps` setting leaves the
  movie unchanged and makes a click inside one movie frame expressible.
- navigator (`navigator-list.txt`) - movies load their associated files from
  the guest's file system; reruns are identical.
- audio (`audio-list.txt`) - upstream's amplitude assertions hold, native and
  sandbox samples are byte-identical, reruns are identical.
- state (`state-list.txt`) - a run with the machine saved and reloaded before
  every frame (`--rerecord`) has the same trace, audio and picture digests as a
  plain run.
- image (`image-list.txt`) - each movie's frame is compared with upstream's
  `output.expected.png` on both renderers: `software` with `--no-gpu` (3% of
  pixels may differ) and `opengl-hw` (1%), 8 per channel in both. Each is
  rendered twice and the two must be identical.
- settings - the spoofed URL reaches the movie; quality and font substitution
  show in the frame; every other setting is invisible at its declared default.
- heap - the `Heap` bus holds a movie's number where it can be found, followed
  and changed, and reading it changes nothing.
- variables - a movie's ActionScript 1/2 variables are listed by name, followed
  when they move, changed by a write, and asking allocates nothing.

The `opengl-hw` image pass and the quality checks need a host OpenGL through
EGL. The script prints one summary line per leg and exits non-zero if any leg
failed. Each run's stderr goes to `/dev/null`, so a core that does not start at
all only says `FAIL` on every line: run the smoke test above to see why.

### The frontend gate

```sh
./tests/run-frontend.sh --chimera-root <chimera>
```

Options: `--chimera-root <path>` and `--minibox <path>` (or `MINIBOX_DIR`). It
needs Chimera built. The workflow's commands:

```sh
cd <chimera>
meson setup build/meson-linux --prefix "$PWD/build" --libdir dll
meson compile -C build/meson-linux
meson install -C build/meson-linux
dotnet build source/gui/Chimera.sln -c Release /nodeReuse:false -p:UseSharedCompilation=false
```

It tests the package at `<chimera>/build/Cores/ruffle.chimeraCore` and builds
it first if it is missing. Its legs:

- `package` - the package holds `core.wbx`, `waterbox.config`,
  `default_keybinds.json` and `file_slots.json`.
- `keybinds` - every declared button and axis has a default binding.
- `frontend:loads` - Chimera's own loader discovers and loads the package.
  SKIP without `mcs` or without Chimera's assemblies in `<chimera>/build/dll`.
- `engine:picture` - `chimera-run` plays a test movie from the package and the
  frame matches upstream's expected PNG. SKIP without
  `<chimera>/build/meson-linux/chimera-run`.
- `engine:rebuild-at-zero` - a state load makes the core rebuild its GL
  backend, at frame 0 and at frame 2. SKIP without `chimera-run`, or when the
  machine gives the bridge no GL context.

Its work directory is `tests/work/`.

### Chimera's contract tests

Run against the package just built: it must be readable, built for an ABI this
frontend runs, become a working factory, bind only buttons its controller
declares, and stamp a version.

```sh
cd <chimera>
CHIMERA_CORES_DIR=<chimera>/build/Cores dotnet test source/gui/Chimera.Tests.Client.Common/Chimera.Tests.Client.Common.csproj -c Release --nologo --filter "FullyQualifiedName~InstalledCorePackagesTests|FullyQualifiedName~MnemonicUniquenessTests"
```

## Files the core needs at run time

Game files are never in this repository or in the package. The user provides
them. This core needs no BIOS and no firmware (`waterbox.config` declares
`"firmware": []`). A project's files, from `waterbox/file_slots.json`:

- **Movie** - one `.swf`, required.
- **Files the movie loads** - any number, optional: `.swf`, `.mp3`, `.png`,
  `.jpg`, `.jpeg`, `.gif`, `.xml`, `.txt`, `.dat`, `.bin`, `.flv`, `.csv`. They
  must keep the names the movie asks for, because a movie loads them by name
  relative to itself.

The `renderer` setting defaults to `opengl-hw`, which draws on the machine's
GPU and needs a Chimera built with the GPU bridge; without one the core refuses
to start. `software` draws with the Mesa inside the package and needs nothing.

## Troubleshooting

- `extern/ruffle is not checked out` - the submodule is missing. Run
  `git submodule update --init` in `<core>`.
- `extern/ruffle is partly patched` - see "The patches" above for what it
  means and for the reset command.
- `the series does not apply to the submodule's HEAD at <patch>` - the
  submodule pin moved and the patches were not rebased.
- `java not found` from `build-native.sh` - install `default-jdk`, or put a
  JDK on `PATH` (the scripts also look in `~/.local/jdk/bin`).
- `miniBox guest kit not built at ... (musl-gcc missing)` - miniBox's
  `build/meson-linux` is not built, or `MINIBOX_DIR` points elsewhere.
- `guest sysroot not built at ... (build miniBox's meson-cpp first)` or
  `libstdc++ guest headers missing` - miniBox's `build/meson-cpp` is not built.
- `NOTE: no guest Mesa under ... - this core will only draw through the GPU
  bridge.` - run `waterbox/setup-mesa.sh`, then `build-guest.sh` again.
- `mesa: no usable meson - install python3-venv, or meson plus python3-mako` -
  install the apt packages above.
- `mesa: the tarball is not the release this core is pinned to` - the file in
  `build/deps/` (or `MESA_TARBALL`) is not Mesa 24.0.9 with the pinned SHA256.
- The build of `run-wbx` stops at `EGL/egl.h` - `libegl-dev` is missing.
- The gate prints `FAIL` for every test and nothing else - the core does not
  run at all. Run the smoke test; it keeps stderr.
- miniBox's `check-wbx.sh` refuses a guest that addresses memory below the
  stack pointer. The custom target turns the red zone off for Rust code, and
  the C code gets the same through the `musl-gcc` specs.
