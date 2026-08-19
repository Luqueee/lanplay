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
# Nothing here parses the probe's prose. Each end of each arm writes one JSON
# envelope and `xtask verdict` decides it, which is the arrangement docs/testing.md
# argues for. The regular expressions this script used to carry were the family of
# defect that read 6001 captured packets as none in a sibling harness: three of the
# eight instrument failures this project has recorded were that mechanism. Every
# probe still prints its keyed block beside the document, because that is what a
# person whose gate just failed reads, and neither audience is served by being
# handed the other's form.
#
# Which criteria an arm states follows from what the probe is told the arm is, and
# it is told because that is the one property of a run no process can see from the
# inside: a receive-only run cannot tell this machine's fault relay from a peer
# across the air, and a sending run cannot tell an arm whose far end does the
# counting from one that lost everything. So the negative control is judged against
# the loopback criteria rather than against a threshold of its own, and what it has
# to break is exactly what the loopback arm forbids.
#
# usage:
#   tools/audio-rtp-gate.sh [seconds]
#
# exit 0  every arm held what it stated, and the control did not
# exit 1  an arm was decided and disagreed, or the control was decided and held,
#         and the block above the verdict names the criterion and its numbers
# exit 2  refused: an arm stated a criterion nobody could decide, or a document that
#         had to exist does not, so nothing here says whether the stream's
#         arithmetic holds either way - and a control nobody could decide has not
#         been shown to fail, which is a different thing from having failed

set -euo pipefail

SECONDS_TO_RUN="${1:-30}"
PORT=5008
REPO="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${OUT:-/tmp/audio-rtp-gate/$(date +%Y%m%d-%H%M%S)}"
HOST="${WIN_HOST:-windows}"
XTASK="$REPO/target/release/xtask"

mkdir -p "$OUT"
echo "results   $OUT"

cargo build --release -q -p lanplay-audio-codec -p lanplay-transport -p xtask
PROBE="$(ls "$REPO"/target/release/*audio-rtp* 2>/dev/null | head -1)"
if [ -z "$PROBE" ]; then
    echo "no audio rtp probe was built; the packetiser's probe has to exist for this to mean anything" >&2
    exit 1
fi

# Every arm carries the same arguments, and the commit only when git can say what
# it is: a probe told a hash nobody read out of a repository would record a
# provenance that is worse than an absent one.
ARM_ARGS=(--seconds "$SECONDS_TO_RUN" --frame-ms 5)
COMMIT_ARGS=()
if COMMIT="$(git -C "$REPO" rev-parse --short HEAD 2>/dev/null)"; then
    COMMIT_ARGS=(--commit "$COMMIT")
fi

# A port of its own, away from video on 5004 and input on 5006. The plan wants the
# streams on separate sockets so that pushing out a video access unit cannot sit in
# front of an audio packet, and a gate that shared a port would be measuring
# something the product will never do.
LOCAL_IP="$(ifconfig en0 2>/dev/null | awk '/inet /{print $2; exit}')"
WIN_IP="$(ssh -G "$HOST" 2>/dev/null | awk '/^hostname /{print $2}')"

# The radio arm is skipped rather than faked when the host is not there. Declaring a
# measurement unavailable costs nothing; producing one from a path that never left
# this machine cost an hour earlier today, when a datagram addressed to this
# machine's own routable address was short-circuited onto loopback and the gate
# reported the radio losing nothing. Counted at the time: 1000 packets to the local
# address moved lo0 by 1016 and en0 by 138, while the same 1000 to the router moved
# en0 by 1091.
RADIO=no
if [ -n "$LOCAL_IP" ] && [ -n "$WIN_IP" ] &&
    ssh -o BatchMode=yes -o ConnectTimeout=5 "$HOST" 'echo alive' >/dev/null 2>&1; then
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
"$PROBE" --bind "0.0.0.0:$PORT" --send-to "127.0.0.1:$PORT" --arm loopback \
    --envelope "$OUT/loopback.json" "${ARM_ARGS[@]}" ${COMMIT_ARGS[@]+"${COMMIT_ARGS[@]}"} \
    >"$OUT/loopback.out" 2>&1 || true
echo "arm       loopback done"
sleep 1
# The negative control, and it belongs on the loopback arm rather than the radio one.
# Loopback is where this gate calls a lost packet, a duplicate or a reordering a defect
# in the code, because there is no wire to blame; a criterion stated that strongly has
# to be shown capable of firing, and until now it never had been. udp-fault relays the
# same path with a seed, so the arm is reproducible and its faults are known rather
# than hoped for, and the seed is in the document the relayed end writes.
#
# Two probes, not one. udp-fault decides which way a datagram is going by comparing its
# source against --forward, so a probe that sends from the socket it also receives on
# looks like the reply direction and every datagram goes nowhere. Wired that way first,
# this control reported 2000 lost of 2000 and passed - a control that fires because the
# harness is broken proves nothing about the criteria it is there to exercise, which is
# why what it received is held against what the loopback arm received further down.
#
# Only the relayed end writes a document. The sender saw an ordinary run, its own
# document would pass, and a control credited with that pass would be a control half of
# which cannot fail.
FAULT="$REPO/target/release/udp-fault"
SEED=20250815
if [ -x "$FAULT" ]; then
    "$FAULT" --listen "0.0.0.0:$((PORT + 3))" --forward "127.0.0.1:$((PORT + 2))" \
        --loss 2.0 --duplicate 1.0 --reorder 1.0 --reorder-hold-ms 8 --seed "$SEED" \
        >"$OUT/control.relay" 2>&1 &
    relay=$!
    trap 'kill "$relay" 2>/dev/null || true' EXIT INT TERM
    "$PROBE" --bind "0.0.0.0:$((PORT + 2))" --receive-only --arm control --seed "$SEED" \
        --envelope "$OUT/control.json" --seconds "$((SECONDS_TO_RUN + 5))" --frame-ms 5 \
        ${COMMIT_ARGS[@]+"${COMMIT_ARGS[@]}"} >"$OUT/control.out" 2>&1 &
    receiver=$!
    sleep 0.5
    "$PROBE" --bind "0.0.0.0:$((PORT + 4))" --send-to "127.0.0.1:$((PORT + 3))" \
        "${ARM_ARGS[@]}" >"$OUT/control.sender.out" 2>&1 || true
    wait "$receiver" 2>/dev/null || true
    kill "$relay" 2>/dev/null || true
    echo "arm       control done (seed $SEED)"
    sleep 1
else
    echo "control   udp-fault is not built, so the loopback criteria are unproven this run"
fi

if [ "$RADIO" = yes ]; then
    # The radio arm needs two machines. The receiver runs on the host, in the
    # interactive session, and the sender is here; the datagrams therefore leave this
    # radio, cross the router and arrive at a different network stack, which is the
    # only arrangement in which a loss figure is the radio's.
    #
    # Two documents, one per end, because neither end can state the other's numbers.
    # What arrived is the host's to say and what went out is this machine's, and the
    # loss figure below is the one place they are put together.
    WIN_TASK=lanplay-audio-rtp WIN_TIMEOUT=$((SECONDS_TO_RUN + 120)) "$REPO/tools/win-session.sh" \
        'C:\Users\luque\audio-rtp.log' \
        "target\\release\\audio-rtp-probe.exe --bind 0.0.0.0:$PORT --receive-only --arm radio-receiver --envelope C:\\Users\\luque\\audio-rtp-radio.json --seconds $((SECONDS_TO_RUN + 15))" \
        >"$OUT/radio.out" 2>&1 &
    receiver=$!

    for _ in $(seq 1 40); do
        "$REPO/tools/win-ssh.sh" \
            'powershell -NoProfile -Command "(Get-Process audio-rtp-probe -ErrorAction SilentlyContinue).Count"' \
            2>/dev/null | tr -d '\r ' | grep -q '^[1-9]' && break
        sleep 0.5
    done
    echo "receiver  listening on the host"

    "$PROBE" --bind "0.0.0.0:$((PORT + 1))" --send-to "$WIN_IP:$PORT" --arm radio-sender \
        --envelope "$OUT/radio-sender.json" "${ARM_ARGS[@]}" ${COMMIT_ARGS[@]+"${COMMIT_ARGS[@]}"} \
        >"$OUT/radio.sender.out" 2>&1 || true
    wait "$receiver" 2>/dev/null || true

    # The host end's document, brought back by itself: win-session gives a scheduled
    # task no stdout worth reading, so the envelope crosses as a file or not at all.
    # What went wrong is kept rather than discarded, because the refusal downstream can
    # only say that the document is not here and not why.
    scp -q "$HOST:C:/Users/luque/audio-rtp-radio.json" "$OUT/radio.json" \
        2>"$OUT/radio.transfer" || true
    echo "arm       radio done"
fi

# ---- verdict -----------------------------------------------------------------

status=0
refused=0

# Decides one document and prints the block a person reads. Two and one are
# different answers and are kept apart: `xtask verdict` refuses a document whose
# criterion had no number to read, and an arm nobody could decide is not an arm that
# disagreed. Reading the refusal as a failure would put this gate back to claiming it
# had tested something it had not.
decide() {
    local document="$1" what="$2"
    echo
    if [[ ! -s "$document" ]]; then
        echo "REFUSE $what wrote no document, so this arm produced no result to decide;"
        echo "       what it printed is beside it in $OUT"
        return 2
    fi
    local code=0
    "$XTASK" verdict "$document" || code=$?
    return "$code"
}

judge() {
    local code=0
    decide "$1" "$2" || code=$?
    if [[ "$code" -ge 2 ]]; then
        refused=1
    elif [[ "$code" -ne 0 ]]; then
        status=1
    fi
}

# The same parser every other number here comes through, and the word `absent`
# rather than an empty string when a document reports no such observation: an empty
# string compared against a target is how a harness reads a number that is not there
# as a zero and calls it a match.
observation() {
    "$XTASK" verdict --observation "$1" "$2" 2>/dev/null || echo absent
}

judge "$OUT/loopback.json" "the loopback arm"
if [ "$RADIO" = yes ]; then
    judge "$OUT/radio-sender.json" "the radio arm's sending end"
    judge "$OUT/radio.json" "the radio arm's receiving end"
fi

echo
echo "control   the arm below must fail, and this gate fails if it does not"

if [ ! -x "$FAULT" ]; then
    echo
    echo "REFUSE udp-fault is not built, so no control ran and a clean loopback arm is a figure"
    echo "       nobody has shown this gate capable of failing"
    refused=1
else
    control_code=0
    decide "$OUT/control.json" "the control arm" || control_code=$?
    if [[ "$control_code" -eq 0 ]]; then
        echo
        echo "FAIL the control passed. udp-fault dropped, duplicated and reordered datagrams on"
        echo "     the loopback path at seed $SEED and this gate read the arm as clean, so its"
        echo "     loopback criteria cannot fail and every clean loopback arm it has ever passed"
        echo "     meant nothing"
        status=1
    elif [[ "$control_code" -ge 2 ]]; then
        echo
        echo "REFUSE the control could not be decided, so it has not been shown to fail and the"
        echo "       criteria it is aimed at stand exactly where a gate with no control at all"
        echo "       leaves them"
        refused=1
    else
        # A control that failed is not yet a control that worked. This one fired on
        # losing 2000 datagrams of 2000 to a relay pointed the wrong way, certifying
        # criteria it never reached, so the numbers come back through the same
        # evaluator and are held against what a seeded relay - and only a relay that
        # actually carried the stream - produces.
        #
        # The relay is told to drop two per cent, duplicate one and reorder one, so a
        # working control receives about ninety-eight per cent of what the loopback arm
        # received and each of the three faults appears in the hundreds of datagrams
        # this run carries. The failure being excluded delivered none at all. Half of
        # the loopback count separates the two by forty-eight points either way, which
        # is why the floor is there rather than tight.
        awk -v received="$(observation packets_received "$OUT/control.json")" \
            -v clean="$(observation packets_received "$OUT/loopback.json")" \
            -v missing="$(observation packets_missing "$OUT/control.json")" \
            -v duplicates="$(observation duplicates "$OUT/control.json")" \
            -v reordered="$(observation reordered "$OUT/control.json")" \
            -v seed="$SEED" '
        function numeric(text) { return text ~ /^-?[0-9]+([.][0-9]+)?$/ }
        BEGIN {
            held = 1
            if (!numeric(received) || !numeric(clean) || received * 2 < clean) {
                printf "  the control received %s datagrams where the loopback arm received %s, so the\n", received, clean
                printf "  relay carried nothing worth breaking and the arm failed on the harness rather\n"
                printf "  than on the criteria it is aimed at\n"
                held = 0
            }
            if (!numeric(missing) || missing + 0 == 0) {
                printf "  the control lost %s packets, so the criterion forbidding loss on a lossless\n", missing
                printf "  path was never reached\n"
                held = 0
            }
            if (!numeric(duplicates) || duplicates + 0 == 0) {
                printf "  the control saw %s duplicates, so the criterion forbidding them was never\n", duplicates
                printf "  reached\n"
                held = 0
            }
            if (!numeric(reordered) || reordered + 0 == 0) {
                printf "  the control saw %s reordered datagrams, so the criterion forbidding them was\n", reordered
                printf "  never reached\n"
                held = 0
            }
            if (held) {
                printf "\n  CONTROL the three criteria loopback states about a path with no wire to blame\n"
                printf "          disagreed, on a stream the relay really carried: %s missing, %s\n", missing, duplicates
                printf "          duplicated and %s reordered of %s that arrived, at seed %s\n", reordered, received, seed
                exit 0
            }
            printf "\nFAIL the control failed, but not on what it was aimed at, so the loopback criteria\n"
            printf "     are as unproven as before and the numbers above are the harness\n"
            exit 1
        }' || status=1
    fi
fi

# ---- findings ----------------------------------------------------------------
#
# Above the verdict and voting on nothing. What the radio loses is what this phase
# owes, and a failing criterion beside it does not make it uninteresting.

echo
if [ "$RADIO" = yes ] && [ -s "$OUT/radio-sender.json" ] && [ -s "$OUT/radio.json" ]; then
    # The loss figure comes out of the host's own sequence accounting rather than out
    # of differencing the two ends, and the first run after this gate was migrated is
    # why. It sent 2000 and the host received 1740 with no sequence gap at all: the
    # 260 are a contiguous head that left before the host's socket was listening, and
    # subtracting them would have reported 13 per cent loss on a link that dropped
    # nothing the host could see. A6 reached the same conclusion for the same reason -
    # a count and a span taken over different intervals is the defect that once put
    # 150 ppm into a measurement whose whole subject was parts per million.
    #
    # The two counts are still both read, because their difference is worth stating as
    # what it is. Neither end can tell a packet sent before the far end was listening
    # from one lost before its first arrival, so the difference is named and not
    # counted, and the percentage is taken over the stream the host's window covered.
    awk -v sent="$(observation packets_sent "$OUT/radio-sender.json")" \
        -v received="$(observation packets_received "$OUT/radio.json")" \
        -v missing="$(observation packets_missing "$OUT/radio.json")" \
        -v reordered="$(observation reordered "$OUT/radio.json")" \
        -v duplicates="$(observation duplicates "$OUT/radio.json")" '
    function numeric(text) { return text ~ /^-?[0-9]+([.][0-9]+)?$/ }
    BEGIN {
        if (!numeric(sent) || !numeric(received) || !numeric(missing) || received + 0 == 0) {
            printf "  FINDING the two ends of the radio arm did not both state a packet count, so the\n"
            printf "          loss figure this phase owes is not in this run\n"
            exit 0
        }
        observed = received + missing
        outside = sent - observed
        if (outside < 0) outside = 0
        printf "  FINDING the radio lost %d of %d packets, %.3f %%, with %s reordered and %s\n", missing, observed, 100 * missing / observed, reordered, duplicates
        printf "          duplicated, counted over the sequence numbers the window there covered\n"
        printf "  FINDING this end sent %d and the host observed %d of them; the %d outside its\n", sent, observed, outside
        printf "          window left before its socket was listening or were lost before its first\n"
        printf "          arrival, and no end can tell those two apart, so they are named here and\n"
        printf "          not counted as loss\n"
    }'
else
    echo "  FINDING loss over the air is NOT measured here: the host was unreachable, so only the"
    echo "          stream's arithmetic and the loopback path were tested. The figure this phase"
    echo "          owes is a second endpoint's, and A6 is where it arrives."
fi

echo
# The failure first when a run produced both, because an arm that was decided and
# disagreed says more about the path than one nobody could decide at all.
if [[ "$status" -ne 0 ]]; then
    echo "FAIL an arm did not hold what it stated, and the blocks above say which and why"
    exit 1
fi
if [[ "$refused" -ne 0 ]]; then
    echo "REFUSE an arm stated a criterion nobody could decide, named above, so this run says"
    echo "       nothing either way about the criteria it could not read"
    exit 2
fi
if [ "$RADIO" = yes ]; then
    echo "PASS one Opus frame is one datagram, the timestamp counts samples exactly, loopback"
    echo "     is lossless, and what the radio loses is now a number instead of an assumption"
else
    echo "PASS on correctness only: one Opus frame is one datagram, the timestamp counts"
    echo "     samples exactly, and loopback is lossless. The radio figure is owed, not given."
fi
