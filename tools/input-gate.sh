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
# The motion itself is posted rather than performed, unless MOVER=0. A hand on
# the mouse gives an unrepeatable run and only two totals, so a disagreement
# between them has nothing to arbitrate it. Posting the events adds a third
# that is exact by construction, and the run becomes something a script can do
# at three in the morning. What it gives up is the path from a physical mouse
# into the window server, which a posted event does not travel; that still
# needs a hand, and needs it only once.
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
#   MOVER=0     require a hand on the mouse instead of posting the motion
#   KEYS=1      also send keys, synthetically, cycling W A S D
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
# Asked of the SSH configuration rather than of the host. Querying Windows
# for "the first non-loopback IPv4" returned a link-local address from a
# virtual adapter, and the run then reported a clean capture whose datagrams
# all failed to send. The address SSH already reaches the machine on is the
# one that works, by construction.
WIN_IP="$(ssh -G windows 2>/dev/null | awk '/^hostname /{print $2}')"

if [ -z "${WIN_IP:-}" ]; then
    echo "could not find the host's address" >&2
    exit 1
fi

# Cleared before anything runs. A previous arm's output left in place is read
# by the report as if it belonged to this one, which is how a keyboard run just
# reported motion totals from the run before it.
rm -f /tmp/input-mover.out /tmp/input-capture.out /tmp/input-inject.out

echo "gate      input, ${SECONDS_TO_RUN}s, session $SESSION_ID"
echo "path      $MAC_IP -> $WIN_IP:$PORT"
echo "inject    $([ "${DRY_RUN:-0}" = 1 ] && echo "dry run, nothing moves" || echo "live SendInput")"

# Built where they run. Cross-compiling the host binary from here would work
# for the check but not for the run: the Windows machine is the only place its
# tests and its scheduled task can execute.
echo
echo "building"
cargo build --release -p lanplay-input-capture 2>&1 | tail -1
"$REPO/tools/win-sync.sh"
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
# Asking for both is a contradiction the probe would resolve silently by
# ignoring one of them, which is exactly how the last keyboard run reported a
# clean pass having sent no keys at all.
if [ "${KEYS:-0}" = 1 ] && [ "${MOVER:-1}" = 1 ]; then
    echo "KEYS=1 and MOVER=1 cannot both hold: the probe runs one or the other" >&2
    exit 2
fi

if [ "${MOVER:-1}" = 1 ]; then
    cargo build --release -q -p lanplay-mouse-mover
    echo "posting motion for ${SECONDS_TO_RUN}s"
    if [ "${KEYS:-0}" = 1 ] && [ "${DRY_RUN:-0}" != 1 ]; then
        # Injected keys land wherever the host has focus, so this says so
        # rather than letting somebody discover it in a document.
        echo "warning   keys will be typed into whatever has focus on the host"
    fi
    # Started after the capture so no posted event lands before there is
    # anything listening, and given a shorter run so it cannot still be moving
    # when the capture stops counting.
    (
        sleep 2
        "$REPO/target/release/mouse-mover" \
            --seconds "$((SECONDS_TO_RUN - 4))" --hz "${MOVER_HZ:-250}" \
            --amplitude "${MOVER_AMPLITUDE:-6}" --pattern "${MOVER_PATTERN:-drift}" \
            --capture-click \
            >/tmp/input-mover.out 2>&1
    ) &
    mover=$!
else
    echo "move the mouse for the next ${SECONDS_TO_RUN}s"
    mover=""
fi
# Synthetic keys and mouse capture are exclusive in the probe, deliberately:
# one session may hold only one event id counter. So a keyboard arm posts no
# motion and a motion arm sends no keys, and the two are separate runs rather
# than one run that quietly did half of what was asked.
probe_args=(--send-to "$WIN_IP:$PORT" --seconds "$SECONDS_TO_RUN" --session-id "$SESSION_ID")
if [ "${KEYS:-0}" = 1 ]; then
    probe_args+=(--synthetic-keys --key-rate "${KEY_RATE:-20}")
fi
"$REPO/target/release/input-capture-probe" "${probe_args[@]}" |
    tee /tmp/input-capture.out
[ -n "$mover" ] && wait "$mover" 2>/dev/null
[ -f /tmp/input-mover.out ] && cat /tmp/input-mover.out

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
    # Each axis separately: the two are printed as "total dx N  total dy N",
    # and a single pattern spanning both silently matched nothing.
    dx_match = re.search(r"(?:total|injected)\s+dx\s+(-?\d+)", text)
    dy_match = re.search(r"(?:total|injected)\s+dy\s+(-?\d+)", text)
    # Anchored, because an unanchored alternation matched the port number in
    # "first datagram from 192.168.1.108:49901" and reported it as a count.
    count = re.search(r"^\s*datagrams\s+(\d+)\s*$|^events \d+\s+datagrams (\d+)",
                      text, re.M)
    failed = re.search(r"failed sends\s+(\d+)", text)
    if not dx_match or not dy_match:
        print(f"  {label:<8} no total in output")
        return None
    dx, dy = int(dx_match.group(1)), int(dy_match.group(1))
    if failed and int(failed.group(1)) > 0:
        print(f"  {label:<8} {failed.group(1)} sends FAILED - the totals below are not comparable")
    datagrams = next((g for g in count.groups() if g), None) if count else None
    print(f"  {label:<8} dx {dx:>8}  dy {dy:>8}"
          f"{'  datagrams ' + datagrams if datagrams else ''}")
    return dx, dy


import os


def line(path, pattern, label):
    """One `events N dx N dy N` row, or None when the run did not print it."""
    try:
        text = open(path).read()
    except OSError:
        return None
    got = re.search(pattern + r"\s+events\s+(\d+)\s+dx\s+(-?\d+)\s+dy\s+(-?\d+)", text)
    if not got:
        return None
    events, dx, dy = (int(g) for g in got.groups())
    print(f"  {label:<28} events {events:>8}  dx {dx:>8}  dy {dy:>8}")
    return events, dx, dy


if os.path.exists("/tmp/input-mover.out"):
    print("\nmotion, which is additive and therefore comparable")
    posted = totals("/tmp/input-mover.out", "posted")
    sent = totals("/tmp/input-capture.out", "sent")
    applied = totals("/tmp/input-inject.out", "injected")
else:
    print("\nmotion: not this arm")
    posted = sent = applied = None

# Two checks, each comparing like with like, because one run already produced a
# discrepancy no loss could have caused: a total that includes a hand on the
# trackpad cannot be compared against a generator that never posted it, and the
# host is sent everything regardless of where it came from.
attributed = line("/tmp/input-capture.out", "motion posted by a program", "attributed to a program")
intruded = line("/tmp/input-capture.out", "motion from a device", "and to a device")
if posted and attributed:
    if intruded and intruded[0] > 0:
        # Refused rather than failed. The window server coalesces mouse-moved
        # events by summing their deltas, and a merged event carries one origin
        # for both contributions, so once a device has moved the attribution is
        # approximate by construction. Printing DISAGREE here would manufacture
        # evidence of a fault out of a limit of the instrument, which is the same
        # mistake as reading absence of evidence as evidence and no better for
        # being in the other direction.
        print(f"  the generator against what the capture attributed to it: not available, "
              f"a device moved {intruded[0]} times during the run")
    else:
        # The window server coalesces mouse-moved events, summing their deltas,
        # so the counts legitimately differ while the totals must not. That is
        # the whole reason motion is additive rather than latest-wins.
        agree = posted == attributed[1:]
        print("  the generator against what the capture attributed to it: "
              + ("totals agree" if agree else f"DISAGREE {posted} against {attributed[1:]}"))
# The one that decides a keyboard run. A host holding a key nobody is pressing
# is the failure the whole reliability design exists to prevent, so it is
# checked whether or not keys were asked for.
held = re.search(r"still held: keys (\{[^}]*\})", open("/tmp/input-inject.out").read())
if held:
    empty = held.group(1) in ("{}", "{ }")
    print(f"\nkeys held on the host at the end: {held.group(1)}"
          f"{'' if empty else '   NOT EMPTY, a key is stuck'}")

if sent and applied:
    print("  the wire: everything the capture sent against what the host injected")
    for axis, a, b in (("dx", sent[0], applied[0]), ("dy", sent[1], applied[1])):
        if a == 0:
            continue
        print(f"  {axis} ratio {b / a:+.3f}"
              f"{'   exact' if a == b else '   scaled by the pointer curve'}")
PY
