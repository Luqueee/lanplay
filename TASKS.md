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

### A retraction: the pair argument had its sign inverted

What stood here claimed the second Opus frame of each WASAPI packet arrives 5 ms later relative to
its own moment, and that pacing it would return the margin. **That is backwards and the paragraph
was wrong.**

Lateness is arrival minus deadline. Both frames leave within 44 microseconds of each other, and the
second frame's deadline is 240 samples - 5 ms - after the first's, so:

```
lateness(first)  = t - D
lateness(second) = (t + 0.044 ms) - (D + 5 ms) = lateness(first) - 4.956 ms
```

Arriving at the same instant with a later deadline is **more** margin, not less. The second frame of
each pair is the safer of the two, and the sender's burst cannot be the thing that spends the
margin. No pacing change is authorised, and none should be made on this reasoning.

The rest of that paragraph's arithmetic was fitted to the conclusion after the fact: taking p95 of
-4.25 ms and adding five to reach +0.75 ms produced a number that crossed zero near the observed
3.82 per cent, which looked like corroboration and was a coincidence between a wrong sign and a
plausible magnitude.

What survives is the measurement rather than the story. Drift deepened the buffer by 5 ms across the
ten minutes and the hole fell **41 per cent** - 134880 samples in the first minute against 78960 in
the last - so more margin does remove lateness, without saying where the lateness comes from. And a
real tail exists underneath: p99 at +12.2 ms and a worst arrival at +85.0 ms on a link that was not
moving.

That tail is delay and not loss - zero packets lost of 120005 - so nothing recovers it. FEC and NACK
stay out for want of anything to retransmit.

### A6.1, the pair timing audit, before anything is changed

The sign has to be settled by measurement now, because if a per-pair delta comes out at +5 ms rather
than the -4.956 ms the timestamps require, then something upstream is wrong and it is one of: the
RTP timestamp, the playout deadline arithmetic, a capture timestamp assigned to the wrong sample,
the sign or definition of the arrival-delay metric, or the pair ordering itself.

Report separately, over 60 to 120 s, for the first and second frame of each WASAPI packet: arrival
margin p50, p95 and p99, late count, and underrun count - and above all the per-pair difference
`lateness(second) - lateness(first)`, which is the quantity with a predicted value.

The anchor for a frame's media time is its **sample position**, not the instant a thread processed
the packet. `IAudioCaptureClient::GetBuffer` reports the device position and a QPC value for the
packet, so the first frame belongs at position P and the second at P + 240. Assigning both frames
the packet's single QPC while the receiver reads their RTP timestamps as 5 ms apart would fabricate
exactly a fixed offset in this telemetry, and that is one of the candidates above.

### What A8's question became, and what it is conditional on

Not "which of 5, 10, 15 and 20 ms holds", which the sweep answered with none. The reordering that is
authorised is: **establish that the sender introduces no artificial structure, then find the smallest
target that absorbs the link's natural tail.** Measuring targets against a stream with a cadence
defect ranks the defect rather than the buffer.

What is **not** authorised is the sender change itself. A6.1 has to come back first, because the
reasoning that proposed it was wrong by a sign and a cadence defect has not been shown to exist. If
the audit finds the per-pair delta at the -4.956 ms the timestamps require, the sender is innocent,
the whole tail belongs to the link, and A8 is simply the sweep repeated on a stable link with the
occupancy instrument in place.

And if a spacing change is ever justified, it is not free and should not be described as free. It
holds the second frame 5 ms longer in the sender; its playout latency need not rise, since its
deadline is 5 ms later too, but when it enters the network does change. The claim to be demonstrated
would be that spacing smooths the cadence without raising the playout target, provided the second
frame still arrives before its deadline. Nor may it be implemented by sleeping on the thread that
consumes WASAPI: encoder, then a bounded transmit queue, then a scheduler, which is the shape the
video pacer already settled.

### The A7 discrepancy: the sign is real, the magnitude was never measured

Occupancy at p50 rose from 10.0 to 15.0 ms with zero overruns, and the **direction** is unambiguous:
it went up once and never came back, so the effective producer outran the effective consumer.

The magnitude quoted here before - 12.3 ppm - was not a measurement and is withdrawn. The
per-window p50 takes exactly two values across the whole arm, 10.0 for twenty-six windows and 15.0
for the remaining thirty-four, with a **single step at window 26**:

```
AAAAAAAAAAAAAAAAAAAAAAAAAABBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB
```

A least-squares slope over that staircase gives 12.3 ppm and its endpoints give 8.33 ppm, and
neither is a rate: one step in a two-valued series carries no rate information at all. Occupancy p50
is quantised to the frame duration, 5 ms, because the histogram's buckets are frames, so this
instrument cannot resolve parts per million by construction however long the arm is. The step is
consistent with a slow drift crossing a bucket boundary somewhere between windows 25 and 27, and
equally consistent with something changing once.

What remains is a contradiction in sign, which is enough to stop work. The table below has the host
audio clock at -15 ppm and this Mac's output at +5 ppm, so the sink should drain 20 ppm faster than
the source fills and the buffer should empty. It filled. **Neither of those two rows is safe to use
for designing a rate matcher until A7 settles which is wrong.**

### A7.1, the clock rate audit, measured directly and simultaneously

Two clocks cannot be synchronised across these machines and do not need to be: a rate is measurable
on each machine against its own monotonic clock, in the same run.

On the host, over the same window: the change in the capture device's sample position against the
change in QPC, which is what `IAudioCaptureClient::GetBuffer` reports the packet's position and QPC
for, giving `source_ppm = (samples/seconds / 48000 - 1) x 1e6`. On this Mac, in the render callback:
the change in `sampleTime` against the change in `hostTime`, giving `sink_ppm` from what the device
physically consumed rather than from what it says its rate is.

And a third measure that is independent of both, in samples rather than in quantised milliseconds:
the RTP stream states the samples the source produced and CoreAudio states the samples the sink
consumed, so over a long window `buffer_growth = produced - consumed`. That is the invariant, and
all three have to close - the growth predicted from the two measured rates against the growth
observed. If they do not, a mechanism in the buffer is still unaccounted for.

### The order from here

The stable link exists and A6 has been run on it, so the question is no longer whether a base
condition can be had. It is that two numbers contradict themselves, and neither a sender change nor
a target sweep means anything until they stop.

```
A6.1  pair timing audit      first against second frame: sample and RTP
                             timestamps, arrival margin, late counts, and the
                             per-pair lateness difference, which has a
                             predicted value of -4.956 ms
A7.1  clock rate audit       host device-position against QPC, Mac sampleTime
                             against hostTime, and samples produced against
                             samples consumed; all three must close and agree
                             in sign
A6.2  only if A6.1 shows a defect: the cadence fix, then a clean 600 s, then
                             read what lateness is left
A8    5, 10, 15 and 20 ms on a link that holds, with the occupancy instrument
```

A6.1 and A7.1 are independent of each other and can run in either order or together. Nothing after
them starts until both have answered.

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
