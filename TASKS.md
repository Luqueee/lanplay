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

### A6.1 answered: the sender is innocent and the tail is the link's

The per-pair difference `lateness(second) - lateness(first)` is what the timestamps require, measured
on three arms rather than argued:

| arm | p50 | p95 | in [-5, -4) ms | pairs |
|---|---|---|---|---|
| clean-90s | **-4.997 ms** | -4.992 | 96.0 % | 8998 |
| a61-pair-90s | **-4.996 ms** | -4.988 | 96.4 % | 9000 |
| clean-1200s | **-4.996 ms** | -4.988 | 95.9 % | 120004 |

Against a prediction of -4.956 ms. The second frame of each packet has five milliseconds **more**
margin, so the burst cannot be what spends it, and the retracted paragraph above was wrong in exactly
the way the arithmetic said it was.

It is the **first** frame that is late, consistently: 524 against 384 on one 90 s arm, 476 against 354
on the other, 8594 against 6391 over the 1200 s. The opposite of the claim that started this.

Two internal checks make the figures load-bearing rather than plausible. The population closes to the
sample - 524 + 384 = 908 = the late count, 524 + 385 = 909 = the underruns, and 909 x 240 = 218160 =
the continuity hole - and the within-pair step is mirrored by the cross-packet one, -4.996 against
+4.979, summing to -0.017 ms so that nothing is left unaccounted per 10 ms of audio.

**No cadence defect exists. No sender change is justified and none should be proposed on this
ground.** The whole arrival tail belongs to the link.

The candidate that would have fabricated this offset was checked and does not exist: no frame carries
a capture timestamp and no QPC value reaches the wire. The second frame's timestamp is the first's
plus 240 by counter arithmetic, which is what RFC 7587 asks for and is also why the clock audit found
the wire carrying the device's rate as an identity.

How the pairing was recovered is worth keeping, because the obvious route does not work. A 480-frame
packet becomes two datagrams 240 ticks apart, so every frame falls into one of two residue classes
modulo 480 computed from its own timestamp - immune to loss and to reordering, and needing no arrival
time. But which class is *first* cannot come from the wire: the packetiser seeds its timestamp from
`random_u32()` as RFC 3550 requires, and a receiver joining a running stream cannot tell position from
an absolute residue. Deciding it from arrival order was rejected as circular, since arrival order
within a pair is the question. So one integer went into the sender's internal telemetry and not into
the protocol - the packetiser's starting timestamp, read before any datagram can move it - and the
join is `(anchor - base) mod 480`. It came out 0 on all three arms, and the base and anchor being
identical also proves each receiver caught the stream's very first datagram, that timestamp being
unique for twenty-five hours.

### What A8's question is, now that both audits have answered

Simply the sweep repeated on a link that holds, with the occupancy instrument in place. There is no
cadence to fix first, so A6.2 is skipped.

The criterion stands as decided: the **smallest** target that holds continuity reproducibly, not the
one with the prettiest statistics. And one caution the audits produced: arrival-delay percentiles are
not comparable between arms, because the anchor is the first admitted packet's own arrival plus the
target and a bad first packet shifts every delay in a run by a constant. Late counts and continuity
holes are comparable; p50 and p95 of arrival delay are not.

### A8 attempted on 19 August and refused by the link, before any arm ran

The sweep was armed with three things it did not have before - `tools/radio-preflight.sh` as a hard
precondition, a per-arm interpretability check, and the per-arm depth control - and then it did not
run, because the preflight refused. Three live 120 s windows, all on channel 100:

| window | RSSI | slope | half medians |
|---|---|---|---|
| 22:36 | -68 -> -70 dBm | -0.593 dB/min | -1.0 dB |
| 22:38 | -71 -> **-59** dBm | **+6.907 dB/min** | +10.0 dB |
| 22:41 | -59 -> -61 dBm | -1.474 dB/min | -1.0 dB |

The link is not falling, it is **swinging twelve decibels in six minutes**, and none of the three can
project across a 600 s run inside the 3 dB budget. A forty-minute sweep across that ranks the swing.

A mistake worth keeping, because it is the reason this is written down rather than retried. After the
first refusal a 30 s sample read a flat -59 dBm and that was used to argue the refusal was transient.
It was taken at the top of the 22:38 climb: **a window short enough to be convenient cannot see a
swing slower than itself**, and the instrument was right both times while the argument against it was
not. Two windows were taken and both are recorded; a third would have been shopping for a pass.

What the arming added, and the failure each part is capable of:

The **interpretability check** turns A6.1's finding into an instrument. That run had 4587 frames late,
4587 underruns, 4587 concealments and exactly 4587 x 240 samples of hole, and the identity is the
pipeline's shape rather than a coincidence: nothing else starves this buffer and nothing else fills an
underrun. An arm where it stops holding has some other mechanism in it and its percentage reads like
one that does not, so it is refused rather than noted. Demonstrated by lowering `plc_frames` by one in
a copy of the clean 600 s envelope: the real document is interpretable and the copy is refused at a
single frame of disagreement.

The **depth control** records occupancy at both ends of every arm and its slope. It decides nothing -
A7 is closed and a sweep does not reopen it - and exists because a target that ran while the buffer
sat three frames deeper was ranked with three frames it did not earn. A7's +9.29 ppm projects 1.1 ms
across a 120 s arm, below the 5 ms this instrument resolves, so the check is between arms and not
within one: if the targets that held began a whole frame deeper than the ones that broke, the report
says the winner is not safe to build on.

And a defect found in that control before it ever ran: re-deciding an older record raised a
`KeyError`. A record written before those columns cannot answer the question, and a traceback is
neither an answer nor a refusal. It now refuses and names the reason.

The ranking contract, restated because the audits changed it: rank on **late frames, continuity hole,
concealments and underruns**, never on arrival-delay percentiles. If every target fails, A8 has no
answer on this link and the targets are **not** extended to 30, 40 or 80 ms - that would be a product
decision about the latency budget, taken elsewhere.

### What A8's preconditions became, and why the projection stopped deciding

Two questions were being asked as one, and only the categorical half belongs before a run.

**Before the run, and binding: the channel.** Channel 36 at 80 MHz occupies 5170 to 5250 MHz, and
5150-5250 is the only WAS/RLAN band in Spain with no DFS obligation - CNAF note UN-128 as rewritten by
Orden ETD/625/2023 imposes DFS on 5250-5350 and 5470-5725, pointing at EN 301 893 v2.1.1, whose radar
detection attaches to any channel whose nominal bandwidth falls partly or completely within either
range. So the non-DFS set is **36, 40, 44 and 48**, the 36/40/44/48 block is the only non-DFS 80 MHz
configuration available here, and channel 100 - centre 5500, 80 MHz span 5490 to 5570 - is not one of
them. The width is required alongside the channel because the obligation attaches to the occupied
span: 160 MHz anchored at 36 reaches 5330 and is a radar band. A8's twelve committed arms all ran on
36 at 80 MHz with `radar_band 0`, so this is the baseline recovered rather than a new preference.

`radar_band` is **this repository's own derived column**, not anything the OS reports. CoreWLAN exposes
`channelNumber`, `channelWidth` and `channelBand` and nothing else; neither "radar" nor "dfs" occurs
anywhere in its headers. `Association::uses_radar_band` computes it by intersecting the occupied span
against the two ranges with strict inequalities, which is why the 36/80 block - whose upper edge
touches 5250 exactly, the same number that starts the first DFS range - is correctly not flagged.
`crates/capabilities` already tests that boundary and the 160 MHz case beside it.

**Not before the run, and no longer deciding: whether the signal will hold still.** That criterion fits
a line to a two-minute window and extrapolates it to ten, and this radio was measured at -0.593,
+6.907 and -1.474 dB/min in three consecutive windows on one evening. A line through any part of a
swing projects a disaster that may not arrive. Worse, the 3 dB it is judged against was derived as the
spread of median signal **between** A8's arms, so applying it to a projection inside one window put a
between-arm number in a within-window place. The sweep is counterbalanced so that a monotone drift
contributes a term proportional to position, which cancels; what it cannot survive is arms measured in
different regimes. So the projection is downgraded to a note for this caller, and the question it was
standing in for is asked of the arms themselves.

**After the run, and binding: overlap.** Every arm records signal and rate at p10, p50 and p90 as well
as its extremes, plus how many channels it saw. The criterion is that the intersection of the arms'
p10-to-p90 signal intervals is non-empty - a band of signal every arm actually spent time in. It needs
no threshold: either such a band exists or it does not. Arms at -58..-61 and -70..-78 have none and are
two links; arms at -57..-62, -58..-63, -57..-61 and -59..-63 intersect over -59..-61 and are one link
breathing, which is the most this radio has ever offered. Both are exercised as fixtures and refuse and
pass respectively. A channel change **inside** an arm refuses that arm outright: it is two links
wearing one name, and every percentile of it is a mixture.

The `RSSI_SPREAD_DB=8` that used to guard this is gone. Applied to the spread of arm means it admitted
two arms with equal means and disjoint ranges, and refused four arms whose ranges nested.

Two defects found while wiring this, both in the instrument rather than the pipeline:

The comparability checks **only ran when the outcome was mixed**. A sweep in which every target broke
skipped all of them and then printed "no target between 5 and 20 ms held continuity" - a statement
about a link, made by arms that had never been shown to share one. The strongest claim this gate can
print was resting on the weakest evidence it collects. The radio checks now run unconditionally, and
incomparability refuses **before** the ranking fails, because a ranking of arms from different regimes
is not repaired by knowing how different they were.

And the first version of the per-arm distribution used `asort`, which is a gawk extension; this
machine's awk is the one true awk and does not have it. It would have refused every arm for having no
rows. It is python now.

Re-checked against the committed A8 record, which the new criteria accept **by a hair**: the arms'
p10-p90 intervals intersect at exactly -68 to -68 dBm, a degenerate band, and their median rates run
288 to 576 Mbps, a factor of 2.00 against a limit of 2.0. That sweep's conclusion stands and it was
never robust.

### A7 answered: both clock figures were right and the prediction crossed a boundary

Measured directly over a 1200 s arm, each rate against its own machine's monotonic clock and nothing
subtracted across the pair:

| quantity | value | population |
|---|---|---|
| host capture device against QPC | **-15.0769 ppm** +-0.0001 | 125999 readings over 1260 s |
| this Mac's output against `mHostTime` | **+5.1545 ppm** +-0.00003 | 224993 IO cycles over 1200 s |

So the two rows in the table below are correct to a quarter of a part per million, and the
contradiction was never in them. It was in the arithmetic that used them: `source_ppm` is samples per
**QPC second** and `sink_ppm` is samples per **mach second**, and subtracting the two is subtracting
rates held against the reference clocks of two different machines - the thing `AGENTS.md` forbids,
committed in a formula rather than in a timestamp.

The term that was missing is QPC against mach, which no measurement taken on either machine alone
can supply. The invariant supplies it, from samples produced against samples consumed:
**-24.37 ppm +-1.30, 18.8 sigma from zero.** Referred to this Mac's timebase the host's audio clock
is therefore +9.293 ppm and not -15.077, and the account closes:

```
host vs mach   +9.293 ppm      (-15.077 vs QPC, plus 24.37 of QPC vs mach)
Mac  vs mach   +5.155 ppm
net filling    +4.139 ppm  ->  +0.1987 samples/s  ->  +238.4 samples over 1200 s
observed                       +0.1985 +-0.0622   ->  +238 +-75
```

**The consequence is the part that matters for the phase.** A rate matcher must be driven by the
observed buffer and never by these two figures, however precise they are, because the quantity it has
to null is the host's audio clock against *this* machine's timebase, and that is not measurable on
either machine alone. It is measurable exactly as it was measured here.

Two corroborations, one of which was withdrawn during the audit and is worth recording as a trap.
The arrival delay's p50 moved from -10.9 ms on a 90 s arm to -36.3 ms on the 1200 s one, which looks
like drift and cannot be used as one: the anchor is set by the first admitted packet's own arrival
plus the target, so a first packet that landed in a bad moment shifts every delay in the run by a
constant, permanently, and a 25 ms anchor offset reproduces that whole move with no drift at all. It
corroborates the sign and nothing else, and separating the two needs a per-window arrival delay this
build does not record.

What does corroborate is the occupancy staircase, and only because this arm has more than one step:
0.0 to 5.0 to 10.0 to 15.0 ms with steps at windows 15, 56 and 96, 405 s per 5 ms step, **+12.35
ppm** against the +9.29 the invariant predicts for the jitter buffer alone - agreeing inside the one
frame that instrument is quantised to. A6's single-step staircase carried no rate, which is why its
12.3 ppm was withdrawn; three steps do.

The sample bookkeeping closed exactly and independently of all this: 60479520 frames captured from
device positions with zero position gaps and zero rewinds, the same 60479520 encoded with zero sample
disagreement and zero split residue, 251998 datagrams with every timestamp step exact, and
251998 x 240 = 60479520. The RTP timestamp is not a nominal 48 kHz counter running beside the device;
it is the running total of frames the device delivered, so the wire carries the device's rate as an
identity rather than as an assumption.

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
A6.2  SKIPPED - A6.1 found no cadence defect, so there is nothing to fix
A8    5, 10, 15 and 20 ms on a link that holds, with the occupancy instrument
```

Both audits have answered. A6.1 cleared the sender at -4.996 ms per pair against a required -4.956,
and A7 closed to +238.4 samples predicted against +238 +-75 observed once the QPC-to-mach term was
measured rather than assumed away. A8 is the next thing to run, and it needs `tools/radio-preflight.sh`
to pass first.

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
