//! The render-independent state machine: widgets emit [`Action`]s and
//! [`Model::apply`] is the only mutator, which is what makes the GUI testable
//! without a window.

pub mod board;
pub mod data;
pub mod nav;
pub mod scope;

pub use board::{
    Board, BoardDraft, BoardExportFormat, BoardItem, SavedView, SavedViews, target_label,
    view_for_selection,
};
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
    SetKinds(Vec<String>),
    SetKindsNot(Vec<String>),
    SetKindFilter {
        kinds: Vec<String>,
        kinds_not: Vec<String>,
    },
    SetHosts(Vec<String>),
    SetHostsNot(Vec<String>),
    SetHostFilter {
        hosts: Vec<String>,
        hosts_not: Vec<String>,
    },
    SetSources(Vec<String>),
    SetSourcesNot(Vec<String>),
    SetSourceFilter {
        sources: Vec<String>,
        sources_not: Vec<String>,
    },
    SetConfidence(Vec<fp_core::Confidence>),
    SetRun(Option<u64>),
    SetTiming(fp_store::EventTiming),
    SetIocOnly(bool),
    Clear,
}

/// A mutating IOC operation queued for the worker's writer thread.
#[derive(Debug)]
pub enum IocWriteRequest {
    Add {
        ioc_type: String,
        value: String,
        note: Option<String>,
    },
    Import {
        content: String,
    },
    Remove {
        id: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmptyRecoveryHint {
    pub remove: crate::model::scope::ChipRemove,
    pub label: String,
    pub restored_count: u64,
}

#[derive(Debug)]
pub enum Action {
    Scope(ScopeAction),
    UndoScope,
    RedoScope,
    Go(View),
    Select(Selection),
    JumpTo(Selection),
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
    Histogram {
        generation: Generation,
        result: Result<fp_query::overview::Histogram, String>,
    },
    Category {
        generation: Generation,
        result: Result<[u64; fp_query::category::Category::ALL.len()], String>,
    },
    Timing {
        generation: Generation,
        result: Result<fp_query::overview::TimingCounts, String>,
    },
    Facets {
        facet: fp_query::overview::Facet,
        generation: Generation,
        result: Result<fp_store::FacetPage, String>,
    },
    DetailHistogram {
        generation: Generation,
        result: Result<fp_query::overview::Histogram, String>,
    },
    IocHistogram {
        generation: Generation,
        result: Result<fp_query::overview::Histogram, String>,
    },
    Runs {
        generation: Generation,
        result: Result<Vec<crate::vm::RunMarkerVm>, String>,
    },
    IocCount {
        generation: Generation,
        result: Result<u64, String>,
    },
    Detail {
        generation: Generation,
        result: Result<Option<Box<crate::vm::DetailVm>>, String>,
    },
    Around {
        generation: Generation,
        result: Result<Vec<crate::vm::RowVm>, String>,
    },
    Groups {
        generation: Generation,
        result: Result<Vec<crate::vm::GroupRowVm>, String>,
    },
    DashboardSummary {
        generation: Generation,
        result: Result<crate::vm::DashboardSummaryVm, String>,
    },
    DashboardHosts {
        generation: Generation,
        result: Result<Vec<crate::vm::HostSummaryVm>, String>,
    },
    Hosts {
        generation: Generation,
        result: Result<Vec<crate::vm::HostSummaryVm>, String>,
    },
    Lanes {
        generation: Generation,
        result: Result<crate::vm::LanesDataVm, String>,
    },
    SetHostSort(fp_query::overview::HostSort),
    RequestHostsQuery,
    SetLanesMode(fp_query::overview::LaneMode),
    AddIoc {
        ioc_type: String,
        value: String,
        note: Option<String>,
    },
    EmptyRecovery {
        generation: Generation,
        hints: Vec<EmptyRecoveryHint>,
    },
    /// import one IOC per line (type value [note], csv, or bare auto-detected values)
    ImportIocs(String),
    /// remove an IOC and rebuild the IOC pass so its hits disappear too
    RemoveIoc(u64),
    /// start a collect with the collect view's form (handled by the shell)
    StartCollect,
    /// ask the running collect to stop after the current source
    CancelCollect,
    /// write the current scope to a file (handled by the shell)
    ExportScope {
        path: std::path::PathBuf,
        format: String,
        redact: bool,
    },
    /// switch the colour theme by name (handled by the shell)
    SetTheme(String),
    /// open the export dialog (handled by the shell)
    OpenExport,
    /// open or focus the session story board (handled by the shell)
    OpenBoard,
    PinToBoard(BoardDraft),
    UnpinBoardItem(u64),
    UnpinBoardTarget(Selection),
    MoveBoardItem {
        index: usize,
        delta: isize,
    },
    SetBoardNote {
        id: u64,
        note: String,
    },
    ClearBoard,
    SaveView(String),
    ApplySavedView(u64),
    RenameSavedView {
        id: u64,
        name: String,
    },
    RemoveSavedView(u64),
    IocWriteDone(Result<String, String>),
    DismissIocStatus,
    SetGroupBy(Option<fp_query::overview::GroupBy>),
    SetReverse(bool),
    TogglePinInspector,
    ShowInContext(i64, u64),
    RequestSeek(i64),
    SeekResolved {
        generation: Generation,
        offset: Result<u64, String>,
    },
    ClearScrollTarget,
    /// open the causal trace rooted at `target` (artifact path, URL, or event id)
    RequestTrace(String),
    Trace {
        generation: Generation,
        result: Result<crate::vm::TraceVm, String>,
    },
    /// the state view wants its rows re-queried (params live in the shell's view state)
    RequestStateQuery,
    States {
        generation: Generation,
        result: Result<Vec<crate::vm::StateRowVm>, String>,
    },
    /// pick the two runs the diff view compares
    SetDiffRuns(u64, u64),
    IocList {
        generation: Generation,
        result: Result<Vec<crate::vm::IocSummaryVm>, String>,
    },
    IocHits {
        generation: Generation,
        result: Result<Vec<crate::vm::IocHitVm>, String>,
    },
    SourcesView {
        generation: Generation,
        result: Result<crate::vm::SourcesDataVm, String>,
    },
    Diff {
        generation: Generation,
        result: Result<Vec<crate::vm::DiffEntryVm>, String>,
    },
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
    /// bumped whenever the inspected item changes (independent of scope)
    pub selection_gen: Generation,
    /// bumped on scope or group-by changes
    pub group_gen: Generation,
    /// bumped on each seek request
    pub seek_gen: Generation,
    pub counts: Slot<Counts>,
    /// when `counts.in_scope == 0`, how many events each single removable chip would restore
    pub empty_recovery: Slot<Vec<EmptyRecoveryHint>>,
    /// the case's timed timespan; `None` while the case has no timed events
    pub bounds: Slot<Option<(i64, i64)>>,
    /// untimed events matching the scope with its time window dropped; feeds the
    /// "untimed hidden by the time window" notice so they are never silently dropped
    pub untimed: Slot<u64>,
    pub watch: WatchState,
    pub histogram: Slot<fp_query::overview::Histogram>,
    pub detail_histogram: Slot<fp_query::overview::Histogram>,
    pub ioc_histogram: Slot<fp_query::overview::Histogram>,
    pub runs: Slot<Vec<crate::vm::RunMarkerVm>>,
    pub ioc_count: Slot<u64>,
    pub category: Slot<[u64; fp_query::category::Category::ALL.len()]>,
    pub timing: Slot<fp_query::overview::TimingCounts>,
    /// one page per value facet (kind/source/host/confidence/run), computed under
    /// the scope minus that facet's own selections
    pub facets: std::collections::HashMap<fp_query::overview::Facet, Slot<fp_store::FacetPage>>,
    pub detail: Slot<Option<Box<crate::vm::DetailVm>>>,
    pub around: Slot<Vec<crate::vm::RowVm>>,
    pub groups: Slot<Vec<crate::vm::GroupRowVm>>,
    pub dashboard_summary: Slot<crate::vm::DashboardSummaryVm>,
    pub dashboard_hosts: Slot<Vec<crate::vm::HostSummaryVm>>,
    pub hosts: Slot<Vec<crate::vm::HostSummaryVm>>,
    pub lanes: Slot<crate::vm::LanesDataVm>,
    pub hosts_gen: Generation,
    pub lanes_gen: Generation,
    pub host_sort: fp_query::overview::HostSort,
    pub lanes_mode: fp_query::overview::LaneMode,
    pub iocs: Slot<Vec<crate::vm::IocSummaryVm>>,
    pub ioc_hits: Slot<Vec<crate::vm::IocHitVm>>,
    pub sources: Slot<crate::vm::SourcesDataVm>,
    pub pending_ioc_write: Option<IocWriteRequest>,
    pub ioc_status: Option<Result<String, String>>,
    pub group_by: Option<fp_query::overview::GroupBy>,
    pub reverse: bool,
    pub inspector_pin: Option<Selection>,
    pub pending_seek: Option<i64>,
    pub scroll_to_row: Option<usize>,
    pub seek_error: Option<String>,
    pub trace: Slot<crate::vm::TraceVm>,
    pub trace_gen: Generation,
    /// the trace target currently shown (path, URL, or event id); also the pending request
    pub pending_trace: Option<String>,
    pub states: Slot<Vec<crate::vm::StateRowVm>>,
    pub states_gen: Generation,
    pub diff: Slot<Vec<crate::vm::DiffEntryVm>>,
    pub diff_gen: Generation,
    /// the two runs the diff view compares; `None` until the view picks a default
    pub pending_diff: Option<(u64, u64)>,
    pub board: Board,
    pub saved_views: SavedViews,
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

    pub fn inspected(&self) -> Selection {
        self.inspector_pin
            .clone()
            .unwrap_or_else(|| self.selection())
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
                    ScopeAction::SetKinds(v) => {
                        self.scope.set_kinds(v);
                        true
                    }
                    ScopeAction::SetKindsNot(v) => {
                        self.scope.set_kinds_not(v);
                        true
                    }
                    ScopeAction::SetKindFilter { kinds, kinds_not } => {
                        self.scope.set_kind_filter(kinds, kinds_not);
                        true
                    }
                    ScopeAction::SetHosts(v) => {
                        self.scope.set_hosts(v);
                        true
                    }
                    ScopeAction::SetHostsNot(v) => {
                        self.scope.set_hosts_not(v);
                        true
                    }
                    ScopeAction::SetHostFilter { hosts, hosts_not } => {
                        self.scope.set_host_filter(hosts, hosts_not);
                        true
                    }
                    ScopeAction::SetSources(v) => {
                        self.scope.set_sources(v);
                        true
                    }
                    ScopeAction::SetSourcesNot(v) => {
                        self.scope.set_sources_not(v);
                        true
                    }
                    ScopeAction::SetSourceFilter {
                        sources,
                        sources_not,
                    } => {
                        self.scope.set_source_filter(sources, sources_not);
                        true
                    }
                    ScopeAction::SetConfidence(v) => {
                        self.scope.set_confidence(v);
                        true
                    }
                    ScopeAction::SetRun(v) => {
                        self.scope.set_run(v);
                        true
                    }
                    ScopeAction::SetTiming(v) => {
                        self.scope.set_timing(v);
                        true
                    }
                    ScopeAction::SetIocOnly(v) => {
                        self.scope.set_ioc_only(v);
                        true
                    }
                    ScopeAction::Clear => self.scope.clear(),
                };
                if changed {
                    self.scope_changed();
                }
            }
            Action::UndoScope => {
                if self.scope.undo() {
                    self.scope_changed();
                }
            }
            Action::RedoScope => {
                if self.scope.redo() {
                    self.scope_changed();
                }
            }
            Action::Go(view) => {
                let prev = self.inspected();
                self.nav.navigate(view, Selection::None);
                if self.inspected() != prev {
                    self.selection_gen += 1;
                }
            }
            Action::Select(selection) => {
                let prev = self.inspected();
                self.nav.select(selection);
                if self.inspected() != prev {
                    self.selection_gen += 1;
                }
            }
            Action::JumpTo(selection) => {
                let prev = self.inspected();
                self.nav.navigate(self.view(), selection);
                if self.inspected() != prev {
                    self.selection_gen += 1;
                }
            }
            Action::NavBack => {
                let prev = self.inspected();
                if self.nav.back() && self.inspected() != prev {
                    self.selection_gen += 1;
                }
            }
            Action::NavForward => {
                let prev = self.inspected();
                if self.nav.forward() && self.inspected() != prev {
                    self.selection_gen += 1;
                }
            }
            Action::Counts { generation, result } => {
                self.counts.absorb(generation, self.generation, result)
            }
            Action::EmptyRecovery { generation, hints } => {
                self.empty_recovery
                    .absorb(generation, self.generation, Ok(hints));
            }
            Action::Bounds { generation, result } => {
                self.bounds.absorb(generation, self.generation, result)
            }
            Action::Untimed { generation, result } => {
                self.untimed.absorb(generation, self.generation, result)
            }
            Action::Observed { version } => self.watch.observe(version),
            Action::RefreshCase => self.watch.refresh(),
            Action::Histogram { generation, result } => {
                self.histogram.absorb(generation, self.generation, result)
            }
            Action::Category { generation, result } => {
                self.category.absorb(generation, self.generation, result)
            }
            Action::Timing { generation, result } => {
                self.timing.absorb(generation, self.generation, result)
            }
            Action::Facets {
                facet,
                generation,
                result,
            } => self
                .facets
                .entry(facet)
                .or_default()
                .absorb(generation, self.generation, result),
            Action::DetailHistogram { generation, result } => {
                self.detail_histogram
                    .absorb(generation, self.generation, result);
            }
            Action::IocHistogram { generation, result } => {
                self.ioc_histogram
                    .absorb(generation, self.generation, result);
            }
            Action::Runs { generation, result } => {
                self.runs.absorb(generation, self.generation, result);
            }
            Action::IocCount { generation, result } => {
                self.ioc_count.absorb(generation, self.generation, result);
            }
            Action::Detail { generation, result } => {
                self.detail.absorb(generation, self.selection_gen, result);
            }
            Action::Around { generation, result } => {
                self.around.absorb(generation, self.selection_gen, result);
            }
            Action::Groups { generation, result } => {
                self.groups.absorb(generation, self.group_gen, result);
            }
            Action::SetGroupBy(by) => {
                if self.group_by != by {
                    self.group_by = by;
                    self.group_gen += 1;
                }
            }
            Action::SetReverse(reverse) => {
                self.reverse = reverse;
            }
            Action::TogglePinInspector => {
                if self.inspector_pin.is_some() {
                    self.inspector_pin = None;
                } else if self.selection() != Selection::None {
                    self.inspector_pin = Some(self.selection());
                }
                self.selection_gen += 1;
            }
            Action::ShowInContext(ts, event_id) => {
                if self.scope.clear() {
                    self.scope_changed();
                }
                let target = Selection::Event(event_id);
                if self.view() != View::Timeline {
                    self.nav.navigate(View::Timeline, target.clone());
                } else {
                    self.nav.select(target.clone());
                }
                if self.inspector_pin.is_some() {
                    self.inspector_pin = Some(target);
                }
                self.selection_gen += 1;
                self.group_by = None;
                self.pending_seek = Some(ts);
                self.seek_error = None;
                self.seek_gen += 1;
            }
            Action::RequestSeek(ts) => {
                self.pending_seek = Some(ts);
                self.seek_error = None;
                self.seek_gen += 1;
            }
            Action::SeekResolved { generation, offset } => {
                if generation == self.seek_gen {
                    self.pending_seek = None;
                    match offset {
                        Ok(row) => {
                            self.scroll_to_row = usize::try_from(row).ok();
                            self.seek_error = None;
                        }
                        Err(msg) => {
                            self.seek_error = Some(msg);
                        }
                    }
                }
            }
            Action::ClearScrollTarget => {
                self.scroll_to_row = None;
            }
            Action::RequestTrace(target) => {
                self.pending_trace = Some(target);
                self.trace_gen += 1;
            }
            Action::Trace { generation, result } => {
                self.trace.absorb(generation, self.trace_gen, result);
            }
            Action::RequestStateQuery => {
                self.states_gen += 1;
            }
            Action::States { generation, result } => {
                self.states.absorb(generation, self.states_gen, result);
            }
            Action::SetDiffRuns(a, b) => {
                if self.pending_diff != Some((a, b)) {
                    self.pending_diff = Some((a, b));
                    self.diff_gen += 1;
                }
            }
            Action::Diff { generation, result } => {
                self.diff.absorb(generation, self.diff_gen, result);
            }
            Action::IocList { generation, result } => {
                self.iocs.absorb(generation, self.generation, result);
            }
            Action::IocHits { generation, result } => {
                self.ioc_hits.absorb(generation, self.generation, result);
            }
            Action::SourcesView { generation, result } => {
                self.sources.absorb(generation, self.generation, result);
            }
            Action::DashboardSummary { generation, result } => {
                self.dashboard_summary
                    .absorb(generation, self.generation, result);
            }
            Action::DashboardHosts { generation, result } => {
                self.dashboard_hosts
                    .absorb(generation, self.generation, result);
            }
            Action::Hosts { generation, result } => {
                self.hosts.absorb(generation, self.hosts_gen, result);
            }
            Action::Lanes { generation, result } => {
                self.lanes.absorb(generation, self.lanes_gen, result);
            }
            Action::SetHostSort(sort) => {
                if self.host_sort != sort {
                    self.host_sort = sort;
                    self.hosts_gen += 1;
                }
            }
            Action::RequestHostsQuery => {
                self.hosts_gen += 1;
            }
            Action::SetLanesMode(mode) => {
                if self.lanes_mode != mode {
                    self.lanes_mode = mode;
                    self.lanes_gen += 1;
                }
            }
            Action::AddIoc {
                ioc_type,
                value,
                note,
            } => {
                self.pending_ioc_write = Some(IocWriteRequest::Add {
                    ioc_type,
                    value,
                    note,
                });
            }
            Action::ImportIocs(content) => {
                self.pending_ioc_write = Some(IocWriteRequest::Import { content });
            }
            Action::RemoveIoc(id) => {
                self.pending_ioc_write = Some(IocWriteRequest::Remove { id });
            }
            Action::IocWriteDone(result) => {
                let ok = result.is_ok();
                self.ioc_status = Some(result);
                if ok {
                    // Refresh scope & host/dashboard counts after the IOC correlation pass completes.
                    self.scope_changed();
                }
            }
            Action::DismissIocStatus => {
                self.ioc_status = None;
            }
            Action::PinToBoard(draft) => {
                self.board.pin(draft);
            }
            Action::UnpinBoardItem(id) => {
                self.board.remove(id);
            }
            Action::UnpinBoardTarget(target) => {
                self.board.remove_target(&target);
            }
            Action::MoveBoardItem { index, delta } => {
                self.board.move_item(index, delta);
            }
            Action::SetBoardNote { id, note } => {
                self.board.set_note(id, note);
            }
            Action::ClearBoard => {
                self.board.clear();
            }
            Action::SaveView(name) => {
                let filter = self.scope.filter();
                let view = self.view();
                self.saved_views.save(name, view, filter);
            }
            Action::ApplySavedView(id) => {
                if let Some(sv) = self.saved_views.get(id).cloned() {
                    if self.scope.set_filter(sv.filter) {
                        self.scope_changed();
                    }
                    let prev = self.inspected();
                    self.nav.navigate(sv.view, Selection::None);
                    if self.inspected() != prev {
                        self.selection_gen += 1;
                    }
                }
            }
            Action::RenameSavedView { id, name } => {
                self.saved_views.rename(id, name);
            }
            Action::RemoveSavedView(id) => {
                self.saved_views.remove(id);
            }
            // handled by the shell before they reach the model
            Action::StartCollect | Action::CancelCollect | Action::ExportScope { .. } => {}
            Action::SetTheme(_) | Action::OpenExport | Action::OpenBoard => {}
        }
    }

    fn scope_changed(&mut self) {
        self.generation += 1;
        self.group_gen += 1;
        self.hosts_gen += 1;
        self.lanes_gen += 1;
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
