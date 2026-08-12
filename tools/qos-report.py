#!/usr/bin/env python3
"""Reads a service-class sweep and says whether the class changed anything.

Two questions, kept apart because they fail independently:

  1. Did the marking survive the path? Answered by the DSCP the receiver saw,
     not by what the sender asked for. An arm whose marking was stripped is
     not a QoS arm at all, and averaging it in would hide that.
  2. Given it survived, did the radio treat it differently? Answered by
     cadence and by what the viewer saw - not by throughput, which W2 already
     showed is not the constraint.

Medians across repeats. The per-run rows stay visible because one bad minute
of Wi-Fi is a real event, and a summary that smooths it away is the reason
this sweep is shuffled in the first place.
"""

import json
import statistics
import sys
from collections import defaultdict
from pathlib import Path


def median(values):
    return statistics.median(values) if values else float("nan")


def load(path):
    report = json.loads(path.read_text())
    run, net, display = report["run"], report["network"], report["display"]
    stream, decode = report["stream"], report["decode"]
    seconds = run["seconds"] or 1.0
    offered = display["rendered"] + display["superseded"]
    steady = [w for w in report["windows"] if w["source_hz"] > 1.0][1:]
    return {
        "dscp": net["observed_dscp"],
        "dscp_share": net["observed_dscp_share"],
        "arrival_p99": net["arrival_p99_ms"],
        "arrival_max": net["arrival_max_ms"],
        "src_p99": median([w["source_interval_p99_ms"] for w in steady]),
        "render_hz": display["rendered"] / seconds,
        "empty_pct": median([w["empty_pct"] for w in steady]),
        "age_p99": display["frame_age_p99_ms"],
        "superseded_pct": 100.0 * display["superseded"] / offered if offered else 0.0,
        "au_loss": stream["au_loss"],
        "packet_loss": stream["packet_loss"],
        "errors": decode["errors"],
    }


def main(directory):
    arms = defaultdict(list)
    for report in sorted(Path(directory).glob("*-r*.json")):
        arms[report.name.rsplit("-r", 1)[0]].append(load(report))
    if not arms:
        print("no reports found")
        return 1

    print("per run")
    print(
        f"{'arm':<12} {'dscp':>5} {'share':>6} {'srcp99':>7} {'arrp99':>7} {'arrmax':>7} "
        f"{'rndHz':>6} {'empty%':>7} {'agep99':>7} {'sup%':>6} {'auloss':>7} {'err':>4}"
    )
    for arm in arms:
        for row in arms[arm]:
            dscp = "-" if row["dscp"] is None else row["dscp"]
            print(
                f"{arm:<12} {dscp:>5} {row['dscp_share']:>5.0f}% {row['src_p99']:>7.2f} "
                f"{row['arrival_p99']:>7.2f} {row['arrival_max']:>7.1f} {row['render_hz']:>6.1f} "
                f"{row['empty_pct']:>7.1f} {row['age_p99']:>7.2f} {row['superseded_pct']:>6.1f} "
                f"{row['au_loss']:>7} {row['errors']:>4}"
            )

    print()
    print("medians")
    print(
        f"{'arm':<12} {'runs':>5} {'dscp':>5} {'srcp99':>7} {'arrp99':>7} {'rndHz':>6} "
        f"{'empty%':>7} {'agep99':>7} {'auloss':>7}"
    )
    baseline = None
    for arm, rows in arms.items():
        seen = {r["dscp"] for r in rows}
        summary = {
            "src_p99": median([r["src_p99"] for r in rows]),
            "render_hz": median([r["render_hz"] for r in rows]),
            "age_p99": median([r["age_p99"] for r in rows]),
        }
        if arm == "best-effort":
            baseline = summary
        print(
            f"{arm:<12} {len(rows):>5} "
            f"{'/'.join('-' if d is None else str(d) for d in sorted(seen, key=lambda x: (x is None, x))):>5} "
            f"{summary['src_p99']:>7.2f} "
            f"{median([r['arrival_p99'] for r in rows]):>7.2f} "
            f"{summary['render_hz']:>6.1f} "
            f"{median([r['empty_pct'] for r in rows]):>7.1f} "
            f"{summary['age_p99']:>7.2f} "
            f"{median([r['au_loss'] for r in rows]):>7.0f}"
        )

    if baseline:
        print()
        print("against best effort")
        for arm, rows in arms.items():
            if arm == "best-effort":
                continue
            src = median([r["src_p99"] for r in rows]) - baseline["src_p99"]
            hz = median([r["render_hz"] for r in rows]) - baseline["render_hz"]
            age = median([r["age_p99"] for r in rows]) - baseline["age_p99"]
            print(f"  {arm:<12} srcp99 {src:+7.2f} ms   render {hz:+6.1f} Hz   age p99 {age:+6.2f} ms")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1] if len(sys.argv) > 1 else "."))
