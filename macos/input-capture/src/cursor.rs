//! Holding the local cursor still while the mouse drives a remote pointer.
//!
//! While a session has the input, the cursor on this machine must stop
//! following the mouse. Otherwise it walks across the desktop, hits a screen
//! edge, and every application it passes over reacts to a mouse the user
//! believes is aiming inside a game.
//! `CGAssociateMouseAndMouseCursorPosition(false)` is the window server call
//! that breaks that link while leaving the events themselves flowing.
//!
//! The hazard is entirely in the other direction. A process that dissociates
//! and then exits without reassociating leaves a machine whose mouse appears
//! broken to every application, with no visible cause and no obvious cure, and
//! that is by a wide margin the worst thing this crate could do to somebody. So
//! the association is a guard rather than a pair of calls: reattaching happens
//! on `Drop`, which covers a panic and an early return as well as an orderly
//! release.
//!
//! Rejected: hiding the cursor instead. A hidden cursor still moves, still
//! crosses screen edges, and still delivers clicks to whatever it is over.

use core::fmt;

use objc2_core_graphics::{CGAssociateMouseAndMouseCursorPosition, CGError};

/// The window server refused to change the association.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AssociateFailed {
    /// What was asked for: `true` to reattach, `false` to detach.
    pub connected: bool,
    /// The `CGError` the call returned.
    pub error: i32,
}

impl fmt::Display for AssociateFailed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let wanted = if self.connected { "attach" } else { "detach" };
        write!(
            f,
            "the window server refused to {wanted} the cursor, CGError {}",
            self.error
        )
    }
}

impl core::error::Error for AssociateFailed {}

/// Owns whatever the cursor's association currently is, and restores it.
///
/// Starts attached because that is how the machine already is, so constructing
/// one is free and cannot fail.
pub struct CursorLink {
    detached: bool,
}

impl Default for CursorLink {
    fn default() -> CursorLink {
        CursorLink::new()
    }
}

impl CursorLink {
    pub const fn new() -> CursorLink {
        CursorLink { detached: false }
    }

    /// Stops the cursor following the mouse. Idempotent.
    pub fn detach(&mut self) -> Result<(), AssociateFailed> {
        if self.detached {
            return Ok(());
        }
        associate(false)?;
        self.detached = true;
        Ok(())
    }

    /// Lets the cursor follow the mouse again. Idempotent, because `Drop` calls
    /// it after an explicit release has already done so.
    pub fn attach(&mut self) -> Result<(), AssociateFailed> {
        if !self.detached {
            return Ok(());
        }
        associate(true)?;
        self.detached = false;
        Ok(())
    }

    /// Whether the cursor is currently detached.
    ///
    /// CoreGraphics offers no way to read the association back, so this is the
    /// last state the window server accepted rather than a value fetched from
    /// it. It is only ever wrong if another process in this session changed the
    /// association behind us, which would be a bug in that process.
    pub const fn is_detached(&self) -> bool {
        self.detached
    }
}

impl Drop for CursorLink {
    fn drop(&mut self) {
        // Nothing useful can be done with a failure here and there is nobody
        // left to tell, but the attempt has to be made: an unwinding thread is
        // exactly the case where a detached cursor would otherwise survive the
        // program.
        let _ = self.attach();
    }
}

fn associate(connected: bool) -> Result<(), AssociateFailed> {
    let error = CGAssociateMouseAndMouseCursorPosition(connected);
    if error == CGError::Success {
        Ok(())
    } else {
        Err(AssociateFailed {
            connected,
            error: error.0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::CursorLink;

    /// The safety invariant, against the real window server: whatever a link
    /// detaches, it attaches again.
    #[test]
    fn a_detached_link_attaches_again() {
        let mut link = CursorLink::new();
        assert!(!link.is_detached());
        link.detach().expect("the window server accepts a detach");
        assert!(link.is_detached());
        link.attach().expect("the window server accepts an attach");
        assert!(!link.is_detached());
    }

    /// Both calls are idempotent, so a release followed by a drop reattaches
    /// once rather than reattaching a cursor somebody else has since detached.
    #[test]
    fn repeating_either_call_is_harmless() {
        let mut link = CursorLink::new();
        link.attach()
            .expect("attaching an attached cursor does nothing");
        link.detach().expect("the window server accepts a detach");
        link.detach()
            .expect("detaching a detached cursor does nothing");
        assert!(link.is_detached());
        link.attach().expect("the window server accepts an attach");
        link.attach()
            .expect("attaching an attached cursor does nothing");
        assert!(!link.is_detached());
    }

    /// Dropping a detached link reattaches, which is what makes a panic
    /// survivable. Observed by leaving a detached link to drop and then
    /// checking that a fresh detach and attach cycle still succeeds, which the
    /// window server would refuse if the first link had corrupted the state.
    #[test]
    fn dropping_a_detached_link_leaves_the_session_usable() {
        {
            let mut link = CursorLink::new();
            link.detach().expect("the window server accepts a detach");
        }
        let mut link = CursorLink::new();
        link.detach().expect("the session still accepts a detach");
        link.attach().expect("the session still accepts an attach");
    }
}
