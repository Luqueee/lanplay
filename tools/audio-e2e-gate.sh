#!/usr/bin/env bash
# A6: the first Windows to Mac audio, and whether what came out was continuous.
#
# A1 to A5 each proved one stage against a fixture and left a probe behind. This joins
# them - WASAPI loopback on the host, Opus at 5 ms, RTP over the radio, the jitter buffer
# and CoreAudio here - and the only thing it can prove that the five isolated gates could
# not is that the joins hold while both ends run on their own clocks.
#
# Two criteria and not one, because two different quantities were reported here under
# one name for the whole of this phase. Source concealment is how much of the audio the
# sender produced was replaced by the concealer's invention: a gap with the source's own
# audio either side of it counts as played, and a period the buffer had nothing for does
# not, so it is the receiver's `concealed_samples` over a population of `samples_expected`
# that measures fidelity here - zero concealed out of zero expected comes back Unavailable
# from `xtask verdict`, which refuses the whole run rather than passing it. Playout
# continuity is `render_underruns` over `render_callbacks`: cycles the device asked for
# audio and the ring had none, which is an audible click.
#
# The pair is the finding and neither half is reported without the other. Forty of the
# forty envelopes committed under results/audio report zero render underruns, so the
# device has never once been handed silence in this project - the concealer keeps the
# timeline fed whatever the radio does. A concealment figure quoted alone therefore invites
# its reader to hear a starved device that was never starved, which is what the old name
# for it did. Beside both, the receiver states that frames were genuinely decoded rather
# than concealed throughout, because a path carrying nothing at all conceals everything and
# passes every zero-check anybody would think to write.
#
# Nothing here parses the other end's prose. Both ends print the keyed block a person
# reads when a gate fails and both write the JSON envelope `xtask verdict` decides, which
# is the arrangement docs/testing.md argues for: a harness reading its own output with a
# regular expression is how 6001 captured packets were once read as none.
#
# Sixty seconds first and then six hundred, because the two lengths answer different
# questions. A minute says the joins are correct. Ten minutes is where the two clocks
# separate: the host endpoint runs -15 ppm and this Mac's output +5 ppm, so about 20 ppm
# between them, which is twelve milliseconds of drift over the long arm and more than a
# 10 ms jitter buffer holds. A6 measures that and corrects nothing - rate matching is A7,
# and a gate that hid the drift would remove the measurement A7 is planned around. The
# occupancy at sixty seconds against the occupancy at six hundred is reported as a
# finding for exactly that reason, and it votes on nothing.
#
# The negative control is the `broken-link` arm, and it must fail. `tools/udp-fault`
# relays the datagrams on this machine and holds everything back for 400 ms every two
# seconds, forty times the target and thirteen times the ceiling. A hold does not move a
# frame's timestamp, so the playout cursor walks past the whole burst while it is in
# flight and every frame of it is discarded as late on arrival; the concealer stands in for
# a fifth of the source. Note what does not happen: the device is never handed silence, so
# the render underrun count stays at zero throughout, and an arm judged on those would have
# passed a run in which a fifth of the audio the listener heard was invented. That is the
# split argued for above, demonstrated rather than asserted. The relay is seeded so
# that an arm that fails fails the same way twice, and it sits on this side of the radio
# rather than on the host, so its numbers are not comparable with the clean arms' and are
# not compared - its whole job is to break the verdict.
#
# The radio loss figure this phase owes comes out of the receiver's own sequence
# accounting rather than out of differencing the two ends. The sender outlives the
# receiver's window by design, so `datagrams_sent` and `rtp_received` describe different
# intervals, and a rate computed from a count and a span that disagree is the defect that
# once put 150 ppm into a measurement whose whole subject was parts per million.
#
# The sender runs in the interactive session through tools/win-session.sh, not through
# tools/win-ssh.sh, and the two are not interchangeable. SSH into Windows lands in
# session 0, which has no audio endpoints: loopback capture there finds nothing to
# capture. Only a scheduled task created with /IT reaches the logged-on desktop, and a
# process launched that way has no stdout anybody can read, which is why the sender is
# given `--envelope` on a path on C: and the document is copied back afterwards. The
# build goes the other way round - over plain ssh, because a compiler needs neither a
# desktop nor an endpoint and does need its output. Getting that pair the wrong way round
# cost this project a day.
#
# Loopback delivers nothing while the endpoint is idle, so tone-source plays 997 Hz left
# and 1997 Hz right for the whole of every arm. Two frequencies rather than one, because
# a frame count cannot tell audio from silence and an equal pair would pass with the
# channels swapped.
#
# Refusal is neither a pass nor a failure, and this gate exits 2 for it. No host, no
# radio, no endpoint on the host, no output device here, a sender that never started, an
# envelope that never arrived or one missing a key a criterion reads: in each of those
# nothing was measured, and reporting a verdict from a run that did not happen is the one
# failure a criterion cannot catch.
#
# What this gate does not cover: no rate matching and no drift correction, which is A7;
# no FEC, no NACK and no retransmission, because the plan is explicit that loss is
# measured before anything is built to conceal it; and not the jitter buffer's ceiling,
# which no delay can breach and only a sink slower than its source can, which is again
# A7.
#
# usage:
#   tools/audio-e2e-gate.sh [short-seconds] [long-seconds]

set -euo pipefail

SHORT_S="${1:-60}"
LONG_S="${2:-600}"

# The receiver's own default, away from video on 5004, input on 5006, the RTP probe on
# 5008 and the jitter probe on 5108, so an interrupted run of one gate cannot be measured
# by another.
PORT=5012
RELAY_PORT=5013
FRAME_MS=5
TARGET_MS=10
# The output device this end renders through, named rather than inherited.
#
# The receiver refuses a device that does not mix at 48000 Hz stereo, because a converter
# on this path would make every figure below a statement about the converter. Naming the
# device is what makes that refusal useful. Inheriting whatever CoreAudio reports as the
# system default made a ten-minute measurement depend on a setting nobody here sets: the
# default on this Mac is a pair of Bluetooth headphones that mix at 44100 Hz and reconnect
# on their own, and a run that had waited for the radio to recover was refused thirty-seven
# seconds after it started. Named, the same machine is refused before the sender is
# launched, and the result says which endpoint the figures came from - which a figure
# nobody can attribute to a device is not reproducible without.
#
# The built-in output is the 48 kHz device on this Mac, under the name the A5 render gate
# recorded in results/audio/render/. Overridable because another machine calls it
# something else - one running in English calls it "MacBook Pro Speakers" - and a name no
# device answers to is refused with the available ones listed rather than falling back.
DEVICE="${AUDIO_OUTPUT_DEVICE:-Altavoces del MacBook Pro}"
# The seed the negative control's faults come from. Stated here and printed with the arm,
# because a fault nobody can reproduce turns a failure into a rumour.
SEED=20250815
# The stall the control injects. Forty times the target and thirteen times the ceiling,
# so it is not a margin question: what arrives after it is past its moment and is
# discarded rather than played.
STALL_MS=400
STALL_EVERY_MS=2000
# How much longer the host end runs than the window being measured, and it is one
# arithmetic with the receiver's first-packet wait rather than two guesses. The receiver
# anchors its window on the first datagram and ends a measured span later, so the sender
# has to outlast it by however long the first datagram took to arrive: that wait gives up
# at thirty seconds, this is sixty, and the tail margin is therefore never less than
# thirty. A receiver that measured a tail with nothing arriving would report underruns
# belonging to the harness and read them as the path's.
SENDER_SLACK=60

REPO="$(cd "$(dirname "$0")/.." && pwd)"
# Stamped and never cleared: six minutes of measurement was lost once to a gate that
# emptied its output directory on startup, so re-running it to re-read a verdict deleted
# the verdict.
OUT="${OUT:-/tmp/audio-e2e-gate/$(date +%Y%m%d-%H%M%S)}"
HOST="${WIN_HOST:-windows}"

XTASK="$REPO/target/release/xtask"
RECEIVER="$REPO/target/release/audio-e2e-receiver"
FAULT="$REPO/target/release/udp-fault"
WIN_REPO='C:\Users\luque\lanplay-rs'
# libopus is vendored C and is built by the cmake that ships inside Visual Studio
# BuildTools, which is not on the host's PATH by default. Nothing else on the host needs
# it and cross-compiling it from here is not possible, so the sender is built where it
# runs.
CMAKE_BIN='C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin'

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

# Anything this gate starts, this gate ends, including on an interrupt and including on
# the other machine. A relay left holding a port makes the next thing to bind it fail for
# a reason that has nothing to do with it, and a sender left running on the host holds the
# loopback client, keeps playing a tone at somebody, and holds the executable that the
# next build on that host has to replace.
#
# On the host it is the scheduled tasks this run created that are ended, by name, and
# never an image: one workspace and one target directory are shared by everything driving
# that machine, and `taskkill /IM tone-source.exe` shoots whichever sibling happens to be
# measuring with it. `schtasks /end` reaches exactly the tree this gate launched.
#
# The host half runs only when this gate got as far as launching something there. Reaching
# for a host that was never touched costs a round trip on every exit, and on the exit that
# refused because the host did not answer it would send tools/win-ssh.sh through its
# ssh-agent recovery - killing a working agent to fix a machine that is merely switched
# off.
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

# ---- what this run is in a position to measure ------------------------------
#
# Asked of `xtask gates --runnable`, not re-derived here. That detector is tested, it
# reports a requirement nobody could check as unknown rather than as absent, and a second
# implementation of the same four questions in shell is a second set of answers that can
# disagree with the first.

cargo build --release -q -p xtask
"$XTASK" gates --runnable --json --host "$HOST" >"$OUT/environment.json" ||
    refuse "the environment could not be read, so nothing here knows what it is measuring"

if ! python3 - "$OUT/environment.json" >"$OUT/preflight.txt" 2>&1; then
    cat "$OUT/preflight.txt"
    refuse "the run was not in a position to measure anything; the line above says which requirement and why"
fi <<'PY'
import json
import sys

# What A6 physically needs. Anything not `present` stops the run, unknown included: a
# host that did not answer has not been found to lack an endpoint, nobody looked, and a
# suite that reads unknown as absent shrinks without anybody deciding it should.
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

# ---- both ends built where they run -----------------------------------------

cargo build --release -q -p lanplay-audio-render
cargo build --release -q -p lanplay-udp-fault
cargo build --release -q -p lanplay-radio-sample
[ -x "$RECEIVER" ] || refuse "the receiver was not built at $RECEIVER, so this end cannot play anything"

# What state the radio was in, recorded rather than remembered.
#
# Every figure this gate produces is a figure about this link on this evening, and the
# same link has run at -46 dBm and MCS 11 as well as at -71 dBm and MCS 4. A weak
# association once contaminated an hour of video measurement before anybody noticed it,
# so a continuity figure without the conditions beside it is a number somebody will later
# compare against one taken on a different radio.
#
# Through radio-sample, which reads the association through CoreWLAN and does not scan.
# `system_profiler SPAirPortDataType` is the obvious instrument and takes the radio off
# channel to fill in the neighbouring networks, which turned an 8 ms delivery interval
# into 133 ms at p99 in a sibling experiment - the instrument produced the very bunching
# that experiment was looking for. Taken before the first arm and after the last, never
# during one, so a run that degraded halfway is visible and no reading sits inside a
# window being measured.
radio_conditions() {
    local label="$1"
    "$REPO/target/release/radio-sample" 1 1000 >"$OUT/radio-$label.csv" 2>/dev/null ||
        refuse "the Wi-Fi association could not be read, so the conditions this run was measured under would be unknown and its numbers uncomparable"
    awk -F, -v label="$label" 'NR == 2 {
        printf "radio     %-6s %s dBm signal, %s dBm noise, %s Mbps, channel %s at %s MHz\n", label, $3, $4, $5, $6, $7
    }' "$OUT/radio-$label.csv"
}

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
radio_conditions before

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

# ---- arms --------------------------------------------------------------------

# Whether the tone is playing, asked of the host because nothing else can answer it before
# the run starts. Thirty seconds, which is not generosity: win-session copies a wrapper,
# creates a task and starts it, each of those a round trip over ssh.
#
# By image and not by command line, which on a shared host is a deliberate choice rather
# than a looseness: another harness's tone-source plays the same contract tone into the
# same endpoint and serves this capture as well as its own, and what actually proves the
# tone reached this end is the receiver's own analysis of the samples it played. The
# command-line form of this question needs a pipe through cmd and PowerShell, which
# silently returned nothing and refused a run whose sender was up.
tone_playing() {
    local deadline=$((SECONDS + 30))
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

# One arm: the tone, then the sender, then the receiver.
#
# Nothing here polls the host for the sender. The receiver waits for a first datagram and
# anchors its window on it, so a slow scheduled task costs the measurement nothing, and
# whether the sender reached the air is answered by the datagrams themselves rather than
# by asking a shared machine what is running on it. A receiver that waited its whole
# first-packet window and saw nothing exits 3, and that is a refusal: no stream, no run.
run_arm() {
    local arm="$1" seconds="$2"
    shift 2
    local send_to="$LOCAL_IP:$PORT"
    local host_seconds=$((seconds + SENDER_SLACK))

    if [ "$#" -gt 0 ]; then
        # The relay is on this side of the radio, so the arm measures what the fault does
        # to the receiver and says nothing about the air.
        "$FAULT" --listen "0.0.0.0:$RELAY_PORT" --forward "127.0.0.1:$PORT" \
            --seed "$SEED" "$@" >"$OUT/$arm.relay" 2>&1 &
        send_to="$LOCAL_IP:$RELAY_PORT"
        sleep 0.5
    fi

    # Distinct task names per arm and per process: win-session derives its wrapper from
    # the task name, and two invocations sharing one wrapper overwrite each other's
    # command between the copy and the launch, after which the loser reports a timeout
    # while the winner runs twice.
    HOST_TASKS="$HOST_TASKS lanplay-e2e-tone-$arm lanplay-e2e-sender-$arm"
    WIN_TASK="lanplay-e2e-tone-$arm" WIN_TIMEOUT=$((host_seconds + 180)) \
        "$REPO/tools/win-session.sh" "C:\\Users\\luque\\e2e-tone-$arm.log" \
        "target\\release\\tone-source.exe --seconds $((host_seconds + 20))" \
        >"$OUT/$arm.tone.out" 2>&1 &
    local tone=$!
    tone_playing || refuse "nothing is playing on the host's endpoint, so loopback would have captured an idle device and the run would measure silence"

    WIN_TASK="lanplay-e2e-sender-$arm" WIN_TIMEOUT=$((host_seconds + 180)) \
        "$REPO/tools/win-session.sh" "C:\\Users\\luque\\e2e-sender-$arm.log" \
        "target\\release\\audio-e2e-sender.exe --send-to $send_to --seconds $host_seconds --arm $arm --envelope C:\\Users\\luque\\e2e-sender-$arm.json $COMMIT_FLAG" \
        >"$OUT/$arm.sender.out" 2>&1 &
    local sender=$!
    echo "arm       $arm: host sending to $send_to for $host_seconds s"

    # The first-packet wait is why SENDER_SLACK is sixty: the receiver may spend thirty of
    # them waiting for the scheduled task to reach the desktop, and its window has to end
    # inside the sender's run whichever way that goes.
    local code=0
    "$RECEIVER" --bind "0.0.0.0:$PORT" --seconds "$seconds" --arm "$arm" --device "$DEVICE" \
        --frame-ms "$FRAME_MS" --target-ms "$TARGET_MS" --first-packet-wait 30 \
        --envelope "$OUT/$arm.receiver.json" ${COMMIT_ARGS[@]+"${COMMIT_ARGS[@]}"} \
        >"$OUT/$arm.receiver.out" 2>&1 || code=$?
    case "$code" in
        0) ;;
        2) refuse "the receiver could not serve $DEVICE, so this end was in no position to play anything; its report is in $OUT/$arm.receiver.out" ;;
        3) refuse "no datagram arrived in thirty seconds, so the sender never reached the air and this arm measured a path with nothing on it; its report is in $OUT/$arm.receiver.out" ;;
        *) echo "arm       $arm: the receiver exited $code and its report is in $OUT/$arm.receiver.out" ;;
    esac

    wait "$sender" 2>/dev/null || true
    wait "$tone" 2>/dev/null || true
    pkill -f "udp-fault --listen 0.0.0.0:$RELAY_PORT" 2>/dev/null || true

    # The host end's document, brought back by itself: win-session gives a scheduled task
    # no stdout worth reading, so the envelope crosses as a file or not at all. What went
    # wrong is kept rather than discarded, because the refusal downstream can only say
    # that the document is not here and not why.
    scp -q "$HOST:C:/Users/luque/e2e-sender-$arm.json" "$OUT/$arm.sender.json" \
        2>"$OUT/$arm.sender.transfer" || true
    echo "arm       $arm done, $seconds s measured"
}

# ---- verdict -----------------------------------------------------------------

status=0

# Every number a finding reads, out of one envelope and through the one parser, set as
# variables rather than returned. A refusal cannot be raised inside `$( )`: the `exit`
# would end the subshell and leave the caller holding an empty string, which is the
# silent failure this whole arrangement exists to prevent. So the reader returns
# non-zero, `xtask` has already named the file and the name it could not find on the way
# past, and the caller refuses in this shell.
arm_numbers() {
    local document="$1"
    # rtp_expected and not rtp_received, which counts every well-formed datagram from the
    # accepted source and so exceeds what the sender put on the air by exactly the
    # duplicates the radio made. A loss fraction over that denominator would flatter the
    # radio by the one number this phase owes.
    datagrams="$("$XTASK" verdict --observation rtp_expected "$document")" || return 1
    lost="$("$XTASK" verdict --observation rtp_lost "$document")" || return 1
    late="$("$XTASK" verdict --observation rtp_late "$document")" || return 1
    concealed="$("$XTASK" verdict --observation plc_frames "$document")" || return 1
    expected="$("$XTASK" verdict --observation samples_expected "$document")" || return 1
    played="$("$XTASK" verdict --observation samples_played "$document")" || return 1
    underruns="$("$XTASK" verdict --observation render_underruns "$document")" || return 1
    callbacks="$("$XTASK" verdict --observation render_callbacks "$document")" || return 1
    occupancy="$("$XTASK" verdict --observation jitter_occupancy_p50_ms "$document")" || return 1
    overruns="$("$XTASK" verdict --observation jitter_overruns "$document")" || return 1
}

# The numbers a document is not a result without, insisted on before anything is decided
# from it.
#
# Belt and braces, and both of them deliberate. `xtask verdict` no longer passes a run
# holding a check it could not evaluate: an absent observation or an empty population makes
# the whole document a refusal, it exits 2, and the report names the observation it wanted.
# That was the central hole and it is closed. This stays because it names the eleven keys
# this gate in particular turns on, which the general answer cannot know - a document
# stating no concealment check at all would parse, would decide whatever else it stated,
# and would never mention the numbers this phase exists to measure. Asked through the same
# parser rather than through a pattern over the report, and refused here when one is missing.
insist() {
    local document="$1"
    shift
    local name
    for name in "$@"; do
        "$XTASK" verdict --observation "$name" "$document" >/dev/null ||
            refuse "$document does not state $name, so this end reported something other than the run this gate asked for"
    done
}

# And a population of zero is the same absence wearing the other hat: zero concealed
# samples out of zero expected is what a path carrying nothing looks like, and it is the
# single most common way a gate here has lied.
insist_positive() {
    local document="$1" name="$2" value
    value="$("$XTASK" verdict --observation "$name" "$document")" ||
        refuse "$document does not state $name, which is the population a zero would be measured over"
    awk -v value="$value" 'BEGIN { exit (value > 0 ? 0 : 1) }' ||
        refuse "$document reports $name as $value, so every zero in it is an absence and this run measured nothing"
}

# A record written before `continuity_hole` became `concealed_samples` states the old key
# and not the new one, and there is no way to re-decide it here: the criteria inside it are
# the old criteria, which folded source concealment and playout continuity together under a
# name that claimed the device had been starved. Refusing and naming the old key is the
# answer. Reading the old key instead would re-print the conflation this rename exists to
# end, and insisting on the new one alone would refuse with a message about a key the
# record's author had never heard of.
refuse_pre_rename() {
    local document="$1"
    "$XTASK" verdict --observation concealed_samples "$document" >/dev/null 2>&1 && return 0
    "$XTASK" verdict --observation continuity_hole "$document" >/dev/null 2>&1 || return 0
    refuse "$document states continuity_hole and not concealed_samples, so it was written" \
        "before source concealment and playout continuity were separated and its criteria are" \
        "the ones that conflated them. Re-run the gate rather than re-deciding this record"
}

# Decides one end of one arm and returns what `xtask` decided: 0 held, 1 did not, 2 could
# not be decided at all - either the document would not parse or a criterion in it had no
# number to read. The third is a refusal wherever it appears, including in the arm that is
# supposed to fail, because a criterion nobody could evaluate is not a criterion anybody
# observed disagreeing.
decide() {
    local document="$1"
    echo
    if [ ! -s "$document" ]; then
        refuse "$document was never written, so this arm produced no result to decide"
    fi
    local code=0
    "$XTASK" verdict "$document" || code=$?
    if [ "$code" -ge 2 ]; then
        refuse "$document was not decided: either it would not parse or a criterion in it" \
            "had nothing to read, and whichever it was is named above"
    fi
    return "$code"
}

# What each end has to have stated. The capture and encode counts on one side, and on the
# other the four that carry the phase: the stream arrived, audio was decoded rather than
# concealed from end to end, the concealment was accounted over a real expectation, and the
# render underruns were accounted over a real population of callbacks.
decide_sender() {
    local document="$1"
    insist "$document" capture_packets capture_frames frames_encoded datagrams_sent samples_captured samples_encoded
    decide "$document"
}

decide_receiver() {
    local document="$1"
    refuse_pre_rename "$document"
    insist "$document" rtp_received rtp_expected rtp_lost plc_frames frames_played \
        render_callbacks render_underruns render_underrun_frames samples_expected \
        samples_played concealed_samples
    insist_positive "$document" samples_expected
    insist_positive "$document" render_callbacks
    decide "$document"
}

# Named after the duration each one ran rather than after the default, because the arm is
# what a result is filed under and a document labelled for sixty seconds of a twenty
# second run is a result nobody can place.
SHORT_ARM="clean-${SHORT_S}s"
LONG_ARM="clean-${LONG_S}s"
CONTROL_ARM="broken-link"

# Whether an arm carried audio at all, which is a different question from whether it held
# its criteria and is asked with the same parser. It decides only how the ten minutes are
# spent, so it reads the two observations that say the path was alive rather than
# restating any criterion the receiver stated.
carried() {
    local document="$1" name value
    for name in rtp_received frames_played; do
        value="$("$XTASK" verdict --observation "$name" "$document")" || return 1
        awk -v value="$value" 'BEGIN { exit (value > 0 ? 0 : 1) }' || return 1
    done
}

# The short arm first: it is the one that says the joins are correct.
run_arm "$SHORT_ARM" "$SHORT_S"
decide_sender "$OUT/$SHORT_ARM.sender.json" || status=1
decide_receiver "$OUT/$SHORT_ARM.receiver.json" || status=1

# The control next, whatever the short arm said, because a gate that skipped its own
# failure mode after a bad arm would be at its least trustworthy exactly when it matters.
#
# Only the receiving end is decided here. The relay sits between the radio and this
# machine's socket, so the sender saw an ordinary run and its document would pass; a
# control credited with the sender's pass would be a control that half of the gate cannot
# fail.
run_arm "$CONTROL_ARM" "$SHORT_S" --stall-ms "$STALL_MS" --stall-every-ms "$STALL_EVERY_MS"
echo
echo "control   udp-fault held every datagram for $STALL_MS ms every $STALL_EVERY_MS ms, seed $SEED"
control_failed=0
decide_receiver "$OUT/$CONTROL_ARM.receiver.json" || control_failed=1

# The long arm runs whenever the short one carried audio, and not only when it held every
# criterion. The drift over ten minutes is the second thing this phase owes, a link that
# conceals part of the source does not make it uninteresting, and a gate that withheld the
# measurement whenever the criterion failed would produce it on exactly the runs that
# needed it least. What does make ten minutes pointless is a path that carried nothing,
# and that is refused rather than failed: nothing was measured either way.
if carried "$OUT/$SHORT_ARM.receiver.json"; then
    run_arm "$LONG_ARM" "$LONG_S"
    decide_sender "$OUT/$LONG_ARM.sender.json" || status=1
    decide_receiver "$OUT/$LONG_ARM.receiver.json" || status=1
else
    refuse "the $SHORT_S s arm carried nothing, so there was nothing for the $LONG_S s arm to measure and this run has no result to report"
fi

echo
radio_conditions after

# ---- findings ----------------------------------------------------------------
#
# Above the verdict and voting on nothing. The radio loss figure is what this phase owes
# and a failing criterion beside it does not make it uninteresting.

echo
# The conditions every figure below was taken under, and a finding rather than a note: a
# concealment figure with no association beside it invites a comparison against a run
# taken on a different radio, which is how a weak link contaminated an hour of video
# measurement in this project without anybody noticing.
if [ -s "$OUT/radio-before.csv" ] && [ -s "$OUT/radio-after.csv" ]; then
    awk -F, 'BEGIN { n = 0 }
    FNR == 2 {
        rssi[n] = $3; noise[n] = $4; rate[n] = $5; channel = $6; width = $7; n++
    }
    END {
        printf "  FINDING measured on channel %s at %s MHz, %s dBm over %s dBm noise at %s Mbps\n", channel, width, rssi[0], noise[0], rate[0]
        printf "          when the run started and %s dBm at %s Mbps when it ended; the same link has\n", rssi[1], rate[1]
        printf "          run at -46 dBm and 1200 Mbps, so nothing below is comparable with a figure\n"
        printf "          taken on a healthy radio\n"
    }' "$OUT/radio-before.csv" "$OUT/radio-after.csv"
fi

short_occupancy=""
long_occupancy=""
long_overruns=""
long_underruns=""
for arm in "$SHORT_ARM" "$LONG_ARM"; do
    document="$OUT/$arm.receiver.json"
    [ -s "$document" ] || continue
    arm_numbers "$document" ||
        refuse "$document is missing a number a finding reads and the line above names it; a figure computed from a name that is not there is worse than an absent one"
    awk -v arm="$arm" -v datagrams="$datagrams" -v lost="$lost" -v late="$late" \
        -v concealed="$concealed" -v expected="$expected" -v played="$played" \
        -v underruns="$underruns" -v callbacks="$callbacks" 'BEGIN {
        printf "  FINDING %s: the radio lost %d of %d datagrams, %.3f %%, and %d arrived past\n", arm, lost, datagrams, (datagrams > 0 ? 100 * lost / datagrams : 0), late
        printf "          their moment; the concealer stood in for %d samples of %d expected, %.3f %%,\n", expected - played, expected, (expected > 0 ? 100 * (expected - played) / expected : 0)
        printf "          over %d concealed frames - and the device was handed silence on %d of its %d\n", concealed, underruns, callbacks
        printf "          callbacks, which is the half of this pair that says whether anything clicked\n"
    }'
    if [ "$arm" = "$SHORT_ARM" ]; then
        short_occupancy="$occupancy"
    else
        long_occupancy="$occupancy"
        long_overruns="$overruns"
        long_underruns="$underruns"
    fi
done

# The drift, measured and not corrected. Twenty ppm between the two clocks is twelve
# milliseconds over ten minutes, which is more than the buffer holds, so the occupancy
# over the long arm against the settled occupancy of the short one is the estimate A7 is
# planned around. Two arms of very different lengths on purpose: a minute cannot see a
# slope that takes ten to accumulate, and comparing two minutes would be comparing two
# samples of the same noise.
if [ -n "$short_occupancy" ] && [ -n "$long_occupancy" ]; then
    awk -v short_ms="$short_occupancy" -v long_ms="$long_occupancy" -v target="$TARGET_MS" \
        -v overruns="$long_overruns" -v underruns="$long_underruns" \
        -v short_s="$SHORT_S" -v seconds="$LONG_S" 'BEGIN {
        printf "  FINDING occupancy p50 moved from %.2f ms over %d s to %.2f ms over %d s\n", short_ms, short_s, long_ms, seconds
        printf "          against a %d ms target, with %d buffer overruns and %d device underruns:\n", target, overruns, underruns
        printf "          that is the drift A7 corrects, and A6 states it rather than hiding it\n"
    }'
fi

echo
if [ "$control_failed" -eq 0 ]; then
    # The one result that is worse than a failing gate: an instrument that cannot fail.
    # Nine of the twenty-one gates here owe a negative control and say so in the index; a
    # control that passed would quietly make this one the tenth.
    echo "FAIL the $CONTROL_ARM arm held every criterion while udp-fault stalled the path for"
    echo "     $STALL_MS ms every $STALL_EVERY_MS ms at seed $SEED, so nothing this gate says about a"
    echo "     clean arm has been shown to be capable of coming out otherwise"
    status=1
else
    echo "the control failed as it must: a link stalled for $STALL_MS ms every $STALL_EVERY_MS ms puts a"
    echo "fifth of the source through the concealer, and this gate saw it happen"
fi

echo
if [ "$status" -ne 0 ]; then
    echo "FAIL an arm did not hold what it stated, and the blocks above say which and why"
    exit 1
fi
echo "PASS Windows to Mac audio arrived unconcealed over $SHORT_S s and over $LONG_S s with the"
echo "     device never handed silence, a bridged gap counted as played and an empty buffer did"
echo "     not, and the control broke it"
