#!/usr/bin/env bash
# A8: which jitter target this path should pay for every frame it ever plays.
#
# The target is not a tuning knob, it is a bill. Every millisecond of it is added to the
# mouth-to-ear latency of every frame for the life of the product, and it is paid whether
# or not the link ever needs it. So the answer is the smallest target that holds
# continuity, and never the largest that reports no faults: if five milliseconds loses
# audio and ten, fifteen and twenty do not, the answer is ten, and a run that recommended
# twenty because twenty also looked clean would have charged the listener ten milliseconds
# for nothing.
#
# The deciding counter is the one A6 turns on, for the reason A6 states: continuity, the
# per-channel samples the playout cursor travelled against the ones the producer actually
# deposited, with a gap concealment credited because the waveform continued across it and
# an underrun refused because nothing of the stream was there. Underruns alone cannot
# decide this. A6's own control broke a fifth of the audio and reported zero underruns
# throughout, because the concealer kept the device fed the whole way, and an instrument
# judging targets on underruns would have called that arm clean.
#
# ## What is hard here, and it is not the measurement
#
# Four targets cannot be measured at once. The target is fixed when the buffer is built,
# so each one is its own run, the runs are minutes apart, and the link does not stand
# still between them: a 1 Hz CoreWLAN trace during A6's ten-minute arm read -70 dBm at the
# start and -78 dBm two minutes later, at 288 to 432 Mbps, and the same association has
# run at -52 dBm and 1200 Mbps on a better evening. A sweep that ignores that ranks the
# link's mood and prints it as a property of the buffer.
#
# Worse, and this is the term that decides the design. The playout deadline of the whole
# stream is anchored on the arrival of one datagram - the first - plus the target, and
# every later frame's deadline is arithmetic from there. So the anchor's own delay is added
# to or taken off the target for the entire run: a first datagram that turned up late buys
# the rest of the arm that much extra margin, and one that arrived early spends it. The
# effective target of an arm is therefore the nominal one plus a draw from the link's own
# jitter, and the size of that draw is measurable. It is the negated median arrival delay,
# because a frame is late exactly when its delay relative to the anchor exceeds the target,
# so the margin the median frame enjoyed is what the sweep is really varying. A6's two
# clean arms were both configured at 10 ms and reported -11.7 ms and -17.7 ms: an effective
# margin differing by 5.97 ms between two arms of one gate at one nominal target, which is
# more than the whole 5 ms step this sweep is asked to resolve.
#
# That is not a reason to give up on A8, and it is not something a longer arm fixes - the
# anchor offset is a constant for the whole of an arm, so it does not average out inside
# one. It is a reason for the arrangement below, and for a gate that refuses when the draw
# turns out to be worth more than the step.
#
# ## Two minutes an arm, and why not one and why not five
#
# The arm has to sample the tail it is being used to rank, and the tail is where all the
# information is: at a 10 ms target A6 measured p95 still 10.2 ms in hand and p99 only
# 10.8 ms late, so what separates a 10 ms target from a 20 ms one is the frames arriving
# between 10 and 20 ms past their moment - well under one per cent of the stream. At the
# wire's 200 datagrams a second, two minutes is 24000 of them, so that fraction is tens of
# events: countable, and countable is the whole requirement, since a maximum read as a
# frequency is how one 4.3 ms event in this project once argued for building something that
# turned out to be 0.007 per cent of a minute.
#
# One minute was A6's short arm and it undercounts. Its 60 s arm put 1.68 per cent of its
# datagrams past their moment where its 600 s arm put 2.08, so a minute reads a fifth low
# on the very quantity being ranked, and halving the count again puts the deciding events
# into single figures where Poisson noise alone is a third of them.
#
# Five minutes would sample the tail better and rank worse. Thirteen arms of 300 s is over
# an hour of measurement, and this link moved 8 dB inside a single two-minute window during
# A6; an hour of it is not one link, so the longer arm would buy tail resolution with the
# comparability the ranking is made of, and the gate would spend the hour and then refuse.
# Two minutes is where the tail stops being thin before the link has had time to become a
# different link. It is also `radio-sample`'s own default window, so every arm carries
# exactly one trace of its own conditions.
#
# Measured, an arm costs its two minutes plus the thirty seconds the sender outlasts it plus
# about fifteen for the scheduled-task round trips - a 20 s arm came to 55 s of wall clock -
# so the thirteen arms of a three-pass sweep and its control are about thirty-six minutes.
#
# ## The arrangement, and the three that were rejected
#
# Every target is measured three times, and the incumbent's three runs are the instrument's
# own noise floor: three arms configured identically, taken at three moments spread through
# the sweep, whose disagreement is exactly the anchor draw plus whatever the link did. No
# difference between two targets smaller than that disagreement is a difference, and if the
# disagreement reaches the 5 ms step then nothing here can rank anything and the run says
# so.
#
# The passes are ordered 5, 10, 15, 20 and then exactly reversed, 20, 15, 10, 5, because a
# link that degrades through the sweep adds a term proportional to an arm's position and
# reversing the order flips the sign of that term for every target, so the mean over the
# two passes cancels a monotone drift to first order and does so by construction rather
# than by hoping. Two passes of four positions balance perfectly; a third pass cannot be
# balanced as well, so it is ordered 15, 20, 5, 10 - neither of the first two - and its job
# is to give each target a third draw and to keep a step change halfway through the sweep
# from landing on the same arm in every pass.
#
# Repeating the whole sweep and ranking within each pass was the first candidate and is
# what this is, with the ordering designed instead of repeated: three identical passes give
# each target the same position every time, so a drift and the target order stay perfectly
# confounded however many passes are run.
#
# Interleaving at a finer grain than an arm was rejected because it cannot be built from
# outside. The target is fixed at construction, so switching targets means restarting the
# receiver, and a fifteen-second slice of a link whose deciding events are a few per
# thousand samples the tail too thinly to tell two targets apart - which is the trap
# TASKS.md names, where forty seconds could not measure a phase that swept its period in
# two hundred.
#
# Bracketing every candidate with a reference arm was rejected as a strictly worse spend of
# the same time: it buys locality in the drift estimate, which the reversed ordering
# already handles, and it pays for it by measuring three of the four targets once each, so
# a single unlucky anchor draw inverts a rank with nothing to catch it. The incumbent is
# the reference here, and it is bracketing every candidate anyway - it appears in every
# pass.
#
# The radio is sampled through every arm rather than before and after the sweep, at 1 Hz
# through CoreWLAN, which reads the association and does not scan; `system_profiler` is the
# obvious instrument and takes the radio off channel to fill in neighbouring networks,
# which once turned an 8 ms delivery interval into 133 ms at p99 and manufactured the very
# bunching the experiment was hunting. Thirteen arms over forty minutes cannot be attributed
# to one reading taken at each end, so each arm carries the conditions it was measured
# under and the comparison can be refused on them.
#
# ## When this gate refuses, in numbers
#
# Refusal is neither a pass nor a failure and it exits 2. Five tests decide it, and they are
# consulted only where a comparison is actually being made: a sweep in which every target
# lost audio, or one in which the smallest held it, has answered without comparing anything,
# and refusing such a run on the link's behaviour would be withholding an answer already in
# hand. What needs the arms to be comparable is a boundary somewhere in the middle, where
# the position of the boundary is the whole result.
#
# Three of the five are exact rather than statistical, and they are stated in the units the
# decision is made in - each arm's continuity, held or broke - rather than in a hole fraction
# averaged over passes. That last point cost an earlier version of this file its verdict
# section: on the run committed with it, pass one lost between 21 and 37 per cent where
# passes two and three lost between 0.8 and 2.6, and a per-target mean over those three
# reported every target as losing fourteen per cent when no arm of any of them ever did.
#
# A target whose own three arms disagree, one holding continuity and another not, has had
# its outcome decided by the moment its arm ran at rather than by its target, so the
# boundary is a boundary in time.
#
# A pattern that is not a single step in the target: raising the target delays playout
# strictly, so every frame late at the smaller target is late at the larger, and a smaller
# target holding continuity where a larger one broke is arithmetically impossible on one
# link with one anchor.
#
# A pass whose measured margins do not rise with its nominal targets did not sweep what it
# was asked to. This is the anchor draw caught in the act, and it is not hypothetical: in
# pass one of the committed run the 5 ms arm ran with 28.05 ms of margin and the 20 ms arm
# with 20.50, because the datagram that anchored the first turned up that much later than a
# typical one. Whatever the holes of that pass came out as, it did not compare 5 against 20.
#
# Then the instrument's resolution against the step it was asked to resolve: the arms at the
# incumbent target must agree on effective margin to better than 5 ms, the gap between
# adjacent targets. A6 measured 5.97 ms of disagreement between two such arms and the
# committed run here measured 2.57 ms across three, so this is a live test either way.
# Averaging the draws and claiming the mean is resolved to a fraction of their spread was
# rejected: three samples cannot establish the distribution that arithmetic assumes, and the
# house rule here is to refuse rather than to model.
#
# And a link that moved between arms: the arms' mean PHY rates must stay inside a factor of
# two of each other and their mean signal inside 8 dB. Airtime per datagram is inversely
# proportional to PHY rate and airtime is the mechanism that produces the tail a target is
# chosen against, so a factor of two is two different links rather than one link breathing.
# The 8 dB is A6's own trace, which moved that much inside a single two-minute arm: a
# spread between arm means wider than the movement inside an arm is arms sitting on
# different links.
#
# ## The negative control, and the one that was rejected
#
# The control is the largest target in the sweep, 20 ms, behind `tools/udp-fault` holding
# every datagram for 400 ms every 2 s at a fixed seed. It must fail, and this gate fails if
# it passes: a deciding counter that cannot come out negative makes every ranking above it
# worthless, which is why the control runs first and aborts the sweep rather than being
# discovered forty minutes later. Twenty times the largest target under test, so it is not
# a margin question and no candidate here could have held it. A hold does not move a
# frame's timestamp, so the cursor walks past the whole burst while it is in flight and
# every frame of it is discarded on arrival.
#
# The control has to fail for that reason and not because something else broke, so it also
# has to fail by the right amount. Four hundred milliseconds held out of every two thousand
# is a fifth of the stream past its moment, and A6's control measured exactly that: 2522 of
# 12005 datagrams late and a continuity hole of 605280 samples of 2880000, 21.0 per cent,
# against 1.7 per cent on its clean arm. A control reporting far less than half that duty
# cycle is a fault that did not reach the path, which is a harness that broke rather than a
# criterion that fired, and it is refused rather than counted as the control this gate owes.
#
# A target of zero was the obvious control and is wrong, which took two seconds to find out
# and is recorded here because the near-miss is the same one that disqualified codec-gate's
# first candidate. The buffer quantises its target to whole frames and floors it at one,
# deliberately, since a buffer with no target is not a jitter buffer but a queue that
# underruns on the first packet a microsecond late. Measured through the jitter probe, which
# builds the same buffer the receiver does: `--target-ms 0` and `--target-ms 1` both report
# `target ms 5` and a 25 ms ceiling over 7 slots, identical to `--target-ms 5`. So a zero
# target is not a control at all, it is a fourth copy of the smallest candidate arm, and on
# a link where 5 ms holds continuity the control would hold it too and this gate would fail
# on its own instrument. It is refused before a run rather than after, and it is refused for
# aliasing a candidate rather than for being rejected by the receiver - the receiver accepts
# it silently, which is what made it worth measuring.
#
# ## What this gate does not cover
#
# It chooses a target against this link's tail and says nothing about any other link: a
# target that holds continuity here has not been shown to hold it at -52 dBm, and one that
# fails here has not been shown to fail there. It corrects no drift, which is A7, and it
# does not exercise the buffer's ceiling, which no delay can breach and only a sink slower
# than its source can. It has no opinion on the latency the winning target costs beyond
# stating it, because the whole of the argument for choosing the smallest is that the number
# is a bill and not a measurement.
#
# One provenance wrinkle worth knowing before somebody reads an envelope from here and
# places it wrongly: the receiver writes its own gate name into every document it produces
# and takes no flag for it, so every arm of this sweep is filed under `audio-e2e-gate`. What
# places a document is therefore its arm, which is why each one here is named for its target
# and its pass, and why the record this gate decides from is `arms.csv` in its own output
# directory rather than the documents' own labels.
#
# usage:
#   tools/jitter-target-sweep.sh [seconds-per-arm] [passes]
#   VERDICT_ONLY=1 OUT=<a previous run's directory> tools/jitter-target-sweep.sh
#
# The second form re-decides a sweep that already happened, from the record it left. It is
# there because the arithmetic above is the part of a harness most likely to be wrong - the
# instruments in this project have been wrong more often than the code they measured - and
# a verdict that can only be exercised by spending forty minutes of radio is a verdict
# nobody exercises.
#
# exit 0  a target was chosen, and it is the smallest that held continuity
# exit 1  no target held continuity, or the control held every criterion
# exit 2  refused: the arms were not comparable, or the run was in no position to measure

set -euo pipefail

ARM_S="${1:-120}"
PASSES="${2:-3}"

# Away from video on 5004, input on 5006, the RTP probe on 5008, A6 on 5012 and 5013 and
# the jitter probe on 5108, so an interrupted run of one gate cannot be measured by another.
PORT=5014
RELAY_PORT=5015
FRAME_MS=5
# The four TASKS.md names, and the step between them is the resolution this gate has to
# beat before it may rank anything.
TARGETS="5 10 15 20"
STEP=5
# The incumbent, which is what A6 measured at and therefore what a candidate has to beat to
# be worth changing. Its repeats are the noise floor every comparison below is read against.
REFERENCE=10
# Ascending, then exactly reversed so a monotone drift cancels between the two, then an
# order that is neither.
ORDERS=("5 10 15 20" "20 15 10 5" "15 20 5 10")

# The output device this end renders through, named rather than inherited. The receiver
# refuses a device that does not mix at 48000 Hz stereo, because a converter on this path
# would make every figure below a statement about the converter, and the default on this
# Mac is a pair of Bluetooth headphones at 44100 Hz that reconnect on their own - which
# refused a ten-minute A6 measurement thirty-seven seconds after it started. Overridable
# because a machine running in English calls the built-in output "MacBook Pro Speakers".
DEVICE="${AUDIO_OUTPUT_DEVICE:-Altavoces del MacBook Pro}"

# The control's fault, and the seed it comes from: a fault nobody can reproduce turns a
# failure into a rumour. A6's seed, so its arm and this one break the same way.
SEED=20250815
STALL_MS=400
STALL_EVERY_MS=2000
CONTROL_TARGET=20
# Half the fault's duty cycle. Four hundred milliseconds held of every two thousand is a
# fifth of the stream past its moment, and A6's control measured 21.0 per cent; an arm
# reporting less than half of that failed for a reason other than the fault.
CONTROL_FLOOR_PCT=10

# What movement makes the arms uncomparable. Each is derived in the header above and each
# is printed with its measured value when it fires.
RATE_FACTOR=2.0
RSSI_SPREAD_DB=8

# How much longer the host end runs than the window being measured. One arithmetic with the
# receiver's first-packet wait rather than two guesses: the receiver anchors its window on
# the first datagram and ends a measured span later, so the sender has to outlast it by
# however long that first datagram took. The wait gives up at twenty seconds and the slack
# is thirty, so the tail margin is never less than ten. A6 used sixty against a thirty
# second wait; thirteen arms make that slack thirteen minutes of nothing, and the wait is
# only long because a scheduled task takes seconds to reach the desktop.
FIRST_PACKET_WAIT=20
SENDER_SLACK=30

REPO="$(cd "$(dirname "$0")/.." && pwd)"
# Stamped and never cleared: six minutes of measurement was lost once to a gate that
# emptied its output directory on startup, so re-running it to re-read a verdict deleted
# the verdict.
OUT="${OUT:-/tmp/jitter-target-sweep/$(date +%Y%m%d-%H%M%S)}"
HOST="${WIN_HOST:-windows}"
VERDICT_ONLY="${VERDICT_ONLY:-0}"

XTASK="$REPO/target/release/xtask"
RECEIVER="$REPO/target/release/audio-e2e-receiver"
FAULT="$REPO/target/release/udp-fault"
RADIO="$REPO/target/release/radio-sample"
WIN_REPO='C:\Users\luque\lanplay-rs'
# libopus is vendored C and is built by the cmake inside Visual Studio BuildTools, which is
# not on the host's PATH by default. Nothing else on the host needs it and it cannot be
# cross-compiled from here, so the sender is built where it runs.
CMAKE_BIN='C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin'

ARMS="$OUT/arms.csv"

mkdir -p "$OUT"
echo "results   $OUT"

# Neither a pass nor a failure. A gate that reported a verdict for a run it was in no
# position to take would be stating a criterion nobody tested, and an agent reading the
# result later cannot tell that from a real one.
refuse() {
    echo
    echo "REFUSE $*"
    exit 2
}

# Anything this gate starts, this gate ends, including on an interrupt and including on the
# other machine. A relay left holding a port makes the next thing to bind it fail for a
# reason that has nothing to do with it, and a sender left running on the host holds the
# loopback client and the executable the next build has to replace.
#
# On the host it is the scheduled tasks this run created that are ended, by name, never an
# image: one workspace is shared by everything driving that machine, and `taskkill /IM
# tone-source.exe` shoots whichever sibling happens to be measuring with it.
#
# And this is weaker than it looks, which is why the tone and the sender are each bounded by
# their own argument rather than trusted to this. Measured on the host: a tone launched
# through a scheduled task for 300 s and ended after 25 reported "ha finalizado
# correctamente" and was still playing four seconds later. `schtasks /end` ends the wrapper
# the task runs and not the process the wrapper started, so what actually stops the host
# half of an interrupted arm is the `--seconds` it was given.
HOST_TASKS=""
cleanup() {
    pkill -f "udp-fault --listen 0.0.0.0:$RELAY_PORT" 2>/dev/null || true
    pkill -f "audio-e2e-receiver --bind 0.0.0.0:$PORT" 2>/dev/null || true
    local task
    for task in $HOST_TASKS; do
        "$REPO/tools/win-ssh.sh" "schtasks /end /tn $task" >/dev/null 2>&1 || true
    done
}
trap cleanup EXIT INT TERM

# Every number the verdict reads, out of one envelope and through the one parser, set as
# variables rather than returned. A refusal cannot be raised inside `$( )`: the `exit` would
# end the subshell and leave the caller holding an empty string, which is the silent failure
# this whole arrangement exists to prevent.
RECEIVER_KEYS="rtp_received rtp_expected rtp_lost rtp_late rtp_off_grid plc_frames \
frames_played render_callbacks render_underruns jitter_underruns jitter_overruns \
jitter_occupancy_p50_ms jitter_occupancy_p95_ms samples_expected samples_played \
continuity_hole arrival_delay_p50_ms arrival_delay_p95_ms arrival_delay_p99_ms \
arrival_delay_max_ms"

# The numbers a document is not a result without, insisted on before anything is decided
# from it. Belt and braces with `xtask verdict`, which already refuses a document holding a
# criterion it could not read, and deliberately so: this names the keys this gate in
# particular turns on, which the general answer cannot know. A document stating no
# continuity check and no arrival delay would parse, would decide whatever else it stated,
# and would never mention the two numbers this phase exists to compare.
insist() {
    local document="$1"
    shift
    local name
    for name in "$@"; do
        "$XTASK" verdict --observation "$name" "$document" >/dev/null ||
            refuse "$document does not state $name, so this end reported something other than the run this gate asked for"
    done
}

# A population of zero is absence wearing the other hat: a hole of zero samples out of zero
# expected is what a path carrying nothing looks like, and it is the single most common way
# a gate here has lied.
insist_positive() {
    local document="$1" name="$2" value
    value="$("$XTASK" verdict --observation "$name" "$document")" ||
        refuse "$document does not state $name, which is the population a zero would be measured over"
    awk -v value="$value" 'BEGIN { exit (value > 0 ? 0 : 1) }' ||
        refuse "$document reports $name as $value, so every zero in it is an absence and this run measured nothing"
}

# Decides one end of one arm and returns what `xtask` decided: 0 held, 1 did not, 2 could
# not be decided at all. The third is a refusal wherever it appears, including in the arm
# that is supposed to fail, because a criterion nobody could evaluate is not a criterion
# anybody observed disagreeing.
decide() {
    local document="$1"
    if [ ! -s "$document" ]; then
        refuse "$document was never written, so this arm produced no result to decide"
    fi
    local code=0
    "$XTASK" verdict "$document" >"$document.verdict" 2>&1 || code=$?
    if [ "$code" -ge 2 ]; then
        cat "$document.verdict"
        refuse "$document was not decided: either it would not parse or a criterion in it had" \
            "nothing to read, and whichever it was is named above"
    fi
    return "$code"
}

if [ "$VERDICT_ONLY" != "1" ]; then

    case "$PASSES" in
        '' | *[!0-9]*) refuse "the pass count must be a whole number of passes" ;;
    esac
    [ "$PASSES" -ge 1 ] || refuse "a sweep of no passes measures nothing"

    # ---- what this run is in a position to measure ---------------------------
    #
    # Asked of `xtask gates --runnable`, not re-derived here. That detector is tested, it
    # reports a requirement nobody could check as unknown rather than as absent, and a
    # second implementation of the same four questions in shell is a second set of answers
    # that can disagree with the first.

    cargo build --release -q -p xtask
    "$XTASK" gates --runnable --json --host "$HOST" >"$OUT/environment.json" ||
        refuse "the environment could not be read, so nothing here knows what it is measuring"

    if ! python3 - "$OUT/environment.json" >"$OUT/preflight.txt" 2>&1; then
        cat "$OUT/preflight.txt"
        refuse "the run was not in a position to measure anything; the line above says which requirement and why"
    fi <<'PY'
import json
import sys

# Anything not `present` stops the run, unknown included: a host that did not answer has
# not been found to lack an endpoint, nobody looked, and a suite that reads unknown as
# absent shrinks without anybody deciding it should.
wanted = {
    "windows-host": "the sender has nowhere to run",
    "radio": "audio addressed to this machine's own address never leaves it; the kernel puts it on loopback",
    "audio-endpoint": "loopback has no endpoint to capture on the host",
    "audio-output": "there is nothing here for CoreAudio to play into",
}
environment = json.load(open(sys.argv[1]))["environment"]
short = []
for requirement, cost in wanted.items():
    found = environment.get(requirement, {"state": "unknown", "why": "the detector said nothing about it"})
    print(f"  {requirement:<16} {found['state']:<8} {found['why']}")
    if found["state"] != "present":
        short.append(f"{requirement} is {found['state']} - {cost}")
if short:
    print("\n" + "\n".join(f"  missing: {reason}" for reason in short))
    sys.exit(1)
PY
    cat "$OUT/preflight.txt"

    # The address the host aims at, read rather than assumed: this is the interface the
    # radio detection above just confirmed traffic can cross.
    LOCAL_IP="$(ipconfig getifaddr en0 || true)"
    [ -n "$LOCAL_IP" ] || refuse "en0 has no address, so there is nothing for the host to send to"
    echo "radio     host -> en0 $LOCAL_IP:$PORT"
    echo "device    rendering through $DEVICE"

    # ---- and whether the link will hold still long enough to rank anything ----
    #
    # The environment probe above answers whether a radio exists. It has no opinion on
    # whether that radio is about to move, and this sweep's whole output is a comparison
    # between arms taken forty minutes apart: A6 was measured twice on the same
    # association, once at -52 dBm and 1200 Mbps and once falling from -70 to -78 at 288 to
    # 432 Mbps, and the second produced a continuity figure 1.8 times the first. Ranking
    # four targets across a link doing that ranks the link.
    #
    # So the projection is asked for once, before any arm runs, from the instrument that
    # measures it rather than from a second reading of the same idea here. A refusal here
    # costs two minutes and ends the run; the alternative costs forty and produces a
    # ranking of the weather.

    if ! "$REPO/tools/radio-preflight.sh" >"$OUT/radio-preflight.txt" 2>&1; then
        sed 's/^/  /' "$OUT/radio-preflight.txt"
        refuse "the link was not steady enough to rank targets across; the preflight above says by how much"
    fi
    grep -E "^(preflight|NOTE)" "$OUT/radio-preflight.txt" | sed 's/^/  /'

    # ---- both ends built where they run -------------------------------------

    cargo build --release -q -p lanplay-audio-render
    cargo build --release -q -p lanplay-udp-fault
    cargo build --release -q -p lanplay-radio-sample
    [ -x "$RECEIVER" ] || refuse "the receiver was not built at $RECEIVER, so this end cannot play anything"
    [ -x "$RADIO" ] || refuse "radio-sample was not built, so the conditions each arm ran under would be assumed rather than recorded"

    "$REPO/tools/win-sync.sh" >/dev/null
    "$REPO/tools/win-ssh.sh" \
        "set \"PATH=%PATH%;$CMAKE_BIN\" && cd $WIN_REPO && cargo build --release -q -p lanplay-audio-codec --bin audio-e2e-sender && cargo build --release -q -p lanplay-tone-source" \
        >"$OUT/host-build.out" 2>&1 ||
        {
            cat "$OUT/host-build.out"
            refuse "the sender did not build on the host; libopus needs the cmake inside BuildTools on the PATH"
        }
    "$REPO/tools/win-ssh.sh" \
        "if exist $WIN_REPO\\target\\release\\audio-e2e-sender.exe (exit 0) else (exit 1)" \
        >/dev/null 2>&1 || refuse "the host has no audio-e2e-sender.exe after a build that reported success"
    echo "built     sender and tone source on the host, receiver here"

    # Stated only when git can say what it is: a run recording a hash nobody read out of a
    # repository has a provenance worse than an absent one. Two shapes of the same thing,
    # because the host end's command line is assembled as a string for cmd and this one is
    # an array, and an empty array is not expandable under `set -u`.
    COMMIT_ARGS=()
    COMMIT_FLAG=""
    if COMMIT="$(git -C "$REPO" rev-parse --short HEAD 2>/dev/null)"; then
        COMMIT_ARGS=(--commit "$COMMIT")
        COMMIT_FLAG="--commit $COMMIT"
    fi

    # ---- the tone ------------------------------------------------------------
    #
    # Loopback delivers nothing while the endpoint is idle, so a tone plays on the host for
    # every arm: without it every arm would capture silence. 997 Hz left against 1997 Hz
    # right, because a frame count cannot tell audio from silence and an equal pair would
    # pass with the channels swapped, so two frequencies are what make channel order
    # provable at the far end rather than assumed.
    #
    # One tone per arm, bounded to that arm, and the first arrangement was one tone for the
    # whole sweep - the same source in the strongest available sense, with the waveform
    # continuing across every arm boundary. It was abandoned on a measurement rather than on
    # taste. `schtasks /end` reports "ha finalizado correctamente" and ends the task's
    # wrapper without touching the process the wrapper started: a tone launched for 300 s
    # and ended after 25 was still playing four seconds later, and still playing when asked
    # again. So a sweep-long tone survives its own gate by however much of its argument is
    # left, which for thirteen arms is up to half an hour of tone at somebody after an
    # interrupt. Bounding each tone to its arm is the only remedy available from outside:
    # what ends it is its own argument and not a kill that does not arrive. Killing by image
    # was rejected for the reason A6 states - one host is shared, and `taskkill /IM
    # tone-source.exe` shoots whichever sibling harness happens to be measuring with it.
    #
    # What is given up is small. Every arm's tone starts before its sender and the
    # receiver's window opens on the sender's first datagram, seconds later, so no arm ever
    # measures a start transient; and the receiver analyses the frequencies it actually
    # played, so that the source was the contract tone is measured per arm rather than
    # assumed across the sweep.
    TONE_S=$((ARM_S + SENDER_SLACK + 20))

    # Whether the tone is playing, asked of the host because nothing else can answer it
    # before an arm starts. By image and not by command line: on a shared host another
    # harness's tone-source plays the same contract tone into the same endpoint and serves
    # this capture as well as its own, and what proves the tone reached the far end is the
    # receiver's own analysis of the samples it played. The command-line form of this
    # question needs a pipe through cmd and PowerShell, which silently returned nothing and
    # refused a run whose sender was up.
    tone_playing() {
        local deadline=$((SECONDS + ${1:-30}))
        while [ "$SECONDS" -lt "$deadline" ]; do
            if "$REPO/tools/win-ssh.sh" \
                'powershell -NoProfile -Command "(Get-Process tone-source -ErrorAction SilentlyContinue).Count"' \
                2>/dev/null | tr -d '\r ' | grep -q '^[1-9]'; then
                return 0
            fi
            sleep 0.5
        done
        return 1
    }

    # ---- one arm ------------------------------------------------------------
    #
    # Nothing here polls the host for the sender. The receiver waits for a first datagram
    # and anchors its window on it, so a slow scheduled task costs the measurement nothing,
    # and whether the sender reached the air is answered by the datagrams themselves rather
    # than by asking a shared machine what is running on it. A receiver that waited its
    # whole first-packet window and saw nothing exits 3, and that is a refusal: no stream,
    # no run.
    #
    # The radio is sampled for the length of the arm, alongside it. The trace is this arm's
    # conditions and the comparability tests are decided on them, so a sweep whose arms sat
    # on different links says so rather than ranking them.
    run_arm() {
        local arm="$1" target="$2"
        shift 2
        local send_to="$LOCAL_IP:$PORT"
        local host_seconds=$((ARM_S + SENDER_SLACK))

        # The tone first, then the sender, then the receiver. The order is the arithmetic:
        # a sender launched before the endpoint is streaming captures an idle device, and
        # loopback on an idle endpoint delivers nothing at all rather than delivering
        # silence.
        local tone_task="lanplay-a8-tone-$arm"
        HOST_TASKS="$HOST_TASKS $tone_task"
        WIN_TASK="$tone_task" WIN_TIMEOUT=$((TONE_S + 180)) \
            "$REPO/tools/win-session.sh" "C:\\Users\\luque\\a8-tone-$arm.log" \
            "target\\release\\tone-source.exe --seconds $TONE_S" \
            >"$OUT/$arm.tone.out" 2>&1 &
        local tone=$!
        tone_playing ||
            refuse "nothing is playing on the host's endpoint before arm $arm, so loopback would capture an idle device and this arm would measure silence"

        if [ "$#" -gt 0 ]; then
            # The relay is on this side of the radio, so the arm measures what the fault
            # does to the receiver and says nothing about the air.
            "$FAULT" --listen "0.0.0.0:$RELAY_PORT" --forward "127.0.0.1:$PORT" \
                --seed "$SEED" "$@" >"$OUT/$arm.relay" 2>&1 &
            send_to="$LOCAL_IP:$RELAY_PORT"
            sleep 0.5
        fi

        # A task name per arm: win-session derives its wrapper from the task name, and two
        # invocations sharing one wrapper overwrite each other's command between the copy
        # and the launch, after which the loser reports a timeout while the winner runs
        # twice.
        local task="lanplay-a8-sender-$arm"
        HOST_TASKS="$HOST_TASKS $task"
        WIN_TASK="$task" WIN_TIMEOUT=$((host_seconds + 180)) \
            "$REPO/tools/win-session.sh" "C:\\Users\\luque\\a8-sender-$arm.log" \
            "target\\release\\audio-e2e-sender.exe --send-to $send_to --seconds $host_seconds --arm $arm --envelope C:\\Users\\luque\\a8-sender-$arm.json $COMMIT_FLAG" \
            >"$OUT/$arm.sender.out" 2>&1 &
        local sender=$!
        echo
        echo "arm       $arm: target $target ms, host sending to $send_to for $host_seconds s"

        "$RADIO" "$ARM_S" 1000 >"$OUT/$arm.radio.csv" 2>"$OUT/$arm.radio.err" &
        local radio=$!

        local code=0
        "$RECEIVER" --bind "0.0.0.0:$PORT" --seconds "$ARM_S" --arm "$arm" --device "$DEVICE" \
            --frame-ms "$FRAME_MS" --target-ms "$target" --first-packet-wait "$FIRST_PACKET_WAIT" \
            --envelope "$OUT/$arm.receiver.json" ${COMMIT_ARGS[@]+"${COMMIT_ARGS[@]}"} \
            >"$OUT/$arm.receiver.out" 2>&1 || code=$?
        case "$code" in
            0) ;;
            2) refuse "the receiver could not serve $DEVICE, so this end was in no position to play anything; its report is in $OUT/$arm.receiver.out" ;;
            3) refuse "no datagram arrived in $FIRST_PACKET_WAIT s, so the sender never reached the air and arm $arm measured a path with nothing on it; its report is in $OUT/$arm.receiver.out" ;;
            *) echo "arm       $arm: the receiver exited $code and its report is in $OUT/$arm.receiver.out" ;;
        esac

        wait "$radio" 2>/dev/null || true
        [ -s "$OUT/$arm.radio.csv" ] ||
            refuse "the Wi-Fi association could not be read during arm $arm, so the conditions it was measured under are unknown and its numbers are not comparable with any other arm's"
        wait "$sender" 2>/dev/null || true
        # The tone outlives the sender by twenty seconds on purpose, and the next arm waits
        # for it rather than starting beside it: two tone-source processes on one endpoint
        # sum into the mix, and an arm whose capture carried twice the level would be
        # measured against tolerances taken on one.
        wait "$tone" 2>/dev/null || true
        pkill -f "udp-fault --listen 0.0.0.0:$RELAY_PORT" 2>/dev/null || true

        # The host end's document, brought back by itself: win-session gives a scheduled
        # task no stdout worth reading, so the envelope crosses as a file or not at all.
        scp -q "$HOST:C:/Users/luque/a8-sender-$arm.json" "$OUT/$arm.sender.json" \
            2>"$OUT/$arm.sender.transfer" || true
    }

    # What each end has to have stated. The capture and encode counts on one side, and on
    # the other the numbers that carry the phase: the stream arrived, audio was decoded
    # rather than concealed throughout, continuity was accounted over a real expectation,
    # and the arrival delays that say what margin this arm actually ran with.
    record_arm() {
        local arm="$1" pass="$2" target="$3"
        local document="$OUT/$arm.receiver.json"
        local sender_document="$OUT/$arm.sender.json"

        insist "$sender_document" capture_packets capture_frames frames_encoded \
            datagrams_sent samples_captured samples_encoded
        local sender_code=0
        decide "$sender_document" || sender_code=$?

        insist "$document" $RECEIVER_KEYS
        insist_positive "$document" samples_expected
        insist_positive "$document" render_callbacks
        insist_positive "$document" rtp_expected
        local code=0
        decide "$document" || code=$?

        # ---- is this arm interpretable at all ----------------------------------
        #
        # A6.1 measured the three deciding counters on a clean run and found them not close
        # but identical: 4587 frames late, 4587 buffer underruns, 4587 concealments, and
        # 4587 x 240 exactly the continuity hole. That is not a coincidence to be admired,
        # it is the pipeline's shape - every late frame is an underrun because nothing else
        # ever starves this buffer, and every underrun is concealed because the concealer is
        # the only thing that fills one - and it makes the identity an instrument.
        #
        # An arm where it stops holding has some other mechanism in it: a starve that was
        # not a late frame, a concealment with no underrun behind it, a hole that does not
        # divide by the frame. Whatever that mechanism is, the arm is no longer measuring
        # the tail this sweep ranks, and a percentage from it reads exactly like one that
        # is. So it is refused rather than noted, and it is checked on every arm including
        # the control, whose relay breaks the path on purpose and must break it in a way
        # that still shows up in these four counters.
        local late underruns concealed hole expected_hole
        late="$("$XTASK" verdict --observation rtp_late "$document")"
        underruns="$("$XTASK" verdict --observation jitter_underruns "$document")"
        concealed="$("$XTASK" verdict --observation plc_frames "$document")"
        hole="$("$XTASK" verdict --observation continuity_hole "$document")"
        expected_hole=$((concealed * FRAME_MS * 48))
        if [ "$late" != "$underruns" ] || [ "$underruns" != "$concealed" ]; then
            refuse "arm $arm reports $late frames late, $underruns underruns and $concealed concealed," \
                "which do not agree, so something starves or conceals in it that is not a late frame" \
                "and its continuity figure is not comparable with the other arms'"
        fi
        # One frame of slack, and only one: the hole is counted in samples against a
        # per-callback expectation, so a run ending mid-callback can round by less than a
        # frame. Anything larger is a second mechanism.
        awk -v seen="$hole" -v want="$expected_hole" -v slack="$((FRAME_MS * 48))" \
            'BEGIN { exit ((seen - want <= slack && want - seen <= slack) ? 0 : 1) }' ||
            refuse "arm $arm conceals $concealed frames, which is $expected_hole samples, against a" \
                "continuity hole of $hole; the two describe different events and the arm is not interpretable"

        local name value
        local -a values=()
        for name in $RECEIVER_KEYS; do
            value="$("$XTASK" verdict --observation "$name" "$document")" ||
                refuse "$document stopped reporting $name between two reads of it"
            values+=("$value")
        done

        # The arm's own conditions, out of its own trace. Mean, and the range beside it,
        # because a mean of a link that fell 8 dB across the arm is a number that describes
        # neither end of it.
        local radio_summary
        radio_summary="$(awk -F, 'NR > 1 && NF >= 5 {
            rssi += $3; rate += $5; n++
            if (lo == "" || $3 < lo) lo = $3
            if (hi == "" || $3 > hi) hi = $3
        }
        END {
            if (n == 0) { print "absent"; exit }
            printf "%.2f,%s,%s,%.1f,%d", rssi / n, lo, hi, rate / n, n
        }' "$OUT/$arm.radio.csv")"
        [ "$radio_summary" != "absent" ] ||
            refuse "arm $arm left a radio trace with no rows in it, so its conditions were not recorded"

        # ---- the depth this arm was handed, as a control on the ranking ---------
        #
        # A7 measured a real drift: this pair of machines fills the buffer at +9.29 ppm
        # referred to the Mac's timebase, closing against +238 samples observed over 1200 s.
        # Over one 120 s arm that is 1.1 ms, which is less than the frame this instrument is
        # quantised to, so drift cannot move an arm's own occupancy by a readable amount.
        # Across thirteen arms and forty minutes it can, and an arm that began with 15 ms of
        # depth was ranked with three frames more headroom than one that began with 5.
        #
        # So the depth is recorded, per arm, at both ends. It decides nothing: A7 is closed
        # and is not reopened by a sweep. It is here so that a target cannot come out ahead
        # on depth it was handed rather than on the target it was testing, and so the
        # question is answerable from the file afterwards instead of being unanswerable.
        local occupancy_control
        occupancy_control="$(awk '/^window /{
            for (i = 1; i < NF; i++) if ($i == "occupancy" && $(i + 1) == "ms" && $(i + 2) == "p50") {
                p50 = $(i + 3)
                if (n == 0) first = p50
                last = p50; n++
                sx += n; sy += p50; sxy += n * p50; sxx += n * n
            }
        }
        END {
            if (n < 3) { print "absent"; exit }
            slope = (n * sxy - sx * sy) / (n * sxx - sx * sx)
            printf "%.1f,%.1f,%.1f,%.2f", first, last, last - first, slope
        }' "$OUT/$arm.receiver.out")"
        [ "$occupancy_control" != "absent" ] ||
            refuse "arm $arm printed fewer than three per-window occupancy rows, so the depth it was" \
                "handed cannot be read and its rank cannot be told from a rank it was given"

        local row
        row="$arm,$pass,$target,$code,$sender_code"
        for value in "${values[@]}"; do
            row="$row,$value"
        done
        echo "$row,$radio_summary,$occupancy_control" >>"$ARMS"
    }

    # ---- the control first --------------------------------------------------
    #
    # Before the sweep and not after it. A deciding counter that cannot come out negative
    # makes every ranking above it worthless, and finding that out forty minutes into a run
    # costs the forty minutes as well as the answer.
    {
        printf 'arm,pass,target_ms,verdict,sender_verdict'
        for key in $RECEIVER_KEYS; do printf ',%s' "$key"; done
        printf ',rssi_mean_dbm,rssi_min_dbm,rssi_max_dbm,rate_mean_mbps,radio_rows'
        printf ',occupancy_start_ms,occupancy_end_ms,occupancy_travel_ms,occupancy_slope_ms_per_window\n'
    } >"$ARMS"

    run_arm control "$CONTROL_TARGET" --stall-ms "$STALL_MS" --stall-every-ms "$STALL_EVERY_MS"
    record_arm control 0 "$CONTROL_TARGET"
    echo "control   udp-fault held every datagram for $STALL_MS ms every $STALL_EVERY_MS ms at seed $SEED,"
    echo "          against the largest target in the sweep, $CONTROL_TARGET ms"

    control_verdict="$(awk -F, '$1 == "control" { print $4 }' "$ARMS")"
    control_hole="$("$XTASK" verdict --observation continuity_hole "$OUT/control.receiver.json")"
    control_expected="$("$XTASK" verdict --observation samples_expected "$OUT/control.receiver.json")"
    control_pct="$(awk -v h="$control_hole" -v e="$control_expected" 'BEGIN { printf "%.2f", 100 * h / e }')"

    if [ "$control_verdict" = "0" ]; then
        echo
        echo "FAIL the control held every criterion while udp-fault stalled the path for $STALL_MS ms"
        echo "     every $STALL_EVERY_MS ms at seed $SEED against a $CONTROL_TARGET ms target, losing"
        echo "     $control_hole samples of $control_expected, $control_pct %. Nothing this gate could say"
        echo "     about a target has been shown to be capable of coming out otherwise, so the"
        echo "     sweep is not run: a ranking from a counter that cannot move ranks nothing"
        exit 1
    fi
    awk -v pct="$control_pct" -v floor="$CONTROL_FLOOR_PCT" 'BEGIN { exit (pct >= floor ? 0 : 1) }' ||
        refuse "the control lost only $control_pct % of its samples against the $CONTROL_FLOOR_PCT % that" \
            "$STALL_MS ms held of every $STALL_EVERY_MS ms must produce, and A6's own control measured 21.0 %:" \
            "the fault did not reach the path, so this arm is a harness that broke rather than a criterion" \
            "that fired and it demonstrates nothing"
    echo "control   failed as it must, losing $control_hole samples of $control_expected, $control_pct %"

    # ---- the sweep ----------------------------------------------------------

    pass=1
    while [ "$pass" -le "$PASSES" ]; do
        order="${ORDERS[$(((pass - 1) % ${#ORDERS[@]}))]}"
        echo
        echo "pass      $pass of $PASSES, targets in the order $order"
        for target in $order; do
            arm="t${target}-p${pass}"
            run_arm "$arm" "$target"
            record_arm "$arm" "$pass" "$target"
        done
        pass=$((pass + 1))
    done

    # Every arm has already waited out its own sender and its own tone, so nothing this
    # gate started on the host is still running here. The tasks are ended anyway, and left
    # on the cleanup list as well, because an interrupt between two arms is the case this is
    # for and it does not arrive on this line.
    for task in $HOST_TASKS; do
        "$REPO/tools/win-ssh.sh" "schtasks /end /tn $task" >/dev/null 2>&1 || true
    done
fi

[ -s "$ARMS" ] || refuse "$ARMS holds no arms, so there is nothing to decide"

# ---- verdict -----------------------------------------------------------------
#
# Everything below reads the one row per arm that the recorder wrote, and every number in
# those rows came out of an envelope through `xtask verdict`. Nothing here parses another
# program's prose: a harness reading its own output with a regular expression is how 6001
# captured packets were once read as none.

set +e
python3 - "$ARMS" "$STEP" "$REFERENCE" "$CONTROL_FLOOR_PCT" "$RATE_FACTOR" "$RSSI_SPREAD_DB" "$ARM_S" <<'PY'
import csv
import statistics
import sys

arms_path, step, reference, control_floor, rate_factor, rssi_spread, arm_s = (
    sys.argv[1],
    float(sys.argv[2]),
    int(sys.argv[3]),
    float(sys.argv[4]),
    float(sys.argv[5]),
    float(sys.argv[6]),
    float(sys.argv[7]),
)

PASS, FAIL, REFUSE = 0, 1, 2

with open(arms_path) as handle:
    rows = list(csv.DictReader(handle))

# Every column but the name is a number, and the names come from the row rather than from a
# list here: a second copy of the writer's columns is a second thing to keep in step, which
# is how a gate came to read `margin_ms` after a probe renamed it from `target_ms`.
for row in rows:
    for name, value in row.items():
        if name != "arm":
            row[name] = float(value)

control = [row for row in rows if row["arm"] == "control"]
sweep = [row for row in rows if row["arm"] != "control"]


def hole_pct(row):
    return 100.0 * row["continuity_hole"] / row["samples_expected"]


def late_pct(row):
    return 100.0 * row["rtp_late"] / row["rtp_expected"]


def margin(row):
    # The margin the median frame ran with. A frame is late exactly when its delay past
    # its own moment turns positive, and that moment is the anchor's arrival plus the
    # target, so the negated median delay is the effective target this arm actually had -
    # nominal target plus whatever the one anchoring datagram's own delay happened to be.
    return -row["arrival_delay_p50_ms"]


def spread(values):
    return max(values) - min(values)


findings = []
refusals = []
failures = []

# --- must not be zero: that the arms happened at all -------------------------
#
# Without this every fraction below is an absence dressed as a number, which is the way a
# gate here has lied more often than any other.
if not sweep:
    print("REFUSE the sweep recorded no arms, so there is nothing to rank")
    sys.exit(REFUSE)
targets = sorted({row["target_ms"] for row in sweep})
by_target = {target: [row for row in sweep if row["target_ms"] == target] for target in targets}

if arm_s < 120:
    findings.append(
        f"each arm measured {arm_s:.0f} s, below the 120 s this gate is derived for: A6's 60 s arm\n"
        f"          put 1.68 % of its datagrams past their moment where its 600 s arm put 2.08 %, so a\n"
        f"          short arm undercounts the very tail these targets are being ranked on"
    )

# --- what each target did, pass by pass --------------------------------------
#
# The median across the passes and every pass's own figure beside it, and never the mean.
# Measured on this link: pass one lost between 21 and 37 per cent of its samples where
# passes two and three lost between 0.8 and 2.6, so a mean over the three describes neither
# and would report a target as losing fourteen per cent when no arm of it ever did. The
# per-pass list is also what a reader checks the ordering against, which is the only thing a
# sweep on a link this variable can honestly claim.
for target in targets:
    rows_for = sorted(by_target[target], key=lambda row: row["pass"])
    holds = all(row["verdict"] == 0 for row in rows_for)
    findings.append(
        f"{target:.0f} ms over {len(rows_for)} arm(s): continuity hole "
        f"{statistics.median(hole_pct(row) for row in rows_for):.3f} % of samples expected at the\n"
        f"          median of its passes, which were "
        + ", ".join(f"{hole_pct(row):.3f}" for row in rows_for)
        + f" %; {sum(row['plc_frames'] for row in rows_for):.0f} frames concealed,\n"
        f"          {sum(row['render_underruns'] for row in rows_for):.0f} device underruns and "
        f"{sum(row['jitter_overruns'] for row in rows_for):.0f} buffer overruns in total, occupancy p50 "
        f"{min(row['jitter_occupancy_p50_ms'] for row in rows_for):.0f} to\n"
        f"          {max(row['jitter_occupancy_p50_ms'] for row in rows_for):.0f} ms, median margin "
        f"{statistics.median(margin(row) for row in rows_for):.2f} ms; continuity "
        f"{'held' if holds else 'broke'}"
    )

# --- and the depth each target was handed, which decides nothing ---------------
#
# A7 is closed and a sweep does not reopen it. This exists because the ranking above is a
# comparison between arms up to forty minutes apart on a link that fills this buffer at
# +9.29 ppm, and a target that happened to run while the buffer sat three frames deeper was
# ranked with three frames it did not earn. Predicted travel over one arm is below the frame
# this instrument resolves, so within an arm the drift is unreadable and the check is
# between arms: if the targets that held started systematically deeper than the ones that
# broke, the ranking is depth and not target, and the reader has to be able to see that.
#
# A record written before these columns existed cannot answer the question, and the second
# form of this gate re-decides exactly such records. Refusing is the answer; a traceback is
# not, and neither is carrying on without the control and printing a winner as though the
# depth had been checked.
if any("occupancy_start_ms" not in row for row in rows):
    print()
    print("REFUSE this record predates the per-arm occupancy columns, so the depth each target was")
    print("       handed cannot be read from it. The ranking in it may be sound and cannot be")
    print("       confirmed here: re-run the sweep rather than re-deciding this one.")
    sys.exit(REFUSE)
starts = {target: [row["occupancy_start_ms"] for row in by_target[target]] for target in targets}
predicted_travel_ms = 9.29e-6 * arm_s * 1000.0
findings.append(
    f"depth handed to each target at its first window: "
    + ", ".join(f"{target:.0f} ms -> {statistics.median(starts[target]):.1f}" for target in targets)
    + f" ms;\n          A7's +9.29 ppm projects {predicted_travel_ms:.2f} ms of travel across a "
    f"{arm_s:.0f} s arm, below the {step:.0f} ms this\n          instrument resolves, and the arms "
    f"travelled "
    + ", ".join(f"{row['occupancy_travel_ms']:+.1f}" for row in sorted(sweep, key=lambda r: r["arm"]))
    + " ms"
)

deepest_held = [target for target in targets if all(row["verdict"] == 0 for row in by_target[target])]
if deepest_held and len(deepest_held) < len(targets):
    broke = [target for target in targets if target not in deepest_held]
    held_depth = statistics.median(d for target in deepest_held for d in starts[target])
    broke_depth = statistics.median(d for target in broke for d in starts[target])
    if held_depth - broke_depth >= step:
        findings.append(
            f"the targets that held continuity began {held_depth:.1f} ms deep against "
            f"{broke_depth:.1f} ms for those that\n          broke, a whole frame or more apart, so "
            f"part of what separates them is depth they were handed\n          rather than the target "
            f"they were testing; the winner below is not safe to build on"
        )

# --- and whether the direction survived each pass ----------------------------
#
# One line per pass, its targets ordered by what they lost and its measured margins beside
# them in nominal order. This is the statement a sweep on a moving link can make when the
# magnitudes cannot be compared across passes: inside a pass the four arms sit within about
# eleven minutes of each other, and the second pass runs the targets in exactly the reverse
# order of the first, so a direction that holds in both is a direction the drift did not
# produce.
#
# The margins are there because they say whether the pass swept what it meant to. Measured
# on this link, pass one ran its 5 ms arm with 28.05 ms of margin and its 20 ms arm with
# 20.50: the anchoring datagram of the 5 ms arm turned up so late that the arm's effective
# target exceeded the largest one under test, and the nominal order of that pass means
# nothing whatever. A reader with the holes alone cannot see that, and it is the single most
# important thing a reader of this gate has to be able to see.
scrambled = {}
for pass_number in sorted({row["pass"] for row in sweep}):
    in_pass = [row for row in sweep if row["pass"] == pass_number]
    by_nominal = sorted(in_pass, key=lambda row: row["target_ms"])
    margins_in_order = [margin(row) for row in by_nominal]
    scrambled[pass_number] = margins_in_order != sorted(margins_in_order)
    findings.append(
        f"pass {pass_number:.0f} ordered by what each target lost: "
        + " then ".join(
            f"{row['target_ms']:.0f} ms at {hole_pct(row):.3f} %"
            for row in sorted(in_pass, key=hole_pct)
        )
        + f"\n          and the margin each one actually ran with, in nominal order: "
        + ", ".join(f"{row['target_ms']:.0f} to {margin(row):.2f}" for row in by_nominal)
        + " ms"
        + (", which is not ascending, so this pass did not sweep what it meant to" if scrambled[pass_number] else "")
    )

for row in sweep:
    findings.append(
        f"{row['arm']}: {hole_pct(row):.3f} % hole, {late_pct(row):.3f} % late, margin "
        f"{margin(row):.2f} ms, worst arrival {row['arrival_delay_max_ms']:.1f} ms,\n"
        f"          radio {row['rssi_mean_dbm']:.1f} dBm mean over {row['rssi_min_dbm']:.0f} to "
        f"{row['rssi_max_dbm']:.0f}, {row['rate_mean_mbps']:.0f} Mbps over {row['radio_rows']:.0f} samples"
    )

if control:
    row = control[0]
    findings.append(
        f"the control at {row['target_ms']:.0f} ms behind a stalling relay lost {hole_pct(row):.2f} % of its\n"
        f"          samples and put {late_pct(row):.2f} % of its datagrams past their moment, against a floor of\n"
        f"          {control_floor:.0f} % that the fault's duty cycle must produce"
    )

# --- the sender end, which says the source was the same each time ------------
sender_bad = [row["arm"] for row in sweep if row["sender_verdict"] != 0]
if sender_bad:
    failures.append(
        "the host end did not hold its own criteria on "
        + ", ".join(sender_bad)
        + ", so the audio those arms were measured on had already lost something before the radio"
    )

# --- what the outcome is, before any comparison is attempted ----------------
#
# Two of the three shapes an outcome can take need no comparison at all, and saying so is
# what keeps the comparability tests from refusing a run that had already answered. Every
# arm losing audio is a fact about every arm; the smallest target holding it in every pass
# is a fact about that target. Only a boundary somewhere in the middle has to be
# attributed to the target rather than to the moment the arm ran at, and that is the case
# the tests below decide.
held = {target: all(row["verdict"] == 0 for row in by_target[target]) for target in targets}
winner = next((target for target in targets if held[target]), None)
mixed = winner is not None and not all(held.values())

if winner is None:
    worst = max(row["arrival_delay_max_ms"] for row in sweep)
    failures.append(
        "no target between {:.0f} and {:.0f} ms held continuity: the hole runs from {:.3f} % to {:.3f} % "
        "of samples expected across the sweep, so A8 has no answer on this link and the choice is owed "
        "rather than read off a ranking of failures. The tail is the term - the "
        "worst arrival in the sweep came {:.0f} ms past its moment, {:.1f} times the largest target under "
        "test, and no target this phase is allowed to consider is within reach of that".format(
            min(targets),
            max(targets),
            min(hole_pct(row) for row in sweep),
            max(hole_pct(row) for row in sweep),
            worst,
            worst / max(targets),
        )
    )

# --- what two identically configured arms disagreed by -----------------------
#
# Reported whatever the outcome was, because it is the instrument's own resolution and a
# reader needs it beside every figure above rather than only when a ranking was attempted.
reference_rows = sorted(by_target.get(float(reference), []), key=lambda row: row["pass"])
margin_spread = spread([margin(row) for row in reference_rows]) if len(reference_rows) > 1 else None
if len(reference_rows) > 1:
    findings.append(
        f"the {reference} ms arms, identically configured and spread through the sweep, ran with median\n"
        f"          margins of "
        + ", ".join(f"{margin(row):.2f}" for row in reference_rows)
        + f" ms - a spread of {margin_spread:.2f} ms against the {step:.0f} ms step\n"
        f"          between adjacent targets - and holes of "
        + ", ".join(f"{hole_pct(row):.3f}" for row in reference_rows)
        + " %"
    )

if mixed:
    # Every test here is in the units the decision is made in, which is the per-arm
    # held-or-broke and not a hole fraction averaged over passes. An earlier version
    # compared per-target means and it would have been read off numbers mixing a 37 per cent
    # arm with a 1.3 per cent one, which is the same mistake as comparing two arms taken on
    # two links with the arithmetic hidden one level down.
    #
    # The first two are exact rather than statistical. A target whose own arms disagree has
    # had its outcome decided by the moment it ran at; and raising the target strictly
    # delays playout, so a frame late at one target is late at every smaller one and the
    # held-or-broke pattern cannot be anything but a single step in the target.
    for target in targets:
        outcomes = {row["verdict"] == 0 for row in by_target[target]}
        if len(outcomes) > 1:
            refusals.append(
                f"the {target:.0f} ms arms disagreed with each other: "
                + ", ".join(
                    f"pass {row['pass']:.0f} {'held' if row['verdict'] == 0 else 'broke'} at "
                    f"{hole_pct(row):.3f} %"
                    for row in sorted(by_target[target], key=lambda row: row["pass"])
                )
                + ". One configuration came out both ways, so what decided this target was the moment its "
                "arm ran at, and the boundary between the targets that held and those that did not is a "
                "boundary in time"
            )
    for lower, upper in zip(targets, targets[1:]):
        if held[lower] and not held[upper]:
            refusals.append(
                f"{lower:.0f} ms held continuity where {upper:.0f} ms did not, which the path forbids: a "
                "larger target delays playout strictly, so every frame late at the smaller one is late at "
                "the larger. The arms were not on one link and the ordering this gate would otherwise have "
                "reported would be the link's rather than the target's"
            )
    # And whether each pass swept what it meant to. The margin is what decides lateness -
    # a frame is late exactly when its delay past its own moment turns positive - so a pass
    # whose measured margins do not rise with its nominal targets has not compared the
    # targets at all, whatever its holes came out as.
    for pass_number, was_scrambled in sorted(scrambled.items()):
        if was_scrambled:
            in_pass = sorted(
                (row for row in sweep if row["pass"] == pass_number),
                key=lambda row: row["target_ms"],
            )
            refusals.append(
                f"pass {pass_number:.0f} ran its targets with margins of "
                + ", ".join(f"{row['target_ms']:.0f} ms at {margin(row):.2f}" for row in in_pass)
                + " ms, which do not rise with the nominal targets. The playout deadline is anchored on one "
                "datagram's arrival plus the target, so an arm whose anchoring datagram arrived late runs "
                "with more margin than its nominal target asked for - and this pass therefore did not put "
                "the targets in the order it was asked to compare them in"
            )
    if margin_spread is None:
        refusals.append(
            f"the incumbent {reference} ms target was measured {len(reference_rows)} time(s), so this run has no "
            "estimate of what two identically configured arms disagree by, and the boundary between the "
            "targets that held and those that did not cannot be told from that disagreement"
        )
    elif margin_spread >= step:
        refusals.append(
            f"the {reference} ms arms disagreed by {margin_spread:.2f} ms on the margin the median frame ran "
            f"with, which is not less than the {step:.0f} ms step between adjacent targets. The playout "
            "deadline is anchored on one datagram's arrival plus the target, so that disagreement is the "
            "effective target being shuffled by more than the sweep's resolution, and an ordering of "
            "nominal targets read off arms whose effective targets were shuffled by a whole step is an "
            "ordering of the shuffle"
        )
    # And the link itself, which is what those disagreements are usually made of.
    rates = [row["rate_mean_mbps"] for row in sweep]
    signals = [row["rssi_mean_dbm"] for row in sweep]
    if min(rates) > 0 and max(rates) / min(rates) > rate_factor:
        refusals.append(
            f"the arms ran at mean PHY rates from {min(rates):.0f} to {max(rates):.0f} Mbps, a factor of "
            f"{max(rates) / min(rates):.2f} against the {rate_factor:.0f} this gate allows. Airtime per "
            "datagram is inversely proportional to PHY rate and airtime is the mechanism that produces "
            "the tail a target is chosen against, so those are two links and not one link breathing"
        )
    if spread(signals) > rssi_spread:
        refusals.append(
            f"the arms' mean signal ran from {min(signals):.1f} to {max(signals):.1f} dBm, a spread of "
            f"{spread(signals):.1f} dB against the {rssi_spread:.0f} dB this gate allows - which is what "
            "A6's trace moved inside a single arm, so a wider spread between arm means is arms sitting "
            "on different links"
        )

print()
for finding in findings:
    print(f"  FINDING {finding}")

print()
if failures:
    for failure in failures:
        print(f"FAIL {failure}")
    sys.exit(FAIL)
if refusals:
    for refusal in refusals:
        print(f"REFUSE {refusal}")
    print()
    print("      Neither a pass nor a failure: the arms were measured and the numbers above stand,")
    print("      but they cannot be ranked against each other and no target is chosen here.")
    sys.exit(REFUSE)

# The latency the winner costs above the smallest target under test, named because that is
# the whole subject: a target is a bill every frame pays forever, and the arms below the
# winner are what bought the difference.
premium = (
    ""
    if winner == min(targets)
    else f",\n     costing {winner - min(targets):.0f} ms more than the smallest target under test, and "
    + ", ".join(f"{target:.0f}" for target in targets if not held[target])
    + " ms losing audio is what bought that"
)
print(
    f"PASS {winner:.0f} ms is the smallest jitter target that held continuity, over "
    f"{len(by_target[winner])} arm(s) of {arm_s:.0f} s each"
    + premium
)
sys.exit(PASS)
PY
verdict=$?
set -e
exit "$verdict"
