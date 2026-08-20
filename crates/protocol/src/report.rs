use serde::{Deserialize, Serialize};

use crate::{SessionState, VideoCodec, VideoMode};

/// Bounded health counters exported with a real session diagnostic.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct SubsystemHealth {
    pub active: bool,
    pub observations: u64,
    pub faults: u64,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct NegotiatedMode {
    pub generation: u32,
    pub codec: VideoCodec,
    pub video_mode: VideoMode,
    pub audio_sample_rate: u32,
    pub audio_channels: u16,
    pub gamepad: bool,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SessionReport {
    pub generation: u32,
    pub state: SessionState,
    pub negotiated: Option<NegotiatedMode>,
    pub video: SubsystemHealth,
    pub audio: SubsystemHealth,
    pub input: SubsystemHealth,
    pub gamepad: SubsystemHealth,
    pub adaptations: Vec<String>,
    pub errors: Vec<String>,
}

impl SessionReport {
    pub fn new(generation: u32, state: SessionState) -> Self {
        Self {
            generation,
            state,
            negotiated: None,
            video: SubsystemHealth::default(),
            audio: SubsystemHealth::default(),
            input: SubsystemHealth::default(),
            gamepad: SubsystemHealth::default(),
            adaptations: Vec::new(),
            errors: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_serializes_unavailable_negotiation_distinctly() {
        let report = SessionReport::new(3, SessionState::Negotiating);
        let json = serde_json::to_string(&report).expect("report serializes");
        assert!(json.contains("\"negotiated\":null"));
        assert!(json.contains("\"generation\":3"));
    }
}
