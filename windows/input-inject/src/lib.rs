//! Applying the client's input to Windows, and nothing else.
//!
//! A datagram decoded by `lanplay-input-protocol` becomes calls to
//! `SendInput`. There is no queue, no timer and no batching between the socket
//! and the input system: one message in, one call out per action it implies.
//! That is deliberate, because it is the baseline the project will measure a
//! batched or coalescing injector against later, and a baseline with a
//! scheduler in it measures the scheduler.
//!
//! The work splits in two so that the hard part is testable. [`state`] decides
//! what the OS must be told, keeps the host's own idea of what is held, and
//! never touches an API; [`send`] turns each decision into one `INPUT`. Every
//! interesting case -- a retransmitted key press, a lost release, a reordered
//! snapshot -- is decided in the first half and can be tested anywhere.
//!
//! Two properties of this backend are worth knowing before reading a
//! measurement taken with it.
//!
//! Synthesised relative motion is not exempt from the mouse settings. It
//! enters the same path a physical device's motion does, so the system pointer
//! speed slider and, when it is on, Enhanced Pointer Precision both apply, and
//! the pointer therefore travels by something other than the delta the client
//! measured. Nothing here tries to compensate for it: a compensation would
//! have to model a curve that belongs to the host's user, and it would have to
//! be undone the moment a game reads raw input instead of the cursor. The
//! honest fix is a different backend, and evaluating a virtual HID device
//! against this one is exactly why this one is measured first.
//!
//! `SendInput` can also be refused. User Interface Privilege Isolation blocks
//! injection into a window belonging to a process at a higher integrity level,
//! and the return value says only how many events were inserted, so a refusal
//! is indistinguishable from any other failure. Refusals are counted and
//! reported rather than swallowed: a run where every event was refused looks
//! exactly like a run where every event landed, unless somebody counts.
//!
//! ```text
//! Mac                         Windows
//! ─────────────────────────────────────────────────────
//! input UDP 5006  ──────────► recv_from
//!                             decode
//!                             HostState::apply
//!                             SendInput, once per action
//! ```
//!
//! `input-inject-probe` is that path with a histogram around it and nothing
//! else running, so the pair can be exercised from a shell without the video
//! pipeline.

pub mod probe;
pub mod state;

#[cfg(windows)]
pub mod send;

pub use state::{Action, HostState, Outcome, WheelAxis, key_slot, slot_key};

#[cfg(windows)]
pub use send::Injector;
