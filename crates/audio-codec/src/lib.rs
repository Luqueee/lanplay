//! What Opus costs and what it produces, measured on its own before anything
//! is built on top of it.
//!
//! Audio for this project has to survive a 5 ms frame budget, and every later
//! decision — how deep the jitter buffer is, whether a packetiser can carry two
//! frames, whether the encoder can share a core with the video path — turns on
//! numbers nobody has taken yet. So this crate is two things at once: the codec
//! boundary the host and the client will each own half of, and an instrument
//! that runs both halves back to back and prints what it saw.
//!
//! The wrapper is deliberately thin and deliberately split. [`OpusEncoder`] and
//! [`OpusDecoder`] share nothing but a [`CodecConfig`] value, so the two ends
//! can live in different processes on different machines without either
//! carrying the other's code, and neither can inherit a setting the other did
//! not agree to. Both own their buffers from construction: one frame in, one
//! packet out, no allocation, no queue and no lock between them.
//!
//! libopus does the coding, through `opus-head-sys`, which vendors and builds
//! the reference C. A pure-Rust reimplementation was considered and rejected:
//! an encoder that is subtly wrong produces audio that sounds nearly right and
//! fails in ways nobody can see, and the reference implementation is what every
//! Opus decoder in the world is tested against. The cost of that decision is
//! that anything depending on this crate needs a C toolchain, and in particular
//! cannot be cross-checked for Windows from a machine with no MSVC installed.
//!
//! That crate publishes bindings and nothing above them, so the safe boundary
//! is this crate's own: the whole of the FFI lives in one private module and
//! the encoder and decoder above it are ordinary safe Rust. Which is the right
//! shape rather than a concession, because this crate already exists to be the
//! codec boundary, and the wrapper that used to sit in between brought
//! `audiopus_sys` with it — unmaintained since 2020, and vendoring a libopus
//! whose `cmake_minimum_required(VERSION 3.1)` CMake 4 refuses, so whether it
//! built at all depended on whether the machine happened to have a system
//! libopus for pkg-config to find.
//!
//! What is absent, because each belongs to a later phase: no resampler, no
//! capture, no audio device. The previous phase established that the render
//! endpoint mixes at exactly 48000 Hz stereo and hands over packets of exactly
//! 480 frames, so the path from a captured packet to an Opus frame needs no
//! conversion at all — a 480 frame packet is two 5 ms frames or one 10 ms
//! frame, and nothing is ever left over. Writing a resampler here would be
//! writing code against a problem this machine does not have.
//!
//! RTP is absent from the wrapper for a different reason and present in one
//! binary. The payload format lives in `lanplay-transport`, where it takes
//! encoded bytes and a sample count so that crate never needs libopus; but a
//! program that sends the tone over a socket and decodes what comes back needs
//! both halves at once, and it has to sit on the side that already vendors the
//! C. That is [`rtp_probe`], and it is an instrument rather than a component:
//! nothing in [`encoder`] or [`decoder`] knows a socket exists.
//!
//! The receiving path is here for the same reason the RTP probe is: a jitter
//! buffer that conceals a missing frame needs the decoder's own concealer, so
//! it has to sit on the side that vendors the C. [`jitter`] is the buffer
//! itself and holds no socket; [`jitter_probe`] is the run that drives it with
//! a synthetic sink, and nothing in [`jitter`] knows either exists.
//!
//! Discontinuous transmission and in-band forward error correction are both
//! off, and both are off on purpose. Each spends or withholds bytes according
//! to conditions this phase does not create — silence for the first, packet
//! loss for the second — and a measurement of the codec that included them
//! would be a measurement of its recovery features instead. See
//! [`encoder::OpusEncoder::new`], where each is set and each is justified.
//!
//! ```no_run
//! use lanplay_audio_codec::{CodecConfig, FrameDuration, OpusDecoder, OpusEncoder};
//!
//! let config = CodecConfig::contract(FrameDuration::Ms5, 128_000);
//! let mut encoder = OpusEncoder::new(config)?;
//! let mut decoder = OpusDecoder::new(config)?;
//!
//! let silence = vec![0f32; config.frame_interleaved()];
//! let packet = encoder.encode(&silence)?;
//! let frame = decoder.decode(packet)?;
//! assert_eq!(frame.len(), config.frame_interleaved());
//! # Ok::<(), lanplay_audio_codec::CodecError>(())
//! ```

pub mod config;
pub mod decoder;
pub mod encoder;
pub mod envelope;
pub mod error;
mod ffi;
pub mod jitter;
pub mod jitter_probe;
pub mod probe;
pub mod rtp_probe;

pub use config::{CodecConfig, FrameDuration, MAX_PACKET_BYTES, SAMPLE_RATES};
pub use decoder::OpusDecoder;
pub use encoder::{EncoderSettings, OpusEncoder};
pub use error::CodecError;
pub use ffi::ErrorCode;
