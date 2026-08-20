//! The input wire format, and which messages may be lost.
//!
//! Video and input want opposite things from the network. A late frame is
//! worthless, so the video path keeps only the newest and throws the rest
//! away. Input cannot work that way:
//!
//! ```text
//! mouse motion        may be lost
//! keys and buttons    may NEVER be lost
//! ```
//!
//! A dropped `W` down is an annoyance. A dropped `W` up leaves the player
//! walking into a wall forever. So the two classes are separated here, in the
//! format, rather than left to whoever writes the send loop:
//! [`Message::reliability`] is the single place that decides what has to be
//! retransmitted.
//!
//! Motion is additive, which is the other half of the same point. Given
//! `+4`, `+3`, `-2`, the pointer has moved by `+5`. Keeping only the newest
//! would move it by `-2`. Coalescing them into one `+5` is correct and
//! allowed; discarding the first two is not.
//!
//! ```text
//! Mac                               Windows
//! ─────────────────────────────────────────────
//! input UDP  ───────────────────────►
//!            ◄────────────────────── ACK / state
//! ```
//!
//! Nothing here reads a clock, opens a socket or touches an OS input API.
//! Both ends encode and decode with this and disagree about nothing.

#![forbid(unsafe_code)]

use core::fmt;

/// Bumped only for a change that an older peer would misread. The handshake
/// carries it, so a mismatch is refused before any input is injected.
pub const VERSION: u8 = 2;

/// Bytes before the payload. Fixed, so a decoder can reject a short datagram
/// before looking at anything.
pub const HEADER_LEN: usize = 20;

/// Largest datagram this format produces, which a snapshot sets.
pub const MAX_DATAGRAM: usize = HEADER_LEN + 37;

/// Identifies one streaming session, minted by the control plane.
///
/// The reason this exists is narrow and important: a datagram from a previous
/// session must never inject input into a new one. A stale motion packet
/// arriving after a reconnect would move the pointer; a stale key-down would
/// press a key nobody is touching.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct SessionId(pub u32);

/// Per-datagram counter, for loss and reorder measurement. Wraps, and
/// carries no reliability meaning: a gap here is not a lost event, because
/// most datagrams carry motion that nobody will retransmit.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Sequence(pub u32);

/// Identifies one reliable event across retransmissions.
///
/// Separate from [`Sequence`] because they answer different questions. The
/// sequence says which datagram this is; the event id says which key press
/// this is, so a host that sees it twice can acknowledge it twice and inject
/// it once.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct EventId(pub u64);

impl EventId {
    pub fn next(self) -> EventId {
        EventId(self.0 + 1)
    }
}

/// Whether losing a message is acceptable.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reliability {
    /// Lost silently. The next message supersedes or extends it, and
    /// retransmitting would deliver stale movement late, which is worse than
    /// not delivering it.
    Unreliable,
    /// Retransmitted until acknowledged, and applied exactly once.
    Reliable,
}

/// Which mouse button, in the order Windows and macOS agree on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Button {
    Left,
    Right,
    Middle,
    X1,
    X2,
}

impl Button {
    pub fn from_index(index: u8) -> Option<Button> {
        Some(match index {
            0 => Button::Left,
            1 => Button::Right,
            2 => Button::Middle,
            3 => Button::X1,
            4 => Button::X2,
            _ => return None,
        })
    }

    pub fn index(self) -> u8 {
        match self {
            Button::Left => 0,
            Button::Right => 1,
            Button::Middle => 2,
            Button::X1 => 3,
            Button::X2 => 4,
        }
    }

    /// Bit for this button in a snapshot's button field.
    pub fn mask(self) -> u8 {
        1 << self.index()
    }
}

/// Which direction the directional pad is held.
///
/// A hat is one value rather than four buttons because XInput exposes it that
/// way and diagonal positions are physical states, not two independent presses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Dpad {
    Neutral = 0,
    North = 1,
    NorthEast = 2,
    East = 3,
    SouthEast = 4,
    South = 5,
    SouthWest = 6,
    West = 7,
    NorthWest = 8,
}

impl Dpad {
    pub fn from_wire(value: u8) -> Option<Self> {
        Some(match value {
            0 => Self::Neutral,
            1 => Self::North,
            2 => Self::NorthEast,
            3 => Self::East,
            4 => Self::SouthEast,
            5 => Self::South,
            6 => Self::SouthWest,
            7 => Self::West,
            8 => Self::NorthWest,
            _ => return None,
        })
    }
}

/// The complete state of one controller at one instant.
///
/// `sequence` belongs to a slot rather than the UDP datagram stream. A keyboard
/// packet sharing the socket must not make an older controller state look new.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct GamepadStateV1 {
    pub session_generation: u32,
    pub controller_slot: u8,
    pub sequence: u32,
    pub buttons: u16,
    pub dpad: Dpad,
    pub left_x: i16,
    pub left_y: i16,
    pub right_x: i16,
    pub right_y: i16,
    pub left_trigger: u16,
    pub right_trigger: u16,
}

impl GamepadStateV1 {
    pub const NEUTRAL: Self = Self {
        session_generation: 0,
        controller_slot: 0,
        sequence: 0,
        buttons: 0,
        dpad: Dpad::Neutral,
        left_x: 0,
        left_y: 0,
        right_x: 0,
        right_y: 0,
        left_trigger: 0,
        right_trigger: 0,
    };

    pub fn neutral_for(session_generation: u32, controller_slot: u8, sequence: u32) -> Self {
        Self {
            session_generation,
            controller_slot,
            sequence,
            ..Self::NEUTRAL
        }
    }
}

/// Which keys are held, by scan code.
///
/// A bitset rather than a list, so a snapshot is a fixed 32 bytes whatever
/// the user is doing and can never be truncated by a datagram limit. Indexed
/// by set-1 scan code, which is what the host injects; the extended flag is
/// folded in by the caller because 0xE0-prefixed codes occupy the high half.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct KeyBitset([u8; 32]);

impl KeyBitset {
    pub const EMPTY: KeyBitset = KeyBitset([0; 32]);

    pub fn set(&mut self, scancode: u8, down: bool) {
        let byte = (scancode / 8) as usize;
        let bit = 1u8 << (scancode % 8);
        if down {
            self.0[byte] |= bit;
        } else {
            self.0[byte] &= !bit;
        }
    }

    pub fn contains(&self, scancode: u8) -> bool {
        self.0[(scancode / 8) as usize] & (1u8 << (scancode % 8)) != 0
    }

    pub fn is_empty(&self) -> bool {
        self.0.iter().all(|byte| *byte == 0)
    }

    pub fn held(&self) -> impl Iterator<Item = u8> + '_ {
        (0u8..=255).filter(|code| self.contains(*code))
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn from_bytes(bytes: [u8; 32]) -> KeyBitset {
        KeyBitset(bytes)
    }
}

impl fmt::Debug for KeyBitset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.held()).finish()
    }
}

/// What one datagram carries.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Message {
    /// Relative pointer movement since the last motion message. Additive.
    Motion { dx: i32, dy: i32 },
    Button {
        id: EventId,
        button: Button,
        down: bool,
    },
    /// A physical key, by set-1 scan code rather than by virtual key, so the
    /// host reproduces the key that was pressed rather than the character
    /// the client's layout would have produced.
    Key {
        id: EventId,
        scancode: u16,
        down: bool,
        /// Needs the 0xE0 prefix on the wire to the OS.
        extended: bool,
    },
    /// Wheel detents. Reliable: losing one can change weapon or tool, which
    /// is a state change and not a smoothed motion.
    Wheel { id: EventId, dx: i16, dy: i16 },
    /// Everything the client believes is held, so a lost release can be
    /// repaired without waiting for the user to press the key again.
    ///
    /// `generation` increases whenever the client's own view changes, so a
    /// reordered snapshot is discarded rather than resurrecting a key.
    Snapshot {
        generation: u32,
        keys: KeyBitset,
        buttons: u8,
    },
    /// Release everything. Sent on focus loss, capture release and
    /// disconnect, and reliable because it is the safety invariant.
    ReleaseAll { id: EventId },
    /// Keeps the host's idea of liveness fresh while the user is idle, so a
    /// silent client can be told apart from a departed one.
    Heartbeat,
    /// Host to client. `top` is the highest event id the host has applied,
    /// and bit `i` of `missing` says that `top - 1 - i` has not been.
    ///
    /// Anchored at the top rather than at the bottom, and fault injection is
    /// what settled it. A cumulative acknowledgement plus a window of ids
    /// *above* it collapses the moment one event is lost for good: the
    /// frontier stops, the window is 32 ids wide, and everything beyond that
    /// can never be acknowledged however many times it arrives. Five per cent
    /// loss abandoned 95% of events that way.
    ///
    /// Anchored here, a hole delays only itself. An id that falls out of the
    /// window below `top` has outlived the client's retransmission ladder, so
    /// it is beyond repair by retransmission and the periodic snapshot is what
    /// fixes the state.
    Ack { top: EventId, missing: u32 },
    /// Creates one virtual controller. The control plane retries this until
    /// the host acknowledges it; no controller state is accepted before it.
    GamepadAttach {
        id: EventId,
        controller_slot: u8,
        session_generation: u32,
    },
    /// Destroys one virtual controller. The host first submits neutral state,
    /// so a lost final state cannot leave an axis or button held.
    GamepadDetach {
        id: EventId,
        controller_slot: u8,
        session_generation: u32,
    },
    /// A complete, newest-wins controller state. It is never retransmitted:
    /// preserving a stale stick position is worse than losing it.
    GamepadState(GamepadStateV1),
}

impl Message {
    pub fn reliability(&self) -> Reliability {
        match self {
            // Motion is superseded by the next motion and additive with it,
            // so a retransmission would apply movement the user has already
            // continued past.
            Message::Motion { .. }
            | Message::Heartbeat
            | Message::Ack { .. }
            | Message::Snapshot { .. }
            | Message::GamepadState(_) => Reliability::Unreliable,
            Message::Button { .. }
            | Message::Key { .. }
            | Message::Wheel { .. }
            | Message::ReleaseAll { .. }
            | Message::GamepadAttach { .. }
            | Message::GamepadDetach { .. } => Reliability::Reliable,
        }
    }

    /// The id a host deduplicates on, for messages that have one.
    pub fn event_id(&self) -> Option<EventId> {
        match self {
            Message::Button { id, .. }
            | Message::Key { id, .. }
            | Message::Wheel { id, .. }
            | Message::ReleaseAll { id }
            | Message::GamepadAttach { id, .. }
            | Message::GamepadDetach { id, .. } => Some(*id),
            _ => None,
        }
    }

    fn kind(&self) -> u8 {
        match self {
            Message::Motion { .. } => 1,
            Message::Button { .. } => 2,
            Message::Key { .. } => 3,
            Message::Wheel { .. } => 4,
            Message::Snapshot { .. } => 5,
            Message::ReleaseAll { .. } => 6,
            Message::Heartbeat => 7,
            Message::Ack { .. } => 8,
            Message::GamepadAttach { .. } => 9,
            Message::GamepadDetach { .. } => 10,
            Message::GamepadState(_) => 11,
        }
    }
}

/// A decoded datagram: who sent it, when they say they sent it, and what it
/// carries.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Datagram {
    pub session: SessionId,
    pub sequence: Sequence,
    /// The sender's own monotonic clock, in nanoseconds.
    ///
    /// Deliberately not comparable with the receiver's: the two machines have
    /// no common epoch and pretending otherwise would produce a one-way
    /// latency figure that is really a clock offset. Each side measures its
    /// own intervals; this exists so a host can tell how long a burst spanned
    /// at the client and discard stale motion.
    pub sent_at_ns: u64,
    pub message: Message,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DecodeError {
    TooShort {
        len: usize,
    },
    BadVersion {
        version: u8,
    },
    UnknownKind {
        kind: u8,
    },
    /// The kind is known but the payload is the wrong length for it.
    BadPayload {
        kind: u8,
        len: usize,
    },
    BadButton {
        index: u8,
    },
    BadDpad {
        value: u8,
    },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::TooShort { len } => write!(f, "{len} bytes is shorter than a header"),
            DecodeError::BadVersion { version } => write!(f, "version {version} is not {VERSION}"),
            DecodeError::UnknownKind { kind } => write!(f, "unknown message kind {kind}"),
            DecodeError::BadPayload { kind, len } => {
                write!(f, "kind {kind} cannot have a {len} byte payload")
            }
            DecodeError::BadButton { index } => write!(f, "no button {index}"),
            DecodeError::BadDpad { value } => write!(f, "no d-pad position {value}"),
        }
    }
}

impl core::error::Error for DecodeError {}

/// Writes a datagram. Returns how many bytes were used.
///
/// Never allocates and never panics on a buffer of at least [`MAX_DATAGRAM`]
/// bytes; a shorter one yields `None` rather than a partial write.
pub fn encode(datagram: &Datagram, out: &mut [u8]) -> Option<usize> {
    let payload_len = payload_len(&datagram.message);
    if out.len() < HEADER_LEN + payload_len {
        return None;
    }
    out[0] = VERSION;
    out[1] = datagram.message.kind();
    out[2] = 0;
    out[3] = 0;
    out[4..8].copy_from_slice(&datagram.session.0.to_be_bytes());
    out[8..12].copy_from_slice(&datagram.sequence.0.to_be_bytes());
    out[12..20].copy_from_slice(&datagram.sent_at_ns.to_be_bytes());

    let body = &mut out[HEADER_LEN..HEADER_LEN + payload_len];
    match &datagram.message {
        Message::Motion { dx, dy } => {
            body[0..4].copy_from_slice(&dx.to_be_bytes());
            body[4..8].copy_from_slice(&dy.to_be_bytes());
        }
        Message::Button { id, button, down } => {
            body[0..8].copy_from_slice(&id.0.to_be_bytes());
            body[8] = button.index();
            body[9] = u8::from(*down);
        }
        Message::Key {
            id,
            scancode,
            down,
            extended,
        } => {
            body[0..8].copy_from_slice(&id.0.to_be_bytes());
            body[8..10].copy_from_slice(&scancode.to_be_bytes());
            body[10] = u8::from(*down) | (u8::from(*extended) << 1);
        }
        Message::Wheel { id, dx, dy } => {
            body[0..8].copy_from_slice(&id.0.to_be_bytes());
            body[8..10].copy_from_slice(&dx.to_be_bytes());
            body[10..12].copy_from_slice(&dy.to_be_bytes());
        }
        Message::Snapshot {
            generation,
            keys,
            buttons,
        } => {
            body[0..4].copy_from_slice(&generation.to_be_bytes());
            body[4..36].copy_from_slice(keys.as_bytes());
            body[36] = *buttons;
        }
        Message::ReleaseAll { id } => body[0..8].copy_from_slice(&id.0.to_be_bytes()),
        Message::Heartbeat => {}
        Message::Ack { top, missing } => {
            body[0..8].copy_from_slice(&top.0.to_be_bytes());
            body[8..12].copy_from_slice(&missing.to_be_bytes());
        }
        Message::GamepadAttach {
            id,
            controller_slot,
            session_generation,
        }
        | Message::GamepadDetach {
            id,
            controller_slot,
            session_generation,
        } => {
            body[0..8].copy_from_slice(&id.0.to_be_bytes());
            body[8] = *controller_slot;
            body[9..13].copy_from_slice(&session_generation.to_be_bytes());
        }
        Message::GamepadState(state) => {
            body[0..4].copy_from_slice(&state.session_generation.to_be_bytes());
            body[4] = state.controller_slot;
            body[5..9].copy_from_slice(&state.sequence.to_be_bytes());
            body[9..11].copy_from_slice(&state.buttons.to_be_bytes());
            body[11] = state.dpad as u8;
            body[12..14].copy_from_slice(&state.left_x.to_be_bytes());
            body[14..16].copy_from_slice(&state.left_y.to_be_bytes());
            body[16..18].copy_from_slice(&state.right_x.to_be_bytes());
            body[18..20].copy_from_slice(&state.right_y.to_be_bytes());
            body[20..22].copy_from_slice(&state.left_trigger.to_be_bytes());
            body[22..24].copy_from_slice(&state.right_trigger.to_be_bytes());
        }
    }
    Some(HEADER_LEN + payload_len)
}
fn payload_len(message: &Message) -> usize {
    match message {
        Message::Motion { .. } => 8,
        Message::Button { .. } => 10,
        Message::Key { .. } => 11,
        Message::Wheel { .. } => 12,
        Message::Snapshot { .. } => 37,
        Message::ReleaseAll { .. } => 8,
        Message::Heartbeat => 0,
        Message::Ack { .. } => 12,
        Message::GamepadAttach { .. } | Message::GamepadDetach { .. } => 13,
        Message::GamepadState(_) => 24,
    }
}

/// Reads a datagram. Never panics, never allocates.
pub fn decode(bytes: &[u8]) -> Result<Datagram, DecodeError> {
    if bytes.len() < HEADER_LEN {
        return Err(DecodeError::TooShort { len: bytes.len() });
    }
    if bytes[0] != VERSION {
        return Err(DecodeError::BadVersion { version: bytes[0] });
    }
    let kind = bytes[1];
    let body = &bytes[HEADER_LEN..];
    let short = |need: usize| DecodeError::BadPayload {
        kind,
        len: body.len().min(need),
    };
    let id = |body: &[u8]| {
        EventId(u64::from_be_bytes([
            body[0], body[1], body[2], body[3], body[4], body[5], body[6], body[7],
        ]))
    };

    let message = match kind {
        1 => {
            if body.len() < 8 {
                return Err(short(8));
            }
            Message::Motion {
                dx: i32::from_be_bytes([body[0], body[1], body[2], body[3]]),
                dy: i32::from_be_bytes([body[4], body[5], body[6], body[7]]),
            }
        }
        2 => {
            if body.len() < 10 {
                return Err(short(10));
            }
            Message::Button {
                id: id(body),
                button: Button::from_index(body[8])
                    .ok_or(DecodeError::BadButton { index: body[8] })?,
                down: body[9] != 0,
            }
        }
        3 => {
            if body.len() < 11 {
                return Err(short(11));
            }
            Message::Key {
                id: id(body),
                scancode: u16::from_be_bytes([body[8], body[9]]),
                down: body[10] & 1 != 0,
                extended: body[10] & 2 != 0,
            }
        }
        4 => {
            if body.len() < 12 {
                return Err(short(12));
            }
            Message::Wheel {
                id: id(body),
                dx: i16::from_be_bytes([body[8], body[9]]),
                dy: i16::from_be_bytes([body[10], body[11]]),
            }
        }
        5 => {
            if body.len() < 37 {
                return Err(short(37));
            }
            let mut keys = [0u8; 32];
            keys.copy_from_slice(&body[4..36]);
            Message::Snapshot {
                generation: u32::from_be_bytes([body[0], body[1], body[2], body[3]]),
                keys: KeyBitset::from_bytes(keys),
                buttons: body[36],
            }
        }
        6 => {
            if body.len() < 8 {
                return Err(short(8));
            }
            Message::ReleaseAll { id: id(body) }
        }
        7 => Message::Heartbeat,
        8 => {
            if body.len() < 12 {
                return Err(short(12));
            }
            Message::Ack {
                top: id(body),
                missing: u32::from_be_bytes([body[8], body[9], body[10], body[11]]),
            }
        }
        9 | 10 => {
            if body.len() < 13 {
                return Err(short(13));
            }
            let id = id(body);
            let controller_slot = body[8];
            let session_generation = u32::from_be_bytes([body[9], body[10], body[11], body[12]]);
            if kind == 9 {
                Message::GamepadAttach {
                    id,
                    controller_slot,
                    session_generation,
                }
            } else {
                Message::GamepadDetach {
                    id,
                    controller_slot,
                    session_generation,
                }
            }
        }
        11 => {
            if body.len() < 24 {
                return Err(short(24));
            }
            Message::GamepadState(GamepadStateV1 {
                session_generation: u32::from_be_bytes([body[0], body[1], body[2], body[3]]),
                controller_slot: body[4],
                sequence: u32::from_be_bytes([body[5], body[6], body[7], body[8]]),
                buttons: u16::from_be_bytes([body[9], body[10]]),
                dpad: Dpad::from_wire(body[11]).ok_or(DecodeError::BadDpad { value: body[11] })?,
                left_x: i16::from_be_bytes([body[12], body[13]]),
                left_y: i16::from_be_bytes([body[14], body[15]]),
                right_x: i16::from_be_bytes([body[16], body[17]]),
                right_y: i16::from_be_bytes([body[18], body[19]]),
                left_trigger: u16::from_be_bytes([body[20], body[21]]),
                right_trigger: u16::from_be_bytes([body[22], body[23]]),
            })
        }
        other => return Err(DecodeError::UnknownKind { kind: other }),
    };

    Ok(Datagram {
        session: SessionId(u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]])),
        sequence: Sequence(u32::from_be_bytes([
            bytes[8], bytes[9], bytes[10], bytes[11],
        ])),
        sent_at_ns: u64::from_be_bytes([
            bytes[12], bytes[13], bytes[14], bytes[15], bytes[16], bytes[17], bytes[18], bytes[19],
        ]),
        message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(message: Message) {
        let datagram = Datagram {
            session: SessionId(0xDEAD_BEEF),
            sequence: Sequence(12345),
            sent_at_ns: 987_654_321_000,
            message,
        };
        let mut buffer = [0u8; MAX_DATAGRAM];
        let len = encode(&datagram, &mut buffer).expect("encodes");
        assert!(
            len <= MAX_DATAGRAM,
            "{len} bytes exceeds the stated maximum"
        );
        let decoded = decode(&buffer[..len]).expect("decodes");
        assert_eq!(decoded, datagram);
    }

    #[test]
    fn every_message_survives_the_wire() {
        let mut keys = KeyBitset::EMPTY;
        keys.set(0x11, true); // W
        keys.set(0x2A, true); // left shift
        for message in [
            Message::Motion { dx: -7, dy: 13 },
            Message::Button {
                id: EventId(1),
                button: Button::X2,
                down: true,
            },
            Message::Key {
                id: EventId(2),
                scancode: 0x11,
                down: true,
                extended: false,
            },
            Message::Key {
                id: EventId(3),
                scancode: 0x1D,
                down: false,
                extended: true,
            },
            Message::Wheel {
                id: EventId(4),
                dx: -1,
                dy: 3,
            },
            Message::Snapshot {
                generation: 9,
                keys,
                buttons: Button::Left.mask() | Button::Middle.mask(),
            },
            Message::ReleaseAll { id: EventId(5) },
            Message::Heartbeat,
            Message::Ack {
                top: EventId(41),
                missing: 0b1011,
            },
            Message::GamepadAttach {
                id: EventId(6),
                controller_slot: 0,
                session_generation: 7,
            },
            Message::GamepadDetach {
                id: EventId(7),
                controller_slot: 0,
                session_generation: 7,
            },
            Message::GamepadState(GamepadStateV1 {
                session_generation: 7,
                controller_slot: 0,
                sequence: 19,
                buttons: 0xABCD,
                dpad: Dpad::SouthWest,
                left_x: i16::MIN,
                left_y: -1,
                right_x: 1,
                right_y: i16::MAX,
                left_trigger: 0,
                right_trigger: u16::MAX,
            }),
        ] {
            round_trip(message);
        }
    }

    #[test]
    fn state_is_droppable_but_controller_lifecycle_is_not() {
        for droppable in [
            Message::Motion { dx: 1, dy: 1 },
            Message::Snapshot {
                generation: 1,
                keys: KeyBitset::EMPTY,
                buttons: 0,
            },
            Message::GamepadState(GamepadStateV1::NEUTRAL),
        ] {
            assert_eq!(droppable.reliability(), Reliability::Unreliable);
        }
        for reliable in [
            Message::Key {
                id: EventId(1),
                scancode: 0x11,
                down: false,
                extended: false,
            },
            Message::Button {
                id: EventId(1),
                button: Button::Left,
                down: false,
            },
            Message::Wheel {
                id: EventId(1),
                dx: 0,
                dy: 1,
            },
            Message::ReleaseAll { id: EventId(1) },
            Message::GamepadAttach {
                id: EventId(2),
                controller_slot: 0,
                session_generation: 1,
            },
            Message::GamepadDetach {
                id: EventId(3),
                controller_slot: 0,
                session_generation: 1,
            },
        ] {
            assert_eq!(
                reliable.reliability(),
                Reliability::Reliable,
                "{reliable:?} must not be droppable"
            );
            assert!(
                reliable.event_id().is_some(),
                "{reliable:?} needs an id to be deduplicated by"
            );
        }
    }

    #[test]
    fn negative_motion_is_not_mangled() {
        // Full 32-bit signed range: a high-resolution mouse whipped across a
        // desk can produce large deltas, and a wrapped one would fling the
        // pointer the wrong way.
        for (dx, dy) in [(-1, -1), (i32::MIN, i32::MAX), (0, -32768), (65536, -65536)] {
            round_trip(Message::Motion { dx, dy });
        }
    }

    #[test]
    fn a_truncated_datagram_is_refused_rather_than_guessed() {
        let datagram = Datagram {
            session: SessionId(1),
            sequence: Sequence(1),
            sent_at_ns: 0,
            message: Message::Snapshot {
                generation: 1,
                keys: KeyBitset::EMPTY,
                buttons: 0,
            },
        };
        let mut buffer = [0u8; MAX_DATAGRAM];
        let len = encode(&datagram, &mut buffer).expect("encodes");
        for cut in 1..len {
            assert!(
                decode(&buffer[..cut]).is_err(),
                "{cut} of {len} bytes decoded as a whole message"
            );
        }
    }

    #[test]
    fn a_buffer_too_small_writes_nothing() {
        let datagram = Datagram {
            session: SessionId(1),
            sequence: Sequence(1),
            sent_at_ns: 0,
            message: Message::Snapshot {
                generation: 1,
                keys: KeyBitset::EMPTY,
                buttons: 0,
            },
        };
        let mut buffer = [0u8; HEADER_LEN + 4];
        assert!(encode(&datagram, &mut buffer).is_none());
        assert!(buffer.iter().all(|byte| *byte == 0), "partial write");
    }

    #[test]
    fn an_unknown_version_or_kind_is_named() {
        let mut buffer = [0u8; MAX_DATAGRAM];
        let len = encode(
            &Datagram {
                session: SessionId(1),
                sequence: Sequence(1),
                sent_at_ns: 0,
                message: Message::Heartbeat,
            },
            &mut buffer,
        )
        .expect("encodes");
        let mut wrong_version = buffer;
        wrong_version[0] = VERSION + 1;
        assert_eq!(
            decode(&wrong_version[..len]),
            Err(DecodeError::BadVersion {
                version: VERSION + 1
            })
        );
        let mut wrong_kind = buffer;
        wrong_kind[1] = 99;
        assert_eq!(
            decode(&wrong_kind[..len]),
            Err(DecodeError::UnknownKind { kind: 99 })
        );
    }

    #[test]
    fn a_bad_dpad_is_refused_rather_than_reinterpreted() {
        let mut buffer = [0u8; MAX_DATAGRAM];
        let len = encode(
            &Datagram {
                session: SessionId(1),
                sequence: Sequence(1),
                sent_at_ns: 0,
                message: Message::GamepadState(GamepadStateV1::NEUTRAL),
            },
            &mut buffer,
        )
        .expect("encodes");
        buffer[HEADER_LEN + 11] = 9;
        assert_eq!(
            decode(&buffer[..len]),
            Err(DecodeError::BadDpad { value: 9 })
        );
    }
    #[test]
    fn a_key_bitset_holds_every_scan_code() {
        let mut keys = KeyBitset::EMPTY;
        assert!(keys.is_empty());
        for code in 0u8..=255 {
            keys.set(code, true);
        }
        assert_eq!(keys.held().count(), 256);
        for code in 0u8..=255 {
            assert!(keys.contains(code), "lost {code}");
            keys.set(code, false);
        }
        assert!(keys.is_empty());
    }

    #[test]
    fn a_snapshot_is_the_largest_datagram() {
        // MAX_DATAGRAM is a promise to every caller sizing a buffer, and a
        // snapshot is what makes it true.
        let mut buffer = [0u8; MAX_DATAGRAM];
        let len = encode(
            &Datagram {
                session: SessionId(1),
                sequence: Sequence(1),
                sent_at_ns: 0,
                message: Message::Snapshot {
                    generation: 1,
                    keys: KeyBitset::EMPTY,
                    buttons: 0,
                },
            },
            &mut buffer,
        )
        .expect("encodes");
        assert_eq!(len, MAX_DATAGRAM);
    }
}
