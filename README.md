# chimera-core-ruffle

Ruffle (Flash / .swf) as a Chimera waterbox core. See docs/PLAN.md for the
design, the crate map, and the milestone plan.

- `extern/ruffle` - upstream, pinned, unmodified (clone with `--recursive`)
- `patches/` - the local patch series, applied into the submodule's working
  tree by `waterbox/apply-patches.sh` at the start of every build
- `waterbox/` - the adapter, the guest, the build and the gate

Status: in use. The core plays movies in the sandbox with a software renderer
and a GPU one, and CI runs the core gate and the frontend gate on every push.

## Using it in Chimera

Chimera ships no cores and downloads nothing. Download the `.chimeraCore`
package from this repository's
[Releases](https://github.com/ToolAssisted-run/chimera-core-ruffle/releases)
page, or build it, and put it in the `Cores` folder beside `Chimera.exe` (or
the folder chosen in File > Core Manager > Change folder...). File > Core
Manager lists the cores in that folder. The same package works on Linux and on
Windows.

## Building

`waterbox/build-package.sh -m <miniBox dir> -r <chimera checkout>` builds the
guest and writes `<chimera checkout>/build/Cores/ruffle.chimeraCore`. The
requirements, the steps that come before it and the gates are in
[docs/BUILDING.md](docs/BUILDING.md). [AGENTS.md](AGENTS.md) is the short
operating guide for a coding agent.
