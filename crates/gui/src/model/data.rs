//! Per-request load slots. Stored state is loading / ready / error; "stale" is
//! derived — a ready result from an older scope generation stays visible but dimmed
//! until the replacement arrives.

use crate::worker::Generation;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadStatus {
    Loading,
    Ready,
    Stale,
    Error(String),
}

#[derive(Debug, Clone)]
enum SlotState<T> {
    Loading,
    Ready(T),
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct Slot<T> {
    generation: Generation,
    state: SlotState<T>,
}

impl<T> Default for Slot<T> {
    fn default() -> Self {
        Self {
            generation: 0,
            state: SlotState::Loading,
        }
    }
}

impl<T> Slot<T> {
    pub fn status(&self, current: Generation) -> LoadStatus {
        match &self.state {
            SlotState::Loading => LoadStatus::Loading,
            SlotState::Failed(message) => LoadStatus::Error(message.clone()),
            SlotState::Ready(_) if self.generation < current => LoadStatus::Stale,
            SlotState::Ready(_) => LoadStatus::Ready,
        }
    }

    pub fn value(&self) -> Option<&T> {
        match &self.state {
            SlotState::Ready(value) => Some(value),
            _ => None,
        }
    }

    pub fn generation(&self) -> Generation {
        self.generation
    }

    /// Stores the result unless it belongs to a superseded scope generation.
    pub fn absorb(
        &mut self,
        generation: Generation,
        current: Generation,
        result: Result<T, String>,
    ) {
        if generation < current {
            return;
        }
        match result {
            Ok(value) => {
                self.generation = generation;
                self.state = SlotState::Ready(value);
            }
            Err(message) => {
                self.generation = generation;
                self.state = SlotState::Failed(message);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_turns_stale_against_a_newer_generation() {
        let mut slot = Slot::default();
        slot.absorb(1, 1, Ok(10u64));
        assert_eq!(slot.status(1), LoadStatus::Ready);
        assert_eq!(slot.status(2), LoadStatus::Stale);
        assert_eq!(slot.value(), Some(&10));
    }

    #[test]
    fn stale_responses_are_dropped_quietly() {
        let mut slot = Slot::default();
        slot.absorb(2, 2, Ok(20u64));
        slot.absorb(1, 2, Ok(99u64));
        assert_eq!(slot.value(), Some(&20));
        slot.absorb(3, 2, Ok(30u64));
        assert_eq!(slot.value(), Some(&30));
    }

    #[test]
    fn errors_are_kept_with_their_generation() {
        let mut slot: Slot<u64> = Slot::default();
        slot.absorb(1, 1, Err("disk vanished".to_string()));
        assert_eq!(
            slot.status(1),
            LoadStatus::Error("disk vanished".to_string())
        );
        assert_eq!(
            slot.status(2),
            LoadStatus::Error("disk vanished".to_string())
        );
        assert_eq!(slot.generation(), 1);
    }
}
