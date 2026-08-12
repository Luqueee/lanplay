#!/usr/bin/env bash
# B0 and B5: a fresh baseline, and whether datagram size changes the bunching.
#
# The 1200-byte arm is the baseline - same position, same channel, same access
# point - and it is measured interleaved with the others rather than before
# them, so it shares whatever the radio was doing that quarter of an hour.
# Cross-session baselines have already proved untrustworthy here: the access
# point changed channel between sittings on its own.
#
# The hypothesis for the larger sizes is not throughput. At 40 Mbps an access
# unit is about 45 datagrams; fewer, larger ones may aggregate differently.
# It may equally do nothing, which is a cheap answer to have.
#
# The radio is sampled alongside every run, so a collapsed window can be
# checked against RSSI and PHY rate instead of guessed at.
#
# usage:
#   tools/mtu-sweep.sh [seconds]
#
#   SIZES="1200 1350 1400"   REPEATS=3   IFACE=en0   BITRATE=40   OUT=dir

set -euo pipefail

SECONDS_TO_RUN="${1:-60}"
SIZES="${SIZES:-1200 1350 1400}"
REPEATS="${REPEATS:-3}"
IFACE="${IFACE:-en0}"
BITRATE="${BITRATE:-40}"
REPO="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${OUT:-/tmp/mtu-sweep-$(date +%Y%m%d-%H%M%S)}"

mkdir -p "$OUT"
echo "mtu sweep  [$SIZES] bytes x $REPEATS runs of ${SECONDS_TO_RUN}s at ${BITRATE} Mbps on $IFACE"
echo "output     $OUT"

: >"$OUT/schedule"
for rep in $(seq 1 "$REPEATS"); do
    for size in $SIZES; do
        echo "$size $rep"
    done
done | sort -R >>"$OUT/schedule"

total="$(wc -l <"$OUT/schedule" | tr -d ' ')"
index=0
while read -r size rep; do
    index=$((index + 1))
    printf '\n[%d/%d] mtu %s run %s\n' "$index" "$total" "$size" "$rep"
    "$REPO/tools/wifi-sample.sh" "$OUT/$size-r$rep.wifi.csv" $((SECONDS_TO_RUN + 30)) \
        >/dev/null 2>&1 &
    sampler=$!
    IFACE="$IFACE" BITRATE="$BITRATE" MTU="$size" QUIET=1 \
        REPORT="$OUT/$size-r$rep.json" \
        "$REPO/tools/e2e-gate.sh" "$SECONDS_TO_RUN" \
        >"$OUT/$size-r$rep.log" 2>&1 </dev/null ||
        echo "      run returned $? (a failing gate is a data point, not an abort)"
    kill "$sampler" 2>/dev/null || true
    grep -oE "= [0-9.]+ frames/s" "$OUT/$size-r$rep.log" | tail -1 | sed 's/^/      host /' || true
    sleep 5
done <"$OUT/schedule"

echo
python3 "$REPO/tools/qos-report.py" "$OUT"
echo
echo "radio during the runs"
python3 - "$OUT" <<'PY'
import csv, glob, statistics, sys
for path in sorted(glob.glob(f"{sys.argv[1]}/*.wifi.csv")):
    rows = [r for r in csv.DictReader(open(path)) if r.get("rssi_dbm")]
    if not rows:
        continue
    rssi = [float(r["rssi_dbm"]) for r in rows]
    rate = [float(r["tx_rate_mbps"]) for r in rows if r["tx_rate_mbps"]]
    channels = {r["channel"] for r in rows}
    name = path.split("/")[-1].replace(".wifi.csv", "")
    print(f"  {name:<12} n={len(rows):>3} rssi {min(rssi):.0f}..{max(rssi):.0f} dBm  "
          f"tx {min(rate):.0f}..{max(rate):.0f} Mbps  channel {'/'.join(sorted(channels))}")
PY
