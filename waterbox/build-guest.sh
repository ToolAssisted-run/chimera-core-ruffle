#!/bin/bash
# Build the Rust waterbox guest (M1). The proven recipe:
#  - nightly rustc + -Z build-std recompiles std for the musl guest with the
#    LARGE code model and static relocation (the waterbox fixed base), against
#    our musl (target x86_64-unknown-linux-musl).
#  - panic = "immediate-abort" strips the unwinder; std still keeps inert
#    personality/backtrace objects that reference libgcc's _Unwind_* API, so we
#    link tiny abort() stubs (unwind-stubs.c) rather than the host's small-model
#    glibc libgcc_eh (which cannot live at the large-model base).
#  - musl-gcc does the final link with emulibc + cxxglue + linkscript.T,
#    forcing the exports with -u.
# Needs: rustup nightly + rust-src; a built miniBox guest kit (musl-gcc,
# emulibc.c.o) under $MINIBOX/build/meson-linux; Java on PATH only if the guest
# later pulls a crate that needs it.
set -eu
here="$(cd "$(dirname "$0")" && pwd)"
minibox="${MINIBOX_DIR:-$HOME/miniBox}"
mb="$minibox/build/meson-linux"
guest="$here/guest"
[ -x "$mb/musl-gcc" ] || { echo "miniBox guest kit not built at $mb (musl-gcc missing)" >&2; exit 1; }

( cd "$guest" && cargo +nightly build --release --target x86_64-unknown-linux-musl )
a="$guest/target/x86_64-unknown-linux-musl/release/libruffle_guest.a"

mkdir -p "$here/build"
"$mb/musl-gcc" -c -mcmodel=large -fno-pic -fno-pie -fno-stack-protector \
  -o "$here/build/unwind-stubs.o" "$guest/unwind-stubs.c"

exports="-Wl,-u,Init -Wl,-u,GetAudio -Wl,-u,GetAudioSampleCount -Wl,-u,AllocSwf -Wl,-u,SetButton -Wl,-u,SetAxis -Wl,-u,SetTextInput -Wl,-u,FrameAdvance -Wl,-u,GetTty -Wl,-u,GetTtySize -Wl,-u,GetTraceDigest -Wl,-u,GetFrameCount -Wl,-u,GetLoadError -Wl,-u,IsRunning"
"$mb/musl-gcc" -mcmodel=large -fno-pic -fno-pie -static -no-pie \
  -Wl,--eh-frame-hdr,-O2,--no-relax -T "$minibox/source/guest/linkscript.T" \
  $exports -o "$here/build/core.wbx" \
  "$minibox/source/guest/cxxglue.c" "$mb/source/guest/emulibc.c.o" \
  "$a" "$here/build/unwind-stubs.o"
echo "built: $here/build/core.wbx"

# the frontend-free runner, against the same miniBox host the gates use
if [ -f "$mb/source/host/libminiboxhost.so" ]; then
	cc -O2 -I"$minibox/source/host" -o "$here/build/run-wbx" "$here/run-wbx.c" \
		"$mb/source/host/libminiboxhost.so" -Wl,-rpath,"$mb/source/host"
	echo "built: $here/build/run-wbx"
fi
