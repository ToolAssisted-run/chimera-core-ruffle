#!/usr/bin/env bash
# Builds the OpenGL this core draws through when it draws in software: Mesa's
# softpipe driver behind the gallium OSMesa front end, static, no LLVM,
# cross-built for the miniBox guest.
#
# There is no GPU inside a sandbox and no host driver to borrow one from, so a
# core that wants a deterministic picture brings its own OpenGL. softpipe is
# plain C - no JIT, no dispatch on host CPU features - so it draws the same
# picture on every machine, which is the only kind of renderer a core that
# replays movies can promise anything about. llvmpipe would be far faster and
# is not an option: it is fast because it JITs through LLVM, and that would mean
# carrying LLVM in the guest.
#
# The same recipe, the same release and the same options as the Flycast core's
# copy (chimera-cores/flycast/waterbox/setup-mesa.sh). Each core that needs Mesa
# fetches its own - Chimera ships no cores and has no business carrying a core's
# dependencies - and they are pinned to the same tarball so that what links here
# is what links there.
#
# Produces:  build/mesa/build-guest2/  (the *.a archives plus the gallium osmesa
# target.c.o that waterbox/build-guest.sh globs and links).
#
# Usage: ./setup-mesa.sh [-m <miniBox dir>] [-j N]
#   MESA_TARBALL=<path>            use a tarball already on this machine
#   MESA_BUILD_CONFIGURE_ONLY=1    validate the recipe without the ~15min compile
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/.." && pwd)"
minibox="${MINIBOX_DIR:-$HOME/chimera/extern/chimera-common-minibox}"
jobs="$(nproc)"
while getopts "m:j:" opt; do
	case "$opt" in
		m) minibox="$OPTARG" ;;
		j) jobs="$OPTARG" ;;
		*) exit 2 ;;
	esac
done
minibox="$(cd "$minibox" && pwd)"

# The guest sysroot must be the C++ (meson-cpp) one: it carries libstdc++ and
# the C++ headers the C-only meson-linux sysroot lacks, and Mesa's GLSL
# compiler is C++.
sr="$minibox/build/meson-cpp/guest-sysroot"
[ -f "$sr/lib/musl-gcc.specs" ] || { echo "guest sysroot not built at $sr (build miniBox's meson-cpp first)" >&2; exit 1; }
gccver="$(basename "$(ls -d "$sr"/include/c++/* | head -1)")"

version="24.0.9"
sha256="51aa686ca4060e38711a9e8f60c8f1efaa516baf411946ed7f2c265cd582ca4c"
url="https://archive.mesa3d.org/mesa-$version.tar.xz"

mesa="$root/build/mesa"
build="$mesa/build-guest2"
cache="${CHIMERA_DEPS_DIR:-$root/build/deps}"
tarball="${MESA_TARBALL:-$cache/mesa-$version.tar.xz}"

if [ -d "$build" ] && find "$build" -name '*.a' -print -quit 2>/dev/null | grep -q . \
	&& find "$build/src/gallium/targets/osmesa" -name 'target.c.o' -print -quit 2>/dev/null | grep -q .; then
	echo "mesa: already built in $build"
	exit 0
fi

if [ ! -f "$tarball" ]; then
	mkdir -p "$cache"
	echo "mesa: fetching $url"
	curl -fL --retry 3 -o "$tarball.part" "$url"
	mv "$tarball.part" "$tarball"
fi
echo "$sha256  $tarball" | sha256sum -c - >/dev/null || {
	echo "mesa: the tarball is not the release this core is pinned to" >&2
	exit 1
}

if [ ! -d "$mesa/src/gallium" ]; then
	rm -rf "$mesa"
	mkdir -p "$mesa"
	tar -xf "$tarball" -C "$mesa" --strip-components=1
fi

# Prefer a system meson that already has what Mesa needs; fall back to a
# private venv. Not the other way round: `python3 -m venv` fails outright on a
# machine without python3-venv, and there is no reason to need it when the host
# can already generate Mesa's GL dispatch.
meson=""
if command -v meson >/dev/null && python3 -c "import mako, packaging" 2>/dev/null; then
	meson="$(command -v meson)"
else
	venv="${MESA_BUILD_VENV:-$HOME/.cache/chimera-mesa-build-venv}"
	if [ ! -x "$venv/bin/meson" ]; then
		python3 -m venv "$venv" || {
			echo "mesa: no usable meson - install python3-venv, or meson plus python3-mako" >&2
			exit 1
		}
		"$venv/bin/pip" -q install --upgrade pip
		"$venv/bin/pip" -q install meson ninja mako packaging
	fi
	meson="$venv/bin/meson"
fi

# single-`-specs` wrapper compilers: passing -specs through meson's *_args
# doubles it and the specs file errors, so the flag rides inside the compiler.
cat > "$mesa/gw-cc"  <<EOF
#!/bin/sh
exec gcc "\$@" -specs $sr/lib/musl-gcc.specs
EOF
cat > "$mesa/gw-cxx" <<EOF
#!/bin/sh
exec g++ "\$@" -specs $sr/lib/musl-gcc.specs
EOF
chmod +x "$mesa/gw-cc" "$mesa/gw-cxx"

# large code model, static reloc, no %fs stack guard, the guest's own libstdc++.
cat > "$mesa/guest-cross.ini" <<EOF
[binaries]
c = '$mesa/gw-cc'
cpp = '$mesa/gw-cxx'
ar = 'ar'
strip = 'strip'
pkg-config = 'pkg-config'

[host_machine]
system = 'linux'
cpu_family = 'x86_64'
cpu = 'x86_64'
endian = 'little'

[properties]
needs_exe_wrapper = true
# The guest has no system libraries, so pkg-config must find NOTHING. Left
# pointing at the host's, Mesa picks up whatever happens to be installed on the
# machine doing the build: a host libdrm turns on externalobjects.c, which
# includes <linux/types.h>, which the musl guest sysroot does not have, and the
# build stops there. What links must not depend on what this machine has.
pkg_config_libdir = ['$mesa/no-host-packages']

[built-in options]
c_args = ['-mcmodel=large', '-mstack-protector-guard=global', '-fno-stack-protector', '-fno-pic', '-fno-pie', '-fcf-protection=none']
cpp_args = ['-mcmodel=large', '-mstack-protector-guard=global', '-fno-stack-protector', '-fno-pic', '-fno-pie', '-fcf-protection=none', '-fexceptions', '-I$sr/include/c++/$gccver', '-I$sr/include/c++/$gccver/x86_64-linux-musl']
EOF

# softpipe + gallium OSMesa, static, no LLVM, nothing that pulls a host lib.
# -Dshared-glapi=disabled is ESSENTIAL: otherwise _glapi_tls_Context lands only
# in a .so this core cannot link. zlib/expat are vendored via wraps.
opts="-Dforce_fallback_for=zlib,expat -Dgallium-drivers=swrast -Dvulkan-drivers= \
  -Dllvm=disabled -Dosmesa=true -Dopengl=true -Dglx=disabled -Degl=disabled \
  -Dgbm=disabled -Dglvnd=false -Dplatforms= -Dgles1=disabled -Dgles2=disabled \
  -Ddefault_library=static -Dbuild-tests=false -Dzstd=disabled -Dshared-glapi=disabled"

mkdir -p "$mesa/no-host-packages"

if [ -f "$build/build.ninja" ]; then
	"$meson" setup --reconfigure "$build" "$mesa" --cross-file "$mesa/guest-cross.ini" $opts
else
	"$meson" setup "$build" "$mesa" --cross-file "$mesa/guest-cross.ini" $opts
fi

[ "${MESA_BUILD_CONFIGURE_ONLY:-}" = 1 ] && { echo "mesa: configured OK (configure-only)"; exit 0; }

# Mesa's final shared osmesa .so fails to link for the guest (__dso_handle,
# -fno-pic, large model) - EXPECTED. This core links the static archives plus
# the target's own object, both built before that step. So tolerate the .so
# failure and verify the artifacts that are actually needed.
"$meson" compile -C "$build" -j "$jobs" || true

target_o="$(find "$build/src/gallium/targets/osmesa" -name 'target.c.o' 2>/dev/null | head -1)"
archives="$(find "$build" -name '*.a' 2>/dev/null | wc -l)"
if [ -n "$target_o" ] && [ "$archives" -gt 0 ]; then
	echo "mesa: ready - $archives archives + $target_o"
else
	echo "mesa: the build did NOT produce the osmesa target.c.o / archives" >&2
	exit 1
fi
