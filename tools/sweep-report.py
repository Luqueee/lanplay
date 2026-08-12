#!/usr/bin/env python3
"""Reads a bitrate sweep and says where the knee is.

The reports carry far more than fits in a table, so this prints the columns
that decide the question and nothing else: what the link delivered, what it
lost, how bunched the arrivals were, and what the viewer actually saw.

Medians across repeats, not means. One bad minute of Wi-Fi is a real event
worth seeing in the per-run rows, but it should not drag the summary for a
bitrate that was otherwise fine.
"""

import json
import re
import statistics
import sys
from pathlib import Path


def median(values):
    return statistics.median(values) if values else float("nan")


def host_metrics(path):
    """Throughput, encode tail and late frames, from the sender's own log."""
    text = path.read_text(errors="replace") if path.exists() else ""
    out = {"host_fps": None, "host_late": None, "host_enc_p99": None, "host_gate": "?"}
    if m := re.search(r"frames (\d+) in ([\d.]+) s = ([\d.]+) frames/s", text):
        out["host_fps"] = float(m.group(3))
    if m := re.search(r"gate: (PASS|FAIL)", text):
        out["host_gate"] = m.group(1)
    # Per-window lines, so the worst window can speak instead of the mean.
    windows = re.findall(
        r"^\s+\d+-\d+\s+(\d+) fr\s+([\d.]+) Hz\s+enc p99\s+([\d.]+)\s+cap p99\s+([\d.]+)\s+([\d.]+) Mbit/s\s+late (\d+)",
        text,
        re.M,
    )
    if windows:
        # The trailing window is a partial slice of whatever was left.
        full = windows[:-1] if len(windows) > 1 else windows
        out["host_enc_p99"] = max(float(w[2]) for w in full)
        out["host_late"] = sum(int(w[5]) for w in full)
        out["host_mbps"] = median([float(w[4]) for w in full])
    return out


def client_metrics(path):
    report = json.loads(path.read_text())
    run, stream, net = report["run"], report["stream"], report["network"]
    display, decode = report["display"], report["decode"]
    seconds = run["seconds"] or 1.0
    offered = display["rendered"] + display["superseded"]
    # Steady windows only: the first is the client waiting for the sender to
    # be launched, and counting its empty refreshes as the link finding
    # nothing new would blame the link for the harness.
    steady = [w for w in report["windows"] if w["source_hz"] > 1.0][1:]
    return {
        "expected": stream["expected"],
        "reconstructed": stream["reconstructed"],
        "packet_loss": stream["packet_loss"],
        "au_loss": stream["au_loss"],
        "reordered": stream["reordered"],
        "arrival_p99": net["arrival_p99_ms"],
        "arrival_max": net["arrival_max_ms"],
        "decode_p99": decode["p99_ms"],
        "render_hz": display["rendered"] / seconds,
        "age_p99": display["frame_age_p99_ms"],
        "superseded_pct": 100.0 * display["superseded"] / offered if offered else 0.0,
        "worst_window_render": min((w["render_hz"] for w in steady), default=float("nan")),
        "src_p99": median([w["au_interval_p99_ms"] for w in steady]),
        "windows": len(steady),
    }


def main(directory):
    root = Path(directory)
    runs = {}
    for report in sorted(root.glob("*m-r*.json")):
        bitrate = int(report.name.split("m-r")[0])
        try:
            row = client_metrics(report)
        except (json.JSONDecodeError, KeyError) as error:
            print(f"skipping {report.name}: {error}")
            continue
        row.update(host_metrics(report.with_suffix("").with_suffix(".host.log")))
        runs.setdefault(bitrate, []).append(row)

    if not runs:
        print("no reports found")
        return 1

    print("per run")
    header = (
        f"{'Mbps':>5} {'host':>5} {'hfps':>6} {'late':>5} {'encp99':>7} "
        f"{'AUs':>10} {'ploss':>6} {'auloss':>6} {'arrp99':>7} {'arrmax':>7} "
        f"{'rndHz':>6} {'wrst':>6} {'aup99':>7} {'agep99':>7} {'sup%':>5}"
    )
    print(header)
    for bitrate in sorted(runs, reverse=True):
        for row in runs[bitrate]:
            print(
                f"{bitrate:>5} {row['host_gate']:>5} {row['host_fps'] or 0:>6.1f} "
                f"{row['host_late'] or 0:>5} {row['host_enc_p99'] or 0:>7.2f} "
                f"{row['reconstructed']:>5}/{row['expected']:<4} {row['packet_loss']:>6} "
                f"{row['au_loss']:>6} {row['arrival_p99']:>7.2f} {row['arrival_max']:>7.1f} "
                f"{row['render_hz']:>6.1f} {row['worst_window_render']:>6.1f} "
                f"{row['src_p99']:>7.2f} {row['age_p99']:>7.2f} {row['superseded_pct']:>5.1f}"
            )

    print()
    print("medians")
    print(
        f"{'Mbps':>5} {'runs':>5} {'auloss':>7} {'ploss':>7} {'arrp99':>7} {'arrmax':>8} "
        f"{'rndHz':>6} {'wrst':>6} {'aup99':>7} {'agep99':>7} {'late':>5}"
    )
    for bitrate in sorted(runs, reverse=True):
        rows = runs[bitrate]
        print(
            f"{bitrate:>5} {len(rows):>5} "
            f"{median([r['au_loss'] for r in rows]):>7.0f} "
            f"{median([r['packet_loss'] for r in rows]):>7.0f} "
            f"{median([r['arrival_p99'] for r in rows]):>7.2f} "
            f"{median([r['arrival_max'] for r in rows]):>8.1f} "
            f"{median([r['render_hz'] for r in rows]):>6.1f} "
            f"{median([r['worst_window_render'] for r in rows]):>6.1f} "
            f"{median([r['src_p99'] for r in rows]):>7.2f} "
            f"{median([r['age_p99'] for r in rows]):>7.2f} "
            f"{median([r['host_late'] or 0 for r in rows]):>5.0f}"
        )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1] if len(sys.argv) > 1 else "."))
