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
#   input        - (M2) tests that ship an input.json: the stream is converted
#                  to per-frame levels (tests/input2moves.py), replayed through
#                  SetAxis/SetButton, and the trace must again equal ruffle's
#                  output.txt. tests/input-list.txt holds the ones the level
#                  model can express (no press-and-release inside one frame).
#
# The oracle list holds only tests the null-backend player can fully serve
# (trace + frame stepping); tests needing input, a navigator or fonts arrive
# with M2/M3/M5 and are not asserted here.
#
# Usage: ./run-gate.sh [--ruffle <ruffle checkout>] [--bin <run-native>]
set -u
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/.." && pwd)"
ruffle="${RUFFLE_SRC:-$HOME/ruffle-src}"
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
[ -d "$swfs" ] || { echo "ruffle corpus not found: $swfs (set RUFFLE_SRC)" >&2; exit 1; }
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

if [ "$have_sandbox" = 1 ]; then
  echo "ruffle navigator gate: $nok/$ntotal movies load their associated files (loadMovie/loadSound/loadVariables/URLLoader) with the trace ruffle expects, reruns identical; $nbad failures"
  echo "ruffle audio gate: $aok/$atotal sound movies: ruffle's amplitude assertions hold, native == sandbox byte for byte, reruns identical; $abad failures"
  echo "ruffle input gate: $iok/$itotal input.json streams replayed as levels, trace identical to ruffle; $ibad failures"
  echo "ruffle gate: $ok/$total trace-identical to ruffle, deterministic, and IDENTICAL IN THE SANDBOX; $bad correctness, $nondet determinism, $sbad sandbox failures"
else
  echo "ruffle gate: $ok/$total trace-identical to ruffle AND deterministic; $bad correctness, $nondet determinism failures (sandbox SKIPPED: build waterbox/build/core.wbx with build-guest.sh)"
fi
[ "$bad" -eq 0 ] && [ "$nondet" -eq 0 ] && [ "$sbad" -eq 0 ] && [ "$ibad" -eq 0 ] && [ "$abad" -eq 0 ] && [ "$nbad" -eq 0 ]
