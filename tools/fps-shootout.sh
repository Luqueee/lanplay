#!/usr/bin/env bash
# N4-B: whether reducing the frame rate fixes cadence degradation.
#
# The failure the whole network phase exists to avoid is a controller that looks
# intelligent and, faced with a cadence problem, starts lowering bitrate. N4 is
# where that gets settled, and it gets settled by deciding which intervention
# fixes which fault before any intervention is wired to any detection.
#
# ---------------------------------------------------------------------------
# The matrix, derived before anything is run
# ---------------------------------------------------------------------------
#
# Three interventions - bitrate, frame rate, resolution - against the conditions
# this project has actually observed. The conditions are not a wish list: the
# ground-truth table in `tools/classify-sessions.sh` is one row per committed
# session with the commit its diagnosis comes from, and what it names is four
# Healthy, eight CadenceDegraded and one TransientStall. `CapacityPressure`
# appears in that file zero times.
#
#   condition          bitrate down        frame rate down     resolution down
#   Healthy            nothing to fix      nothing to fix      nothing to fix
#   TransientStall     must not act        must not act        must not act
#   CadenceDegraded    largely falsified   THIS HARNESS        waits on this cell
#   CapacityPressure   a debt, not a run   UNREACHABLE         UNREACHABLE
#   SevereLoss         UNREACHABLE         UNREACHABLE         UNREACHABLE
#
# Cell by cell, with what decides it.
#
# **CapacityPressure against anything is unreachable, and that is a statement
# worth making rather than an omission.** `crates/network-health` never returns
# that variant and says why in its own doc comment: its discriminator as
# `NETWORK.md` states it needs PHY capacity, which is a `RadioHint` and therefore
# barred from deciding, and the stream-side substitute needs two windows at
# different bitrates. N3's own run prints the debt in words - "CapacityPressure
# UNCONFIRMED, waiting on a session that ever exhibited it, and
# tools/bitrate-sweep.sh output that NETWORK.md records as not in results/". No
# session in this repository has ever exhibited it, so no cell in that row can be
# run by anybody, and an experiment that claimed to intervene on it would be
# inventing its own condition. The same holds for `SevereLoss`: the committed
# envelopes carry no datagram population, so no loss rate is derivable from any of
# them, and the tier that would confirm it landed after all of them were written.
#
# **CapacityPressure against bitrate reduction is a debt rather than an
# experiment.** The belief that lowering bitrate protects integrity around the
# knee is the one cell everyone assumes is settled, and in this repository it is
# owed rather than shown: `tools/bitrate-sweep.sh` is a registered gate and
# nothing under `results/` holds its output. That is a commit of an existing
# sweep's output, not a new design, and until it lands the claim may not be cited
# as a reason for anything.
#
# **CadenceDegraded against bitrate reduction is largely falsified already and is
# not re-run here.** Moving from channel 116 to channel 36 took access units
# arriving more than two source periods late from 69 a minute to 5.5, recorded in
# `crates/capabilities/src/wifi.rs`: a link-side change fixing a cadence problem
# at fixed bitrate. `TASKS.md` section 2.1 already lists "reducir bitrate como
# solución automática a cualquier stall" among the decisions not to reopen without
# new evidence. Re-running it would cost half an hour to re-establish a closed
# decision, so it stays closed unless the configuration changes materially.
#
# **CadenceDegraded against resolution reduction waits on this cell.** N4-C is
# explicit that resolution comes after bitrate and frame rate, and it shares this
# harness's whole difficulty: a smaller frame is fewer datagrams per access unit,
# which is a capacity argument, and there is no confirmed capacity condition to
# aim it at. It gets designed when this one has an answer.
#
# **Healthy and TransientStall need no experiment in any column.** There is
# nothing to fix in the first, and in the second the correct action is no action -
# the failure this phase exists to avoid is a controller that reacts to one 80 ms
# stall, which is why `classify` returns `TransientStall` before it looks at how
# much of the stream was affected.
#
# So one cell is worth running, and it is the one that answers the question a
# controller will actually face: if the link delivers one frame badly every
# 8.3 ms, does it become stable when the temporal obligation is 11.1 or 16.7 ms?
#
# ---------------------------------------------------------------------------
# Why the condition is injected rather than waited for
# ---------------------------------------------------------------------------
#
# An experiment that needs the weather to misbehave is an experiment that reports
# whatever the afternoon gave it. The disturbance here is `udp-fault` holding
# every datagram for 60 ms every 150 ms, seeded, on this side of the radio, and
# the air is still under every arm.
#
# That injection is sized against this link rather than against taste, and the
# sizing is N2's: 60 ms held every 150 ms produces about 400 threshold crossings a
# minute against the 162 a minute N1 measured between arms with nothing running at
# all. The earlier draft of 120 ms every 1500 ms would have sat inside that noise.
# The A6 control of 400 ms every 2 s was rejected there and stays rejected: at
# 50 Mbps it queues some 2000 datagrams and what arrives is the relay's release
# burst rather than the link.
#
# The hold rather than more loss is the whole point. `udp-fault` holds datagrams
# and releases them together, which is what an access point going off channel
# does, and `crates/link-metrics` counts that as a stall followed by units
# arriving early - bunching, which is the fault a stall counter exists to separate
# from loss. Loss stays at zero, which is what makes the arm CadenceDegraded
# rather than SevereLoss.
#
# The injection's period is also what makes the control quantitative rather than
# merely present: 60000/150 is 400 occurrences a minute, and each occurrence is a
# gap of at least 60 ms, which is past two source periods at 120, 90 and 60 fps
# alike. So the control predicts about 400 crossings a minute at every rate, from
# its configuration and before any arm runs, and the baseline arm is refused if it
# does not show them.
#
# ---------------------------------------------------------------------------
# What the one real mechanism in this corpus predicts, before any arm runs
# ---------------------------------------------------------------------------
#
# An injected disturbance can be accused of being unrepresentative, so it is worth
# asking what the real one would do. This repository has measured exactly one
# cadence mechanism in the field, and `crates/capabilities/src/wifi.rs` records it
# precisely: a 34 ms stall every 220 ms on channel 116, which took access units
# arriving more than two source periods late from 69 a minute to 5.5 and more than
# four periods late from 42 a minute to 1.5 when the channel changed.
#
# Divide that 34 ms by each candidate period. It is 4.08 periods at 120 fps, 3.06
# at 90 and 2.04 at 60. So the real mechanism crosses two periods at every rate
# this harness tests, and at 60 fps it does so with forty microseconds of margin.
# Lowering the frame rate would move those 69 crossings a minute to 69 crossings a
# minute.
#
# That is a prediction rather than a result, and it is written here before the run
# rather than fitted to it afterwards. It also states the shape of disturbance that
# frame rate could help with: one longer than two of the old periods and shorter
# than two of the new, which for 120 down to 60 is a window from 16.7 to 33.3 ms.
# Nothing in this repository's corpus falls in it.
#
# ---------------------------------------------------------------------------
# Six arms, and which comparison decides
# ---------------------------------------------------------------------------
#
#   clean120 clean90 clean60   the link as it is, at each rate
#   hold120  hold90  hold60    the same rates behind the same injection
#
# `hold120` against `clean120` establishes that this harness can see a cadence
# disturbance at all. It is the negative control and it decides nothing about
# frame rate: the two differ by the relay as well as by the hold, which is fine
# for a power check and would not be fine for an effect.
#
# `hold90` and `hold60` against `hold120` is the experiment. All three carry the
# same relay, the same seed and the same absolute disturbance, so the only thing
# that moves is how long the stream has to deliver each frame.
#
# The clean triple is not decoration. It is what says whether the rate on its own
# changes anything when nothing is wrong, and it carries the cost side: a source
# below the display's rate cannot fill every refresh, and that price is real
# whether or not the intervention buys anything.
#
# ---------------------------------------------------------------------------
# The virtual display stays at 120 Hz throughout
# ---------------------------------------------------------------------------
#
# The sender chooses which of the display's frames to transmit; the display's mode
# is never touched. A network experiment and an IddCx mode switch landing in one
# test would leave neither answerable, and IDD-LAB stays at 1920x1080 at 120 Hz
# from the first arm to the last.
#
# That is `--mode paced --fps N` on the host for the reduced rates, which waits on
# an absolute grid of 1/N and takes whatever the capture has ready. The baseline
# is `--mode uncapped`, which follows the source, and the asymmetry is deliberate
# rather than tolerated: pacing at the source's own rate makes two clocks at one
# nominal rate beat, which reads as a capture p50 of exactly one frame period and
# a throughput near 110, and `TASKS.md` section 2.1 lists it among the closed
# decisions. The baseline arm has to be what the product actually does, and what
# the product does at 120 is follow the source. Pacing is safe below the source
# rate for a stateable reason: the deadline period is strictly longer than the
# source period, so a deadline never lands waiting for a vblank that has not
# happened. Measured at 20 s per rate before this harness was written, the host
# held 120.05, 90.03 and 60.04 of its nominal.
#
# ---------------------------------------------------------------------------
# Every threshold normalised by the arm's own period
# ---------------------------------------------------------------------------
#
# `crates/link-metrics` counts crossings against multiples of the source period -
# `THRESHOLDS` are 1.25, 1.5, 2, 3, 4 and 6 - and the client builds its
# `Delivery` from `1000/feed_fps`, so telling the client `--fps 90` normalises
# every counter in its report to an 11.11 ms period with no second set of
# multiples anywhere. A raw millisecond comparison across 120, 90 and 60 fps
# compares different questions, and a p99 that halves because T doubled is not an
# improvement.
#
# Both are reported and the report says which decides. The normalised crossings
# and clusters are the metrics that define the condition in `classify`, so they
# are the metrics an intervention has to move; the raw milliseconds are printed
# beside them under a heading that says no criterion reads them, because they are
# what tells a reader whether the link changed or only the obligation did.
#
# ---------------------------------------------------------------------------
# The controls that have to hold before anything is ranked
# ---------------------------------------------------------------------------
#
# **Comparability.** The criterion that survived A8: the ratio between the extreme
# per-arm median PHY rates, against a factor of two. Signal is recorded beside it
# and decides nothing - ten arms at -48 dBm and a median 1200 Mbps produced
# concealment from 0.196 to 7.442 per cent while 3 dB between those arms moved the
# negotiated rate by nothing at all. The rate population comes from the client's
# own 1 Hz sampler running inside each arm, which is a population per arm rather
# than a reading per arm: N2 watched `tx_rate_mbps` fall from 432 to 103 Mbps
# between the two boundary reads of one five-second probe, so one reading cannot
# characterise anything. Sampling inside the arm is available here and is not in
# `tools/monitor-neutrality-gate.sh` for a reason that does not apply: there is no
# monitor-off arm to destroy, every arm runs the sampler identically.
#
# **Under-production.** An arm whose delivered rate falls short of its own nominal
# by more than one per cent is refused before anything is compared. This check
# exists because it has already fired for real: N1's arms delivered 101 to 120
# access units a second against a target of 120 while packet loss was zero on
# seven of ten, so the air delivered everything it was given and those units were
# never sent. Crossings counted against a period the host was not holding measure
# the producer and not the link. One per cent of the nominal, which sits far
# inside the gap between the 99.8 per cent the good arms held and the 84 per cent
# the bad ones did. The same rule is reached twice here, independently: this
# harness checks it, and `crates/network-health`'s own reader refuses a session
# below 99 per cent of `run.target_fps` with the shortfall named.
#
# **Power.** The negative control must fire. `hold120` must separate from
# `clean120` on a deciding metric and must show at least three quarters of the 400
# crossings a minute its own configuration predicts. A run where the metrics did
# not move is otherwise indistinguishable from a run where they could not, and the
# absence of a difference between hold arms would then say nothing whatever.
#
# **A condition to intervene on.** `hold120` must be labelled CadenceDegraded by
# `crates/network-health`, not by eye. Every arm is labelled by that classifier
# and the label is printed beside the expectation stated here, because an arm this
# harness calls CadenceDegraded and the classifier calls something else is a
# finding about one of them.
#
# ---------------------------------------------------------------------------
# The order of the arms, and why it is rotated
# ---------------------------------------------------------------------------
#
# Pass k runs the arm list rotated left by k-1. This project has twice been bitten
# by a monotone drift confounded with the effect: N1's first two arms sat at 11.9
# and 13.2 ms delivery p99 and everything from the fourth on sat at 17.2 to 18.6,
# a settling step in the first few minutes that blocking would have handed
# entirely to whichever arm ran first.
#
# Six arms and three passes is a partial square rather than a complete one. Each
# arm occupies three different positions and never the same one twice, so a
# monotone drift contributes a term that differs between arms by two slots rather
# than by the length of the harness; exact cancellation would need six passes and
# ninety minutes of a link this variable. The position each arm ran in is recorded
# per run, so a reader can check that the ordering did not line up with the
# finding instead of taking it on trust.
#
# ---------------------------------------------------------------------------
# usage
# ---------------------------------------------------------------------------
#
#   tools/fps-shootout.sh [seconds] [repeats]
#
#     IFACE=en0          which interface receives; must be the radio
#     BITRATE=50         encoder target, held constant so frame rate is the only
#                        variable. At 60 fps each access unit then carries more
#                        bits and more datagrams, which is a property of the
#                        intervention rather than a confound to remove
#     RELAY_PORT=5116    this harness's own port, never 5106: that one belongs to
#                        udp-fault by convention across this repository and a
#                        relay from another worktree owned it while this was
#                        being written
#     SEED=20250815      the injection's seed, so a run repeats exactly
#     OUT=...            defaults to results/network/fps-shootout-<s>s-<r>x
#
# Everything the run needs on the Windows side must already be up: the IDD-LAB
# controller and a present-source covering the virtual monitor. `tools/e2e-gate.sh`
# brings both up and locks the host's GPU clocks.

set -euo pipefail

SECONDS_TO_RUN="${1:-120}"
REPEATS="${2:-3}"
IFACE="${IFACE:-en0}"
BITRATE="${BITRATE:-50}"
PORT="${PORT:-5004}"
RELAY_PORT="${RELAY_PORT:-5116}"
SEED="${SEED:-20250815}"
# The injection. Read by the analysis as well as by the relay, because the
# predicted crossing rate is derived from it rather than stated twice.
HOLD_MS="${HOLD_MS:-60}"
HOLD_EVERY_MS="${HOLD_EVERY_MS:-150}"
REPO="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${OUT:-$REPO/results/network/fps-shootout-${SECONDS_TO_RUN}s-${REPEATS}x}"
CLIENT="$REPO/target/release/lanplay-client"
RELAY="$REPO/target/release/udp-fault"
SAMPLER="$REPO/target/release/radio-sample"
CLASSIFY="$REPO/target/release/classify-sessions"

ARMS=(clean120 clean90 clean60 hold120 hold90 hold60)

fail() { echo "REFUSED: $*" >&2; exit 2; }

# A fresh directory per invocation, so one arm can never read another run's
# artefacts. Two datasets in this project were lost to exactly that.
rm -rf "$OUT"
mkdir -p "$OUT/arms" "$OUT/label"
STARTED_AT="$(date '+%Y-%m-%d %H:%M:%S')"

echo "fps shootout, N4-B"
echo "  question  does reducing the frame rate fix cadence degradation"
echo "  arms      ${ARMS[*]}, $REPEATS rotated passes of ${SECONDS_TO_RUN}s"
echo "  injection ${HOLD_MS} ms held every ${HOLD_EVERY_MS} ms, seed $SEED, on the hold arms only"
echo "  display   IDD-LAB stays at 1920x1080 120 Hz; the sender chooses which frames to send"
echo "  bitrate   ${BITRATE} Mbps on every arm"
echo "  output    $OUT"
echo "  started   $STARTED_AT"
echo

# ---- preconditions --------------------------------------------------------

status="$(ifconfig "$IFACE" 2>/dev/null | awk '/status:/{print $2}')"
[ "$status" = "active" ] || fail "$IFACE is ${status:-missing}, and this experiment is about the air"
LOCAL_IP="$(ipconfig getifaddr "$IFACE" || true)"
[ -n "$LOCAL_IP" ] || fail "$IFACE is up but has no IPv4 address"

for binary in "$CLIENT" "$RELAY" "$SAMPLER" "$CLASSIFY"; do
    [ -x "$binary" ] || fail "$binary is not built"
done

# The relay's port, checked rather than assumed. A gate that shares a port with
# the tool it starts measures whichever process won the race, and a relay left
# running by another harness in another worktree cost two arms of
# tools/net-preflight-gate.sh and looked like a hang.
if lsof -nP -iUDP:"$RELAY_PORT" >/dev/null 2>&1; then
    fail "UDP $RELAY_PORT is already held: $(lsof -nP -iUDP:"$RELAY_PORT" | awk 'NR==2{print $1, $2}')"
fi

# Boundary reads of the radio, for the record. The population that decides
# comparability is sampled inside each arm by the client's own monitor.
RADIO_BEFORE="$("$SAMPLER" 1 1000 | tail -1)"
echo "radio     $RADIO_BEFORE"
echo

# ---- one arm --------------------------------------------------------------

# fps, sender mode and whether the injection is in the path, from the arm name.
arm_fps() { case "$1" in *120) echo 120 ;; *90) echo 90 ;; *60) echo 60 ;; esac; }
arm_mode() { case "$1" in *120) echo uncapped ;; *) echo paced ;; esac; }
arm_held() { case "$1" in hold*) echo 1 ;; *) echo 0 ;; esac; }

POSITIONS="$OUT/positions.tsv"
printf 'run\tarm\tpass\tposition\n' > "$POSITIONS"

run_arm() {
    local arm=$1 rep=$2 position=$3
    local label="$arm-r$rep"
    local report="$OUT/arms/$label.json"
    local log="$OUT/$label.log"
    local fps mode held relay_pid=0 send_port="$PORT"

    fps="$(arm_fps "$arm")"
    mode="$(arm_mode "$arm")"
    held="$(arm_held "$arm")"

    printf 'arm       %-12s pass %s position %s, %s fps %s ... ' \
        "$label" "$rep" "$position" "$fps" "$mode"
    printf '%s\t%s\t%s\t%s\n' "$label" "$arm" "$rep" "$position" >> "$POSITIONS"

    if [ "$held" = 1 ]; then
        # A fresh relay per arm, so its seeded sequence starts at the same place
        # for every hold arm and its counters belong to one run.
        "$RELAY" --listen "0.0.0.0:$RELAY_PORT" --forward "$LOCAL_IP:$PORT" \
            --stall-ms "$HOLD_MS" --stall-every-ms "$HOLD_EVERY_MS" --seed "$SEED" \
            > "$OUT/$label.relay.out" 2>&1 &
        relay_pid=$!
        send_port="$RELAY_PORT"
        for _ in $(seq 1 50); do
            grep -q '^udp-fault:' "$OUT/$label.relay.out" && break
            sleep 0.1
        done
        if ! grep -q '^udp-fault:' "$OUT/$label.relay.out"; then
            kill "$relay_pid" 2>/dev/null || true
            fail "the relay never announced itself for $label: $(cat "$OUT/$label.relay.out")"
        fi
    fi

    # `REQUIRE_CLEAN_DISPLAY=0` for the reason tools/e2e-gate.sh gives: an
    # occluded window is a fight with the window server, occlusion still reaches
    # `invalidating_events`, and no presentation figure can then be mistaken for
    # something it is not. `PHASE_ALIGN=off` because the phase loop acts on the
    # host's capture timing and its own cadence test only passes near one source
    # period, so leaving it on would let it act on one arm and not another.
    local status=0
    QUIET=1 IFACE="$IFACE" BITRATE="$BITRATE" PORT="$PORT" SEND_PORT="$send_port" \
        FPS="$fps" SOURCE_MODE="$mode" PHASE_ALIGN=off MONITOR=on \
        REQUIRE_CLEAN_DISPLAY=0 REPORT="$report" \
        "$REPO/tools/e2e-gate.sh" "$SECONDS_TO_RUN" > "$log" 2>&1 || status=$?

    if [ "$relay_pid" != 0 ]; then
        kill "$relay_pid" 2>/dev/null || true
        wait "$relay_pid" 2>/dev/null || true
    fi

    if [ ! -s "$report" ]; then
        # A degraded arm is expected to fail the client's own gate, so the exit
        # code is a data point rather than an abort. A missing report is not: it
        # means nothing was measured.
        echo "no report (exit $status)"
        return 1
    fi
    printf 'ok (gate exit %s)\n' "$status"
    return 0
}

for rep in $(seq 1 "$REPEATS"); do
    for index in $(seq 0 $(( ${#ARMS[@]} - 1 ))); do
        arm="${ARMS[$(( (index + rep - 1) % ${#ARMS[@]} ))]}"
        run_arm "$arm" "$rep" "$(( index + 1 ))" || true
    done
    echo
done

RADIO_AFTER="$("$SAMPLER" 1 1000 | tail -1)"
printf '%s\n%s\n' "$RADIO_BEFORE" "$RADIO_AFTER" > "$OUT/radio-boundary.csv"

# ---- the radio trace each arm sampled for itself --------------------------
# Written out beside each report in the column layout `crates/network-health`
# reads, so the classifier's own radio column carries a per-arm median taken
# during the run rather than the absence a missing file would give it.
python3 - "$OUT/arms" <<'PY'
import json, pathlib, sys

arms = pathlib.Path(sys.argv[1])
for path in sorted(arms.glob("*.json")):
    trace = json.loads(path.read_text()).get("monitor", {}).get("radio_trace") or []
    if not trace:
        continue
    lines = ["at_s,rssi_dbm,noise_dbm,tx_rate_mbps,channel,width_mhz,cost_ms"]
    for row in trace:
        lines.append(",".join(str(row[key]) for key in (
            "at_s", "rssi_dbm", "noise_dbm", "tx_rate_mbps",
            "channel", "width_mhz", "cost_ms")))
    path.with_suffix(".wifi.csv").write_text("\n".join(lines) + "\n")
PY

# ---- what the classifier calls each arm -----------------------------------
# Asked rather than reimplemented, and asked without parsing a word of its
# output: the classifier's contract is a three-valued exit code against a table
# of expectations, so one probe per candidate condition names the label with no
# reader in between. CadenceDegraded is probed first because it is the one every
# hold arm is expected to be.
#
# The radio trace is deliberately left out of the scratch directory a probe reads,
# and that is worth stating because it looks like an oversight. `classify-sessions`
# refuses a corpus in which every session carries a radio trace, on the ground
# that `NetworkObservation.radio` being an `Option` is the property the contract
# turns on and a run where nothing exercised it certified nothing. That guard is
# right for the corpus-wide validation it was written for and it makes a
# single-session probe unanswerable whenever the session has a trace, so the probe
# asks about the middle tier alone - which is the only tier `classify` can read.
# The trace stays beside the committed report for anything that comes later.
CANDIDATES=(CadenceDegraded Healthy TransientStall UnknownDegradation SevereLoss CapacityPressure REFUSED)
LABELS="$OUT/labels.tsv"
printf 'run\tlabel\n' > "$LABELS"
echo "labels    asking crates/network-health what each arm is"
for report in "$OUT"/arms/*.json; do
    name="$(basename "$report")"
    scratch="$OUT/label/${name%.json}"
    mkdir -p "$scratch"
    cp "$report" "$scratch/$name"
    found=none
    for candidate in "${CANDIDATES[@]}"; do
        printf '%s\t%s\t%s\n' "$name" "$candidate" "probe" > "$scratch/expect.tsv"
        if "$CLASSIFY" --results "$scratch" --expect "$scratch/expect.tsv" --as-found \
            > "$scratch/out" 2>&1; then
            found="$candidate"
            break
        fi
    done
    if [ "$found" = none ]; then
        fail "no condition, no refusal and no unreadable verdict fits $name; the classifier \
and this harness disagree about what its vocabulary is, which is a defect in one of them and not \
a result about a link. Its last answer: $(cat "$scratch/out")"
    fi
    printf '%s\t%s\n' "${name%.json}" "$found" >> "$LABELS"
    printf '  %-16s %s\n' "${name%.json}" "$found"
done
echo

# ---- the comparison -------------------------------------------------------
python3 - "$OUT" "$HOLD_MS" "$HOLD_EVERY_MS" "$RADIO_BEFORE" "$RADIO_AFTER" <<'PY'
import json, pathlib, statistics, sys

out = pathlib.Path(sys.argv[1])
hold_ms, hold_every_ms = float(sys.argv[2]), float(sys.argv[3])
radio_before, radio_after = sys.argv[4], sys.argv[5]

ARM_NAMES = ("clean120", "clean90", "clean60", "hold120", "hold90", "hold60")
BASELINE = "hold120"
CONTROL_AGAINST = "clean120"
EXPERIMENT = ("hold90", "hold60")

# One per cent of the arm's own nominal. See the header: N1's arms fell to 84 per
# cent while the good ones held 99.8, and crossings counted against a period the
# host was not holding measure the producer.
PRODUCTION_TOLERANCE = 0.01
# The ratio between the extreme per-arm median PHY rates. A8's criterion.
RATE_RATIO_LIMIT = 2.0
# The control has to show at least this share of the crossings its own period
# predicts. One-sided: the injection cannot produce fewer occurrences than its
# own rate unless something absorbed them, and more is the link adding its own,
# which the separation test covers.
CONTROL_FLOOR_SHARE = 0.75

labels = {}
for line in (out / "labels.tsv").read_text().splitlines()[1:]:
    run, label = line.split("\t")
    labels[run] = label

arms, empty = {}, []
for path in sorted((out / "arms").glob("*.json")):
    report = json.loads(path.read_text())
    arm = path.stem.rsplit("-r", 1)[0]
    # A run that received nothing measured nothing, and a zero delivered into a
    # median reads as a link with a flawless cadence.
    if report["delivery"]["delivered"] == 0:
        empty.append((path.stem, arm, report.get("observation_refused")))
        continue
    arms.setdefault(arm, []).append((path.stem, report))

positions = {}
for line in (out / "positions.tsv").read_text().splitlines()[1:]:
    run, arm, rep, position = line.split("\t")
    positions[run] = (int(rep), int(position))


def rate_samples(report):
    return [row["tx_rate_mbps"] for row in report["monitor"]["radio_trace"]]


def rssi_samples(report):
    return [row["rssi_dbm"] for row in report["monitor"]["radio_trace"]]


def period_ms(report):
    return 1000.0 / report["run"]["target_fps"]


print("Arms, and the position each ran in")
for arm in ARM_NAMES:
    for name, report in arms.get(arm, []):
        rep, position = positions.get(name, (0, 0))
        print(f"  {name:<16} pass {rep} position {position}, T {period_ms(report):5.2f} ms, "
              f"{labels.get(name, '?'):<19} "
              f"{'INVALIDATED' if report['run']['invalidated'] else ''}")
if empty:
    print()
    print("Excluded, received nothing")
    for name, arm, refusal in empty:
        print(f"  {name:<16} {arm:<10} {refusal or 'no refusal recorded'}")

# ---- under-production, before anything is compared ----
print()
print("Production, which every arm must hold before any of them are compared")
under = []
for arm in ARM_NAMES:
    for name, report in arms.get(arm, []):
        nominal = report["run"]["target_fps"]
        rate = report["delivery"]["delivered"] / max(report["delivery"]["span_s"], 1e-9)
        short = 1.0 - rate / nominal if nominal else 1.0
        flag = "  UNDER" if short > PRODUCTION_TOLERANCE else ""
        if flag:
            under.append((name, rate, nominal, short))
        print(f"  {name:<16} {rate:7.2f} of {nominal:.0f} a second, {short * 100:+6.2f}% short{flag}")

# ---- comparability ----
print()
print("Rate per arm, sampled at 1 Hz inside each run by the client's own monitor")
rates, rssis = {}, {}
for arm in ARM_NAMES:
    samples = [value for _, report in arms.get(arm, []) for value in rate_samples(report)]
    signal = [value for _, report in arms.get(arm, []) for value in rssi_samples(report)]
    if not samples:
        print(f"  {arm:<10} no samples")
        continue
    rates[arm] = statistics.median(samples)
    rssis[arm] = statistics.median(signal)
    print(f"  {arm:<10} median {rates[arm]:>7.1f} Mbps [{min(samples):.0f}, {max(samples):.0f}] "
          f"over {len(samples)} samples, signal median {rssis[arm]:>6.1f} dBm")

comparable, rate_ratio = None, None
if len(rates) == len(ARM_NAMES) and min(rates.values()) > 0:
    rate_ratio = max(rates.values()) / min(rates.values())
    comparable = rate_ratio < RATE_RATIO_LIMIT
    print(f"  ratio between the extreme arm medians {rate_ratio:.2f}, limit "
          f"{RATE_RATIO_LIMIT:.1f}: {'comparable' if comparable else 'NOT COMPARABLE'}")
else:
    print("  ratio unmeasured: an arm has no rate samples")

# ---- the two tables, and which one decides ----
DECIDING = [
    (">2T/min", lambda r: r["delivery"]["over_2t_per_min"]),
    ("clusters/min", lambda r: r["delivery"]["stall_clusters_per_min"]),
]
RAW = [
    ("p50 ms", lambda r: r["delivery"]["au_interval_p50_ms"]),
    ("p99 ms", lambda r: r["delivery"]["au_interval_p99_ms"]),
    ("max ms", lambda r: r["delivery"]["au_interval_max_ms"]),
    ("stall gap p50 ms", lambda r: r["delivery"]["stall_gap_p50_ms"]),
]


def spread(arm, extract):
    values = [extract(report) for _, report in arms.get(arm, [])]
    if not values:
        return None
    return {"median": statistics.median(values), "min": min(values),
            "max": max(values), "values": values}


def separated(a, b):
    # Complete separation of the two value sets. Exact, distribution-free, and it
    # gains power from added runs rather than losing it, which a range rule this
    # project tried first did not.
    return a["min"] > b["max"] or b["min"] > a["max"]


def table(title, metrics, note):
    print()
    print(title)
    print(f"  {note}")
    header = "  " + f"{'arm':<10}" + f"{'T ms':>7}" + "".join(f"{name:>22}" for name, _ in metrics)
    print(header)
    for arm in ARM_NAMES:
        runs = arms.get(arm, [])
        if not runs:
            print(f"  {arm:<10}{'-':>7}")
            continue
        cells = ""
        for _, extract in metrics:
            s = spread(arm, extract)
            cells += f"{s['median']:>9.1f} [{s['min']:>5.1f},{s['max']:>5.1f}]"
        print(f"  {arm:<10}{period_ms(runs[0][1]):>7.2f}{cells}")


table("Normalised by each arm's own period - THE VERDICT READS THESE",
      DECIDING,
      "counted crossings of two of this arm's source periods, and the stalls among "
      "them that were followed by units arriving early")
table("Raw milliseconds - REPORTED, AND READ BY NO CRITERION HERE",
      RAW,
      "the same runs in absolute time, where 120, 90 and 60 fps are three different "
      "questions; a p99 that halves because T doubled is not an improvement")

# ---- the cost ----
# Freshness is measured at presentation, so it carries the display's faults and
# `run.invalidated` is the client's own way of saying something moved underneath a
# run and its presentation numbers cannot be trusted. Those runs are dropped from
# this table alone: the delivery tier above is timestamped at the depacketiser and
# is unaffected, which is the whole reason the two are separate sections.
print()
print("Cost - reported. Only new loss or decode errors can refuse an arm; the rest is the price")
print(f"  {'arm':<10}{'fresh frames/s':>16}{'ceiling':>10}{'fresh %':>9}"
      f"{'datagrams lost':>16}{'decode errors':>15}{'  presentation runs used':>26}")
new_cost = []
invalidated = [name for arm in ARM_NAMES for name, report in arms.get(arm, [])
               if report["run"]["invalidated"]]
for arm in ARM_NAMES:
    runs = arms.get(arm, [])
    if not runs:
        continue
    fresh, ceiling, lost, errors = [], None, 0, 0
    for name, report in runs:
        display = report["display"]
        hz = display["nominal_hz"]
        ceiling = min(report["run"]["target_fps"], hz)
        if not report["run"]["invalidated"]:
            fresh.append(display["fresh_tick_ratio"] / 100.0 * hz)
        lost += report["stream"]["packet_loss"]
        errors += report["decode"]["errors"]
    if lost or errors:
        new_cost.append((arm, lost, errors))
    used = f"{len(fresh)} of {len(runs)}"
    if not fresh:
        print(f"  {arm:<10}{'unavailable':>16}{ceiling:>10.1f}{'-':>9}{lost:>16}{errors:>15}"
              f"{used:>26}")
        continue
    print(f"  {arm:<10}{statistics.median(fresh):>16.1f}{ceiling:>10.1f}"
          f"{100.0 * statistics.median(fresh) / ceiling:>9.1f}{lost:>16}{errors:>15}{used:>26}")
print("  the ceiling is min(fps, display Hz): a source below the display's rate cannot fill")
print("  every refresh, so the ratio is reported against what the arm could reach and the")
print("  absolute rate is reported beside it, because that is what the eye receives")
if invalidated:
    print(f"  {len(invalidated)} run(s) had something move underneath them and are excluded from")
    print(f"  this table only: {', '.join(invalidated)}")

# ---- power: did the negative control fire ----
print()
print("The negative control, which has to fire")
predicted = 60000.0 / hold_every_ms
floor = predicted * CONTROL_FLOOR_SHARE
control = spread(BASELINE, DECIDING[0][1])
against = spread(CONTROL_AGAINST, DECIDING[0][1])
control_separated = control is not None and against is not None and separated(control, against)
control_reached = control is not None and control["min"] >= floor
print(f"  {hold_ms:.0f} ms held every {hold_every_ms:.0f} ms predicts {predicted:.0f} crossings a "
      f"minute from its own period, before any arm ran")
if control is None or against is None:
    print("  unmeasured: an arm is missing")
else:
    print(f"  {BASELINE} measured {control['median']:.1f} [{control['min']:.1f}, "
          f"{control['max']:.1f}] against a floor of {floor:.0f}: "
          f"{'reached' if control_reached else 'NOT REACHED'}")
    print(f"  {BASELINE} against {CONTROL_AGAINST} at {against['median']:.1f} "
          f"[{against['min']:.1f}, {against['max']:.1f}]: "
          f"{'separated' if control_separated else 'NOT SEPARATED'}")
    # The same ratio for the two experimental arms, which is not a criterion and is
    # the most legible thing in the run: the injection recurs every 150 ms and each
    # occurrence is a gap of at least 60 ms, which is past two source periods at
    # 8.33, 11.11 and 16.67 ms alike. If the measured rate tracks the injection's
    # own period at every frame rate, then the crossing rate is set by the
    # disturbance and not by the stream, which is the mechanism behind whatever
    # this run concludes.
    print("  the same ratio at the other two rates, which decides nothing and explains much")
    for arm in (BASELINE,) + EXPERIMENT:
        seen = spread(arm, DECIDING[0][1])
        if seen is None:
            continue
        print(f"    {arm:<10} {seen['median']:>7.1f} of {predicted:.0f} predicted, "
              f"{seen['median'] / predicted:.2f}x")

# ---- the causal criterion ----
print()
print("The causal criterion, applied to the metrics that define the condition")
print(f"  an arm mitigates only if both {DECIDING[0][0]} and {DECIDING[1][0]} are completely")
print(f"  separated below {BASELINE}'s and the classifier stops calling it CadenceDegraded")
mitigates, findings = [], []
baseline_labels = [labels.get(name) for name, _ in arms.get(BASELINE, [])]
for arm in EXPERIMENT:
    if not arms.get(arm):
        findings.append({"arm": arm, "improved": {}, "mitigates": False, "labels": []})
        print(f"  {arm:<10} no runs")
        continue
    improved, detail = {}, []
    for name, extract in DECIDING:
        here, base = spread(arm, extract), spread(BASELINE, extract)
        better = separated(here, base) and here["median"] < base["median"]
        improved[name] = better
        detail.append(f"{name} {here['median']:.1f} vs {base['median']:.1f} "
                      f"{'IMPROVED' if better else 'not separated below'}")
    arm_labels = [labels.get(name) for name, _ in arms[arm]]
    left = all(label != "CadenceDegraded" for label in arm_labels)
    verdict_arm = all(improved.values()) and left
    if verdict_arm:
        mitigates.append(arm)
    findings.append({"arm": arm, "improved": improved, "mitigates": verdict_arm,
                     "labels": arm_labels})
    print(f"  {arm:<10} {'; '.join(detail)}")
    print(f"  {'':<10} classifier says {', '.join(sorted(set(arm_labels)))}: "
          f"{'no longer CadenceDegraded' if left else 'still CadenceDegraded'}")

# ---- verdict ----
print()
if under:
    verdict, finding = "REFUSED", None
    why = (f"{len(under)} arm(s) delivered more than {PRODUCTION_TOLERANCE * 100:.0f} per cent "
           "under their own nominal, so their crossings are counted against a period the host "
           "was not holding and measure the producer rather than the link: "
           + ", ".join(f"{n} at {r:.1f}/{v:.0f}" for n, r, v, _ in under))
elif any(len(arms.get(arm, [])) < 2 for arm in ARM_NAMES):
    verdict, finding = "REFUSED", None
    thin = [arm for arm in ARM_NAMES if len(arms.get(arm, [])) < 2]
    why = (f"fewer than two runs for {', '.join(thin)}; a spread cannot be taken from one run "
           "and complete separation is a statement about value sets")
elif comparable is None:
    verdict, finding = "REFUSED", None
    why = ("an arm sampled no PHY rate, so the arms cannot be shown to be measurements of the "
           "same link")
elif not comparable:
    verdict, finding = "REFUSED", None
    why = (f"the extreme per-arm median rates differ by a factor of {rate_ratio:.2f} against a "
           f"limit of {RATE_RATIO_LIMIT:.1f}: these arms are not samples of one link. The "
           "numbers are kept; re-running on a link that is still moving produces another "
           "incomparable set")
elif not control_reached or not control_separated:
    verdict, finding = "REFUSED", None
    why = ("the negative control did not fire, so a run where the frame rate changed nothing is "
           "indistinguishable from a run where nothing could have been seen. "
           + ("The injection did not reach the crossing rate its own period predicts. "
              if not control_reached else "")
           + ("The disturbed baseline did not separate from the clean one at the same rate. "
              if not control_separated else ""))
elif any(label != "CadenceDegraded" for label in baseline_labels):
    verdict, finding = "REFUSED", None
    why = ("the disturbed baseline is not CadenceDegraded according to "
           f"crates/network-health - it is {', '.join(sorted(set(str(l) for l in baseline_labels)))} "
           "- so there was no instance of the condition for a lower frame rate to fix, and any "
           "improvement would be attributable to nothing")
elif new_cost:
    verdict, finding = "FAIL", None
    why = ("the arms are comparable and the control fired, but an arm introduced a cost that is "
           "not part of the bargain and indicts the run rather than the frame rate: "
           + ", ".join(f"{arm} lost {lost} datagrams with {errors} decode errors"
                       for arm, lost, errors in new_cost))
elif mitigates:
    verdict = "PASS"
    finding = ("FPS REDUCTION MITIGATES CADENCE DEGRADATION at " + ", ".join(mitigates))
    why = ("the arms are comparable, every arm held its nominal, the control fired, the baseline "
           "is CadenceDegraded, and the metrics that define that condition are completely "
           "separated below it at " + ", ".join(mitigates))
else:
    verdict = "PASS"
    finding = "FPS IS NOT A NETWORK MITIGATION for cadence degradation, and stays out of the controller"
    why = ("the experiment could have come out either way and came out negative. The arms are "
           "comparable, every arm held its nominal, the control fired by the amount its own "
           "period predicts, and the baseline is CadenceDegraded - so the instrument was "
           "working and the condition was present. Under the same absolute disturbance, "
           "normalising every threshold by the arm's own period, neither 90 nor 60 fps "
           "separated below 120 on the metrics that define the condition. The mechanism is "
           "legible in the numbers: a periodic disturbance longer than two source periods "
           "produces one crossing per occurrence whatever the frame rate, so the crossing rate "
           "is set by the disturbance's period and not by the stream's. Lowering the rate can "
           "only help where the disturbance falls between two old periods and two new ones, "
           "which is a narrow window and not a mitigation")

print(f"verdict   {verdict}")
if finding:
    print(f"finding   {finding}")
print(f"why       {why}")

(out / "verdict.json").write_text(json.dumps({
    "gate": "fps-shootout",
    "phase": "N4-B",
    "verdict": verdict,
    "finding": finding,
    "why": why,
    "question": "does reducing the frame rate fix cadence degradation",
    "decides_on": "normalised crossings of two source periods a minute, and stall clusters a "
                  "minute; the raw millisecond percentiles are reported and read by no criterion",
    "separation_rule": "separated(A,B) = min(A) > max(B) or min(B) > max(A), complete separation "
                       "of the value sets; exact and distribution-free, p = 1/20 one-directional "
                       "for three against three",
    "arm_order": "pass k runs the arm list rotated left by k-1; six arms and three passes is a "
                 "partial square, and the position each arm ran in is in positions.tsv",
    "display": "IDD-LAB held at 1920x1080 120 Hz throughout; the sender chose which frames to "
               "transmit with --mode paced below 120 and --mode uncapped at 120",
    "injection": {"hold_ms": hold_ms, "every_ms": hold_every_ms,
                  "predicted_crossings_per_min": predicted,
                  "floor": floor, "reached": control_reached,
                  "separated_from_clean": control_separated},
    "production_tolerance": PRODUCTION_TOLERANCE,
    "under_production": [{"run": n, "rate": r, "nominal": v, "short": s} for n, r, v, s in under],
    "comparable": comparable, "rate_ratio": rate_ratio, "rate_ratio_limit": RATE_RATIO_LIMIT,
    "rate_median_mbps": rates, "rssi_median_dbm": rssis,
    "labels": labels,
    "positions": {name: {"pass": rep, "position": position}
                  for name, (rep, position) in positions.items()},
    "deciding": {arm: {name: spread(arm, extract) for name, extract in DECIDING}
                 for arm in ARM_NAMES if arms.get(arm)},
    "raw_ms": {arm: {name: spread(arm, extract) for name, extract in RAW}
               for arm in ARM_NAMES if arms.get(arm)},
    "findings": findings,
    "mitigates": mitigates,
    "new_cost": [{"arm": a, "datagrams_lost": l, "decode_errors": e} for a, l, e in new_cost],
    "excluded_empty": [{"run": n, "arm": a, "refusal": r} for n, a, r in empty],
    "radio_before": radio_before, "radio_after": radio_after,
}, indent=2, default=str) + "\n")
print(f"verdict written to {out / 'verdict.json'}")
raise SystemExit({"PASS": 0, "FAIL": 1, "REFUSED": 2}[verdict])
PY
