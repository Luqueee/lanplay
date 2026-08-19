# Audio, what is left

A1 to A5 are done and committed. This file is what the remaining phases need and what has
already been decided, so that none of it has to be reconstructed from the commits.

`docs/testing.md` has the harness design, `.claude/skills/realtime-audio/SKILL.md` has the
findings that shaped the code, and `tools/gates.toml` says which gates can run where.
`docs/reports/2026-08-15-audio-and-ci.md` is how the phase reached the state below.

## Where the phase stands, after the first clean-link run

**The continuity failure is exactly and only the arrival tail.** On the clean arm the three counts
are not close, they are identical: 4587 frames late, 4587 buffer underruns, 4587 concealments, and
4587 times 240 samples is 1100880, which is the hole to the sample. Every underrun is a late frame
and every late frame is an underrun, so nothing else in this pipeline ever starves the buffer -
the `min occupancy 0.0` that appears in all sixty windows is those moments and nothing more.

That closes the question the phase opened. There is no defect in capture, the codec, RTP, the
jitter buffer or CoreAudio, and there is no longer a suspicion of one.

It also removes the radio as the explanation. The clean arm ran at -58 to -59 dBm on one channel
with the preflight passing, and its hole is **3.82 per cent against the contaminated run's 2.09**.
A better link produced a worse figure, so link instability was never what the hole was made of.

```
A0  contract and telemetry                     done
A1  WASAPI loopback                            done
A2  Opus in isolation                           done
A3  RTP audio, with its radio figure and control done
A4  receiver and jitter buffer                  done
A5  CoreAudio                                   done
A6  Windows to Mac, functional                  works
A6  the continuity criterion                    fails, 3.82 % on a link that held
A7  drift                                       measurable now, and it disagrees with the table
A8  jitter target                               the question has changed, see below
```

### The two contributions, and only one of them is free

The margin is thin by construction before the link does anything. The sender takes a 480-frame
WASAPI packet - 10 ms of audio - and sends both of its 5 ms frames at once, 44 microseconds apart,
so the stream lands as a pair every 10 ms rather than one frame every 5. The second frame of each
pair has its moment 5 ms after the first, so relative to its own moment it arrives 5 ms later than
its partner. At the measured p95 of -4.25 ms that puts the second of each pair at +0.75 ms - past
its moment - which is where the tail crosses zero, and the observed 3.82 per cent sits just beyond
it.

**Pacing the second frame 5 ms after the first would return that margin at no latency cost.** It
is not buying continuity with delay; it is declining to spend margin on a burst this end creates.

But the burst is not all of it, and the natural experiment in the same run says so. Drift deepened
the buffer by 5 ms across the ten minutes and the hole fell by **41 per cent, not by all of it** -
134880 samples in the first minute against 78960 in the last. Had the pair structure been the whole
cause, five milliseconds would have removed essentially every late frame. So a real tail remains
underneath: p99 at +12.2 ms and a worst arrival at +85.0 ms on a link that was not moving.

That tail is delay and not loss - zero packets lost of 120005 - so nothing recovers it. Only more
buffer or less burst helps, which is why FEC and NACK stay out: there is nothing for them to
retransmit.

### What A8's question became

Not "which of 5, 10, 15 and 20 ms holds", which the sweep answered with none. It is now: **pace the
sender, then find the smallest target that holds.** The order matters, because measuring targets
against a stream that wastes 5 ms of every second frame ranks the burst rather than the buffer.

### The A7 discrepancy, recorded and not resolved

Occupancy rose monotonically from 10.0 to 15.0 ms at p50, +0.737 ms/min, with zero overruns. That
is 12.3 ppm of relative rate with **the source faster than the sink**. The table below has the host
audio clock at -15 ppm and this Mac's output at +5 ppm, which predicts the sink draining 20 ppm
faster and a buffer that empties. It fills. Thirty-two parts per million and a reversed sign
between two established figures and a joined run, and it is A7's subject rather than a footnote.

Neither of those two rows is safe to keep using until A7 settles which is wrong.

### The order when a stable link exists

Not the sweep first. A6 has to establish that a base condition exists in which this system works
before anything is ranked against anything.

```
1  per-window occupancy in place
2  audio-e2e-gate 60
3  audio-e2e-gate 600
4  A7 from those same 600 s, if A6 passed
5  jitter-target-sweep
6  choose A8
```

### What stays out until the data asks for it

Opus FEC, NACK, audio retransmission, jitter targets above 20 ms, another codec, another frame
size, per-process capture, an A/V sync controller and an adaptive resampler. And the capture
period, which is not a candidate any more: the losses live in an upper tail of lateness, not in a
fixed cost every frame pays, and the distribution said so before anything was changed.

A8's answer, when it comes, is the **smallest** target that holds continuity reproducibly - not the
one with the prettiest statistics. If 5 ms fails and 10, 15 and 20 hold, the answer is 10. Audio
latency is structural and paid on every frame forever; ten more milliseconds bought for
statistical comfort is a loss.

## What is already established

Numbers below are measured, not assumed. They are the inputs to everything that follows.

| fact | value | where |
|---|---|---|
| host render endpoint | LG ULTRAWIDE, 48000 Hz, 2 ch, 32-bit float | A1 |
| host device period | 10.000 ms default, 3.000 ms minimum | A1 |
| loopback packet size | exactly 480 frames, every packet | A1 |
| host audio clock | −15 ppm against nominal | A1 |
| Opus frame | 5 ms, RESTRICTED_LOWDELAY, 128 kbps constrained VBR | A2 |
| Opus packet | 81 bytes, p50 and p99 alike, 129.6 kbps effective | A2 |
| Opus cost | encode p99 40 µs, decode p99 10 µs | A2 |
| Opus algorithmic delay | 7.5 ms: one 5 ms frame plus 2.5 ms lookahead | A2 |
| RTP | payload type 111, clock 48000, one frame per datagram, no extension | A3 |
| jitter target | 10 ms, ceiling 3× target, PLC concealment | A4 |
| Mac output device | 48000 Hz, 2 ch f32, 256-frame IO buffer, 5.333 ms | A5 |
| Mac output clock | +5 ppm against nominal | A5 |
| producer scheduling | THREAD_TIME_CONSTRAINT_POLICY, not a QoS band | A5 |

Two consequences already drawn:

- A 480-frame WASAPI packet is exactly two 5 ms Opus frames, so the accumulator **splits**
  and never accumulates, and no residue ever carries between packets.
- The two audio clocks differ by about 20 ppm, which is 12 ms of drift over ten minutes -
  more than a 10 ms jitter buffer holds. **A7 is not hypothetical.**

## Debts carried forward

**The radio loss figure is owed.** `tools/audio-rtp-gate.sh` declares it unavailable
rather than faking it. Everything for it is wired: `--receive-only` exists on the probe,
and libopus builds on the host through the cmake inside Visual Studio BuildTools
(`Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin`). It needs the host switched on
and nothing else.

**Nine of nineteen gates have no negative control**, including three of the audio ones.
`cargo run -p xtask -- gates --debt` lists them. A gate whose failure mode has never been
observed has not earned being trusted.

**The ceiling in the jitter buffer is untested and cannot be tested by a delay.** Only a
sink slower than its source breaches it, which is A7's subject. Do not add a fault arm
claiming to cover it.

---

## A6 - first Windows to Mac audio

Join the halves: WASAPI loopback, Opus, RTP over Wi-Fi, jitter buffer, CoreAudio.

Needs the host, the radio and an output device. The encoder must be built **on** the host,
with cmake on the PATH.

Exit criterion, 60 s first and then 600 s, in ten-second windows:

```
capture packets/s, frames encoded/s, RTP received/s
packet loss, PLC count
jitter buffer occupancy p50/p95/p99
decode p99, render callbacks, underruns, overruns
continuous samples expected against played
```

The counter that decides it is the last one. A run whose underruns are zero because the
concealer ran the whole time is a run that carried nothing, and the continuity accounting
is what tells those apart: gap concealment counts as played, an underrun does not.

This is also where the radio loss figure finally arrives, which is the debt above.

## A7 - clock drift

The interesting one, and its number is estimable before it starts: about 20 ppm between the
two devices.

First measure, correct nothing. Ten minutes, and compute occupancy slope per minute,
underruns per minute, overruns per minute. Expect the buffer to drain or fill completely
inside that window; if it does not, the estimate above is wrong and that is the finding.

### What A6's ten-minute run already answered, and what it cannot

The prediction did not materialise, which is the outcome this section asked to be told about.
Over A6's 600 s clean arm, in sixty ten-second windows recorded in
`results/audio/e2e/clean-600s.receiver.out`: **zero buffer overruns, zero device underruns,
and a continuity hole that does not grow.** Per minute the hole reads 107760, 108720, 104640,
100080, 120960, 135840, 96240, 82080, 76080, 97200 samples - a least-squares slope of **-2624
samples per minute** against a mean of 102960, which is noise around a flat line and if
anything a slight decline.

The reason is worth more than the number, because it says A7 cannot be measured on this link
at all. The hole is lateness: 4290 frames arrived past their moment over the ten minutes and
were discarded, and 4290 x 240 samples is exactly the 1029600 the hole came to. Discarding a
late frame sheds backlog, so **the lateness is itself a drift correction**, continuously
dumping the very accumulation drift would build. Drift fills the buffer and lateness empties
it, and while both are present neither can be read.

So A7 needs a link whose tail does not produce lateness - the same condition A6's exit
criterion is waiting on - and not a longer run.

**One instrument gap to close first.** The per-window row carries rtp/s, lost, plc, played,
jitter underruns, callbacks, underruns, overruns, expected, played and hole. It does not carry
occupancy, which is the one quantity this section asks for per minute. Only the run-wide
aggregate exists - p50 15.0, p95 20.0, p99 20.0, max 25.0 ms against a 30 ms ceiling - and a
p50 higher than the 60 s arm's 10.0 cannot be told apart from growth within a run without the
per-window figure. Add occupancy to `WindowRow` before A7 runs, or A7's first number will be
two runs compared instead of one run measured.

Only then rate matching, and only if the measurement demands it: occupancy above target
means consume infinitesimally faster, below means slower, at parts per million and slowly
enough to be inaudible. Never by dropping a packet - that is a click every few seconds in
exchange for an arithmetic problem.

This is the phase whose fault arm can finally exercise the jitter buffer's ceiling, because
a sink slower than its source is exactly what breaches it.

## A8 - choosing the jitter buffer

A shootout at 5, 10, 15 and 20 ms, three runs each, same network and same source, after A7
so that drift is not what is being measured.

Measure underruns, PLC count, occupancy and late packets. The winner is **the smallest
buffer that holds continuity**, not the largest that reports zero faults: if 5 ms gives
twenty underruns a minute and 10, 15 and 20 all give none, the answer is 10.

Each arm must be long enough to sample what varies between the arms. Forty seconds was not
enough for a video phase that swept its period in two hundred, and the same trap applies
here to anything that drifts.

## A9 - fault injection

`tools/udp-fault` already does all of it and takes a seed: loss, duplication, reordering
with a hold, and periodic stalls. No injector needs writing.

Arms at 0.1, 0.5, 1 and 3 per cent loss, plus reordering, duplicates and stalls of 10, 20
and 40 ms. What must hold:

```
loss                 -> concealed, continuity unbroken
duplicate            -> dropped, counted
reordering in window -> decodes normally
late packet          -> discarded, never played
a stall              -> never unbounded occupancy
```

The loss arm must show concealment happening. An arm with nothing concealed either lost
nothing or bypassed the mechanism, and both are failures of the gate rather than passes.

## A10 - audio and video and input together

1080p120 at 40 Mbps, plus Opus, plus input, for ten minutes. Bandwidth is not the question -
audio is small beside video. Interference between subsystems is:

```
does audio add video stalls?
do video bursts cause audio underruns?
do the audio threads disturb NVENC?
does CoreAudio disturb the display link?
does input stay clean?
```

The gate is that all three existing gates still pass. No subsystem may buy its success by
breaking another.

## A11 - A/V sync

Not before both work on their own. Then a Windows source that emits a visual flash and an
audio click from the same event, so relative skew can be characterised without external
hardware.

The product rule, decided in advance so a measurement does not quietly become a policy:
**do not delay video systematically to achieve perfect lip sync.** This is an interactive
streamer. Given a choice between perfect A/V with 20 ms more latency and a small skew with
8 ms less, take the second while the skew stays perceptually acceptable. Measure first, and
say what acceptable turned out to mean.

## A12 - per-process capture

`AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK` includes or excludes a PID and its
children, from Windows 10 build 20348. It offers system audio or game-only audio as a
choice.

Last, and explicitly not allowed to delay anything above it.

## Deliberately out of scope

Surround, spatial audio, Dolby, microphone forwarding, voice chat, echo cancellation,
Opus in-band FEC, audio NACK, separate audio encryption, Bluetooth compensation, per-app
mixing.

FEC and NACK stay out until A6 has produced a real loss figure. The audio deadline is too
short to build recovery because something sounds right, and the plan is explicit that loss
gets measured before anything is built to hide it.
