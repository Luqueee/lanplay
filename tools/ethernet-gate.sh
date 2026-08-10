#!/usr/bin/env bash
# The 1080p120 end-to-end gate, over Ethernet, in one command.
#
#   Windows: IDD-LAB 1920x1080@120 -> DDA -> GPU BGRA->NV12 -> NVENC H.264
#            P1 LL 50 Mbps -> RTP burst -> Ethernet
#   macOS:   RTP -> VideoToolbox -> latest-frame-wins -> CAMetalDisplayLink
#
# Wi-Fi and Ethernet results are never mixed, so the path is pinned rather
# than left to macOS: the client binds the wired address explicitly and the
# host is told to send there. Both interfaces can stay up; a datagram
# addressed to the wired IP arrives on the wired NIC.
#
# usage:
#   tools/ethernet-gate.sh [seconds]        # 60 for the sanity run, 600 to soak
#
# Everything the run needs on the Windows side must already be up: the
# IDD-LAB controller (LanPlayIddLabCtl) and, for a synthetic source, a
# present-source covering the virtual monitor.

set -euo pipefail

SECONDS_TO_RUN="${1:-60}"
WIRED_IF="${WIRED_IF:-en8}"
PORT="${PORT:-5004}"
REPO="$(cd "$(dirname "$0")/.." && pwd)"
CLIENT="$REPO/target/release/lanplay-client"
REPORT="${REPORT:-/tmp/ethernet-gate-${SECONDS_TO_RUN}s.json}"
WIN_IP="$(ssh -G windows 2>/dev/null | awk '/^hostname /{print $2}')"

fail() {
    echo "ethernet-gate: $1" >&2
    exit 1
}

# ---- link preflight -------------------------------------------------------
# A run over a link that silently fell back to Wi-Fi would be a Wi-Fi result
# wearing an Ethernet label, which is worse than no result.

status="$(ifconfig "$WIRED_IF" 2>/dev/null | awk '/status:/{print $2}')"
[ "$status" = "active" ] || fail "$WIRED_IF is ${status:-missing}: connect the cable"

WIRED_IP="$(ipconfig getifaddr "$WIRED_IF" || true)"
[ -n "$WIRED_IP" ] || fail "$WIRED_IF is up but has no IPv4 address"

media="$(ifconfig "$WIRED_IF" | awk '/media:/{$1=""; print substr($0,2)}')"
echo "link      $WIRED_IF $WIRED_IP, $media"

# Reaching the host from that source address proves the path exists before a
# measurement depends on it.
ping -c 2 -t 2 -S "$WIRED_IP" "$WIN_IP" >/dev/null 2>&1 ||
    fail "no route to $WIN_IP from $WIRED_IP"
echo "route     $WIRED_IP -> $WIN_IP reachable"

[ -x "$CLIENT" ] || fail "build the client first: cargo build --release -p lanplay-client"

# ---- receiver -------------------------------------------------------------
# The renderer refuses to measure an occluded window, and a window opened by
# a process launched over ssh does not come forward on its own.

CLIENT_LOG="$(mktemp -t ethernet-gate-client)"
"$CLIENT" \
    --transport lan --bind "$WIRED_IP:$PORT" \
    --width 1920 --height 1080 --fps 120 \
    --seconds "$SECONDS_TO_RUN" --fixture-seconds 10 --fixture-dir "$REPO/fixtures" \
    --mode display-link --require-clean-display \
    --window-seconds 10 --report "$REPORT" >"$CLIENT_LOG" 2>&1 &
CLIENT_PID=$!
trap 'kill "$CLIENT_PID" 2>/dev/null || true' EXIT

for _ in $(seq 1 60); do
    kill -0 "$CLIENT_PID" 2>/dev/null || break
    if grep -q "preflight: complete" "$CLIENT_LOG" 2>/dev/null; then
        break
    fi
    osascript \
        -e 'tell application "System Events" to tell process "lanplay-client" to set frontmost to true' \
        -e 'tell application "System Events" to tell process "lanplay-client" to perform action "AXRaise" of window 1' \
        >/dev/null 2>&1 || true
    sleep 0.2
done
grep -q "preflight: complete" "$CLIENT_LOG" || {
    cat "$CLIENT_LOG" >&2
    fail "client preflight refused the run"
}
echo "receiver  listening on $WIRED_IP:$PORT"

# ---- sender ---------------------------------------------------------------
# Through the interactive session: ssh lands in session 0, which has no
# display devices, so Desktop Duplication finds nothing there.

RUNNER='C:\Users\luque\ethernet-gate.ps1'
LOCAL_RUNNER="$(mktemp -t ethernet-gate-runner)"
cat >"$LOCAL_RUNNER" <<PS1
\$ErrorActionPreference = 'Stop'
\$probe = 'C:\\Users\\luque\\lanplay-rs\\target\\release\\lanplay-nvenc-probe.exe'
Get-Process lanplay-nvenc-probe -ErrorAction SilentlyContinue | Stop-Process -Force
# One line: PowerShell continues with a backtick, not a backslash, and a
# wrapped command that silently loses its tail is a run that measures nothing.
& \$probe --mode paced --input nv12 --source dda --output 1 --send-to $WIRED_IP:$PORT --mtu 1200 --seconds $SECONDS_TO_RUN --warmup 0 --fps 120 --width 1920 --height 1080 --bitrate-mbps 50 --preset p1 --tuning ll --idr-interval 120 --window-seconds 10
exit \$LASTEXITCODE
PS1
scp -q "$LOCAL_RUNNER" "windows:$(printf '%s' "$RUNNER" | tr '\\' '/')"
rm -f "$LOCAL_RUNNER"

echo "sender    1080p120 NV12 -> NVENC -> RTP burst for ${SECONDS_TO_RUN} s"
echo
host_status=0
WIN_TIMEOUT=$((SECONDS_TO_RUN + 120)) "$REPO/tools/win-session.sh" \
    'C:\Users\luque\ethernet-gate.log' \
    "powershell -NoProfile -ExecutionPolicy Bypass -File $RUNNER" || host_status=$?

wait "$CLIENT_PID" && client_status=0 || client_status=$?
trap - EXIT
echo
cat "$CLIENT_LOG"
rm -f "$CLIENT_LOG"

echo
echo "host gate   $([ "$host_status" -eq 0 ] && echo PASS || echo "FAIL ($host_status)")"
echo "client gate $([ "$client_status" -eq 0 ] && echo PASS || echo "FAIL ($client_status)")"
echo "report      $REPORT"
[ "$host_status" -eq 0 ] && [ "$client_status" -eq 0 ]
