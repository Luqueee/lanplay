# 15 August 2026 - the audio phase joined, and continuous integration made honest

A report of one session's work, written once and not maintained. The living documents are
`TASKS.md` for what the audio phase still needs, `tools/gates.toml` for what every harness
proves and what it requires, and `results/` for the runs themselves. Where this file states a
number, the run it came from is named so the number can be checked rather than believed.

Fourteen commits, `2303924` through `9833ab1`. The workspace ends the session at 738 unit tests,
clippy clean on both targets, `cargo deny` clean, twenty-two gates of which six still owe a
negative control, and continuous integration green on all four jobs.

## 1. What was asked and what happened

The session began with continuous integration failing and ended with the first Windows-to-Mac
audio measured end to end. In between, two phases of the audio plan were answered and a third
was refused with a number, and eleven instrument defects were found - most of them in
instruments written earlier the same day.

The through-line is not the code. It is that almost every wrong answer this session produced
came from an instrument that looked like it was working: a gate that passed because its harness
was broken, a counter that saturated at a plausible-looking value, a reporter silenced by a
shell flag, a test that crossed the case it was named for without ever reaching it.

## 2. Continuous integration

Four red runs became green. The useful part was not the fixes but discovering that the run logs
need a token neither machine here has, so two of the failures had been diagnosed by reading
source and guessing, and one of those guesses stood wrong for four hours.

`tools/ci-annotate.sh` closes that: it prints what cargo actually said as workflow annotations,
which the public check-runs API serves, and keeps cargo's exit code so it decides nothing. It
took four attempts of its own, and each failure is recorded in its comments because each was a
different way for a reporter to be silent:

- the runner starts `shell: bash` with `-eo pipefail`, so the failing pipeline killed the step
  before the reporter ran;
- rust puts the thread id between the test name and `panicked`, so a pattern anchored on
  `^thread '...' panicked` matches nothing;
- the panic message is the line after that one and carries no prefix, so it has to be taken by
  position;
- and `CARGO_TERM_COLOR: always` puts an escape sequence at the start of every clippy line, which
  defeats every pattern anchored at `^`.

Three substantive fixes came with it. `tools/radio-sample` is macOS-only and was missing from the
windows job's exclude list, which arrived as `cannot find wifi in lanplay_capabilities` in a crate
nobody was thinking about - and had been dismissed that morning as an artefact of a wrong local
command. It was real. Crates now declare their platforms in their own manifests and
`xtask platforms` checks that against the workflow in both directions, because a grep heuristic
was tried first and wrongly caught `crates/telemetry`, which builds for Windows perfectly well.

Two tests were failing for reasons that were not the environment. `marks_from_two_clocks_are_measured_but_flagged`
built its foreign clock domain as `LocalWindows`, which on the host is the local domain, so
nothing crossed; reproduced here by forcing `ClockDomain::local()`, the old line fails with
`left: 0 right: 1`, byte for byte what CI reported. And a 120 Hz cadence isolation test was a
measurement in a suite that cannot host one: libtest runs its neighbours alongside it, which is
free on ten cores and not on three. All 726 tests were enumerated to find the others - 32 touch a
wall clock, exactly one asserted the machine achieved a rate - and that one now runs in
`tools/cadence-isolation-gate.sh` with the negative control this repository owed.

`AGENTS.md` had told anyone cross-checking Windows to run `cargo check`, which reports no lints at
all. Four Windows-only lints had been arriving one CI round at a time; one local
`cargo clippy --target x86_64-pc-windows-msvc` with the job's exclusions found all four in under
four seconds. One of them was not a defect: `if !(cli.seconds > 0.0)` was deliberate, because
every comparison against NaN is false and `<= 0.0` would have let `--seconds nan` through.

## 3. A hole in how every gate was decided

`xtask verdict` printed `PASS` for runs whose deciding check was never evaluated. `Outcome::Unavailable`
is produced when an observation is missing or a population is zero, and `report()` collected only
`Outcome::Fail`, so an unreadable check was printed under "what could not be tested" and changed
nothing. The root was the return type: `(String, bool)` cannot express the three answers this
project's harnesses have always had.

`Verdict::{Passed, Failed, Refused}` now maps to 0, 1 and 2. A criterion read and disagreed with
is a failure; a criterion nobody could read is a refusal; a run with both is a failure, because a
criterion that actually disagreed is the stronger statement. Proven against the old code rather
than argued - the pre-change binary, built from `cd1362d` in a throwaway worktree, prints `PASS`
on a real envelope with one observation deleted.

Nothing under `results/` changed its answer. The only documents that moved from 0 to 2 were built
to be unreadable, four of them A6's own fixtures - which is why that harness had already insisted
on ten observation keys by name to protect itself from this.

## 4. The audio phase

### A3's two debts, both paid

The radio loss figure had been declared unavailable since A3 rather than faked. Measured: **0 of
3000 packets, 0.000 per cent**, at -71 dBm and MCS 4 - the degraded state of this link, which
makes it a conservative bound rather than a flattering one. The conditions are filed beside the
number in `results/audio/rtp-radio/` because a loss figure without them is not a measurement.

Its negative control cost two attempts and the first is the more instructive. Wired with one
probe sending from the socket it received on, `udp-fault` read it as the reply direction, the arm
lost 2000 of 2000, and the gate went green having exercised nothing. A control that fires because
the harness is broken certifies the thing it never tested. Rewired with separate sockets it reads
**33 lost, 24 duplicated, 18 reordered of 2000** at seed 20250815.

### A6 - the halves joined

Windows loopback to Opus to RTP over Wi-Fi to a jitter buffer to CoreAudio, in
`crates/audio-codec`'s `audio-e2e-sender` and `macos/audio-render`'s `audio-e2e-receiver`, run by
`tools/audio-e2e-gate.sh`. The audio crosses: the contract tone comes out of the played samples at
996.984 and 1996.975 Hz, decode p99 19 us, both ends granted the scheduling they asked for.

From `results/audio/e2e-corrected/clean-600s.receiver.out`, ten minutes:

```
rtp received 120007      rtp lost 0 of 120007      rtp late 2499
arrival delay ms   p50 -17.7   p95 -10.2   p99 10.8   max 232.8
jitter occupancy   p50 15.0    p95 20.0    p99 25.0   max 25.0
continuity expected 28801200 played 28200240 hole 600960
```

**The exit criterion is not met**, at 2.09 per cent of expected samples. The two independent
counters agree: 2499 late frames times 240 samples is 599760 against a hole of 600960, one
window's rounding apart.

Three things about that number matter more than the number. Device underruns were zero in every
arm, which is exactly why continuity and not underruns is the criterion. Negative arrival delay is
margin in hand, so p50 -17.7 and p95 -10.2 mean the median frame arrives with the whole target
spare and only the top few per cent are late. And the first explanation offered was wrong: a 10 ms
capture period against a 3 ms minimum would have been a fixed cost every frame paid, and the
distribution says the losses live entirely in an upper tail. Asking for the distribution before
acting on it is what stopped a change to the sender that would have fixed nothing.

### A7 - answered from A6's recordings, and blocked for a reason

A6's ten-minute arm recorded sixty ten-second windows, so A7's first measurement needed analysis
rather than hardware. Zero buffer overruns, zero device underruns, and a continuity hole that does
not grow: a least-squares slope of **-2624 samples per minute** against a mean of 102960.

The reason is worth more than the slope. The hole is lateness, and a late frame is discarded, which
sheds backlog - so **the lateness is itself a drift correction**, dumping continuously the
accumulation drift would build. Drift fills the buffer and lateness empties it; while both are
present neither can be read. A7 needs a link whose tail produces no lateness, not a longer run.

One instrument gap is named in `TASKS.md` rather than worked around: the per-window row carries
eleven counters and not occupancy, which is the one quantity A7 asks for per minute.

### A8 - swept, and refused

Thirteen arms of 120 s, 38m26s of measurement, in `results/audio/jitter-target-sweep/`:

```
pass 1   20 ms 21.550 %   15 ms 32.171 %    5 ms 36.884 %   10 ms 37.358 %
pass 2   15 ms  1.129 %   20 ms  1.308 %   10 ms  2.054 %    5 ms  2.562 %
pass 3   20 ms  0.758 %   15 ms  1.317 %   10 ms  1.750 %    5 ms  2.379 %
```

No target between 5 and 20 ms held continuity, so the sweep refuses to rank failures: the choice
A8 exists to make is owed rather than read off an ordering of things that all broke. The term that
decides it is that **the worst arrival came 87 ms past its moment, 4.4 times the largest target the
phase is allowed to consider.**

The arrangement is what makes those rows readable at all. Pass 1 landed in a bad spell and lost an
order of magnitude more than the others, and the ordering inside every pass still came out the
same, larger monotonically better. Three passes with the second ordered exactly opposite to the
first means a monotone drift contributes a term proportional to position that cancels by
construction rather than by hope. Interleaving finer than an arm was rejected because the target is
fixed when the buffer is built; reference bracketing was rejected as a worse spend of the same
minutes. The incumbent 10 ms appears in every pass and its three arms disagree by 2.57 ms on the
median frame's margin - half the step between adjacent targets, which is the instrument's noise
floor and is declared as a refusal threshold rather than left implicit.

## 5. The defect that invalidated two committed runs

`Loss::expected()` took its span from RTP's sixteen-bit modular distance, which can only place a
packet within half the sequence space. Past 32767 packets from the base it returns negative, the
guard stops updating, and the span saturates at **32768 forever**. At 200 packets a second that is
163.8 seconds: every sixty-second arm was right and every ten-minute arm reported 32768 expected
against the 120000 it sent.

What gave it away was a denominator appearing twice. One run said "0 lost of 32768" and an earlier
one on a different radio said "4290 of 32768" - two links, the same figure to the digit, and 32768
is two to the fifteenth.

The test standing beside it passed. `loss_survives_the_sequence_wrap` crosses the wrap with four
packets, which never leave the half of the space a signed distance can describe. It was true, it
was about the right subject, and it gave cover to a saturating counter through two committed
ten-minute runs. The replacement spans 120000 packets because that is what the phase runs, and a
second test puts a reordered packet across the wrap so counting cycles cannot be confused with
guessing them.

Continuity is computed from samples the playout cursor travelled rather than from sequence numbers,
so the holes stand. What did not stand was any per-packet fraction quoted against 32768, and "0
lost over 600 s" was unproven rather than wrong.

## 6. The instrument defects, as a list

Because the pattern is the finding. Eleven, of which nine were introduced during this session:

1. A gate control that fired because its relay direction was wrong, and passed.
2. `git checkout <path>` on unstaged work, twice, the second time after apologising for the first;
   one commit message described workflow changes the commit did not contain.
3. `grep "set +e"` reading `+` as a quantifier, so a correct file appeared empty twice.
4. The CI reporter silenced by `-eo pipefail`.
5. The same reporter defeated by the thread id in rust's panic line.
6. The same reporter defeated by ANSI colour at the start of every line.
7. `system_profiler SPAirPortDataType` scans, which takes the radio off channel - the instrument
   that had once produced the bunching an experiment went looking for. Three radio readings were
   taken with it before a harness pointed it out. CoreWLAN reads without scanning.
8. `radio-sample` takes no arguments: it ignored `--seconds 1100` in silence, sampled its fixed
   120 seconds, and answers `--help` by sampling for 120 seconds.
9. `Loss::expected()` saturating at 32768, with a test that crossed the wrap without reaching it.
10. `xtask verdict` printing PASS for criteria nobody could read.
11. `tools/gates.toml` describing `wifi-matrix` as ranking channels and widths, which its script
    cannot do - found by reaching for it as the instrument that would settle A6's tail.

Four candidates were rejected before being built, which is the same discipline arriving early
enough to be cheap. A mono decoder as a control for the codec gate would have aliased 997 Hz to
1994 Hz, three hertz from the other channel's target and inside the tolerance, so the arm would
have reported the right channel holding for a reason nobody could defend. A silent endpoint as a
control for the capture gate produces the same report as a probe that never started. A capture
period of 3 ms would not have touched A6's tail. And `wifi-matrix` would not have answered the
question it was reached for.

## 7. What is blocked, and by what

One thing, and it is not code. The radio does not hold a signal for the length of a measurement:
sampled at -52 dBm and 1200 Mbps immediately before a run, a 1 Hz CoreWLAN trace read -70 dBm at
t=0 falling to -78 by t=120, at 288 to 432 Mbps. Two A6 attempts, both labelled honestly by the
harness's own before-and-after readings, and neither is a clean-link run.

A6's exit criterion, A7's drift and A8's choice all wait on the same condition. Shortening the
window until the link holds would be choosing the measurement to fit the instrument.

When the link is good for twenty minutes, `tools/audio-e2e-gate.sh 60 600` and
`tools/jitter-target-sweep.sh` answer all three without anything else being written.

## 8. What is owed

- Six of twenty-two gates have no negative control. `cargo run -p xtask -- gates --debt` lists
  them; the remainder all need the host, the virtual display or a person at the machine.
- Occupancy is missing from the per-window row, which A7 needs before it runs.
- `tools/audio-rtp-gate.sh` still parses its own prose with a regular expression, the family of
  defect the envelope exists to end. It has a negative control now; it has not been migrated.
- The GitHub actions are pinned to tags rather than commit SHAs. `tools/pin-actions.sh` exists and
  needs a token this machine does not have. The claim in `ci.yml`'s header that
  `xtask actions --check` enforces it is false: that subcommand does not exist.
