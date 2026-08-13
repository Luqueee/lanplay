//! The missing segment of the loop: how long a Windows application takes to
//! turn an injected input into a changed pixel on the display.
//!
//! Every other segment of the round trip has already been measured. The Mac
//! knows what its event callback costs before the datagram leaves, the host
//! knows what `SendInput` costs before the injected event enters the system,
//! and the capture side knows what it costs from a changed desktop to an
//! encoded frame. Between those two there is a hole: nobody has measured what
//! happens inside the application that receives the input. This crate is a
//! target that fills the hole by being the simplest possible application that
//! reacts to input, and by writing down exactly when it reacted.
//!
//! Both ends of the interval are read on the host with
//! `QueryPerformanceCounter`, through [`lanplay_telemetry::Timestamp`]. The
//! figure is therefore a host-local interval and is never to be subtracted
//! from anything the Mac timestamped: the two machines share no epoch and the
//! difference would be a clock offset wearing a latency's clothes.
//!
//! The process runs in the Windows interactive session, launched by a
//! scheduled task, because a window has nowhere to appear in session 0. A
//! process launched that way has no usable stdout, so the report goes to a
//! file named on the command line and the exit code carries the verdict for
//! anything that can only see that. `lanplay_capture::display_mode` works
//! under the same contract and for the same reason.
//!
//! Everything that does not need a window lives outside the `window` module,
//! so the argument parsing, the flash state machine and the report can be
//! built and tested on a machine that has no Windows at all. Only the window,
//! the swap chain and the raw input registration are Windows-only.
//!
//! Exit codes: 0 the run measured something, 2 the display or the graphics
//! stack refused, 3 this is not Windows, 4 the run saw no input at all.

pub mod cli;
pub mod flash;
pub mod report;
pub mod run;

#[cfg(windows)]
mod window;
