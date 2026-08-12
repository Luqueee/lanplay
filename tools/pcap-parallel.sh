#!/usr/bin/env bash
# Where does the bunching enter? Captures the link at the kernel's BPF tap
# while the receiver measures it at the socket, and compares the two.
#
#   AP / air
#     -> Wi-Fi firmware and NIC
#     -> driver and kernel
#     -> BPF tap            <- tcpdump timestamps here
#     -> socket
#     -> lanplay receive    <- the live measurement happens here
#
# A capture that is already bunched narrows the fault to everything above the
# tap - which still includes this Mac's driver and firmware, so it does not
# exonerate the machine. A regular capture against a bursty application is
# the stronger result: it puts the fault in socket delivery or scheduling.
#
# Both sides are counted by the same code. `pcap-analyse` replays the capture
# through the receiver's own depacketiser and delivery metrics, so a
# difference between them is a difference in timing and never in definition.
#
# The capture keeps headers only. Payload would cost disk bandwidth during
# the very run whose timing is being measured, and nothing here reads it.
#
# usage:
#   tools/pcap-parallel.sh [seconds] [repeats]
#
#   IFACE=en0  BITRATE=40  MTU=1200  PORT=5004  OUT=/tmp/pcap

set -euo pipefail

SECONDS_TO_RUN="${1:-120}"
REPEATS="${2:-3}"
IFACE="${IFACE:-en0}"
BITRATE="${BITRATE:-40}"
MTU="${MTU:-1200}"
PORT="${PORT:-5004}"
REPO="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${OUT:-/tmp/pcap}"

mkdir -p "$OUT"

# Headers only: Ethernet 14, IPv4 20, UDP 8, RTP 12 and a 16-byte frame id
# extension is 70 bytes. 96 leaves room for a variable IP header without
# capturing a byte of video.
SNAPLEN=96

echo "capture   $IFACE port $PORT, snaplen $SNAPLEN, $REPEATS runs of ${SECONDS_TO_RUN}s"
echo "output    $OUT"

# /dev/bpf* is root:access_bpf on macOS. Membership is what makes this
# runnable unattended; sudo would stop the harness dead waiting for a
# password halfway through a sweep.
if ! id -Gn | tr ' ' '\n' | grep -qx access_bpf; then
    echo "not in the access_bpf group: tcpdump would need a password" >&2
    exit 1
fi

run() {
    local label="$1" capture="$2"
    "$REPO/target/release/radio-sample" $((SECONDS_TO_RUN + 30)) 1000 \
        >"$OUT/$label.wifi.csv" 2>/dev/null &
    local sampler=$!
    local tcpdump_pid=""
    if [ "$capture" = "yes" ]; then
        # -B raises the kernel buffer so a scheduling hiccup in tcpdump
        # cannot drop packets and be mistaken for a link event. -F pcap
        # because the analyser reads pcap, not pcapng.
        tcpdump -i "$IFACE" -s "$SNAPLEN" -B 16384 -n -p \
            -w "$OUT/$label.pcap" "udp port $PORT" >/dev/null 2>"$OUT/$label.tcpdump.log" &
        tcpdump_pid=$!
        # tcpdump must be listening before the first datagram, and it prints
        # nothing useful to wait on, so wait for the file it opens.
        for _ in $(seq 1 50); do
            [ -s "$OUT/$label.pcap" ] && break
            sleep 0.1
        done
    fi

    IFACE="$IFACE" BITRATE="$BITRATE" MTU="$MTU" LINK_ONLY=1 QUIET=1 \
        REPORT="$OUT/$label.json" \
        "$REPO/tools/e2e-gate.sh" "$SECONDS_TO_RUN" \
        >"$OUT/$label.log" 2>&1 </dev/null ||
        echo "      run returned $? (a failing gate is a data point, not an abort)"

    [ -n "$tcpdump_pid" ] && kill "$tcpdump_pid" 2>/dev/null && wait "$tcpdump_pid" 2>/dev/null
    kill "$sampler" 2>/dev/null || true
    grep -oE "au (delivery|late/min|bunching).*" "$OUT/$label.log" | sed 's/^/      app  /' || true
    if [ "$capture" = "yes" ]; then
        grep -oE "[0-9]+ packets dropped by kernel" "$OUT/$label.tcpdump.log" |
            sed 's/^/      pcap /' || true
        "$REPO/target/release/pcap-analyse" "$OUT/$label.pcap" \
            --port "$PORT" --fps 120 --json "$OUT/$label.pcap.json" |
            grep -E "^(au|datagrams|losses)" | sed 's/^/      pcap /'
    fi
    sleep 5
}

# The control comes first and is short: if switching the capture on changes
# what the receiver measures, nothing after it means anything.
echo
echo "=== control: does capturing perturb the measurement? ==="
for arm in nocap-control cap-control; do
    printf '\n[%s]\n' "$arm"
    case "$arm" in
    nocap-control) run "$arm" no ;;
    cap-control) run "$arm" yes ;;
    esac
done

echo
echo "=== parallel capture ==="
for rep in $(seq 1 "$REPEATS"); do
    printf '\n[%d/%d]\n' "$rep" "$REPEATS"
    run "parallel-r$rep" yes
done

echo
python3 "$REPO/tools/pcap-report.py" "$OUT"
