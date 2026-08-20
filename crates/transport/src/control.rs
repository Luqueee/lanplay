//! Control plane: a small TCP channel that negotiates a session and then gets
//! out of the way.
//!
//! The defining property of everything in this file is what it *cannot* do. A
//! control peer that stops reading, stalls for a minute or vanishes must not
//! move a single frame of a 120 fps RTP stream, so the control plane shares no
//! lock, no queue and no thread with the media path. The one datum that
//! crosses over is a [`SocketAddr`], and it is copied once when a stream
//! starts, never looked up per packet.
//!
//! Everything here is bounded on purpose. The frame header caps a payload at
//! 64 KiB before a single byte is allocated, the session table has a hard
//! ceiling, and every socket operation carries a timeout, because the failure
//! mode this file exists to prevent is *waiting*.

use core::fmt;
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use lanplay_protocol::VideoCodec;
use lanplay_telemetry::{Nanos, Timestamp};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::rtp::{Ssrc, random_u64};

/// `"LPLY"`. A stray HTTP request or a port scan is rejected on its first four
/// bytes rather than being decoded as a length.
pub const CONTROL_MAGIC: u32 = 0x4C_50_4C_59;

/// Wire format version of the framing itself.
pub const CONTROL_VERSION: u16 = 1;

/// Application protocol version carried inside the hello messages.
pub const PROTOCOL_VERSION: u16 = 1;

/// Largest payload a frame may declare.
///
/// The number matters less than the fact that there is one: `payload_len` is
/// an attacker-chosen `u32`, and a receiver that turns it straight into a
/// `Vec` capacity is a one-datagram denial of service. Real control messages
/// are a few hundred bytes; 64 KiB leaves room for a parameter-set blob later
/// without leaving room for abuse.
pub const MAX_CONTROL_PAYLOAD: usize = 64 * 1024;

/// magic(4) + version(2) + message_type(2) + payload_len(4).
pub const FRAME_HEADER_LEN: usize = 12;

/// Ceiling on live sessions a server will hand out tokens for.
///
/// Without it, a peer that connects, says hello and disconnects in a loop
/// grows the session table forever.
pub const MAX_SESSIONS: usize = 64;

/// Applied to a session socket once the handshake is done, so a stalled peer
/// costs a bounded wait rather than a wedged thread.
const DEFAULT_IO_TIMEOUT: Nanos = Nanos::from_millis(1_000);

/// Granularity of the accept poll. Accept is a once-per-session operation on
/// the caller's own thread, so a 1 ms poll is cheaper than the machinery a
/// precise wakeup would need.
const ACCEPT_POLL: Duration = Duration::from_millis(1);

/// Everything that can be wrong with a control exchange that is not an
/// underlying socket failure.
///
/// All of these close the connection. None of them panic, and none of them
/// allocate anything sized by the peer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ControlError {
    /// First four bytes were not [`CONTROL_MAGIC`].
    BadMagic(u32),
    /// Framing version this build does not speak.
    UnsupportedVersion(u16),
    /// Declared payload length exceeds [`MAX_CONTROL_PAYLOAD`].
    PayloadTooLarge(u32),
    /// The header's `message_type` disagreed with the decoded payload, so one
    /// of the two is lying.
    MessageTypeMismatch { declared: u16, decoded: u16 },
    /// A valid message arrived at a point in the exchange that forbids it.
    UnexpectedMessage { expected: u16, received: u16 },
    /// A `Pong` came back carrying somebody else's nonce.
    NonceMismatch,
    /// [`MAX_SESSIONS`] live sessions already.
    TooManySessions,
    /// An operation that needs a negotiated token was attempted without one.
    NoSession,
}

impl fmt::Display for ControlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ControlError::BadMagic(magic) => write!(f, "bad control magic {magic:#010x}"),
            ControlError::UnsupportedVersion(version) => {
                write!(f, "unsupported control version {version}")
            }
            ControlError::PayloadTooLarge(len) => {
                write!(
                    f,
                    "control payload of {len} bytes exceeds {MAX_CONTROL_PAYLOAD}"
                )
            }
            ControlError::MessageTypeMismatch { declared, decoded } => {
                write!(
                    f,
                    "frame declared message type {declared} but carried {decoded}"
                )
            }
            ControlError::UnexpectedMessage { expected, received } => {
                write!(f, "expected message type {expected}, received {received}")
            }
            ControlError::NonceMismatch => write!(f, "pong nonce did not match the ping"),
            ControlError::TooManySessions => {
                write!(f, "session table is full ({MAX_SESSIONS} sessions)")
            }
            ControlError::NoSession => write!(f, "no session has been negotiated"),
        }
    }
}

impl std::error::Error for ControlError {}

impl From<ControlError> for io::Error {
    fn from(error: ControlError) -> io::Error {
        let kind = match error {
            ControlError::TooManySessions => io::ErrorKind::ConnectionRefused,
            ControlError::NoSession => io::ErrorKind::NotConnected,
            _ => io::ErrorKind::InvalidData,
        };
        io::Error::new(kind, error)
    }
}

/// Opaque handle tying a UDP source address to a control session.
///
/// This stops stale and accidental associations: a client that reconnects gets
/// a fresh token, so packets from its previous incarnation, or from an
/// unrelated process that happens to reach the media port, do not land on the
/// new session. It is **not authentication**. It travels in clear over a
/// plain TCP connection and anyone who can observe the link can replay it.
/// Real authentication belongs in a TLS handshake, not here.
#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct SessionToken([u8; 16]);

impl SessionToken {
    /// 128 bits from the same source RFC 3550 initial values come from.
    pub fn generate() -> Self {
        let mut bytes = [0u8; 16];
        bytes[..8].copy_from_slice(&random_u64().to_le_bytes());
        bytes[8..].copy_from_slice(&random_u64().to_le_bytes());
        SessionToken(bytes)
    }

    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        SessionToken(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl PartialEq for SessionToken {
    /// Constant time: every byte is always examined.
    ///
    /// A comparison that returns early on the first differing byte tells a
    /// prober how much of a guess was right, which turns 2^128 into 16 rounds
    /// of 256 guesses.
    fn eq(&self, other: &Self) -> bool {
        let mut difference = 0u8;
        for (a, b) in self.0.iter().zip(other.0.iter()) {
            difference |= a ^ b;
        }
        difference == 0
    }
}

impl Eq for SessionToken {}

impl core::hash::Hash for SessionToken {
    /// Hand-written only because `eq` is: the comparison is constant time but
    /// still plain byte equality, so hashing the bytes stays consistent with
    /// it.
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

impl fmt::Display for SessionToken {
    /// Four bytes, because a token in a log file is a token leaked.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let b = &self.0;
        write!(f, "{:02x}{:02x}{:02x}{:02x}...", b[0], b[1], b[2], b[3])
    }
}

impl fmt::Debug for SessionToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SessionToken({self})")
    }
}

/// What a server tells a client about the media stream it is about to receive.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct UdpBinding {
    pub ssrc: Ssrc,
    pub payload_type: u8,
    pub clock_rate: u32,
}

/// `Ssrc` lives in `rtp.rs` and stays free of serde; the control plane is the
/// only thing that puts one in a structured message.
mod ssrc_serde {
    use serde::{Deserialize, Deserializer, Serializer};

    use crate::rtp::Ssrc;

    pub fn serialize<S: Serializer>(value: &Ssrc, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u32(value.0)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Ssrc, D::Error> {
        u32::deserialize(deserializer).map(Ssrc)
    }
}

/// The control vocabulary. Eight messages, and none of them carries media.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ControlMessage {
    ClientHello {
        protocol_version: u16,
        client_name: String,
    },
    ServerHello {
        protocol_version: u16,
        session_token: SessionToken,
        server_name: String,
    },
    UdpBind {
        session_token: SessionToken,
    },
    UdpBindAck {
        #[serde(with = "ssrc_serde")]
        ssrc: Ssrc,
        payload_type: u8,
        clock_rate: u32,
    },
    StartStream {
        width: u32,
        height: u32,
        fps: u32,
    },
    /// The codec configuration the media stream will actually use.
    ///
    /// Parameter sets belong on the wire because they belong to the encoder
    /// that produced the stream. A decoder configured from anything else -
    /// a fixture encoded elsewhere, a remembered blob - is describing a
    /// different stream, and rejects real slices as corrupt data.
    ///
    /// `generation` exists before anything can change it on purpose. When a
    /// resolution or a codec does change, frames of the old configuration are
    /// still in flight, and a receiver that cannot tell which configuration a
    /// frame belongs to can only guess.
    VideoConfig {
        generation: u32,
        codec: VideoCodec,
        width: u16,
        height: u16,
        /// Annex-B payloads, start codes removed.
        sps: Vec<u8>,
        pps: Vec<u8>,
    },
    /// The receiver has a decoder for that generation and will accept media.
    ConfigAck {
        generation: u32,
    },
    StopStream,
    Ping {
        nonce: u64,
    },
    Pong {
        nonce: u64,
    },
    /// The receiver asking the sender to move its capture tick within the
    /// period, without changing the rate.
    ///
    /// The largest term in this pipeline's latency is not work, it is the wait
    /// between a frame being ready and the viewer's display next being willing
    /// to show it. With two unsynchronised 120 Hz clocks that wait averages half
    /// a refresh period whatever the software does, and it disappears if the
    /// sender produces each frame just before the receiver can present it.
    ///
    /// A delay only, never an advance. Asking a sender to move a tick earlier
    /// asks it for a frame it has already produced, while delaying by a period
    /// minus the amount is the same phase and always in the future, so the
    /// signed version buys nothing and costs a class of bug.
    PhaseShift {
        /// Nanoseconds to hold the next capture tick back by. Less than one
        /// period; a sender that receives more is entitled to take it modulo the
        /// period rather than to stall.
        delay_nanos: u32,
    },
}

impl ControlMessage {
    pub const CLIENT_HELLO: u16 = 1;
    pub const SERVER_HELLO: u16 = 2;
    pub const UDP_BIND: u16 = 3;
    pub const UDP_BIND_ACK: u16 = 4;
    pub const START_STREAM: u16 = 5;
    pub const STOP_STREAM: u16 = 6;
    pub const PING: u16 = 7;
    pub const PONG: u16 = 8;
    pub const VIDEO_CONFIG: u16 = 9;
    pub const CONFIG_ACK: u16 = 10;
    pub const PHASE_SHIFT: u16 = 11;

    pub const fn message_type(&self) -> u16 {
        match self {
            ControlMessage::ClientHello { .. } => Self::CLIENT_HELLO,
            ControlMessage::ServerHello { .. } => Self::SERVER_HELLO,
            ControlMessage::UdpBind { .. } => Self::UDP_BIND,
            ControlMessage::UdpBindAck { .. } => Self::UDP_BIND_ACK,
            ControlMessage::StartStream { .. } => Self::START_STREAM,
            ControlMessage::StopStream => Self::STOP_STREAM,
            ControlMessage::Ping { .. } => Self::PING,
            ControlMessage::Pong { .. } => Self::PONG,
            ControlMessage::VideoConfig { .. } => Self::VIDEO_CONFIG,
            ControlMessage::ConfigAck { .. } => Self::CONFIG_ACK,
            ControlMessage::PhaseShift { .. } => Self::PHASE_SHIFT,
        }
    }

    /// Serialises the payload and wraps it in a frame.
    ///
    /// The payload codec is JSON, which is the boring choice: the framing is
    /// what has to be right, and replacing the codec later touches this
    /// function and [`ControlMessage::decode_parts`] and nothing else.
    pub fn encode(&self) -> io::Result<ControlFrame> {
        let payload =
            serde_json::to_vec(self).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        if payload.len() > MAX_CONTROL_PAYLOAD {
            return Err(ControlError::PayloadTooLarge(payload.len() as u32).into());
        }
        Ok(ControlFrame {
            version: CONTROL_VERSION,
            message_type: self.message_type(),
            payload,
        })
    }

    pub fn decode(frame: &ControlFrame) -> io::Result<ControlMessage> {
        Self::decode_parts(frame.version, frame.message_type, &frame.payload)
    }

    fn decode_parts(version: u16, message_type: u16, payload: &[u8]) -> io::Result<ControlMessage> {
        if version != CONTROL_VERSION {
            return Err(ControlError::UnsupportedVersion(version).into());
        }
        let message: ControlMessage = serde_json::from_slice(payload)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        // The discriminant is redundant with the payload only as long as they
        // agree; if they ever do not, a router that dispatched on the header
        // and a handler that matched on the payload would disagree too.
        if message.message_type() != message_type {
            return Err(ControlError::MessageTypeMismatch {
                declared: message_type,
                decoded: message.message_type(),
            }
            .into());
        }
        Ok(message)
    }
}

/// One framed control message.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ControlFrame {
    pub version: u16,
    pub message_type: u16,
    pub payload: Vec<u8>,
}

impl ControlFrame {
    pub fn write(&self, writer: &mut impl Write) -> io::Result<()> {
        if self.payload.len() > MAX_CONTROL_PAYLOAD {
            return Err(ControlError::PayloadTooLarge(self.payload.len() as u32).into());
        }
        let mut header = [0u8; FRAME_HEADER_LEN];
        header[0..4].copy_from_slice(&CONTROL_MAGIC.to_be_bytes());
        header[4..6].copy_from_slice(&self.version.to_be_bytes());
        header[6..8].copy_from_slice(&self.message_type.to_be_bytes());
        header[8..12].copy_from_slice(&(self.payload.len() as u32).to_be_bytes());
        writer.write_all(&header)?;
        writer.write_all(&self.payload)?;
        writer.flush()
    }

    /// Blocking read of exactly one frame.
    ///
    /// The header is validated in full before the payload buffer exists, so a
    /// declared length of 100 MB costs twelve bytes and an error.
    pub fn read(reader: &mut impl Read) -> io::Result<ControlFrame> {
        let mut header = [0u8; FRAME_HEADER_LEN];
        reader.read_exact(&mut header)?;
        let (version, message_type, payload_len) = parse_header(&header)?;
        let mut payload = vec![0u8; payload_len];
        reader.read_exact(&mut payload)?;
        Ok(ControlFrame {
            version,
            message_type,
            payload,
        })
    }
}

fn parse_header(header: &[u8; FRAME_HEADER_LEN]) -> Result<(u16, u16, usize), ControlError> {
    let magic = u32::from_be_bytes([header[0], header[1], header[2], header[3]]);
    if magic != CONTROL_MAGIC {
        return Err(ControlError::BadMagic(magic));
    }
    let version = u16::from_be_bytes([header[4], header[5]]);
    if version != CONTROL_VERSION {
        return Err(ControlError::UnsupportedVersion(version));
    }
    let message_type = u16::from_be_bytes([header[6], header[7]]);
    let payload_len = u32::from_be_bytes([header[8], header[9], header[10], header[11]]);
    if payload_len as usize > MAX_CONTROL_PAYLOAD {
        return Err(ControlError::PayloadTooLarge(payload_len));
    }
    Ok((version, message_type, payload_len as usize))
}

fn is_timeout(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

/// `SO_RCVTIMEO` of zero means "block forever" to the kernel, which is exactly
/// the opposite of what a caller passing zero wants.
fn timeout_duration(timeout: Nanos) -> Duration {
    Duration::from_nanos(timeout.get().max(1_000))
}

fn remaining_until(deadline: Timestamp) -> Nanos {
    deadline.since(Timestamp::now()).unwrap_or(Nanos::ZERO)
}

/// A resumable frame reader.
///
/// A read timeout can fire after half a header has arrived. Restarting the
/// parse on the next call would resynchronise onto the middle of a message and
/// then reject it as bad magic, so the partial state lives here instead. The
/// payload buffer is reused and never exceeds [`MAX_CONTROL_PAYLOAD`].
#[derive(Debug)]
struct FrameReader {
    header: [u8; FRAME_HEADER_LEN],
    payload: Vec<u8>,
    filled: usize,
    expected: usize,
    version: u16,
    message_type: u16,
    in_payload: bool,
}

struct FrameView<'a> {
    version: u16,
    message_type: u16,
    payload: &'a [u8],
}

impl FrameReader {
    fn new() -> Self {
        FrameReader {
            header: [0u8; FRAME_HEADER_LEN],
            payload: Vec::new(),
            filled: 0,
            expected: 0,
            version: 0,
            message_type: 0,
            in_payload: false,
        }
    }

    /// `Ok(None)` means the socket timed out part-way through; call again.
    fn poll<R: Read>(&mut self, reader: &mut R) -> io::Result<Option<FrameView<'_>>> {
        if !self.in_payload {
            while self.filled < FRAME_HEADER_LEN {
                match reader.read(&mut self.header[self.filled..]) {
                    Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                    Ok(n) => self.filled += n,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(e) if is_timeout(&e) => return Ok(None),
                    Err(e) => return Err(e),
                }
            }
            let (version, message_type, payload_len) = parse_header(&self.header)?;
            self.version = version;
            self.message_type = message_type;
            self.expected = payload_len;
            self.payload.clear();
            self.payload.resize(payload_len, 0);
            self.filled = 0;
            self.in_payload = true;
        }
        while self.filled < self.expected {
            match reader.read(&mut self.payload[self.filled..]) {
                Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Ok(n) => self.filled += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) if is_timeout(&e) => return Ok(None),
                Err(e) => return Err(e),
            }
        }
        self.in_payload = false;
        self.filled = 0;
        Ok(Some(FrameView {
            version: self.version,
            message_type: self.message_type,
            payload: &self.payload[..self.expected],
        }))
    }
}

/// Session token -> the UDP source address bound to it, if any.
type SessionTable = Arc<Mutex<HashMap<SessionToken, Option<SocketAddr>>>>;

/// Accepts control connections and owns the session table.
///
/// `&ControlServer` is `Sync`, so the accept loop and whatever else needs to
/// consult the table can share one. Note the emphasis on *consult*: see
/// [`ControlServer::udp_peer`].
#[derive(Debug)]
pub struct ControlServer {
    listener: TcpListener,
    sessions: SessionTable,
    server_name: String,
}

impl ControlServer {
    pub fn bind(addr: impl ToSocketAddrs, server_name: &str) -> io::Result<ControlServer> {
        let listener = TcpListener::bind(addr)?;
        // Accept has to honour a caller's timeout, and the blocking accept has
        // no timeout of its own.
        listener.set_nonblocking(true)?;
        Ok(ControlServer {
            listener,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            server_name: server_name.to_string(),
        })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    /// Accepts one connection and completes ClientHello/ServerHello within
    /// `timeout`, issuing the session token.
    ///
    /// The whole exchange shares one deadline: a peer that connects and then
    /// says nothing cannot hold the accepting thread past `timeout`.
    pub fn accept_session(&self, timeout: Nanos) -> io::Result<ControlSession> {
        let deadline = Timestamp::now().add(timeout);
        let (mut stream, peer) = self.accept_before(deadline)?;
        // BSD accept semantics on inheritance of O_NONBLOCK vary; say it.
        stream.set_nonblocking(false)?;
        stream.set_nodelay(true)?;
        let budget = timeout_duration(remaining_until(deadline));
        stream.set_read_timeout(Some(budget))?;
        stream.set_write_timeout(Some(budget))?;

        let frame = ControlFrame::read(&mut stream)?;
        let client_name = match ControlMessage::decode(&frame)? {
            ControlMessage::ClientHello {
                protocol_version,
                client_name,
            } => {
                if protocol_version != PROTOCOL_VERSION {
                    return Err(ControlError::UnsupportedVersion(protocol_version).into());
                }
                client_name
            }
            other => {
                return Err(ControlError::UnexpectedMessage {
                    expected: ControlMessage::CLIENT_HELLO,
                    received: other.message_type(),
                }
                .into());
            }
        };

        let token = {
            let mut sessions = self.sessions.lock();
            if sessions.len() >= MAX_SESSIONS {
                return Err(ControlError::TooManySessions.into());
            }
            let token = SessionToken::generate();
            sessions.insert(token, None);
            token
        };

        let hello = ControlMessage::ServerHello {
            protocol_version: PROTOCOL_VERSION,
            session_token: token,
            server_name: self.server_name.clone(),
        };
        hello.encode()?.write(&mut stream)?;

        let default = timeout_duration(DEFAULT_IO_TIMEOUT);
        stream.set_read_timeout(Some(default))?;
        stream.set_write_timeout(Some(default))?;

        Ok(ControlSession {
            stream,
            reader: FrameReader::new(),
            peer,
            token,
            client_name,
            sessions: Arc::clone(&self.sessions),
        })
    }

    fn accept_before(&self, deadline: Timestamp) -> io::Result<(TcpStream, SocketAddr)> {
        loop {
            match self.listener.accept() {
                Ok(accepted) => return Ok(accepted),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    if Timestamp::now() >= deadline {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "no control connection arrived before the deadline",
                        ));
                    }
                    std::thread::sleep(ACCEPT_POLL);
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }

    /// Associates a UDP source address with a session token.
    ///
    /// Returns `false` for a token that was never issued, for one whose
    /// session has ended, and for one that is already bound. Re-binding is
    /// refused rather than overwritten: a second address claiming an existing
    /// token is either a stale client or an off-path attacker, and neither
    /// should be able to redirect a stream.
    pub fn bind_udp_peer(&self, token: SessionToken, addr: SocketAddr) -> bool {
        let mut sessions = self.sessions.lock();
        match sessions.get_mut(&token) {
            Some(slot) if slot.is_none() => {
                *slot = Some(addr);
                true
            }
            _ => false,
        }
    }

    /// The address bound to `token`, if any.
    ///
    /// **The media loop must not call this per packet.** It takes a mutex that
    /// a control thread also takes, and a control thread that is descheduled
    /// while holding it would stall the send path — which is the one coupling
    /// this whole module exists to avoid. Call it once when the stream starts,
    /// copy the `SocketAddr` into the sender, and never look at it again for
    /// the life of the stream.
    pub fn udp_peer(&self, token: SessionToken) -> Option<SocketAddr> {
        *self.sessions.lock().get(&token)?
    }

    pub fn session_count(&self) -> usize {
        self.sessions.lock().len()
    }
}

/// A negotiated control connection.
///
/// After the handshake this is an ordinary blocking socket with timeouts on
/// both directions. It is deliberately not a thread, a channel or a callback:
/// the owner polls it from wherever it likes, and nothing else in the process
/// waits on it.
#[derive(Debug)]
pub struct ControlSession {
    stream: TcpStream,
    reader: FrameReader,
    peer: SocketAddr,
    token: SessionToken,
    client_name: String,
    sessions: SessionTable,
}

impl ControlSession {
    pub fn token(&self) -> SessionToken {
        self.token
    }

    pub fn peer_addr(&self) -> SocketAddr {
        self.peer
    }

    pub fn client_name(&self) -> &str {
        &self.client_name
    }

    /// Waits up to `timeout` for one message.
    ///
    /// `Ok(None)` is a timeout, not a failure. A polling caller wants to check
    /// its own shutdown flag between attempts, and making that path an `Err`
    /// would force it to distinguish "nothing yet" from "connection broken" by
    /// inspecting an error kind.
    pub fn next_message(&mut self, timeout: Nanos) -> io::Result<Option<ControlMessage>> {
        self.stream
            .set_read_timeout(Some(timeout_duration(timeout)))?;
        match self.reader.poll(&mut self.stream)? {
            Some(frame) => {
                ControlMessage::decode_parts(frame.version, frame.message_type, frame.payload)
                    .map(Some)
            }
            None => Ok(None),
        }
    }

    pub fn send(&mut self, message: &ControlMessage) -> io::Result<()> {
        message.encode()?.write(&mut self.stream)
    }

    pub fn set_write_timeout(&self, timeout: Nanos) -> io::Result<()> {
        self.stream
            .set_write_timeout(Some(timeout_duration(timeout)))
    }
}

impl Drop for ControlSession {
    /// Releases the token so the table stays bounded by live sessions rather
    /// than by lifetime connection count.
    fn drop(&mut self) {
        self.sessions.lock().remove(&self.token);
    }
}

/// The client half of the handshake.
#[derive(Debug)]
pub struct ControlClient {
    stream: TcpStream,
    token: Option<SessionToken>,
    server_name: String,
    client_name: Option<String>,
}

impl ControlClient {
    pub fn connect(addr: SocketAddr, timeout: Nanos) -> io::Result<ControlClient> {
        let budget = timeout_duration(timeout);
        let stream = TcpStream::connect_timeout(&addr, budget)?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(budget))?;
        stream.set_write_timeout(Some(budget))?;
        Ok(ControlClient {
            stream,
            token: None,
            server_name: String::new(),
            client_name: None,
        })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.stream.local_addr()
    }

    pub fn token(&self) -> Option<SessionToken> {
        self.token
    }

    pub fn server_name(&self) -> &str {
        &self.server_name
    }
    pub fn client_name(&self) -> Option<&str> {
        self.client_name.as_deref()
    }

    pub fn set_timeout(&self, timeout: Nanos) -> io::Result<()> {
        let budget = timeout_duration(timeout);
        self.stream.set_read_timeout(Some(budget))?;
        self.stream.set_write_timeout(Some(budget))
    }

    /// ClientHello -> ServerHello. Returns the issued session token.
    pub fn hello(&mut self, client_name: &str) -> io::Result<SessionToken> {
        self.client_name = Some(client_name.to_owned());
        self.send(&ControlMessage::ClientHello {
            protocol_version: PROTOCOL_VERSION,
            client_name: client_name.to_string(),
        })?;
        match self.expect(ControlMessage::SERVER_HELLO)? {
            ControlMessage::ServerHello {
                protocol_version,
                session_token,
                server_name,
            } => {
                if protocol_version != PROTOCOL_VERSION {
                    return Err(ControlError::UnsupportedVersion(protocol_version).into());
                }
                self.token = Some(session_token);
                self.server_name = server_name;
                Ok(session_token)
            }
            other => Err(ControlError::UnexpectedMessage {
                expected: ControlMessage::SERVER_HELLO,
                received: other.message_type(),
            }
            .into()),
        }
    }

    /// Claims a UDP source address for this session and learns the stream's
    /// RTP identity.
    pub fn bind_udp(&mut self) -> io::Result<UdpBinding> {
        let session_token = self.token.ok_or(ControlError::NoSession)?;
        self.send(&ControlMessage::UdpBind { session_token })?;
        match self.expect(ControlMessage::UDP_BIND_ACK)? {
            ControlMessage::UdpBindAck {
                ssrc,
                payload_type,
                clock_rate,
            } => Ok(UdpBinding {
                ssrc,
                payload_type,
                clock_rate,
            }),
            other => Err(ControlError::UnexpectedMessage {
                expected: ControlMessage::UDP_BIND_ACK,
                received: other.message_type(),
            }
            .into()),
        }
    }

    /// Fire and forget: the stream itself is the acknowledgement.
    pub fn start_stream(&mut self, width: u32, height: u32, fps: u32) -> io::Result<()> {
        self.send(&ControlMessage::StartStream { width, height, fps })
    }

    pub fn stop_stream(&mut self) -> io::Result<()> {
        self.send(&ControlMessage::StopStream)
    }

    /// Replaces the control connection only after a new hello succeeds.
    ///
    /// Keeping the old stream until the replacement is negotiated prevents a
    /// transient reconnect failure from erasing a session that is still alive.
    pub fn reconnect(&mut self, addr: SocketAddr, timeout: Nanos) -> io::Result<()> {
        let client_name = self.client_name.clone().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotConnected,
                "cannot reconnect before ClientHello",
            )
        })?;
        let mut replacement = Self::connect(addr, timeout)?;
        replacement.hello(&client_name)?;
        let old = std::mem::replace(self, replacement);
        let _ = old.stream.shutdown(Shutdown::Both);
        Ok(())
    }

    /// Sends the control teardown before closing the socket.
    pub fn teardown(mut self) -> io::Result<()> {
        let stop = self.stop_stream();
        let close = self.stream.shutdown(Shutdown::Both);
        stop.and(close)
    }

    /// Round trip over the control connection.
    ///
    /// This measures TCP, not the media path, and is only useful as a coarse
    /// liveness and reachability check.
    pub fn ping(&mut self) -> io::Result<Nanos> {
        let nonce = random_u64();
        let sent = Timestamp::now();
        self.send(&ControlMessage::Ping { nonce })?;
        match self.expect(ControlMessage::PONG)? {
            ControlMessage::Pong { nonce: echoed } if echoed == nonce => {
                Ok(Timestamp::now().saturating_since(sent))
            }
            ControlMessage::Pong { .. } => Err(ControlError::NonceMismatch.into()),
            other => Err(ControlError::UnexpectedMessage {
                expected: ControlMessage::PONG,
                received: other.message_type(),
            }
            .into()),
        }
    }

    pub fn send(&mut self, message: &ControlMessage) -> io::Result<()> {
        message.encode()?.write(&mut self.stream)
    }

    pub fn recv(&mut self) -> io::Result<ControlMessage> {
        let frame = ControlFrame::read(&mut self.stream)?;
        ControlMessage::decode(&frame)
    }

    fn expect(&mut self, message_type: u16) -> io::Result<ControlMessage> {
        let message = self.recv()?;
        if message.message_type() != message_type {
            return Err(ControlError::UnexpectedMessage {
                expected: message_type,
                received: message.message_type(),
            }
            .into());
        }
        Ok(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_message_round_trips_through_its_frame() {
        let messages = [
            ControlMessage::ClientHello {
                protocol_version: PROTOCOL_VERSION,
                client_name: "mac".into(),
            },
            ControlMessage::ServerHello {
                protocol_version: PROTOCOL_VERSION,
                session_token: SessionToken::from_bytes([7u8; 16]),
                server_name: "pc".into(),
            },
            ControlMessage::UdpBind {
                session_token: SessionToken::from_bytes([9u8; 16]),
            },
            ControlMessage::UdpBindAck {
                ssrc: Ssrc(0xDEAD_BEEF),
                payload_type: 96,
                clock_rate: 90_000,
            },
            ControlMessage::StartStream {
                width: 1920,
                height: 1080,
                fps: 120,
            },
            ControlMessage::StopStream,
            ControlMessage::Ping { nonce: u64::MAX },
            ControlMessage::Pong { nonce: 0 },
            ControlMessage::PhaseShift {
                delay_nanos: 8_333_333,
            },
        ];
        for message in messages {
            let mut buffer = Vec::new();
            message.encode().unwrap().write(&mut buffer).unwrap();
            let frame = ControlFrame::read(&mut buffer.as_slice()).unwrap();
            assert_eq!(frame.message_type, message.message_type());
            assert_eq!(ControlMessage::decode(&frame).unwrap(), message);
        }
    }

    #[test]
    fn a_declared_type_that_contradicts_the_payload_is_rejected() {
        let mut frame = ControlMessage::StopStream.encode().unwrap();
        frame.message_type = ControlMessage::PING;
        let error = ControlMessage::decode(&frame).unwrap_err();
        assert_eq!(
            error
                .get_ref()
                .and_then(|e| e.downcast_ref::<ControlError>()),
            Some(&ControlError::MessageTypeMismatch {
                declared: ControlMessage::PING,
                decoded: ControlMessage::STOP_STREAM,
            })
        );
    }

    #[test]
    fn tokens_compare_by_value_and_display_only_a_prefix() {
        let token = SessionToken::from_bytes([
            0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0, 0, 0, 0, 0, 0, 0, 0,
        ]);
        assert_eq!(token, SessionToken::from_bytes(*token.as_bytes()));

        let mut other = *token.as_bytes();
        other[15] ^= 1;
        assert_ne!(token, SessionToken::from_bytes(other));

        let shown = token.to_string();
        assert_eq!(shown, "01234567...");
        assert!(!shown.contains("89ab"));
    }

    #[test]
    fn a_frame_reader_resumes_across_a_split_header() {
        // A socket read can return a header in pieces; restarting the parse
        // would resynchronise onto the middle of a message.
        let mut wire = Vec::new();
        ControlMessage::StopStream
            .encode()
            .unwrap()
            .write(&mut wire)
            .unwrap();

        struct Dribble<'a> {
            bytes: &'a [u8],
        }
        impl Read for Dribble<'_> {
            fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
                if self.bytes.is_empty() {
                    return Err(io::ErrorKind::WouldBlock.into());
                }
                out[0] = self.bytes[0];
                self.bytes = &self.bytes[1..];
                Ok(1)
            }
        }

        let mut reader = FrameReader::new();
        for split in 1..wire.len() {
            let mut first = Dribble {
                bytes: &wire[..split],
            };
            assert!(reader.poll(&mut first).unwrap().is_none());
            let mut rest = Dribble {
                bytes: &wire[split..],
            };
            let frame = reader.poll(&mut rest).unwrap().expect("frame completes");
            assert_eq!(frame.message_type, ControlMessage::STOP_STREAM);
        }
    }
}
