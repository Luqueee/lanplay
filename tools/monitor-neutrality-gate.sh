#!/usr/bin/env bash
# Whether the passive monitor perturbs the session it is watching, and whether
# this comparison could tell if it did.
#
# A monitor that changes what it measures is worse than no monitor. The argument
# that this one does not is short - one CoreWLAN association read costs 3.2 ms at
# p50 and 15.5 ms at worst, so it gets a thread of its own at 1 Hz and never
# touches a callback - and an argument is not evidence.
#
# So the same 1080p120 workload runs under three monitors and the arms are
# compared on the quantities that would move first if the monitor cost anything:
#
#   delivery cadence at p99          the link's own interval, at the depacketiser
#   crossings of two periods a minute the counted tail, which ranks link changes
#   presented-frame cadence           what the display actually put on screen
#   fresh tick ratio                  the share of refreshes carrying something new
#
# ---------------------------------------------------------------------------
# The positive control, which is the point of this harness
# ---------------------------------------------------------------------------
#
# This repository has already concluded from an absence once, and a comparison
# that finds no difference proves nothing unless it can detect one. So there is
# a third arm, `expensive`: the same sampler with its interval removed, reading
# CoreWLAN as fast as it will answer. The comparison MUST separate that arm from
# the cheap one. If it cannot, the comparison has no power, the neutrality claim
# is unsupported, and REFUSED is the result rather than PASS.
#
# The control is expensive in the same currency any real cost of the monitor
# would be spent in - a thread inside CoreWLAN, contending for the same shared
# client - rather than in an unrelated one. A control that burned CPU in an empty
# loop would prove this comparison detects empty loops.
#
# Rejected: a control that scans. `system_profiler SPAirPortDataType` is known to
# perturb this pipeline - it turned a link delivering at 8.09 ms p50 and 11.35 ms
# p99 into one reading 2.04 ms p50 and 133 ms p99 - so an arm that scanned would
# be re-testing that finding instead of this one, and would violate the no-scan
# assertion below in the same breath.
#
# ---------------------------------------------------------------------------
# The decision rule, stated before any run
# ---------------------------------------------------------------------------
#
# Three runs an arm is too few for a distribution, so the rule is a separation
# rule and not a p-value. Two arms are called different on a metric when neither
# arm's values reach into the other's range at all:
#
#   separated(A, B) = min(A) > max(B) or min(B) > max(A)
#
# Exact and distribution-free. Under the null that the two arms are draws from
# one population, complete separation of three against three has probability
# 2/C(6,3) = 1/10, and that probability falls as runs are added: the criterion
# gains power from more data.
#
# It replaced a rule this harness shipped with and which was wrong in a way worth
# recording, because the mistake is easy to repeat. That rule was
# `|median(A) - median(B)| > max(range(A), range(B))`, and a sample range grows
# with the number of samples for any non-degenerate distribution - so adding runs
# made separation *harder* and no amount of data could ever satisfy it. A
# criterion that cannot be satisfied by collecting more evidence is not a strict
# criterion, it is a broken one.
#
# The replacement was adopted on that argument alone, which needs no reference to
# any measurement, and it changed nothing about the run committed under
# `results/network/`: both rules refuse every one of the twelve arm pairings
# there. Recorded so nobody has to wonder whether a rule was chosen to suit an
# answer.
#
# Power   : `expensive` separated from `off` or from `on`, on at least one metric.
# Neutral : `on` NOT separated from `off`, on every metric.
#
# PASS needs both. Power without neutrality is FAIL - the monitor was detected.
# No power is REFUSED whatever the arms did, because nothing has been shown.
#
# ---------------------------------------------------------------------------
# Read this before adding passes: the comparison cannot work, and it is a
# division rather than a matter of luck
# ---------------------------------------------------------------------------
#
# Four numbers settle it, and none of them is about the weather.
#
#   0.500 ms   spread of delivery p99 across the loopback arms that had no
#              monitor at all, 8.421 to 8.921
#   8.442 ms   the base those sit on, against a 8.333 ms source period
#   ~60 ms/s   what a perturbation must therefore accumulate before a
#              separation rule can see it: about 0.5 ms on each frame it
#              touches, at 120 frames a second
#   3.2 ms/s   what the monitor costs - one CoreWLAN association read a second,
#              measured by tools/radio-sample/examples/read-cost.rs
#
# The effect is about nineteen times smaller than the smallest thing this
# instrument can resolve. Measured at the source it is smaller still, because
# CPU on an idle core costs the receive thread nothing and the only path the two
# threads share is one mutex: a 90 s run with the monitor on holds that lock
# 1.521 ms in total across 31 takes, 0.0017 per cent of the run, which bounds the
# mean delay to any access unit at 0.141 us against a 8333 us period. That is
# three thousand times under the floor.
#
# So two independent positive controls both failed to fire, and a third would
# too. `contend` at 8.462 ms against `off` at 8.442 ms is a twenty microsecond
# difference measured by an instrument whose floor is five hundred. Adding passes
# narrows a median; it does not narrow the spread that sets the floor.
#
# The neutrality claim is therefore a derivation and not a detection, and it is
# in the client's own report under `monitor.cost`: the sampler consumed X of the
# budget on the path it shares, so it cannot account for more than Z of the
# cadence. Derive before building applies to instruments too.
#
# What these arms do establish, which is worth having and is NOT neutrality: the
# monitor's effect is below the resolution of the delivery path. And one thing
# they establish outright - presented Hz read 119.971 with a spread of 0.00 on
# every arm of both runs, radio and loopback, cheap and expensive and contending.
# Not one presented frame was lost to any of them.
#
# ---------------------------------------------------------------------------
# What the two committed runs found, which is about the metrics
# ---------------------------------------------------------------------------
#
# Both runs are REFUSED and the second one explains the first. The finding is not
# "the monitor is cheap" and it is not "the machine has headroom": it is that two
# of the four metrics cannot respond to the perturbation the controls apply, and
# the other two are swamped by the presentation path's own noise floor.
#
# The delivery metrics are immune by construction, and that is a property of a
# deliberate design rather than a defect. `macos/client/src/transport.rs` captures
# `arrival = Timestamp::now()` the instant a datagram is received, at line 322,
# and only then hands it to `delivery.completed(arrival)`, which takes
# `crates/link-metrics`' mutex and records the timestamp it was given. So delay
# imposed on the receive thread - by a lock, by a scheduler, by anything - moves
# when the interval is recorded and never what the interval is. That is exactly
# the "measure a stage at that stage" rule the module exists to enforce, and the
# consequence for this harness is that `delivery p99` and `>2T per min` are
# structurally incapable of seeing receive-thread contention. The contention
# control's failure to separate on them is therefore not evidence of blindness.
# Measured: the control took the mutex 2418288 to 2490350 times per 90 s arm,
# about 27000 a second, and delivery p99 moved from 8.442 ms to 8.462 ms.
#
# The presentation metrics can respond and did, in the predicted direction, and
# were not separable: `presented fps` 119.355 off against 116.478 contending, and
# `fresh tick %` 99.546 against 97.100, with the off arm's own range running to
# 94.766 because this Mac's display link drops frames on its own. Four runs an
# arm cannot resolve a three-point shift against that.
#
# What a run that could answer needs, and none of it is available by running this
# harness for longer: a perturbation that reaches a metric which is not insulated
# from it, or a presentation path quiet enough that a three-point shift clears its
# noise floor. Stated here rather than discovered again.
#
# ---------------------------------------------------------------------------
# Why the arms are rotated rather than blocked
# ---------------------------------------------------------------------------
#
# Running all the `off` arms and then all the `on` arms confounds any drift in
# the link across the harness with the thing being measured, perfectly, and a
# weak moving link is exactly where that bites. This project lost a whole sweep
# to it today.
#
# So pass k runs the arm list rotated left by k-1. With three arms and three
# passes that is a Latin square: each arm occupies position one, two and three
# exactly once, so a monotone drift contributes an identical positional term to
# every arm and cancels rather than becoming the effect. Blocking cancels
# nothing and simple alternation only cancels partly.
#
# ---------------------------------------------------------------------------
# Whether the arms are comparable at all
# ---------------------------------------------------------------------------
#
# The negotiated PHY rate is the mechanism, because airtime is: ten arms at
# -48 dBm and a median 1200 Mbps produced concealment from 0.196 to 7.442 per
# cent, while 3 dB of signal difference between those arms moved the negotiated
# rate by nothing at all. So the comparability criterion is the ratio between
# the highest and lowest per-arm median rate, against a factor of two. Signal is
# recorded beside it and decides nothing.
#
# Rejected: intersecting per-arm p10-p90 rate intervals, which is degenerate
# when the arms are internally flat - two arms each pinned at one rate never
# intersect however close the rates are, so the test refuses exactly the
# cleanest links.
#
# The rate population cannot be taken during the runs. The `off` arm's whole
# definition is that no sampler is running, so characterising it with a sampler
# would destroy it. It is taken in the gaps instead: five seconds at 1 Hz
# immediately before and immediately after each run, outside every measured
# window, and the figure is stated as a boundary measurement because that is
# what it is.
#
#
# ---------------------------------------------------------------------------
# No scan, asserted twice, and each assertion has a companion that can fail
# ---------------------------------------------------------------------------
#
# Structural: the shipped client binary must reference no CoreWLAN scan selector
# and no scanning system tool. Companion: the same search must find the
# association selectors it does use, or the search is looking at the wrong
# strings and its silence means nothing.
#
# Observational: `airportd` logs every scan request with the requesting pid and
# process name, so over each run's own window there must be no
# `SCAN request received from pid N (lanplay-client)`. Companion: there must be
# at least one `SCAN request received` from *somebody* across the harness's whole
# window - `locationd` scans every few minutes on this machine - because a reader
# that sees no scan requests at all cannot have seen ours either.
#
# ---------------------------------------------------------------------------
# usage
# ---------------------------------------------------------------------------
#
#   tools/monitor-neutrality-gate.sh [seconds] [repeats]
#
#     IFACE=en0            which interface receives; must be the radio
#     OUT=...              defaults to results/network/monitor-neutrality-<s>s-<r>x,
#                          so two runs of different shapes cannot overwrite each
#                          other's evidence. The directory is emptied on entry,
#                          because an arm reading a previous run's artefacts is
#                          how two datasets were lost here.
#     MEMORY_SECONDS=600   the ten-minute arm; 0 skips it
#     BITRATE=50
#
# Every arm is a real session over the air with the renderer running, because
# presented-frame cadence is one of the compared quantities and it does not exist
# without a display link. The arms are interleaved rather than blocked, so link
# drift across the harness cannot be attributed to the monitor.

set -euo pipefail

SECONDS_TO_RUN="${1:-120}"
REPEATS="${2:-3}"
# Which link the arms cross. `loopback` is the default because the question is
# local and the radio cannot answer it: see the block below.
LINK="${LINK:-loopback}"
IFACE="${IFACE:-en0}"
BITRATE="${BITRATE:-50}"
MEMORY_SECONDS="${MEMORY_SECONDS:-600}"
REPO="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${OUT:-$REPO/results/network/monitor-neutrality-${LINK}-${SECONDS_TO_RUN}s-${REPEATS}x}"
CLIENT="$REPO/target/release/lanplay-client"
ARMS=(off on expensive contend)

fail() { echo "REFUSED: $*" >&2; exit 2; }

# A fresh directory per invocation, so one arm can never read another run's
# artefacts. Two datasets in this project were lost to exactly that.
rm -rf "$OUT"
mkdir -p "$OUT"
STARTED_AT="$(date '+%Y-%m-%d %H:%M:%S')"
echo "monitor neutrality gate"
echo "  link      $LINK"
echo "  arms      ${ARMS[*]}, $REPEATS rotated repeats of ${SECONDS_TO_RUN}s"
echo "  memory    ${MEMORY_SECONDS}s arm with the monitor on"
echo "  output    $OUT"
echo "  started   $STARTED_AT"
echo

# ---- the link this is measured over ---------------------------------------
#
# Why loopback is the default, which is the correction that made this harness
# able to answer anything at all.
#
# The question is local. A sampler on its own thread cannot perturb the air:
# whatever the monitor costs, it costs through CPU contention on this Mac. The
# first committed run of this gate asked that question through the radio and was
# REFUSED for want of power, and its own numbers say why - the `off` arms alone
# ran 20.62 to 182.66 crossings a minute and 11.93 to 17.79 ms at delivery p99,
# which is the radio at -72 dBm across arms taken minutes apart and has nothing
# to do with a sampler. Reading a local effect through the noisiest remote
# channel available is how a comparison ends up with no power. Its per-window
# data settles that this was not fixable by running longer: the relative scatter
# of the crossing rate is 1.14 over 3 s blocks and 1.04 over 90 s ones, flat
# under averaging, so the link's bursts are correlated across a whole arm and no
# arm length averages them down.
#
# `LINK=radio` keeps that arrangement available, because what it measures is
# worth having on its own: whether the effect is smaller than a real link's own
# variance. It is not what settles neutrality.
RADIO_BEFORE="unavailable"
if [ -x "$REPO/target/release/radio-sample" ]; then
    RADIO_BEFORE="$("$REPO/target/release/radio-sample" 1 1000 | tail -1)"
    echo "radio     $RADIO_BEFORE"
    echo "$RADIO_BEFORE" > "$OUT/radio-before.csv"
else
    fail "build the radio sampler first: cargo build --release -p lanplay-radio-sample"
fi

if [ "$LINK" = "radio" ]; then
    status="$(ifconfig "$IFACE" 2>/dev/null | awk '/status:/{print $2}')"
    [ "$status" = "active" ] || fail "$IFACE is ${status:-missing}"
fi
[ -x "$CLIENT" ] || fail "build the client first: cargo build --release -p lanplay-client"

# ---- structural no-scan assertion -----------------------------------------
# Whether this binary is even capable of a scan, which no run can answer for
# every future run.
SCAN_SELECTORS='scanForNetworks|cachedScanResults|startMonitoringEventWithType|SPAirPortDataType|scanForNetworksWithName'
READ_SELECTORS='rssiValue|noiseMeasurement|transmitRate|wlanChannel'
scan_hits="$(strings -a "$CLIENT" | grep -coE "$SCAN_SELECTORS" || true)"
read_hits="$(strings -a "$CLIENT" | grep -coE "$READ_SELECTORS" || true)"
echo "no-scan   structural: $scan_hits scan selectors, $read_hits association selectors"
[ "$read_hits" -gt 0 ] ||
    fail "the binary contains none of the association selectors it is built on, \
so a search finding no scan selectors in it proves nothing"
[ "$scan_hits" -eq 0 ] &&
    echo "          the client cannot scan: it carries no scan selector at all" ||
    fail "the client references a scan selector; the whole tier is association reads"
echo

# ---- the rate population, and why it is taken in the gaps ------------------
# The negotiated PHY rate characterises each arm, and it is sampled either side
# of every run and never inside one.
#
# Not an optimisation and not laziness: the `off` arm's whole definition is that
# no sampler is running, so a sampler running through it would destroy the thing
# that defines it. That is the same shape of error as measuring a stage through
# a later one - the defect `crates/link-metrics` exists because of - and the
# honest way out is to measure the rate at the arm's boundaries and to say so.
# Do not "fix" this by sampling during the arm.
RATE_SAMPLE_SECONDS=5

bracket_radio() {
    local label=$1 side=$2
    "$REPO/target/release/radio-sample" "$RATE_SAMPLE_SECONDS" 1000 |
        tail -n +2 | sed "s|^|$label,$side,|" >> "$OUT/radio-per-arm.csv"
}

# ---- one arm ---------------------------------------------------------------
run_arm() {
    local monitor=$1 rep=$2 seconds=$3 label
    label="$monitor-r$rep"
    local report="$OUT/$label.json"
    local log="$OUT/$label.log"
    local from_ts

    bracket_radio "$monitor" before
    from_ts="$(date '+%Y-%m-%d %H:%M:%S')"

    printf 'arm       %-14s %ss ... ' "$label" "$seconds"
    # Delivery cadence and presented-frame cadence are both compared, so the
    # renderer runs in both modes.
    local status=0
    if [ "$LINK" = "loopback" ]; then
        # Sender, depacketiser and renderer in one process on this machine. The
        # real RTP packetiser, a real UDP socket and the real depacketiser are
        # all in the path - what is absent is the air, which is the only thing
        # the monitor provably cannot touch. `--require-clean-display` is left
        # off for the same reason it is over the radio: an occluded window is a
        # fight with the window server, and occlusion still reaches
        # `invalidating_events` so no presentation figure can be mistaken for a
        # measurement it is not.
        caffeinate -d "$CLIENT" \
            --transport loopback --mtu 1200 \
            --width 1920 --height 1080 --fps 120 \
            --seconds "$seconds" --fixture-seconds 10 --fixture-dir "$REPO/fixtures" \
            --mode display-link --phase-align off \
            --monitor "$monitor" \
            --window-seconds 10 --report "$report" > "$log" 2>&1 || status=$?
    else
        # `REQUIRE_CLEAN_DISPLAY` stays off for the reason `tools/e2e-gate.sh`
        # gives.
        QUIET=1 IFACE="$IFACE" BITRATE="$BITRATE" REPORT="$report" \
            MONITOR="$monitor" \
            "$REPO/tools/e2e-gate.sh" "$seconds" > "$log" 2>&1 || status=$?
    fi

    if [ ! -s "$report" ]; then
        echo "no report (exit $status)"
        bracket_radio "$monitor" after
        return 1
    fi

    # Observational no-scan assertion, over this run's own window.
    local ours
    ours="$(log show --start "$from_ts" --predicate 'process == "airportd"' --style compact 2>/dev/null |
        grep -cE 'SCAN request received from pid [0-9]+ \(lanplay-client\)' || true)"
    echo "$ours" > "$OUT/$label.scans"
    printf 'ok, %s scans by the client\n' "$ours"
    bracket_radio "$monitor" after
    [ "$ours" -eq 0 ] || fail "$label requested $ours scans; the tier is association reads only"
    return 0
}

# `awk 'NR==1'` rather than `head -1`, which closes the pipe on the sampler's
# second line and kills it with SIGPIPE.
printf 'arm,side,%s\n' \
    "$("$REPO/target/release/radio-sample" 1 1000 | awk 'NR==1')" > "$OUT/radio-per-arm.csv"

# Rotated, not blocked and not merely alternated. Pass k runs the list rotated
# left by k-1, so with three arms and three passes each arm sits in each position
# exactly once and a monotone drift across the harness contributes the same
# positional term to all three and cancels.
for rep in $(seq 1 "$REPEATS"); do
    for index in $(seq 0 $(( ${#ARMS[@]} - 1 ))); do
        arm="${ARMS[$(( (index + rep - 1) % ${#ARMS[@]} ))]}"
        run_arm "$arm" "$rep" "$SECONDS_TO_RUN" || true
    done
done

# ---- the ten-minute arm ----------------------------------------------------
# Memory across a run long enough for a leak to show. The client's own gate
# already decides on this and the report now states it, so nothing here parses a
# sentence.
if [ "$MEMORY_SECONDS" -gt 0 ]; then
    echo
    run_arm on memory "$MEMORY_SECONDS" || true
    mv -f "$OUT/on-rmemory.json" "$OUT/memory.json" 2>/dev/null || true
    mv -f "$OUT/on-rmemory.log" "$OUT/memory.log" 2>/dev/null || true
fi

RADIO_AFTER="$("$REPO/target/release/radio-sample" 1 1000 | tail -1)"
echo "$RADIO_AFTER" > "$OUT/radio-after.csv"

# ---- the companion the observational assertion needs -----------------------
# Zero scans by the client means nothing unless the reader can see a scan at all.
# The companion is a claim about the reader, not about the harness window: it
# asks whether this predicate can see a scan request at all. Scoping it to the
# harness window made it fail on a short run for a reason unrelated to the
# client - locationd scans about every five minutes here, so a two-minute
# harness legitimately contains none - and a companion that refuses for the
# wrong reason is the defect a companion exists to prevent. The per-arm
# assertion above stays scoped to its arm, which is where the claim about the
# client belongs.
# Widened until the predicate demonstrates the capability, and the window that
# demonstrated it is printed. macOS thins its in-memory log for recent windows
# under load - this harness produced 343070 airportd lines in three hours, and
# the same predicate that found 25 scan requests over 180m found 0 over 60m - so
# a fixed window turns a capability check into a lottery on log pressure.
ANY_SCANS=0
SCAN_WINDOW=none
for window in 60m 180m 360m; do
    ANY_SCANS="$(log show --last "$window" --predicate 'process == "airportd"' --style compact 2>/dev/null |
        grep -cE 'SCAN request received from pid' || true)"
    if [ "$ANY_SCANS" -gt 0 ]; then SCAN_WINDOW="$window"; break; fi
done
echo
echo "no-scan   observational: 0 by the client in every arm's own window, and this reader"
echo "          sees $ANY_SCANS scan requests from other processes over the last $SCAN_WINDOW, so its"
echo "          silence about the client is a measurement rather than a blind spot"
[ "$ANY_SCANS" -gt 0 ] ||
    fail "this predicate sees no scan request from any process over six hours, so it \
could not have seen one from the client either; the observational assertion certifies nothing"

# ---- the comparison --------------------------------------------------------
python3 - "$OUT" "$RADIO_BEFORE" "$RADIO_AFTER" <<'PY'
import json, pathlib, statistics, sys

ARM_NAMES = ("off", "on", "expensive", "contend")

out = pathlib.Path(sys.argv[1])
radio_before, radio_after = sys.argv[2], sys.argv[3]

# The quantities that would move first if the monitor cost anything. `worse`
# says which direction is worse, so the report can say which way a separation
# went rather than only that there was one.
METRICS = [
    ("delivery p99 ms",     lambda r: r["delivery"]["au_interval_p99_ms"], "higher"),
    (">2T per min",         lambda r: r["delivery"]["over_2t_per_min"],    "higher"),
    # `display.observed_hz` was here and is a constant: it is derived from the
    # display link's own targetPresentationTimestamp median, so it read exactly
    # 119.971 on all nine arms of the radio run - the panel's clock, not a count
    # of what this pipeline presented. A metric that cannot vary cannot
    # separate, so presented-frame cadence is now frames actually drawn over the
    # span they were drawn in. Replaced on that argument, which needs no
    # reference to which way any effect went.
    ("presented fps",       lambda r: r["display"]["rendered"] / max(r["delivery"]["span_s"], 1e-9), "lower"),
    ("fresh tick %",        lambda r: r["display"]["fresh_tick_ratio"],    "lower"),
]

arms = {}
empty = []
for path in sorted(out.glob("*.json")):
    # The memory arm is judged on its own criterion, and `verdict.json` is this
    # reader's own output: globbing it back in makes the harness unable to
    # re-read a directory it already wrote, which is how a committed result
    # stops being re-checkable.
    if path.name in ("memory.json", "verdict.json"):
        continue
    report = json.loads(path.read_text())
    arm = report["monitor"]["cadence"]
    # A run that received nothing measured nothing, and a zero delivered into a
    # median would read as a link with a flawless cadence. This is also the only
    # thing that catches a media path a firewall dropped while the route check
    # passed on tcp/22, which is a different failure from an unreachable host.
    if report["delivery"]["delivered"] == 0:
        empty.append((path.name, arm, report.get("observation_refused")))
        continue
    arms.setdefault(arm, []).append((path.name, report))

# Per-arm negotiated rate, from the samples taken either side of each run and
# never inside one. The mechanism is airtime and rate is its proxy of record;
# signal is recorded and decides nothing.
rates = {}
rate_csv = out / "radio-per-arm.csv"
if rate_csv.exists():
    for line in rate_csv.read_text().splitlines()[1:]:
        parts = line.split(",")
        # arm, side, then radio-sample's eight committed columns.
        if len(parts) < 10:
            continue
        rates.setdefault(parts[0], {"rate": [], "rssi": []})
        rates[parts[0]]["rate"].append(float(parts[6]))
        rates[parts[0]]["rssi"].append(float(parts[4]))

print()
print("Arms")
for arm in ARM_NAMES:
    runs = arms.get(arm, [])
    print(f"  {arm:<10} {len(runs)} runs")
    for name, report in runs:
        monitor = report["monitor"]
        invalid = report["run"]["invalidated"]
        print(
            f"    {name:<22} reads {monitor['radio_reads']:>6} at "
            f"{monitor['radio_reads_per_s']:>7.2f}/s, worst read "
            f"{monitor['radio_cost_max_ms']:>6.2f} ms, windows "
            f"{monitor['short_windows']:>3}/{monitor['long_windows']:<3}"
            + (f", lock takes {monitor.get('radio_lock_takes', 0)}"
               if monitor.get('radio_lock_takes') else "")
            + ("  INVALIDATED" if invalid else "")
        )

if empty:
    print()
    print("Excluded, received nothing")
    for name, arm, refusal in empty:
        print(f"  {name:<22} {arm:<10} {refusal or 'no refusal recorded'}")

print()
print("Rate per arm, sampled either side of each run and never inside one")
for arm in ARM_NAMES:
    seen = rates.get(arm)
    if not seen:
        print(f"  {arm:<10} no samples")
        continue
    print(f"  {arm:<10} median {statistics.median(seen['rate']):>7.1f} Mbps "
          f"[{min(seen['rate']):.0f}, {max(seen['rate']):.0f}] over "
          f"{len(seen['rate'])} samples, signal median "
          f"{statistics.median(seen['rssi']):>6.1f} dBm")

# Comparability: the ratio between the extreme per-arm median rates, against a
# factor of two. Signal is beside it and decides nothing - ten arms at -48 dBm
# and a median 1200 Mbps produced concealment from 0.196 to 7.442 per cent while
# 3 dB between those arms moved the negotiated rate by nothing at all.
#
# Rejected: intersecting per-arm p10-p90 rate intervals, which is degenerate
# when the arms are internally flat and refuses exactly the cleanest links.
RATE_RATIO_LIMIT = 2.0
comparable, rate_ratio = None, None
medians = [statistics.median(rates[arm]["rate"])
           for arm in ARM_NAMES if rates.get(arm)]
if len(medians) == len(ARM_NAMES) and min(medians) > 0:
    rate_ratio = max(medians) / min(medians)
    comparable = rate_ratio < RATE_RATIO_LIMIT
    print(f"  ratio between the extreme arm medians {rate_ratio:.2f}, "
          f"limit {RATE_RATIO_LIMIT:.1f}: "
          f"{'comparable' if comparable else 'NOT COMPARABLE'}")
else:
    print("  ratio unmeasured: an arm has no rate samples")

# Every arm has to have produced the same amount of work, or the comparison is
# across different workloads rather than across monitors.
#
# Derived from the radio run committed beside this one, where it was violated and
# nobody noticed: its pass-1 arms delivered 119.7 to 119.8 access units a second
# against a 120 nominal - 99.75 to 99.83 per cent - while later arms fell to
# 101.1, or 84 per cent, with packet loss zero on seven of ten. The air delivered
# everything it was given; the host never sent those units. So the session
# degraded monotonically underneath the Latin square, and a square cancels a
# linear positional term, not a producer that stops keeping up halfway through.
# That was a second and independent reason that comparison had no power.
#
# One per cent, which sits far inside the gap between 99.8 and 84 and still above
# the 0.2 per cent the good arms actually showed. Refused before anything is
# compared, because a neutrality comparison across arms that produced different
# amounts of work is not a neutrality comparison.
#
# Deliberately NOT adopted as a fifth metric, however tempting: this Mac cannot
# make a producer emit fewer frames - UDP does not backpressure - so production
# rate measures the other machine and could show a large effect while being
# unable to respond to the thing under test.
PRODUCTION_TOLERANCE = 0.01
print()
print("Production, which every arm must match before any of them are compared")
under = []
for arm in ARM_NAMES:
    for name, report in arms.get(arm, []):
        nominal = report["run"]["target_fps"]
        rate = report["delivery"]["delivered"] / max(report["delivery"]["span_s"], 1e-9)
        short = 1.0 - rate / nominal if nominal else 1.0
        flag = ""
        if short > PRODUCTION_TOLERANCE:
            under.append((name, arm, rate, nominal, short))
            flag = "  UNDER"
        print(f"  {name:<22} {arm:<10} {rate:7.2f} of {nominal:.0f} a second, "
              f"{short * 100:+6.2f}% short{flag}")
if under:
    print()
    print(f"REFUSED: {len(under)} arm(s) produced more than "
          f"{PRODUCTION_TOLERANCE * 100:.0f}% under nominal, so the arms are not the same "
          "workload and comparing them would compare producers rather than monitors: "
          + ", ".join(f"{n} at {r:.1f}/{v:.0f}" for n, _, r, v, _ in under))
    raise SystemExit(2)

missing = [arm for arm in ARM_NAMES if len(arms.get(arm, [])) < 2]
if missing:
    print()
    print(f"REFUSED: fewer than two runs for {', '.join(missing)}; "
          "a spread cannot be taken from one run and a comparison needs both")
    raise SystemExit(2)

def summary(arm, extract):
    values = [extract(report) for _, report in arms[arm]]
    return {
        "median": statistics.median(values),
        "min": min(values),
        "max": max(values),
        "spread": max(values) - min(values),
        "values": values,
    }

def separated(a, b):
    # Complete separation of the two value sets. Exact, distribution-free, and it
    # gains power from added runs, which the range rule it replaced did not.
    return a["min"] > b["max"] or b["min"] > a["max"]

print()
print("Comparison")
print(f"  {'metric':<16} {'off':>24} {'on':>24} {'expensive':>24} {'contend':>24}"
      f"   on/off  exp/off  con/off")
power = False
headroom = False
detected = []
rows = []
for name, extract, worse in METRICS:
    off, on, exp, con = (summary(arm, extract) for arm in ARM_NAMES)
    on_off = separated(on, off)
    exp_off, exp_on = separated(exp, off), separated(exp, on)
    con_off, con_on = separated(con, off), separated(con, on)
    # Power comes from the contention arm, not from the frequency arm. The two
    # controls exercise different things and their failures mean different
    # things, so only the one with a named mechanism gates the verdict.
    power = power or con_off or con_on
    headroom = headroom or exp_off or exp_on
    if on_off:
        detected.append(name)
    def cell(s):
        return f"{s['median']:>8.3f} [{s['min']:>7.3f},{s['max']:>7.3f}]"
    print(f"  {name:<16} {cell(off):>24} {cell(on):>24} {cell(exp):>24} {cell(con):>24}"
          f"   {'YES' if on_off else 'no':>6}  {'YES' if exp_off else 'no':>7}"
          f"  {'YES' if con_off else 'no':>7}")
    rows.append({
        "metric": name, "worse_when": worse,
        "off": off, "on": on, "expensive": exp,
        "on_vs_off_separated": on_off,
        "expensive_vs_off_separated": exp_off,
        "expensive_vs_on_separated": exp_on,
        "contend_vs_off_separated": con_off,
        "contend_vs_on_separated": con_on,
        "contend": con,
    })

memory = None
memory_path = out / "memory.json"
if memory_path.exists():
    report = json.loads(memory_path.read_text())
    memory = report["memory"]
    memory["seconds"] = report["run"]["seconds"]
    memory["cadence"] = report["monitor"]["cadence"]
    print()
    print("Memory")
    slope = memory["steady_slope_mb_per_min"]
    print(f"  {memory['seconds']:.0f} s with the monitor {memory['cadence']}: "
          f"{memory['first_mb']:.1f} MB -> {memory['last_mb']:.1f} MB, peak "
          f"{memory['max_mb']:.1f} MB")
    if slope is None:
        print(f"  slope unmeasured: {memory['steady_samples']} samples after "
              f"{memory['warmup_ms']:.0f} ms of warm-up")
    else:
        print(f"  {slope:+.3f} MB/min in steady state over {memory['steady_samples']} "
              f"samples, allowed {memory['allowed_mb_per_min']:.2f}")

print()
# Power first, and before comparability, because it is the strongest thing this
# harness can say. A comparison that cannot see a sampler hammering CoreWLAN as
# fast as it will answer has no power on this link, and then nothing the other
# two arms did means anything - not even on a link whose arms are comparable.
if not power:
    verdict = "REFUSED"
    why = ("the comparison did not separate the contention control from the cheap "
           "sampler or from no monitor on any metric. That control takes "
           "lanplay-link-metrics' own mutex - the one the receive thread takes on "
           "every access unit - thousands of times a second, so a perturbation "
           "arriving down the one path it is certain to arrive by was invisible: the "
           "comparison is blind, and the absence of a difference between on and off "
           "says nothing whatever about the monitor"
           + (". The frequency control did separate, so the machine does not simply "
              "have unlimited headroom" if headroom else
              ". The frequency control did not separate either, which on its own would "
              "only have said this machine had headroom"))
elif comparable is None:
    verdict = "REFUSED"
    why = ("the comparison has power, but an arm has no rate samples so the arms "
           "cannot be shown to be measurements of the same link")
elif not comparable:
    verdict = "REFUSED"
    why = (f"the comparison has power, but the extreme per-arm median rates differ by "
           f"a factor of {rate_ratio:.2f} against a limit of {RATE_RATIO_LIMIT:.1f}: "
           "these arms are not samples of the same link. The numbers are kept; "
           "re-running on a link that is still moving produces another incomparable set")
elif detected:
    verdict = "FAIL"
    why = ("the comparison has power, the arms are comparable, and it detected the "
           "monitor: " + ", ".join(detected))
elif memory is None:
    verdict = "REFUSED"
    why = ("the comparison has power and did not detect the monitor, but no "
           "ten-minute arm ran, so nothing was measured about memory")
elif memory["steady_slope_mb_per_min"] is None:
    verdict = "REFUSED"
    why = ("the comparison has power and did not detect the monitor, but the "
           "memory arm produced too few samples to fit a slope through")
elif memory["steady_slope_mb_per_min"] > memory["allowed_mb_per_min"]:
    verdict = "FAIL"
    why = (f"resident memory grew {memory['steady_slope_mb_per_min']:+.3f} MB/min "
           f"with the monitor on, past the {memory['allowed_mb_per_min']:.2f} allowed")
else:
    verdict = "PASS"
    why = ("the comparison separates the expensive sampler from the cheap one, does "
           "not separate the cheap one from no monitor at all, the arms are samples "
           "of the same link, and memory is flat across ten minutes")

print(f"{verdict}: {why}")
(out / "verdict.json").write_text(json.dumps({
    "verdict": verdict, "why": why,
    "has_power": power, "frequency_control_separated": headroom,
    "detected_on_metrics": detected,
    "comparable": comparable, "rate_ratio": rate_ratio,
    "rate_ratio_limit": RATE_RATIO_LIMIT,
    "rule": "separated(A,B) = min(A) > max(B) or min(B) > max(A), complete separation of the value sets; exact and distribution-free, p = 1/10 under the null for three against three",
    "comparability_rule": "max(per-arm median rate) / min(per-arm median rate) < 2",
    "arm_order": "pass k runs the arm list rotated left by k-1",
    "metrics": rows,
    "rates": {arm: {"median": statistics.median(v["rate"]),
                    "min": min(v["rate"]), "max": max(v["rate"]),
                    "samples": len(v["rate"]),
                    "rssi_median": statistics.median(v["rssi"])}
              for arm, v in rates.items()},
    "excluded_empty": [{"run": n, "arm": a, "refusal": r} for n, a, r in empty],
    "memory": memory,
    "radio_before": radio_before, "radio_after": radio_after,
}, indent=2, default=str) + "\n")
print(f"verdict written to {out / 'verdict.json'}")
raise SystemExit(0 if verdict == "PASS" else 1)
PY
