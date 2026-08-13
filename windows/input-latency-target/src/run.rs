//! Turning one run into a file and an exit code.
//!
//! The verdict lives here rather than in the window because it is a judgement
//! about what was observed, and a judgement is worth testing on a machine that
//! cannot open a window.

use std::path::Path;
use std::process::ExitCode;

use crate::report::Observed;

/// The display, the graphics stack or the window refused.
pub const EXIT_REFUSED: u8 = 2;
/// There is no Windows desktop here.
pub const EXIT_NOT_WINDOWS: u8 = 3;
/// The run completed and measured nothing.
pub const EXIT_NO_INPUT: u8 = 4;

#[cfg(windows)]
pub fn main() -> ExitCode {
    let cli = crate::cli::Cli::from_args();
    match crate::window::run(&cli) {
        Ok(observed) => {
            emit(cli.out.as_deref(), &crate::report::render(&observed));
            ExitCode::from(verdict(&observed))
        }
        // The refusal goes to the report file as well, because under the
        // scheduled task the file is the only place anyone will look and an
        // empty one is indistinguishable from a run that never started.
        Err(why) => {
            emit(
                cli.out.as_deref(),
                &format!("input-latency-target: {why}\n"),
            );
            ExitCode::from(EXIT_REFUSED)
        }
    }
}

/// There is no window, no swap chain and no raw input queue here, and a report
/// full of plausible zeroes would be worse than none.
#[cfg(not(windows))]
pub fn main() -> ExitCode {
    eprintln!(
        "input-latency-target: needs Windows; there is no window to flash and no raw input \
         queue to read here."
    );
    ExitCode::from(EXIT_NOT_WINDOWS)
}

/// Writes the report where the command line asked for it.
///
/// Standard output as well as the file, because a run started by hand from a
/// console should show its answer, and a write that failed must not be the
/// only copy that ever existed.
pub fn emit(out: Option<&Path>, text: &str) {
    print!("{text}");
    if let Some(path) = out
        && let Err(error) = std::fs::write(path, text)
    {
        eprintln!(
            "input-latency-target: cannot write the report to {}: {error}",
            path.display()
        );
    }
}

/// Zero only when the run actually measured something.
///
/// A run that saw no input has a well-formed report with a zero in every
/// column, and nothing in that report distinguishes it from a fast machine
/// until somebody reads the prose. The exit code has to make the distinction
/// for the gate that never will.
pub fn verdict(observed: &Observed) -> u8 {
    if observed.saw_input() {
        0
    } else {
        EXIT_NO_INPUT
    }
}

#[cfg(test)]
mod tests {
    use super::{EXIT_NO_INPUT, verdict};
    use crate::report::{Display, Observed};

    fn observed() -> Observed {
        Observed::new(
            Display {
                index: 0,
                device_name: "\\\\.\\DISPLAY1".into(),
                monitor_name: String::new(),
                adapter_name: "LanPlay IDD-LAB 1080p120".into(),
                left: 0,
                top: 0,
                width: 1920,
                height: 1080,
            },
            8,
        )
    }

    #[test]
    fn a_run_that_saw_nothing_fails_rather_than_passing_quietly() {
        assert_eq!(verdict(&observed()), EXIT_NO_INPUT);
    }

    #[test]
    fn one_event_on_either_path_is_enough_to_have_observed_something() {
        let mut only_raw = observed();
        only_raw.raw.seen = 1;
        assert_eq!(verdict(&only_raw), 0);

        let mut only_messages = observed();
        only_messages.messages.seen = 1;
        assert_eq!(verdict(&only_messages), 0);
    }
}
