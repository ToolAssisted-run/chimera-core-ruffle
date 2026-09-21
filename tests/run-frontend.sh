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
minibox="${MINIBOX_DIR:-$HOME/chimera/extern/chimera-common-minibox}"
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
# The upstream submodule, which is where the guest's path dependencies point
# too. RUFFLE_SRC still overrides, for a developer testing against another
# checkout.
ruffle_src="${RUFFLE_SRC:-$root/extern/ruffle}"
swfs="$ruffle_src/tests/tests/swfs"

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

# --- the frontend's own loader must accept the package ---
dlls="$chimera_root/build/dll"
libs="$chimera_root/build/meson-linux"
if ! command -v mcs >/dev/null || [ ! -f "$dlls/Chimera.Client.Common.dll" ]; then
	report "frontend:loads" SKIP "mono/mcs or the frontend's assemblies not built"
elif [ ! -f "$package" ]; then
	report "frontend:loads" SKIP "no package"
else
	if mcs -out:"$work/probe.exe" -r:"$dlls/Chimera.Client.Common.dll" \
		-r:"$dlls/Chimera.Emulation.Common.dll" -r:"$dlls/Chimera.Common.dll" \
		"$here/discovery-probe.cs" > "$work/probe.build.log" 2>&1; then
		out=$( cd "$dlls" && MONO_PATH=. LD_LIBRARY_PATH="$libs:$dlls" \
			timeout 180 mono "$work/probe.exe" "$(dirname "$package")" 2>&1 )
		echo "$out" > "$work/probe.log"
		if echo "$out" | grep -q "^RUFFLE OK"; then
			report "frontend:loads" PASS "discovered and loaded by the frontend's own loader"
		else
			report "frontend:loads" FAIL "$(echo "$out" | grep -E "RUFFLE (NOT|FAILED)" | head -1)"
		fi
	else
		report "frontend:loads" SKIP "could not build the probe (see tests/work/probe.build.log)"
	fi
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

# --- every restore rebuilds, the frame-0 anchor included (chimera issue 126) -
#
# On the GPU bridge the wgpu backend's objects live in the driver and a
# savestate carries only their NAMES, so the engine mints a fresh context id on
# every state load and this core rebuilds the backend when the id it stored
# beside those objects no longer matches (FrameAdvance, guest/src/lib.rs).
#
# One state used to slip through: the greenzone's FRAME-0 ANCHOR, taken right
# after Init and before the first frame advance. Init builds the hardware
# backend against the live context and never records WHICH one - gl_context is
# first written at the bottom of FrameAdvance - so the anchor is the only state
# in a session that carries a zero while real GL objects already exist, and zero
# was read as "the bridge cannot tell, nothing moved". It reaches a person
# because TAStudio goes to a frame by loading the state BEFORE it and emulating
# one forward, so frames 0 and 1 both load that anchor and frame 2 is the first
# that does not.
#
# What it measures: the calls that cross the bridge on the frame after a
# restore, against the busiest frame of a run that restored nothing. A backend
# rebuild is about 950 calls on top of whatever the frame was drawing anyway,
# and the core announces it on stderr, so both are asserted - a restore to frame
# 0 and a restore to frame 2 must BOTH rebuild, because the difference between
# them was the bug.
#
# WHAT THIS DOES NOT STAND IN FOR (chimera docs/gates.md, E): this SWF draws
# static text, so the backend it rebuilds is holding almost nothing, and
# llvmpipe is not a driver. What is proven is that the rebuild RUNS, not that a
# real movie's picture is right on real hardware.
if [ ! -x "$crun" ]; then
	report "engine:rebuild-at-zero" SKIP "chimera-run not built"
elif [ ! -f "$package" ]; then
	report "engine:rebuild-at-zero" SKIP "no package"
elif [ ! -f "$swfs/text/br_at_start/test.swf" ]; then
	report "engine:rebuild-at-zero" SKIP "ruffle's test corpus not present"
else
	gz="$work/glzero"
	mkdir -p "$gz"
	swf="$swfs/text/br_at_start/test.swf"
	printf '[Input]\nLogKey:#\n' > "$gz/none.txt"
	glrun() { # <movie> <out> <extra args...>
		glmovie="$1"; glout="$2"; shift 2
		( cd "$chimera_root" && CHIMERA_GL_TRACE=1 CHIMERA_GL_STATEAUDIT=1 \
			timeout 600 "$crun" "$package" "$swf" "$glmovie" \
			--settings '{"renderer":"opengl-hw"}' --frames 30 --gpu "$@" \
		) > "$glout" 2>&1 || true
	}
	# the busiest frame of a run, and the first traced frame after a restore
	biggestFrame() { grep -o "\[ce-gl\] frame [0-9]*: [0-9]* calls" "$1" | awk '{print $4}' | sort -n | tail -1; }
	afterRestore() {
		awk '/ce-gl-audit\] restore/ { seen = 1 }
		     seen && match($0, /\[ce-gl\] frame [0-9]+: [0-9]+ calls/) {
			s = substr($0, RSTART, RLENGTH); split(s, f, " "); print f[4]; exit }' "$1"
	}
	glrun "$gz/none.txt" "$gz/record.log" --record "$gz/movie.txt"
	if [ ! -s "$gz/movie.txt" ]; then
		report "engine:rebuild-at-zero" FAIL "could not record a movie to rewind through (see $gz/record.log)"
	else
		glrun "$gz/movie.txt" "$gz/plain.log"
		glrun "$gz/movie.txt" "$gz/rewind0.log" --greenzone 4096 --rewind-loop 0,1
		glrun "$gz/movie.txt" "$gz/rewind2.log" --greenzone 4096 --rewind-loop 2,1
		plain="$(biggestFrame "$gz/plain.log")"
		zero="$(afterRestore "$gz/rewind0.log")"
		two="$(afterRestore "$gz/rewind2.log")"
		if grep -q "^chimera gl: no context" "$gz/plain.log"; then
			report "engine:rebuild-at-zero" SKIP "this machine gives the bridge no GL context"
		elif [ -z "$plain" ] || [ -z "$zero" ] || [ -z "$two" ]; then
			report "engine:rebuild-at-zero" FAIL "nothing was traced across the bridge (see $gz)"
		elif grep -q "came from context" "$gz/plain.log"; then
			report "engine:rebuild-at-zero" FAIL "a run with no state load rebuilt the backend anyway: $(grep -o 'ruffle: GL objects came from.*' "$gz/plain.log" | head -1)"
		elif ! grep -q "came from context" "$gz/rewind0.log"; then
			report "engine:rebuild-at-zero" FAIL "restoring the frame-0 anchor made $zero GL calls on the next frame, against $plain for the busiest frame of a run with no restore, and the backend was never rebuilt"
		elif ! grep -q "came from context" "$gz/rewind2.log"; then
			report "engine:rebuild-at-zero" FAIL "restoring frame 2 made $two GL calls and the backend was never rebuilt"
		elif [ "$zero" -le "$plain" ] || [ "$two" -le "$((plain / 2))" ]; then
			report "engine:rebuild-at-zero" FAIL "a rebuild was announced but the calls do not show it: $zero after frame 0 and $two after frame 2, against $plain for the busiest frame with no restore"
		else
			report "engine:rebuild-at-zero" PASS "a restore rebuilds the backend wherever it lands - $zero calls after frame 0 and $two after frame 2, against $plain with no restore"
		fi
	fi
fi

echo
echo "$ok ok, $failed failed"
[ "$failed" -eq 0 ]
