#!/usr/bin/env bash
# Is the periodic stall the Mac leaving the access point's channel?
#
# The capture experiment put the stall at or above the BPF tap: it is not
# socket delivery and not this application. What it left open is everything
# from the air down to the driver, and one candidate on that list is the Mac
# itself - AWDL, Apple's peer-to-peer link, time-shares the radio with the
# infrastructure connection and is up whenever AirDrop, Handoff, Continuity
# or Sidecar might be wanted.
#
# The measured fingerprint is a 34 ms stall every 221 ms with a standard
# deviation of 3.4 ms. That is a clock. It is not a textbook AWDL
# availability window, which is nearer 16 ms every 64 ms, so this script
# exists to test the hypothesis rather than to confirm a foregone
# conclusion: a run with awdl0 down that keeps the same 221 ms cadence
# clears AWDL completely.
#
# This script never changes the interface. Bringing awdl0 down needs root,
# and asking for a password halfway through a sweep would stall it; more to
# the point, a measurement harness that quietly reconfigures the machine it
# is measuring is how the last three sweeps got contaminated. It records
# what the state was and refuses to mislabel a run.
#
# Run it once per state:
#
#   tools/awdl-test.sh awdl-up
#   sudo ifconfig awdl0 down
#   tools/awdl-test.sh awdl-down
#   sudo ifconfig awdl0 up
#   tools/awdl-test.sh awdl-restored
#
# The third arm matters for the same reason B1 needed a return: this link
# drifts on its own, and two arms cannot tell an intervention from an hour
# passing.
#
#   IFACE=en0  BITRATE=40  MTU=1200  PORT=5004  OUT=/tmp/awdl

set -euo pipefail

LABEL="${1:?usage: awdl-test.sh <label> [seconds] [repeats]}"
SECONDS_TO_RUN="${2:-120}"
REPEATS="${3:-2}"
IFACE="${IFACE:-en0}"
BITRATE="${BITRATE:-40}"
MTU="${MTU:-1200}"
PORT="${PORT:-5004}"
REPO="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${OUT:-/tmp/awdl}"
SNAPLEN=96

mkdir -p "$OUT"

awdl_state() {
    if ifconfig awdl0 2>/dev/null | head -1 | grep -q "UP"; then
        echo up
    else
        echo down
    fi
}

before="$(awdl_state)"
echo "awdl0     $before"
echo "arm       $LABEL, $REPEATS runs of ${SECONDS_TO_RUN}s"
echo "output    $OUT"

for rep in $(seq 1 "$REPEATS"); do
    name="$LABEL-r$rep"
    printf '\n[%d/%d] %s\n' "$rep" "$REPEATS" "$name"
    at_start="$(awdl_state)"

    "$REPO/target/release/radio-sample" $((SECONDS_TO_RUN + 30)) 1000 \
        >"$OUT/$name.wifi.csv" 2>/dev/null &
    sampler=$!
    tcpdump -i "$IFACE" -s "$SNAPLEN" -B 16384 -n -p \
        -w "$OUT/$name.pcap" "udp port $PORT" >/dev/null 2>"$OUT/$name.tcpdump.log" &
    capture=$!
    for _ in $(seq 1 50); do
        [ -s "$OUT/$name.pcap" ] && break
        sleep 0.1
    done

    IFACE="$IFACE" BITRATE="$BITRATE" MTU="$MTU" LINK_ONLY=1 QUIET=1 \
        REPORT="$OUT/$name.json" \
        "$REPO/tools/e2e-gate.sh" "$SECONDS_TO_RUN" \
        >"$OUT/$name.log" 2>&1 </dev/null ||
        echo "      run returned $? (a failing gate is a data point, not an abort)"

    kill "$capture" 2>/dev/null && wait "$capture" 2>/dev/null || true
    kill "$sampler" 2>/dev/null || true

    at_end="$(awdl_state)"
    echo "      awdl0 $at_start -> $at_end"
    if [ "$at_start" != "$at_end" ]; then
        # macOS brings awdl0 back up on its own the moment a service wants
        # it. A run that straddles that is not a measurement of either state.
        echo "      MIXED STATE: this run measures neither arm" |
            tee "$OUT/$name.mixed"
    fi
    "$REPO/target/release/pcap-analyse" "$OUT/$name.pcap" --port "$PORT" --fps 120 \
        --json "$OUT/$name.pcap.json" | grep -E "^au (delivery|late|bunching)" |
        sed 's/^/      /'
    "$REPO/tools/stall-period.py" "$OUT/$name.pcap" | sed 's/^/      /'
    sleep 5
done

echo
python3 "$REPO/tools/link-report.py" "$OUT"
