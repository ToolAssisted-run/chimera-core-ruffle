#!/bin/bash
# The frontend half of the gate: take the package chimera actually loads, run a
# movie through chimera's own engine (chimera-run, the same session the GUI
# drives), and require the picture to be the one ruffle draws.
#
# The core gate proves the machine; this proves the integration - that the
# package's config, exports, keybinds and GL bridge line up with what the engine
# expects, which is the part a core cannot test on its own.
#
# Usage: ./run-frontend.sh [--chimera-root <path>] [--minibox <path>]
set -u
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/.." && pwd)"
chimera_root=""
minibox="${MINIBOX_DIR:-$HOME/miniBox}"
while [ $# -gt 0 ]; do
	case "$1" in
		--chimera-root) chimera_root="$2"; shift ;;
		--minibox) minibox="$2"; shift ;;
		*) echo "unknown option: $1" >&2; exit 2 ;;
	esac
	shift
done
if [ -z "$chimera_root" ]; then
	for c in "$root/../chimera" "$HOME/chimera"; do [ -d "$c" ] && { chimera_root="$c"; break; }; done
fi
[ -n "$chimera_root" ] && [ -d "$chimera_root" ] || { echo "chimera checkout not found" >&2; exit 1; }
chimera_root="$(cd "$chimera_root" && pwd)"

ok=0; failed=0
report() {
	if [ "$2" = PASS ]; then ok=$((ok+1)); printf '  %-22s PASS  %s\n' "$1" "$3"
	elif [ "$2" = SKIP ]; then printf '  %-22s SKIP  %s\n' "$1" "$3"
	else failed=$((failed+1)); printf '  %-22s FAIL  %s\n' "$1" "$3"; fi
}

work="$here/work"; rm -rf "$work"; mkdir -p "$work"
package="$chimera_root/build/Cores/ruffle.chimeraCore"
crun="$chimera_root/build/meson-linux/chimera-run"
swfs="$HOME/ruffle-src/tests/tests/swfs"

# --- the package must exist and carry what the engine reads ---
if [ ! -f "$package" ]; then
	MINIBOX_DIR="$minibox" sh "$root/waterbox/build-package.sh" -r "$chimera_root" > "$work/package.log" 2>&1
fi
if [ ! -f "$package" ]; then
	report "package" FAIL "not built (see tests/work/package.log)"
else
	missing=""
	for f in core.wbx waterbox.config default_keybinds.json file_slots.json; do
		unzip -l "$package" 2>/dev/null | grep -q " $f\$" || missing="$missing $f"
	done
	[ -z "$missing" ] && report "package" PASS "$(basename "$package")" \
		|| report "package" FAIL "missing:$missing"
fi

# --- every declared button and axis must have a default binding ---
if python3 "$here/check-keybinds.py" "$root/waterbox/waterbox.config" \
	"$root/waterbox/default_keybinds.json" > "$work/keys.txt" 2>&1; then
	report "keybinds" PASS "$(cat "$work/keys.txt")"
else
	report "keybinds" FAIL "$(cat "$work/keys.txt")"
fi

# --- the engine renders the movie ruffle draws ---
if [ ! -x "$crun" ]; then
	report "engine:picture" SKIP "chimera-run not built"
elif [ ! -f "$package" ]; then
	report "engine:picture" SKIP "no package"
else
	test_dir="$swfs/text/br_at_start"
	if [ ! -f "$test_dir/test.swf" ]; then
		report "engine:picture" SKIP "ruffle's test corpus not present"
	else
		python3 - "$work/movie.txt" "$root/waterbox/waterbox.config" <<'PYMOVIE'
import json, sys
cfg = json.load(open(sys.argv[2]))
naxes = len(cfg["input"]["axes"]); nbuttons = len(cfg["input"]["buttons"])
entry = "|" + ("%5d," % 0) * naxes + "." * nbuttons + "|"
open(sys.argv[1], "w").write((entry + "\n") * 5)
PYMOVIE
		( cd "$chimera_root" && timeout 600 "$crun" "$package" "$test_dir/test.swf" \
			"$work/movie.txt" --gpu --screenshot 4="$work/frame.tga" ) > "$work/run.log" 2>&1
		if [ ! -s "$work/frame.tga" ]; then
			report "engine:picture" FAIL "no frame written (see tests/work/run.log)"
		else
			res=$(python3 "$here/tga-compare.py" "$test_dir/output.expected.png" "$work/frame.tga" 8 2200 2>&1)
			if [ $? -eq 0 ]; then report "engine:picture" PASS "$res"
			else report "engine:picture" FAIL "$res"; fi
		fi
	fi
fi

echo
echo "$ok ok, $failed failed"
[ "$failed" -eq 0 ]
