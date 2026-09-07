#!/bin/bash
# Build the M0 native reference (waterbox/run-native), which drives ruffle_core
# with null backends. Needs a Java runtime on PATH (ruffle_core builds its AS3
# playerglobal with asc.jar); a portable JDK under ~/.local/jdk is enough.
set -eu
here="$(cd "$(dirname "$0")" && pwd)"
[ -d "$HOME/.local/jdk/bin" ] && export PATH="$HOME/.local/jdk/bin:$PATH"
command -v java >/dev/null || { echo "java not found: ruffle_core needs it to build playerglobal (apt install default-jdk, or a portable JDK on PATH)" >&2; exit 1; }
command -v cargo >/dev/null || { echo "cargo not found" >&2; exit 1; }
sh "$here/apply-patches.sh"
cd "$here/run-native"
cargo build --release
echo "built: $here/run-native/target/release/run-native"
