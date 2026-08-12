#!/usr/bin/env python3
"""The link, by arm, with the radio that carried it.

Only link metrics. No render rate, no fresh ticks, no frame age: those are
statements about a screen, and a link experiment must be readable on a
machine whose screen is asleep, occluded or in use by somebody else.

Arms are taken from the file name: everything before the trailing `-rN`.
That is the position for B1, the datagram size for B5, the channel width
for B2, and so on.

usage: tools/link-report.py <dir> [<dir> ...]
"""

import csv
import glob
import json
import os
import statistics
import sys


def runs(directory):
    """Every report in the directory, grouped by the label before -rN."""
    grouped = {}
    for path in sorted(glob.glob(os.path.join(directory, "*.json"))):
        name = os.path.basename(path)[: -len(".json")]
        label = name.rsplit("-r", 1)[0]
        with open(path) as handle:
            grouped.setdefault(label, []).append((name, json.load(handle)))
    return grouped


def radio(directory, name):
    """What the radio reported while that run was being measured."""
    path = os.path.join(directory, f"{name}.wifi.csv")
    if not os.path.exists(path):
        return None
    with open(path) as handle:
        rows = [r for r in csv.DictReader(handle) if r.get("rssi_dbm")]
    if not rows:
        return None

    def numbers(field):
        return [float(r[field]) for r in rows if r.get(field)]

    rssi = numbers("rssi_dbm")
    rate = numbers("tx_rate_mbps")
    noise = numbers("noise_dbm")
    # Channel and width together: an access point that moves between
    # sessions changes both, and comparing positions across a move of either
    # would be comparing two different links.
    channels = sorted(
        {f"{r['channel']}/{r.get('width_mhz', '?')}" for r in rows if r.get("channel")}
    )
    return {
        "n": len(rows),
        "rssi": statistics.median(rssi) if rssi else None,
        "rssi_min": min(rssi) if rssi else None,
        "noise": statistics.median(noise) if noise else None,
        "rate": statistics.median(rate) if rate else None,
        "channel": "/".join(channels),
    }


def link(report):
    """The measurements this experiment is about, and nothing else."""
    delivery = report["delivery"]
    network = report["network"]
    stream = report["stream"]
    return {
        "aus": delivery["delivered"],
        "au_p50": delivery["au_interval_p50_ms"],
        "au_p95": delivery["au_interval_p95_ms"],
        "au_p99": delivery["au_interval_p99_ms"],
        "au_max": delivery["au_interval_max_ms"],
        "span_p99": network["arrival_p99_ms"],
        "span_max": network["arrival_max_ms"],
        "reordered": stream["reordered"],
        "depth": stream["max_reorder_depth"],
        # Present only in reports written after the gap-fill tail was added.
        "fill_p99": stream.get("reorder_wait_p99_ms"),
        "fill_max": stream["reorder_wait_max_ms"],
        "ploss": stream["packet_loss"],
        "auloss": stream["au_loss"],
    }


HEAD = (
    f"{'run':<16} {'rssi':>5} {'noise':>6} {'phy':>6} {'ch/MHz':>9} "
    f"{'aup50':>6} {'aup95':>6} {'aup99':>6} {'aumax':>7} "
    f"{'spn99':>6} {'spnmx':>6} {'reord':>6} {'dpth':>5} "
    f"{'fil99':>6} {'filmx':>6} {'ploss':>6} {'auloss':>6}"
)


def row(name, measured, air):
    air = air or {}
    fill99 = measured["fill_p99"]
    return (
        f"{name:<16} "
        f"{air.get('rssi') or float('nan'):>5.0f} "
        f"{air.get('noise') or float('nan'):>6.0f} "
        f"{air.get('rate') or float('nan'):>6.0f} "
        f"{air.get('channel') or '?':>9} "
        f"{measured['au_p50']:>6.2f} {measured['au_p95']:>6.2f} "
        f"{measured['au_p99']:>6.2f} {measured['au_max']:>7.2f} "
        f"{measured['span_p99']:>6.2f} {measured['span_max']:>6.2f} "
        f"{measured['reordered']:>6} {measured['depth']:>5} "
        f"{fill99 if fill99 is not None else float('nan'):>6.2f} "
        f"{measured['fill_max']:>6.2f} "
        f"{measured['ploss']:>6} {measured['auloss']:>6}"
    )


def median_of(values):
    present = [v for v in values if v is not None]
    return statistics.median(present) if present else None


def main(directories):
    for directory in directories:
        grouped = runs(directory)
        if not grouped:
            print(f"no reports in {directory}")
            continue
        print(f"=== {directory} ===")
        print("per run")
        print(HEAD)
        summary = {}
        for label, items in grouped.items():
            for name, report in items:
                measured = link(report)
                print(row(name, measured, radio(directory, name)))
                summary.setdefault(label, []).append(
                    (measured, radio(directory, name) or {})
                )

        print("\nmedians by arm")
        print(HEAD)
        for label, items in summary.items():
            measured = {
                key: median_of([m[key] for m, _ in items])
                for key in items[0][0]
            }
            air = {
                key: median_of([a.get(key) for _, a in items])
                for key in ("rssi", "noise", "rate")
            }
            air["channel"] = "/".join(
                sorted({a.get("channel", "") for _, a in items if a.get("channel")})
            )
            print(row(f"{label} (n={len(items)})", measured, air))


if __name__ == "__main__":
    main(sys.argv[1:] or ["/tmp/link"])
