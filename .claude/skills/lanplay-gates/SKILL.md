---
name: lanplay-gates
description: Writing and running the hardware harnesses ("gates") in the lanplay repository. Use when asked to verify something on real hardware, to write or fix a gate or harness or probe, to work out which gates can run right now, or when a gate reports a result that looks wrong. Also use before trusting any number a harness printed.
---

# Gates

A gate is a harness that drives real hardware and prints numbers. In this repository the
gates are the verification: `cargo test` proves the pure logic and CI proves the code
compiles, but a hosted runner has no encoder, no second machine and no radio, so nothing
except a gate can say the system works.

Which makes the gates load-bearing, and makes a defective gate more dangerous than
defective code. Over one long session of building them, **the defects found in the
instruments outnumbered the defects found in the code they measured.** Everything below
exists because of one of them.

## Before writing anything: what can run

```
cargo run -p xtask -- gates --runnable
```

That reads `tools/gates.toml` and probes the environment. It answers in one command what
was previously re-derived by reading eighteen shell scripts every time a machine was
switched off or a radio degraded.

Read its `environment` block carefully. A requirement is `absent`, `present` or
**`unknown`**, and the third is not a shade of the first: if the Windows host does not
answer, `nvidia-nvenc` is not absent, nobody looked. A suite that treats unknown as
absent shrinks silently; one that treats it as present fails for reasons that have
nothing to do with the code.

`--debt` lists the gates whose failure mode has never been observed. `--json` is the
same data for a program.

## The eight defects, and the mechanism against each

Three were failures of parsing and five of criteria. Both families are avoidable by
construction rather than by care.

### Parsing

**Anchor with the multiline flag.** A pattern with `^` and no `re.M` reported a run of
6001 captured packets as having captured nothing, and the gate failed a clean run. A gate
that can read a success as a failure is worse than no gate: it teaches its reader to
ignore it.

**Do not compute a rate from a count and a span that describe different intervals.** A
device position and a QPC position both timestamp the *first* frame of their packet, so a
span between two of them excludes the last packet's audio that the count includes. One
480-frame packet over a minute is 150 ppm, and it would have been the largest term in a
gate whose subject was drift measured in parts per million. Measure the elapsed quantity
between the two instants the span was actually taken at.

**A renamed key is a silent failure.** When a probe's report changes a field name, the
gate reading it fails for the wrong reason. Prefer one machine-readable envelope over
per-gate regular expressions; see `docs/testing.md` for the schema.

### Criteria

**Every zero needs a population.** "Zero discontinuities" is meaningless without "over
how many packets". A run that produced no events passes every zero-check there is. This
recurred five times in different subsystems, and it is the single most common way a gate
lies.

**Every gate needs an arm that must fail.** If you cannot describe the run that would
fail a check, the check is decoration. The one harness here that had two arms from the
start - a graceful ending and a killed one, checked apart rather than summed - is the
only one that never produced a false pass. Nine of nineteen gates still have no negative
control, and `--debt` counts them.

**A criterion must be meetable.** One gate demanded zero session expiries while the host
deliberately holds a longer window than the client, so it always sweeps a client that has
finished. An impossible criterion is worse than an absent one.

**A comparison must control for what moves between its arms.** Two forty-second arms
were compared to prove an alignment mechanism, when the quantity being aligned takes two
hundred seconds to sweep its period; the arm credited with a 3 ms improvement had simply
started closer to the target. Before comparing two runs, ask what changes on its own
between them and whether the run is long enough to average it out.

**Never let a gate destroy its own evidence.** One cleared its output directory on
startup, so re-running it to re-read a verdict deleted the verdict. Stamp the output
directory with the time and never clear it.

## The shape of a gate

Start from `assets/template.sh`. It arrives with the trap, the stamped output, the arms
and the verdict sections already in place, because those are the parts that are always
the same and always forgotten.

Three sections in the verdict, and the order matters:

```
must not be zero        the evidence that the run happened at all
must be zero            the faults, each naming its population
findings                what was measured but does not vote
```

A **finding** is not a soft failure. It is a number the gate exists to produce and has no
opinion about: what the radio lost, what a frame duration costs in bitrate, which
direction a phase moved. Put them above the verdict so they survive a failure, because a
failing arm does not make its measurement uninteresting.

## End what you start

```bash
trap cleanup EXIT INT TERM
```

A relay or probe surviving an interrupted gate holds a port, and the next thing to bind
it fails for a reason that has nothing to do with it. That cost a spurious failure in an
unrelated test suite, and from the outside it is indistinguishable from a real one.

Seed anything that injects faults, so an arm that fails fails the same way twice.
`tools/udp-fault` takes `--seed` and injects loss, duplication, reordering with a hold,
and periodic stalls - use it rather than writing an injector.

## When a gate reports something surprising

Suspect the gate first. That is not humility, it is the base rate here.

Work through `assets/checklist.md`. It is eight questions, one per defect above, and it
is faster than reasoning from scratch every time.

## Registering it

Add the gate to `tools/gates.toml` in the same change. A test asserts that every harness
is described and every described script exists, so an unregistered gate fails the suite -
which is the point, because the index is what makes an agent able to work unattended.
