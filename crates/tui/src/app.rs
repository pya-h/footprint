//! The TUI state machine. Terminal-independent: the event loop feeds [`App::on_key`]
//! abstract keys, so the whole UI is testable without a terminal.

use anyhow::{Context, Result};
use fp_query::{
    CollectEvent, CollectParams, DiffEntry, EventDetailView, IocHit, PAGE_SIZE, SourceView,
    StateView, TimelineFilter, TimelineRow, TraceTree, Tz,
};
use fp_store::{IocRow, Store};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Timeline,
    Detail,
    Trace,
    State,
    Diff,
    Ioc,
    Sources,
    Collect,
}

impl View {
    pub const ALL: [View; 8] = [
        View::Timeline,
        View::Detail,
        View::Trace,
        View::State,
        View::Diff,
        View::Ioc,
        View::Sources,
        View::Collect,
    ];

    pub fn title(self) -> &'static str {
        match self {
            View::Timeline => "timeline",
            View::Detail => "detail",
            View::Trace => "trace",
            View::State => "state",
            View::Diff => "diff",
            View::Ioc => "ioc",
            View::Sources => "sources",
            View::Collect => "collect",
        }
    }

    /// views with no data of their own fall back to the timeline when revisited
    pub fn index(self) -> usize {
        View::ALL.iter().position(|v| *v == self).unwrap_or(0)
    }
}

/// Terminal-agnostic key, mapped from crossterm events by the event loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Esc,
    Tab,
    BackTab,
    Up,
    Down,
    PageUp,
    PageDown,
    Backspace,
}

/// The editable filter bar fields, in Tab order.
pub const FILTER_FIELDS: [&str; 6] = ["since", "until", "kind", "host", "source", "match"];

#[derive(Debug, Default, Clone)]
pub struct FilterDraft {
    pub since: String,
    pub until: String,
    pub kinds: String,
    pub hosts: String,
    pub sources: String,
    pub match_text: String,
    pub active: usize,
}

impl FilterDraft {
    pub fn get(&self, field: usize) -> &str {
        match field {
            0 => &self.since,
            1 => &self.until,
            2 => &self.kinds,
            3 => &self.hosts,
            4 => &self.sources,
            _ => &self.match_text,
        }
    }

    fn get_mut(&mut self, field: usize) -> &mut String {
        match field {
            0 => &mut self.since,
            1 => &mut self.until,
            2 => &mut self.kinds,
            3 => &mut self.hosts,
            4 => &mut self.sources,
            _ => &mut self.match_text,
        }
    }

    fn from_filter(filter: &TimelineFilter) -> Self {
        Self {
            since: filter
                .since
                .map(|t| fp_query::format_time(t, "exact", Tz::Utc, "%Y-%m-%dT%H:%M"))
                .unwrap_or_default(),
            until: filter
                .until
                .map(|t| fp_query::format_time(t, "exact", Tz::Utc, "%Y-%m-%dT%H:%M"))
                .unwrap_or_default(),
            kinds: filter.kinds.join(","),
            hosts: filter.hosts.join(","),
            sources: filter.sources.join(","),
            match_text: filter.match_text.clone().unwrap_or_default(),
            active: 0,
        }
    }

    fn apply(&self, filter: &mut TimelineFilter) -> std::result::Result<(), String> {
        let parse = |raw: &str| -> std::result::Result<Option<i64>, String> {
            let raw = raw.trim();
            if raw.is_empty() {
                return Ok(None);
            }
            fp_query::time::parse(raw)
                .map(Some)
                .map_err(|e| format!("bad time `{raw}`: {e:#}"))
        };
        filter.since = parse(&self.since)?;
        filter.until = parse(&self.until)?;
        if let (Some(lo), Some(hi)) = (filter.since, filter.until)
            && lo > hi
        {
            return Err("--since is after --until".into());
        }
        let split = |raw: &str| -> Vec<String> {
            raw.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        };
        filter.kinds = split(&self.kinds);
        filter.hosts = split(&self.hosts);
        filter.sources = split(&self.sources);
        filter.match_text = {
            let m = self.match_text.trim();
            (!m.is_empty()).then(|| m.to_string())
        };
        Ok(())
    }
}

/// One line of collect progress.
#[derive(Debug, Clone)]
pub struct CollectLine {
    pub label: String,
    pub outcome: String,
}

#[derive(Debug, Default)]
pub struct CollectState {
    pub running: bool,
    pub lines: Vec<CollectLine>,
    pub summary: Option<String>,
    pub since: String,
    pub until: String,
    pub sources: String,
    pub active_field: usize,
    pub editing: bool,
}

pub struct App {
    pub case: PathBuf,
    pub store: Store,
    pub view: View,
    pub tz: Tz,
    pub status: String,
    pub help: bool,

    pub filter: TimelineFilter,
    pub draft: FilterDraft,
    pub editing: bool,
    pub rows: Vec<TimelineRow>,
    pub has_more: bool,
    pub selected: usize,

    pub detail: Option<EventDetailView>,
    pub detail_link: usize,

    pub trace: Option<TraceTree>,
    pub trace_target: String,
    pub trace_selected: usize,

    pub state_rows: Vec<StateView>,
    pub state_kind_draft: String,
    pub state_kind: Option<String>,
    pub state_editing: bool,
    pub state_selected: usize,

    pub diff_entries: Vec<DiffEntry>,
    pub diff_a: String,
    pub diff_b: String,
    pub diff_editing: bool,
    pub diff_field: usize,
    pub diff_selected: usize,

    pub iocs: Vec<IocRow>,
    pub ioc_hits: Vec<IocHit>,
    pub ioc_selected: usize,

    pub sources: Vec<SourceView>,
    pub coverage: Vec<fp_query::CoverageRow>,
    pub source_selected: usize,

    pub collect: CollectState,
    collect_rx: Option<Receiver<CollectEvent>>,
}

impl App {
    pub fn new(case: &Path) -> Result<Self> {
        let store = Store::open(case).with_context(|| {
            format!(
                "cannot open case {}; run `footprint collect` first",
                case.display()
            )
        })?;
        let mut app = Self {
            case: case.to_path_buf(),
            store,
            view: View::Timeline,
            tz: Tz::Local,
            status: String::new(),
            help: false,
            filter: TimelineFilter::default(),
            draft: FilterDraft::default(),
            editing: false,
            rows: Vec::new(),
            has_more: false,
            selected: 0,
            detail: None,
            detail_link: 0,
            trace: None,
            trace_target: String::new(),
            trace_selected: 0,
            state_rows: Vec::new(),
            state_kind_draft: String::new(),
            state_kind: None,
            state_editing: false,
            state_selected: 0,
            diff_entries: Vec::new(),
            diff_a: "prev".into(),
            diff_b: "last".into(),
            diff_editing: false,
            diff_field: 0,
            diff_selected: 0,
            iocs: Vec::new(),
            ioc_hits: Vec::new(),
            ioc_selected: 0,
            sources: Vec::new(),
            coverage: Vec::new(),
            source_selected: 0,
            collect: CollectState::default(),
            collect_rx: None,
        };
        app.reload_timeline()?;
        app.reload_sources()?;
        app.reload_state()?;
        app.reload_ioc()?;
        app.status = format!("{} events in the case", app.rows_total());
        Ok(app)
    }

    /// Total events currently loaded (not necessarily the whole case).
    pub fn rows_total(&self) -> usize {
        self.rows.len()
    }

    fn reload_timeline(&mut self) -> Result<()> {
        let page = fp_query::timeline_page(&self.store, &self.filter, 0, PAGE_SIZE)?;
        self.rows = page.rows;
        self.has_more = page.has_more;
        self.selected = 0;
        Ok(())
    }

    fn load_more(&mut self) -> Result<()> {
        if !self.has_more {
            return Ok(());
        }
        let page = fp_query::timeline_page(&self.store, &self.filter, self.rows.len(), PAGE_SIZE)?;
        self.has_more = page.has_more;
        self.rows.extend(page.rows);
        Ok(())
    }

    fn reload_state(&mut self) -> Result<()> {
        self.state_rows =
            fp_query::state_records(&self.store, None, self.state_kind.as_deref(), None)?;
        self.state_selected = self
            .state_selected
            .min(self.state_rows.len().saturating_sub(1));
        Ok(())
    }

    fn reload_sources(&mut self) -> Result<()> {
        self.sources = fp_query::sources(&self.store, None)?;
        self.coverage = fp_query::coverage(&self.sources);
        Ok(())
    }

    pub fn reload_ioc(&mut self) -> Result<()> {
        self.iocs = self.store.list_iocs()?;
        self.ioc_hits = fp_query::check_iocs(&self.store)?;
        self.ioc_selected = self.ioc_selected.min(self.ioc_hits.len().saturating_sub(1));
        Ok(())
    }

    pub fn open_detail(&mut self, id: u64) -> Result<()> {
        match fp_query::event_detail(&self.store, id)? {
            Some(detail) => {
                self.detail = Some(detail);
                self.detail_link = 0;
                self.view = View::Detail;
                Ok(())
            }
            None => {
                self.status = format!("event {id} not found");
                Ok(())
            }
        }
    }

    pub fn open_trace(&mut self, target: &str) -> Result<()> {
        match fp_query::trace_tree(&self.store, target) {
            Ok(tree) => {
                self.trace = Some(tree);
                self.trace_target = target.to_string();
                self.trace_selected = 0;
                self.view = View::Trace;
            }
            Err(e) => self.status = format!("trace: {e:#}"),
        }
        Ok(())
    }

    fn compute_diff(&mut self) -> Result<()> {
        let run_a = self.resolve_run(&self.diff_a.clone())?;
        let run_b = self.resolve_run(&self.diff_b.clone())?;
        self.diff_entries = fp_query::diff_runs(&self.store, run_a, run_b)?;
        self.diff_selected = 0;
        self.status = format!(
            "{} difference(s) between run {run_a} and run {run_b}",
            self.diff_entries.len()
        );
        Ok(())
    }

    fn resolve_run(&self, raw: &str) -> Result<u64> {
        if let Ok(id) = raw.parse::<u64>() {
            return Ok(id);
        }
        let last = self.store.last_run_id()?.context("no runs found in case")?;
        match raw {
            "last" | "latest" => Ok(last),
            "prev" | "previous" => Ok(last.saturating_sub(1).max(1)),
            other => anyhow::bail!("invalid run id '{other}'; expected integer, 'last', or 'prev'"),
        }
    }

    pub fn start_collect(&mut self) -> Result<()> {
        if self.collect.running {
            return Ok(());
        }
        let params = CollectParams {
            since: (!self.collect.since.trim().is_empty())
                .then(|| self.collect.since.trim().into()),
            until: (!self.collect.until.trim().is_empty())
                .then(|| self.collect.until.trim().into()),
            sources: self
                .collect
                .sources
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            assume_tz: None,
            ..Default::default()
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let case = self.case.clone();
        std::thread::spawn(move || {
            let _ = fp_query::run_collect(&case, &params, Some(&tx));
        });
        self.collect.running = true;
        self.collect.summary = None;
        self.collect.lines.clear();
        self.status = "collecting…".into();
        self.collect_rx = Some(rx);
        Ok(())
    }

    /// Drains collect progress; call periodically from the event loop.
    pub fn poll_collect(&mut self) -> Result<()> {
        let Some(rx) = &self.collect_rx else {
            return Ok(());
        };
        let mut finished = None;
        while let Ok(event) = rx.try_recv() {
            match event {
                CollectEvent::InstanceStarted { label } => {
                    self.collect.lines.push(CollectLine {
                        label,
                        outcome: "reading…".into(),
                    });
                }
                CollectEvent::InstanceDone {
                    label,
                    totals,
                    status,
                    ..
                } => {
                    let outcome = format!(
                        "{} new / {} known, {}",
                        totals.events_new, totals.events_known, status
                    );
                    if let Some(line) = self
                        .collect
                        .lines
                        .iter_mut()
                        .rev()
                        .find(|l| l.label == label && l.outcome == "reading…")
                    {
                        line.outcome = outcome;
                    } else {
                        self.collect.lines.push(CollectLine { label, outcome });
                    }
                }
                CollectEvent::InstanceError { label, error } => {
                    let outcome = format!("error: {error}");
                    if let Some(line) = self
                        .collect
                        .lines
                        .iter_mut()
                        .rev()
                        .find(|l| l.label == label)
                    {
                        line.outcome = outcome;
                    } else {
                        self.collect.lines.push(CollectLine { label, outcome });
                    }
                }
                CollectEvent::CorrelateFailed(e) => self.collect.lines.push(CollectLine {
                    label: "correlate".into(),
                    outcome: format!("error: {e}"),
                }),
                CollectEvent::Finished(summary) => {
                    finished = Some(format!(
                        "run {}: {} new events ({} known), {} states, {} artifacts from {} sources{}",
                        summary.run_id,
                        summary.totals.events_new,
                        summary.totals.events_known,
                        summary.totals.states,
                        summary.totals.artifacts,
                        summary.sources_read,
                        if summary.errors.is_empty() {
                            String::new()
                        } else {
                            format!(", {} failed", summary.errors.len())
                        }
                    ));
                }
            }
        }
        if let Some(text) = finished {
            self.collect.running = false;
            self.collect.summary = Some(text.clone());
            self.collect_rx = None;
            self.status = text;
            // the case grew: refresh everything the UI caches
            self.reload_timeline()?;
            self.reload_sources()?;
            self.reload_state()?;
            self.reload_ioc()?;
        }
        Ok(())
    }

    /// Handles one key. Returns true when the app should quit.
    pub fn on_key(&mut self, key: Key) -> Result<bool> {
        if self.help {
            if matches!(key, Key::Esc | Key::Char('?') | Key::Char('q')) {
                self.help = false;
            }
            return Ok(false);
        }
        if matches!(key, Key::Char('?')) && !self.editing {
            self.help = true;
            return Ok(false);
        }
        if matches!(key, Key::Char('q')) && !self.editing {
            return Ok(true);
        }
        if matches!(key, Key::Char('t')) && !self.editing && self.view == View::Timeline {
            self.tz = match self.tz {
                Tz::Local => Tz::Utc,
                Tz::Utc | Tz::Fixed(_) => Tz::Local,
            };
            return Ok(false);
        }
        if let Some(digit) = key
            .char()
            .and_then(|c| c.to_digit(10))
            .filter(|d| (1..=8).contains(d))
            && !self.editing
            && !matches!(self.view, View::Detail | View::Trace)
        {
            self.view = View::ALL[(digit - 1) as usize];
            self.on_enter_view()?;
            return Ok(false);
        }

        match self.view {
            View::Timeline => self.on_timeline_key(key),
            View::Detail => self.on_detail_key(key),
            View::Trace => self.on_trace_key(key),
            View::State => self.on_state_key(key),
            View::Diff => self.on_diff_key(key),
            View::Ioc => self.on_ioc_key(key),
            View::Sources => self.on_sources_key(key),
            View::Collect => self.on_collect_key(key),
        }
    }

    fn on_enter_view(&mut self) -> Result<()> {
        match self.view {
            View::Timeline | View::Detail | View::Trace => {}
            View::State => self.reload_state()?,
            View::Diff => self.compute_diff()?,
            View::Ioc => self.reload_ioc()?,
            View::Sources => self.reload_sources()?,
            View::Collect => self.poll_collect()?,
        }
        Ok(())
    }

    fn on_timeline_key(&mut self, key: Key) -> Result<bool> {
        if self.editing {
            return self.edit_filter(key);
        }
        match key {
            Key::Char('i') | Key::Char('/') => {
                self.draft = FilterDraft::from_filter(&self.filter);
                self.editing = true;
                self.status =
                    "filter: Tab/Shift-Tab between fields, Enter applies, Esc cancels".into();
            }
            Key::Down => {
                if self.selected + 1 < self.rows.len() {
                    self.selected += 1;
                } else {
                    self.load_more()?;
                    if self.selected + 1 < self.rows.len() {
                        self.selected += 1;
                    }
                }
            }
            Key::Up => self.selected = self.selected.saturating_sub(1),
            Key::PageDown => {
                for _ in 0..PAGE_SIZE {
                    if self.selected + 1 < self.rows.len() {
                        self.selected += 1;
                    } else {
                        self.load_more()?;
                        if self.selected + 1 >= self.rows.len() {
                            break;
                        }
                    }
                }
            }
            Key::PageUp => self.selected = self.selected.saturating_sub(PAGE_SIZE),
            Key::Enter => {
                if let Some(row) = self.rows.get(self.selected) {
                    self.open_detail(row.id)?;
                }
            }
            Key::Char('r') => {
                let target = self
                    .rows
                    .get(self.selected)
                    .and_then(|r| r.subject.clone().filter(|s| !s.is_empty()))
                    .unwrap_or_default();
                if target.is_empty() {
                    if let Some(row) = self.rows.get(self.selected) {
                        self.open_trace(&row.id.to_string())?;
                    }
                } else {
                    self.open_trace(&target)?;
                }
            }
            _ => {}
        }
        Ok(false)
    }

    fn edit_filter(&mut self, key: Key) -> Result<bool> {
        match key {
            Key::Esc => {
                self.editing = false;
                self.status.clear();
            }
            Key::Tab => self.draft.active = (self.draft.active + 1) % FILTER_FIELDS.len(),
            Key::BackTab => {
                self.draft.active =
                    (self.draft.active + FILTER_FIELDS.len() - 1) % FILTER_FIELDS.len();
            }
            Key::Enter => match self.draft.apply(&mut self.filter) {
                Ok(()) => {
                    self.editing = false;
                    self.reload_timeline()?;
                    self.status = format!("{} event(s) match", self.rows.len());
                }
                Err(e) => self.status = e,
            },
            Key::Backspace => {
                self.draft.get_mut(self.draft.active).pop();
            }
            Key::Char(c) => self.draft.get_mut(self.draft.active).push(c),
            _ => {}
        }
        Ok(false)
    }

    fn on_detail_key(&mut self, key: Key) -> Result<bool> {
        let Some(detail) = &self.detail else {
            self.view = View::Timeline;
            return Ok(false);
        };
        match key {
            Key::Esc => self.view = View::Timeline,
            Key::Down => {
                let n = detail.links.len();
                if n > 0 {
                    self.detail_link = (self.detail_link + 1) % n;
                }
            }
            Key::Up => {
                let n = detail.links.len();
                if n > 0 {
                    self.detail_link = (self.detail_link + n - 1) % n;
                }
            }
            Key::Enter => {
                let link = &detail.links[self.detail_link.min(detail.links.len() - 1)];
                if link.target_type == "event" {
                    let id = link.target_id;
                    self.open_detail(id)?;
                }
            }
            Key::Char('r') => {
                let id = detail.id;
                self.open_trace(&id.to_string())?;
            }
            _ => {}
        }
        Ok(false)
    }

    fn on_trace_key(&mut self, key: Key) -> Result<bool> {
        let Some(trace) = &self.trace else {
            self.view = View::Timeline;
            return Ok(false);
        };
        match key {
            Key::Esc => self.view = View::Timeline,
            Key::Down => {
                if self.trace_selected + 1 < trace.rows.len() {
                    self.trace_selected += 1;
                }
            }
            Key::Up => self.trace_selected = self.trace_selected.saturating_sub(1),
            Key::Enter => {
                if let Some(row) = trace.rows.get(self.trace_selected)
                    && let Some(id) = row.event_id
                {
                    self.open_detail(id)?;
                }
            }
            _ => {}
        }
        Ok(false)
    }

    fn on_state_key(&mut self, key: Key) -> Result<bool> {
        if self.state_editing {
            match key {
                Key::Esc => self.state_editing = false,
                Key::Enter => {
                    self.state_editing = false;
                    let draft = self.state_kind_draft.trim().to_string();
                    self.state_kind = (!draft.is_empty()).then_some(draft);
                    self.reload_state()?;
                    self.status = format!("{} state record(s)", self.state_rows.len());
                }
                Key::Backspace => {
                    self.state_kind_draft.pop();
                }
                Key::Char(c) => self.state_kind_draft.push(c),
                _ => {}
            }
            return Ok(false);
        }
        match key {
            Key::Char('k') => {
                self.state_editing = true;
                self.state_kind_draft = self.state_kind.clone().unwrap_or_default();
            }
            Key::Down => {
                if self.state_selected + 1 < self.state_rows.len() {
                    self.state_selected += 1;
                }
            }
            Key::Up => self.state_selected = self.state_selected.saturating_sub(1),
            _ => {}
        }
        Ok(false)
    }

    fn on_diff_key(&mut self, key: Key) -> Result<bool> {
        if self.diff_editing {
            match key {
                Key::Esc => self.diff_editing = false,
                Key::Tab => self.diff_field = 1 - self.diff_field,
                Key::Enter => {
                    self.diff_editing = false;
                    self.compute_diff()?;
                }
                Key::Backspace => {
                    let field = if self.diff_field == 0 {
                        &mut self.diff_a
                    } else {
                        &mut self.diff_b
                    };
                    field.pop();
                }
                Key::Char(c) => {
                    let field = if self.diff_field == 0 {
                        &mut self.diff_a
                    } else {
                        &mut self.diff_b
                    };
                    field.push(c);
                }
                _ => {}
            }
            return Ok(false);
        }
        match key {
            Key::Char('e') => self.diff_editing = true,
            Key::Down => {
                if self.diff_selected + 1 < self.diff_entries.len() {
                    self.diff_selected += 1;
                }
            }
            Key::Up => self.diff_selected = self.diff_selected.saturating_sub(1),
            _ => {}
        }
        Ok(false)
    }

    fn on_ioc_key(&mut self, key: Key) -> Result<bool> {
        match key {
            Key::Down => {
                if self.ioc_selected + 1 < self.ioc_hits.len() {
                    self.ioc_selected += 1;
                }
            }
            Key::Up => self.ioc_selected = self.ioc_selected.saturating_sub(1),
            Key::Enter => {
                if let Some(hit) = self.ioc_hits.get(self.ioc_selected)
                    && hit.target_type == "event"
                {
                    let id = hit.target_id;
                    self.open_detail(id)?;
                }
            }
            _ => {}
        }
        Ok(false)
    }

    fn on_sources_key(&mut self, key: Key) -> Result<bool> {
        let max = self.sources.len();
        match key {
            Key::Down => {
                if self.source_selected + 1 < max {
                    self.source_selected += 1;
                }
            }
            Key::Up => self.source_selected = self.source_selected.saturating_sub(1),
            _ => {}
        }
        Ok(false)
    }

    fn on_collect_key(&mut self, key: Key) -> Result<bool> {
        if self.collect.running {
            return Ok(false); // progress keeps streaming; q still works via the global handler
        }
        const FIELDS: [&str; 3] = ["since", "until", "sources"];
        match key {
            Key::Char('e') => self.collect.editing = true,
            Key::Tab => {
                self.collect.active_field = (self.collect.active_field + 1) % FIELDS.len();
                self.collect.editing = true;
            }
            Key::BackTab => {
                self.collect.active_field =
                    (self.collect.active_field + FIELDS.len() - 1) % FIELDS.len();
                self.collect.editing = true;
            }
            Key::Enter => {
                if self.collect.editing {
                    self.collect.editing = false;
                } else {
                    self.start_collect()?;
                }
            }
            Key::Esc => self.collect.editing = false,
            Key::Backspace => {
                if self.collect.editing {
                    self.collect.field_mut(self.collect.active_field).pop();
                }
            }
            Key::Char(c) if self.collect.editing => {
                self.collect.field_mut(self.collect.active_field).push(c);
            }
            _ => {}
        }
        Ok(false)
    }
}

impl CollectState {
    fn field_mut(&mut self, field: usize) -> &mut String {
        match field {
            0 => &mut self.since,
            1 => &mut self.until,
            _ => &mut self.sources,
        }
    }

    pub fn field(&self, field: usize) -> &str {
        match field {
            0 => &self.since,
            1 => &self.until,
            _ => &self.sources,
        }
    }
}

impl Key {
    fn char(self) -> Option<char> {
        match self {
            Key::Char(c) => Some(c),
            _ => None,
        }
    }
}
