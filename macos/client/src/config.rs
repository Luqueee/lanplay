//! Learning the codec configuration from the host before decoding anything.
//!
//! A VideoToolbox format description has to be built from the parameter sets
//! of the stream it will decode. Anything else - a fixture encoded by another
//! encoder, a remembered blob - describes a different stream, and the decoder
//! reports `kVTVideoDecoderBadDataErr` on slices that are perfectly intact.
//! That failure costs every frame up to the next IDR, so it is a second of
//! video for a configuration mistake made before the first packet.
//!
//! The exchange is deliberately blocking and deliberately before the media:
//!
//! ```text
//! client                         host
//!   |--- ClientHello ------------->|
//!   |<-- ServerHello --------------|
//!   |<-- VideoConfig(generation) --|
//!   | build decoder                |
//!   |--- ConfigAck(generation) --->|
//!   |                       IDR + media
//! ```
//!
//! No frame is sent before the acknowledgement, so nothing has to be buffered
//! while the receiver catches up. Buffering here is how a pipeline that
//! refuses queues everywhere else grows one at startup.

use std::error::Error;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use lanplay_telemetry::Nanos;
use lanplay_transport::{ControlClient, ControlMessage};
use lanplay_video_core::ParameterSets;

use crate::Cli;

/// How long to keep retrying the connection.
///
/// The host is launched by hand or by a harness after the receiver is already
/// listening, so "connection refused" is the normal first answer rather than
/// a failure.
const CONNECT_DEADLINE: Duration = Duration::from_secs(60);
const RETRY: Duration = Duration::from_millis(250);
/// Long enough to cover a host still starting its encoder.
const MESSAGE_TIMEOUT: Nanos = Nanos(60_000_000_000);

pub struct VideoConfig {
    pub generation: u32,
    pub width: u16,
    pub height: u16,
    pub sets: ParameterSets,
}

/// Connects, learns the configuration, and leaves the acknowledgement to the
/// caller: the host is told a decoder exists only once one does.
pub fn negotiate(cli: &Cli) -> Result<(VideoConfig, ControlClient), Box<dyn Error>> {
    let address: SocketAddr = cli.control.ok_or(
        "--control <host:port> is required for --transport lan: the decoder \
         cannot be configured without the host's parameter sets",
    )?;

    let deadline = Instant::now() + CONNECT_DEADLINE;
    // Printed before the first attempt, not after the connection: a harness
    // starts the host only once the receiver is ready to negotiate, and it
    // has to be able to see that moment.
    println!("control: connecting to {address}");
    let mut control = loop {
        match ControlClient::connect(address, MESSAGE_TIMEOUT) {
            Ok(client) => break client,
            Err(error) if Instant::now() < deadline => {
                let _ = error;
                std::thread::sleep(RETRY);
            }
            Err(error) => return Err(format!("no control connection to {address}: {error}").into()),
        }
    };
    control.hello("lanplay-client")?;

    let message = control.recv()?;
    let ControlMessage::VideoConfig {
        generation,
        codec,
        width,
        height,
        sps,
        pps,
    } = message
    else {
        return Err(format!(
            "expected VideoConfig, got message type {}",
            message.message_type()
        )
        .into());
    };
    if codec != lanplay_protocol::VideoCodec::H264 {
        return Err(format!("host offered {codec:?}; this client decodes H.264").into());
    }
    if sps.is_empty() || pps.is_empty() {
        return Err("host sent an empty parameter set".into());
    }

    Ok((
        VideoConfig {
            generation,
            width,
            height,
            sets: ParameterSets {
                sps: vec![sps],
                pps: vec![pps],
                nal_length_size: lanplay_transport::NAL_LENGTH_SIZE,
            },
        },
        control,
    ))
}

/// Tells the host a decoder for this generation exists and media may start.
pub fn acknowledge(control: &mut ControlClient, generation: u32) -> Result<(), Box<dyn Error>> {
    control.send(&ControlMessage::ConfigAck { generation })?;
    Ok(())
}
