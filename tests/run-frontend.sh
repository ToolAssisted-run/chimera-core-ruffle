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
# A game runs only in a project (Chimera, 2026-10-26): crun_game <package>
# <game> <movie> [chimera-run's options] makes one of the game and the movie,
# with any --settings and --firmware given, and runs it with the rest
# (each run under CRUN_TIMEOUT seconds when it is set).
crun_game() {
	local pkg="$1" game="$2" movie="$3" made=() run=() dir
	shift 3
	while [ $# -gt 0 ]; do
		case "$1" in
			--settings) made+=("$1" "$2"); shift ;;
			--settings=*) made+=(--settings "${1#--settings=}") ;;
			--firmware) made+=("$1" "$2"); run+=("$1" "$2"); shift ;;
			--firmware=*) made+=(--firmware "${1#--firmware=}"); run+=(--firmware "${1#--firmware=}") ;;
			*) run+=("$1") ;;
		esac
		shift
	done
	dir="$(mktemp -d "$work/project.XXXXXX")"
	timeout "${CRUN_TIMEOUT:-0}" "$crun" --new-project "$dir/game.chimeraProject" "$pkg" "$game" "$movie" "${made[@]}" > "$dir/make.log" 2>&1 \
		|| { echo "chimera-run: no project made: $(tail -1 "$dir/make.log")" >&2; return 2; }
	timeout "${CRUN_TIMEOUT:-0}" "$crun" --project "$dir/game.chimeraProject" "$pkg" --files "$(dirname "$game")" "${run[@]}"
}
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

# --- Chimera's window must open a project of the package and run it ---
# The engine's parser and the window's are different programs reading the same
# file, and the window's is the strict one: a package can pass the gate and
# chimera-run and still be unreadable to the program a user opens. So this runs
# the window itself, headless (it needs no display), on a project made from the
# package and one of ruffle's own test movies, for a few frames.
#
# The window offers a GPU only to a core whose "renderer" setting is named
# something-hw, and this core cannot start without one, so the default is
# checked as well.
chimera_exe="$chimera_root/build/meson-linux/source/ui/chimera"
if [ ! -x "$chimera_exe" ]; then
	report "frontend:loads" SKIP "Chimera's window not built: $chimera_exe"
elif [ ! -f "$package" ]; then
	report "frontend:loads" SKIP "no package"
elif [ ! -f "$swfs/text/br_at_start/test.swf" ]; then
	report "frontend:loads" SKIP "ruffle's test corpus not present"
else
	wl="$work/window"
	mkdir -p "$wl"
	python3 - "$wl/movie.txt" "$root/waterbox/waterbox.config" <<'PYMOVIE'
import json, sys
cfg = json.load(open(sys.argv[2]))
naxes = len(cfg["input"]["axes"]); nbuttons = len(cfg["input"]["buttons"])
entry = "|" + ("%5d," % 0) * naxes + "." * nbuttons + "|"
open(sys.argv[1], "w").write((entry + "\n") * 5)
PYMOVIE
	default_renderer=$(python3 -c '
import json, sys
for s in json.load(open(sys.argv[1])).get("settings", []):
    if s.get("name") == "renderer": print(s.get("default", ""))' "$root/waterbox/waterbox.config")
	# the movie file beside the project, where the window looks for it
	cp "$swfs/text/br_at_start/test.swf" "$wl/test.swf"
	if ! timeout 600 "$crun" --new-project "$wl/game.chimeraProject" "$package" "$wl/test.swf" "$wl/movie.txt" > "$wl/make.log" 2>&1; then
		report "frontend:loads" FAIL "no project made: $(tail -1 "$wl/make.log")"
	else
		out=$( cd "$chimera_root" && env ALSOFT_DRIVERS=null CHIMERA_CORES_DIR="$chimera_root/build/Cores" 			CHIMERA_QT_CACHE="$wl/qt-cache" CHIMERA_QT_RECOVERY="$wl/qt-recovery" 			CHIMERA_QT_HASHES="$wl/qt-file-hashes.txt" CHIMERA_QT_CHIMERA_SETTINGS="$wl/no-chimera-config.ini" 			timeout 600 "$chimera_exe" --headless "--config=$wl/settings.json" 			"--project=$wl/game.chimeraProject" "--lua=$here/window-loads.lua" 2>&1 )
		rc=$?
		echo "$out" > "$wl/window.log"
		case "$default_renderer" in
			*-hw) hw=1 ;;
			*) hw=0 ;;
		esac
		if [ "$rc" -eq 0 ] && echo "$out" | grep -q "RUFFLE OK frames=" && [ "$hw" = 1 ]; then
			report "frontend:loads" PASS "opened and run by Chimera's window; renderer default \"$default_renderer\" asks for a GPU"
		elif [ "$hw" != 1 ]; then
			report "frontend:loads" FAIL "the window would not offer a GPU: renderer default is \"$default_renderer\""
		else
			report "frontend:loads" FAIL "exit $rc: $(echo "$out" | tail -1 | cut -c1-160) (see tests/work/window/window.log)"
		fi
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
		( cd "$chimera_root" && CRUN_TIMEOUT=600 crun_game "$package" "$test_dir/test.swf" \
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
			CRUN_TIMEOUT=600 crun_game "$package" "$swf" "$glmovie" \
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

# --- a renderer rebuilt after a load lets go of the old one ---
#
# A load in a new GL context makes the core build a new renderer. The old one
# used to stay alive as long as anything still held a shape or bitmap it had
# made, and ruffle_core lets go of those only when it next draws them - so a
# shape drawn once and never again kept a whole old renderer in the guest's
# memory. Over a long TAStudio session that filled the sandbox: Corporate
# Climber died at frame 48775 with "memory allocation of 12800000 bytes
# failed" (2026-10-22). Patch 0008 has a dropped renderer take back the GPU
# objects of every handle it made.
#
# The movie shows a different shape on every frame, each only once, and the
# run loads a state before every frame, so every frame's renderer is left
# holding one shape nobody draws again. A state's size is the guest's memory.
# Building a second renderer costs about a megabyte once; what must not happen
# is growth with every rebuild, so the test compares how much the state grows
# over the last 120 frames - 120 more rebuilds - with and without the loads.
if [ ! -x "$crun" ]; then
	report "engine:rebuild-frees" SKIP "chimera-run not built"
elif [ ! -f "$package" ]; then
	report "engine:rebuild-frees" SKIP "no package"
else
	rf="$work/rebuild-frees"
	mkdir -p "$rf"
	python3 "$here/make-shapes-swf.py" "$rf/shapes.swf" 240
	printf '[Input]\nLogKey:#\n' > "$rf/none.txt"
	rfrun() { # <movie> <out> <extra args...>
		rfmovie="$1"; rfout="$2"; shift 2
		( cd "$chimera_root" && CRUN_TIMEOUT=900 crun_game "$package" "$rf/shapes.swf" "$rfmovie" \
			--settings '{"renderer":"opengl-hw"}' --frames 240 --gpu "$@" \
		) > "$rfout" 2>&1 || true
	}
	rfrun "$rf/none.txt" "$rf/record.log" --record "$rf/movie.txt"
	rfrun "$rf/movie.txt" "$rf/plain.log" --render-every-frame \
		--save-state "119=$rf/plain-119.state" --save-state "239=$rf/plain-239.state"
	rfrun "$rf/movie.txt" "$rf/loads.log" --render-every-frame --rerecord \
		--save-state "119=$rf/loads-119.state" --save-state "239=$rf/loads-239.state"
	rebuilds=$(grep -c "rebuilt the renderer" "$rf/loads.log")
	size() { stat -c %s "$1" 2>/dev/null || echo 0; }
	if grep -q "^chimera gl: no context" "$rf/plain.log"; then
		report "engine:rebuild-frees" SKIP "this machine gives the bridge no GL context"
	elif [ ! -s "$rf/plain-239.state" ] || [ ! -s "$rf/loads-239.state" ]; then
		report "engine:rebuild-frees" FAIL "no state written (see $rf)"
	elif [ "$rebuilds" -lt 200 ]; then
		report "engine:rebuild-frees" FAIL "a load before every frame rebuilt the renderer only $rebuilds times in 240 frames (see $rf/loads.log)"
	else
		plain=$(( ($(size "$rf/plain-239.state") - $(size "$rf/plain-119.state")) / 1024 ))
		loads=$(( ($(size "$rf/loads-239.state") - $(size "$rf/loads-119.state")) / 1024 ))
		# kept renderers grew it by about 100 KiB a rebuild
		if [ $((loads - plain)) -gt 512 ]; then
			report "engine:rebuild-frees" FAIL "over the last 120 frames the state grew $loads KiB with a load before every frame and $plain KiB without: old renderers are kept"
		else
			report "engine:rebuild-frees" PASS "over the last 120 frames the state grew $loads KiB with a load before every frame and $plain KiB without ($rebuilds rebuilds)"
		fi
	fi
fi

# --- a movie that needs more memory than the core used to have ---
#
# Corporate Climber stopped at frame 55836 (2026-10-23) on plain playback,
# with no loads at all: its heap needs 315 MiB, the sandbox gave the core 256,
# and the rest spilled into the 512 MiB area where large blocks go and broke
# it into pieces too small for the next 12.8 MB picture. The core now has
# 1 GiB of each (waterbox.config, memoryLayoutMiB). This movie keeps 20
# pictures of 12.8 MB on the stage, about 490 MiB of large blocks with the
# in-sandbox renderer; the old 512 MiB area stops before the 15th.
if [ ! -x "$crun" ]; then
	report "engine:big-movie" SKIP "chimera-run not built"
elif [ ! -f "$package" ]; then
	report "engine:big-movie" SKIP "no package"
else
	bm="$work/big-movie"
	mkdir -p "$bm"
	python3 "$here/make-bitmaps-swf.py" "$bm/bitmaps.swf" 20 30
	printf '[Input]\nLogKey:#\n' > "$bm/none.txt"
	( cd "$chimera_root" && CRUN_TIMEOUT=900 crun_game "$package" "$bm/bitmaps.swf" "$bm/none.txt" \
		--settings '{"renderer":"software"}' --record "$bm/movie.txt" --frames 30 \
	) > "$bm/run.log" 2>&1
	rc=$?
	if [ $rc -eq 0 ] && grep -q "^frames=30" "$bm/run.log"; then
		report "engine:big-movie" PASS "20 pictures of 12.8 MB kept on the stage, all 30 frames played"
	else
		report "engine:big-movie" FAIL "exit $rc: $(grep -a -m1 "the core stopped\|memory allocation" "$bm/run.log" | cut -c1-160) (see $bm/run.log)"
	fi
fi

echo
echo "$ok ok, $failed failed"
[ "$failed" -eq 0 ]
