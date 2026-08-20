//! Vocabulary shared by the Windows host and the macOS client.
//!
//! Everything here is either put on the wire or exchanged between crates that
//! sit on opposite sides of the wire. Nothing in this crate touches an OS API.

mod capabilities;
mod discovery;
mod frame;
mod negotiation;
mod report;
mod session;
mod video;

pub use capabilities::{
    ClientCapabilities, DisplayInfo, GpuInfo, GpuVendor, HostCapabilities, NvencInfo,
};
pub use frame::{FrameId, FrameIdSource};

pub use discovery::{Discovery, HostAdvertisement, SERVICE_TYPE, manual_endpoint};
pub use negotiation::{CapabilitySelection, CapabilitySet, NegotiationError, negotiate};
pub use report::{NegotiatedMode, SessionReport, SubsystemHealth};
pub use session::{
    SessionEvent, SessionMachine, SessionState, SessionTimeouts, StartupChannel,
    StartupTransaction, TransitionError,
};
pub use video::{VideoCodec, VideoMode};
