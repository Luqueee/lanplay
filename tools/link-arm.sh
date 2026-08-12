#!/usr/bin/env bash
# One labelled arm of a link experiment: N runs under one set of conditions.
#
# Every link experiment so far has the same shape - hold everything constant,
# change one thing, measure - and the thing being changed is usually physical:
# where the laptop is, which channel the access point uses, whether awdl0 is
# up. No script can make those changes, so each arm is a separate invocation
# and the caller does the intervening work.
#
# What this does guarantee is that a run is never mislabelled. The radio is
# read from the driver before and after every run, and a run that saw the
# channel, width or awdl0 state change under it is marked and excluded. Two
# datasets were thrown away today for want of exactly that check.
#
# Every run is link-only: no window, no display link, no AppKit. Nothing
# measured here depends on what the screen was doing.
#
# usage:
#   tools/link-arm.sh <label> [seconds] [repeats]
#
#   IFACE=en0  BITRATE=40  MTU=1200  PORT=5004  CAPTURE=1  OUT=/tmp/link

set -euo pipefail

LABEL="${1:?usage: link-arm.sh <label> [seconds] [repeats]}"
SECONDS_TO_RUN="${2:-120}"
REPEATS="${3:-3}"
IFACE="${IFACE:-en0}"
BITRATE="${BITRATE:-40}"
MTU="${MTU:-1200}"
PORT="${PORT:-5004}"
CAPTURE="${CAPTURE:-1}"
REPO="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${OUT:-/tmp/link}"

# Headers only: Ethernet 14, IPv4 20, UDP 8, RTP 12 and a 16-byte frame id
# extension is 70 bytes. Capturing payload would cost disk bandwidth during
# the very run whose timing is being measured.
SNAPLEN=96

mkdir -p "$OUT"

# Channel, width and awdl0 in one line. This is the run's identity: two runs
# that disagree on it are not two samples of the same thing.
radio_state() {
    local line awdl
    line="$("$REPO/target/release/radio-sample" 1 100 2>/dev/null | tail -1)"
    awdl=down
    ifconfig awdl0 2>/dev/null | head -1 | grep -q "UP" && awdl=up
    printf 'ch%s/%sMHz awdl-%s' \
        "$(printf '%s' "$line" | cut -d, -f6)" \
        "$(printf '%s' "$line" | cut -d, -f7)" \
        "$awdl"
}

# Wait for the association to settle rather than sleeping a fixed interval,
# which is either too short after a channel change or wasted after none.
settle() {
    local previous="" current="" stable=0 attempt
    for attempt in $(seq 1 30); do
        current="$(radio_state)"
        if [ -n "$current" ] && [ "$current" = "$previous" ]; then
            stable=$((stable + 1))
            if [ "$stable" -ge 2 ]; then
                echo "radio     settled on $current after ${attempt}s"
                return 0
            fi
        else
            stable=0
        fi
        previous="$current"
        sleep 1
    done
    echo "radio     did not settle in 30 s; last seen $current" >&2
}

echo "arm       $LABEL, $REPEATS runs of ${SECONDS_TO_RUN}s"
echo "settings  ${BITRATE} Mbps, MTU $MTU, best-effort, link-only on $IFACE"
echo "capture   $([ "$CAPTURE" = 1 ] && echo "yes, snaplen $SNAPLEN" || echo no)"
echo "output    $OUT"

if [ "$CAPTURE" = 1 ] && ! id -Gn | tr ' ' '\n' | grep -qx access_bpf; then
    echo "not in the access_bpf group: tcpdump would need a password" >&2
    exit 1
fi

settle

for rep in $(seq 1 "$REPEATS"); do
    name="$LABEL-r$rep"
    printf '\n[%d/%d] %s\n' "$rep" "$REPEATS" "$name"
    before="$(radio_state)"

    "$REPO/target/release/radio-sample" $((SECONDS_TO_RUN + 30)) 1000 \
        >"$OUT/$name.wifi.csv" 2>/dev/null &
    sampler=$!
    capture_pid=""
    if [ "$CAPTURE" = 1 ]; then
        # -B raises the kernel buffer so a hiccup in tcpdump cannot drop
        # packets and be mistaken for a link event.
        tcpdump -i "$IFACE" -s "$SNAPLEN" -B 16384 -n -p \
            -w "$OUT/$name.pcap" "udp port $PORT" \
            >/dev/null 2>"$OUT/$name.tcpdump.log" &
        capture_pid=$!
        for _ in $(seq 1 50); do
            [ -s "$OUT/$name.pcap" ] && break
            sleep 0.1
        done
    fi

    IFACE="$IFACE" BITRATE="$BITRATE" MTU="$MTU" LINK_ONLY=1 QUIET=1 \
        REPORT="$OUT/$name.json" \
        "$REPO/tools/e2e-gate.sh" "$SECONDS_TO_RUN" \
        >"$OUT/$name.log" 2>&1 </dev/null ||
        echo "      run returned $? (a failing gate is a data point, not an abort)"

    if [ -n "$capture_pid" ]; then
        kill "$capture_pid" 2>/dev/null && wait "$capture_pid" 2>/dev/null || true
    fi
    kill "$sampler" 2>/dev/null || true

    after="$(radio_state)"
    if [ "$before" != "$after" ]; then
        echo "      MIXED: $before -> $after, this run measures neither" |
            tee "$OUT/$name.mixed"
    else
        echo "      radio $after"
    fi

    grep -oE "au (delivery|late/min|bunching).*" "$OUT/$name.log" |
        sed 's/^/      /' || true
    if [ "$CAPTURE" = 1 ]; then
        "$REPO/target/release/pcap-analyse" "$OUT/$name.pcap" \
            --port "$PORT" --fps 120 --json "$OUT/$name.pcap.json" >/dev/null
        "$REPO/tools/stall-period.py" "$OUT/$name.pcap" | sed 's/^/      /'
    fi
    sleep 5
done

echo
python3 "$REPO/tools/link-report.py" "$OUT"
