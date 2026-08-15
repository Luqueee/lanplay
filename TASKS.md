# Audio, what is left

A1 to A5 are done and committed. This file is what the remaining phases need and what has
already been decided, so that none of it has to be reconstructed from the commits.

`docs/testing.md` has the harness design, `.claude/skills/realtime-audio/SKILL.md` has the
findings that shaped the code, and `tools/gates.toml` says which gates can run where.

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
