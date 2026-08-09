//! What the window and the display are doing, and whether that changed while
//! a measurement was running.
//!
//! A presentation measurement is only worth the environment it was taken in.
//! macOS suspends a covered window's display link, throttles a background
//! process under App Nap, and keeps advertising the panel's nominal rate while
//! delivering a fraction of the callbacks. None of that shows up as an error;
//! it shows up as a plausible-looking number that is wrong. So the renderer
//! states the environment it started in, refuses to start in a bad one when
//! asked, and counts every transition that would invalidate the run.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use objc2::rc::Retained;
use objc2_app_kit::{NSScreen, NSWindow, NSWindowOcclusionState};
use objc2_foundation::NSString;

/// Counters a supervising thread can read while the renderer runs.
///
/// Every field is written by the render loop or the [`Watcher`] on the main
/// thread and read by whoever holds the `Arc`, so `Relaxed` is enough: nothing
/// downstream orders other memory against these.
#[derive(Debug, Default)]
pub struct LiveCounters {
    /// Display-link callbacks, or draw attempts in [`Immediate`] mode.
    ///
    /// [`Immediate`]: crate::DriveMode::Immediate
    pub callbacks: AtomicU64,
    pub rendered: AtomicU64,
    pub empty_ticks: AtomicU64,
    /// Ticks that took a frame and then found no drawable free. The third
    /// outcome of a tick, alongside a render and an empty one, so a
    /// supervisor differencing these across a span can close the books:
    /// `callbacks == rendered + empty_ticks + missed_drawables`.
    pub missed_drawables: AtomicU64,
    pub occlusion_changes: AtomicU64,
    pub space_changes: AtomicU64,
    pub miniaturise_events: AtomicU64,
    pub display_changes: AtomicU64,
    /// Callback intervals longer than twice the expected refresh period.
    pub link_pauses: AtomicU64,
}

impl LiveCounters {
    pub fn new() -> Arc<LiveCounters> {
        Arc::new(LiveCounters::default())
    }
}

#[inline]
pub(crate) fn bump(counter: &AtomicU64) {
    counter.fetch_add(1, Ordering::Relaxed);
}

#[inline]
pub(crate) fn read(counter: &AtomicU64) -> u64 {
    counter.load(Ordering::Relaxed)
}

/// The window and display as they were when the run started.
#[derive(Clone, Debug)]
pub struct Environment {
    pub display_name: String,
    /// The link's own figure once it is running. Before that it is what the
    /// link was asked for, which is the display's nominal rate.
    pub display_hz: f64,
    /// `NSScreen.maximumFramesPerSecond` for the screen the window is on.
    pub maximum_frames_per_second: f64,
    pub on_active_space: bool,
    pub occluded: bool,
    pub miniaturised: bool,
    pub drawable: (u32, u32),
}

/// One preflight item: the short name an orchestrator greps for, whether it
/// holds, and a line describing what was observed.
///
/// The detail describes the observation either way, so the same string reads
/// correctly behind `ok` and behind `FAIL`.
pub(crate) struct Check {
    pub(crate) name: &'static str,
    pub(crate) passed: bool,
    pub(crate) detail: String,
}

impl Environment {
    /// One readable line per reason this environment is unfit for a run that
    /// needs `required_hz`. An empty vector means fit.
    pub fn problems(&self, required_hz: f64) -> Vec<String> {
        self.checks(required_hz)
            .into_iter()
            .filter(|check| !check.passed)
            .map(|check| check.detail)
            .collect()
    }

    pub(crate) fn checks(&self, required_hz: f64) -> Vec<Check> {
        vec![
            self.display_check(),
            self.refresh_check(required_hz),
            Check {
                name: "occlusion",
                passed: !self.occluded,
                detail: if self.occluded {
                    "window is occluded, so macOS will suspend its display link".into()
                } else {
                    "window is unoccluded".into()
                },
            },
            Check {
                name: "space",
                passed: self.on_active_space,
                detail: if self.on_active_space {
                    "window is on the active Space".into()
                } else {
                    "window is not on the active Space".into()
                },
            },
            Check {
                name: "miniaturised",
                passed: !self.miniaturised,
                detail: if self.miniaturised {
                    "window is miniaturised".into()
                } else {
                    "window is not miniaturised".into()
                },
            },
        ]
    }

    /// A window AppKit places on no screen has no display to be measured
    /// against, and every rate below is then a guess.
    fn display_check(&self) -> Check {
        let (width, height) = self.drawable;
        Check {
            name: "display",
            passed: !self.display_name.is_empty(),
            detail: if self.display_name.is_empty() {
                "the window is on no display".into()
            } else {
                format!(
                    "window is on \"{}\", drawable {width}x{height}",
                    self.display_name
                )
            },
        }
    }

    fn refresh_check(&self, required_hz: f64) -> Check {
        // AppKit reports an integer rate and the caller asks in floating
        // point, so a display that answers 120 must satisfy a request for
        // 120.0 without depending on the two being bit-identical.
        let fast_enough = self.maximum_frames_per_second >= required_hz - 0.5;
        let detail = if fast_enough {
            format!(
                "display \"{}\" offers {:.0} Hz, the run needs {:.0} Hz",
                self.display_name, self.maximum_frames_per_second, required_hz
            )
        } else {
            format!(
                "display \"{}\" offers only {:.0} Hz, the run needs {:.0} Hz",
                self.display_name, self.maximum_frames_per_second, required_hz
            )
        };
        Check {
            name: "refresh",
            passed: fast_enough,
            detail,
        }
    }
}

/// What one poll of the window saw.
pub(crate) struct WindowState {
    pub(crate) occluded: bool,
    pub(crate) on_active_space: bool,
    pub(crate) miniaturised: bool,
    /// `None` when AppKit places the window on no screen at all, which is what
    /// it reports for a miniaturised or fully off-screen window.
    pub(crate) screen: Option<Retained<NSScreen>>,
}

impl WindowState {
    pub(crate) fn read(window: &NSWindow) -> WindowState {
        // `isVisible` is the wrong call here. Apple defines it as true for a
        // window that is on screen and unhidden, including when another window
        // covers it completely — which is exactly the case that makes macOS
        // suspend the display link. `occlusionState` is the only property that
        // distinguishes the two.
        let occluded = !window
            .occlusionState()
            .contains(NSWindowOcclusionState::Visible);
        WindowState {
            occluded,
            on_active_space: window.isOnActiveSpace(),
            miniaturised: window.isMiniaturized(),
            screen: window.screen(),
        }
    }
}

/// Counts the window transitions that invalidate a presentation measurement.
///
/// This polls once per pass of the run loop rather than registering
/// notification observers. The run loop already owns the main thread and runs
/// at least once per refresh, so a poll costs four AppKit property reads at
/// the rate the display is already ticking, and cannot miss a transition that
/// lasts longer than one refresh. An observer would buy nothing but a second
/// path into the same state, plus the registration and teardown to go with it.
pub(crate) struct Watcher {
    counters: Arc<LiveCounters>,
    occluded: bool,
    on_active_space: bool,
    miniaturised: bool,
    /// Held as the framework's own string rather than a `String`, so comparing
    /// it once per refresh for ten minutes costs no allocation at all.
    display_name: Retained<NSString>,
}

impl Watcher {
    pub(crate) fn new(
        counters: Arc<LiveCounters>,
        window: &NSWindow,
        start: &Environment,
    ) -> Watcher {
        let display_name = window.screen().map_or_else(
            || NSString::from_str(&start.display_name),
            |screen| screen.localizedName(),
        );
        Watcher {
            counters,
            occluded: start.occluded,
            on_active_space: start.on_active_space,
            miniaturised: start.miniaturised,
            display_name,
        }
    }

    /// Samples the window and counts each state that differs from the last
    /// sample. Counting on the transition rather than on the sample is what
    /// makes the numbers mean "how often did this change", not "for how many
    /// refreshes was it true".
    pub(crate) fn sample(&mut self, window: &NSWindow) {
        let state = WindowState::read(window);

        if state.occluded != self.occluded {
            self.occluded = state.occluded;
            bump(&self.counters.occlusion_changes);
        }
        if state.on_active_space != self.on_active_space {
            self.on_active_space = state.on_active_space;
            bump(&self.counters.space_changes);
        }
        if state.miniaturised != self.miniaturised {
            self.miniaturised = state.miniaturised;
            bump(&self.counters.miniaturise_events);
        }
        // A window that is on no screen has not moved to a different display,
        // it has left the desktop; that is already counted as miniaturisation
        // or occlusion, and treating it as a display change would double-count
        // one event as two.
        if let Some(screen) = state.screen {
            let name = screen.localizedName();
            if name != self.display_name {
                self.display_name = name;
                bump(&self.counters.display_changes);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Environment;

    fn clean() -> Environment {
        Environment {
            display_name: "Built-in Retina Display".into(),
            display_hz: 119.98,
            maximum_frames_per_second: 120.0,
            on_active_space: true,
            occluded: false,
            miniaturised: false,
            drawable: (3456, 1944),
        }
    }

    #[test]
    fn a_clean_environment_has_no_problems() {
        assert!(clean().problems(120.0).is_empty());
    }

    #[test]
    fn occlusion_is_a_problem() {
        let environment = Environment {
            occluded: true,
            ..clean()
        };
        let problems = environment.problems(120.0);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("occluded"), "{problems:?}");
    }

    #[test]
    fn leaving_the_active_space_is_a_problem() {
        let environment = Environment {
            on_active_space: false,
            ..clean()
        };
        let problems = environment.problems(120.0);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("active Space"), "{problems:?}");
    }

    #[test]
    fn miniaturisation_is_a_problem() {
        let environment = Environment {
            miniaturised: true,
            ..clean()
        };
        let problems = environment.problems(120.0);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("miniaturised"), "{problems:?}");
    }

    #[test]
    fn a_display_below_the_required_rate_is_a_problem() {
        let environment = Environment {
            display_name: "Studio Display".into(),
            maximum_frames_per_second: 60.0,
            ..clean()
        };
        let problems = environment.problems(120.0);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("Studio Display"), "{problems:?}");
        assert!(problems[0].contains("60 Hz"), "{problems:?}");
        assert!(problems[0].contains("120 Hz"), "{problems:?}");
    }

    #[test]
    fn a_display_at_exactly_the_required_rate_is_fit() {
        let environment = Environment {
            maximum_frames_per_second: 120.0,
            ..clean()
        };
        assert!(environment.problems(120.0).is_empty());
    }

    #[test]
    fn a_window_on_no_display_is_a_problem() {
        let environment = Environment {
            display_name: String::new(),
            ..clean()
        };
        let problems = environment.problems(120.0);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("no display"), "{problems:?}");
    }

    #[test]
    fn every_reason_is_reported_at_once() {
        let environment = Environment {
            display_name: String::new(),
            occluded: true,
            on_active_space: false,
            miniaturised: true,
            maximum_frames_per_second: 60.0,
            ..clean()
        };
        assert_eq!(environment.problems(120.0).len(), 5);
    }

    /// The orchestrator matches on these names, so they are part of the
    /// interface and not free to be reworded.
    #[test]
    fn every_item_is_named_and_reported_in_order() {
        let names: Vec<&str> = clean()
            .checks(120.0)
            .into_iter()
            .map(|check| check.name)
            .collect();
        assert_eq!(
            names,
            ["display", "refresh", "occlusion", "space", "miniaturised"]
        );
    }

    #[test]
    fn a_faster_display_satisfies_a_slower_requirement() {
        assert!(clean().problems(60.0).is_empty());
    }
}
