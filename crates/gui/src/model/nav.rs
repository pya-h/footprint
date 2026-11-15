//! Navigation history: a stack of (view, selection). Scope changes never appear
//! here; they have their own undo/redo.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Dashboard,
    Timeline,
    Lanes,
    Hosts,
    Trace,
    State,
    Diff,
    Ioc,
    Sources,
    Collect,
}

impl View {
    /// rail order; the index doubles as the `1…0` shortcut
    pub const ALL: [Self; 10] = [
        Self::Dashboard,
        Self::Timeline,
        Self::Lanes,
        Self::Hosts,
        Self::Trace,
        Self::State,
        Self::Diff,
        Self::Ioc,
        Self::Sources,
        Self::Collect,
    ];

    pub fn from_shortcut(index: usize) -> Option<Self> {
        Self::ALL.get(index).copied()
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Dashboard => "Dashboard",
            Self::Timeline => "Timeline",
            Self::Lanes => "Lanes",
            Self::Hosts => "Hosts",
            Self::Trace => "Trace",
            Self::State => "State",
            Self::Diff => "Diff",
            Self::Ioc => "IOC",
            Self::Sources => "Sources",
            Self::Collect => "Collect",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Selection {
    #[default]
    None,
    Event(u64),
    State(i64),
    Ioc(u64),
    Source(u64),
    TraceNode(u64),
}

const HISTORY_CAP: usize = 200;

#[derive(Debug, Clone)]
pub struct Nav {
    stack: Vec<(View, Selection)>,
    index: usize,
}

impl Default for Nav {
    fn default() -> Self {
        Self::new()
    }
}

impl Nav {
    pub fn new() -> Self {
        Self {
            stack: vec![(View::Dashboard, Selection::None)],
            index: 0,
        }
    }

    pub fn current(&self) -> (View, Selection) {
        self.stack[self.index].clone()
    }

    /// explicit navigation (rail click, shortcut, drill-in) records history
    pub fn navigate(&mut self, view: View, selection: Selection) {
        if self.stack[self.index] == (view, selection.clone()) {
            return;
        }
        self.stack.truncate(self.index + 1);
        self.stack.push((view, selection));
        if self.stack.len() > HISTORY_CAP {
            self.stack.remove(0);
        }
        self.index = self.stack.len() - 1;
    }

    /// selection moves (arrow keys, hovers that stick) update the current entry only
    pub fn select(&mut self, selection: Selection) {
        self.stack[self.index].1 = selection;
    }

    pub fn back(&mut self) -> bool {
        if self.index == 0 {
            return false;
        }
        self.index -= 1;
        true
    }

    pub fn forward(&mut self) -> bool {
        if self.index + 1 >= self.stack.len() {
            return false;
        }
        self.index += 1;
        true
    }

    pub fn can_back(&self) -> bool {
        self.index > 0
    }

    pub fn can_forward(&self) -> bool {
        self.index + 1 < self.stack.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_on_dashboard_and_navigates() {
        let mut nav = Nav::new();
        assert_eq!(nav.current(), (View::Dashboard, Selection::None));
        nav.navigate(View::Timeline, Selection::Event(7));
        assert_eq!(nav.current(), (View::Timeline, Selection::Event(7)));
        nav.select(Selection::Event(8));
        assert_eq!(nav.current().1, Selection::Event(8));
        assert!(nav.back());
        assert_eq!(nav.current(), (View::Dashboard, Selection::None));
        assert!(!nav.back());
        assert!(nav.forward());
        assert_eq!(nav.current().1, Selection::Event(8));
    }

    #[test]
    fn selection_moves_do_not_create_history_entries() {
        let mut nav = Nav::new();
        nav.navigate(View::Timeline, Selection::Event(1));
        nav.select(Selection::Event(2));
        nav.select(Selection::Event(3));
        assert!(nav.back());
        assert_eq!(nav.current(), (View::Dashboard, Selection::None));
    }

    #[test]
    fn navigating_cuts_the_forward_tail() {
        let mut nav = Nav::new();
        nav.navigate(View::Timeline, Selection::None);
        nav.navigate(View::Hosts, Selection::None);
        assert!(nav.back());
        nav.navigate(View::Ioc, Selection::None);
        assert!(!nav.can_forward());
        assert_eq!(nav.current().0, View::Ioc);
    }
}
