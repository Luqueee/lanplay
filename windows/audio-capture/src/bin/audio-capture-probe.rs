//! Captures the endpoint mix in loopback and reports what arrived.
//!
//! The work lives in the library so that the accounting, the format decoding
//! and the tone detector can be tested on a machine with no audio endpoint,
//! which a binary cannot arrange for itself: a crate-level `cfg` would leave it
//! with no `main` at all.

fn main() -> std::process::ExitCode {
    lanplay_audio_capture::probe::main()
}
