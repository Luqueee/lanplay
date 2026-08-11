#!/usr/bin/env bash
# W2: what bitrate this link actually holds at 1080p120.
#
# One run per bitrate would measure the minute, not the bitrate - Wi-Fi
# varies far more between minutes than between 5 Mbps steps. So every
# bitrate is run several times and the order is shuffled, which keeps a slow
# drift in the radio environment from being read as a slope in the sweep.
#
# Nothing else moves: same IDD-LAB source, same DDA, same NV12 conversion,
# same P1 low-latency encoder, same RTP burst, no FEC, no retransmission, no
# adaptive bitrate. The bitrate is the only variable.
#
# usage:
#   tools/bitrate-sweep.sh [seconds]
#
#   BITRATES="50 45 40 35 30 25 20"
#   REPEATS=3
#   IFACE=en0        the link under test
#   OUT=dir          where the reports land

set -euo pipefail

SECONDS_TO_RUN="${1:-60}"
BITRATES="${BITRATES:-50 45 40 35 30 25 20}"
REPEATS="${REPEATS:-3}"
IFACE="${IFACE:-en0}"
REPO="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${OUT:-/tmp/bitrate-sweep-$(date +%Y%m%d-%H%M%S)}"

mkdir -p "$OUT"
echo "sweep     $BITRATES Mbps x $REPEATS runs of ${SECONDS_TO_RUN}s on $IFACE"
echo "output    $OUT"

# The shuffled schedule is written down before the first run, so a sweep that
# is interrupted can still say what it had and had not reached.
: >"$OUT/schedule"
for rep in $(seq 1 "$REPEATS"); do
    for bitrate in $BITRATES; do
        echo "$bitrate $rep"
    done
done | sort -R >>"$OUT/schedule"

total="$(wc -l <"$OUT/schedule" | tr -d ' ')"
index=0
while read -r bitrate rep; do
    index=$((index + 1))
    printf '\n[%d/%d] %s Mbps run %s\n' "$index" "$total" "$bitrate" "$rep"
    # `</dev/null`: the gate runs ssh, ssh reads stdin, and stdin here is the
    # schedule. Without it the first run swallows the rest of the schedule and
    # the sweep quietly becomes one sample.
    IFACE="$IFACE" BITRATE="$bitrate" QUIET=1 \
        REPORT="$OUT/${bitrate}m-r${rep}.json" \
        "$REPO/tools/e2e-gate.sh" "$SECONDS_TO_RUN" \
        >"$OUT/${bitrate}m-r${rep}.host.log" 2>&1 </dev/null ||
        echo "      run returned $? (a failing gate is a data point, not an abort)"
done <"$OUT/schedule"

echo
python3 "$REPO/tools/sweep-report.py" "$OUT"
