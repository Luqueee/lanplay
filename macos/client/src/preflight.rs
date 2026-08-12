//! Refusing to start a ten-minute run that cannot produce a trustworthy
//! number.
//!
//! The failures this guards against are all silent: a display that is not
//! running at the rate we are measuring, a window the compositor has stopped
//! drawing, a process the system is free to throttle. Each of them yields a
//! run that finishes cleanly and reports numbers about something other than
//! the pipeline. Eight good minutes and two contaminated ones is not a
//! measurement, so the check happens in the first second or not at all.

use core::fmt;

/// How much a check's finding matters.
///
/// A warning is not a soft failure: it is a fact about the environment that
/// will show up in the numbers and that the operator has to know, on a run
/// that is still worth doing. Measuring a DFS channel on purpose is exactly
/// that, and a check that refused it would make the experiment that found
/// the problem impossible to repeat.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Level {
    Ok,
    Warn,
    Fail,
}

#[derive(Clone)]
pub struct Item {
    pub name: &'static str,
    pub level: Level,
    pub detail: String,
}

/// Returned when the run refused to start. Carries no detail because the
/// block has already been printed in the form the orchestrator parses.
#[derive(Debug)]
pub struct Refused;

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("preflight refused the run")
    }
}

impl core::error::Error for Refused {}

impl Item {
    pub fn ok(name: &'static str, detail: impl Into<String>) -> Item {
        Item {
            name,
            level: Level::Ok,
            detail: detail.into(),
        }
    }

    pub fn warn(name: &'static str, detail: impl Into<String>) -> Item {
        Item {
            name,
            level: Level::Warn,
            detail: detail.into(),
        }
    }

    pub fn fail(name: &'static str, detail: impl Into<String>) -> Item {
        Item {
            name,
            level: Level::Fail,
            detail: detail.into(),
        }
    }

    /// Whether the run may proceed. Warnings may.
    pub fn passed(&self) -> bool {
        self.level != Level::Fail
    }
}

impl fmt::Display for Item {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.level {
            Level::Ok => write!(f, "preflight: ok {} — {}", self.name, self.detail),
            Level::Warn => write!(f, "preflight: WARN {} — {}", self.name, self.detail),
            Level::Fail => write!(f, "preflight: FAIL {} — {}", self.name, self.detail),
        }
    }
}

/// Prints the block and says whether the run may proceed.
///
/// The terminator lines are part of the contract with `xtask`: it waits for
/// one of them and must never be left waiting on a client that has already
/// given up.
pub fn report(items: &[Item]) -> bool {
    for item in items {
        println!("{item}");
    }
    let failed = items.iter().filter(|item| !item.passed()).count();
    if failed == 0 {
        println!("preflight: complete");
        true
    } else {
        println!("preflight: aborted ({failed} failed)");
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_terminator_says_which_way_it_went() {
        assert!(report(&[Item::ok("display", "Built-in, 120 Hz")]));
        // A warning is information, not a veto: the run that discovered the
        // DFS problem had to be able to run on a DFS channel.
        assert!(report(&[
            Item::ok("display", "Built-in, 120 Hz"),
            Item::warn("wifi", "channel 116 requires radar detection"),
        ]));
        assert!(
            Item::warn("wifi", "x").to_string().contains("WARN"),
            "a warning must be greppable as one"
        );
        assert!(!report(&[
            Item::ok("display", "Built-in, 120 Hz"),
            Item::fail("space", "window is on another Space"),
        ]));
    }

    #[test]
    fn an_item_renders_the_way_the_orchestrator_parses_it() {
        let ok = Item::ok("refresh", "120 Hz").to_string();
        assert!(ok.starts_with("preflight: ok refresh"));
        let bad = Item::fail("refresh", "panel is 60 Hz").to_string();
        assert!(bad.starts_with("preflight: FAIL refresh"));
        assert!(
            bad.contains(" — "),
            "the orchestrator splits on the em dash"
        );
    }
}
