#!/usr/bin/env python3
"""Is the stalling periodic, and at what period?

A rate says how often the link stalls; it cannot say whether the stalls are
scattered or on a clock, and that is the difference between contention and a
timer somewhere in the stack. The interval between consecutive stalls does
say it: a standard deviation of a few milliseconds around a fixed value is a
timer, and nothing else looks like that.

Reads the capture rather than the client's report, because the report holds
two percentiles and this needs the shape.

usage: tools/stall-period.py <capture.pcap> [--port 5004] [--fps 120]
"""

import collections
import statistics
import struct
import sys


def access_unit_arrivals(path, port):
    """When each access unit's first datagram arrived, in capture order.

    Grouped by the frame id in the RTP header extension rather than by the
    marker bit, so a reordered last packet cannot invent a boundary.
    """
    with open(path, "rb") as handle:
        data = handle.read()
    if len(data) < 24:
        return []
    magic = struct.unpack("<I", data[:4])[0]
    if magic in (0xA1B2C3D4, 0xA1B23C4D):
        endian, nanosecond = "<", magic == 0xA1B23C4D
    elif magic in (0xD4C3B2A1, 0x4D3CB2A1):
        endian, nanosecond = ">", magic == 0x4D3CB2A1
    else:
        raise SystemExit(f"{path}: not a pcap file")

    first = {}
    offset = 24
    header = struct.Struct(endian + "IIII")
    while offset + 16 <= len(data):
        seconds, fraction, captured, _ = header.unpack_from(data, offset)
        frame = data[offset + 16 : offset + 16 + captured]
        offset += 16 + captured
        stamp = seconds + (fraction / 1e9 if nanosecond else fraction / 1e6)
        # Ethernet, IPv4, UDP, RTP with a one-byte extension profile.
        if len(frame) < 60 or frame[12:14] != b"\x08\x00":
            continue
        ip = frame[14:]
        header_len = (ip[0] & 0x0F) * 4
        if ip[9] != 17 or len(ip) < header_len + 8:
            continue
        udp = ip[header_len:]
        if struct.unpack(">H", udp[2:4])[0] != port:
            continue
        rtp = udp[8:]
        if len(rtp) < 25 or rtp[0] >> 6 != 2 or not rtp[0] & 0x10:
            continue
        if rtp[12:14] != b"\xbe\xde":
            continue
        frame_id = struct.unpack(">Q", rtp[17:25])[0]
        if frame_id not in first or stamp < first[frame_id]:
            first[frame_id] = stamp
    return [first[key] for key in sorted(first)]


def main(argv):
    path = None
    port = 5004
    fps = 120.0
    index = 0
    while index < len(argv):
        if argv[index] == "--port":
            index += 1
            port = int(argv[index])
        elif argv[index] == "--fps":
            index += 1
            fps = float(argv[index])
        else:
            path = argv[index]
        index += 1
    if path is None:
        raise SystemExit(__doc__)

    times = access_unit_arrivals(path, port)
    if len(times) < 3:
        print("stall period     too few access units")
        return
    period = 1.0 / fps
    intervals = [(b, b - a) for a, b in zip(times, times[1:])]
    stalls = [(at, gap) for at, gap in intervals if gap >= 2 * period]
    span = times[-1] - times[0]
    if len(stalls) < 3:
        print(f"stall period     {len(stalls)} stalls in {span:.0f} s, too few to fit")
        return

    gaps = [(b[0] - a[0]) * 1000 for a, b in zip(stalls, stalls[1:])]
    sizes = [gap * 1000 for _, gap in stalls]
    # The clock, if there is one, is the mode. Long gaps are the quiet
    # between bursts of stalls and would drag a mean off the period.
    bins = collections.Counter(round(gap / 4) * 4 for gap in gaps if gap < 1000)
    if not bins:
        print(f"stall period     {len(stalls)} stalls, none within a second of each other")
        return
    mode, hits = bins.most_common(1)[0]
    near = [gap for gap in gaps if abs(gap - mode) < 30]
    share = 100.0 * len(near) / len(gaps)
    print(
        f"stall period     {mode:.0f} ms  sd {statistics.pstdev(near):.1f}  "
        f"{share:.0f}% of {len(gaps)} gaps within 30 ms of it  ({hits} in the mode bin)"
    )
    print(
        f"stall size       mean {statistics.mean(sizes):.1f} ms  "
        f"sd {statistics.pstdev(sizes):.1f}  max {max(sizes):.1f}  "
        f"duty {100.0 * sum(sizes) / (span * 1000):.1f}%"
    )


if __name__ == "__main__":
    main(sys.argv[1:])
