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
# Every run is `--link-only`: this experiment asks what the radio delivered,
# and routing that question through a display link is what made the previous
# attempt unreadable.
#
# The radio is sampled alongside every run, so a collapsed window can be
# checked against RSSI and PHY rate instead of guessed at. Read from
# CoreWLAN, never system_profiler: that tool scans, and scanning is what a
# link measurement must not do to itself.
#
# Results from before that was understood are not comparable with these: the
# old sampler drove p99 access unit arrival from 11 ms to 133 ms on its own.
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
    "$REPO/target/release/radio-sample" $((SECONDS_TO_RUN + 30)) 1000 \
        >"$OUT/$size-r$rep.wifi.csv" 2>/dev/null &
    sampler=$!
    IFACE="$IFACE" BITRATE="$BITRATE" MTU="$size" LINK_ONLY=1 QUIET=1 \
        REPORT="$OUT/$size-r$rep.json" \
        "$REPO/tools/e2e-gate.sh" "$SECONDS_TO_RUN" \
        >"$OUT/$size-r$rep.log" 2>&1 </dev/null ||
        echo "      run returned $? (a failing gate is a data point, not an abort)"
    kill "$sampler" 2>/dev/null || true
    grep -oE "= [0-9.]+ frames/s" "$OUT/$size-r$rep.log" | tail -1 | sed 's/^/      host /' || true
    sleep 5
done <"$OUT/schedule"

echo
python3 "$REPO/tools/link-report.py" "$OUT"
