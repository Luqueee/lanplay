//! Opting the process out of App Nap.
//!
//! macOS throttles a process whose window is occluded, and the throttling is
//! not limited to drawing: timers slow down and threads are descheduled, so a
//! receive loop stops draining its socket on time. Measured on this project,
//! a client whose window sat behind a terminal reported one-second gaps in
//! packet arrival, while a receiver with no window at all, taking the same
//! stream over the same Wi-Fi at the same moment, saw a worst gap of 37 ms.
//! Every one of those seconds was the operating system doing exactly what it
//! is designed to do to a background application.
//!
//! A remote-desktop client is not a background application. `LatencyCritical`
//! is the activity class Apple documents for media playback that must not be
//! interrupted, which is precisely this case.

use objc2::rc::Retained;
use objc2::runtime::NSObjectProtocol;
use objc2_foundation::{NSActivityOptions, NSProcessInfo, NSString};

/// Holds the activity for as long as it is alive. Dropping it lets the system
/// throttle the process again, so it must outlive the run.
pub struct Awake(Retained<objc2::runtime::ProtocolObject<dyn NSObjectProtocol>>);

impl Awake {
    pub fn begin(reason: &str) -> Awake {
        let options = NSActivityOptions::UserInitiated | NSActivityOptions::LatencyCritical;
        let token = NSProcessInfo::processInfo()
            .beginActivityWithOptions_reason(options, &NSString::from_str(reason));
        Awake(token)
    }
}

impl Drop for Awake {
    fn drop(&mut self) {
        // SAFETY: the token came from `beginActivityWithOptions:reason:` on
        // this same process info object, which is what `endActivity:` expects,
        // and it is ended exactly once because `Awake` owns it.
        unsafe { NSProcessInfo::processInfo().endActivity(&self.0) };
    }
}
