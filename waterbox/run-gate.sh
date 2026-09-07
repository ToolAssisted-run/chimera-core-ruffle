#!/bin/bash
# The native gate for the chimera Ruffle core, M0.
#
# Two properties, both on ruffle's own MIT test corpus (no copyrighted SWFs):
#   correctness  - the native reference reproduces ruffle's committed
#                  output.txt (the SWF's own trace() output) for a frozen list
#                  of trace-only tests (tests/oracle-list.txt), byte for byte.
#   determinism  - each of those SWFs, run three times, gives an identical
#                  trace digest. This is the property the whole core exists to
#                  provide.
#   sandbox      - the SAME SWF through the waterboxed core produces the same
#                  trace as ruffle expects AND the same digest as native. This
#                  is the milestone: Flash running inside the sandbox, where
#                  the determinism is enforced rather than hoped for.
#   image        - (M4) the PICTURE. Each movie in tests/image-list.txt is
#                  rendered through the core's GL renderer and compared with
#                  ruffle's own output.expected.png, and rendered twice to show
#                  the frame does not wander. This is the leg that proves the
#                  core draws Flash rather than merely running it.
#   state        - (M6) savestates. Each movie in tests/state-list.txt is run
#                  twice: normally, and with the whole machine saved and
#                  reloaded before EVERY frame. Both runs must agree on the
#                  trace, the audio and the picture - which is the question a
#                  rewind asks, asked harder than a rewind asks it.
#   input        - (M2) tests that ship an input.json: the stream is converted
#                  to per-frame levels (tests/input2moves.py), replayed through
#                  SetAxis/SetButton, and the trace must again equal ruffle's
#                  output.txt. tests/input-list.txt holds the ones the level
#                  model can express (no press-and-release inside one frame).
#   sub-frame    - the same, for the ones it CANNOT express at 1x: with the fps
#                  setting the machine steps several times per movie frame, and
#                  a click that begins and ends inside one of the movie's
#                  frames becomes an ordinary level stream. Same bar - ruffle's
#                  own output.txt, byte for byte - and the same movie must run
#                  the same movie frames it always did.
#
# The oracle list holds only tests the null-backend player can fully serve
# (trace + frame stepping); tests needing input, a navigator or fonts arrive
# with M2/M3/M5 and are not asserted here.
#
# Usage: ./run-gate.sh [--ruffle <ruffle checkout>] [--bin <run-native>]
set -u
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/.." && pwd)"
ruffle="${RUFFLE_SRC:-$root/extern/ruffle}"
bin="$root/waterbox/run-native/target/release/run-native"
wbx="$root/waterbox/build/run-wbx"
core="$root/waterbox/build/core.wbx"
while [ $# -gt 0 ]; do
  case "$1" in
    --ruffle) ruffle="$2"; shift 2 ;;
    --bin) bin="$2"; shift 2 ;;
    *) echo "unknown: $1" >&2; exit 2 ;;
  esac
done
swfs="$ruffle/tests/tests/swfs"
list="$root/tests/oracle-list.txt"
[ -x "$bin" ] || { echo "run-native not built: $bin (cargo build --release in waterbox/run-native)" >&2; exit 1; }
[ -d "$swfs" ] || { echo "ruffle corpus not found: $swfs (git submodule update --init extern/ruffle)" >&2; exit 1; }
[ -f "$list" ] || { echo "oracle list missing: $list" >&2; exit 1; }

ok=0; bad=0; nondet=0; total=0; sbad=0; sskip=0
have_sandbox=1
[ -x "$wbx" ] && [ -f "$core" ] || have_sandbox=0
while IFS='|' read -r rel nf; do
  [ -n "$rel" ] || continue
  total=$((total+1))
  sw="$swfs/$rel/test.swf"; exp="$swfs/$rel/output.txt"
  a=$(timeout 30 "$bin" "$sw" --frames "$nf" 2>/dev/null)
  if [ "$a" != "$(cat "$exp")" ]; then echo "FAIL correctness: $rel"; bad=$((bad+1)); continue; fi
  # determinism: the trace digest must be identical across three runs
  d1=$(timeout 30 "$bin" "$sw" --frames "$nf" 2>&1 >/dev/null | grep -oP 'traceSha1=\K\w+')
  d2=$(timeout 30 "$bin" "$sw" --frames "$nf" 2>&1 >/dev/null | grep -oP 'traceSha1=\K\w+')
  if [ "$d1" != "$d2" ]; then echo "FAIL determinism: $rel ($d1 vs $d2)"; nondet=$((nondet+1)); continue; fi
  if [ "$have_sandbox" = 1 ]; then
    sout=$(timeout 60 "$wbx" "$core" "$sw" --frames "$nf" 2>/dev/null)
    # against ruffle's own expectation AND against what native produced: the
    # second is the one that says the sandbox changed nothing.
    if [ "$sout" != "$(cat "$exp")" ]; then echo "FAIL sandbox trace: $rel"; sbad=$((sbad+1)); continue; fi
    if [ "$sout" != "$a" ]; then echo "FAIL sandbox != native: $rel"; sbad=$((sbad+1)); continue; fi
    # and the AUDIO the two produced, byte for byte (M3): a digest each side prints
    nad=$(timeout 60 "$bin" "$sw" --frames "$nf" 2>&1 >/dev/null | grep -oP 'audioDigest=\K\w+')
    sad=$(timeout 60 "$wbx" "$core" "$sw" --frames "$nf" 2>&1 >/dev/null | grep -oP 'audioDigest=\K\w+')
    if [ -n "$nad" ] && [ "$nad" != "$sad" ]; then echo "FAIL sandbox audio != native: $rel ($sad vs $nad)"; sbad=$((sbad+1)); continue; fi
  else
    sskip=$((sskip+1))
  fi
  ok=$((ok+1))
done < "$list"

echo "-----"
# ---- input (M2): replay each test's input.json as per-frame levels ----------
iok=0; ibad=0; itotal=0
ilist="$root/tests/input-list.txt"
if [ "$have_sandbox" = 1 ] && [ -f "$ilist" ]; then
  while IFS='|' read -r rel nf; do
    [ -n "$rel" ] || continue
    case "$rel" in \#*) continue ;; esac   # a comment: why something is NOT on the list
    itotal=$((itotal+1))
    sw="$swfs/$rel/test.swf"; exp="$swfs/$rel/output.txt"
    mv="$(mktemp)"
    if ! python3 "$root/tests/input2moves.py" "$swfs/$rel/input.json" > "$mv" 2>/dev/null; then
      echo "FAIL input convert: $rel"; ibad=$((ibad+1)); rm -f "$mv"; continue
    fi
    got=$(timeout 60 "$wbx" "$core" "$sw" --frames "$nf" --input "$mv" 2>/dev/null)
    rm -f "$mv"
    if [ "$got" != "$(cat "$exp")" ]; then echo "FAIL input: $rel"; ibad=$((ibad+1)); continue; fi
    iok=$((iok+1))
  done < "$ilist"
fi

# ---- sub-frame input: what a raised frame rate buys ------------------------
# The fps setting steps the machine faster than the movie without running the
# movie faster: the extra frames carry input, timers and a picture. Two claims,
# both checked here - the movie is unchanged (same frames, same trace), and
# input that could not be expressed at 1x now is.
fok=0; fbad=0; ftotal=0
flist="$root/tests/subframe-input-list.txt"
if [ "$have_sandbox" = 1 ] && [ -f "$flist" ]; then
  ftmp=$(mktemp -d)
  # first: a raised rate must not touch the movie. The same movie, run for the
  # same MOVIE time at twice the rate, must produce the same trace and report
  # the doubled rate to the frontend.
  usw="$swfs/avm1/conflicting_instance_names/test.swf"
  if [ -f "$usw" ]; then
    ftotal=$((ftotal+1))
    base=$(timeout 60 "$wbx" "$core" "$usw" --frames 6 2>&1 >/dev/null | grep -oP 'traceDigest=\K\w+')
    printf '{"fps":48}' > "$ftmp/x2.json"
    out=$(timeout 60 "$wbx" "$core" "$usw" --frames 12 --file "settings=$ftmp/x2.json" 2>&1 >/dev/null)
    got=$(printf '%s' "$out" | grep -oP 'traceDigest=\K\w+')
    vs=$(printf '%s' "$out" | grep -oP 'vsync=\K[0-9]+/[0-9]+')
    if [ "$got" = "$base" ] && [ "$vs" = "48/1" ]; then
      fok=$((fok+1))
    else
      fbad=$((fbad+1)); echo "  subframe unchanged-movie: 12 frames at 48fps gave $got/$vs, 6 at 24 gave $base/24-1"
    fi
  fi
  while IFS='|' read -r rel nf sub; do
    [ -n "$rel" ] || continue
    case "$rel" in \#*) continue ;; esac
    ftotal=$((ftotal+1))
    sw="$swfs/$rel/test.swf"; exp="$swfs/$rel/output.txt"
    # the point of the list: these are NOT expressible at the movie's own rate
    if python3 "$root/tests/input2moves.py" "$swfs/$rel/input.json" >/dev/null 2>&1; then
      echo "FAIL subframe: $rel is expressible at 1x - it belongs in input-list.txt"; fbad=$((fbad+1)); continue
    fi
    if ! python3 "$root/tests/input2moves.py" "$swfs/$rel/input.json" --split "$sub" > "$ftmp/moves" 2>/dev/null; then
      echo "FAIL subframe convert: $rel at $sub sub-frames"; fbad=$((fbad+1)); continue
    fi
    vs=$(timeout 60 "$wbx" "$core" "$sw" --frames 1 2>&1 >/dev/null | grep -oP 'vsync=\K[0-9]+/[0-9]+')
    case "$vs" in */1) ;; *) echo "FAIL subframe: $rel runs at $vs, not a whole rate"; fbad=$((fbad+1)); continue ;; esac
    printf '{"fps":%d}' $(( ${vs%/*} * sub )) > "$ftmp/s.json"
    got=$(timeout 120 "$wbx" "$core" "$sw" --frames $((nf * sub)) --input "$ftmp/moves" --file "settings=$ftmp/s.json" 2>/dev/null)
    if [ "$got" != "$(cat "$exp")" ]; then echo "FAIL subframe: $rel"; fbad=$((fbad+1)); continue; fi
    fok=$((fok+1))
  done < "$flist"
  rm -rf "$ftmp"
fi

# ---- navigator (M5): associated files served from the guest VFS -------------
nok=0; nbad=0; ntotal=0
nlist="$root/tests/navigator-list.txt"
if [ "$have_sandbox" = 1 ] && [ -f "$nlist" ]; then
  while IFS='|' read -r rel nf; do
    [ -n "$rel" ] || continue
    case "$rel" in \#*) continue ;; esac
    ntotal=$((ntotal+1))
    d="$swfs/$rel"
    # mount every serveable sibling by basename, the way the harness serves the
    # test's own directory
    fargs=""
    for f in $(cd "$d" && ls | grep -vE '^(test\.swf|output\.txt|test\.toml|input\.json|source\.as|Test\.as|.*\.fla|.*\.flad|regenerate.*\.sh|.*\.md|.*\.rs)$'); do
      case "$f" in *.swf|*.mp3|*.bin|*.txt|*.xml|*.flv|*.csv|*.dat|*.gif|*.jpg|*.jpeg|*.png) fargs="$fargs --file $f=$d/$f" ;; esac
    done
    iarg=""; mv=""
    if [ -f "$d/input.json" ]; then mv="$(mktemp)"; python3 "$root/tests/input2moves.py" "$d/input.json" > "$mv" 2>/dev/null && iarg="--input $mv"; fi
    a=$(timeout 60 "$wbx" "$core" "$d/test.swf" --frames "$nf" $iarg $fargs 2>/dev/null)
    b=$(timeout 60 "$wbx" "$core" "$d/test.swf" --frames "$nf" $iarg $fargs 2>/dev/null)
    [ -n "$mv" ] && rm -f "$mv"
    if [ "$a" != "$(cat "$d/output.txt")" ]; then echo "FAIL navigator: $rel"; nbad=$((nbad+1))
    elif [ "$a" != "$b" ]; then echo "FAIL navigator determinism: $rel"; nbad=$((nbad+1))
    else nok=$((nok+1)); fi
  done < "$nlist"
fi

# ---- audio (M3): the corpus's amplitude assertions, plus native == sandbox ----
aok=0; abad=0; atotal=0
alist="$root/tests/audio-list.txt"
if [ "$have_sandbox" = 1 ] && [ -f "$alist" ]; then
  while IFS='|' read -r rel nf; do
    [ -n "$rel" ] || continue
    case "$rel" in \#*) continue ;; esac
    atotal=$((atotal+1))
    sw="$swfs/$rel/test.swf"; exp="$swfs/$rel/output.txt"; tml="$swfs/$rel/test.toml"
    t="$(mktemp -d)"
    nout=$(timeout 120 "$bin" "$sw" --frames "$nf" --audio-out "$t/n.raw" --audio-peaks "$t/n.pk" 2>/dev/null)
    sout=$(timeout 120 "$wbx" "$core" "$sw" --frames "$nf" --audio-out "$t/s.raw" --audio-peaks "$t/s.pk" 2>/dev/null)
    timeout 120 "$wbx" "$core" "$sw" --frames "$nf" --audio-out "$t/s2.raw" >/dev/null 2>&1
    if [ "$nout" != "$(cat "$exp")" ] || [ "$sout" != "$(cat "$exp")" ]; then echo "FAIL audio trace: $rel"; abad=$((abad+1))
    elif ! cmp -s "$t/n.raw" "$t/s.raw"; then echo "FAIL audio sandbox != native: $rel"; abad=$((abad+1))
    elif ! cmp -s "$t/s.raw" "$t/s2.raw"; then echo "FAIL audio determinism: $rel"; abad=$((abad+1))
    elif ! msg=$(python3 "$root/tests/audio-assert.py" "$tml" "$t/s.pk"); then echo "FAIL audio assertion: $rel: $msg"; abad=$((abad+1))
    else aok=$((aok+1)); fi
    rm -rf "$t"
  done < "$alist"
fi

# ---- state: does the machine survive being saved and reloaded? ----
sok=0; sbadstate=0; stotal=0
slist="$root/tests/state-list.txt"
if [ "$have_sandbox" = 1 ] && [ -f "$slist" ]; then
  while IFS='|' read -r rel nf; do
    case "$rel" in ''|\#*) continue ;; esac
    stotal=$((stotal+1))
    d="$swfs/$rel"
    fargs=""
    for f in $(cd "$d" && ls | grep -vE '^(test\.swf|output.*|test\.toml|input\.json|source\.as|Test\.as|.*\.fla|.*\.flad|regenerate.*\.sh|.*\.md|.*\.rs)$'); do
      case "$f" in *.swf|*.mp3|*.bin|*.txt|*.xml|*.flv|*.csv|*.dat|*.gif|*.jpg|*.jpeg|*.png) fargs="$fargs --file $f=$d/$f" ;; esac
    done
    iarg=""; mv=""
    if [ -f "$d/input.json" ]; then mv=$(mktemp); python3 "$root/tests/input2moves.py" "$d/input.json" > "$mv" 2>/dev/null && iarg="--input $mv"; fi
    plain=$(timeout 120 "$wbx" "$core" "$d/test.swf" --frames "$nf" --quiet $iarg $fargs 2>&1 | grep -oE '(traceDigest|audioDigest|videoDigest)=[0-9a-f]+' | tr '\n' ' ')
    rer=$(timeout 300 "$wbx" "$core" "$d/test.swf" --frames "$nf" --quiet $iarg $fargs --rerecord 2>&1 | grep -oE '(traceDigest|audioDigest|videoDigest)=[0-9a-f]+' | tr '\n' ' ')
    [ -n "$mv" ] && rm -f "$mv"
    if [ -z "$plain" ]; then sbadstate=$((sbadstate+1)); echo "  state NO RUN $rel"; continue; fi
    if [ "$plain" = "$rer" ]; then sok=$((sok+1)); else
      sbadstate=$((sbadstate+1)); echo "  state DIVERGED $rel"; echo "    plain:    $plain"; echo "    rerecord: $rer"
    fi
  done < "$slist"
fi

# ---- image: does the core draw what ruffle draws? ----
gok=0; gbad=0; gtotal=0
glist="$root/tests/image-list.txt"
if [ "$have_sandbox" = 1 ] && [ -f "$glist" ]; then
  gtmp=$(mktemp -d)
  while IFS='|' read -r rel nf; do
    case "$rel" in ''|\#*) continue ;; esac
    gtotal=$((gtotal+1))
    d="$swfs/$rel"
    exp="$d/output.expected.png"
    [ -f "$exp" ] || { gbad=$((gbad+1)); echo "  image MISSING EXPECTED $rel"; continue; }
    fargs=""
    for f in $(cd "$d" && ls | grep -vE '^(test\.swf|output.*|test\.toml|input\.json|source\.as|Test\.as|.*\.fla|.*\.flad|regenerate.*\.sh|.*\.md|.*\.rs)$'); do
      case "$f" in *.swf|*.mp3|*.bin|*.txt|*.xml|*.flv|*.csv|*.dat|*.gif|*.jpg|*.jpeg|*.png) fargs="$fargs --file $f=$d/$f" ;; esac
    done
    iarg=""; mv=""
    if [ -f "$d/input.json" ]; then mv=$(mktemp); python3 "$root/tests/input2moves.py" "$d/input.json" > "$mv" 2>/dev/null && iarg="--input $mv"; fi
    p1="$gtmp/a.ppm"; p2="$gtmp/b.ppm"
    d1=$(timeout 120 "$wbx" "$core" "$d/test.swf" --frames "$nf" --quiet $iarg $fargs --video-out "$p1" 2>&1 | grep -oE 'videoDigest=[0-9a-f]+')
    d2=$(timeout 120 "$wbx" "$core" "$d/test.swf" --frames "$nf" --quiet $iarg $fargs --video-out "$p2" 2>&1 | grep -oE 'videoDigest=[0-9a-f]+')
    [ -n "$mv" ] && rm -f "$mv"
    if [ ! -s "$p1" ]; then gbad=$((gbad+1)); echo "  image NO FRAME $rel"; continue; fi
    if [ "$d1" != "$d2" ]; then gbad=$((gbad+1)); echo "  image NONDETERMINISTIC $rel ($d1 vs $d2)"; continue; fi
    # the stated budget: 8 per channel, at most 1% of pixels (see image-list.txt)
    px=$(head -2 "$p1" | tail -1 | awk '{print $1*$2}')
    if python3 "$root/tests/image-compare.py" "$exp" "$p1" 8 $((px/100)) >/dev/null 2>&1; then
      gok=$((gok+1))
    else
      gbad=$((gbad+1))
      echo "  image MISMATCH $rel: $(python3 "$root/tests/image-compare.py" "$exp" "$p1" 8 $((px/100)) 2>&1)"
    fi
  done < "$glist"
  rm -rf "$gtmp"
fi

# ---- spoofed URL: the setting, and the two ways it arrives ----
# A movie's own address is not cosmetic in Flash - a sponsor lock reads it, and
# relative loads resolve against it - so the setting has to actually reach the
# player. loaderinfo_loadurl traces its loaderURL, which is precisely the value
# spoofing changes, so the trace answers the question directly.
pok=0; pbad=0; ptotal=0
if [ "$have_sandbox" = 1 ]; then
  psw="$swfs/avm2/loaderinfo_loadurl/test.swf"
  if [ ! -f "$psw" ]; then
    echo "  spoof SKIPPED (no loaderinfo_loadurl in the corpus)"
  else
    ptmp=$(mktemp -d); spoofed="https://games.example.invalid/arcade/game.swf"
    printf '{"spoofUrl":"%s"}' "$spoofed" > "$ptmp/settings.json"
    printf '{"spoofUrl":"   "}' > "$ptmp/blank.json"

    check() { # name expected-substring unexpected-substring output
      ptotal=$((ptotal+1))
      if printf '%s' "$4" | grep -qF "$2" && { [ -z "$3" ] || ! printf '%s' "$4" | grep -qF "$3"; }; then
        pok=$((pok+1))
      else
        pbad=$((pbad+1)); echo "  spoof $1: expected '$2' in the trace, got: $(printf '%s' "$4" | grep -i loaderURL | head -1)"
      fi
    }

    out=$(timeout 60 "$wbx" "$core" "$psw" --frames 1 2>/dev/null)
    check "unset" "file:///game.swf" "" "$out"

    out=$(timeout 60 "$wbx" "$core" "$psw" --frames 1 --spoof-url "$spoofed" 2>/dev/null)
    check "direct" "$spoofed" "file:///game.swf" "$out"

    # the route the frontend uses: the engine mounts the effective settings
    out=$(timeout 60 "$wbx" "$core" "$psw" --frames 1 --file "settings=$ptmp/settings.json" 2>/dev/null)
    check "setting" "$spoofed" "file:///game.swf" "$out"

    # whitespace is not a URL, and an empty setting must not spoof anything
    out=$(timeout 60 "$wbx" "$core" "$psw" --frames 1 --file "settings=$ptmp/blank.json" 2>/dev/null)
    check "blank-setting" "file:///game.swf" "" "$out"

    # both given: the direct call is the gate runner's own channel and wins
    out=$(timeout 60 "$wbx" "$core" "$psw" --frames 1 --spoof-url "$spoofed" --file "settings=$ptmp/blank.json" 2>/dev/null)
    check "direct-wins" "$spoofed" "file:///game.swf" "$out"

    # ---- the settings that show up in the picture ----
    # A digest, so "the setting reached the renderer" is answered by the frame
    # rather than by trusting the plumbing. (run-wbx reports it on stderr.)
    digest() { # swf [settings-json]
      if [ -n "${2:-}" ]; then printf '%s' "$2" > "$ptmp/s.json"
        timeout 60 "$wbx" "$core" "$1" --frames 2 --file "settings=$ptmp/s.json" 2>&1 >/dev/null | grep -oP 'videoDigest=\K\w+'
      else
        timeout 60 "$wbx" "$core" "$1" --frames 2 2>&1 >/dev/null | grep -oP 'videoDigest=\K\w+'
      fi
    }
    same() { # name swf settings expected-digest same|differ
      ptotal=$((ptotal+1)); got=$(digest "$2" "$3")
      if [ "$5" = same ] && [ "$got" = "$4" ]; then pok=$((pok+1))
      elif [ "$5" = differ ] && [ -n "$got" ] && [ "$got" != "$4" ]; then pok=$((pok+1))
      else pbad=$((pbad+1)); echo "  settings $1: wanted the frame to $5 from $4, got $got"; fi
    }

    qsw="$swfs/text/br_at_start/test.swf"
    if [ -f "$qsw" ]; then
      base=$(digest "$qsw")
      same "quality:default"  "$qsw" '{"quality":"high"}'     "$base" same
      same "quality:low"      "$qsw" '{"quality":"low"}'      "$base" differ
      # an unknown name must fall back to the default, not to something else
      same "quality:unknown"  "$qsw" '{"quality":"nonsense"}' "$base" same
      # every other setting, at the default waterbox.config declares, must be
      # invisible: this is what catches a key read under the wrong name or a
      # guest fallback that disagrees with the declared default
      same "inert:pageUrl"    "$qsw" '{"pageUrl":""}'                 "$base" same
      same "inert:version"    "$qsw" '{"playerVersion":0}'            "$base" same
      same "inert:runtime"    "$qsw" '{"playerRuntime":"flashPlayer"}' "$base" same
      same "inert:load"       "$qsw" '{"loadBehavior":"streaming"}'   "$base" same
      same "inert:compat"     "$qsw" '{"compatibilityRules":false}'   "$base" same
      same "inert:font"       "$qsw" '{"defaultFont":true}'           "$base" same
      same "inert:fps"        "$qsw" '{"fps":0}'                      "$base" same
    fi

    fsw="$swfs/fonts/device_font_list/test.swf"
    if [ -f "$fsw" ]; then
      fbase=$(digest "$fsw")
      same "font:substituted" "$fsw" '{"defaultFont":true}'  "$fbase" same
      same "font:missing"     "$fsw" '{"defaultFont":false}' "$fbase" differ
    fi

    rm -rf "$ptmp"
  fi
fi

if [ "$have_sandbox" = 1 ]; then
  echo "ruffle settings gate: $pok/$ptotal the settings channel reaches the player - the movie reports the address it was told to, quality and font substitution show up in the frame, and every other setting is invisible at its declared default; $pbad failures"
  echo "ruffle state gate: $sok/$stotal movies survive a save and reload before every frame with the same trace, audio and picture; $sbadstate failures"
  echo "ruffle image gate: $gok/$gtotal movies draw the frame ruffle draws (8/channel, 99% of pixels), reruns identical; $gbad failures"
  echo "ruffle navigator gate: $nok/$ntotal movies load their associated files (loadMovie/loadSound/loadVariables/URLLoader) with the trace ruffle expects, reruns identical; $nbad failures"
  echo "ruffle audio gate: $aok/$atotal sound movies: ruffle's amplitude assertions hold, native == sandbox byte for byte, reruns identical; $abad failures"
  echo "ruffle input gate: $iok/$itotal input.json streams replayed as levels, trace identical to ruffle; $ibad failures"
  echo "ruffle sub-frame gate: $fok/$ftotal a raised frame rate leaves the movie alone and makes a click inside one movie frame expressible, trace identical to ruffle; $fbad failures"
  echo "ruffle gate: $ok/$total trace-identical to ruffle, deterministic, and IDENTICAL IN THE SANDBOX; $bad correctness, $nondet determinism, $sbad sandbox failures"
else
  echo "ruffle gate: $ok/$total trace-identical to ruffle AND deterministic; $bad correctness, $nondet determinism failures (sandbox SKIPPED: build waterbox/build/core.wbx with build-guest.sh)"
fi
[ "$bad" -eq 0 ] && [ "$nondet" -eq 0 ] && [ "$sbad" -eq 0 ] && [ "$ibad" -eq 0 ] && [ "$fbad" -eq 0 ] && [ "$abad" -eq 0 ] && [ "$nbad" -eq 0 ] && [ "$gbad" -eq 0 ] && [ "$sbadstate" -eq 0 ] && [ "$pbad" -eq 0 ]
