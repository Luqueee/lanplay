#!/usr/bin/env bash
# W3-A: does asking for a better service class change when datagrams arrive?
#
# W2 settled that cadence does not follow bitrate: between 20 and 50 Mbps the
# arrival p99 sits at 32-36 ms and the correlation is +0.138. What has not
# been tested is the other axis - not how much we send, but when the radio is
# willing to send it. Four arms, one variable:
#
#   best-effort   ask for nothing; the baseline
#   dscp          CS4 through IP_TOS, which Windows may well strip
#   audio-video   qWAVE's A/V class: DSCP 40, 802.11 user priority 5
#   control       qWAVE's highest class, as a diagnostic ceiling only - it is
#                 documented for critical control traffic, not for video, and
#                 would not be shipped for this even if it won
#
# Every run reports the DSCP the Mac actually saw, because an experiment that
# records only what the sender asked for measures intent, not the network.
#
# usage:
#   tools/qos-sweep.sh [seconds]
#
#   ARMS="best-effort dscp audio-video control"
#   REPEATS=3   IFACE=en0   BITRATE=40   OUT=dir

set -euo pipefail

SECONDS_TO_RUN="${1:-60}"
ARMS="${ARMS:-best-effort dscp audio-video control}"
REPEATS="${REPEATS:-3}"
IFACE="${IFACE:-en0}"
BITRATE="${BITRATE:-40}"
REPO="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${OUT:-/tmp/qos-sweep-$(date +%Y%m%d-%H%M%S)}"

mkdir -p "$OUT"
echo "qos sweep  [$ARMS] x $REPEATS runs of ${SECONDS_TO_RUN}s at ${BITRATE} Mbps on $IFACE"
echo "output     $OUT"

# Shuffled, not blocked: the radio drifts over the twenty minutes this takes,
# and running each arm's three runs back to back would let that drift land on
# one arm and look like an effect.
: >"$OUT/schedule"
for rep in $(seq 1 "$REPEATS"); do
    for arm in $ARMS; do
        echo "$arm $rep"
    done
done | sort -R >>"$OUT/schedule"

total="$(wc -l <"$OUT/schedule" | tr -d ' ')"
index=0
while read -r arm rep; do
    index=$((index + 1))
    printf '\n[%d/%d] %s run %s\n' "$index" "$total" "$arm" "$rep"
    IFACE="$IFACE" BITRATE="$BITRATE" SERVICE_CLASS="$arm" QUIET=1 \
        REPORT="$OUT/$arm-r$rep.json" \
        "$REPO/tools/e2e-gate.sh" "$SECONDS_TO_RUN" \
        >"$OUT/$arm-r$rep.log" 2>&1 </dev/null ||
        echo "      run returned $? (a failing gate is a data point, not an abort)"
    grep -E "^service class" "$OUT/$arm-r$rep.log" | sed 's/^/      /' || true
    sleep 5
done <"$OUT/schedule"

echo
python3 "$REPO/tools/qos-report.py" "$OUT"
