# Working in this repository

Read this first. It applies to every task here, which is why it is short: everything
in it earns its place by having cost something already.

## What this project is

A low-latency streaming system: a Windows PC captures its screen, encodes on NVENC,
sends over Wi-Fi, and a Mac decodes on VideoToolbox and presents on Metal, with input
and audio travelling the other way and alongside. Video and input are finished and
verified; audio is in progress.

The interesting behaviour is not in the code. It is in a radio channel, an encoder
engine, a virtual display driver, a 120 Hz panel, an audio endpoint's crystal, and the
phase between two clocks. That single fact shapes everything below.

## Verification

`cargo test` proves the pure logic. It cannot prove the system works, and CI cannot
either - a hosted runner has no GPU, no second machine and no radio. The real
verification is the harnesses in `tools/`, driven against real hardware, and their
output committed under `results/`.

So: **a green test suite is not evidence that a change works.** Find the harness that
covers what you touched and run it. `cargo run -p xtask -- gates --runnable` says which
can run where you are, and why the others cannot.

`docs/testing.md` explains the harness design and the defects that shaped it.
`docs/ci.md` explains what CI covers and what it deliberately does not.

## The discipline that produced the good numbers here

These are not preferences. Each one is here because breaking it cost a measurement.

**Absence of evidence is never evidence.** A counter reading zero over a population of
zero is not a clean result. Every "this must be zero" needs a companion "and this must
not be", or the check passes hardest when nothing happened.

**A criterion that cannot fail is worse than no criterion.** It teaches its reader that
failure here is normal. If you cannot describe the run that would fail a check, the
check is decoration.

**Refuse rather than repair.** When an instrument cannot answer, it says so and exits
non-zero. It never produces a number from a state it did not check, and it never quietly
converts, pads or defaults its way past a surprise.

**Never subtract a timestamp taken on one machine from one taken on another**, and be
as careful within a machine: `mach_absolute_time` and `mach_continuous_time` diverge by
time spent asleep, and an audio device's clock is not QPC. Where a figure would need to
cross clocks, name the two hops and leave them unmeasured rather than interpolating.

**Derive before building.** Three separate mechanisms in this repository were built and
then measured to do nothing at all. Before implementing a lever, work out on paper
whether it can move what you want, and prefer a cheap experiment to a confident
implementation.

## Verifying your own work

Two mistakes in the obvious recipe hid sixteen clippy findings for a whole session, and
both are easy to repeat.

**Clippy answers from cache.** A crate it has already evaluated is not evaluated again, so
a run that reports nothing may simply not have looked. Force it:

```bash
find crates macos windows tools xtask -name "*.rs" -exec touch {} +
cargo clippy --workspace --all-targets --locked -- -D warnings
```

**Read cargo's exit code, not grep's.** `cargo clippy ... | grep -c error` reports grep's
status, so it prints success next to the words "could not compile". Let the command's own
exit code decide, which is what `-D warnings` is for.

CI is always a cold checkout, so it is authoritative where a local run is not. If CI
disagrees with your local check, CI is right.

## Prose

Comments explain **why**, never what. Prose, never bullet lists. Never write "we", never
name a person or an agent, never leave a `TODO`.

A comment earns its place by recording a decision, a rejected alternative, or a measured
number that explains a choice. `// increment the counter` does not.

Commit messages follow the same rule and go further: they carry the numbers, they name
the alternatives rejected, and they own the defects found on the way. Read the last
twenty for the register before writing one.

## The workspace

`cargo test --workspace` succeeds **only on macOS**. The crates under `windows/` are
Windows-only, those under `macos/` need AppKit, and anything depending on
`lanplay-audio-codec` vendors libopus in C, so it needs cmake and MSVC wherever it is
built and cannot be cross-checked for Windows from a Mac.

No counts appear in this file on purpose. It is loaded for every task, so a number in it
that drifts is a number nobody notices going wrong - two were already stale within a day
of being written.

Cross-check Windows crates with `clippy`, not `check`, and with the exclusions the CI job
uses. `check` compiles and misses every lint, and the lints are platform-specific because
the code under them is: three collapsible `if`s and a hex literal were invisible to a
passing `cargo check` and failed the Windows job one at a time.

```
cargo clippy --target x86_64-pc-windows-msvc --workspace --all-targets --locked \
  --exclude lanplay-client --exclude lanplay-decoder-videotoolbox \
  --exclude lanplay-input-capture --exclude lanplay-renderer-metal \
  --exclude lanplay-audio-render --exclude lanplay-mouse-mover \
  --exclude lanplay-radio-sample --exclude lanplay-audio-codec -- -D warnings
```

`xtask platforms` keeps every exclusion but the last honest: each crate declares its
platforms and that list is checked against the workflow. `lanplay-audio-codec` is excluded
here and not there, because only this machine lacks the linker for it.

Touch the sources first. Clippy answers from its cache, and a whole session was spent
reporting zero warnings from a cache that had never seen the code.

`crates/` is shared, `macos/` and `windows/` are platform halves, `tools/` holds probes,
generators and harnesses, `xtask` holds repository automation.

## Hot paths

A capture callback, an encoder submission, an audio render callback and an input
delivery path all run under deadlines. In any of them: no allocation, no blocking, no
logging, no lock that can be contended, and every buffer bounded and owned before the
path starts.

An audio render callback is the hardest of these. A dropped video frame is replaced by
the next one and nobody learns of it; an unfilled audio buffer is a click, and there is
no version of a click a listener does not notice.

## Tests

A test defends an observable contract, and its name is the sentence it defends -
`a_released_capture_reattaches_the_cursor`, not `test_release`. A test that breaks when
an internal function is renamed was testing the wrong thing.

Never bind a fixed port, and never assert that a released port is free again: the
harnesses hold ports for minutes and a test that fails when the machine is busy teaches
its reader to re-run rather than to look.

## Delegating

Decide the contract before fanning out. Every mechanism that crosses a seam - a message
type, an IOCTL, the direction a phase moves - must be stated in the shared context, not
left for two agents to discover separately. One of the neutral levers above was caught
before being built because an agent had the contract and could see it was wrong; the
other two were not.

Tell each agent what NOT to touch, and that project-wide `fmt`, `clippy` and test runs
are yours to do once at the end.
