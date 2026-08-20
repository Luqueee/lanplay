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
}

impl SessionMachine {
    pub const fn new() -> Self {
        Self {
            state: SessionState::Disconnected,
        }
    }

    pub const fn state(self) -> SessionState {
        self.state
    }

    pub fn apply(&mut self, event: SessionEvent) -> Result<SessionState, TransitionError> {
        let next = match (self.state, event) {
            (SessionState::Disconnected, SessionEvent::ConnectRequested) => {
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
            let mut machine = SessionMachine { state: start };
            machine
                .apply(SessionEvent::StopRequested)
                .expect("active state can stop");
            assert_eq!(
                machine.apply(SessionEvent::Stopped),
                Ok(SessionState::Disconnected)
            );
        }
    }
}
