#!/usr/bin/env bash
# The 1080p120 end-to-end run, host to Mac, in one command.
#
#   Windows: IDD-LAB 1920x1080@120 -> DDA -> GPU BGRA->NV12 -> NVENC H.264
#            P1 LL -> RTP burst -> link
#   macOS:   RTP -> VideoToolbox -> latest-frame-wins -> CAMetalDisplayLink
#
# The path is pinned rather than left to macOS: the client binds one
# interface's address explicitly and the host is told to send there. Both
# interfaces can stay up; a datagram addressed to that IP arrives on that NIC.
# A run labelled Ethernet can therefore never quietly be a Wi-Fi result, and
# the two are never mixed.
#
# usage:
#   tools/e2e-gate.sh [seconds]
#
#   IFACE=en0     which macOS interface receives (en0 Wi-Fi, en8 Ethernet)
#   BITRATE=50    encoder target, Mbps
#   REPORT=path   where the client's JSON lands
#   QUIET=1       suppress the client's own report; the caller reads the JSON
#
# Everything the run needs on the Windows side must already be up: the
# IDD-LAB controller (LanPlayIddLabCtl) and, for a synthetic source, a
# present-source covering the virtual monitor.

set -euo pipefail

SECONDS_TO_RUN="${1:-60}"
IFACE="${IFACE:-${WIRED_IF:-en8}}"
BITRATE="${BITRATE:-50}"
# One rate, read by both ends. The producer checks the rate a phase request was
# computed for against the rate it is pacing and says so when they differ, but
# what reaches it is the host's number standing in for the client's. Two
# literals here could drift apart and make that check accuse a matched pair, or
# stay quiet about a real disagreement, so there is only one.
FPS="${FPS:-120}"
PORT="${PORT:-5004}"
CONTROL_PORT="${CONTROL_PORT:-5005}"
REPO="$(cd "$(dirname "$0")/.." && pwd)"
CLIENT="$REPO/target/release/lanplay-client"
REPORT="${REPORT:-/tmp/e2e-gate-${BITRATE}m-${SECONDS_TO_RUN}s.json}"
WIN_IP="$(ssh -G windows 2>/dev/null | awk '/^hostname /{print $2}')"

fail() {
    echo "e2e-gate: $1" >&2
    exit 1
}

# ---- link preflight -------------------------------------------------------
# An interface that is down, or up without an address, would send the run
# somewhere else entirely and label the result with this one.

status="$(ifconfig "$IFACE" 2>/dev/null | awk '/status:/{print $2}')"
[ "$status" = "active" ] || fail "$IFACE is ${status:-missing}: connect the cable"

LOCAL_IP="$(ipconfig getifaddr "$IFACE" || true)"
[ -n "$LOCAL_IP" ] || fail "$IFACE is up but has no IPv4 address"

media="$(ifconfig "$IFACE" | awk '/media:/{$1=""; print substr($0,2)}')"
echo "link      $IFACE $LOCAL_IP, $media"

# Reaching the host from that source address proves the path exists before a
# measurement depends on it.
ping -c 2 -t 2 -S "$LOCAL_IP" "$WIN_IP" >/dev/null 2>&1 ||
    fail "no route to $WIN_IP from $LOCAL_IP"
echo "route     $LOCAL_IP -> $WIN_IP reachable"

[ -x "$CLIENT" ] || fail "build the client first: cargo build --release -p lanplay-client"

# ---- GPU power state ------------------------------------------------------
# A desktop stream is a light load: 32% GPU, 26% NVENC. The driver reads that
# as idle and drops the card to 300-495 MHz core and 810 MHz memory, at which
# point capture, conversion and encode no longer fit in 8.33 ms and the run
# settles at 110 Hz with an encode p99 of 14 ms. Nothing thermal - 41 C, 14 W;
# clocks_throttle_reasons reads GpuIdle. Locking clears it: encode p99 falls
# from 14.4 ms to 2.3 ms and every window holds 120.0 Hz.
#
# It is stated here rather than left as a setting someone remembers, because
# a measurement whose conditions are not part of the run is not reproducible.
ssh -o BatchMode=yes windows "nvidia-smi -lgc 2000,2700 && nvidia-smi -lmc 9001" >/dev/null 2>&1 ||
    fail "could not lock GPU clocks on the host"
restore_clocks() { ssh -o BatchMode=yes windows "nvidia-smi -rgc && nvidia-smi -rmc" >/dev/null 2>&1 || true; }
echo "clocks    host GPU locked to 2000-2700 MHz core, 9001 MHz memory"

# ---- laboratory source ----------------------------------------------------
# The virtual monitor exists only while its controller process does, and
# Desktop Duplication only produces frames while something draws on it.
# Bringing both up here rather than assuming them is what keeps a long sweep
# from silently measuring an output that went away an hour ago.

source_state="$(WIN_TIMEOUT=60 "$REPO/tools/win-session.sh" \
    'C:\Users\luque\ensure-lab-source.log' \
    'powershell -NoProfile -ExecutionPolicy Bypass -File C:\Users\luque\ensure-lab-source.ps1' 2>&1)" ||
    { echo "$source_state" >&2; fail "could not bring up the laboratory source"; }
echo "source    $(printf '%s' "$source_state" | tr -d '\r' | grep -E '^monitor' || echo 'monitor unknown')"

# ---- receiver -------------------------------------------------------------
# The renderer refuses to measure an occluded window, because an occluded
# window has its display link suspended and every presentation number would
# be a number about a screensaver.
#
# That guard is right for a run that ranks on presentation and wrong for one
# that ranks on delivery. The receive thread does not care what is in front
# of the window: packet loss, arrival spread and source cadence are as valid
# behind another app as in front of it, and insisting on the window turns a
# link experiment into a fight with the window server. `REQUIRE_CLEAN_DISPLAY=0`
# says the run is about the link; the report still records occlusion changes
# under `invalidating_events`, so nobody can later mistake its presentation
# figures for measurements.
#
# The negative control is in the same position: it answers a question about
# the decoder, and reaches preflight within a second of launch, before any
# raise could land.
# `LINK_ONLY=1` drops the renderer entirely: no window, no display link, no
# AppKit. Delivery, loss, reordering and decode are all measured before
# anything reaches a screen, so a radio experiment has no business waiting on
# one. Runs that rank on presentation still need the window and still say so.
DISPLAY_ARG="--require-clean-display"
if [ "${LINK_ONLY:-0}" = "1" ]; then
    DISPLAY_ARG="--link-only"
elif [ "${PARAMETER_SETS:-host}" = "fixture" ] || [ "${REQUIRE_CLEAN_DISPLAY:-1}" = "0" ]; then
    DISPLAY_ARG=""
fi

# A run that measures presentation needs a display that is awake. The client
# holds a LatencyCritical activity, which stops App Nap but does not stop the
# screen sleeping, and CoreGraphics then reports no active displays at all:
# the preflight fails on occlusion, the client exits without a report, and a
# fifteen-minute sweep quietly loses half its runs to a screensaver. `-u`
# asserts user activity to wake a display that has already gone, `-d` holds it
# awake for as long as the client runs.
caffeinate -u -t 2 >/dev/null 2>&1 || true

CLIENT_LOG="$(mktemp -t e2e-gate-client)"
caffeinate -d "$CLIENT" \
    --transport lan --bind "$LOCAL_IP:$PORT" --control "$WIN_IP:$CONTROL_PORT" \
    --parameter-sets "${PARAMETER_SETS:-host}" \
    --width 1920 --height 1080 --fps "$FPS" \
    --seconds "$SECONDS_TO_RUN" --fixture-seconds 10 --fixture-dir "$REPO/fixtures" \
    --mode display-link $DISPLAY_ARG \
    --phase-align "${PHASE_ALIGN:-on}" \
    --window-seconds 10 --report "$REPORT" >"$CLIENT_LOG" 2>&1 &
CLIENT_PID=$!
trap 'kill "$CLIENT_PID" 2>/dev/null || true; restore_clocks' EXIT

# The renderer refuses to measure an occluded window, and it does not
# preflight until the host has answered - which is fifteen seconds after the
# client starts negotiating. A raise loop that stops at the negotiation
# marker leaves that whole gap unguarded, and anything that comes forward in
# it fails the run for a reason that has nothing to do with the link. This
# has cost five runs of a nine-run sweep, so the raiser now runs for as long
# as the client does.
raise_window() {
    while kill -0 "$CLIENT_PID" 2>/dev/null; do
        osascript \
            -e 'tell application "System Events" to tell process "lanplay-client" to set frontmost to true' \
            -e 'tell application "System Events" to tell process "lanplay-client" to perform action "AXRaise" of window 1' \
            >/dev/null 2>&1 || true
        sleep 0.5
    done
}
raise_window &
RAISER_PID=$!
trap 'kill "$CLIENT_PID" "$RAISER_PID" 2>/dev/null || true; restore_clocks' EXIT

# The receiver blocks on the host's VideoConfig before it can build a
# decoder, so the marker to wait for is the moment it starts negotiating,
# not the preflight block that comes after. Waiting for preflight here would
# deadlock: the client waits for a host this script has not launched yet.
READY='control: connecting'
for _ in $(seq 1 100); do
    kill -0 "$CLIENT_PID" 2>/dev/null || break
    if grep -qE "$READY" "$CLIENT_LOG" 2>/dev/null; then
        break
    fi
    sleep 0.2
done
grep -qE "$READY" "$CLIENT_LOG" || {
    cat "$CLIENT_LOG" >&2
    fail "receiver never became ready"
}
echo "receiver  listening on $LOCAL_IP:$PORT, negotiating on $CONTROL_PORT"

# ---- sender ---------------------------------------------------------------
# Through the interactive session: ssh lands in session 0, which has no
# display devices, so Desktop Duplication finds nothing there.
#
# The config handshake stays on in every arm, including the negative control.
# It is what stops the host sending before the receiver exists, and switching
# it off alongside the parameter sets would confound the two: an arm without
# it loses a large and variable slice of every stream at startup, which is a
# different defect wearing the same clothes.

RUNNER='C:\Users\luque\e2e-gate.ps1'
LOCAL_RUNNER="$(mktemp -t e2e-gate-runner)"
cat >"$LOCAL_RUNNER" <<PS1
\$ErrorActionPreference = 'Stop'
\$probe = 'C:\\Users\\luque\\lanplay-rs\\target\\release\\lanplay-nvenc-probe.exe'
Get-Process lanplay-nvenc-probe -ErrorAction SilentlyContinue | Stop-Process -Force
# One line: PowerShell continues with a backtick, not a backslash, and a
# wrapped command that silently loses its tail is a run that measures nothing.
#
# Uncapped, not paced: a live capture source already has a clock, and adding
# a second one at the same nominal rate makes the two beat. That shows up as
# a capture p50 of exactly one frame period and a throughput near 110, with
# nothing downstream at fault. Uncapped follows the source, which is what the
# product does anyway.
& \$probe --mode uncapped --input nv12 --source dda --output-name IDD-LAB --send-to $LOCAL_IP:$PORT --control-port $CONTROL_PORT --service-class ${SERVICE_CLASS:-best-effort} --mtu ${MTU:-1200} --seconds $SECONDS_TO_RUN --warmup 0 --fps $FPS --width 1920 --height 1080 --bitrate-mbps $BITRATE --preset p1 --tuning ll --idr-interval 120 --window-seconds 10
exit \$LASTEXITCODE
PS1
scp -q "$LOCAL_RUNNER" "windows:$(printf '%s' "$RUNNER" | tr '\\' '/')"
rm -f "$LOCAL_RUNNER"

echo "sender    1080p120 NV12 -> NVENC ${BITRATE} Mbps -> RTP burst for ${SECONDS_TO_RUN} s"
echo
host_status=0
WIN_TIMEOUT=$((SECONDS_TO_RUN + 120)) "$REPO/tools/win-session.sh" \
    'C:\Users\luque\e2e-gate.log' \
    "powershell -NoProfile -ExecutionPolicy Bypass -File $RUNNER" || host_status=$?

wait "$CLIENT_PID" && client_status=0 || client_status=$?
trap - EXIT
restore_clocks
echo
if [ -z "${QUIET:-}" ]; then
    cat "$CLIENT_LOG"
fi
rm -f "$CLIENT_LOG"

echo
echo "host gate   $([ "$host_status" -eq 0 ] && echo PASS || echo "FAIL ($host_status)")"
echo "client gate $([ "$client_status" -eq 0 ] && echo PASS || echo "FAIL ($client_status)")"
echo "report      $REPORT"
[ "$host_status" -eq 0 ] && [ "$client_status" -eq 0 ]
