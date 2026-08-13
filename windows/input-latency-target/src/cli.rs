//! The command line, kept apart from the window so that what the operator
//! typed can be checked without a display attached.

use std::path::PathBuf;

use clap::Parser;

/// The monitor this target always paints on.
///
/// A name fragment rather than an index, resolved by
/// `lanplay_capture::output_named`. Attaching a monitor renumbers every DXGI
/// output after it, and a run that silently moved to the wrong screen has
/// already cost this lab a day.
pub const DISPLAY: &str = "IDD-LAB";

#[derive(Parser, Debug, PartialEq)]
#[command(
    name = "input-latency-target",
    about = "a window that flashes on input, and times how long that took"
)]
pub struct Cli {
    /// Where the report goes. Standard output when absent, which is only
    /// useful from a console: under the scheduled task that runs this in the
    /// interactive session there is nowhere for stdout to go.
    #[arg(long)]
    pub out: Option<PathBuf>,
    /// How long to run before reporting.
    #[arg(long, default_value_t = 30.0)]
    pub seconds: f64,
    /// How many presents white stays up for.
    ///
    /// One is enough for the measurement, which ends when the first white
    /// present returns. More than one exists for the capture side: a
    /// free-running present loop can put white up and take it away again
    /// inside a single frame of a 120 Hz duplication, and a transition nobody
    /// downstream can see is a transition that did not happen.
    #[arg(long, default_value_t = 8, value_parser = clap::value_parser!(u32).range(1..))]
    pub flash_presents: u32,
}

impl Cli {
    pub fn from_args() -> Cli {
        Cli::parse()
    }
}

#[cfg(test)]
mod tests {
    use super::Cli;
    use clap::Parser;

    #[test]
    fn the_defaults_are_the_ones_the_help_promises() {
        let cli = Cli::try_parse_from(["input-latency-target"]).expect("no arguments is valid");
        assert_eq!(cli.out, None);
        assert_eq!(cli.seconds, 30.0);
        assert_eq!(cli.flash_presents, 8);
    }

    #[test]
    fn every_argument_is_accepted_in_its_long_form() {
        let cli = Cli::try_parse_from([
            "input-latency-target",
            "--out",
            "C:\\lab\\target.txt",
            "--seconds",
            "5.5",
            "--flash-presents",
            "3",
        ])
        .expect("all three arguments are valid");
        assert_eq!(
            cli.out.as_deref(),
            Some(std::path::Path::new("C:\\lab\\target.txt"))
        );
        assert_eq!(cli.seconds, 5.5);
        assert_eq!(cli.flash_presents, 3);
    }

    #[test]
    fn a_flash_of_no_presents_is_refused_rather_than_accepted_and_ignored() {
        // Zero would mean white is armed and never shown, so every event
        // would be timed against a present that displayed nothing. Being told
        // is better than being quietly corrected.
        assert!(Cli::try_parse_from(["input-latency-target", "--flash-presents", "0"]).is_err());
    }
}
