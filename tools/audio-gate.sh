#!/usr/bin/env bash
# A1: what does WASAPI loopback actually deliver on this host?
#
# Every later audio decision rests on the answer. Whether the path to Opus can be
# direct or needs a converter, how big a PCM accumulator has to be, whether the
# capture is event-driven or polled: all of it follows from the endpoint's real mix
# format and real packet cadence, and none of it can be assumed.
#
# Two outcomes are answers and only one is a failure, which is the distinction this
# gate exists to make. A host whose endpoint runs at something other than 48 kHz
# stereo has not failed; it has told us the direct path is unavailable and a later
# phase needs a converter. A capture that lost frames has failed. Conflating those
# would either paper over the finding or condemn the machine for having a mixer.
#
# The tone plays at -20 dBFS rather than a level chosen for measurement, because the
# only active render endpoint here is a monitor's audio and it sits next to a person.
# Loopback takes the digital mix before any converter, so the level costs the
# detector nothing.
#
# Loopback delivers nothing while the endpoint is idle, so a source has to be
# playing or the run measures silence. That is the same shape as Desktop
# Duplication needing something to draw, and it is why tone-source exists. A gate
# that read silence as zero discontinuities would be the third time this project
# made that mistake.
#
# Which is why the negative control is that same silence, held somewhere the gate can
# read it. Stopping the tone was the obvious arm and it does not work. Measured on this
# host with nothing playing: 0 packets, 0 frames, and the probe exits 4 saying it
# captured nothing. That is the report a capture started in session 0 produces, and the
# report a host that fell asleep produces, and the report a probe that failed to start
# produces. An arm that produces nothing is the signature of a broken harness rather
# than of a criterion firing, and a control indistinguishable from a broken harness
# certifies whatever it was pointed at - which is exactly how A3's first control passed
# having lost 2000 of 2000 and exercised nothing.
#
# So the control holds the endpoint open with a stream of zeros instead. A render client
# playing digital silence keeps the audio engine streaming, so loopback delivers its
# whole cadence and the run is structurally perfect: measured over ten seconds, 1001
# packets of 480 frames, 480480 frames against 480000 nominal, no position gap and no
# discontinuity in flight. What it does not deliver is audio, and only the samples say
# so - both channels read 0.0 Hz at -inf dBFS against the arm's 996.8 and 1996.8 at
# -19.87.
#
# Nothing else in the report can say it. WASAPI's silent flag stays clear on a buffer
# full of zeros - measured, 0 silent packets of 1001 - because the flag is the engine
# declaring a buffer's contents undefined rather than a statement about what is in
# them, so the probe exits 0 and calls the run captured. That is what makes this
# control worth running: it passes every structural check the gate has and the probe
# declines nothing, so the only thing between it and a green verdict is the tone
# detector and the channel distinctness beside it. Those two criteria are what the
# control exercises, and they are the two the gate could least afford to have never
# seen fire.
#
# usage:
#   tools/audio-gate.sh [seconds]

set -euo pipefail

SECONDS_TO_RUN="${1:-60}"
# Ten seconds for the control, which is not a shortened copy of the arm above. The
# measuring arm's length is set by the drift it has to average out; the control has
# only to carry enough packets for the tone detector to be read over, and at the
# endpoint's 10 ms period that is a thousand of them against an analysis window of
# 4800 frames.
CONTROL_SECONDS=10
REPO="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${OUT:-/tmp/audio-gate/$(date +%Y%m%d-%H%M%S)}"
# Both the control's readiness signal and the way it is stopped. Expanded by cmd on
# the host, never here.
MARKER='%TEMP%\lanplay-silence.playing'

mkdir -p "$OUT"
echo "results   $OUT"

# Ended on the way out by name, and only these four, every one of them created here.
# The lab host is shared: a run that reached for taskkill and an image name would end
# whatever else was capturing or playing on it, and the agent whose measurement died
# would have no way of knowing why.
TASKS=(lanplay-tone lanplay-audio lanplay-silence lanplay-audio-control)

cleanup() {
    local ends=""
    for task in "${TASKS[@]}"; do
        ends="${ends}schtasks /end /tn $task >nul 2>&1 & "
    done
    # One connection for all of it. Windows sshd throttles new connections and a burst
    # of them once stopped it spawning shells at all, taking the host out of the lab
    # until it recovered; a cleanup path is the last place to risk that.
    "$REPO/tools/win-ssh.sh" "${ends}del /q \"$MARKER\" >nul 2>&1 & exit 0" >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM

"$REPO/tools/win-sync.sh" >/dev/null
"$REPO/tools/win-ssh.sh" 'cd C:\Users\luque\lanplay-rs && cargo build --release -q -p lanplay-audio-capture -p lanplay-tone-source' >/dev/null
echo "built     probe and tone source"

# The control's instrument, written into the results directory rather than kept on the
# host, so the evidence holds exactly what was played and the gate carries its own
# control instead of depending on a file somebody may have edited between runs.
#
# SoundPlayer rather than a WASAPI client of its own: it renders through the same
# shared-mode engine mix the tone goes through, and that mix is what loopback captures.
# A control that reached the endpoint by some other route would be exercising a
# different path from the one the arm above measures.
#
# It stops when the marker is deleted, so the gate ends it by rendezvous rather than by
# killing a process on a machine it shares. The seconds it is given are a dead-man's
# switch for a gate that dies before deleting the marker.
cat >"$OUT/silence.ps1" <<'PS1'
param([int]$Seconds = 130)
$ErrorActionPreference = 'Stop'
$rate = 48000
$channels = 2
$bits = 16
$block = $channels * ($bits / 8)
$bytes = $rate * $block
$path = Join-Path $env:TEMP 'lanplay-silence.wav'
$marker = Join-Path $env:TEMP 'lanplay-silence.playing'
Remove-Item $marker -ErrorAction SilentlyContinue
$fs = [System.IO.File]::Create($path)
$w = New-Object System.IO.BinaryWriter($fs)
$w.Write([System.Text.Encoding]::ASCII.GetBytes('RIFF'))
$w.Write([int](36 + $bytes))
$w.Write([System.Text.Encoding]::ASCII.GetBytes('WAVEfmt '))
$w.Write([int]16)
$w.Write([int16]1)
$w.Write([int16]$channels)
$w.Write([int]$rate)
$w.Write([int]($rate * $block))
$w.Write([int16]$block)
$w.Write([int16]$bits)
$w.Write([System.Text.Encoding]::ASCII.GetBytes('data'))
$w.Write([int]$bytes)
$w.Write((New-Object byte[] $bytes))
$w.Close()
$fs.Close()
$player = New-Object System.Media.SoundPlayer $path
$player.Load()
$player.PlayLooping()
Set-Content -Path $marker -Value 'playing'
Write-Output "silence one second of 48000 Hz stereo zeros, looping, up to $Seconds s"
$deadline = (Get-Date).AddSeconds($Seconds)
while ((Get-Date) -lt $deadline -and (Test-Path $marker)) {
    Start-Sleep -Milliseconds 250
}
$player.Stop()
Remove-Item $marker -ErrorAction SilentlyContinue
Write-Output 'silence stopped'
PS1
scp -q "$OUT/silence.ps1" "${WIN_HOST:-windows}:C:/Users/luque/lanplay-silence.ps1"

# The source first, and in the interactive session, because that is where audio
# endpoints exist. Distinct task names: win-session derives its wrapper from the
# task, and two invocations sharing one wrapper overwrite each other's command
# between the copy and the launch - the loser then reports a timeout while the
# winner runs twice.
WIN_TASK=lanplay-tone WIN_TIMEOUT=$((SECONDS_TO_RUN + 120)) "$REPO/tools/win-session.sh" \
    'C:\Users\luque\tone-source.log' \
    "target\\release\\tone-source.exe --seconds $((SECONDS_TO_RUN + 25))" \
    >"$OUT/tone.out" 2>&1 &
tone=$!

for _ in $(seq 1 40); do
    "$REPO/tools/win-ssh.sh" \
        'powershell -NoProfile -Command "(Get-Process tone-source -ErrorAction SilentlyContinue).Count"' \
        2>/dev/null | tr -d '\r ' | grep -q '^[1-9]' && break
    sleep 0.5
done
echo "tone      997 Hz left, 1997 Hz right at -20 dBFS, playing"

WIN_TASK=lanplay-audio WIN_TIMEOUT=$((SECONDS_TO_RUN + 120)) "$REPO/tools/win-session.sh" \
    'C:\Users\luque\audio-capture.log' \
    "target\\release\\audio-capture-probe.exe --seconds $SECONDS_TO_RUN" \
    >"$OUT/probe.out" 2>&1 || true
echo "probe     done"

# The tone has to be gone before the control runs, or the control captures the tone and
# reports that the criteria it exists to fire did not. It is waited for rather than
# ended: the task self-terminates after the seconds it was given, and ending it would
# kill the wrapper before it wrote the sentinel win-session is polling for, which turns
# a finished arm into a three minute timeout.
wait "$tone" 2>/dev/null || true

WIN_TASK=lanplay-silence WIN_TIMEOUT=$((CONTROL_SECONDS + 240)) "$REPO/tools/win-session.sh" \
    'C:\Users\luque\silence-source.log' \
    "powershell -NoProfile -ExecutionPolicy Bypass -File C:\\Users\\luque\\lanplay-silence.ps1 -Seconds $((CONTROL_SECONDS + 120))" \
    >"$OUT/silence.out" 2>&1 &
silence=$!

for _ in $(seq 1 60); do
    "$REPO/tools/win-ssh.sh" "if exist \"$MARKER\" (exit 0) else (exit 1)" >/dev/null 2>&1 && break
    sleep 0.5
done
echo "control   endpoint held open by a stream of zeros, nothing playing into it"

# The probe's own exit code, which the measuring arm has no use for and the control
# does: a control the probe refused would be the probe declining rather than this gate
# catching, and the distinction is the whole reason the arm is shaped this way.
control_exit=0
WIN_TASK=lanplay-audio-control WIN_TIMEOUT=$((CONTROL_SECONDS + 120)) "$REPO/tools/win-session.sh" \
    'C:\Users\luque\audio-control.log' \
    "target\\release\\audio-capture-probe.exe --seconds $CONTROL_SECONDS" \
    >"$OUT/control.out" 2>&1 || control_exit=$?
echo "control   probe done, exit $control_exit"

"$REPO/tools/win-ssh.sh" "del /q \"$MARKER\"" >/dev/null 2>&1 || true
wait "$silence" 2>/dev/null || true

# Both reports in full, and the control's is the one worth the room: a reader who
# doubts it needs the probe's own lines beside the parsed table below, because the
# fastest way to catch a harness reading the wrong thing is to see both.
echo
echo "the measuring arm reported"
echo
cat "$OUT/probe.out"
echo
echo "the control reported, and it has to be caught"
echo
cat "$OUT/control.out"

python3 - "$OUT" "$SECONDS_TO_RUN" "$CONTROL_SECONDS" "$control_exit" <<'PY'
import re
import sys

out = sys.argv[1]
seconds = float(sys.argv[2])
control_seconds = float(sys.argv[3])
control_exit = int(sys.argv[4])
tone_said = open(f"{out}/tone.out").read()

# The contract's tone. Two frequencies rather than one, so channel order and
# distinctness are provable from the samples: a frame count cannot tell captured
# audio from captured silence.
LEFT_HZ, RIGHT_HZ = 997.0, 1997.0
TONE_TOLERANCE_HZ = 5.0
# How much of its nominal length a run must have delivered before its zeros are read
# as results. The endpoint's cadence is fixed and the probe times its own run, so a
# healthy arm lands within a packet or two of rate times seconds; measured, 2880480
# frames against 2880000 nominal over a minute and 480480 against 480000 over ten
# seconds. Nine tenths is not a slow arm, it is a stream that stopped partway, and that
# is the line between a control that fired and a harness that broke - so it is stated
# once and applied to both arms.
MINIMUM_YIELD = 0.9


def arm(name, filename, asked):
    """One report, read once into the fields every criterion is applied to.

    Both arms are read by this one function on purpose. A control parsed by code of
    its own could be caught by a bug in that code rather than by the criteria it
    exists to exercise, and a criterion proven by a second parser is not proven.
    """
    body = open(f"{out}/{filename}").read()

    def field(pattern):
        # Multiline, because every one of these keys is a line in the middle of a
        # report and `^` without it anchors to the start of the whole string. Getting
        # that wrong turned a run with 6001 packets into "no packet was captured": a
        # gate that can read a success as a failure trains its reader to distrust it,
        # which costs more than a gate that is merely absent.
        got = re.search(pattern, body, re.M)
        return got.group(1) if got else None

    mix = re.search(r"^mix format (\d+) Hz (\d+) ch (\d+) bit (float|int)$", body, re.M)
    gaps = re.search(r"^position gaps (\d+) totalling (\d+) frames$", body, re.M)
    detected = re.search(r"^tone left ([\d.]+) right ([\d.]+)$", body, re.M)
    level = re.search(r"^tone level left (\S+) dBFS right (\S+) dBFS$", body, re.M)
    return {
        "name": name,
        "asked": asked,
        "endpoint": field(r"^endpoint (.+)$"),
        "mix": mix,
        "rate": int(mix.group(1)) if mix else None,
        "channels": int(mix.group(2)) if mix else None,
        "period": re.search(r"^buffer period default ([\d.]+) minimum ([\d.]+)$", body, re.M),
        "requested": field(r"^requested seconds ([\d.]+)$"),
        "packets": field(r"^packets (\d+)$"),
        "frames": field(r"^frames captured (\d+)$"),
        "discontinuities": field(r"^discontinuities (\d+)$"),
        "in_flight": field(r"^discontinuities in flight (\d+)"),
        "silent": field(r"^silent packets (\d+)$"),
        "gaps": (gaps.group(1), gaps.group(2)) if gaps else None,
        "positions": re.search(r"^device position first (\d+) last (\d+)$", body, re.M),
        "span": field(r"^qpc span ([\d.]+)$"),
        "detected": (float(detected.group(1)), float(detected.group(2))) if detected else None,
        "distinct": field(r"^tone channels distinct (yes|no)$"),
        "level": (level.group(1), level.group(2)) if level else None,
    }


def evidence(a):
    """What has to hold before any zero in a report is worth reading.

    A run that captured nothing has no discontinuities and no gaps and reads as a
    clean sweep, which is the mistake this project has made five times in five
    subsystems. It is also what tells a control that fired from a harness that broke,
    because a broken harness produces a report shaped exactly like an absence.
    """
    said = []
    if a["packets"] is None or int(a["packets"]) == 0:
        said.append("no packet was captured, so every zero below is an absence and not a result")
    if a["frames"] is None or int(a["frames"]) == 0:
        said.append("no frame was captured")
    # The arm has to be the arm that was asked for. A gate reading another arm's file
    # would be reading a real report of the wrong run, which is how A3's control came
    # to pass on a stream that never went anywhere.
    if a["requested"] is None:
        said.append("the run did not say how long it was asked for")
    elif abs(float(a["requested"]) - a["asked"]) > 0.5:
        said.append(
            f"this report is of a {float(a['requested']):.0f} second run and the arm asked for "
            f"{a['asked']:.0f}, so it is not the run this gate started"
        )
    if a["frames"] and int(a["frames"]) and a["rate"] and a["requested"]:
        nominal = a["rate"] * float(a["requested"])
        if int(a["frames"]) < MINIMUM_YIELD * nominal:
            said.append(
                f"{a['frames']} frames arrived against {nominal:.0f} nominal for "
                f"{float(a['requested']):.0f} seconds at {a['rate']} Hz: the stream stopped partway, "
                "so the report describes less than it claims to"
            )
    return said


def content(a):
    """Whether what arrived is audio, which nothing else in this report can say.

    The silent packet count cannot. Measured on this host, a buffer full of zeros
    arrives with WASAPI's silent flag clear, because the flag is the engine declaring
    a buffer's contents undefined rather than a statement about what is in them, and
    the probe therefore exits 0 on a capture carrying nothing. So the tone detector
    and the distinctness beside it are the only criteria separating a captured tone
    from a captured silence, and they are the ones the control has to fire.
    """
    said = []
    if a["detected"] is None:
        said.append("the tone was not measured, so nothing proves the capture carried audio")
    else:
        left, right = a["detected"]
        if abs(left - LEFT_HZ) > TONE_TOLERANCE_HZ or abs(right - RIGHT_HZ) > TONE_TOLERANCE_HZ:
            said.append(
                f"the tone read {left:.1f} / {right:.1f} Hz against {LEFT_HZ:.0f} / {RIGHT_HZ:.0f}: "
                "either the channels are swapped or what was captured is not the tone"
            )
    if a["distinct"] != "yes":
        said.append("the two channels are not distinct, so the capture is mono or duplicated")
    if a["silent"] is not None and int(a["silent"]) > 0:
        # Counted apart from a discontinuity on purpose: a silent packet with a source
        # playing means the source stalled, which needs the opposite fix.
        said.append(
            f"{a['silent']} silent packets while a source was playing: the source stalled, "
            "not the capture"
        )
    return said


def accounting(a):
    """Exact accounting, which is the criterion. A device position that advances by
    anything other than the previous packet's frame count is a gap of a known size.
    """
    said = []
    if a["gaps"] is None:
        said.append("position gaps were not reported, so frames were counted rather than accounted")
    elif int(a["gaps"][0]) > 0:
        said.append(
            f"{a['gaps'][0]} position gaps totalling {a['gaps'][1]} frames: the capture lost audio"
        )
    # The first packet is always flagged discontinuous: there is no earlier data for it
    # to be continuous with. Counting it as a fault would fail every correct run, so the
    # criterion is the in-flight count the probe reports separately.
    if a["in_flight"] is None:
        said.append("the in-flight discontinuity count was not reported")
    elif int(a["in_flight"]) > 0:
        said.append(f"{a['in_flight']} discontinuities in flight: the device dropped audio mid-stream")
    return said


def measured_rate(a):
    """The rate check, which is what makes the frame count mean something: a capture
    that dropped a stretch and reported no gap shows up here as a rate below nominal.

    Measured between the two device positions rather than from the frame count. Both
    the device position and the QPC position timestamp the FIRST frame of their packet,
    so a span running first-frame to first-frame excludes the last packet's audio while
    the frame count includes it. One 480-frame packet over sixty seconds is 150 ppm,
    which would be the largest term in a gate whose whole subject is drift measured in
    parts per million. The position difference has no such edge: it is frames elapsed
    between exactly the two instants the span was taken at.
    """
    if not (a["positions"] and a["span"] and a["rate"]):
        return None, []
    elapsed_frames = int(a["positions"].group(2)) - int(a["positions"].group(1))
    rate = elapsed_frames / float(a["span"])
    error_ppm = (rate / a["rate"] - 1.0) * 1e6
    line = f"{rate:.2f} Hz against {a['rate']} nominal, {error_ppm:+.0f} ppm"
    if abs(error_ppm) > 5000:
        return line, [
            f"the device position advanced at {rate:.0f} Hz against {a['rate']} nominal, "
            f"{error_ppm:+.0f} ppm: the accounting and the clock disagree"
        ]
    return line, []


measuring = arm("measuring", "probe.out", seconds)
control = arm("control", "control.out", control_seconds)

print("\nwhat the endpoint is\n")
print(f"  endpoint            {measuring['endpoint'] or 'not reported'}")
if measuring["mix"]:
    rate, channels = measuring["rate"], measuring["channels"]
    bits, kind = measuring["mix"].group(3), measuring["mix"].group(4)
    print(f"  mix format          {rate} Hz, {channels} ch, {bits} bit {kind}")
else:
    rate = channels = None
    print("  mix format          not reported")
if measuring["period"]:
    print(
        f"  buffer period       {measuring['period'].group(1)} ms default, "
        f"{measuring['period'].group(2)} ms minimum"
    )

# The finding this phase exists to produce, reported before any verdict because it
# decides what the next phase builds rather than whether this one passed.
if rate is not None:
    if rate == 48000 and channels == 2:
        print("\n  FINDING the endpoint is already 48 kHz stereo, so the path to Opus can be")
        print("          direct and no resampler is needed")
    else:
        print(f"\n  FINDING the endpoint is {rate} Hz {channels} ch, not 48 kHz stereo, so the path")
        print("          to Opus needs conversion and A2 must budget for it")

print("\nwhat the capture did\n")
for label, value in (
    ("packets", measuring["packets"]),
    ("frames captured", measuring["frames"]),
    ("qpc span (s)", measuring["span"]),
    ("discontinuities", measuring["discontinuities"]),
    ("of those, in flight", measuring["in_flight"]),
    ("silent packets", measuring["silent"]),
    ("position gaps", measuring["gaps"][0] if measuring["gaps"] else None),
    ("frames lost to gaps", measuring["gaps"][1] if measuring["gaps"] else None),
    (
        "tone left / right",
        f"{measuring['detected'][0]} / {measuring['detected'][1]} Hz" if measuring["detected"] else None,
    ),
    ("channels distinct", measuring["distinct"]),
):
    print(f"  {label:<22} {value if value is not None else 'not reported'}")

rate_line, rate_failures = measured_rate(measuring)
if rate_line:
    print(f"\n  measured rate          {rate_line}")

# What the control should read when it is working, printed beside what it did read so
# that a reader can tell a fired criterion from a broken harness without coming back to
# this file. The first four numbers are what make it a criterion firing: the endpoint's
# whole cadence, every frame of it accounted for. The last three are what make it a
# criterion rather than a probe declining: nothing in the samples, and a probe that
# exited zero regardless.
nominal_frames = (control["rate"] or 48000) * control_seconds
print("\nthe control: the same probe with no audio in the endpoint\n")
tone_read = (
    f"{control['detected'][0]} / {control['detected'][1]} Hz" if control["detected"] else None
)
level_read = f"{control['level'][0]} / {control['level'][1]} dBFS" if control["level"] else None
for label, value, expected in (
    ("packets", control["packets"], f"about {int(control_seconds) * 100}, a hundred a second"),
    ("frames captured", control["frames"], f"about {nominal_frames:.0f}, the rate times the seconds"),
    ("position gaps", control["gaps"][0] if control["gaps"] else None, "zero, the capture itself is clean"),
    ("discontinuities in flight", control["in_flight"], "zero, nothing dropped mid-stream"),
    ("silent packets", control["silent"], "zero, WASAPI does not flag zeros as silent"),
    ("tone left / right", tone_read, "0.0 / 0.0 Hz, nothing was playing"),
    ("channels distinct", control["distinct"], "no, there is nothing to be distinct"),
    ("tone level", level_read, "-inf dBFS, digital silence"),
    ("probe exit", control_exit, "0, the probe called this run captured"),
):
    print(f"  {label:<26}{str(value) if value is not None else 'not reported':<22} ({expected})")

failures = []
findings = []

# The measuring arm's other criteria are read only once its evidence holds, because a
# report of a run that did not happen has zeros everywhere and would otherwise pass
# every one of them.
absent = evidence(measuring)
failures += absent
if not absent:
    failures += content(measuring) + accounting(measuring) + rate_failures

# The control is judged apart from the measuring arm, because its expectation is the
# opposite one: it passes by being caught. Folding it into the criteria above would have
# the gate fail on the arm that is supposed to break.
missing = evidence(control)
caught = content(control)
if missing:
    failures.append(
        "the control is not evidence: " + "; ".join(missing) + ". An arm that produces "
        "nothing, or a report of a run this gate did not start, is the signature of a broken "
        "harness rather than of a criterion firing, so the criteria that separate audio from "
        "silence are unproven this run"
    )
    for line in open(f"{out}/silence.out").read().splitlines():
        if line.strip():
            findings.append(f"the silence source said: {line.strip()}")
elif not caught:
    failures.append(
        "the control captured an endpoint with no audio in it and this gate read it as the "
        "tone, so either the criteria that separate audio from silence cannot fail, or "
        "something else was playing into the endpoint while the control ran; in both readings "
        "no clean arm this gate has ever passed means anything"
    )
else:
    findings.append(
        "the control held the endpoint open with a stream of zeros and this gate caught it on "
        f"the samples: {'; '.join(caught)}, over {control['packets']} packets and "
        f"{control['frames']} frames with {control['gaps'][0] if control['gaps'] else '?'} position "
        f"gaps and {control['in_flight']} discontinuities in flight, the probe exiting "
        f"{control_exit}. A criterion disagreed over a full run rather than a harness failing to "
        "produce one"
    )
    muddied = accounting(control)
    if muddied:
        findings.append(
            "the control was not as clean as it should have been, which weakens it rather than "
            "the arm above: " + "; ".join(muddied)
        )

if "refus" in tone_said.lower() or "underrun" in tone_said.lower():
    for line in tone_said.splitlines():
        if re.search(r"refus|underrun", line, re.I):
            findings.append(f"the source said: {line.strip()}")

print()
for finding in findings:
    print(f"NOTE {finding}")
if failures:
    for failure in failures:
        print(f"FAIL {failure}")
    sys.exit(1)
print(
    "PASS the endpoint is identified, its format and cadence are reported, every frame is\n"
    "     accounted for by device position, the captured samples are the tone, and the\n"
    "     control proved the criteria that say so can fail"
)
PY
