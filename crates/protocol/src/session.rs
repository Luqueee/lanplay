//! Session lifecycle vocabulary shared by the product shell and both peers.
//!
//! The machine owns no sockets, timers or platform objects. Those mechanisms
//! report events here; an invalid transition is rejected rather than silently
//! turning a failed session into a streaming one.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SessionState {
    Disconnected,
    Connecting,
    Negotiating,
    Starting,
    Streaming,
    Reconnecting,
    Stopping,
    Failed,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SessionEvent {
    ConnectRequested,
    ConnectionEstablished,
    NegotiationAccepted,
    StartAccepted,
    MediaReady,
    ConnectionLost,
    ReconnectRequested,
    StopRequested,
    Stopped,
    RetryExhausted,
    Recovered,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TransitionError {
    Invalid {
        state: SessionState,
        event: SessionEvent,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SessionMachine {
    state: SessionState,
    generation: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SessionTimeouts {
    pub connecting: std::time::Duration,
    pub negotiating: std::time::Duration,
    pub starting: std::time::Duration,
    pub streaming_idle: std::time::Duration,
    pub reconnecting: std::time::Duration,
}

impl Default for SessionTimeouts {
    fn default() -> Self {
        Self {
            connecting: std::time::Duration::from_secs(60),
            negotiating: std::time::Duration::from_secs(10),
            starting: std::time::Duration::from_secs(10),
            streaming_idle: std::time::Duration::from_secs(2),
            reconnecting: std::time::Duration::from_secs(30),
        }
    }
}

impl SessionTimeouts {
    pub const fn for_state(self, state: SessionState) -> Option<std::time::Duration> {
        match state {
            SessionState::Connecting => Some(self.connecting),
            SessionState::Negotiating => Some(self.negotiating),
            SessionState::Starting => Some(self.starting),
            SessionState::Streaming => Some(self.streaming_idle),
            SessionState::Reconnecting => Some(self.reconnecting),
            SessionState::Disconnected | SessionState::Stopping | SessionState::Failed => None,
        }
    }
}

impl SessionMachine {
    pub const fn new() -> Self {
        Self {
            state: SessionState::Disconnected,
            generation: 0,
        }
    }
    pub const fn state(self) -> SessionState {
        self.state
    }
    pub const fn generation(self) -> u32 {
        self.generation
    }

    pub const fn accepts_generation(self, generation: u32) -> bool {
        generation != 0 && generation == self.generation
    }
    pub fn apply(&mut self, event: SessionEvent) -> Result<SessionState, TransitionError> {
        let next = match (self.state, event) {
            (SessionState::Disconnected, SessionEvent::ConnectRequested) => {
                self.generation = self.generation.wrapping_add(1).max(1);
                SessionState::Connecting
            }
            (SessionState::Connecting, SessionEvent::ConnectionEstablished) => {
                SessionState::Negotiating
            }
            (SessionState::Negotiating, SessionEvent::NegotiationAccepted) => {
                SessionState::Starting
            }
            (SessionState::Starting, SessionEvent::StartAccepted) => SessionState::Streaming,
            (SessionState::Starting, SessionEvent::MediaReady) => SessionState::Streaming,
            (SessionState::Streaming, SessionEvent::ConnectionLost) => SessionState::Reconnecting,
            (SessionState::Reconnecting, SessionEvent::ReconnectRequested) => {
                self.generation = self.generation.wrapping_add(1).max(1);
                SessionState::Connecting
            }
            (SessionState::Reconnecting, SessionEvent::Recovered) => SessionState::Streaming,
            (SessionState::Connecting, SessionEvent::RetryExhausted)
            | (SessionState::Negotiating, SessionEvent::RetryExhausted)
            | (SessionState::Starting, SessionEvent::RetryExhausted)
            | (SessionState::Reconnecting, SessionEvent::RetryExhausted) => SessionState::Failed,
            (SessionState::Connecting, SessionEvent::StopRequested)
            | (SessionState::Negotiating, SessionEvent::StopRequested)
            | (SessionState::Starting, SessionEvent::StopRequested)
            | (SessionState::Streaming, SessionEvent::StopRequested)
            | (SessionState::Reconnecting, SessionEvent::StopRequested)
            | (SessionState::Failed, SessionEvent::StopRequested) => SessionState::Stopping,
            (SessionState::Stopping, SessionEvent::Stopped) => SessionState::Disconnected,
            _ => {
                return Err(TransitionError::Invalid {
                    state: self.state,
                    event,
                });
            }
        };
        self.state = next;
        Ok(next)
    }
}

impl Default for SessionMachine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_reaches_streaming_only_after_negotiation_and_start() {
        let mut machine = SessionMachine::new();
        for event in [
            SessionEvent::ConnectRequested,
            SessionEvent::ConnectionEstablished,
            SessionEvent::NegotiationAccepted,
            SessionEvent::StartAccepted,
        ] {
            machine.apply(event).expect("valid session transition");
        }
        assert_eq!(machine.state(), SessionState::Streaming);
    }

    #[test]
    fn a_lost_stream_reconnects_without_becoming_streaming_early() {
        let mut machine = SessionMachine::new();
        for event in [
            SessionEvent::ConnectRequested,
            SessionEvent::ConnectionEstablished,
            SessionEvent::NegotiationAccepted,
            SessionEvent::StartAccepted,
            SessionEvent::ConnectionLost,
            SessionEvent::ReconnectRequested,
        ] {
            machine.apply(event).expect("valid session transition");
        }
        assert_eq!(machine.state(), SessionState::Connecting);
        assert!(machine.apply(SessionEvent::MediaReady).is_err());
    }

    #[test]
    fn an_invalid_transition_is_named_and_does_not_change_state() {
        let mut machine = SessionMachine::new();
        let error = machine
            .apply(SessionEvent::MediaReady)
            .expect_err("media cannot arrive before a session exists");
        assert_eq!(
            error,
            TransitionError::Invalid {
                state: SessionState::Disconnected,
                event: SessionEvent::MediaReady,
            }
        );
        assert_eq!(machine.state(), SessionState::Disconnected);
    }

    #[test]
    fn every_active_state_can_teardown_to_disconnected() {
        for start in [
            SessionState::Connecting,
            SessionState::Negotiating,
            SessionState::Starting,
            SessionState::Streaming,
            SessionState::Reconnecting,
            SessionState::Failed,
        ] {
            let mut machine = SessionMachine {
                state: start,
                generation: 1,
            };
            machine
                .apply(SessionEvent::StopRequested)
                .expect("active state can stop");
            assert_eq!(
                machine.apply(SessionEvent::Stopped),
                Ok(SessionState::Disconnected)
            );
        }
    }

    #[test]
    fn reconnect_increments_generation_and_rejects_old_messages() {
        let mut machine = SessionMachine::new();
        machine
            .apply(SessionEvent::ConnectRequested)
            .expect("first connection starts");
        let first = machine.generation();
        assert!(machine.accepts_generation(first));
        machine
            .apply(SessionEvent::ConnectionEstablished)
            .expect("connection established");
        machine
            .apply(SessionEvent::NegotiationAccepted)
            .expect("negotiation accepted");
        machine
            .apply(SessionEvent::StartAccepted)
            .expect("stream started");
        machine
            .apply(SessionEvent::ConnectionLost)
            .expect("stream lost");
        machine
            .apply(SessionEvent::ReconnectRequested)
            .expect("reconnect requested");
        assert_ne!(machine.generation(), first);
        assert!(!machine.accepts_generation(first));
        assert!(machine.accepts_generation(machine.generation()));
    }

    #[test]
    fn timeout_policy_is_defined_only_for_live_states() {
        let timeouts = SessionTimeouts::default();
        assert_eq!(
            timeouts.for_state(SessionState::Streaming),
            Some(std::time::Duration::from_secs(2))
        );
        assert_eq!(timeouts.for_state(SessionState::Disconnected), None);
    }
}
