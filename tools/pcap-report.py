#!/usr/bin/env python3
"""The same link, measured at the BPF tap and at the socket.

Both columns are produced by the same Rust code over the same run, so a
difference between them is a difference in when packets became visible and
never a difference in how they were counted.

usage: tools/pcap-report.py <dir>
"""

import glob
import json
import os
import statistics
import sys

FIELDS = [
    ("au p50", "au_interval_p50_ms", "{:.2f}"),
    ("au p95", "au_interval_p95_ms", "{:.2f}"),
    ("au p99", "au_interval_p99_ms", "{:.2f}"),
    ("au max", "au_interval_max_ms", "{:.2f}"),
    ("first p99", "first_interval_p99_ms", "{:.2f}"),
    (">1.25T/min", "over_1_25t_per_min", "{:.1f}"),
    (">1.5T/min", "over_1_5t_per_min", "{:.1f}"),
    (">2T/min", "over_2t_per_min", "{:.1f}"),
    (">3T/min", "over_3t_per_min", "{:.1f}"),
    (">4T/min", "over_4t_per_min", "{:.1f}"),
    (">6T/min", "over_6t_per_min", "{:.1f}"),
    ("clusters/min", "stall_clusters_per_min", "{:.1f}"),
    ("catch-up mean", "mean_catch_up_units", "{:.2f}"),
    ("catch-up max", "max_catch_up_units", "{:.0f}"),
    ("stall gap p50", "stall_gap_p50_ms", "{:.0f}"),
    ("stall gap p95", "stall_gap_p95_ms", "{:.0f}"),
]


def load(directory):
    """Runs that have both halves, keyed by label."""
    paired = {}
    for path in sorted(glob.glob(os.path.join(directory, "*.json"))):
        if path.endswith(".pcap.json"):
            continue
        label = os.path.basename(path)[: -len(".json")]
        capture = os.path.join(directory, f"{label}.pcap.json")
        with open(path) as handle:
            app = json.load(handle)["delivery"]
        tap = None
        if os.path.exists(capture):
            with open(capture) as handle:
                tap = json.load(handle)
        paired[label] = (app, tap)
    return paired


def control(paired):
    """Did switching the capture on change what the receiver measured?"""
    without = paired.get("nocap-control")
    with_capture = paired.get("cap-control")
    if not without or not with_capture:
        return
    print("control: the same measurement with and without the capture running")
    print(f"  {'metric':<16}{'no capture':>12}{'capturing':>12}{'delta':>10}")
    for name, key, fmt in FIELDS:
        a = without[0].get(key)
        b = with_capture[0].get(key)
        if a is None or b is None:
            continue
        print(
            f"  {name:<16}{fmt.format(a):>12}{fmt.format(b):>12}"
            f"{fmt.format(b - a):>10}"
        )
    print()


def comparison(paired):
    runs = [(k, v) for k, v in paired.items() if k.startswith("parallel-") and v[1]]
    if not runs:
        print("no paired runs")
        return
    print(f"tap against socket, median of {len(runs)} runs")
    print(f"  {'metric':<16}{'BPF tap':>12}{'socket':>12}{'delta':>10}")
    for name, key, fmt in FIELDS:
        tap = [v[1].get(key) for _, v in runs if v[1].get(key) is not None]
        app = [v[0].get(key) for _, v in runs if v[0].get(key) is not None]
        if not tap or not app:
            continue
        tap_median = statistics.median(tap)
        app_median = statistics.median(app)
        print(
            f"  {name:<16}{fmt.format(tap_median):>12}{fmt.format(app_median):>12}"
            f"{fmt.format(app_median - tap_median):>10}"
        )

    print("\nper run")
    print(
        f"  {'run':<14}{'tap >2T/m':>10}{'app >2T/m':>10}"
        f"{'tap clu/m':>10}{'app clu/m':>10}{'tap p99':>9}{'app p99':>9}"
    )
    for label, (app, tap) in runs:
        print(
            f"  {label:<14}{tap['over_2t_per_min']:>10.1f}{app['over_2t_per_min']:>10.1f}"
            f"{tap['stall_clusters_per_min']:>10.1f}{app['stall_clusters_per_min']:>10.1f}"
            f"{tap['au_interval_p99_ms']:>9.2f}{app['au_interval_p99_ms']:>9.2f}"
        )


def main(directories):
    for directory in directories:
        paired = load(directory)
        if not paired:
            print(f"no reports in {directory}")
            continue
        print(f"=== {directory} ===")
        control(paired)
        comparison(paired)


if __name__ == "__main__":
    main(sys.argv[1:] or ["/tmp/pcap"])
