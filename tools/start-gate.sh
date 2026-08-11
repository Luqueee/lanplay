#!/usr/bin/env bash
# Gate C-A: the decoder failure lives in startup, so count startups.
#
# One long soak cannot answer this. The symptom was 120 rejected frames -
# exactly one IDR interval - appearing in some runs and not others, which a
# single run can only ever confirm or fail to confirm by luck. Twenty short
# sessions per arm turn it into a rate.
#
# Two arms, because the hypothesis is about where the parameter sets come
# from and a single arm cannot separate "the fix worked" from "the failure
# did not happen to occur":
#
#   host     the encoder's own sequence header, over the control plane
#   fixture  another encoder's, the way it was done before
#
# usage:
#   tools/start-gate.sh [starts] [seconds]
#
#   ARMS="host fixture"   which arms to run
#   IFACE=en0             the link
#   OUT=dir               where the reports land

set -euo pipefail

STARTS="${1:-20}"
SECONDS_TO_RUN="${2:-10}"
ARMS="${ARMS:-host fixture}"
IFACE="${IFACE:-en0}"
REPO="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${OUT:-/tmp/start-gate-$(date +%Y%m%d-%H%M%S)}"

mkdir -p "$OUT"
echo "start gate  $STARTS starts x ${SECONDS_TO_RUN}s per arm [$ARMS] on $IFACE"
echo "output      $OUT"

for arm in $ARMS; do
    for start in $(seq 1 "$STARTS"); do
        printf '\n[%s %d/%d] ' "$arm" "$start" "$STARTS"
        IFACE="$IFACE" BITRATE=40 PARAMETER_SETS="$arm" QUIET=1 \
            REPORT="$OUT/$arm-$start.json" \
            "$REPO/tools/e2e-gate.sh" "$SECONDS_TO_RUN" \
            >"$OUT/$arm-$start.log" 2>&1 </dev/null || true
        if [ -f "$OUT/$arm-$start.json" ]; then
            python3 -c "
import json,sys
r=json.load(open('$OUT/$arm-$start.json'))
d=r['decode']; s=r['stream']
print(f\"decoded {d['decoded']}/{s['reconstructed']} errors {d['errors']} ploss {s['packet_loss']}\", end='')
"
        else
            printf 'no report'
        fi
    done
done

echo
echo
python3 - "$OUT" <<'PY'
import json, glob, sys, collections
root = sys.argv[1]
arms = collections.defaultdict(list)
for path in sorted(glob.glob(f"{root}/*.json")):
    arm = path.split("/")[-1].rsplit("-", 1)[0]
    arms[arm].append(json.load(open(path)))

print(f"{'arm':<10} {'starts':>7} {'with errors':>12} {'total errors':>13} {'decoded':>9} {'submitted':>10}")
for arm, runs in arms.items():
    failed = sum(1 for r in runs if r["decode"]["errors"] > 0)
    errors = sum(r["decode"]["errors"] for r in runs)
    decoded = sum(r["decode"]["decoded"] for r in runs)
    submitted = sum(r["stream"]["reconstructed"] for r in runs)
    print(f"{arm:<10} {len(runs):>7} {failed:>12} {errors:>13} {decoded:>9} {submitted:>10}")
PY
