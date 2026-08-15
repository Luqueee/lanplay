---
name: lanplay-lab
description: Driving the two-machine lanplay laboratory from the Mac - the Windows host over ssh, the IddCx virtual display, the frame and audio sources, and the interactive session. Use when a task needs the Windows machine, mentions the host or the virtual display or NVENC, when a remote command behaves oddly, when a lab run reports no frames or no audio, or when a build on the host fails for a reason the code does not explain.
---

# The laboratory

Two machines. The Mac runs the client and every harness; the Windows PC runs the capture,
the encoder, the input injector and an IddCx virtual display driver. They talk over Wi-Fi
and over ssh.

Almost everything that goes wrong here goes wrong in a way that looks like a code failure
and is not. This is the list, in the order the hours were lost to it.

## Reaching the host

```bash
tools/win-ssh.sh 'one command'                    # a command, back in one shot
tools/win-sync.sh                                 # copy the repo to C:\Users\luque\lanplay-rs
tools/win-session.sh 'C:\path\to\log' 'command'   # run it in the INTERACTIVE session
```

`win-ssh.sh` runs in the ssh session, which has no desktop. That is fine for a build and
useless for anything that needs a window, an audio endpoint or a display: those must go
through `win-session.sh`, which launches a scheduled task in the logged-in session and
polls for a sentinel file rather than for the task's state.

### Two invocations at once need two task names

`win-session.sh` derives its wrapper script's path from `WIN_TASK`. Two concurrent
invocations at the default name overwrite each other's command between the copy and the
launch, and the loser reports a timeout while the winner runs twice.

```bash
WIN_TASK=lanplay-target  WIN_TIMEOUT=180 tools/win-session.sh ... &
WIN_TASK=lanplay-inject  WIN_TIMEOUT=180 tools/win-session.sh ... &
```

This cost a whole run that reported "capture produced no frame for one second" while the
real fault was that the injector's task had been clobbered.

### Launch order decides whether a measurement means anything

Console programs take the foreground when they start and hand it back when they exit. A
target window launched *before* a console injector will have lost the foreground for the
entire run - and a foreground check at either end of that run reads true, because both
ends are after the console has gone.

Start the console programs first and the windowed target last. One latency measurement
reported that `SendInput` never reached the window message queue, which is false, purely
because of this.

## The virtual display

`LanPlayIddLabCtl` creates the software device; the monitor arrives asynchronously after
it. `present-source` draws on that monitor, and Desktop Duplication only hands over a
frame while something changes, so **a still desktop produces no capture at all**.

`tools/win/ensure-lab-source.ps1` brings both up and verifies them. Run it before
believing any video run.

### The preflight cannot actually restore the producer

This is the trap worth knowing before you break it. `ensure-lab-source.ps1` starts
`present-source` from inside a scheduled task and verifies it one second later, which
passes - and then the task ends and takes its child with it. The laboratory worked for
days because the producer had been started by hand and left alone, so the script only ever
reported "already running".

Killing the producer and trusting the preflight to bring it back produced one access unit
out of 4800. If the producer has to be replaced, start it by hand, outside a task:

```
target\release\present-source.exe --width 1920 --height 1080 --fps 120 \
    --seconds 0 --fullscreen --monitor 1
```

And check its build is current rather than assuming: a daemon that predates a change
ignores every message the change added, and the arm that depends on it then looks exactly
like an arm where the mechanism does not work. Compare its start time against the
binary's write time.

### A running program holds its own binary

Kill the daemon **before** rebuilding it. Otherwise the link step fails with access
denied, and the error names a file rather than the reason.

## Toolchains on the host

MSBuild is found through `vswhere`; `tools/win/build-idd-lab.ps1` does it and is the
reviewable copy of what built the installed driver.

`cmake` is not on the PATH and is not absent - it ships inside Visual Studio:

```
C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\IDE\
    CommonExtensions\Microsoft\CMake\CMake\bin
```

That matters because `lanplay-audio-codec` vendors libopus in C. With that directory on
the PATH the whole crate builds on the host in under twenty seconds. From the Mac it
cannot be cross-checked for Windows at all, and the error is `couldn't determine visual
studio generator` rather than anything about the code.

## Replacing the display driver

The driver every video measurement depends on is installed from this repository. Before
replacing the package, capture what is installed so it can be restored, and afterwards
verify the monitor came back by running the preflight and reading what it printed. A run
that leaves the host with no virtual display is a failure however good the code is.

The driver's IOCTL and counters exist as an instrument, not a feature: they proved that
holding `IddCxSwapChainFinishedProcessingFrame` moves nothing the receiver can see. The
verdict is at the top of `windows/idd-lab/PhaseContract.h`. Read it before building a
fourth attempt at that lever.

## The radio

The channel gate picks the channel; the current one is 36 at 80 MHz, non-DFS. Signal
strength dominates everything else measured here:

```
-72 dBm, MCS 5,  97 Mbit/s    access units arrive p99 34 ms
-46 dBm, MCS 11, 1200 Mbit/s  access units arrive p99 11 ms
```

At the weaker figure the video gate fails on cadence, the audio jitter estimator declines
every batch for want of a stream that arrives every refresh, and an hour of measurement
describes the radio rather than the code. Check it before a long run:

```bash
system_profiler SPAirPortDataType | grep -A12 "Current Network" | grep -E "Signal|MCS|Transmit Rate"
```

And a datagram addressed to this Mac's own routable address never reaches the radio - the
kernel puts it on loopback. Any figure about the network needs the host as the far end.

## When a run reports nothing

In order, because that is the order of likelihood:

1. Is the producer drawing? A still desktop yields no frames, silence yields no loopback
   audio.
2. Is the client's window unoccluded? An occluded window suspends the display link, and a
   run that fired 887 callbacks in seventy seconds instead of eight thousand reported it
   as a phase problem.
3. Did both tasks get distinct `WIN_TASK` names?
4. Is the signal what it was?
5. Is the daemon the current build?

Only then suspect the code.
