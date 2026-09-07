#!/bin/sh
# Ensures patches/ are applied to the extern/ruffle submodule tree.
#
# The submodule pin is PRISTINE upstream; the chimera patch set lives in
# patches/ and is applied into the working tree here (idempotent - run at the
# start of every build). Two patches: a clock that advances with the frames
# rather than with the wall, and a renderer that rebuilds its GPU objects when
# the context they belonged to is gone.
set -eu
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/.." && pwd)"
sub="$root/extern/ruffle"

if [ "$(git -C "$sub" rev-parse --show-toplevel 2>/dev/null)" != "$sub" ]; then
	echo "extern/ruffle is not checked out; run:" >&2
	echo "  git -C $root submodule update --init extern/ruffle" >&2
	exit 1
fi

# Applied all-or-nothing, and detected by one thing only patch 0001 introduces.
# Checking patch by patch is a trap: `git apply --check` on one of them fails
# the moment a neighbour has moved its lines.
marker="advance_virtual_time"
if grep -q "$marker" "$sub/core/src/player.rs" 2>/dev/null; then
	exit 0
fi

for p in "$root"/patches/*.patch; do
	git -C "$sub" apply "$p"
	echo "applied $(basename "$p")"
done
