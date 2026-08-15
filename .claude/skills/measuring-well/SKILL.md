---
name: measuring-well
description: Arithmetic on measurements, and the traps that make a number look right and be wrong. Use when computing a rate, a percentile or a drift, when subtracting timestamps, when comparing two runs or two arms, when deciding whether an outlier matters, or when a measurement disagrees with a prediction. Also use before quoting any measured figure as evidence for a decision.
---

# Measuring well

A wrong number is worse than no number, because a decision gets built on it. This is a
collection of traps that each produced a confident, plausible, wrong figure, and the
check that catches each one.

Every example is real and the numbers are the ones that were printed.

## Never subtract across clock bases

Two instants are only comparable if they came from the same clock. This sounds obvious
and is violated constantly, because the violation looks like ordinary arithmetic.

**Across machines.** There is no synchronisation good enough for a sub-millisecond
segment. When a figure would need to cross, name the two hops and leave them unmeasured
rather than interpolating - a chain of intervals each local to one machine, with the
network hops written as "not measured", is worth more than a total that fabricates the
part nobody can see.

Where the code already refuses this, it is deliberate: a frame age computed from a birth
mark on another machine reports as absent rather than as a number.

**Within a machine.** `mach_absolute_time` and `mach_continuous_time` diverge by time
spent asleep, so Core Animation's timestamps and this project's monotonic marks agree
until the machine sleeps once and then disagree by however long that was. An audio
device's clock is neither of them. QPC is neither.

If two bases must be related, measure the offset once, and say in the comment that it is
an offset between two bases rather than a shared clock.

## A count and a span must describe the same interval

The trap, and the arithmetic:

```
frames captured   2880480
qpc span          60.0009 s
2880480 / 60.0009 = 48007 Hz   →  +151 ppm against 48000 nominal
```

That +151 ppm is entirely an artefact. Both the device position and the QPC position
timestamp the **first** frame of their packet, so the span runs first-frame to
first-frame and excludes the last packet's audio that the count includes. One 480-frame
packet over a minute is exactly 150 ppm.

```
device position advanced   2880000 frames between the two instants
2880000 / 60.0009 = 47999.27 Hz  →  -15 ppm
```

The honest figure was −15 ppm. The wrong one was about to become the largest term in a
gate whose entire subject was drift measured in parts per million.

**The check:** measure the elapsed quantity *between the two instants the span was taken
at*, not a total that includes something outside them.

## A maximum is not a frequency

```
recv to injected   p50 123.90 us   p99 292.86 us   max 4321.28 us
```

A maximum twelve times the p99 reads like an argument for building something to avoid it.
Counted:

```
over 0.5 ms   13   0.092 %
over 1.0 ms    1   0.007 %
over 2.0 ms    1   0.007 %
over 5.0 ms    0   0.000 %
```

One event in 14193. The argument evaporated, and a virtual HID device did not get built.

**The check:** count crossings of a threshold. Report the maximum beside them, never
instead of them.

## Comparing two runs: what moves between them on its own?

```
alignment off   presentation wait p50 5.51 ms
alignment on    presentation wait p50 2.38 ms
```

That looks like a 3.13 ms improvement from a mechanism. It was not. The quantity being
aligned drifts on its own at 0.02 ms per second, crossing its period every two hundred
seconds, and the arms were forty seconds each. The "aligned" arm had started at 1.89 ms
against a 2.00 ms target and held twenty-five times out of forty-five: it was a
favourable draw, not a mechanism.

**The check:** before comparing arms, ask what changes between them without anybody
touching it, at what rate, and whether the arm is long enough to average it out. If it is
not, do not compare arms - find the **step** inside one run instead.

## Finding a step instead of comparing ends

When a known change is applied part-way through a run, do not compare the first sample
against the last. The drift between them can be larger than the change:

```
phase 7.43 ms -> 1.03 ms   after a 3.00 ms shift
```

That looked like a −6.40 ms movement and was mostly drift, which sweeps a whole period
every few hundred seconds.

**The method:** difference consecutive samples, take the differences *the short way round*
if the quantity is a phase, and look for one movement far larger than the per-sample
drift. A 3 ms step against 0.125 ms of drift per sample is twenty-four times the largest
innocent movement and stands out on its own. Then check there is exactly **one** such
step: two means something else moved too and neither can be attributed.

This also removes the need to align a shell's clock with a program's, which would have
been another base to cross.

## The same numbers can fit two different stories

Six sessions measured a phase and produced:

```
0.98, 2.21, 3.26, 4.93, 5.77, 6.84 ms
```

A gate checked that each session's phase barely moved within itself and that the sessions
covered much of the period between them, and passed. Both of those are also true of a
single slowly drifting clock, which is what it actually was - and independent draws come
out strictly ordered about once in seven hundred and twenty times.

**The check:** when a criterion would pass under two different mechanisms, it does not
distinguish them. Find the quantity that differs. Here it was the gap between sessions:
predicted from elapsed time times the drift the sessions agreed on, every boundary matched
to within 0.28 ms of an 8.33 ms period, and the lottery story died.

## Prove the traffic went where you think

A datagram addressed to this machine's own routable address never leaves it. Measured:

```
1000 packets to 192.168.1.108   lo0 +1016   en0 +138
1000 packets to 192.168.1.1     lo0   +16   en0 +1091
```

A gate's "radio arm" was the loopback path measured twice.

**The check:** interface counters, not the address you typed. And reordering is intrinsic
evidence in the other direction - loopback delivers strictly in order, so a run reporting
22557 reordered packets did cross a real network, whatever else is in doubt.

## An instrument nobody configured deliberately is not evidence

A smoke test of an audio encoder reported 126 bytes per frame, about 201 kbps against a
128 kbps target, and that discrepancy was written into a work brief as something to
explain. It did not need explaining: the smoke test had left every setting at its
default. Configured deliberately, the same encoder produced 81 bytes and 129.6 kbps.

**The check:** before treating a number as a finding, establish that whatever produced it
was configured on purpose. Especially when the number is your own.

## Derive before building

Three mechanisms in this project were built and then measured to do nothing. One was
neutral by derivation and could have been ruled out on paper in five lines:

```
period T = 8.33 ms, pipeline work P = 3 ms, content drawn at t = 0
capture at once   ready 3    shown 8.33   age 8.33
capture at +4     ready 7    shown 8.33   age 8.33   ← neutral
capture at +6     ready 9    shown 16.67  age 16.67  ← worse
source at +4      ready 7    shown 8.33   age 4.33   ← the lever
```

**The check:** write the arithmetic before writing the code, and prefer a cheap experiment
to a confident implementation. A mechanism measured to do nothing must then be deleted
rather than left wired, because one that looks connected will be believed.
