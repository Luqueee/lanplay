# When a gate says something surprising

Suspect the gate first. That is the base rate here: over one session of building them,
the instruments were wrong more often than the code they measured.

Eight questions, one per defect that has actually happened. Work down them before
reasoning from scratch.

## 1. Did the run produce any evidence?

Find the population behind every zero. If the packet count, callback count or event count
is itself zero, every "must be zero" check passed for the wrong reason and the verdict is
worthless.

This is the failure that recurred five times, in five different subsystems.

## 2. Is the parser reading what the probe printed?

Check the anchors have the multiline flag. Check every key still exists in the probe's
output - a renamed field turns into a `None` and then into a failure that names the wrong
thing.

Fastest test: print what the parser extracted, next to the probe's own line.

## 3. Could this check ever fail?

Describe the run that would fail it. If you cannot, the check is decoration and the gate
is not testing what its name says.

## 4. Could this check ever pass?

The opposite failure, and it happened: a criterion demanded zero session expiries while
the host deliberately outlives the client, so it always sweeps one. An impossible
criterion trains its reader that failure here is normal.

## 5. If it compares two runs, what moves between them on its own?

Two forty-second arms cannot compare a quantity that takes two hundred seconds to sweep
its period. Ask what drifts, at what rate, and whether the arms are long enough to
average it - or whether the comparison should be a step found *within* one run instead.

## 6. Is the arithmetic between compatible quantities?

A count and a span must describe the same interval. Two timestamps that each mark the
first frame of a packet do not span the last packet's contents. Two clock bases are not
one clock: `mach_absolute_time` and `mach_continuous_time` differ by time asleep, and an
audio device's clock is neither.

A 150 ppm error hid here, in a gate whose whole subject was parts per million.

## 7. Is a maximum being read as a frequency?

One event of 4.3 ms looks like an argument for building something. Counted, it was
0.007 % of a minute's events and the argument evaporated. Count crossings of a threshold;
report the max beside them, never instead of them.

## 8. Did the traffic actually go where the gate thinks?

A datagram addressed to this machine's own routable address never leaves it - the kernel
short-circuits it onto loopback. Measured: 1000 packets to the local address moved `lo0`
by 1016 and `en0` by 138, while the same 1000 to the router moved `en0` by 1091.

Verify with interface counters, not with the address you typed. And note that reordering
is intrinsic evidence of a real network: loopback delivers strictly in order, so a run
reporting reordered packets did cross something.

## If all eight pass

Then it may really be the code, and the gate has earned being believed. Say in the
report which of these you checked, because the next reader will otherwise start at
question one.
