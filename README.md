# chimera-core-ruffle

Ruffle (Flash / .swf) as a Chimera waterbox core. See docs/PLAN.md for the
design, the crate map, and the milestone plan.

- `extern/ruffle` - upstream, pinned, unmodified (clone with `--recursive`)
- `patches/` - the local patch series, applied into the submodule's working
  tree by `waterbox/apply-patches.sh` at the start of every build
- `waterbox/` - the adapter, the guest, the build and the gate

Status: scaffold + plan (2026-09-03). M0 (native reference) is blocked on a
Java toolchain, which ruffle_core needs to build its AS3 playerglobal.
