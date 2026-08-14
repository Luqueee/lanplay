//! A synthetic full-screen producer, so both capture backends are measured
//! against motion that is continuous and reproducible.
//!
//! Neither Windows.Graphics.Capture nor Desktop Duplication hands out a frame
//! when nothing changed, and both report timings that only mean something when
//! something did. Pointed at an idle desktop, a capture benchmark measures its
//! own polling loop. This crate presents a picture that is a function of the
//! frame index — every band of it differs from the previous frame — at a rate
//! the operator picks, and reports how well it kept that rate, so the capture
//! numbers can be read against a known input instead of against a game whose
//! own frame pacing is a second unknown.

pub mod pace;
pub mod report;

#[cfg(windows)]
pub mod gpu;
#[cfg(windows)]
pub mod present;
#[cfg(windows)]
pub mod window;

use core::fmt;

#[derive(Debug)]
pub enum Error {
    /// A Win32, DXGI or D3D11 call failed.
    Api { call: &'static str, hresult: i32 },
    /// The request cannot be served on this machine or this platform.
    Unsupported(String),
    /// The HLSL compiler rejected the shader, and said why.
    ShaderCompile(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Api { call, hresult } => {
                write!(f, "{call} failed: 0x{:08X}", *hresult as u32)
            }
            Error::Unsupported(why) => write!(f, "{why}"),
            Error::ShaderCompile(log) => write!(f, "shader compilation failed:\n{log}"),
        }
    }
}

impl core::error::Error for Error {}

/// Turns a `windows` error into ours while naming the call that produced it,
/// because an HRESULT without its callsite is a number nobody can act on.
#[cfg(windows)]
pub(crate) fn api(call: &'static str) -> impl FnOnce(::windows::core::Error) -> Error {
    move |error| Error::Api {
        call,
        hresult: error.code().0,
    }
}
