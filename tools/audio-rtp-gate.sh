#!/usr/bin/env bash
# A3: Opus over RTP, and what the radio actually loses.
#
# Two arms that mean different things, which is the whole design of this gate.
#
# Over loopback the path never touches a wire, so every figure must be perfect. A
# lost packet, a duplicate, a corrupted payload or a timestamp that advanced by
# anything other than the frame's sample count is a defect in the code, and there
# is nowhere else for it to have come from.
#
# Over the radio those same numbers are the measurement the phase exists to
# produce. The radio arm sends to the LAB HOST, not to this machine's own routable
# address: the kernel short-circuits a datagram addressed to a local interface onto
# loopback, so the first version of this gate measured the same path twice and
# reported the radio losing nothing. Counted, 1000 packets to this machine's own
# address moved lo0 by 1016 and en0 by 138, while the same 1000 to the router moved
# en0 by 1091. The plan is explicit that the audio deadline is too short to build
# recovery because something sounds right, so loss gets measured before anything is
# built to hide it. Loss here is therefore reported, not failed: a gate that
# demanded zero loss over Wi-Fi would be demanding a different radio.
#
# What must hold in both arms is the stream's own arithmetic. The RTP timestamp is a
# sample counter, so it advances by exactly the frame's per-channel sample count
# whatever the network does to the packet carrying it, and a timestamp that drifts
# with the sender's scheduling would leave a receiver unable to tell a late packet
# from a packet describing a later moment.
#
# usage:
#   tools/audio-rtp-gate.sh [seconds]

set -euo pipefail

SECONDS_TO_RUN="${1:-30}"
PORT=5008
REPO="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${OUT:-/tmp/audio-rtp-gate/$(date +%Y%m%d-%H%M%S)}"

mkdir -p "$OUT"
echo "results   $OUT"

cargo build --release -q -p lanplay-audio-codec -p lanplay-transport
PROBE="$(ls "$REPO"/target/release/*audio-rtp* 2>/dev/null | head -1)"
if [ -z "$PROBE" ]; then
    echo "no audio rtp probe was built; the packetiser's probe has to exist for this to mean anything" >&2
    exit 1
fi

# A port of its own, away from video on 5004 and input on 5006. The plan wants the
# streams on separate sockets so that pushing out a video access unit cannot sit in
# front of an audio packet, and a gate that shared a port would be measuring
# something the product will never do.
LOCAL_IP="$(ifconfig en0 2>/dev/null | awk '/inet /{print $2; exit}')"
WIN_IP="$(ssh -G windows 2>/dev/null | awk '/^hostname /{print $2}')"

# The radio arm is skipped rather than faked when the host is not there. Declaring a
# measurement unavailable costs nothing; producing one from a path that never left
# this machine cost an hour earlier today, when a datagram addressed to this
# machine's own routable address was short-circuited onto loopback and the gate
# reported the radio losing nothing. Counted at the time: 1000 packets to the local
# address moved lo0 by 1016 and en0 by 138, while the same 1000 to the router moved
# en0 by 1091.
RADIO=no
if [ -n "$LOCAL_IP" ] && [ -n "$WIN_IP" ] &&
    ssh -o BatchMode=yes -o ConnectTimeout=5 windows 'echo alive' >/dev/null 2>&1; then
    RADIO=yes
    echo "radio     en0 $LOCAL_IP -> host $WIN_IP:$PORT"
    # libopus builds on the host through the cmake that ships inside Visual Studio
    # BuildTools, which is what makes a real second endpoint possible at all.
    CMAKE_BIN='C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin'
    "$REPO/tools/win-sync.sh" >/dev/null
    "$REPO/tools/win-ssh.sh" "set \"PATH=%PATH%;$CMAKE_BIN\" && cd C:\\Users\\luque\\lanplay-rs && cargo build --release -q -p lanplay-audio-codec" >/dev/null
    echo "built     probe on the host too"
else
    echo "radio     unavailable: the host does not answer, so loss over the air is not measured"
fi

# Loopback first, alone in the process, because it is the arm that can only fail by
# this code's own doing.
"$PROBE" --bind "0.0.0.0:$PORT" --send-to "127.0.0.1:$PORT" \
    --seconds "$SECONDS_TO_RUN" --frame-ms 5 >"$OUT/loopback.out" 2>&1 || true
echo "arm       loopback done"
sleep 1

if [ "$RADIO" = yes ]; then
    # The radio arm needs two machines. The receiver runs on the host, in the
    # interactive session, and the sender is here; the datagrams therefore leave this
    # radio, cross the router and arrive at a different network stack, which is the
    # only arrangement in which a loss figure is the radio's.
    WIN_TASK=lanplay-audio-rtp WIN_TIMEOUT=$((SECONDS_TO_RUN + 120)) "$REPO/tools/win-session.sh" \
        'C:\Users\luque\audio-rtp.log' \
        "target\\release\\audio-rtp-probe.exe --bind 0.0.0.0:$PORT --receive-only --seconds $((SECONDS_TO_RUN + 15))" \
        >"$OUT/radio.out" 2>&1 &
    receiver=$!

    for _ in $(seq 1 40); do
        "$REPO/tools/win-ssh.sh" \
            'powershell -NoProfile -Command "(Get-Process audio-rtp-probe -ErrorAction SilentlyContinue).Count"' \
            2>/dev/null | tr -d '\r ' | grep -q '^[1-9]' && break
        sleep 0.5
    done
    echo "receiver  listening on the host"

    "$PROBE" --bind "0.0.0.0:$((PORT + 1))" --send-to "$WIN_IP:$PORT" \
        --seconds "$SECONDS_TO_RUN" --frame-ms 5 >"$OUT/sender.out" 2>&1 || true
    wait "$receiver" 2>/dev/null || true
    echo "arm       radio done"
fi

python3 - "$OUT" <<'PY'
import re
import sys

out = sys.argv[1]
LEFT_HZ, RIGHT_HZ = 997.0, 1997.0
TONE_TOLERANCE_HZ = 5.0
MTU = 1200


def arm(name):
    try:
        body = open(f"{out}/{name}.out").read()
    except OSError:
        return None
    # The radio arm is two processes on two machines, so what was sent is only known
    # to the sender and what arrived is only known to the receiver. Reading both from
    # one report would be reading one machine's opinion of the other's socket.
    try:
        sent_body = open(f"{out}/sender.out").read() if name == "radio" else body
    except OSError:
        sent_body = body

    def field(pattern, source=None):
        got = re.search(pattern, source if source is not None else body, re.M)
        return got.group(1) if got else None

    exact = re.search(r"^timestamp delta exact (\d+) of (\d+)$", body, re.M)
    gaps = re.search(r"^sequence gaps (\d+) totalling (\d+)$", body, re.M)
    verified = re.search(r"^payload verified (\d+) of (\d+)$", body, re.M)
    tone = re.search(r"^tone left ([\d.]+) right ([\d.]+)$", body, re.M)
    return {
        "name": name,
        "sent": field(r"^packets sent (\d+)$", sent_body),
        "received": field(r"^packets received (\d+)$"),
        "exact": exact,
        "gaps": gaps,
        "reordered": field(r"^reordered (\d+)$"),
        "duplicates": field(r"^duplicates (\d+)$"),
        "verified": verified,
        "largest": field(r"^largest datagram (\d+)$"),
        "tone": (float(tone.group(1)), float(tone.group(2))) if tone else None,
        "payload_type": field(r"^payload type (\d+)$"),
        "ssrc": field(r"^ssrc (\S+)$"),
    }


arms = [a for a in (arm("loopback"), arm("radio")) if a]
# Named, not omitted. A verdict that simply did not mention the radio would read as a
# gate that had tested it, and the whole point of this phase's plan is that loss gets
# measured before anything is built to hide it.
radio_ran = any(a["name"] == "radio" for a in arms)

print("\nthe stream\n")
for a in arms:
    print(
        f"  {a['name']:<9} ssrc {a['ssrc']}  pt {a['payload_type']}  "
        f"sent {a['sent']}  received {a['received']}  largest {a['largest']} B"
    )
    if a["exact"]:
        print(
            f"            timestamp deltas exact {a['exact'].group(1)} of {a['exact'].group(2)}"
            f"   gaps {a['gaps'].group(1) if a['gaps'] else '?'}"
            f"   reordered {a['reordered']}  duplicates {a['duplicates']}"
            f"   payload verified {a['verified'].group(1) if a['verified'] else '?'}"
            f" of {a['verified'].group(2) if a['verified'] else '?'}"
        )
    tone = f"{a['tone'][0]:.1f} / {a['tone'][1]:.1f} Hz" if a["tone"] else "not measured"
    print(f"            decoded tone {tone}")

failures = []
findings = []

for a in arms:
    where = a["name"]
    if a["sent"] is None or int(a["sent"]) == 0:
        failures.append(f"{where}: nothing was sent, so every zero is an absence")
        continue
    if a["received"] is None:
        failures.append(f"{where}: the receiver reported nothing")
        continue

    # The stream's own arithmetic, which the network cannot excuse in either arm.
    if a["exact"] is None:
        failures.append(f"{where}: timestamp deltas were not reported, which is the criterion")
    elif a["exact"].group(1) != a["exact"].group(2):
        failures.append(
            f"{where}: {a['exact'].group(1)} of {a['exact'].group(2)} timestamp deltas were "
            "exact - the timestamp is a sample counter and a network cannot change it"
        )
    if int(a["largest"]) > MTU:
        failures.append(
            f"{where}: largest datagram {a['largest']} B over a {MTU} B MTU, so something "
            "fragmented and an 81-byte Opus frame cannot have"
        )
    if a["tone"] is None:
        failures.append(f"{where}: the tone was not measured, so nothing proves audio arrived")
    else:
        left, right = a["tone"]
        if abs(left - LEFT_HZ) > TONE_TOLERANCE_HZ or abs(right - RIGHT_HZ) > TONE_TOLERANCE_HZ:
            failures.append(
                f"{where}: decoded tone {left:.1f} / {right:.1f} Hz against "
                f"{LEFT_HZ:.0f} / {RIGHT_HZ:.0f}"
            )
    # Byte verification is a same-process check: the ledger compares against what
    # this process sent, and across two machines the question does not apply. The
    # receiver reports that rather than reporting zero of N, which would read as
    # total corruption. What stands in for it is the tone, which proves the path
    # carried real audio rather than plausible bytes.
    if where == "radio" and a["verified"] is None:
        findings.append("payload bytes are unverifiable across two machines; the tone stands in")
    elif a["verified"] and a["verified"].group(1) != a["verified"].group(2):
        # Corruption is never the network being busy: a datagram either arrives whole
        # or does not arrive, so a payload that differs is a fault in the code.
        failures.append(
            f"{where}: {a['verified'].group(1)} of {a['verified'].group(2)} payloads verified - "
            "a datagram arrives whole or not at all, so a difference is ours"
        )

    lost = int(a["sent"]) - int(a["received"])
    rate = 100.0 * lost / int(a["sent"])
    if where == "loopback":
        # Nothing on this path can lose a packet except a buffer this code sized.
        if lost != 0:
            failures.append(
                f"loopback: {lost} packets of {a['sent']} never arrived, and loopback has no "
                "wire to blame"
            )
        if int(a["duplicates"]) or int(a["reordered"]):
            failures.append(
                f"loopback: {a['duplicates']} duplicates and {a['reordered']} reordered on a "
                "path that cannot do either"
            )
    else:
        # The measurement, not a criterion. A gate demanding zero loss over Wi-Fi
        # would be demanding a different radio.
        findings.append(
            f"the radio lost {lost} of {a['sent']} packets, {rate:.3f} %, and reordered "
            f"{a['reordered']} with {a['duplicates']} duplicates"
        )

if not radio_ran:
    findings.append(
        "loss over the air is NOT measured here: the host was unreachable, so only the "
        "stream's arithmetic and the loopback path were tested. The figure this phase "
        "owes is a second endpoint's, and A6 is where it arrives."
    )

print()
for finding in findings:
    print(f"FINDING {finding}")
print()
if failures:
    for failure in failures:
        print(f"FAIL {failure}")
    sys.exit(1)
if radio_ran:
    print(
        "PASS one Opus frame is one datagram, the timestamp counts samples exactly, loopback\n"
        "     is lossless, and what the radio loses is now a number instead of an assumption"
    )
else:
    print(
        "PASS on correctness only: one Opus frame is one datagram, the timestamp counts\n"
        "     samples exactly, and loopback is lossless. The radio figure is owed, not given."
    )
PY
