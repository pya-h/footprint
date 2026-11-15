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
    /// the "[show]" answer to "untimed events hidden by the time window": drop the
    /// window and look at the untimed events (their only order is file order)
    ShowUntimed,
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
        result: Result<Option<(i64, i64)>, String>,
    },
    Untimed {
        generation: Generation,
        result: Result<u64, String>,
    },
    /// a watcher sample of the case's `data_version`
    Observed {
        version: i64,
    },
    /// the user acknowledged the "case changed" banner: the current version becomes baseline
    RefreshCase,
}

/// Consecutive `data_version` samples of the case file, from which the shell derives
/// the busy and changed banners. The GUI's own writer never raises its own
/// connection's counter, so only external writes are observed.
#[derive(Debug, Default)]
pub struct WatchState {
    baseline: Option<i64>,
    previous: Option<i64>,
    current: Option<i64>,
}

impl WatchState {
    pub fn observe(&mut self, version: i64) {
        self.previous = self.current;
        self.current = Some(version);
        if self.baseline.is_none() {
            self.baseline = Some(version);
        }
    }

    /// two consecutive samples differ: someone is writing right now
    pub fn case_writing(&self) -> bool {
        matches!((self.previous, self.current), (Some(p), Some(c)) if p != c)
    }

    /// the case settled on a version newer than the one the user last acknowledged
    pub fn case_changed(&self) -> bool {
        matches!((self.baseline, self.current), (Some(b), Some(c)) if b != c)
            && !self.case_writing()
    }

    pub fn refresh(&mut self) {
        self.baseline = self.current;
    }
}

#[derive(Debug, Default)]
pub struct Model {
    pub scope: Scope,
    pub nav: Nav,
    /// bumped on every scope change; worker responses carry it and stale ones drop
    pub generation: Generation,
    pub counts: Slot<Counts>,
    /// the case's timed timespan; `None` while the case has no timed events
    pub bounds: Slot<Option<(i64, i64)>>,
    /// untimed events matching the scope with its time window dropped; feeds the
    /// "untimed hidden by the time window" notice so they are never silently dropped
    pub untimed: Slot<u64>,
    pub watch: WatchState,
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
                    ScopeAction::ShowUntimed => {
                        self.scope.show_untimed();
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
            Action::Observed { version } => self.watch.observe(version),
            Action::RefreshCase => self.watch.refresh(),
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
