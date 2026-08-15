# Testing this project autonomously

## What is actually hard here

The interesting behaviour of this system does not live in the code. It lives in a
Wi-Fi channel, an NVENC engine, a virtual display driver, a 120 Hz panel, an audio
endpoint's clock, and the phase relationship between two crystals. Almost none of it
can be observed by running a unit test, and the parts that can are the parts least
likely to be wrong.

So the verification is a set of harnesses that drive real hardware and print numbers,
and the numbers are the deliverable. That works. What repeatedly failed is the
harnesses themselves.

Over one long session of building them, the defects found in the instruments
outnumbered the defects found in the code they measured. They fell into two families,
and both are addressable by construction rather than by care.

### Family one: the parser

Every harness printed keyed lines and then parsed its own output with a bespoke
regular expression. Three separate bugs came out of that.

A pattern anchored with `^` but no multiline flag reported a run of 6001 captured
packets as having captured nothing, and the gate failed a clean run. A gate that can
read a success as a failure is worse than no gate, because it teaches its reader to
ignore it.

A rate computed as frames over a span carried a 150 ppm bias, because the frame count
included the last packet's audio and the span - measured between two timestamps that
both mark a packet's first frame - did not. That would have been the largest term in a
gate whose entire subject was clock drift measured in parts per million.

A key renamed in a probe's report (`target_ms` became `margin_ms`) left the gate
reading a field that no longer existed, and the gate's failure named the wrong thing.

None of these are interesting mistakes. They exist because eight harnesses each
reimplemented the same reading of the same shape of data.

### Family two: the criterion

Five structural defects, each of which passed review at the time.

A gate that could not pass: two arms compared across time, where the quantity being
compared drifts on its own, so the comparison could never isolate the mechanism. Once
every candidate mechanism had measured neutral, it could not pass at all - and a gate
that cannot pass trains its reader that failure here is normal.

A gate that could not fail: criteria all of the form "this counter is zero", on a run
that produced no events. Zero discontinuities over zero packets reads as a clean
sweep. This one recurred five times in different subsystems.

A criterion that could not be met: a demand for zero session expiries, when the host
deliberately holds a longer window than the client so that late retransmissions can
land, so it always sweeps a client that has finished. An impossible criterion is worse
than an absent one.

A comparison that did not control for its variable: two 40 second arms compared to
prove an alignment mechanism worked, when the phase being aligned takes 210 seconds to
sweep its period. The arm credited with a 3 ms improvement had simply started closer to
the target.

A gate that destroyed its own evidence: it cleared its output directory on startup, so
re-running it to re-read a verdict deleted the verdict. Six minutes of measurement lost
to a second invocation.

## The design

Four mechanisms, each aimed at a family above.

### 1. One envelope, one evaluator

Every probe emits a single JSON document to a file. Nothing parses another program's
prose.

```jsonc
{
  "gate": "audio-loopback",
  "run": {
    "started_unix_ms": 1755212345678,
    "span_s": 60.0009,
    "seed": 42,                       // required when any fault is injected
    "args": { "seconds": 60 },
    "commit": "5a34190",
    "arm": "clean"
  },
  "environment": {                     // whatever the run depended on and could read
    "host": "windows",
    "interface": "en0",
    "signal_dbm": -46,
    "display_hz": 119.97
  },
  "declared": ["capture", "tone", "accounting"],
  "exercised": ["capture", "tone", "accounting"],
  "observations": {
    "packets": 6001,
    "frames_captured": 2880480,
    "position_gaps": 0,
    "device_position_span": 2880000
  },
  "checks": [
    {
      "name": "every frame accounted by device position",
      "kind": "must_be_zero",
      "reads": "position_gaps",
      "population": "packets",
      "why": "a device position advancing by other than the previous packet's frame count is a gap of a known size, which is stronger than counting packets"
    }
  ],
  "findings": [
    "the endpoint is 48 kHz stereo, so the path to Opus needs no resampler"
  ]
}
```

The evaluator is `xtask verdict <envelope>`, tested like any other code in the
workspace, and it is the only thing that decides a verdict. A probe reports
observations and states criteria; it does not decide. So a check carries no value and
no verdict, and a document that states either is refused: a value restated beside the
name of the observation it came from is a second copy that can disagree with the
first, which is the shape of three of the defects above, and a verdict written by the
measured party is not a verdict.

`why` is mandatory on every check. A criterion whose reason cannot be written down is
a criterion nobody can review, and four of the five structural defects above would
have been visible in the writing.

### 2. Check kinds that cannot read absence as evidence

A check is data, not code, and the kinds are deliberately few:

| kind | passes when | refuses when |
|---|---|---|
| `must_be_zero` | value is 0 **and** the population that could have produced it is non-zero | the population is zero |
| `must_not_be_zero` | value > 0 | never - this is the evidence check |
| `must_equal` | two observations agree exactly | either is missing |
| `must_be_below` | value < bound, bound stated with its derivation | value or bound missing |
| `must_be_within` | value within tolerance of a target | target missing |

`must_be_zero` carries a mandatory `population` field naming the observation that
proves the check had something to be zero about. This is the mechanism that makes the
recurring defect impossible: "zero discontinuities" must name "packets", and a run with
no packets makes the check `unavailable`, not `pass`.

A check may return `pass`, `fail` or `unavailable`. `unavailable` is not a pass. A gate
with any `unavailable` check reports which and why, and does not claim what it did not
test - the pattern that let A3 close honestly on correctness while stating that loss
over the air was owed rather than given.

`xtask verdict` says so with its exit code: 0 passed, 1 a criterion was evaluated and
disagreed, **2 a criterion could not be evaluated at all**. A run holding both is a
failure, because a criterion that actually disagreed is a stronger statement than one
nobody could read. This paragraph was true and unenforced for a while: `report()`
collected only failures, so an unreadable deciding check was printed under "what could
not be tested" and the verdict above it said `PASS`. A6's harness found it while wiring
itself and had to insist on ten observation keys by name to protect itself; that
insistence stays, as a second lock on a door that is now shut.

### 3. A mandatory negative control

Every gate declares an invocation that is expected to fail, and the harness runs it.
If the negative control passes, the gate fails - whatever the positive arm said.

This is the mechanism against a gate that cannot fail, and it is not theoretical: the
one harness that had it by accident, the input safety gate with its graceful and killed
arms, is the one that never produced a false pass. The video phase gate, which had no
negative control, produced two.

```toml
[gates.audio-loopback]
negative_control = "tone_source_not_running"
# With nothing playing, loopback delivers silence. A gate that passes then is reading
# absence as evidence, and this arm proves it does not.
```

### 4. An index an agent can read

The single largest cost to autonomy is not running the gates. It is knowing which ones
can run right now. Today that question was answered by reading eight shell scripts and
reasoning about each one's prerequisites, every time the environment changed.

`tools/gates.toml` states it as data: what each gate proves, what it requires, how long
it takes, and what its negative control is. An agent that cannot reach the Windows host
filters on `requires`, and gets a correct answer in one read instead of eight.

The `requires` vocabulary is small and physical: `windows-host`, `nvidia-nvenc`,
`virtual-display`, `radio`, `mac-display`, `mac-h264-decode`, `audio-output`,
`audio-endpoint`, `human-attention`. Two of those are worth a sentence each.
`human-attention` matters more than it looks: three gates in this project need somebody
to put a window in the foreground or to keep their hands off the mouse, and an agent
working unattended must be able to tell those apart from the rest mechanically.
`mac-h264-decode` is here because it used to be a unit assertion, which made the claim
that this machine decodes H.264 in hardware a claim about whatever machine ran the test
suite - and a hosted runner has no GPU, so it failed there and proved nothing. As a
requirement it is checked on the machine a gate is about to run on, and a machine nobody
asked reads as unknown rather than as a failure.

### 5. Every gate ends what it starts

A harness that spawns a relay or a probe must kill it on the way out, including on an
interrupt. A process surviving an interrupted run holds a port, and the next thing to
bind that port fails for a reason that has nothing to do with it - which cost a spurious
failure in an unrelated test suite today, and would cost an unattended agent the time to
chase it.

`trap cleanup EXIT INT TERM` is the whole mechanism. It is listed as a design point
rather than a tidiness note because the failure it prevents is indistinguishable, from
the outside, from a real one.

## What this does not fix

Two of the session's defects survive every mechanism above, and it is worth being
explicit that they do.

The comparison that did not control for its variable was a mistake in physics, not in
plumbing. Nothing in a schema knows that a 40 second arm cannot sample a 210 second
period. The only defence is that the reasoning behind a bound has to be written into
`why`, where a reader can see the number and check it against the run length - which is
why `why` is mandatory rather than encouraged.

And a probe can still measure the wrong quantity correctly. The occupancy of the jitter
buffer read 15 ms until it was sampled after the frame was taken rather than before,
because the frame being served is the sink's current audio and not latency it is waiting
through. No harness catches that. What caught it was a number that did not match a
target nobody could explain away, which is an argument for gates that state an expected
value rather than only a bound.

## The order to build it in

The index first, because it costs nothing and it is the piece that pays out immediately
and every day. The evaluator second, with the envelope, and one gate migrated to prove
the shape. The rest migrated as they are next touched rather than in a campaign: a
harness that is working and understood is not worth destabilising for consistency, and
the two families of defect above are found by writing new gates, not by rewriting old
ones.

The gate migrated to prove the shape is `tools/codec-gate.sh`, which needs no hardware
beyond a Mac and runs in under a minute, so the whole loop is readable in one sitting:
`audio-codec-probe --envelope` writes the document beside the keyed report it already
printed, `xtask verdict` decides each arm, and the only arithmetic left in the shell is
the cross-arm exchange rate, whose two terms come back out of the envelopes through the
same parser. Copy that arrangement rather than inventing a second one.
