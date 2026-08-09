#!/usr/bin/env bash
# Compares sender pacers over the real link with no renderer in the way.
#
# The client's presentation cadence is not reproducible while the desktop is
# in use: macOS suspends a display link for a covered window, and how covered
# it is varies from run to run by more than the pacers differ. `net-bench
# receive` opens no window at all, so what it measures is the link.
set -euo pipefail

seconds=${1:-120}
root=$(cd "$(dirname "$0")/.." && pwd)
fixture=motion-1920x1080@120-10s-50M.h264
cd "$root"

run() {
    local label=$1; shift
    local out="target/link-$label.log"
    ./target/release/net-bench receive --bind 0.0.0.0:5004 \
        --seconds "$((seconds + 8))" --fps 120 > "$out" 2>&1 &
    local rx=$!
    until grep -q "mode      receive" "$out" 2>/dev/null; do sleep 0.2; done
    sleep 1

    ssh -o BatchMode=yes windows \
        "cd C:\\Users\\luque\\lanplay-rs && .\\target\\release\\net-bench.exe send \
         --to 192.168.1.108:5004 --fixture fixtures\\$fixture \
         --fps 120 --seconds $seconds --pacer $*" \
        > "target/link-$label.tx.log" 2>&1
    wait $rx || true

    printf '\n--- %-10s %s\n' "$label" "$*"
    grep -E "^rx |    lost|^inter-arrival|^arrival |rfc 3550" "$out" || true
}

run burst burst
run micro025 micro --micro-window-ms 0.25
run micro05 micro --micro-window-ms 0.5
run micro10 micro --micro-window-ms 1.0
