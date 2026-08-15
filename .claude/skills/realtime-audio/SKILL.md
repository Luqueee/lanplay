---
name: realtime-audio
description: Low-latency audio - render callbacks, ring buffers, jitter buffers, packet loss concealment, and clock drift between two devices. Use when working on an audio capture or playback path, when a callback underruns or clicks, when sizing a ring or a jitter buffer, when deciding what to do about a missing packet, or when audio and video need different treatment for the same problem.
---

# Real-time audio

Audio breaks differently from video, and the reflexes that serve video wrong-foot it. This
is the short version of what was learned building a 48 kHz stereo path across two
machines.

## The deadline is not soft

A dropped video frame is replaced by the next one and the viewer never learns of it. An
unfilled audio buffer is a click, and there is no version of a click a listener does not
notice.

That asymmetry decides every trade below. Where video can be late and still useful, audio
must be on time or concealed, and "on time" means a hard period the device keeps whether
anybody filled it or not.

## Video is latest-frame-wins. Audio is an ordered continuous stream.

Do not port the video pipeline's semantics. A late video frame is worthless because a
newer one describes the world better; a gap in audio is not skipped, it is **concealed**,
and the stream continues from where it was.

Concretely:

- A frame that misses its deadline is filled with the codec's own concealment. Opus
  provides it: decode with a null packet, through the same decoder instance as every real
  frame, because concealment both reads and updates decoder state.
- **Never zero-fill.** Silence is a click. Concealment is what the codec is for.
- A frame that arrives after it was concealed is **discarded and counted late**, never
  played. The moment it described has passed, and playing it would be an audible jump.
- Playing the newest available frame and dropping what came before is the video reflex and
  is wrong here.

## A priority band is not a deadline

This one cost three attempts and it is the most transferable finding.

A producer feeding a render callback ran, in order: at half a ring buffer, underrunning
five times in twenty seconds; at the whole ring, sixteen milliseconds of margin,
underrunning ten times in three hundred seconds; and at
`QOS_CLASS_USER_INTERACTIVE`, producing zero, twelve, zero and zero underruns across four
runs. A controlled comparison against a compiler running in parallel showed none at all,
so sustained load was never the trigger and the trigger was never identified.

A quality-of-service class is a band: it says this work matters more than that work and
leaves the scheduler to interpret it. The deadline here is not a matter of interpretation.

```
producer scheduled as time constraint, period 5.333 ms computation 0.667 ms constraint 2.667 ms
```

`THREAD_TIME_CONSTRAINT_POLICY` on macOS, stated in mach ticks - read the timebase, do not
assume nanoseconds, because they are equal on Intel and not on Apple silicon and a policy
in the wrong units asks for a period off by a factor of forty. Same gate, same duration,
same machine: twelve underruns became zero.

Make the computation budget honest. A thread claiming more computation than it uses
reserves a share of the machine nothing needs, and on a laptop that is somebody's battery.
Keep it preemptible: a non-preemptible thread that misbehaves takes the machine with it.

And **report which policy was granted.** The request can be refused, and a measurement
taken under a scheduling state nobody checked is a measurement of something else. A gate
should fail a run that did not get the deadline.

## Do not size a buffer to hide an unreliable producer

Growing the ring would also have fixed those underruns, and it is the wrong answer. In a
finished pipeline the ring sits between the jitter buffer and the device, so every frame of
it is latency added to audio that has already crossed a network. **Keep the buffer small
and make the producer reliable** - the trade every audio system makes.

Fill to capacity less one chunk rather than to a readable fraction: leaving room for a
whole write avoids a partial fill, and using the rest of the ring is free margin already
allocated.

## Jitter buffer

The deadline for a frame comes from its **RTP timestamp** plus the target, never from when
its datagram arrived. A deadline derived from arrival moves with the jitter it exists to
absorb.

Read exactly one arrival time - the first - to anchor the sample counter to the local
clock. Everything after that is arithmetic. A test worth having: feed two buffers the same
stream with arrivals scattered differently and assert identical playout.

Bound it in **time**, not only in slots. A stall that delivers a burst must not leave the
buffer holding more audio than its target: a buffer that absorbed a burst by growing would
trade a fault that ends for latency that never recovers.

When the ceiling is breached, skip forward all the way down to the target, not to the
ceiling. Trimming to the ceiling leaves it one frame from breaching again, which is a
permanent stutter instead of one discontinuity.

**A ceiling cannot be exercised by a delay.** A bounded delay cannot make a real-time
stream arrive faster than real time: after a stall the newest frame leads the cursor by the
target and the oldest is already behind it, so occupancy after the burst is at most the
target however long the stall was. The ceiling exists for a sink slower than its source -
a clock difference - and a fault-injection arm that claims to test it is claiming
something impossible.

## Sample the occupancy after serving, not before

The frame being handed to the sink is the sink's current audio, not latency it is waiting
through. Measured before the frame is taken, a healthy stream at a 10 ms target reads
15 ms; measured after, it reads exactly 10.

## Concealment counts as played. An underrun does not.

The continuity counter is the one that matters: how many samples of continuous audio the
stream should have produced against how many the sink actually consumed.

Gap concealment counts as played, because the listener heard something continuous. An
underrun does not, because the concealer is then running on stale state with nothing behind
it - and crediting it is exactly how a path carrying nothing is made to look like one that
works.

## A frame count cannot tell audio from silence

Every audio harness here plays a tone with a **different frequency in each channel** - 997
Hz left, 1997 Hz right - and measures both with a Goertzel filter at the far end. That
proves content and channel order in one check; an inequality would pass with the channels
swapped.

Neither frequency divides 48000, so neither produces a short repeating pattern that a
broken accumulator could fake.

Choose the level for whoever is near the speakers, not for the detector. Loopback and ring
measurements are entirely digital, so amplitude buys the measurement nothing: −20 dBFS for
a capture path, −40 for a playback one that may run unattended.

## Clock drift is not hypothetical

Two devices nominally at 48 kHz are not at the same frequency. Measured here:

```
Windows capture endpoint   -15 ppm against nominal
Mac output device          +5 ppm against nominal
```

About 20 ppm between them: twelve milliseconds of accumulated drift over ten minutes, more
than a 10 ms jitter buffer holds. So a ten-minute run either fills or drains the buffer
completely, and the number is estimable before the phase that addresses it begins.

Measure it before designing a correction, and measure it as a **slope over minutes** -
occupancy per minute, underruns per minute - because ten seconds cannot see it.
