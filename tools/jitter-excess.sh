#!/usr/bin/env bash
# A8.1: one population, every jitter target at once, and the cluster structure a
# survival curve cannot express.
#
# ## Why the sweep is not here any more
#
# A8 ran four candidate targets three times each over forty minutes and refused to
# choose. The reason is measured rather than suspected: the target is fixed when
# the buffer is built, so each candidate is its own arm minutes away from the
# others, and the between-arm variance of this link's heavy tail is larger than the
# 5 ms step being resolved. Ten arms on the steadiest link this project has ever
# recorded - channel 36 at 80 MHz, every arm negotiating a median 1200 Mbps, the
# effective margins ascending correctly in every pass - produced concealment ratios
# from 0.196 to 7.442 per cent, with worst arrivals of 78, 61, 76, 221, 24, 79, 19
# and 91 ms across arms, and 0 render underruns in 202431 callbacks throughout. The
# arms differed by how many bursts landed inside each 120 second window and not by
# what target they were configured with.
#
# Nothing about the arrangement of the arms fixes that, and a longer arm does not
# either: the anchor offset and the burst incidence are properties of an arm, so
# they do not average out inside one. What fixes it is refusing to run separate
# arms. This gate takes ONE run and derives every candidate from it.
#
# ## The primitive
#
#   excess_i = (arrival_i - arrival_ref) - (rtp_i - rtp_ref) / 48000
#
# It names no target, so `late(T)` is exactly `excess_i > T` and one population
# answers every candidate at once. The receiver already computed this: its arrival
# delay is the same subtraction with the playout anchor still in it, and the anchor
# is a run constant. See `macos/audio-render/src/excess.rs` for the three decisions
# inside it - the reference is the run's minimum and not the first packet, the
# drift is fitted and removed with the uncorrected curve reported beside it, and
# the fit goes through per-block minima because ordinary least squares over every
# arrival is destroyed by exactly the bursts this link produces.
#
# Measured, on this machine, over loopback with the true drift zero: with udp-fault
# holding five per cent of datagrams for 40 ms, the all-points slope read -6.92 ppm
# and the per-block-minima fit read +0.07 ppm. An estimator a burst moves by seven
# parts per million cannot measure the nine parts per million A7 found.
#
# ## Two drift numbers, and the sign that has to be stated
#
# The fit returns the slope of lateness against stream time, and that is not the
# source clock's rate: from `late_i = A_i - P0 - (R_i - R0)/f` the slope is
# `1 - f_s/f`, so a source running FAST makes the subtracted RTP term outrun
# arrival time and gives a NEGATIVE slope. Both are reported under names that say
# which is which - the delay slope, which is what the correction subtracts, and
# the source rate referred to this Mac's timebase, which is A7's convention and
# the only one of the two comparable with A7's figure.
#
# This is recorded because it was got wrong. The first radio run here fitted a
# delay slope of -13.22 ppm over 120005 arrivals across 600 s, which is a source
# clock fast by +13.22 ppm, and the gate printed DISAGREE against A7's +9.29 ppm
# because one doc comment had the convention backwards. The two agree, at a ratio
# of 1.42, and they are not owed an exact match: A7 compared crystals directly and
# this compares the source's audio clock against this Mac's monotonic clock
# through a radio and a jitter buffer.
#
# ## The 5 ms row is the sender, not the radio
#
# This sender packs two Opus frames into one captured packet, so both members of a
# pair arrive at one instant while the second sits a frame later in stream time.
# Excess subtracts stream time, so excess(second) = excess(first) - 5 ms exactly,
# and the population is bimodal with the two modes one frame apart. The first
# radio run read 50.68 per cent of the population late at 5 ms, in clusters of one
# frame separated by gaps of one frame, and that alternation is the signature: a
# burst is consecutive by definition, so no radio can leave every second frame on
# time. A reader who sees only the percentage will think the link is broken, which
# is why the harness names it in its findings rather than only in the table.
#
# A6.1 measured the same thing from the other side this session and the two
# derivations close: the per-pair difference came to -4.996 ms at p50, 96 per cent
# of pairs inside the [-5,-4) ms bucket, over 8998, 9000 and 120004 pairs, and the
# first member is the one that goes late in practice at 524 against 384, 476
# against 354 and 8594 against 6391.
#
# What it establishes is a floor: a target below the pair spacing cannot hold both
# members of a pair, so 5 ms is structurally unreachable on this sender for a
# reason that has nothing to do with the air. What it does not establish is that
# spacing the pair in the sender would be an improvement. That would collapse the
# bimodality and would also delay the second frame by a frame in real time, and
# whether the floor it removes is worth the delay it adds is arithmetic nobody has
# done. An argument from this structure for spacing was made and retracted earlier
# in this session for being wrong by a sign; nothing here revives it, and this
# harness is not the place to settle it.
#
# ## How long the run is, and why it is not a choice
#
# The correlated unit is the cluster, not the frame, so the precision of a rate
# goes as one over the square root of the CLUSTER count. Thirty clusters puts the
# fractional standard error at 18 per cent, which makes a factor of two between two
# thresholds three standard errors and refuses to claim a difference of a quarter.
#
# How long thirty clusters takes is a question about the threshold. A6 measured
# 2.08 per cent of its datagrams past a 10 ms target over 600 s, with p99 only
# 0.8 ms late against that target, so the population past 20 ms is order two per
# thousand. At the wire's 200 datagrams a second that is 0.4 late frames a second,
# and at A8's cluster sizes of order six clusters a minute - thirty clusters in
# 300 s. Six hundred seconds is that with a factor of two in hand, and it is the
# arm A6 already held on this link, so it is a length this pair has demonstrated
# rather than a number invented here.
#
# The derivation has a consequence that is stated rather than hidden. At 100 ms
# this link produced roughly one arrival per two-minute arm, so thirty clusters
# there is an hour of measurement and this link does not stand still for an hour.
# No run this gate can take will quote a cluster rate at 100 ms, and the receiver
# withholds one rather than printing a rate from four events. The curve reaches out
# to 100 ms because its SHAPE says whether this is one heavy distribution or a
# normal regime with a second class of stall behind it, and that is read off the
# histogram.
#
# ## What this gate refuses, and why refusal is not failure
#
# Refusal exits 2 and is neither a pass nor a failure, for the reason
# `tools/radio-preflight.sh` sets out at length: a precondition is not a criterion,
# and a gate reporting a verdict for a run it was in no position to take states
# something nobody tested. Every refusal is in `tools/jitter-excess.py` and every
# one of them is exercised by `tools/jitter-excess.py selftest`, which builds a
# document that trips it and requires it to fire - and requires the unmutated
# document to pass, so the suite cannot be satisfied by a function that refuses
# everything.
#
# The refusals, and the number each reads: any lost packet, because a lost frame is
# a timeline position with no delay and a curve across holes describes a stream
# nobody sent; a non-zero render underrun count over a positive callback
# population, because that is audible silence and a change at this end rather than
# in the air, and 40 of 40 committed envelopes read zero there; a callback
# population of zero, which is the companion, since zero underruns over zero cycles
# is what a device that never ran looks like; any off-grid timestamp, because such
# a frame has no timeline position and is not in the population; a run shorter than
# the 600 s derived above; a trace that overflowed or that saw a timeline position
# claimed twice, both of which are the instrument disagreeing with itself; a
# timeline hole where the sequence numbers report no loss, which is two accountings
# disagreeing; fewer blocks than the run's own length implies; and a curve that
# rises, since raising a threshold cannot make more frames late on one population.
#
# Before any of that, and in the shell rather than in the document: the radio
# preflight, and channel 36 at 80 MHz specifically. Channel 36 at 80 MHz occupies
# 5170 to 5250 MHz, and 5150-5250 is the only WAS/RLAN band in Spain carrying no
# DFS obligation, so it is the only 80 MHz configuration that cannot be told to
# vacate in the middle of a ten minute measurement. Moving from 116 to 36 took late
# access units from 69/min to 5.5/min, which is why it is the baseline rather than
# a preference. The width is required as well as the channel because the obligation
# attaches to the occupied span: 160 MHz anchored at 36 reaches 5330 and is a radar
# band.
#
# ## The negative control
#
# `tools/udp-fault` relays the stream on this side of the radio and holds a known
# fraction of the datagrams for a known extra delay, at a fixed seed, so the
# injected distribution is reproducible and its shape is known before the run.
# What the control has to demonstrate is that the curve SEES it. A curve that
# cannot see a population deliberately put in front of it is a curve whose clean
# arm says nothing, and no synthetic assertion about the arithmetic tests that: it
# would pass equally on an instrument reading its own scratch buffer.
#
# Three things are required of the control and one of the clean arm, and the fourth
# is what keeps the other three from being satisfied by an instrument that reports
# five per cent past 30 ms on every run it ever sees. The fraction past 30 ms must
# be within a factor of two of the injected five per cent; the curve must fall off
# a cliff between 30 and 60 ms rather than carrying a tail, because the injected
# distribution is a point mass at the hold; p95 must land on the hold; and the
# clean arm's fraction past 30 ms must be at least ten times smaller.
#
# Measured over loopback at seed 20250815 with five per cent held 40 ms: 5.036 per
# cent past 30 ms, 403 frames past 30 and 0 past 60, p95 at 37.53 ms, against a
# clean arm of 0 past 5 ms and a maximum excess of 0.10 ms. The step is where it
# was injected and the clean arm does not have it.
#
# A stall was the obvious control and is weaker here. `--stall-ms 400
# --stall-every-ms 2000` is what A6 and A8 used, and it holds a fifth of the stream
# for forty times the target - which breaks continuity comprehensively and says
# nothing about whether the curve can resolve a distribution, because every held
# datagram lands in one enormous bin. What this gate has to prove about itself is
# resolution, so the control injects a shape at a known position rather than a
# catastrophe.
#
# ## What this gate does not do
#
# It does not choose a target. It produces the curve and the cluster table a target
# would be chosen from, and reporting a figure at 30, 50 or 80 ms authorises
# nothing: targets above 20 ms are a product decision about the latency budget,
# taken elsewhere. It corrects the drift between the two clocks and reports the
# size of the correction; it does not correct the drift in the audio path, which is
# A7's subject. It says nothing about any other link: a tail measured at -67 dBm on
# channel 36 has not been shown to be the tail at -78 dBm.
#
# One provenance wrinkle: the receiver writes its own gate name into every document
# it produces and takes no flag for it, so both arms here are filed under
# `audio-e2e-gate`. What places a document is its arm, which is why each is named
# for what it is.
#
# usage:
#   tools/jitter-excess.sh [clean-arm-seconds] [control-arm-seconds]
#   VERDICT_ONLY=1 OUT=<a previous run's directory> tools/jitter-excess.sh
#   SELFTEST=1 tools/jitter-excess.sh
#
# The second form re-decides a run that already happened, from the documents it
# left, and the third exercises every refusal without a radio at all.
#
# env:
#   AUDIO_OUTPUT_DEVICE  the output device to render through, named rather than
#                        inherited; the default on this Mac is a pair of Bluetooth
#                        headphones at 44100 Hz that reconnect on their own
#   WIN_HOST             ssh host for the sender; default `windows`
#
# exit 0  the curve was measured and the control fired
# exit 1  the control did not fire, or fired for the wrong reason, so the clean
#         arm's curve has not been shown to be a measurement of anything
# exit 2  refused: the run was in no position to measure a curve, and the block
#         above names which criterion and the number it read

set -euo pipefail

CLEAN_S="${1:-600}"
CONTROL_S="${2:-120}"

# Away from video on 5004, input on 5006, the RTP probe on 5008, A6 on 5012 and
# 5013, the target sweep on 5014 and 5015 and the jitter probe on 5108, so an
# interrupted run of one gate cannot be measured by another.
PORT=5016
RELAY_PORT=5017
FRAME_MS=5

# The incumbent target, and it is not what this gate measures. The curve is
# target-independent by construction, so the only thing this figure decides is the
# concealment ratio reported beside the curve - which is A6's and A8's deciding
# counter and is here as a consequence rather than as the measurement.
TARGET_MS=10

# The control's injected distribution, and the seed it comes from: a fault nobody
# can reproduce turns a failure into a rumour. Five per cent held 40 ms - above
# every threshold a target could be chosen at, below the 50 ms row so the cliff
# has somewhere to fall, and thin enough that the arm is still a stream rather
# than a catastrophe.
SEED=20250815
INJECT_PCT=5
HOLD_MS=40

# The link this project validated for audio, and the only non-DFS 80 MHz
# configuration available in Spain.
BASELINE_CHANNEL=36
BASELINE_WIDTH_MHZ=80

# The output device this end renders through, named rather than inherited. The
# receiver refuses a device that does not mix at 48000 Hz stereo, because a
# converter on this path would make every figure below a statement about the
# converter. Overridable because a machine running in English calls the built-in
# output "MacBook Pro Speakers".
DEVICE="${AUDIO_OUTPUT_DEVICE:-Altavoces del MacBook Pro}"

# How much longer the host end runs than the window being measured. One arithmetic
# with the receiver's first-packet wait rather than two guesses: the receiver
# anchors its window on the first datagram and ends a measured span later, so the
# sender has to outlast it by however long that first datagram took.
FIRST_PACKET_WAIT=20
SENDER_SLACK=30

REPO="$(cd "$(dirname "$0")/.." && pwd)"
# Stamped and never cleared: six minutes of measurement was lost once to a gate
# that emptied its output directory on startup, so re-running it to re-read a
# verdict deleted the verdict.
OUT="${OUT:-/tmp/jitter-excess/$(date +%Y%m%d-%H%M%S)}"
HOST="${WIN_HOST:-windows}"
VERDICT_ONLY="${VERDICT_ONLY:-0}"
SELFTEST="${SELFTEST:-0}"

XTASK="$REPO/target/release/xtask"
RECEIVER="$REPO/target/release/audio-e2e-receiver"
FAULT="$REPO/target/release/udp-fault"
RADIO="$REPO/target/release/radio-sample"
DECIDE="$REPO/tools/jitter-excess.py"
WIN_REPO='C:\Users\luque\lanplay-rs'
# libopus is vendored C and is built by the cmake inside Visual Studio BuildTools,
# which is not on the host's PATH by default. Nothing else on the host needs it and
# it cannot be cross-compiled from here, so the sender is built where it runs.
CMAKE_BIN='C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin'

# The self-test needs no radio, no host and no output device, so it is answered
# before anything else is set up. It is the whole refusal suite and it is here
# rather than only in the python so that a person reaching for this gate finds it.
if [ "$SELFTEST" = "1" ]; then
    echo "selftest  every refusal this gate can raise, fired against a document built to trip it"
    echo
    exec python3 "$DECIDE" selftest
fi

mkdir -p "$OUT"
echo "results   $OUT"

# Neither a pass nor a failure. A gate that reported a verdict for a run it was in
# no position to take would be stating a criterion nobody tested, and an agent
# reading the result later cannot tell that from a real one.
refuse() {
    echo
    echo "REFUSE $*"
    exit 2
}

# Anything this gate starts, this gate ends, including on an interrupt and
# including on the other machine. A relay left holding a port makes the next thing
# to bind it fail for a reason that has nothing to do with it, and a sender left
# running on the host holds the loopback client and the executable the next build
# has to replace.
#
# On the host it is the scheduled tasks this run created that are ended, by name,
# never an image: one workspace is shared by everything driving that machine, and
# `taskkill /IM tone-source.exe` shoots whichever sibling happens to be measuring
# with it. And this is weaker than it looks, which is why the tone and the sender
# are each bounded by their own `--seconds`: `schtasks /end` ends the wrapper the
# task runs and not the process the wrapper started, and a tone launched for 300 s
# and ended after 25 was measured still playing four seconds later.
HOST_TASKS=""
cleanup() {
    pkill -f "udp-fault --listen 0.0.0.0:$RELAY_PORT" 2>/dev/null || true
    pkill -f "$RADIO" 2>/dev/null || true
    for task in $HOST_TASKS; do
        "$REPO/tools/win-ssh.sh" "schtasks /end /tn $task" >/dev/null 2>&1 || true
    done
}

if [ "$VERDICT_ONLY" != "1" ]; then
    trap cleanup EXIT INT TERM

    case "$CLEAN_S" in '' | *[!0-9]*) refuse "the clean arm's length must be a whole number of seconds" ;; esac
    case "$CONTROL_S" in '' | *[!0-9]*) refuse "the control arm's length must be a whole number of seconds" ;; esac

    # ---- what this run is in a position to measure ---------------------------
    #
    # Asked of `xtask gates --runnable`, not re-derived here. That detector is
    # tested, it reports a requirement nobody could check as unknown rather than as
    # absent, and a second implementation of the same four questions in shell is a
    # second set of answers that can disagree with the first.
    cargo build --release -q -p xtask
    "$XTASK" gates --runnable --json --host "$HOST" >"$OUT/environment.json" ||
        refuse "the environment could not be read, so nothing here knows what it is measuring"

    if ! python3 - "$OUT/environment.json" >"$OUT/preflight.txt" 2>&1; then
        cat "$OUT/preflight.txt"
        refuse "the run was not in a position to measure anything; the line above names the" \
            "requirement and what it costs"
    fi <<'PY'
import json
import sys

# Anything not `present` stops the run, unknown included: a host that did not
# answer has not been found to lack an endpoint, nobody looked, and a suite that
# reads unknown as absent shrinks without anybody deciding it should.
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

    LOCAL_IP="$(ipconfig getifaddr en0 || true)"
    [ -n "$LOCAL_IP" ] || refuse "en0 has no address, so there is nothing for the host to send to"
    echo "radio     host -> en0 $LOCAL_IP:$PORT"
    echo "device    rendering through $DEVICE"

    # ---- the link this run is allowed to happen on --------------------------
    #
    # The channel and the width are categorical and belong before the run: no
    # projection is involved in saying whether a channel number is the validated
    # one, and a DFS channel may be told to vacate in the middle of a ten minute
    # measurement, after which no arithmetic downstream survives.
    #
    # The projection - whether the signal will hold still - is asked for as well
    # here, unlike in the counterbalanced sweep. This gate takes ONE run, so it has
    # no counterbalancing to fall back on: a link that moves inside the run moves
    # inside the population the curve is computed over, and the curve would be a
    # statement about the weather. The preflight's window is 120 s and its budget
    # is 3 dB, derived as the amount of level movement that doubles this radio's
    # rate.
    if ! RUN_SECONDS="$CLEAN_S" REQUIRE_CHANNEL="$BASELINE_CHANNEL" \
        REQUIRE_WIDTH="$BASELINE_WIDTH_MHZ" REQUIRE_NON_DFS=1 OUT="$OUT/radio-preflight" \
        "$REPO/tools/radio-preflight.sh" 120 >"$OUT/radio-preflight.txt" 2>&1; then
        sed 's/^/  /' "$OUT/radio-preflight.txt"
        refuse "the link is not the one this project validated, or the window could not be" \
            "read; the preflight above names the criterion and its number"
    fi
    grep -E "^(PASS|NOTE|REFUSE)" "$OUT/radio-preflight.txt" | sed 's/^/  /'

    # ---- both ends built where they run -------------------------------------

    cargo build --release -q -p lanplay-audio-render -p lanplay-udp-fault -p lanplay-radio-sample
    [ -x "$RECEIVER" ] || refuse "the receiver was not built at $RECEIVER, so this end cannot play anything"
    [ -x "$FAULT" ] || refuse "udp-fault was not built, so this gate has no negative control and its clean arm would be a curve nobody has shown to be a measurement"
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

    # Stated only when git can say what it is: a run recording a hash nobody read
    # out of a repository has a provenance worse than an absent one.
    COMMIT_ARGS=()
    COMMIT_FLAG=""
    if COMMIT="$(git -C "$REPO" rev-parse --short HEAD 2>/dev/null)"; then
        COMMIT_ARGS=(--commit "$COMMIT")
        COMMIT_FLAG="--commit $COMMIT"
    fi

    # Whether the tone is playing, asked of the host because nothing else can
    # answer it before an arm starts. By image and not by command line: on a shared
    # host another harness's tone-source plays the same contract tone into the same
    # endpoint and serves this capture as well as its own, and what proves the tone
    # reached the far end is the receiver's own analysis of the samples it played.
    tone_playing() {
        local deadline=$((SECONDS + ${1:-40}))
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
    # Nothing here polls the host for the sender. The receiver waits for a first
    # datagram and anchors its window on it, so a slow scheduled task costs the
    # measurement nothing, and whether the sender reached the air is answered by
    # the datagrams themselves rather than by asking a shared machine what is
    # running on it.
    run_arm() {
        local arm="$1" seconds="$2"
        shift 2
        local send_to="$LOCAL_IP:$PORT"
        local host_seconds=$((seconds + SENDER_SLACK))
        local tone_s=$((host_seconds + 20))

        # The tone first, then the relay, then the sender, then the receiver. The
        # order is the arithmetic: a sender launched before the endpoint is
        # streaming captures an idle device, and loopback on an idle endpoint
        # delivers nothing at all rather than delivering silence.
        local tone_task="lanplay-a81-tone-$arm"
        HOST_TASKS="$HOST_TASKS $tone_task"
        WIN_TASK="$tone_task" WIN_TIMEOUT=$((tone_s + 180)) \
            "$REPO/tools/win-session.sh" "C:\\Users\\luque\\a81-tone-$arm.log" \
            "target\\release\\tone-source.exe --seconds $tone_s" \
            >"$OUT/$arm.tone.out" 2>&1 &
        local tone=$!
        tone_playing ||
            refuse "nothing is playing on the host's endpoint before arm $arm, so loopback would capture an idle device and this arm would measure silence"

        if [ "$#" -gt 0 ]; then
            # The relay is on this side of the radio, so the arm measures what the
            # injected distribution does to the curve and says nothing about the
            # air. udp-fault decides direction by comparing a datagram's source
            # against --forward, so the sender must be a different process from the
            # receiver - which it is, on the other machine.
            "$FAULT" --listen "0.0.0.0:$RELAY_PORT" --forward "127.0.0.1:$PORT" \
                --seed "$SEED" "$@" >"$OUT/$arm.relay" 2>&1 &
            send_to="$LOCAL_IP:$RELAY_PORT"
            sleep 0.5
        fi

        # A task name per arm: win-session derives its wrapper from the task name,
        # and two invocations sharing one wrapper overwrite each other's command
        # between the copy and the launch, after which the loser reports a timeout
        # while the winner runs twice.
        local task="lanplay-a81-sender-$arm"
        HOST_TASKS="$HOST_TASKS $task"
        WIN_TASK="$task" WIN_TIMEOUT=$((host_seconds + 180)) \
            "$REPO/tools/win-session.sh" "C:\\Users\\luque\\a81-sender-$arm.log" \
            "target\\release\\audio-e2e-sender.exe --send-to $send_to --seconds $host_seconds --arm $arm --envelope C:\\Users\\luque\\a81-sender-$arm.json $COMMIT_FLAG" \
            >"$OUT/$arm.sender.out" 2>&1 &
        local sender=$!
        echo
        echo "arm       $arm: host sending to $send_to for $host_seconds s, measuring $seconds s"

        "$RADIO" --seconds "$seconds" --interval-ms 1000 \
            >"$OUT/$arm.radio.csv" 2>"$OUT/$arm.radio.err" &
        local radio=$!

        local code=0
        "$RECEIVER" --bind "0.0.0.0:$PORT" --seconds "$seconds" --arm "$arm" --device "$DEVICE" \
            --frame-ms "$FRAME_MS" --target-ms "$TARGET_MS" \
            --first-packet-wait "$FIRST_PACKET_WAIT" \
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
            refuse "the Wi-Fi association could not be read during arm $arm, so the conditions it was measured under are unknown"
        wait "$sender" 2>/dev/null || true
        # The tone outlives the sender by twenty seconds on purpose, and the next
        # arm waits for it rather than starting beside it: two tone-source
        # processes on one endpoint sum into the mix, and an arm whose capture
        # carried twice the level would be measured against tolerances taken on
        # one.
        wait "$tone" 2>/dev/null || true
        pkill -f "udp-fault --listen 0.0.0.0:$RELAY_PORT" 2>/dev/null || true

        # The host end's document, brought back by itself: win-session gives a
        # scheduled task no stdout worth reading, so the envelope crosses as a file
        # or not at all.
        scp -q "$HOST:C:/Users/luque/a81-sender-$arm.json" "$OUT/$arm.sender.json" \
            2>"$OUT/$arm.sender.transfer" || true
    }

    # The control first, and it aborts the run rather than being discovered ten
    # minutes later. A clean arm's curve is worth nothing until the instrument that
    # produced it has been shown able to see a population put in front of it, so
    # spending the ten minutes before that is established is spending it on a
    # number that may have to be thrown away.
    run_arm control "$CONTROL_S" --reorder "$INJECT_PCT" --reorder-hold-ms "$HOLD_MS"

    # One number, read here rather than at the end, purely to abort early. The full
    # control decision needs the clean arm too - the fourth criterion is that the
    # clean arm does NOT show the injected shape - so it happens once, below, in
    # the same place as everything else.
    seen="$("$XTASK" verdict --observation excess_late_30ms "$OUT/control.receiver.json" 2>/dev/null || echo absent)"
    case "$seen" in
        absent | 0)
            refuse "the control arm reports $seen frames past 30 ms, so a fault injected at" \
                "$INJECT_PCT per cent held $HOLD_MS ms never reached the curve. The clean arm is" \
                "not being run: an instrument that cannot see a population put in front of it" \
                "produces a curve nobody can interpret, and this is a harness that broke rather" \
                "than a criterion that fired"
            ;;
    esac
    echo "control   $seen frames past 30 ms, so the fault reached the curve; the clean arm follows"

    run_arm clean "$CLEAN_S"
fi

# ---- verdict -----------------------------------------------------------------

CLEAN="$OUT/clean.receiver.json"
CONTROL="$OUT/control.receiver.json"
for document in "$CLEAN" "$CONTROL"; do
    [ -s "$document" ] || refuse "$document does not exist, so there is nothing to decide"
done

# The receiver's own criteria, through the one evaluator, before this gate's
# preconditions are applied to the same document. Two different questions and both
# belong: `xtask verdict` decides whether the run carried audio at all, and the
# block below decides whether a curve taken from it means anything. A run that
# carried nothing would produce a perfectly monotone curve over an empty
# population.
echo
echo "receiver  the clean arm's own criteria, decided by the one evaluator"
verdict=0
"$XTASK" verdict "$CLEAN" >"$OUT/clean.verdict" 2>&1 || verdict=$?
grep -E "^(PASS|FAIL|REFUSE)" "$OUT/clean.verdict" | sed 's/^/  /'
case "$verdict" in
    0) ;;
    2) refuse "the clean arm stated a criterion nobody could decide; $OUT/clean.verdict names it" ;;
    *) echo "  the clean arm disagreed with one of its own criteria, which is reported here and"
       echo "  decided below on the curve: a run that concealed source audio still measured the"
       echo "  delay that made it conceal, and that delay is this gate's subject" ;;
esac

echo
echo "curve     the clean arm, and every condition it means anything under"
echo
set +e
python3 "$DECIDE" curve "$CLEAN"
curve=$?
set -e
[ "$curve" -eq 0 ] || exit "$curve"

echo
set +e
python3 "$DECIDE" control "$CLEAN" "$CONTROL" "$INJECT_PCT" "$HOLD_MS"
control=$?
set -e

echo
if [ "$control" -ne 0 ]; then
    echo "FAIL the control did not fire, or fired for the wrong reason. The clean arm's curve"
    echo "     above has not been shown to be a measurement of the link rather than of the"
    echo "     instrument, so nothing in it may be used to choose a target."
    exit 1
fi
echo "PASS the curve was measured over one population with no loss, no timeline hole and no"
echo "     render underrun, and the control's injected distribution is visible in the curve"
echo "     where the clean arm's is not. Rows at or below 20 ms are a target's cost; rows"
echo "     above are shape and authorise nothing."
exit 0
