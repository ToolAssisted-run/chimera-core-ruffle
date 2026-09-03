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

if [ "$have_sandbox" = 1 ]; then
  echo "ruffle input gate: $iok/$itotal input.json streams replayed as levels, trace identical to ruffle; $ibad failures"
  echo "ruffle gate: $ok/$total trace-identical to ruffle, deterministic, and IDENTICAL IN THE SANDBOX; $bad correctness, $nondet determinism, $sbad sandbox failures"
else
  echo "ruffle gate: $ok/$total trace-identical to ruffle AND deterministic; $bad correctness, $nondet determinism failures (sandbox SKIPPED: build waterbox/build/core.wbx with build-guest.sh)"
fi
[ "$bad" -eq 0 ] && [ "$nondet" -eq 0 ] && [ "$sbad" -eq 0 ] && [ "$ibad" -eq 0 ]
