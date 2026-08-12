#!/usr/bin/env python3
"""Does this Mac's radio go away periodically, independently of our pipeline?

The capture experiment found access units arriving in a 34 ms stall every
220 ms. That is above the BPF tap, which leaves the air, the access point,
the Mac's firmware and the Mac's driver all in scope. This narrows it from
a completely different direction: if the radio itself is unavailable in
periodic windows, then *any* traffic crosses them, including an ICMP echo
to the access point one hop away.

Nothing here involves the Windows host, NVENC, RTP, the depacketiser or
120 fps of video. If the same signature appears in a plain ping, the fault
is not in anything this project built.

macOS refuses intervals below 0.1 s to an unprivileged user, so the sampling
rate is 10 Hz against a suspected 220 ms period. Too coarse to reconstruct
the period cleanly - it is barely two samples per cycle - but ample to
measure how often a round trip lands in a hole and how deep the hole is,
which is the part that discriminates.

usage: tools/radio-gap.py [host] [--count 900]
"""

import re
import statistics
import subprocess
import sys


def sample(host, count):
    """Round trip times in milliseconds, in order."""
    result = subprocess.run(
        ["ping", "-c", str(count), "-i", "0.1", host],
        capture_output=True,
        text=True,
        check=False,
    )
    pattern = re.compile(r"time=([0-9.]+) ms")
    return [float(m.group(1)) for m in pattern.finditer(result.stdout)]


def main(argv):
    host = None
    count = 900
    index = 0
    while index < len(argv):
        if argv[index] == "--count":
            index += 1
            count = int(argv[index])
        else:
            host = argv[index]
        index += 1
    if host is None:
        route = subprocess.run(
            ["route", "-n", "get", "default"], capture_output=True, text=True, check=False
        )
        match = re.search(r"gateway:\s*(\S+)", route.stdout)
        if not match:
            raise SystemExit("no default gateway; pass a host")
        host = match.group(1)

    times = sample(host, count)
    if len(times) < 20:
        raise SystemExit(f"only {len(times)} replies from {host}")

    floor = statistics.median(times)
    # A round trip is "held" when it cost materially more than the median.
    # Ten milliseconds is more than a whole 802.11 retry sequence at these
    # rates and well below the 34 ms the capture measured, so it separates
    # a hole from ordinary variation without being tuned to the answer.
    held = [t for t in times if t > floor + 10.0]
    excess = [t - floor for t in held]
    print(f"host             {host}, {len(times)} replies")
    print(
        f"round trip       median {floor:.2f} ms  p95 {quantile(times, 0.95):.2f}  "
        f"p99 {quantile(times, 0.99):.2f}  max {max(times):.2f}"
    )
    share = 100.0 * len(held) / len(times)
    print(f"held             {len(held)} of {len(times)} ({share:.1f}%) cost more than median + 10 ms")
    if held:
        print(
            f"hold depth       median {statistics.median(excess):.1f} ms  "
            f"max {max(excess):.1f} ms"
        )
        # Where the held replies sit relative to each other, in samples. A
        # hole that recurs shows up as a preferred spacing even when the
        # sampling rate cannot resolve the period itself.
        positions = [i for i, t in enumerate(times) if t > floor + 10.0]
        spacings = [(b - a) * 100 for a, b in zip(positions, positions[1:])]
        if spacings:
            print(
                f"spacing          median {statistics.median(spacings):.0f} ms  "
                f"min {min(spacings):.0f}  over {len(spacings)} intervals"
            )


def quantile(values, q):
    ordered = sorted(values)
    return ordered[min(int(len(ordered) * q), len(ordered) - 1)]


if __name__ == "__main__":
    main(sys.argv[1:])
