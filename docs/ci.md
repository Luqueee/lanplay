# Continuous integration and releases

## The uncomfortable part first

Almost nothing this project cares about can be tested in CI.

The behaviour that matters lives in an NVENC engine, an IddCx virtual display driver,
a Wi-Fi channel between two physical machines, a 120 Hz panel whose display link
suspends when a window is occluded, an audio endpoint's crystal, and the phase
relationship between two clocks that drift a hundredth of a millisecond per second. A
GitHub runner has none of it. It has no GPU with an encoder, no second machine, no
radio, no panel, and no way to plug one in.

So the shape of an honest CI here is decided before any workflow is written: **CI
proves the code compiles and that its pure logic holds. It cannot prove the system
works, and it must never be arranged so that a green check appears to say otherwise.**

That is not a limitation to work around. It is the same discipline the harnesses in
this repository already follow, and the failure mode is identical: a gate that reads
absence of evidence as evidence. A CI that ran `cargo test` and called the result
"passing" would be claiming a scope it never touched, and the release built from it
would carry that claim to whoever installed it.

## What CI can honestly cover

| covered | how |
|---|---|
| the workspace compiles for every target it claims | `cargo check` per platform, `--locked` |
| pure logic | `cargo test`, 601 tests today, none of which touch a device |
| the declared minimum Rust version | a job pinned to it, because `rust-version` is a promise |
| lint and format | `clippy -D warnings`, `fmt --check` |
| dependency licences and advisories | `cargo-deny` |
| the gate index has not rotted | `xtask gates`, whose test asserts every harness is indexed and every indexed script exists |
| release artefacts build reproducibly enough to attest | provenance attestation over the built binaries |

## What CI cannot cover, stated so nobody has to discover it

| not covered | why | where it is covered |
|---|---|---|
| capture, encode, virtual display | no NVIDIA GPU, no IddCx driver, driver signing needs a certificate | `e2e-gate`, on the lab |
| anything over the air | one runner is one machine, and a datagram addressed to its own interface never leaves it | the lab, two machines |
| presentation and display link | no 120 Hz panel, and an unoccluded window cannot be arranged | `e2e-gate`, `phase-*` |
| audio capture and playback | no render endpoint, no output device | `audio-gate`, `audio-render` |
| clock drift | two physical crystals, and a run of minutes | `phase-lottery`, A7 |
| whether it feels right to play | a person | `game-input-test` |

## The workspace does not build with one command

This is the first practical problem and it is worth stating plainly, because the
obvious workflow is wrong. `cargo test --workspace` succeeds on macOS and fails
elsewhere: five crates under `windows/` are Windows-only and four under `macos/`
depend on `objc2`. Several are gated with `#![cfg(target_os = ...)]` so they compile
to an empty crate off their platform, but not all, and `lanplay-audio-codec` vendors
libopus in C so it needs a C toolchain wherever it is built - `cmake` and MSVC on
Windows, which is why nothing depending on it cross-checks from a Mac.

So the matrix is per-platform package sets, not `--workspace` everywhere, and the sets
belong beside the code that decides them rather than duplicated into YAML. `xtask`
already exists for exactly this kind of thing.

## Release, and the part that is specific to this project

A tag builds artefacts for macOS arm64 and Windows x86_64, attests their provenance,
and publishes them. That part is ordinary and the workflow below does it with the
current practices: actions pinned to commit SHAs rather than tags, least-privilege
permissions raised only in the job that needs them, OIDC for attestation instead of a
long-lived token, `--locked` so the lockfile is authoritative, and no secret ever
reaching a job that does not need it.

The part that is not ordinary is this: **a release must not be publishable while the
lab evidence for it is older than the code it covers.**

Every gate writes its output under `results/`. A release job can therefore ask a
question CI usually cannot: has anybody actually run the real verification against
this commit? If `crates/audio-codec` changed after `results/audio/` was last written,
then the audio gates in that release describe a different program, and the release
notes would be quoting stale numbers as though they were current.

That check is the bridge between a CI that cannot test the system and a release that
should not pretend otherwise. It fails the release, names which evidence is stale, and
says which gate to run. It is the only mechanism here that makes the un-CI-able
verification a release requirement rather than a good intention.

The release notes then carry the gate outputs themselves - not a summary, the numbers -
so that whoever installs a build can read what was measured, on what hardware, and when.

## Things deliberately not done

**No driver in the release.** The IddCx driver needs the WDK to build, which a runner
can install, and a signing certificate to load, which cannot be faked and should not
sit in a repository secret for a lab tool. It stays a locally built artefact, and
`tools/win/build-idd-lab.ps1` is in the repository so that at least the build is
reviewable.

**No macOS notarisation.** Distributing a signed and notarised Mac binary needs a
Developer ID and an Apple ID with an app password. Until this is something people other
than its author install, an unsigned binary with clear instructions is more honest than
a half-configured signing path that fails at the worst moment.

**No `cargo publish`.** Nothing here is a library anybody should depend on.

**No nightly toolchain.** Every dependency here builds on stable, and a nightly job
would eventually fail for reasons that have nothing to do with this code, which is the
fastest way to teach a team to ignore a red check.

**No self-hosted lab runner, yet.** It is the one arrangement that could bring the real
gates into CI: register the Windows lab machine as a runner and let it drive the Mac.
It is genuinely attractive and it is not free - a self-hosted runner executing workflow
code from any branch is a machine anybody with push access can run code on, and this one
has a GPU, a display driver and a network the rest of the lab trusts. If it is ever
done, it needs the runner restricted to protected branches and to workflows that cannot
be edited in a pull request.
