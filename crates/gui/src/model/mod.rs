//! The render-independent state machine: widgets emit [`Action`]s and
//! [`Model::apply`] is the only mutator, which is what makes the GUI testable
//! without a window.

pub mod data;
pub mod nav;
pub mod scope;

pub use data::{LoadStatus, Slot};
pub use nav::{Nav, Selection, View};
pub use scope::{Chip, ChipRemove, Scope};

use crate::worker::Generation;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    pub in_scope: u64,
    pub in_case: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ScopeAction {
    Remove(ChipRemove),
    SetText(Option<String>),
    SetWindow {
        since: Option<i64>,
        until: Option<i64>,
    },
    Clear,
}

#[derive(Debug)]
pub enum Action {
    Scope(ScopeAction),
    UndoScope,
    RedoScope,
    Go(View),
    Select(Selection),
    NavBack,
    NavForward,
    Counts {
        generation: Generation,
        result: Result<Counts, String>,
    },
    Bounds {
        generation: Generation,
        result: Result<(i64, i64), String>,
    },
    Untimed {
        generation: Generation,
        result: Result<u64, String>,
    },
}

#[derive(Debug, Default)]
pub struct Model {
    pub scope: Scope,
    pub nav: Nav,
    /// bumped on every scope change; worker responses carry it and stale ones drop
    pub generation: Generation,
    pub counts: Slot<Counts>,
    pub bounds: Slot<(i64, i64)>,
    /// untimed events matching the scope with its time window dropped; feeds the
    /// "untimed hidden by the time window" notice so they are never silently dropped
    pub untimed: Slot<u64>,
}

impl Model {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn view(&self) -> View {
        self.nav.current().0
    }

    pub fn selection(&self) -> Selection {
        self.nav.current().1
    }

    pub fn apply(&mut self, action: Action) {
        match action {
            Action::Scope(edit) => {
                let changed = match edit {
                    ScopeAction::Remove(which) => {
                        self.scope.remove(which);
                        true
                    }
                    ScopeAction::SetText(text) => {
                        self.scope.set_text(text);
                        true
                    }
                    ScopeAction::SetWindow { since, until } => {
                        self.scope.set_window(since, until);
                        true
                    }
                    ScopeAction::Clear => self.scope.clear(),
                };
                if changed {
                    self.generation += 1;
                }
            }
            Action::UndoScope => {
                if self.scope.undo() {
                    self.generation += 1;
                }
            }
            Action::RedoScope => {
                if self.scope.redo() {
                    self.generation += 1;
                }
            }
            Action::Go(view) => self.nav.navigate(view, Selection::None),
            Action::Select(selection) => self.nav.select(selection),
            Action::NavBack => {
                self.nav.back();
            }
            Action::NavForward => {
                self.nav.forward();
            }
            Action::Counts { generation, result } => {
                self.counts.absorb(generation, self.generation, result)
            }
            Action::Bounds { generation, result } => {
                self.bounds.absorb(generation, self.generation, result)
            }
            Action::Untimed { generation, result } => {
                self.untimed.absorb(generation, self.generation, result)
            }
        }
    }

    /// The count for the "untimed events hidden by the time window" notice,
    /// present only while a time window can actually hide them.
    pub fn untimed_hidden(&self) -> Option<u64> {
        let filter = self.scope.filter();
        if filter.since.is_none() && filter.until.is_none() {
            return None;
        }
        match self.untimed.value() {
            Some(&n) if n > 0 => Some(n),
            _ => None,
        }
    }
}
