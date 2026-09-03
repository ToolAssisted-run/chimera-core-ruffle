#!/bin/bash
# The native gate for the chimera Ruffle core, M0.
#
# Two properties, both on ruffle's own MIT test corpus (no copyrighted SWFs):
#   correctness  - the native reference reproduces ruffle's committed
#                  output.txt (the SWF's own trace() output) for a frozen list
#                  of trace-only tests (tests/oracle-list.txt), byte for byte.
#   determinism  - each of those SWFs, run three times, gives an identical
#                  trace digest. This is the property the whole core exists to
#                  provide, and the sandbox will have to preserve it.
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

ok=0; bad=0; nondet=0; total=0
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
  ok=$((ok+1))
done < "$list"

echo "-----"
echo "ruffle M0 native gate: $ok/$total trace-identical to ruffle AND deterministic; $bad correctness, $nondet determinism failures"
[ "$bad" -eq 0 ] && [ "$nondet" -eq 0 ]
