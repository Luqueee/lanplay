#!/usr/bin/env bash
# Runs one sender configuration over the real link and reports what the client
# saw. The client is permanently on Wi-Fi and the host is wired, so the
# question every run answers is whether the link, not the pipeline, is what
# limits cadence.
#
#   usage: wifi-matrix.sh <label> <seconds> <fps> <pacer> [pacer args…]
set -euo pipefail

label=$1; seconds=$2; fps=$3; pacer=$4; shift 4
root=$(cd "$(dirname "$0")/.." && pwd)
out="$root/target/wifi-$label.log"
fixture=motion-1920x1080@120-10s-50M.h264

cd "$root"
# `caffeinate` is not a nicety: an unattended ten-minute run on a laptop will
# otherwise put the display to sleep, `CAMetalDisplayLink` stops with it, and
# the measurement becomes one of the screensaver. A run that lost the display
# reads as a one-second present interval and a starved renderer.
caffeinate -dis ./target/release/lanplay-client --transport lan --bind 0.0.0.0:5004 \
    --seconds "$seconds" --fps 120 --feed-fps "$fps" --mode display-link \
    > "$out" 2>&1 &
client=$!

# The client binds before it prints; give it a moment rather than racing it.
until grep -q "listening on" "$out" 2>/dev/null; do sleep 0.2; done
sleep 1

ssh -o BatchMode=yes windows \
    "cd C:\\Users\\luque\\lanplay-rs && .\\target\\release\\net-bench.exe send \
     --to 192.168.1.108:5004 --fixture fixtures\\$fixture \
     --fps $fps --seconds $seconds --pacer $pacer $*" \
    > "$root/target/wifi-$label.tx.log" 2>&1

wait $client || true

printf '\n===== %s (%s fps, pacer %s %s) =====\n' "$label" "$fps" "$pacer" "$*"
grep -E "^(source interval|present interval|local age|arrival|decode) " "$out" || true
grep -E "\[(pass|FAIL)\] (link|pipeline) +(link holds cadence|access units intact|decoder keeps up|presentation tracks arrival|transport clean)" "$out" || true
grep -E "^gate:" "$out" || true
