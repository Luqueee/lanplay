#!/usr/bin/env bash
# The I2 gate: mouse motion from the Mac reaches Windows and is injected.
#
#   Mac                                    Windows
#   ────────────────────────────────────────────────────────
#   NSEvent deltaX/deltaY
#     -> Message::Motion
#       -> UDP 5006  ──────────────────────► recv_from
#                                              -> SendInput
#
# Two things make this gate meaningful rather than a smoke test.
#
# The first is that motion is additive, so the totals can be compared. The
# client prints the sum of everything it sent and the host prints the sum of
# everything it injected; those two numbers agreeing is a statement about the
# whole path that no packet count can make. They will not agree exactly once
# real injection is on, because synthesised relative motion goes through the
# system pointer speed and acceleration curve - which is itself one of the
# findings this gate exists to produce, and the reason a virtual HID device
# will be measured against this backend later.
#
# The second is where the injector has to run. An SSH session on Windows is
# session 0, which has no desktop and no foreground window, so SendInput there
# has nothing to inject into. The injector must therefore run in the
# interactive session, which means a scheduled task and a log file rather than
# a pipe. Everything awkward about this script comes from that.
#
# usage:
#   tools/input-gate.sh [seconds]
#
#   DRY_RUN=1   decode and count on the host without moving the pointer
#   SESSION_ID  defaults to 1
#   MAC_IP      defaults to the address on IFACE
#   IFACE       defaults to en0

set -euo pipefail

SECONDS_TO_RUN="${1:-30}"
IFACE="${IFACE:-en0}"
SESSION_ID="${SESSION_ID:-1}"
PORT=5006
REPO="$(cd "$(dirname "$0")/.." && pwd)"
WIN_REPO='C:\Users\luque\lanplay-rs'
LOG='C:\Users\luque\input-inject.log'

MAC_IP="${MAC_IP:-$(ipconfig getifaddr "$IFACE" 2>/dev/null || true)}"
WIN_IP="$(ssh -n -o BatchMode=yes windows \
    'powershell -NoProfile -Command "(Get-NetIPAddress -AddressFamily IPv4 | Where-Object InterfaceAlias -notlike \"*Loopback*\" | Select-Object -First 1).IPAddress"' \
    2>/dev/null | tr -d '\r\n ')"

if [ -z "${WIN_IP:-}" ]; then
    echo "could not find the host's address" >&2
    exit 1
fi

echo "gate      input, ${SECONDS_TO_RUN}s, session $SESSION_ID"
echo "path      $MAC_IP -> $WIN_IP:$PORT"
echo "inject    $([ "${DRY_RUN:-0}" = 1 ] && echo "dry run, nothing moves" || echo "live SendInput")"

# Built where they run. Cross-compiling the host binary from here would work
# for the check but not for the run: the Windows machine is the only place its
# tests and its scheduled task can execute.
echo
echo "building"
cargo build --release -p lanplay-input-capture 2>&1 | tail -1
scp -q -r "$REPO/windows/input-inject" "windows:$WIN_REPO\\windows\\" 2>/dev/null || true
scp -q "$REPO/Cargo.toml" "windows:$WIN_REPO\\Cargo.toml"
scp -q -r "$REPO/crates/input-protocol" "windows:$WIN_REPO\\crates\\" 2>/dev/null || true
ssh -n -o BatchMode=yes windows "cd $WIN_REPO && cargo build --release -p lanplay-input-inject" 2>&1 | tail -1

# The injector goes up first and outlives the client, so no motion is sent
# into a closed socket. It stops on its own after the run plus a margin, which
# is what keeps a failed gate from leaving a process holding port 5006.
dry=""
[ "${DRY_RUN:-0}" = 1 ] && dry=" --dry-run"
echo
echo "starting the injector in the interactive session"
WIN_TIMEOUT=$((SECONDS_TO_RUN + 90)) "$REPO/tools/win-session.sh" "$LOG" \
    "target\\release\\input-inject-probe.exe --bind 0.0.0.0:$PORT --seconds $((SECONDS_TO_RUN + 15)) --session-id $SESSION_ID$dry" \
    >/tmp/input-inject.out 2>&1 &
injector=$!

# The scheduled task takes a moment to land, and a client that sends into
# nothing would report a clean run having proved nothing at all.
for _ in $(seq 1 40); do
    if ssh -n -o BatchMode=yes windows \
        'powershell -NoProfile -Command "(Get-Process input-inject-probe -ErrorAction SilentlyContinue).Count"' \
        2>/dev/null | tr -d '\r\n ' | grep -q '^[1-9]'; then
        echo "injector  listening"
        break
    fi
    sleep 0.5
done

echo
echo "move the mouse for the next ${SECONDS_TO_RUN}s"
"$REPO/target/release/input-capture-probe" \
    --send-to "$WIN_IP:$PORT" --seconds "$SECONDS_TO_RUN" --session-id "$SESSION_ID" |
    tee /tmp/input-capture.out

wait "$injector" 2>/dev/null || true
echo
echo "host side"
cat /tmp/input-inject.out

# The comparison the gate turns on. Kept in one place so a reader can see
# exactly which two numbers are being set against each other.
python3 - <<'PY'
import re


def totals(path, label):
    try:
        text = open(path).read()
    except OSError:
        print(f"  {label:<8} no output")
        return None
    match = re.search(r"total\s+dx\s+(-?\d+)\s+dy\s+(-?\d+)", text)
    count = re.search(r"(\d+)\s+datagrams", text)
    if not match:
        print(f"  {label:<8} no total in output")
        return None
    dx, dy = int(match.group(1)), int(match.group(2))
    print(f"  {label:<8} dx {dx:>8}  dy {dy:>8}"
          f"{'  datagrams ' + count.group(1) if count else ''}")
    return dx, dy


print("\nmotion, which is additive and therefore comparable")
sent = totals("/tmp/input-capture.out", "sent")
applied = totals("/tmp/input-inject.out", "injected")
if sent and applied:
    for axis, a, b in (("dx", sent[0], applied[0]), ("dy", sent[1], applied[1])):
        if a == 0:
            continue
        print(f"  {axis} ratio {b / a:+.3f}"
              f"{'   exact' if a == b else '   scaled by the pointer curve'}")
PY
