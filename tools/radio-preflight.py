"""The band radio-preflight applies, and the arithmetic that produced it.

Kept out of the shell script because two callers need the same reader. The gate
reads one live window and one committed control window, and `--band` reads every
trace the band rests on and recomputes the population the limits were derived
from, so that a limit and its evidence cannot drift apart. The reasoning for
each limit is in tools/radio-preflight.sh; what is here is the reasoning that
belongs next to the arithmetic.

Nothing in this file decides a pass or a failure. It decides a pass or a
refusal, which is the whole of this gate's vocabulary.
"""

import csv
import glob
import json
import math
import os
import statistics
import sys

# Three decibels: the spread of median signal across A8's thirteen sweep arms,
# -70 to -67 dBm, over which the median rate those arms negotiated ran from 288
# to 576 Mbps. The measured amount of level movement that doubles this radio's
# rate, and so the most a run may move before it carries inside itself the
# difference that made those arms incomparable.
BUDGET_DB = 3.0

# The same doubling, applied to the quantity it was measured on. Never fired.
PHY_STEP_MAX = 2.0

# A window missing rows is a window that says less than it appears to. Over the
# eighteen windows measured, every one delivered every row it was due at the
# interval it was taken at, so this has never fired either; one per cent lets a
# single row slip out of a two minute window and refuses two.
COVERAGE_MIN = 0.99

PASS, REFUSED = 0, 2

# What a caller may add and what a caller may downgrade. Both are policy and
# neither has a default, so this gate on its own behaves exactly as it did.
#
# The additions are categorical: the channel a baseline was validated on, its
# width, and whether the channel carries a DFS obligation. Nothing is projected
# to state them, which is why they can be required of a short window.
#
# The downgrade exists because one criterion here answers a question a
# counterbalanced experiment has already handled. `the signal holds still for
# the run` fits a line to a window and extrapolates it, and this radio has been
# measured swinging in both directions - -0.593, +6.907 and -1.474 dB/min in
# three consecutive windows on one evening - so a line through any part of that
# projects a disaster that may not arrive, or misses one that does. Worse, the
# 3 dB it is judged against was derived as the spread of median signal BETWEEN
# A8's arms, so applying it to a projection inside one window puts a
# between-arm number in a within-window place. A caller whose design compares
# arms should require the comparison of the arms instead, and say so here.
ADVISORY = [name for name in os.environ.get("ADVISORY", "").split(",") if name]
REQUIRE_CHANNEL = os.environ.get("REQUIRE_CHANNEL", "")
REQUIRE_WIDTH = os.environ.get("REQUIRE_WIDTH", "")
REQUIRE_NON_DFS = os.environ.get("REQUIRE_NON_DFS", "") == "1"


def read(path):
    with open(path, newline="") as handle:
        rows = list(csv.DictReader(handle))
    if len(rows) < 3:
        raise ValueError(f"{os.path.basename(path)} holds {len(rows)} rows, which is no window")
    return rows


def measure(path, interval_ms, run_s):
    """Everything the band reads, and everything the label carries."""
    rows = read(path)
    t = [float(r["t_s"]) for r in rows]
    rssi = [float(r["rssi_dbm"]) for r in rows]
    phy = [float(r["tx_rate_mbps"]) for r in rows]
    n = len(rows)
    span = t[-1] - t[0]
    interval_s = interval_ms / 1000.0

    # Against the interval this trace was actually taken at. Read against a
    # nominal second, A6's 1100 ms window reports an eight per cent hole that
    # was never due.
    due = round(span / interval_s) + 1
    coverage = n / due if due else 0.0

    mean_t = sum(t) / n
    mean_y = sum(rssi) / n
    spread_t = sum((a - mean_t) ** 2 for a in t)
    slope = sum((a - mean_t) * (b - mean_y) for a, b in zip(t, rssi)) / spread_t
    intercept = mean_y - slope * mean_t
    residual = [b - (intercept + slope * a) for a, b in zip(t, rssi)]
    scatter = math.sqrt(sum(r * r for r in residual) / (n - 2))
    # The slope is a prediction and this is what the prediction is worth.
    slope_error = scatter / math.sqrt(spread_t)

    run_min = run_s / 60.0
    half = n // 2
    phy_first, phy_second = statistics.median(phy[:half]), statistics.median(phy[half:])
    phy_median = statistics.median(phy)
    phy_ratio = max(phy_first, phy_second) / min(phy_first, phy_second) if min(phy_first, phy_second) else float("inf")

    return {
        "source": os.path.basename(path),
        "rows": n,
        "rows_due": due,
        "coverage": coverage,
        "span_s": span,
        "interval_ms": interval_ms,
        "run_s": run_s,
        "started_unix_s": float(rows[0]["unix_s"]),
        "rssi_median": statistics.median(rssi),
        "rssi_min": min(rssi),
        "rssi_max": max(rssi),
        "rssi_range": max(rssi) - min(rssi),
        "rssi_sd": statistics.pstdev(rssi),
        # Medians of the halves rather than their means, so that the step is a
        # statement about where the link sat and not about its worst second.
        "rssi_half_step": statistics.median(rssi[half:]) - statistics.median(rssi[:half]),
        "drift_db_per_min": slope * 60.0,
        "drift_error_db_per_min": slope_error * 60.0,
        "scatter_db": scatter,
        "projected_db": slope * 60.0 * run_min,
        "projection_error_db": slope_error * 60.0 * run_min,
        "phy_median": phy_median,
        "phy_min": min(phy),
        "phy_max": max(phy),
        "phy_half_ratio": phy_ratio,
        "noise_median": statistics.median(float(r["noise_dbm"]) for r in rows),
        "channels": sorted({r["channel"] for r in rows}),
        "widths": sorted({r["width_mhz"] for r in rows}),
        "radar_bands": sorted({r["radar_band"] for r in rows}),
    }


def band(m):
    """Each criterion, whether it holds, and the numbers it holds or fails on.

    The first entry of each tuple is the name the control arm is checked
    against, so that a control refusing for the wrong reason is caught rather
    than counted.

    The last three are only present when a caller asked for them, and they are
    categorical rather than projected: a channel number either is the one a
    baseline was validated on or is not, and no extrapolation is involved in
    saying so. They exist because the projected criterion answers the wrong
    question for a counterbalanced experiment, and the argument is in the gate
    below rather than here.
    """
    run_min = m["run_s"] / 60.0
    criteria = [
        (
            "the window was sampled",
            m["coverage"] >= COVERAGE_MIN,
            f"{m['rows']} rows of {m['rows_due']} due at {m['interval_ms']} ms, "
            f"{m['coverage'] * 100:.1f} per cent against {COVERAGE_MIN * 100:.0f}",
        ),
        (
            "the link stayed where it was",
            len(m["channels"]) == 1 and len(m["widths"]) == 1 and len(m["radar_bands"]) == 1,
            f"channel {'/'.join(m['channels'])} at {'/'.join(m['widths'])} MHz, "
            f"radar band {'/'.join(m['radar_bands'])}",
        ),
        (
            "the window can resolve the budget",
            m["projection_error_db"] < BUDGET_DB,
            f"the slope is worth +-{m['drift_error_db_per_min']:.3f} dB/min over this "
            f"{m['span_s']:.0f} s window, which is +-{m['projection_error_db']:.2f} dB over "
            f"{run_min:.0f} min against a {BUDGET_DB:.1f} dB budget",
        ),
        (
            "the signal holds still for the run",
            abs(m["projected_db"]) < BUDGET_DB,
            f"{m['drift_db_per_min']:+.3f} dB/min projects {m['projected_db']:+.2f} dB over "
            f"{run_min:.0f} min against a {BUDGET_DB:.1f} dB budget",
        ),
        (
            "the signal's halves agree",
            abs(m["rssi_half_step"]) < BUDGET_DB,
            f"{m['rssi_half_step']:+.1f} dB between the half medians against {BUDGET_DB:.1f}",
        ),
        (
            "the negotiated rate's halves agree",
            m["phy_half_ratio"] < PHY_STEP_MAX,
            f"a factor of {m['phy_half_ratio']:.2f} between the half medians against "
            f"{PHY_STEP_MAX:.1f}",
        ),
    ]

    if REQUIRE_CHANNEL:
        criteria.append(
            (
                "the channel is the validated one",
                m["channels"] == [REQUIRE_CHANNEL],
                f"channel {'/'.join(m['channels'])} against {REQUIRE_CHANNEL} asked for",
            )
        )
    if REQUIRE_WIDTH:
        criteria.append(
            (
                "the width is the validated one",
                m["widths"] == [REQUIRE_WIDTH],
                f"{'/'.join(m['widths'])} MHz against {REQUIRE_WIDTH} asked for",
            )
        )
    if REQUIRE_NON_DFS:
        criteria.append(
            (
                "the channel carries no DFS obligation",
                m["radar_bands"] == ["0"],
                f"radar band {'/'.join(m['radar_bands'])}; a channel under a DFS obligation can "
                f"be told to vacate mid-measurement and nothing downstream survives that",
            )
        )
    return criteria


def label(m):
    """What the run has to carry, because a run whose conditions were not
    recorded cannot be compared with any other run - which is what happened to
    A8's thirteen arms."""
    return {
        "channel": m["channels"][0] if len(m["channels"]) == 1 else m["channels"],
        "width_mhz": m["widths"][0] if len(m["widths"]) == 1 else m["widths"],
        "radar_band": m["radar_bands"][0] if len(m["radar_bands"]) == 1 else m["radar_bands"],
        "rssi_median_dbm": m["rssi_median"],
        "rssi_min_dbm": m["rssi_min"],
        "rssi_max_dbm": m["rssi_max"],
        "rssi_sd_db": round(m["rssi_sd"], 3),
        "noise_median_dbm": m["noise_median"],
        "phy_median_mbps": m["phy_median"],
        "phy_min_mbps": m["phy_min"],
        "phy_max_mbps": m["phy_max"],
        "drift_db_per_min": round(m["drift_db_per_min"], 3),
        "projected_db_over_run": round(m["projected_db"], 2),
        "window_s": round(m["span_s"], 1),
        "run_s": m["run_s"],
        "started_unix_s": m["started_unix_s"],
    }


def report(title, m, verdicts):
    print(f"{title}\n")
    print(f"  channel           {'/'.join(m['channels'])} at {'/'.join(m['widths'])} MHz, "
          f"radar band {'/'.join(m['radar_bands'])}")
    print(f"  signal            median {m['rssi_median']:.0f} dBm, {m['rssi_min']:.0f} to "
          f"{m['rssi_max']:.0f}, sd {m['rssi_sd']:.2f}, noise {m['noise_median']:.0f} dBm")
    print(f"  negotiated rate   median {m['phy_median']:.0f} Mbps, {m['phy_min']:.0f} to "
          f"{m['phy_max']:.0f}")
    print(f"  movement          {m['drift_db_per_min']:+.3f} +-{m['drift_error_db_per_min']:.3f} "
          f"dB/min, scatter {m['scatter_db']:.2f} dB about it")
    print()
    for name, held, detail in verdicts:
        if held:
            state = "hold"
        elif name in ADVISORY:
            state = "NOTE"
        else:
            state = "OUT "
        print(f"  {state:<5} {name:<36} {detail}")
    print()


def gate(argv):
    window_csv, interval_ms, run_s, control_csv, control_interval_ms, control_run_s, label_path = argv
    m = measure(window_csv, int(interval_ms), float(run_s))
    verdicts = band(m)

    print("the band, and where it came from\n")
    print(f"  {BUDGET_DB:.1f} dB      A8's thirteen arms had median signal from -70 to -67 dBm and")
    print("              median rate from 288 to 576 Mbps over the same arms, so three")
    print("              decibels is the movement that doubles this radio's rate")
    print(f"  {COVERAGE_MIN * 100:.0f} per cent all nineteen committed windows delivered every row they were")
    print("              due at the interval each was taken at")
    print(f"  factor {PHY_STEP_MAX:.0f}    the same doubling, on the quantity it was measured on")
    print()
    report("the window", m, verdicts)

    with open(label_path, "w") as handle:
        json.dump(label(m), handle, indent=2, sort_keys=True)
        handle.write("\n")
    print(f"  label written to {label_path}, and a run that does not carry it cannot be")
    print("  compared with any other run\n")

    control = measure(control_csv, int(control_interval_ms), float(control_run_s))
    control_verdicts = band(control)
    # Named rather than described, because the arm is overridable and a heading
    # that says A6 over somebody else's window is the harness lying about which
    # evidence it just showed.
    report(
        f"the control arm, {control['source']}: the window this gate exists to have stopped",
        control,
        control_verdicts,
    )

    # An advisory name that matches nothing is a caller who thinks a criterion
    # has been downgraded and is wrong about it - the most expensive way to be
    # wrong here, because the run proceeds believing it was checked.
    names = [name for name, _, _ in verdicts]
    unknown = [name for name in ADVISORY if name not in names]
    if unknown:
        print(f"REFUSE the caller downgraded {', '.join(repr(name) for name in unknown)}, which is not a")
        print("       criterion this gate has. Nothing was downgraded and nothing was checked")
        print("       in its place, so the run would proceed believing in a protection that")
        print(f"       does not exist. The names are: {', '.join(names)}")
        return REFUSED

    # A control that refuses for any reason at all would be satisfied by a
    # harness that cannot read a file. The deciding criterion has to be the one
    # that fired, or this arm certifies nothing.
    #
    # When the caller has downgraded the criterion the control was built to
    # fire on, this arm has to demonstrate a criterion that still binds: a
    # control whose only failure is advisory proves that nothing capable of
    # stopping the run can stop it. A6's window fires on its halves as well as
    # on its projection, so the demonstration survives the downgrade - but it
    # is checked here rather than relied upon.
    fired = [name for name, held, _ in control_verdicts if not held]
    binding_fired = [name for name in fired if name not in ADVISORY]
    deciding = "the signal holds still for the run"
    if deciding not in fired:
        print(f"REFUSE the control arm did not refuse on {deciding}: it fired {fired or 'nothing'},")
        print("       so this run has no evidence that the criterion deciding the window")
        print("       above is capable of coming out negative")
        return REFUSED
    if not binding_fired:
        print(f"REFUSE the control arm fired only on {', '.join(fired)}, every one of which this")
        print("       caller downgraded to advisory. Nothing that could stop this run has been")
        print("       shown capable of stopping one, so the remaining criteria are decoration")
        return REFUSED

    out = [name for name, held, _ in verdicts if not held and name not in ADVISORY]
    noted = [name for name, held, _ in verdicts if not held and name in ADVISORY]
    if out:
        print("REFUSE " + f"{len(out)} of {len(verdicts)} criteria are outside the band: " + ", ".join(out))
        print("       This is not a failure and there is no failure available here. A radio")
        print("       that moves is the absence of a condition under which anything")
        print("       downstream can be measured, so the run is not worth starting rather")
        print("       than the pipeline being wrong.")
        return REFUSED
    if noted:
        print(f"NOTE  outside the band but downgraded by the caller: {', '.join(noted)}.")
        print(f"      The control arm still fired on {', '.join(binding_fired)}, so what remains")
        print("      binding here has been shown capable of refusing.")
        print()

    # What held, named by what actually bound. Saying "the link held" over a
    # projection the caller downgraded would be this gate claiming a check it
    # was told not to make.
    if deciding in ADVISORY:
        print(f"PASS every binding criterion held, on channel {m['channels'][0]} at "
              f"{m['widths'][0]} MHz with the")
        print(f"     rate's halves within a factor of {m['phy_half_ratio']:.2f}. The projection is not "
              f"among them at this")
        print(f"     caller's request and read {m['projected_db']:+.2f} dB over {m['run_s']:.0f} s; "
              f"whatever the run does")
        print("     with the link is settled from the run's own arms. Carry the label with it.")
    else:
        print(f"PASS the link held: {m['projected_db']:+.2f} dB projected over {m['run_s']:.0f} s against a")
        print(f"     {BUDGET_DB:.1f} dB budget, on channel {m['channels'][0]} at {m['widths'][0]} MHz with the")
        print(f"     rate's halves within a factor of {m['phy_half_ratio']:.2f}. Start the run, and carry the")
        print("     label with it.")
    if m["radar_bands"][0] == "1":
        print()
        print("     NOTE this is a DFS channel. The window shows no hold and no vacate, and")
        print("          ten unbroken minutes of this channel were measured showing neither,")
        print("          but a hold is rare and abrupt and no window at 1 Hz has the power to")
        print("          exclude one. The run's own trace is where that gets settled, and a")
        print("          run taken here is not comparable with one taken off a radar band.")
    return PASS


def population(control_csv, control_interval_ms, patterns):
    """Every trace the band rests on, measured the same way the gate measures.

    Interval per trace is inferred from its own median row spacing rather than
    assumed, because the one window in this population that was taken at 1100 ms
    is the one that would otherwise read as holed.
    """
    paths = []
    for pattern in patterns:
        paths.extend(sorted(glob.glob(pattern)))
    rows = []
    for path in paths:
        raw = read(path)
        t = [float(r["t_s"]) for r in raw]
        spacing = statistics.median(b - a for a, b in zip(t, t[1:]))
        rows.append((os.path.basename(path), measure(path, round(spacing * 1000), 600.0)))
    rows.append(("CONTAMINATED " + os.path.basename(control_csv),
                 measure(control_csv, int(control_interval_ms), 600.0)))
    return rows


def show_band(argv):
    control_csv, control_interval_ms = argv[0], argv[1]
    rows = population(control_csv, control_interval_ms, argv[2:])
    head = f"{'window':<42}{'n':>5}{'span':>7}{'ms':>6}{'cov':>7}{'rng':>5}{'sd':>6}" \
           f"{'drift':>8}{'+-':>7}{'half':>6}{'phy':>7}{'ch':>5}{'dfs':>5}"
    print(head)
    for name, m in rows:
        print(f"{name:<42}{m['rows']:>5}{m['span_s']:>7.0f}{m['interval_ms']:>6}"
              f"{m['coverage'] * 100:>6.1f}%{m['rssi_range']:>5.0f}{m['rssi_sd']:>6.2f}"
              f"{m['drift_db_per_min']:>8.3f}{m['drift_error_db_per_min']:>7.3f}"
              f"{m['rssi_half_step']:>6.1f}{m['phy_median']:>7.0f}"
              f"{'/'.join(m['channels']):>5}{'/'.join(m['radar_bands']):>5}")

    clean = [m for name, m in rows if not name.startswith("CONTAMINATED")]
    dirty = [m for name, m in rows if name.startswith("CONTAMINATED")]
    drifts = [abs(m["drift_db_per_min"]) for m in clean]
    steps = [abs(m["rssi_half_step"]) for m in clean]
    ranges = [m["rssi_range"] for m in clean]
    print()
    print(f"{len(clean)} windows taken on a link that was not moving within itself:")
    print(f"  |drift|      mean {statistics.mean(drifts):.3f}  sd {statistics.pstdev(drifts):.3f}  "
          f"max {max(drifts):.3f} dB/min")
    print(f"  |half step|  max {max(steps):.1f} dB")
    print(f"  range        {min(ranges):.0f} to {max(ranges):.0f} dB")
    print(f"  rows missing {sum(m['rows_due'] - m['rows'] for m in clean)} of "
          f"{sum(m['rows_due'] for m in clean)}")
    for m in dirty:
        print(f"\nthe contaminated window, {m['source']}:")
        print(f"  |drift|      {abs(m['drift_db_per_min']):.3f} dB/min, "
              f"{(abs(m['drift_db_per_min']) - statistics.mean(drifts)) / statistics.pstdev(drifts):.1f} "
              "standard deviations above the mean of the others")
        print(f"  |half step|  {abs(m['rssi_half_step']):.1f} dB, "
              f"{abs(m['rssi_half_step']) / max(steps):.1f} times the largest of the others")
        print(f"  range        {m['rssi_range']:.0f} dB against {min(ranges):.0f} to {max(ranges):.0f}, "
              "which is why range is reported and is not a criterion")
        print(f"  rows         {m['rows']} of {m['rows_due']} due at {m['interval_ms']} ms, "
              "complete - the hole is an artefact of reading it at 1000")
    print(f"\nthe budget is {BUDGET_DB:.1f} dB and it sits above every window here except that one.")


if __name__ == "__main__":
    mode, rest = sys.argv[1], sys.argv[2:]
    if mode == "band":
        show_band(rest)
        sys.exit(PASS)
    sys.exit(gate(rest))
