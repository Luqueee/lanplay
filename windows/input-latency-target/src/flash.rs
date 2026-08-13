//! What colour the window is painting, and when it goes back to black.
//!
//! The whole screen goes from black to white and back, rather than a small
//! square somewhere in it, because of what happens downstream. The change has
//! to be found again in a frame that has been through NVENC and across a
//! Wi-Fi link, and an encoder given a few changed pixels in a still image will
//! spend almost no bits on them: the block gets smoothed, the transition
//! arrives blurred by a frame or two, and the instant the change appeared
//! stops being recoverable. A full-frame transition survives every encoder
//! setting anybody would use, and it can be detected by averaging the frame
//! rather than by knowing where to look.
//!
//! Rejected: toggling on every present, which would give the capture side a
//! square wave with no relationship to any input, and drawing a counter or a
//! pattern, which would need a font, a shader and a reason.

/// What the window paints for one present.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Colour {
    /// At rest.
    Black,
    /// Reacting to input.
    White,
}

impl Colour {
    /// The clear colour, in the linear RGBA the swap chain wants.
    pub const fn rgba(self) -> [f32; 4] {
        match self {
            Colour::Black => [0.0, 0.0, 0.0, 1.0],
            Colour::White => [1.0, 1.0, 1.0, 1.0],
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Colour::Black => "black",
            Colour::White => "white",
        }
    }
}

/// Counts presents so that white stays up for a fixed number of them.
///
/// Presents rather than milliseconds: this loop does not wait for vertical
/// blank and its rate is whatever the machine manages, so a duration would be
/// a different number of frames on every run and on every display. The
/// capture side counts frames, so the target counts presents.
#[derive(Clone, Copy, Debug)]
pub struct Flash {
    hold: u32,
    /// Presents of white already completed. Only meaningful while `armed`.
    shown: u32,
    armed: bool,
}

impl Flash {
    /// `hold` is the number of presents white stays up for, clamped to at
    /// least one so that an armed flash is always shown at least once.
    pub const fn new(hold: u32) -> Flash {
        Flash {
            hold: if hold == 0 { 1 } else { hold },
            shown: 0,
            armed: false,
        }
    }

    pub const fn hold(&self) -> u32 {
        self.hold
    }

    /// Whether the window is black and the next input is therefore timeable.
    pub const fn at_rest(&self) -> bool {
        !self.armed
    }

    pub const fn colour(&self) -> Colour {
        if self.armed {
            Colour::White
        } else {
            Colour::Black
        }
    }

    /// Reacts to an input event, answering whether this event is the one that
    /// caused the transition.
    ///
    /// Only an event arriving at rest gets a true, because only that event has
    /// a present that first showed a changed colour to be measured against.
    /// An event arriving while white is already up changes nothing on the
    /// display, so timing it would be timing the loop rate rather than the
    /// reaction.
    pub const fn arm(&mut self) -> bool {
        if self.armed {
            return false;
        }
        self.armed = true;
        self.shown = 0;
        true
    }

    /// One present of the current colour has completed, answering whether that
    /// present was the first to carry white.
    pub const fn presented(&mut self) -> bool {
        if !self.armed {
            return false;
        }
        self.shown += 1;
        let first = self.shown == 1;
        if self.shown >= self.hold {
            self.armed = false;
            self.shown = 0;
        }
        first
    }
}

#[cfg(test)]
mod tests {
    use super::{Colour, Flash};

    #[test]
    fn a_flash_at_rest_is_black_and_has_nothing_to_report() {
        let mut flash = Flash::new(4);
        assert!(flash.at_rest());
        assert_eq!(flash.colour(), Colour::Black);
        assert!(!flash.presented());
        assert_eq!(flash.colour(), Colour::Black);
    }

    #[test]
    fn the_first_present_after_arming_is_the_one_that_shows_white() {
        let mut flash = Flash::new(4);
        assert!(flash.arm());
        assert_eq!(flash.colour(), Colour::White);
        assert!(flash.presented());
        assert!(!flash.presented());
        assert!(!flash.presented());
    }

    #[test]
    fn white_lasts_exactly_the_requested_number_of_presents() {
        let mut flash = Flash::new(3);
        flash.arm();
        for _ in 0..3 {
            assert_eq!(flash.colour(), Colour::White);
            flash.presented();
        }
        assert_eq!(flash.colour(), Colour::Black);
        assert!(flash.at_rest());
    }

    #[test]
    fn input_arriving_while_white_is_up_does_not_start_a_second_flash() {
        // It also must not extend the first one: a run whose injector goes
        // faster than the hold would otherwise never see black again, and the
        // capture side would find one transition in a whole run.
        let mut flash = Flash::new(3);
        assert!(flash.arm());
        flash.presented();
        assert!(!flash.arm());
        flash.presented();
        assert!(!flash.arm());
        flash.presented();
        assert!(flash.at_rest());
    }

    #[test]
    fn a_flash_can_be_armed_again_once_it_has_returned_to_black() {
        let mut flash = Flash::new(2);
        flash.arm();
        assert!(flash.presented());
        assert!(!flash.presented());
        assert!(flash.at_rest());
        assert!(flash.arm());
        assert!(flash.presented());
    }

    #[test]
    fn a_hold_of_zero_still_shows_white_once() {
        // The command line refuses zero, but the state machine is what
        // guarantees an armed flash is visible, and a guarantee that depends
        // on an argument parser is not one.
        let mut flash = Flash::new(0);
        assert_eq!(flash.hold(), 1);
        flash.arm();
        assert_eq!(flash.colour(), Colour::White);
        assert!(flash.presented());
        assert!(flash.at_rest());
    }
}
