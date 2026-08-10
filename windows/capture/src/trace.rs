//! Opt-in call-boundary tracing for driver/DXGI diagnosis.
//!
//! Set `LANPLAY_DDA_TRACE=1`. Every begin record is flushed before entering
//! the API so a watchdog-killed process still leaves the exact blocking stage.

#![cfg(windows)]

use std::io::Write as _;
use std::sync::OnceLock;

use lanplay_telemetry::Timestamp;
use windows::Win32::System::Threading::GetCurrentThreadId;

static ENABLED: OnceLock<bool> = OnceLock::new();

pub(crate) struct Span {
    stage: &'static str,
    started: Timestamp,
    enabled: bool,
}

pub(crate) fn begin(stage: &'static str, details: impl core::fmt::Display) -> Span {
    let enabled = *ENABLED
        .get_or_init(|| std::env::var_os("LANPLAY_DDA_TRACE").is_some_and(|value| value != "0"));
    let started = Timestamp::now();
    if enabled {
        eprintln!(
            "dda_trace event=begin stage={stage} qpc_ns={} thread_id={} {details}",
            started.as_nanos(),
            thread_id(),
        );
        let _ = std::io::stderr().flush();
    }
    Span {
        stage,
        started,
        enabled,
    }
}

impl Span {
    pub(crate) fn ok(self, details: impl core::fmt::Display) {
        self.finish("ok", 0, details);
    }

    pub(crate) fn error(self, hresult: i32, details: impl core::fmt::Display) {
        self.finish("error", hresult, details);
    }

    fn finish(self, outcome: &str, hresult: i32, details: impl core::fmt::Display) {
        if !self.enabled {
            return;
        }
        let ended = Timestamp::now();
        eprintln!(
            "dda_trace event=end stage={} qpc_ns={} elapsed_ns={} thread_id={} outcome={} hresult=0x{:08X} {}",
            self.stage,
            ended.as_nanos(),
            ended.as_nanos().saturating_sub(self.started.as_nanos()),
            thread_id(),
            outcome,
            hresult as u32,
            details,
        );
        let _ = std::io::stderr().flush();
    }
}

fn thread_id() -> u32 {
    // SAFETY: no arguments and no failure mode.
    unsafe { GetCurrentThreadId() }
}
