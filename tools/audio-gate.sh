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
# usage:
#   tools/audio-gate.sh [seconds]

set -euo pipefail

SECONDS_TO_RUN="${1:-60}"
REPO="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${OUT:-/tmp/audio-gate/$(date +%Y%m%d-%H%M%S)}"

mkdir -p "$OUT"
echo "results   $OUT"

"$REPO/tools/win-sync.sh" >/dev/null
"$REPO/tools/win-ssh.sh" 'cd C:\Users\luque\lanplay-rs && cargo build --release -q -p lanplay-audio-capture -p lanplay-tone-source' >/dev/null
echo "built     probe and tone source"

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

wait "$tone" 2>/dev/null || true
echo
cat "$OUT/probe.out"

python3 - "$OUT" "$SECONDS_TO_RUN" <<'PY'
import re
import sys

out, seconds = sys.argv[1], float(sys.argv[2])
probe = open(f"{out}/probe.out").read()
tone = open(f"{out}/tone.out").read()

# The contract's tone. Two frequencies rather than one, so channel order and
# distinctness are provable from the samples: a frame count cannot tell captured
# audio from captured silence.
LEFT_HZ, RIGHT_HZ = 997.0, 1997.0
TONE_TOLERANCE_HZ = 5.0


def field(pattern, body=None):
    # Multiline, because every one of these keys is a line in the middle of a
    # report and `^` without it anchors to the start of the whole string. Getting
    # that wrong turned a run with 6001 packets into "no packet was captured": a
    # gate that can read a success as a failure trains its reader to distrust it,
    # which costs more than a gate that is merely absent.
    got = re.search(pattern, body if body is not None else probe, re.M)
    return got.group(1) if got else None


endpoint = field(r"^endpoint (.+)$")
mix = re.search(r"^mix format (\d+) Hz (\d+) ch (\d+) bit (float|int)$", probe, re.M)
period = re.search(r"^buffer period default ([\d.]+) minimum ([\d.]+)$", probe, re.M)
packets = field(r"^packets (\d+)$")
frames = field(r"^frames captured (\d+)$")
discontinuities = field(r"^discontinuities (\d+)$")
silent = field(r"^silent packets (\d+)$")
gaps = re.search(r"^position gaps (\d+) totalling (\d+) frames$", probe, re.M)
positions = re.search(r"^device position first (\d+) last (\d+)$", probe, re.M)
span = field(r"^qpc span ([\d.]+)$")
detected = re.search(r"^tone left ([\d.]+) right ([\d.]+)$", probe, re.M)
distinct = field(r"^tone channels distinct (yes|no)$")

print("\nwhat the endpoint is\n")
print(f"  endpoint            {endpoint or 'not reported'}")
if mix:
    rate, channels, bits, kind = int(mix.group(1)), int(mix.group(2)), mix.group(3), mix.group(4)
    print(f"  mix format          {rate} Hz, {channels} ch, {bits} bit {kind}")
else:
    rate = channels = None
    print("  mix format          not reported")
if period:
    print(f"  buffer period       {period.group(1)} ms default, {period.group(2)} ms minimum")

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
    ("packets", packets),
    ("frames captured", frames),
    ("qpc span (s)", span),
    ("discontinuities", discontinuities),
    ("of those, in flight", field(r"^discontinuities in flight (\d+)")),
    ("silent packets", silent),
    ("position gaps", gaps.group(1) if gaps else None),
    ("frames lost to gaps", gaps.group(2) if gaps else None),
    ("tone left / right", f"{detected.group(1)} / {detected.group(2)} Hz" if detected else None),
    ("channels distinct", distinct),
):
    print(f"  {label:<22} {value if value is not None else 'not reported'}")

failures = []
findings = []

# Nothing may pass by having produced no evidence. A run with no packets has zero
# discontinuities and zero gaps, and would otherwise read as a clean sweep.
if packets is None or int(packets) == 0:
    failures.append("no packet was captured, so every zero below is an absence and not a result")
if frames is None or int(frames) == 0:
    failures.append("no frame was captured")
if detected is None:
    failures.append("the tone was not measured, so nothing proves the capture carried audio")
else:
    left, right = float(detected.group(1)), float(detected.group(2))
    if abs(left - LEFT_HZ) > TONE_TOLERANCE_HZ or abs(right - RIGHT_HZ) > TONE_TOLERANCE_HZ:
        failures.append(
            f"the tone read {left:.1f} / {right:.1f} Hz against {LEFT_HZ:.0f} / {RIGHT_HZ:.0f}: "
            "either the channels are swapped or what was captured is not the tone"
        )
if distinct != "yes":
    failures.append("the two channels are not distinct, so the capture is mono or duplicated")

# Exact accounting, which is the criterion. A device position that advances by
# anything other than the previous packet's frame count is a gap of a known size.
if gaps is None:
    failures.append("position gaps were not reported, so frames were counted rather than accounted")
elif int(gaps.group(1)) > 0:
    failures.append(
        f"{gaps.group(1)} position gaps totalling {gaps.group(2)} frames: the capture lost audio"
    )
# The first packet is always flagged discontinuous: there is no earlier data for it
# to be continuous with. Counting it as a fault would fail every correct run, so the
# criterion is the in-flight count the probe reports separately.
in_flight = field(r"^discontinuities in flight (\d+)")
if in_flight is None:
    failures.append("the in-flight discontinuity count was not reported")
elif int(in_flight) > 0:
    failures.append(f"{in_flight} discontinuities in flight: the device dropped audio mid-stream")
if silent is not None and int(silent) > 0:
    # Counted apart from a discontinuity on purpose: a silent packet with a source
    # playing means the source stalled, which needs the opposite fix.
    failures.append(
        f"{silent} silent packets while the tone was playing: the source stalled, not the capture"
    )

# The rate check, which is what makes the frame count mean something: a capture that
# dropped a stretch and reported no gap shows up here as a rate below nominal.
#
# Measured between the two device positions rather than from the frame count. Both
# the device position and the QPC position timestamp the FIRST frame of their packet,
# so a span running first-frame to first-frame excludes the last packet's audio while
# the frame count includes it. One 480-frame packet over sixty seconds is 150 ppm,
# which would be the largest term in a gate whose whole subject is drift measured in
# parts per million. The position difference has no such edge: it is frames elapsed
# between exactly the two instants the span was taken at.
if positions and span and rate:
    elapsed_frames = int(positions.group(2)) - int(positions.group(1))
    measured = elapsed_frames / float(span)
    error_ppm = (measured / rate - 1.0) * 1e6
    print(f"\n  measured rate         {measured:.2f} Hz against {rate} nominal, {error_ppm:+.0f} ppm")
    if abs(error_ppm) > 5000:
        failures.append(
            f"the device position advanced at {measured:.0f} Hz against {rate} nominal, "
            f"{error_ppm:+.0f} ppm: the accounting and the clock disagree"
        )

if "refus" in tone.lower() or "underrun" in tone.lower():
    for line in tone.splitlines():
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
    "     accounted for by device position, and the captured samples are the tone"
)
PY
