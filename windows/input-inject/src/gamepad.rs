//! Controller lifecycle and newest-state selection before a Windows backend sees a report.
//!
//! The backend is deliberately outside this state machine. A virtual controller can be
//! replaced without changing which network state is accepted or the neutralization that
//! precedes every destroy.

use lanplay_input_protocol::GamepadStateV1;

pub const MAX_GAMEPAD_SLOTS: usize = 4;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GamepadAction {
    Create {
        controller_slot: u8,
        session_generation: u32,
    },
    Submit(GamepadStateV1),
    Destroy {
        controller_slot: u8,
        session_generation: u32,
    },
}

/// The local Windows virtual-device boundary.
///
/// HIDMaestro is selected by the process that owns this trait; no UDP type or
/// capture code depends on its SDK, so changing backend cannot change input
/// ordering or neutralization.
pub trait VirtualGamepadBackend {
    type Error;

    fn create(&mut self, controller_slot: u8, session_generation: u32) -> Result<(), Self::Error>;
    fn submit_state(&mut self, state: GamepadStateV1) -> Result<(), Self::Error>;
    fn destroy(&mut self, controller_slot: u8, session_generation: u32) -> Result<(), Self::Error>;
}

pub fn deliver<B: VirtualGamepadBackend>(
    backend: &mut B,
    action: GamepadAction,
) -> Result<(), B::Error> {
    match action {
        GamepadAction::Create {
            controller_slot,
            session_generation,
        } => backend.create(controller_slot, session_generation),
        GamepadAction::Submit(state) => backend.submit_state(state),
        GamepadAction::Destroy {
            controller_slot,
            session_generation,
        } => backend.destroy(controller_slot, session_generation),
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GamepadOutcome {
    Applied,
    Stale,
    Unattached,
    WrongGeneration,
    InvalidSlot,
}

#[derive(Clone, Copy)]
struct Slot {
    attached: bool,
    generation: u32,
    sequence: Option<u32>,
}

impl Slot {
    const EMPTY: Self = Self {
        attached: false,
        generation: 0,
        sequence: None,
    };
}

/// The host-owned facts for virtual controllers.
///
/// State packets may arrive duplicated or reordered. The state sent after a dropped
/// packet supersedes it, so each slot retains only its largest sequence and has no queue.
pub struct GamepadHost {
    slots: [Slot; MAX_GAMEPAD_SLOTS],
    stale_states: u64,
}

impl Default for GamepadHost {
    fn default() -> Self {
        Self::new()
    }
}

impl GamepadHost {
    pub const fn new() -> Self {
        Self {
            slots: [Slot::EMPTY; MAX_GAMEPAD_SLOTS],
            stale_states: 0,
        }
    }

    pub fn attach(
        &mut self,
        controller_slot: u8,
        session_generation: u32,
        mut emit: impl FnMut(GamepadAction),
    ) -> GamepadOutcome {
        let Some(slot) = self.slots.get_mut(usize::from(controller_slot)) else {
            return GamepadOutcome::InvalidSlot;
        };
        if slot.attached && slot.generation == session_generation {
            return GamepadOutcome::Stale;
        }
        if slot.attached {
            emit(GamepadAction::Submit(GamepadStateV1::neutral_for(
                slot.generation,
                controller_slot,
                slot.sequence.unwrap_or(0).wrapping_add(1),
            )));
            emit(GamepadAction::Destroy {
                controller_slot,
                session_generation: slot.generation,
            });
        }
        *slot = Slot {
            attached: true,
            generation: session_generation,
            sequence: None,
        };
        emit(GamepadAction::Create {
            controller_slot,
            session_generation,
        });
        GamepadOutcome::Applied
    }

    pub fn submit(
        &mut self,
        state: GamepadStateV1,
        mut emit: impl FnMut(GamepadAction),
    ) -> GamepadOutcome {
        let Some(slot) = self.slots.get_mut(usize::from(state.controller_slot)) else {
            return GamepadOutcome::InvalidSlot;
        };
        if !slot.attached {
            return GamepadOutcome::Unattached;
        }
        if slot.generation != state.session_generation {
            return GamepadOutcome::WrongGeneration;
        }
        if slot
            .sequence
            .is_some_and(|last| !newer(state.sequence, last))
        {
            self.stale_states += 1;
            return GamepadOutcome::Stale;
        }
        slot.sequence = Some(state.sequence);
        emit(GamepadAction::Submit(state));
        GamepadOutcome::Applied
    }

    pub fn detach(
        &mut self,
        controller_slot: u8,
        session_generation: u32,
        emit: impl FnMut(GamepadAction),
    ) -> GamepadOutcome {
        self.neutralize(controller_slot, session_generation, emit)
    }

    /// Applies the same safety barrier after heartbeat expiry and session loss.
    pub fn neutralize_all(&mut self, mut emit: impl FnMut(GamepadAction)) {
        for controller_slot in 0..MAX_GAMEPAD_SLOTS as u8 {
            let generation = self.slots[usize::from(controller_slot)].generation;
            let _ = self.neutralize(controller_slot, generation, &mut emit);
        }
    }

    pub fn stale_states(&self) -> u64 {
        self.stale_states
    }
    pub fn attached_slots(&self) -> usize {
        self.slots.iter().filter(|slot| slot.attached).count()
    }

    fn neutralize(
        &mut self,
        controller_slot: u8,
        session_generation: u32,
        mut emit: impl FnMut(GamepadAction),
    ) -> GamepadOutcome {
        let Some(slot) = self.slots.get_mut(usize::from(controller_slot)) else {
            return GamepadOutcome::InvalidSlot;
        };
        if !slot.attached {
            return GamepadOutcome::Unattached;
        }
        if slot.generation != session_generation {
            return GamepadOutcome::WrongGeneration;
        }
        emit(GamepadAction::Submit(GamepadStateV1::neutral_for(
            session_generation,
            controller_slot,
            slot.sequence.unwrap_or(0).wrapping_add(1),
        )));
        emit(GamepadAction::Destroy {
            controller_slot,
            session_generation,
        });
        *slot = Slot::EMPTY;
        GamepadOutcome::Applied
    }
}

fn newer(candidate: u32, previous: u32) -> bool {
    let distance = candidate.wrapping_sub(previous);
    distance != 0 && distance < (1 << 31)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lanplay_input_protocol::{Dpad, GamepadStateV1};

    fn state(sequence: u32) -> GamepadStateV1 {
        GamepadStateV1 {
            session_generation: 9,
            controller_slot: 0,
            sequence,
            buttons: 1,
            dpad: Dpad::North,
            left_x: 1,
            left_y: 2,
            right_x: 3,
            right_y: 4,
            left_trigger: 5,
            right_trigger: 6,
        }
    }

    #[test]
    fn reordered_state_never_reaches_the_backend() {
        let mut host = GamepadHost::new();
        host.attach(0, 9, |_| {});
        let mut actions = Vec::new();
        assert_eq!(
            host.submit(state(502), |action| actions.push(action)),
            GamepadOutcome::Applied
        );
        assert_eq!(
            host.submit(state(501), |action| actions.push(action)),
            GamepadOutcome::Stale
        );
        assert_eq!(host.stale_states(), 1);
        assert_eq!(actions, [GamepadAction::Submit(state(502))]);
    }

    #[test]
    fn detach_submits_neutral_before_destroying_the_controller() {
        let mut host = GamepadHost::new();
        host.attach(0, 9, |_| {});
        host.submit(state(2), |_| {});
        let mut actions = Vec::new();
        assert_eq!(
            host.detach(0, 9, |action| actions.push(action)),
            GamepadOutcome::Applied
        );
        assert_eq!(
            actions,
            [
                GamepadAction::Submit(GamepadStateV1::neutral_for(9, 0, 3)),
                GamepadAction::Destroy {
                    controller_slot: 0,
                    session_generation: 9
                },
            ]
        );
    }

    #[test]
    fn a_session_generation_cannot_destroy_its_replacement() {
        let mut host = GamepadHost::new();
        host.attach(0, 9, |_| {});
        host.attach(0, 10, |_| {});
        assert_eq!(host.detach(0, 9, |_| {}), GamepadOutcome::WrongGeneration);
    }

    #[test]
    fn a_stale_generation_cannot_reenter_a_new_session() {
        let mut host = GamepadHost::new();
        host.attach(0, 9, |_| {});
        host.submit(state(1), |_| {});
        host.attach(0, 10, |_| {});
        let mut stale = state(2);
        stale.session_generation = 9;
        let mut actions = Vec::new();
        assert_eq!(
            host.submit(stale, |action| actions.push(action)),
            GamepadOutcome::WrongGeneration
        );
        assert!(actions.is_empty());
    }

    #[test]
    fn a_short_tap_is_observed_when_down_and_up_states_arrive() {
        let mut host = GamepadHost::new();
        host.attach(0, 9, |_| {});
        let mut actions = Vec::new();
        host.submit(state(1), |action| actions.push(action));
        let mut up = state(2);
        up.buttons = 0;
        host.submit(up, |action| actions.push(action));
        assert_eq!(
            actions
                .iter()
                .filter_map(|action| match action {
                    GamepadAction::Submit(state) => Some(state.buttons),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            [1, 0]
        );
    }

    #[test]
    fn a_lost_down_state_cannot_be_recovered_by_latest_state() {
        let mut host = GamepadHost::new();
        host.attach(0, 9, |_| {});
        let mut up = state(2);
        up.buttons = 0;
        let mut actions = Vec::new();
        host.submit(up, |action| actions.push(action));
        assert_eq!(actions, [GamepadAction::Submit(up)]);
        assert_eq!(host.stale_states(), 0);
    }
}
