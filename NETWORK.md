# Network robustness and adaptation, what is decided

The goal is not to make any Wi-Fi carry 1080p120. It is that this system works out what link it has
in front of it, knows which thing is failing, and picks the best experience available without asking
anybody to touch a router.

`TASKS.md` is the audio phase. This file is the network phase, and the same rule applies to both:
where it states a number, the run it came from is named so the number can be checked rather than
believed.

## N0, the observation contract

Three tiers, and the separation between them is enforced by the type system rather than by a comment.
A comment saying "never decide from RSSI" erodes; a function that cannot see RSSI does not.

```text
RadioHint          diagnostic only, never decides
StreamBehaviour    decides
Experience         describes what the user got, never decides
```

```rust
pub struct NetworkObservation {
    /// `None` when CoreWLAN did not answer, which must not stop anything:
    /// the classifier never reads this tier at all.
    pub radio: Option<RadioHint>,
    pub stream: StreamBehaviour,
    pub experience: Experience,
}

pub fn classify(stream: &StreamBehaviour) -> NetworkCondition;
```

`classify` takes one argument and it is the middle tier. Radio and experience are not parameters, so
no future edit can quietly start deciding from them.

### Why radio may not decide

A link at -48 dBm negotiating 1200 Mbps produced concealment ratios from 0.196 to 7.442 per cent
across ten arms of the A8 sweep - the steadiest signal this project has measured, and a spread of a
factor of thirty-eight in what the stream actually received. Meanwhile 3 dB of signal difference
between those arms moved the negotiated rate by nothing at all. Signal is a proxy for rate and rate
is a proxy for airtime; the stream's own behaviour is the thing itself, and it was measured
disagreeing with its proxies in the same run.

The hints are worth keeping because they are what turns a report into a diagnosis - a user on 2.4 GHz
can be told so - but they answer *why*, never *whether*.

### Why experience may not decide either, which is less obvious

`fresh_tick_ratio` is measured at presentation, and `crates/link-metrics` exists because measuring a
stage through a later one is how a suspended display link made a healthy link read 141 ms at p99
while it was losing nothing. Anything measured through the display carries the display's faults. So
experience describes what the user got and feeds the interface, and it is structurally barred from
indicting the network for the same reason the radio is.

### What already exists and must not be rebuilt

`crates/link-metrics` is the delivery tier, and it carries more than this contract asked for:

| contract asks for | already there |
|---|---|
| `au_interval_p50/p95/p99_us` | `Window::p50_ms`, `p95_ms`, `p99_ms`, `max_ms` |
| - | the same four over the interval between *first* datagrams, which separates a unit that starts late from one that finishes badly |
| `stalls_2t_per_min`, `stalls_3t_per_min` | `Tail::per_minute(i, span)` over `THRESHOLDS` 1.25, 1.5, 2, 3, 4 and 6 |
| `clusters_per_min` | `Tail::clusters_per_minute(span)` |
| - | `Tail::stall_gap_p50_ms` and `stall_gap_p95_ms` |

That last row is the most valuable input N3 has and it is already written. A tight distribution of
gaps between stalls indicts a timer - a scan, a beacon, a power-save cycle - and a broad one indicts
contention. A stall rate alone cannot tell those apart, and they need different actions.

`crates/capabilities::wifi::association()` is the radio read, passive, already used by
`tools/radio-sample`.

`fresh_tick_ratio` does not exist and has to be defined: the fraction of display ticks that presented
a frame newer than the one presented at the tick before.

And the tiers are not a new idea imposed on this client - they are already its shape.
`macos/client/src/report.rs` separates `network`, `delivery`, `decode` and `display` into their own
structs, with `delivery` carrying the comment *the link's own cadence, independent of everything after
it*, and it keeps `Delivery` apart from `Display` deliberately because delivery cadence used to be read
off the display. `gate.rs` already holds a `lanplay_link_metrics::Window`, and `Report.windows` is
already a vector of per-window rows. So N1 extends an existing separation rather than introducing one,
and any design that has to flatten those structs to work is the wrong design.

`Run.invalidated` and `invalidating_events` are the client's existing way of saying a run's numbers
cannot be trusted because something moved underneath it. A monitor that detects a condition the run
was not measuring belongs there rather than in a new mechanism.

`tools/net-bench` is the traffic generator N2 needs, with `send` and `receive` subcommands and pacing,
already used by `tools/link-pacer.sh` and the channel matrix. N2 shapes a probe out of it rather than
writing a generator, because a probe whose traffic does not look like the product's traffic measures
the wrong link.

### Two constraints with measured numbers behind them

**The radio sampler runs at 1 Hz on its own thread and nowhere near a deadline.** One CoreWLAN
association read costs 3.2 ms at p50 and 15.5 ms at worst, measured by
`tools/radio-sample/examples/read-cost.rs`. The worst case is longer than a 120 Hz frame period and
longer than three 5 ms audio frames, so it cannot sit on any callback. 1 Hz because the quantity moves
in seconds and a faster sampler would only cost more of them.

**No active scan, ever, and it is checked rather than promised.** `system_profiler SPAirPortDataType`
takes the radio off its channel: it was used for one reading this session and produced exactly the
bunching that an experiment had gone looking for. The whole tier is association reads.

## The order

```text
N0  observation contract
N1  passive monitor: radio sampler, rolling short and long windows, fresh_tick_ratio
    and a proof that the monitor itself causes no stalls
N2  startup probe, video-shaped, and a persisted report
N3  degradation classifier, validated offline before it ever sees a live session
N4  intervention shootout - the decision point of this phase
N5  NetworkHealth with a condition, a confidence and a duration
N6  controller in shadow mode, changing nothing
N7  automatic bitrate adaptation
N8  FPS and resolution adaptation, only if N4 proves it
N9  hysteresis and the state machine
N10 protect input and audio before video
N11 interface
N12 fault injection
N13 full-session soak
```

No automatic adaptation exists before N7, and nothing reaches N7 without N4.

### The windows

Short around 3 s so something can react; long around 30 s so nothing reacts to one spike. Both
provisional: N3 fixes them from recorded sessions rather than from this paragraph.

### How N3 gets validated before it is trusted

Offline first, against runs whose answer is already written down. `results/` holds sessions whose
ground truth is in the commits that produced them:

```text
results/audio/jitter-target-a8/     ten arms, zero loss, a cadence tail, worst
                                    arrivals 19 to 221 ms, all on one channel
                 its control arm    a fifth of the audio destroyed on purpose
results/audio/e2e-clean/            a clean 600 s arm at -58 dBm
results/audio/e2e-corrected/        the same pipeline on a link falling -70 to -78
results/b3-channel/                 video across channels, with a pcap beside each run
results/b1-proximity/               the same pipeline near the router and far from it
results/b5-datagram-size/           payload size against delivery
results/soak-1080p120/              a long run whose shape is known
```

Each of those video directories carries a `.wifi.csv` beside its JSON, and the JSON already separates
`delivery`, `network`, `stream`, `display` and `decode`. So the corpus is already in the shape the
three tiers want: a classifier can be handed `network` and `delivery` while `display` is withheld, and
checked against a diagnosis that is written down.

A classifier that cannot label runs whose answer is already documented will not label a live one, and
finding that out costs no hardware time at all.

### What stays out of this phase

Router APIs, automatic channel switching, Wi-Fi scans, a learned predictor, adaptive audio jitter,
FEC, a NACK redesign, Wi-Fi QoS. None is needed for the product problem, and the audio phase already
established why the last two stay closed: there has been nothing to retransmit.

### The failure this phase exists to avoid

A controller that looks intelligent and, faced with a cadence problem, starts lowering bitrate.

The reason to expect that failure is not yet evidence in this repository, and the distinction matters.
`tools/bitrate-sweep.sh` exists and is registered as a gate, and **nothing under `results/` holds its
output**, so the claim that lowering bitrate protects integrity without fixing cadence is currently
owed rather than shown. It is the first thing N4 has to establish or refute, and until it is committed
it may not be cited as a reason for anything.

What is already shown is narrower and enough to justify the order: moving from channel 116 to channel
36 took late access units from 69 a minute to 5.5, recorded in `crates/capabilities/src/wifi.rs`. That
is a link-side change fixing a cadence problem, which is exactly why N4 must not assume a stream-side
lever will.
