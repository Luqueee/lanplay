//! Taking the mouse and the keyboard for a session, and giving them back.
//!
//! A player starts with their own machine. A click asks for the game, and from
//! then until something takes it away every event belongs to the host. Four
//! things take it away: control moving to another application, a local
//! combination the player types to get out, command-tab, and the session
//! failing. They are gathered here because they must do the same things in the
//! same order. A handler per cause is where the races live, since each one
//! decides for itself what to undo and the one that forgets to close admission
//! first lets a press made during its own exit onto the wire behind the release
//! that was meant to end it.
//!
//! So there is one exit and its order is fixed. Admission closes, the machine
//! enters [`State::Releasing`], and only then does the caller mint the
//! `ReleaseAll`. The host reads that id as a barrier and refuses every reliable
//! event below it however late it arrives, which is a true statement about the
//! whole capture only if nothing more could be admitted once the barrier was
//! drawn. The other order leaves a window in which a press picks up an id above
//! the release, is therefore legitimately post-barrier, and is applied to a host
//! whose player has already walked away.
//!
//! Admission is a property of this type rather than of its caller for the same
//! reason. Offering an event while the machine is anything but
//! [`State::Capturing`] refuses it and counts it, and that counter is what turns
//! "nothing reaches the host while uncaptured" from an assumption into a figure
//! a run prints.
//!
//! The release combination is shaped by the same argument. A recognizer that
//! decided after its keys had been sent would be deciding too late: the ids are
//! minted, the host is already waiting out the gaps they left, and no datagram
//! can be recalled. So a press that could still extend the combination is held
//! here instead of sent, and it is either suppressed along with the rest of the
//! combination or, when the combination turns out not to match, let through in
//! the order it was made.
//!
//! Entering is two steps because taking the cursor can be refused, and the one
//! state that must be unreachable is the half-captured one: a player who can
//! neither see their cursor nor reach the game cannot tell a bug from a broken
//! machine. [`State::Entering`] is where that refusal is survived. It admits
//! nothing, so an entry that fails leaves exactly the machine the player
//! started with.
//!
//! Nothing here talks to AppKit, opens a socket, reads a clock or touches the
//! window server. The caller owns all four and is told what to do with them,
//! which is what lets a whole capture and every one of its exits be driven by
//! calling methods.

use lanplay_input_protocol::Button;

use crate::focus::FocusState;
use crate::scancode::ScanCode;

/// The button that asks for capture.
///
/// The left button and not any button, because a stray middle-click on a
/// desktop must not silently take the mouse away from the person who made it,
/// and the left button is the one a player presses to start playing.
pub const CAPTURE_BUTTON: Button = Button::Left;

/// Left command, which the host receives as its left Windows key.
pub const LEFT_COMMAND: ScanCode = ScanCode {
    code: 0x5B,
    extended: true,
};

/// Right command, the host's right Windows key.
pub const RIGHT_COMMAND: ScanCode = ScanCode {
    code: 0x5C,
    extended: true,
};

pub const LEFT_CONTROL: ScanCode = ScanCode {
    code: 0x1D,
    extended: false,
};

pub const RIGHT_CONTROL: ScanCode = ScanCode {
    code: 0x1D,
    extended: true,
};

/// Left option, which the host receives as its left alt.
pub const LEFT_OPTION: ScanCode = ScanCode {
    code: 0x38,
    extended: false,
};

/// Right option, the host's right alt.
pub const RIGHT_OPTION: ScanCode = ScanCode {
    code: 0x38,
    extended: true,
};

/// Tab, which is command-tab's second half and nothing else here.
pub const TAB: ScanCode = ScanCode {
    code: 0x0F,
    extended: false,
};

/// How many presses the recognizer can be holding at once: every member of the
/// combination but the last, since the last one completes it and is never held.
const HELD_CAPACITY: usize = 2;

/// A placeholder the held array is filled with. Never read, because
/// [`Machine::held_len`] bounds every read of it.
const NO_KEY: ScanCode = ScanCode {
    code: 0,
    extended: false,
};

/// Where the machine is.
///
/// The two transient states are not decoration. Both are windows in which the
/// caller is part-way through a window server call, and both refuse input,
/// which is what makes half-captured and half-released unreachable rather than
/// merely unlikely.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    /// The mouse and the keyboard belong to this machine. Nothing is sent.
    Uncaptured,
    /// The capture click has been seen and the caller has not yet said whether
    /// the cursor could be taken.
    Entering,
    /// Input is being sent to the host.
    Capturing,
    /// The exit is under way: admission is already closed, and the release the
    /// caller was handed has not yet been reported applied.
    Releasing,
}

impl State {
    pub const fn name(self) -> &'static str {
        match self {
            State::Uncaptured => "uncaptured",
            State::Entering => "entering",
            State::Capturing => "capturing",
            State::Releasing => "releasing",
        }
    }
}

/// What took the input away.
///
/// Counted apart rather than summed, because the claim being made is that all
/// four converge the host to nothing held, and a run that only ever lost focus
/// has not exercised the same path as one that was typed out of.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExitCause {
    /// Another application became the one the input is for.
    FocusLost,
    /// The player typed the combination that means give me my machine back.
    ReleaseHotkey,
    /// The player asked macOS for the application switcher.
    CommandTab,
    /// The session stopped working, which is as much a loss of control as any
    /// of the above and owes the same release.
    SessionFailure,
}

impl ExitCause {
    pub const ALL: [ExitCause; 4] = [
        ExitCause::FocusLost,
        ExitCause::ReleaseHotkey,
        ExitCause::CommandTab,
        ExitCause::SessionFailure,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            ExitCause::FocusLost => "focus lost",
            ExitCause::ReleaseHotkey => "release hotkey",
            ExitCause::CommandTab => "command-tab",
            ExitCause::SessionFailure => "session failure",
        }
    }

    pub const fn index(self) -> usize {
        match self {
            ExitCause::FocusLost => 0,
            ExitCause::ReleaseHotkey => 1,
            ExitCause::CommandTab => 2,
            ExitCause::SessionFailure => 3,
        }
    }
}

/// What the caller has to do beyond sending whatever an [`Outcome`] admitted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    /// Nothing.
    None,
    /// Take the cursor away from the mouse, then say which way it went with
    /// [`Machine::entered`] or [`Machine::entry_failed`].
    Enter,
    /// The one exit path, in this order and no other: mint and send a
    /// `ReleaseAll`, give the cursor back to the mouse, then report it with
    /// [`Machine::released`].
    Exit(ExitCause),
}

/// What one offered event turned into.
///
/// Three answers in one value because they are one decision. A near miss on the
/// combination both releases the presses that were being held and admits the
/// event that ended the hold, and splitting that across two calls would invite
/// a caller to send them in the wrong order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Outcome {
    held: [ScanCode; HELD_CAPACITY],
    held_len: u8,
    admit: bool,
    action: Action,
}

impl Outcome {
    /// The presses the recognizer had been holding back, in the order the
    /// player made them, all of which must be sent before the event that ended
    /// the hold.
    ///
    /// Every one is a press: only a press can extend the combination, so a
    /// release is never held and never appears here.
    pub fn flushed(&self) -> &[ScanCode] {
        &self.held[..self.held_len as usize]
    }

    /// Whether the event just offered is input the host must be told about.
    pub const fn admitted(&self) -> bool {
        self.admit
    }

    pub const fn action(&self) -> Action {
        self.action
    }

    const fn nothing() -> Outcome {
        Outcome {
            held: [NO_KEY; HELD_CAPACITY],
            held_len: 0,
            admit: false,
            action: Action::None,
        }
    }

    const fn only(action: Action) -> Outcome {
        Outcome {
            held: [NO_KEY; HELD_CAPACITY],
            held_len: 0,
            admit: false,
            action,
        }
    }
}

/// Everything a run reports about the capture path.
///
/// The suppression figures are here rather than left to a caller because they
/// are the evidence for two claims this machine makes and cannot otherwise
/// prove: that the click which asked for capture did not also fire a weapon,
/// and that nothing was sent while the mouse belonged to this machine.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Counts {
    /// Captures that reached [`State::Capturing`].
    pub captures: u64,
    /// Clicks that asked for capture and were therefore not input. One per
    /// capture: the button-up that closes such a click belongs to the same
    /// click and is swallowed with it rather than counted again.
    pub capture_clicks_suppressed: u64,
    /// Key events the recognizer kept off the wire because they were part of a
    /// combination meant for this machine.
    pub hotkey_events_suppressed: u64,
    /// Indexed by [`ExitCause::index`].
    pub exits: [u64; ExitCause::ALL.len()],
    /// Releases the machine asked the caller for, which is one per exit.
    pub releases: u64,
    /// Events offered while the machine was not capturing, and therefore never
    /// sent. Includes the presses a hold was still sitting on when an exit
    /// closed admission underneath it.
    pub refused: u64,
    /// Entries that could not take the cursor and left the machine exactly as
    /// it was.
    pub entries_failed: u64,
}

/// Which member of the release combination a key is.
///
/// Named for what the player presses rather than for what the host receives,
/// since the combination is something a person has to be told about; the alt
/// key on the other machine is this one's option.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Member {
    Command,
    Control,
    Alt,
}

impl Member {
    const fn bit(self) -> u8 {
        1 << self as u8
    }
}

/// All three members held, which is the combination.
const COMPLETE: u8 = Member::Command.bit() | Member::Control.bit() | Member::Alt.bit();

/// Which member of the combination a key is, if any. Either side of a pair
/// counts, because the combination is about the key a hand reaches for and not
/// about which half of the keyboard it reached across.
const fn member_of(scan: ScanCode) -> Option<Member> {
    Some(match (scan.code, scan.extended) {
        (0x5B, true) | (0x5C, true) => Member::Command,
        (0x1D, _) => Member::Control,
        (0x38, _) => Member::Alt,
        _ => return None,
    })
}

/// The capture and focus state machine: one entry, one exit, and the admission
/// gate every input event passes through.
pub struct Machine {
    state: State,
    /// Whether this process is the one the input is for. Kept here rather than
    /// read from a notification centre so that the exit a focus loss causes can
    /// be driven without one, and so that the two notifications AppKit posts
    /// for one switch away collapse into the one loss they are.
    focus: FocusState,
    held: [ScanCode; HELD_CAPACITY],
    held_len: u8,
    /// Bit per [`Member`] currently held back, so a second press of a member
    /// already in the hold is recognised as a near miss rather than folded in.
    members: u8,
    /// Whether the button-up that closes the capture click is still owed. The
    /// down was the interface, so the up is not input either, and sending it
    /// would hand the host a release for a press it never received.
    owed_click_up: bool,
    counts: Counts,
}

impl Default for Machine {
    fn default() -> Machine {
        Machine::new()
    }
}

impl Machine {
    pub const fn new() -> Machine {
        Machine {
            state: State::Uncaptured,
            focus: FocusState::new(),
            held: [NO_KEY; HELD_CAPACITY],
            held_len: 0,
            members: 0,
            owed_click_up: false,
            counts: Counts {
                captures: 0,
                capture_clicks_suppressed: 0,
                hotkey_events_suppressed: 0,
                exits: [0; ExitCause::ALL.len()],
                releases: 0,
                refused: 0,
                entries_failed: 0,
            },
        }
    }

    pub const fn state(&self) -> State {
        self.state
    }

    pub const fn is_capturing(&self) -> bool {
        matches!(self.state, State::Capturing)
    }

    pub const fn counts(&self) -> Counts {
        self.counts
    }

    pub const fn focus(&self) -> FocusState {
        self.focus
    }

    /// Whether an event that is neither a key nor a button may be sent, which
    /// is motion and the wheel.
    ///
    /// Neither can extend the release combination and neither can ask for
    /// capture, so both go straight through the gate. They also leave a hold
    /// alone rather than flushing it: motion is continuous, so flushing on it
    /// would mean the combination could only ever be typed by a player holding
    /// the mouse perfectly still, and a press is not aimed the way a click is,
    /// so nothing depends on its order against a movement.
    pub fn admit(&mut self) -> bool {
        if !matches!(self.state, State::Capturing) {
            self.counts.refused += 1;
            return false;
        }
        true
    }

    /// Offers one mouse button transition.
    pub fn button(&mut self, button: Button, down: bool) -> Outcome {
        // Asked before the state, because the up belongs to the click that
        // asked for capture whatever has happened since, and an exit in between
        // does not turn it into input.
        if !down && button == CAPTURE_BUTTON && self.owed_click_up {
            self.owed_click_up = false;
            return Outcome::nothing();
        }

        match self.state {
            State::Uncaptured if down && button == CAPTURE_BUTTON => {
                self.counts.capture_clicks_suppressed += 1;
                self.owed_click_up = true;
                self.state = State::Entering;
                Outcome::only(Action::Enter)
            }
            State::Capturing => {
                // A click is aimed, so it must not overtake a press the
                // recognizer is sitting on: the host would then see the
                // modifier arrive after the click it was meant to modify.
                let mut outcome = self.flush();
                outcome.admit = true;
                outcome
            }
            _ => {
                self.counts.refused += 1;
                Outcome::nothing()
            }
        }
    }

    /// Offers one key transition.
    ///
    /// The combination is command, control and option held together, with
    /// command first. Command is the one modifier that belongs to macOS and to
    /// every Mac application rather than to a game, so no player has a reason
    /// to be holding it mid-session, and requiring two more keys beside it
    /// means a stray command press cannot end a capture on its own. Requiring
    /// command *first* is what keeps control, option and shift — the modifiers
    /// a game actually binds — out of the recognizer entirely whenever the
    /// player has not already reached for command, so a crouch is never held
    /// back by a combination it was not part of.
    pub fn key(&mut self, scan: ScanCode, down: bool) -> Outcome {
        if !matches!(self.state, State::Capturing) {
            self.counts.refused += 1;
            return Outcome::nothing();
        }

        // Command-tab is decided here rather than left to the focus loss that
        // follows it, because the notification arrives after the tab has been
        // offered and by then the press would already carry an id.
        if down && scan == TAB && self.held_len > 0 {
            self.counts.hotkey_events_suppressed += u64::from(self.held_len) + 1;
            self.clear_holds();
            self.exit(ExitCause::CommandTab);
            return Outcome::only(Action::Exit(ExitCause::CommandTab));
        }

        if down
            && let Some(member) = member_of(scan)
            && self.can_hold(member)
        {
            // The press that completes the combination is never held, because
            // there is nothing left to decide once it has arrived.
            if self.members | member.bit() != COMPLETE {
                self.hold(scan, member);
                return Outcome::nothing();
            }
            self.counts.hotkey_events_suppressed += u64::from(self.held_len) + 1;
            self.clear_holds();
            self.exit(ExitCause::ReleaseHotkey);
            return Outcome::only(Action::Exit(ExitCause::ReleaseHotkey));
        }

        // A near miss, which includes the release of a key being held: the
        // player has abandoned the gesture, so what was held is ordinary input
        // and goes out ahead of the event that ended the hold.
        let mut outcome = self.flush();
        outcome.admit = true;
        outcome
    }

    /// Reports that the cursor was taken, which is what turns the click that
    /// asked into a capture.
    ///
    /// Ignored unless the machine is still waiting for the answer. An entry can
    /// lose a race with a focus loss, and the exit that loss started is the
    /// one that must stand: capturing afterwards would leave the input pointed
    /// at a host the player has already left.
    pub fn entered(&mut self) {
        if !matches!(self.state, State::Entering) {
            return;
        }
        self.state = State::Capturing;
        self.counts.captures += 1;
    }

    /// Reports that the cursor could not be taken.
    ///
    /// Failure closed. The player keeps the machine they already had, which is
    /// the only outcome that cannot be mistaken for a broken computer.
    pub fn entry_failed(&mut self) {
        if !matches!(self.state, State::Entering) {
            return;
        }
        self.state = State::Uncaptured;
        self.counts.entries_failed += 1;
    }

    /// Reports the loss of control the exit path exists for, and returns its
    /// cause when there was something to give up.
    ///
    /// The edge and not the notification: AppKit posts two for one switch away,
    /// and a second release would put another event on the retransmission
    /// ladder while making the count of releases an operator reads meaningless.
    pub fn focus_lost(&mut self) -> Option<ExitCause> {
        if !self.focus.resigned() {
            return None;
        }
        self.leave(ExitCause::FocusLost)
    }

    /// Reports that this process is the one the input is for again.
    ///
    /// Deliberately not a capture. Somebody who switched back to look at the
    /// window has not asked to play, and the click is what asks, so taking the
    /// cursor here would take it from a person who was reading.
    pub fn focus_regained(&mut self) {
        self.focus.regained();
    }

    /// Reports that the session stopped working.
    pub fn session_failed(&mut self) -> Option<ExitCause> {
        self.leave(ExitCause::SessionFailure)
    }

    /// Reports that the release has been sent and the cursor is back, which is
    /// the last step of the exit and the only way out of [`State::Releasing`].
    pub fn released(&mut self) {
        if matches!(self.state, State::Releasing) {
            self.state = State::Uncaptured;
        }
    }

    /// The causes that arrive from outside an event, which are the two that can
    /// land while an entry is still in flight. Both exit from there as well as
    /// from a running capture, because a caller part-way through taking the
    /// cursor has to be told to give it back.
    fn leave(&mut self, cause: ExitCause) -> Option<ExitCause> {
        if !matches!(self.state, State::Entering | State::Capturing) {
            return None;
        }
        self.exit(cause);
        Some(cause)
    }

    /// The single exit. Admission closes here, before anything is handed back,
    /// which is the ordering the barrier rests on: the caller cannot mint the
    /// release's id until this has returned, so every id below that release
    /// belongs to the capture the release ends.
    fn exit(&mut self, cause: ExitCause) {
        self.state = State::Releasing;
        // Whatever the capture click still owed is moot once the host has been
        // told to let go of everything.
        self.owed_click_up = false;
        self.drop_holds();
        self.counts.exits[cause.index()] += 1;
        self.counts.releases += 1;
    }

    /// Whether a press may join the hold. Nothing but command opens one, and no
    /// member joins twice.
    fn can_hold(&self, member: Member) -> bool {
        self.members & member.bit() == 0 && (self.held_len > 0 || matches!(member, Member::Command))
    }

    fn hold(&mut self, scan: ScanCode, member: Member) {
        self.held[self.held_len as usize] = scan;
        self.held_len += 1;
        self.members |= member.bit();
    }

    fn flush(&mut self) -> Outcome {
        let outcome = Outcome {
            held: self.held,
            held_len: self.held_len,
            admit: false,
            action: Action::None,
        };
        self.clear_holds();
        outcome
    }

    /// Throws away a hold nobody can send any more. Counted as refused for the
    /// reason the counter exists: admission closed underneath these presses,
    /// and a press that was held and then dropped is a press the host was
    /// deliberately not told about.
    fn drop_holds(&mut self) {
        self.counts.refused += u64::from(self.held_len);
        self.clear_holds();
    }

    fn clear_holds(&mut self) {
        self.held_len = 0;
        self.members = 0;
    }
}

#[cfg(test)]
mod tests {
    use lanplay_input_protocol::{Button, EventId, Message};
    use lanplay_telemetry::Timestamp;

    use super::{
        Action, CAPTURE_BUTTON, ExitCause, LEFT_COMMAND, LEFT_CONTROL, LEFT_OPTION, Machine,
        RIGHT_OPTION, State, TAB,
    };
    use crate::{Reliable, ScanCode};

    const W: ScanCode = ScanCode {
        code: 0x11,
        extended: false,
    };
    const S: ScanCode = ScanCode {
        code: 0x1F,
        extended: false,
    };
    const LEFT_SHIFT: ScanCode = ScanCode {
        code: 0x2A,
        extended: false,
    };

    fn at(millis: u64) -> Timestamp {
        Timestamp::from_nanos(millis * 1_000_000)
    }

    /// Drives the click and the confirmation, which every test but the ones
    /// about failing to enter begins with.
    fn capture(machine: &mut Machine) {
        let outcome = machine.button(CAPTURE_BUTTON, true);
        assert_eq!(outcome.action(), Action::Enter);
        assert!(!outcome.admitted(), "the click that asks is not input");
        machine.entered();
        assert_eq!(machine.state(), State::Capturing);
        // The up that closes the click is part of it and is not input either.
        let up = machine.button(CAPTURE_BUTTON, false);
        assert!(!up.admitted());
        assert!(up.flushed().is_empty());
    }

    fn id(message: Message) -> EventId {
        message.event_id().expect("every message here is reliable")
    }

    /// The ordering the whole barrier rests on, asserted by the ids themselves.
    ///
    /// A press offered while the exit is under way must be unable to take an id
    /// at all, because an id above the release would make it legitimately
    /// post-barrier and the host would apply it to a session the player has
    /// already left.
    #[test]
    fn admission_closes_before_the_release_takes_its_id() {
        let mut machine = Machine::new();
        let mut reliable = Reliable::new(at(0));
        capture(&mut machine);

        assert!(machine.key(W, true).admitted());
        let press = id(reliable.key(W, true, at(1)));

        // The exit itself, which closes admission before it returns.
        let cause = machine.session_failed().expect("a capture was running");
        assert_eq!(cause, ExitCause::SessionFailure);
        assert_eq!(machine.state(), State::Releasing);

        // Everything the exit could plausibly race with, offered before the
        // release has been minted. None of it may reach the reliability layer,
        // so none of it can hold an id.
        assert!(!machine.admit());
        let stray = machine.key(W, false);
        assert!(!stray.admitted());
        assert!(stray.flushed().is_empty());
        let click = machine.button(Button::Right, true);
        assert!(!click.admitted());

        let barrier = id(reliable.release_all(at(2)));
        assert!(
            press < barrier,
            "everything the capture sent is below the barrier: {press:?} {barrier:?}"
        );

        // And the barrier really is the top of the capture: the next id minted
        // is the one the recaptured session takes.
        machine.released();
        assert_eq!(machine.state(), State::Uncaptured);
        capture(&mut machine);
        assert!(machine.key(S, true).admitted());
        let after = id(reliable.key(S, true, at(3)));
        assert!(
            barrier < after,
            "a press made after recapturing outranks the barrier: {barrier:?} {after:?}"
        );
    }

    /// A near miss must reach the host intact. The recognizer was holding two
    /// presses while it decided, and a combination that turns out not to match
    /// owes the player every one of them in the order they were made.
    #[test]
    fn a_near_miss_reaches_the_host_in_order() {
        let mut machine = Machine::new();
        capture(&mut machine);

        assert!(machine.key(LEFT_COMMAND, true).flushed().is_empty());
        assert!(machine.key(LEFT_CONTROL, true).flushed().is_empty());

        // Shift is not the third member, so the gesture was never the
        // combination and all three keys are ordinary input.
        let outcome = machine.key(LEFT_SHIFT, true);
        assert_eq!(outcome.flushed(), [LEFT_COMMAND, LEFT_CONTROL]);
        assert!(outcome.admitted(), "and then the key that ended the hold");
        assert_eq!(outcome.action(), Action::None);
        assert_eq!(machine.state(), State::Capturing);

        let counts = machine.counts();
        assert_eq!(counts.hotkey_events_suppressed, 0, "nothing was suppressed");
        assert_eq!(counts.refused, 0, "and nothing was dropped");
    }

    /// The mirror of the test above: when it does match, none of it travels.
    #[test]
    fn the_combination_never_reaches_the_host() {
        let mut machine = Machine::new();
        capture(&mut machine);

        assert!(machine.key(LEFT_COMMAND, true).flushed().is_empty());
        assert!(machine.key(LEFT_CONTROL, true).flushed().is_empty());
        let outcome = machine.key(RIGHT_OPTION, true);

        assert_eq!(outcome.action(), Action::Exit(ExitCause::ReleaseHotkey));
        assert!(!outcome.admitted());
        assert!(
            outcome.flushed().is_empty(),
            "a matched combination flushes nothing"
        );
        assert_eq!(machine.state(), State::Releasing);
        assert_eq!(machine.counts().hotkey_events_suppressed, 3);
    }

    /// Command first is the whole reason the modifiers a game binds are never
    /// held: control on its own is a crouch and must go out at once.
    #[test]
    fn a_modifier_pressed_without_command_is_never_held() {
        let mut machine = Machine::new();
        capture(&mut machine);

        let outcome = machine.key(LEFT_CONTROL, true);
        assert!(outcome.admitted());
        assert!(outcome.flushed().is_empty());
        assert_eq!(outcome.action(), Action::None);

        // And holding it does not arm the combination either, so command and
        // option after it are ordinary input too.
        assert!(machine.key(LEFT_OPTION, true).admitted());
        assert_eq!(machine.state(), State::Capturing);
    }

    /// Releasing a key that was being held abandons the gesture, and what was
    /// held is still owed to the host.
    #[test]
    fn releasing_a_held_key_flushes_what_was_held() {
        let mut machine = Machine::new();
        capture(&mut machine);

        assert!(machine.key(LEFT_COMMAND, true).flushed().is_empty());
        let outcome = machine.key(LEFT_COMMAND, false);
        assert_eq!(outcome.flushed(), [LEFT_COMMAND]);
        assert!(outcome.admitted(), "the release the player just made");
        assert_eq!(machine.state(), State::Capturing);
    }

    /// Command-tab is the same exit as any other, and its two keys are as local
    /// as the release combination's three.
    #[test]
    fn command_tab_exits_without_sending_either_key() {
        let mut machine = Machine::new();
        capture(&mut machine);

        assert!(machine.key(LEFT_COMMAND, true).flushed().is_empty());
        let outcome = machine.key(TAB, true);

        assert_eq!(outcome.action(), Action::Exit(ExitCause::CommandTab));
        assert!(!outcome.admitted());
        assert!(outcome.flushed().is_empty());
        assert_eq!(machine.counts().hotkey_events_suppressed, 2);
        assert_eq!(machine.counts().exits[ExitCause::CommandTab.index()], 1);
    }

    /// A tab with nothing held is a tab. The recognizer only ever looks at it
    /// while the player is already holding command.
    #[test]
    fn tab_on_its_own_is_input() {
        let mut machine = Machine::new();
        capture(&mut machine);

        let outcome = machine.key(TAB, true);
        assert!(outcome.admitted());
        assert_eq!(outcome.action(), Action::None);
        assert_eq!(machine.state(), State::Capturing);
    }

    /// Coming back to the application is not asking to play. An explicit click
    /// is what asks, and until one arrives nothing is sent.
    #[test]
    fn regaining_focus_does_not_recapture() {
        let mut machine = Machine::new();
        capture(&mut machine);
        assert!(machine.key(W, true).admitted());

        assert_eq!(machine.focus_lost(), Some(ExitCause::FocusLost));
        // The second notification AppKit posts for one switch away.
        assert_eq!(machine.focus_lost(), None);
        machine.released();
        assert_eq!(machine.state(), State::Uncaptured);

        machine.focus_regained();
        assert_eq!(machine.state(), State::Uncaptured);
        assert!(machine.focus().is_focused());
        assert!(!machine.admit(), "and the gate is still shut");
        assert!(!machine.key(W, false).admitted());

        // Only the click reopens it.
        capture(&mut machine);
        assert!(machine.key(W, false).admitted());
        assert_eq!(machine.counts().captures, 2);
    }

    /// Failure closed. Half-captured is the one state that must be unreachable,
    /// so a refused cursor leaves exactly the machine the player started with.
    #[test]
    fn a_failed_entry_leaves_the_machine_uncaptured() {
        let mut machine = Machine::new();

        let outcome = machine.button(CAPTURE_BUTTON, true);
        assert_eq!(outcome.action(), Action::Enter);
        // Nothing is admitted while the answer is outstanding either.
        assert_eq!(machine.state(), State::Entering);
        assert!(!machine.admit());

        machine.entry_failed();
        assert_eq!(machine.state(), State::Uncaptured);
        assert!(!machine.admit());
        assert!(!machine.key(W, true).admitted());

        let counts = machine.counts();
        assert_eq!(counts.entries_failed, 1);
        assert_eq!(counts.captures, 0);
        assert_eq!(counts.releases, 0, "there was nothing to release");
    }

    /// An entry that loses its race with a focus loss must not win it late.
    #[test]
    fn an_entry_that_lost_to_a_focus_loss_does_not_capture() {
        let mut machine = Machine::new();
        assert_eq!(machine.button(CAPTURE_BUTTON, true).action(), Action::Enter);

        assert_eq!(machine.focus_lost(), Some(ExitCause::FocusLost));
        assert_eq!(machine.state(), State::Releasing);

        // The window server call the click started finally returns.
        machine.entered();
        assert_eq!(machine.state(), State::Releasing);
        assert_eq!(machine.counts().captures, 0);

        machine.released();
        assert_eq!(machine.state(), State::Uncaptured);
    }

    /// One suppressed click per capture, and no more: the click is the
    /// interface, every click after it is input.
    #[test]
    fn the_capture_click_is_suppressed_once_per_capture() {
        let mut machine = Machine::new();

        for round in 1..=3 {
            capture(&mut machine);

            // The second click of the same capture is a shot fired.
            let outcome = machine.button(CAPTURE_BUTTON, true);
            assert!(outcome.admitted(), "round {round}");
            assert_eq!(outcome.action(), Action::None);
            assert!(machine.button(CAPTURE_BUTTON, false).admitted());

            machine.session_failed().expect("a capture was running");
            machine.released();

            let counts = machine.counts();
            assert_eq!(counts.capture_clicks_suppressed, round);
            assert_eq!(counts.captures, round);
        }
    }

    /// The button-up that closes a capture click never becomes input, even when
    /// the capture ended between the two halves of the click.
    #[test]
    fn the_click_that_asked_is_never_sent_in_either_half() {
        let mut machine = Machine::new();
        assert_eq!(machine.button(CAPTURE_BUTTON, true).action(), Action::Enter);
        machine.entered();
        machine.session_failed().expect("a capture was running");
        machine.released();

        let up = machine.button(CAPTURE_BUTTON, false);
        assert!(!up.admitted());
        // Refused rather than suppressed: the exit already told the host to let
        // go of everything, so this is one more event offered while uncaptured.
        assert_eq!(machine.counts().refused, 1);
        assert_eq!(machine.counts().capture_clicks_suppressed, 1);
    }

    /// A hold that an exit lands on top of is dropped rather than flushed. Its
    /// presses would carry ids above the release and the host would apply them
    /// after the barrier that was meant to end them.
    #[test]
    fn an_exit_drops_a_hold_instead_of_flushing_it() {
        let mut machine = Machine::new();
        capture(&mut machine);

        assert!(machine.key(LEFT_COMMAND, true).flushed().is_empty());
        assert_eq!(machine.focus_lost(), Some(ExitCause::FocusLost));
        assert_eq!(machine.counts().refused, 1);

        machine.released();
        capture(&mut machine);
        // And the hold did not survive into the next capture, so control on its
        // own is input again.
        assert!(machine.key(LEFT_CONTROL, true).admitted());
    }

    /// Every cause is the same exit, doing the same things in the same order.
    #[test]
    fn all_four_causes_take_the_one_exit() {
        let mut machine = Machine::new();

        for cause in ExitCause::ALL {
            capture(&mut machine);
            assert!(machine.key(W, true).admitted());

            match cause {
                ExitCause::FocusLost => {
                    assert_eq!(machine.focus_lost(), Some(cause));
                    machine.focus_regained();
                }
                ExitCause::SessionFailure => assert_eq!(machine.session_failed(), Some(cause)),
                ExitCause::ReleaseHotkey => {
                    machine.key(LEFT_COMMAND, true);
                    machine.key(LEFT_CONTROL, true);
                    let outcome = machine.key(LEFT_OPTION, true);
                    assert_eq!(outcome.action(), Action::Exit(cause));
                }
                ExitCause::CommandTab => {
                    machine.key(LEFT_COMMAND, true);
                    assert_eq!(machine.key(TAB, true).action(), Action::Exit(cause));
                }
            }

            assert_eq!(machine.state(), State::Releasing);
            machine.released();
            assert_eq!(machine.state(), State::Uncaptured);
        }

        let counts = machine.counts();
        assert_eq!(counts.exits, [1, 1, 1, 1]);
        assert_eq!(counts.releases, 4);
        assert_eq!(counts.captures, 4);
    }

    /// The counter that carries the claim. Nothing offered while uncaptured is
    /// ever admitted, whichever kind of event it was.
    #[test]
    fn everything_offered_while_uncaptured_is_refused_and_counted() {
        let mut machine = Machine::new();

        assert!(!machine.admit());
        assert!(!machine.key(W, true).admitted());
        assert!(!machine.button(Button::Right, true).admitted());
        assert!(!machine.button(Button::Right, false).admitted());
        assert_eq!(machine.counts().refused, 4);

        capture(&mut machine);
        machine.session_failed();
        // And again while the exit is under way, which is the window the
        // barrier depends on being empty.
        assert!(!machine.admit());
        assert!(!machine.key(LEFT_SHIFT, true).admitted());
        assert_eq!(machine.counts().refused, 6);
    }
}
