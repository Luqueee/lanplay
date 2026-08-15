#!/usr/bin/env bash
# A control connection wedged until its writes time out costs a 120 Hz producer
# nothing it can spend.
#
# The control plane and the media path share a process, and the control plane
# talks to a peer that can stop reading at any moment. When it does, the socket
# buffer fills, the server's writes block and then time out, and the connection
# stays wedged for seconds. The whole reason the control plane is given a thread
# and a socket of its own is that none of this may reach the frames.
#
# That is a measurement and not logic, which is why it is not in `cargo test`.
# The claim is about a machine holding a cadence, and a test run can only pass
# or fail, so on a runner with three cores and the rest of a test binary on the
# other two it fails and blames the code. Here the two are separable: the arms
# are ignored tests, `cargo test -- --ignored` runs exactly them and nothing
# else, and what the numbers are worth is decided below rather than by libtest.
#
# `docs/testing.md` asks for one JSON envelope per probe and `xtask verdict` to
# decide it, and this gate does not do that, so here is why rather than an
# oversight. The instrument is a test binary and not a probe: it exists so that
# `cargo test -- --ignored` names exactly these two runs, and the alternative -
# a test that writes a file to a path handed to it in the environment - is the
# environment-dependent test this whole change is removing. What the envelope
# arrangement is for is kept: the run names every number, this gate refuses when
# any of them is missing rather than reading an absence as a zero, and the bound
# travels in the line so that the criterion is stated once and applied where it
# was measured. The arms do assert, which a probe would not; the two cannot
# disagree because the gate applies the arm's own printed bound to the arm's own
# printed number, and an arm that ends badly while every criterion here holds is
# reported as a run nobody has read rather than as a pass.
#
# What this gate requires of the machine: three cores it can have to itself for
# the length of a run. One is for the producer, which spins out the last three
# milliseconds of every 8.33 ms period and has to be resident when its deadline
# arrives; one is for the writer hammering the wedged socket; one is for
# everything else the machine is doing. That is stated rather than probed. The
# evidence is the quiet half of each arm, which is produced with the control
# plane entirely absent: a machine that could not hold 120 Hz there was in no
# position to be asked the question, and this gate refuses such a run instead of
# reporting a difference between two numbers that both describe the scheduler.
#
# What it deliberately does not cover: anything about a real display, a real
# encoder or a real link. The producer is a paced loop and nothing here draws a
# frame. It says the control plane cannot reach the media path, not that the
# media path is fast enough - `e2e-gate` is where that lives.
#
# The arm that must fail is `contended`. It is the same wedge, the same write
# timeout and the same filler, sent from the producer's own thread instead of
# from a thread of its own - the arrangement this design rejected. A run in
# which the isolated arm looks clean and the contended arm looks clean too is a
# run whose instrument reads clean whatever happens, which is the shape of both
# false passes this project has had, so that outcome fails here rather than
# passing twice.
#
# usage:
#   tools/cadence-isolation-gate.sh
#
# exit 0  the claim held
# exit 1  it did not, and the block above the verdict says which arm and by how
#         much
# exit 2  refused: the machine was in no position to answer, and nothing here
#         says anything about isolation either way

set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
# Stamped and never cleared. Six minutes of measurement was lost once to a gate
# that emptied its output directory on startup, so re-running it to re-read a
# verdict deleted the verdict.
OUT="${OUT:-/tmp/cadence-isolation-gate/$(date +%Y%m%d-%H%M%S)}"

mkdir -p "$OUT"
echo "results   $OUT"
echo "requires  three cores free for about half a minute; the quiet half of each"
echo "          arm is the evidence, and a run that could not hold 120 Hz in it"
echo "          is refused rather than reported"

# An arm interrupted between its two halves leaves a test binary holding a
# loopback port and a thread writing into it. The next thing to bind fails for a
# reason that has nothing to do with it, and from the outside that is
# indistinguishable from a real failure.
cleanup() {
    pkill -f "$REPO/target/debug/deps/control-" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

# Built before anything is timed. Compiling inside an arm would put a linker on
# the cores the producer needs and charge it to the control plane. A build that
# fails is refused here rather than left to leave with cargo's own code, which is
# none of the three answers stated above: an instrument that would not compile
# measured nothing, and a caller reading 101 as a failed claim would be told this
# gate had an opinion about isolation.
cargo test -q -p lanplay-transport --test control --no-run || {
    echo
    echo "REFUSE the arms would not build, so nothing was measured and nothing here says"
    echo "       anything about isolation either way"
    exit 2
}

# ---- arms ------------------------------------------------------------------
# `--test-threads=1` because the neighbours are the whole reason this is not an
# ordinary test: libtest runs the other tests in the binary at the same time,
# and on a small machine they are not free and do not land equally on the two
# halves of a run.

run_arm() {
    local arm="$1" test_name="$2"
    local status=0
    cargo test -q -p lanplay-transport --test control -- \
        --ignored --exact "$test_name" --nocapture --test-threads=1 \
        >"$OUT/$arm.out" 2>&1 || status=$?
    echo "$status" >"$OUT/$arm.status"
    echo "arm       $arm done"
}

run_arm wedged a_wedged_control_connection_does_not_perturb_a_120hz_producer
run_arm contended a_producer_that_sends_on_the_control_plane_itself_is_wrecked_by_the_same_wedge

# ---- verdict ---------------------------------------------------------------

set +e
python3 - "$OUT" <<'PY'
import pathlib
import sys

out = pathlib.Path(sys.argv[1])
MARKER = "cadence-isolation "
# Named because the two arms are judged by the same criterion and have to
# disagree about it: the first must hold it and the second must break it.
ARMS = {"wedged": True, "contended": False}
NEEDED = (
    "period_ns tolerance_ns baseline_intervals stalled_intervals baseline_p99_ns "
    "stalled_p99_ns baseline_max_ns stalled_max_ns perturbation_ns frames_written "
    "blocked_writes"
).split()

REFUSED = 2


def refuse(lines):
    print()
    for index, line in enumerate(lines):
        print(("REFUSE " if index == 0 else "       ") + line)
    sys.exit(REFUSED)


def read(arm):
    """The arm's one measurement line, or a refusal.

    A missing key is refused rather than defaulted. A sibling harness read 6001
    captured packets as none because a pattern did not match, and a gate that
    can read a run as an absence is worse than no gate.
    """
    path = out / f"{arm}.out"
    if not path.is_file():
        refuse([f"the {arm} arm produced no output at all, so there is nothing to read"])
    found = [
        line[len(MARKER):]
        for line in path.read_text().splitlines()
        if line.startswith(MARKER)
    ]
    if len(found) != 1:
        refuse([
            f"the {arm} arm printed {len(found)} measurement lines and this gate reads",
            f"exactly one; what it did print is in {path}",
        ])
    fields = {}
    for token in found[0].split():
        name, _, value = token.partition("=")
        fields[name] = value
    missing = [name for name in NEEDED if name not in fields]
    if missing:
        refuse([
            f"the {arm} arm named none of {', '.join(missing)}, so the run and this",
            f"gate no longer agree on what was measured; its output is in {path}",
        ])
    numbers = {name: int(fields[name]) for name in NEEDED}
    numbers["status"] = int((out / f"{arm}.status").read_text().strip())
    return numbers


measured = {arm: read(arm) for arm in ARMS}
ms = lambda ns: ns / 1_000_000.0

# ---- refusals, before anything is judged -----------------------------------
# The quiet half is the only witness this gate has to whether the machine was
# holding the cadence at all. If it was not, the difference between the two
# halves describes the scheduler, and the difference being small describes it
# no better than the difference being large.
for arm, run in measured.items():
    slack = run["baseline_p99_ns"] - run["period_ns"]
    if slack > run["tolerance_ns"]:
        refuse([
            f"the quiet half of the {arm} arm held {ms(run['baseline_p99_ns']):.3f} ms at p99 against an",
            f"{ms(run['period_ns']):.3f} ms period, which is {ms(slack):.3f} ms past the {ms(run['tolerance_ns']):.3f} ms this",
            "comparison is stated in. The machine was not producing at 120 Hz with nothing",
            "in its way, so nothing here says anything about isolation. Free three cores",
            "and run it again.",
        ])

# One bound, taken from the runs rather than restated here, because a criterion
# written down twice is a criterion that drifts.
tolerances = {run["tolerance_ns"] for run in measured.values()}
if len(tolerances) != 1:
    refuse([
        f"the arms were judged against different bounds ({sorted(tolerances)} ns), so they",
        "are not two readings of one measurement",
    ])
tolerance = tolerances.pop()

# ---- findings, above the verdict so they survive a failure -----------------
print()
for arm, run in measured.items():
    print(
        f"  FINDING  {arm:<9} quiet p99 {ms(run['baseline_p99_ns']):8.3f} ms, worst {ms(run['baseline_max_ns']):8.3f} ms"
        f"   over {run['baseline_intervals']} intervals"
    )
    print(
        f"  FINDING  {arm:<9} wedged p99 {ms(run['stalled_p99_ns']):7.3f} ms, worst {ms(run['stalled_max_ns']):8.3f} ms"
        f"   over {run['stalled_intervals']} intervals"
    )
    print(
        f"  FINDING  {arm:<9} the wedge moved the producer by {ms(run['perturbation_ns']):+.3f} ms at p99"
    )

# ---- verdict ---------------------------------------------------------------
faults = []

print()
print("  must not be zero")
for arm, run in measured.items():
    for name, population in (
        ("quiet intervals", run["baseline_intervals"]),
        ("wedged intervals", run["stalled_intervals"]),
        ("control frames written before the buffers filled", run["frames_written"]),
        ("writes that timed out afterwards", run["blocked_writes"]),
    ):
        print(f"    {arm:<9} {population:>6}  {name}")
        if population == 0:
            faults.append(
                f"the {arm} arm reports no {name}, so it never held the condition it names"
            )

print("  must be zero")
for arm, isolated in ARMS.items():
    run = measured[arm]
    inside = run["perturbation_ns"] < tolerance
    if isolated:
        broke = 0 if inside else 1
        print(
            f"    {arm:<9} {broke:>6}  of 1 isolated arms past {ms(tolerance):.3f} ms,"
            f" over {run['stalled_intervals']} intervals"
        )
        if broke:
            faults.append(
                f"the wedged connection cost the producer {ms(run['perturbation_ns']):.3f} ms at p99, past the"
                f" {ms(tolerance):.3f} ms the media path has to spare"
            )
    else:
        held = 1 if inside else 0
        print(
            f"    {arm:<9} {held:>6}  of 1 control arms inside {ms(tolerance):.3f} ms,"
            f" over {run['stalled_intervals']} intervals"
        )
        if held:
            faults.append(
                f"the same wedge on the producer's own thread cost it only"
                f" {ms(run['perturbation_ns']):.3f} ms at p99, so this measurement cannot see the"
                " arrangement it exists to rule out and its clean arm proves nothing"
            )

# An arm that ended badly for a reason none of the criteria above name is not a
# pass with a footnote. It is a run nobody has read.
for arm, run in measured.items():
    if run["status"] != 0 and not faults:
        faults.append(
            f"the {arm} arm exited {run['status']} while every criterion here held, so it"
            f" failed for something this gate does not check; its output is in {out / (arm + '.out')}"
        )

print()
if faults:
    for index, fault in enumerate(faults):
        print(("FAIL " if index == 0 else "     ") + fault)
    sys.exit(1)

wedged, contended = measured["wedged"], measured["contended"]
print(
    f"PASS a wedged control connection moved a 120 Hz producer by {ms(wedged['perturbation_ns']):+.3f} ms at p99"
)
print(
    f"     against a {ms(tolerance):.3f} ms bound, while the same wedge made from the producer's own"
)
print(
    f"     thread moved it by {ms(contended['perturbation_ns']):+.3f} ms; the quiet halves held"
    f" {ms(wedged['baseline_p99_ns']):.3f} ms and"
)
print(
    f"     {ms(contended['baseline_p99_ns']):.3f} ms at p99 over {wedged['baseline_intervals']} and"
    f" {contended['baseline_intervals']} intervals"
)
PY
verdict=$?
set -e
exit "$verdict"
