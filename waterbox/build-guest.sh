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
minibox="${MINIBOX_DIR:-$HOME/chimera/extern/chimera-common-minibox}"
mb="$minibox/build/meson-linux"
guest="$here/guest"
[ -x "$mb/musl-gcc" ] || { echo "miniBox guest kit not built at $mb (musl-gcc missing)" >&2; exit 1; }

# ruffle_core's build script compiles playerglobal with a JVM. It is not on
# every PATH, so a local JDK is picked up when there is one.
[ -x "$HOME/.local/jdk/bin/java" ] && PATH="$HOME/.local/jdk/bin:$PATH"
export PATH
# The target is waterbox-guest.json, not the stock musl triple: it is the same
# target with has-thread-local turned off, so that Rust's thread locals go
# through musl's key-based path ([gs:0x18], the sandbox context in the TEB)
# rather than through %fs, which no host OS maintains for us. See
# guest/.cargo/config.toml for why nothing shorter works.
sh "$here/apply-patches.sh"
( cd "$guest" && cargo +nightly build --release )
a="$guest/target/waterbox-guest/release/libruffle_guest.a"

mkdir -p "$here/build"
"$mb/musl-gcc" -c -mcmodel=large -fno-pic -fno-pie -fno-stack-protector \
  -o "$here/build/unwind-stubs.o" "$guest/unwind-stubs.c"
# libgcc builtins the guest cannot borrow from the host (see the file)
"$mb/musl-gcc" -c -mcmodel=large -fno-pic -fno-pie -fno-stack-protector \
  -o "$here/build/libgcc-builtins.o" "$guest/libgcc-builtins.c"

# The guest's end of the GPU bridge. The opcodes are miniBox's shared,
# append-only table (source/gl/gl-entry-points.txt), because a package is a
# guest binary and the host half lives in whatever runs it - both sides have to
# mean the same thing by 'opcode 137'. waterbox/gl-entry-points.txt is just the
# subset this core's renderer names.
# libstdc++'s guest headers (cstdint, ...) live in the C++ guest kit
# (build/meson-cpp/guest-sysroot), not the C kit ($mb = meson-linux). Glob the
# gcc version rather than pinning it.
cppsys="$minibox/build/meson-cpp/guest-sysroot"
cxxver="$(ls -d "$cppsys"/include/c++/* | head -1)"
[ -d "$cxxver" ] || { echo "libstdc++ guest headers missing under $cppsys/include/c++" >&2; exit 1; }
cxxinc="-I$cxxver -I$cxxver/x86_64-linux-musl"
"$mb/musl-gcc" -c -x c++ -std=gnu++17 -mcmodel=large -fno-pic -fno-pie \
  -fno-stack-protector -fcf-protection=none -fno-exceptions -fno-rtti \
  $cxxinc -I"$here/extern/glad/include" -I"$minibox/source/gl" -I"$here" -I"$here/generated" \
  -o "$here/build/gl-bridge-guest.o" "$here/generated/gl-bridge-guest.cpp"
# glad itself: the shared generator installs the wrappers INTO glad's function
# pointers as well as into the lookup table, so the pointers have to exist here.
"$mb/musl-gcc" -c -mcmodel=large -fno-pic -fno-pie -fno-stack-protector \
  -fcf-protection=none -I"$here/extern/glad/include" \
  -o "$here/build/glad.o" "$here/extern/glad/src/gl.c"

# buffer mapping, emulated on this side (a driver pointer cannot cross)
"$mb/musl-gcc" -c -x c++ -std=gnu++17 -mcmodel=large -fno-pic -fno-pie \
  -fno-stack-protector -fcf-protection=none -fno-exceptions -fno-rtti \
  $cxxinc -I"$here/extern/glad/include" -I"$minibox/source/gl" -I"$here" -I"$here/generated" \
  -o "$here/build/gl-map.o" "$here/gl-map.cpp"

# The OpenGL that never leaves the sandbox: Mesa's softpipe behind OSMesa, built
# by waterbox/setup-mesa.sh into build/mesa. It is the core's DEFAULT renderer,
# so a package built without it can only draw through the bridge - the build
# still succeeds, loudly, because a machine that cannot build Mesa should still
# get a core.
#   MESA_GUEST_DIR=<path>   a guest Mesa built elsewhere
#   MESA_GUEST_DIR=         (empty) deliberately build the bridge-only core
root="$(cd "$here/.." && pwd)"
if [ "${MESA_GUEST_DIR+set}" = set ]; then
	mesa_guest_dir="$MESA_GUEST_DIR"
else
	mesa_guest_dir="$root/build/mesa"
fi
mesa_def=""
mesa_link=""
if [ -n "$mesa_guest_dir" ] && [ -d "$mesa_guest_dir/build-guest2" ]; then
	mesa_target="$(find "$mesa_guest_dir/build-guest2/src/gallium/targets/osmesa" -name 'target.c.o' | head -1)"
	mesa_archives="$(find "$mesa_guest_dir/build-guest2" -name '*.a' | tr '\n' ' ')"
	[ -n "$mesa_target" ] || { echo "guest Mesa at $mesa_guest_dir has no osmesa target.c.o; rerun waterbox/setup-mesa.sh" >&2; exit 1; }
	mesa_def="-DCHIMERA_GUEST_MESA"
	# The archives refer to each other both ways, so they are linked in a
	# group; the osmesa target's own object comes separately because the shared
	# library it belongs to cannot be linked for a guest (-fno-pic, large model)
	# and osmesa_create_screen lives in it. Mesa declares the pthread
	# mutexattr/key/once entry points WEAK as a build workaround - statically
	# linked those resolve to address zero, and Mesa calls null the first time it
	# makes a recursive mutex - so each is forced into the link.
	mesa_link="$mesa_target -Wl,--start-group $mesa_archives -Wl,--end-group \
	  -Wl,-u,pthread_mutexattr_init -Wl,-u,pthread_mutexattr_settype \
	  -Wl,-u,pthread_mutexattr_destroy -Wl,-u,pthread_once \
	  -Wl,-u,pthread_key_create -Wl,-u,pthread_cond_wait \
	  -Wl,-u,pthread_cond_broadcast -L$cppsys/lib -lstdc++"
else
	echo "NOTE: no guest Mesa under ${mesa_guest_dir:-<disabled>} - this core will only draw through the GPU bridge." >&2
fi
"$mb/musl-gcc" -c -x c++ -std=gnu++17 -mcmodel=large -fno-pic -fno-pie \
  -fno-stack-protector -fcf-protection=none -fno-exceptions -fno-rtti \
  $mesa_def $cxxinc -I"$here/extern/glad/include" -I"$minibox/source/gl" -I"$here" -I"$here/generated" \
  -o "$here/build/gl-osmesa.o" "$here/gl-osmesa.cpp"

exports="-Wl,-u,GetMemoryDomainCount -Wl,-u,GetMemoryDomainName -Wl,-u,GetMemoryDomainPtr -Wl,-u,GetMemoryDomainSize -Wl,-u,GetMemoryDomainWritable -Wl,-u,SetMousePixels -Wl,-u,GetVsyncNumerator -Wl,-u,GetVsyncDenominator -Wl,-u,SetGpuBridge -Wl,-u,GetVideoBgra -Wl,-u,GetVideoWidth -Wl,-u,GetVideoHeight -Wl,-u,Init -Wl,-u,SetSpoofUrl -Wl,-u,GetAudio -Wl,-u,GetAudioSampleCount -Wl,-u,AllocSwf -Wl,-u,SetButton -Wl,-u,SetAxis -Wl,-u,SetTextInput -Wl,-u,FrameAdvance -Wl,-u,GetTty -Wl,-u,GetTtySize -Wl,-u,GetTraceDigest -Wl,-u,GetFrameCount -Wl,-u,GetLoadError -Wl,-u,IsRunning -Wl,-u,BenchGlCrossings -Wl,-u,SetRenderingEnabled"
# -fno-stack-protector applies to cxxglue.c, which is compiled right here: the
# canary lives at %fs:0x28, and this guest has no %fs. Everything else in the
# link already carries the flag.
"$mb/musl-gcc" -mcmodel=large -fno-pic -fno-pie -fno-stack-protector -static -no-pie \
  -Wl,--eh-frame-hdr,-O2,--no-relax -T "$minibox/source/guest/linkscript.T" \
  $exports -o "$here/build/core.wbx" \
  "$minibox/source/guest/cxxglue.c" "$mb/source/guest/emulibc.c.o" \
  "$a" "$here/build/unwind-stubs.o" "$here/build/libgcc-builtins.o" \
  "$here/build/gl-bridge-guest.o" "$here/build/gl-map.o" "$here/build/gl-osmesa.o" \
  "$here/build/glad.o" $mesa_link -lm
echo "built: $here/build/core.wbx"

# the frontend-free runner, against the same miniBox host the gates use
if [ -f "$mb/source/host/libminiboxhost.so" ]; then
	cc -O2 -DCHIMERA_GL_BRIDGE -I"$minibox/source/host" -I"$minibox/source/gl" -I"$here" \
		-I"$here/extern/glad/include" -I"$here/generated" \
		-o "$here/build/run-wbx" "$here/run-wbx.c" "$here/gl-host.c" \
		"$here/extern/glad/src/gl.c" \
		"$mb/source/host/libminiboxhost.so" -Wl,-rpath,"$mb/source/host" -lEGL
	echo "built: $here/build/run-wbx"
fi
