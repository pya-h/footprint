//! The window shell around the views: top bar, scope bar, rail, banners and the
//! status bar. The views themselves land in later milestones; the shell already
//! owns the case header, the counts and every full-window state (empty, loading,
//! error, busy, changed).

use std::collections::HashSet;
use std::path::PathBuf;

use eframe::egui;
use fp_query::overview::{Facet, HostSort, LaneMode, TimeWindow};

use crate::model::{Action, Counts, LoadStatus, Model, ScopeAction, Selection, View};
use crate::settings::Settings;
use crate::theme::{Theme, ThemeName};
use crate::views::collect::{
    CollectRun, CollectViewState, InstanceRow, InstanceState, show_collect,
};
use crate::views::dashboard::{DashboardState, show_dashboard};
use crate::views::diff::{DiffViewState, show_diff};
use crate::views::export::{ExportDialog, show_export_dialog, spawn_export};
use crate::views::help::show_help;
use crate::views::hosts::{HOSTS_CAP, HostsState, show_hosts};
use crate::views::ioc::{IocViewState, show_iocs};
use crate::views::lanes::{LanesState, show_lanes};
use crate::views::palette::{PaletteState, show_palette, toggle as palette_toggle};
use crate::views::sources::show_sources;
use crate::views::state::{StateViewState, show_state};
use crate::views::trace::{TraceState, show_trace};
use crate::vm::{
    DashboardSummaryVm, DetailVm, DiffEntryVm, GroupRowVm, HostSummaryVm, LaneRowDensityVm,
    LanesDataVm, RowVm, RunMarkerVm, SafeText, SourceHealthVm, SourceIssueVm, SourcesDataVm,
    StateRowVm, TraceVm, UntimedLaneVm,
};
use crate::widgets::chip::{ChipEditorState, show_scope_bar, thousands};
use crate::widgets::event_table::TableState;
use crate::widgets::facets::{FacetsState, show_facets};
use crate::widgets::inspector::show_inspector;
use crate::widgets::strip::StripState;
use crate::worker::cache::BLOCK_SIZE;
use crate::worker::{BlockCache, Generation, QueryError, ReqKind, Worker};

pub struct Shell {
    ctx: egui::Context,
    case: PathBuf,
    tz: fp_query::Tz,
    theme: ThemeName,
    settings: Settings,
    settings_warning: Option<String>,
    model: Model,
    worker: Option<Worker>,
    startup_error: Option<String>,
    search: String,
    case_missing: bool,
    submitted: Option<Generation>,
    submitted_hist_gen: Option<(Generation, usize)>,
    submitted_detail_hist: Option<(Generation, i64, i64, usize)>,
    submitted_facets: HashSet<(Generation, Facet)>,
    submitted_runs: bool,
    submitted_detail: Option<Generation>,
    submitted_groups: Option<Generation>,
    submitted_seek: Option<Generation>,
    submitted_dashboard: Option<Generation>,
    submitted_dashboard_hosts: Option<Generation>,
    submitted_hosts: Option<(Generation, Generation, String)>,
    submitted_lanes: Option<(Generation, Generation, usize)>,
    submitted_trace: Option<(String, Generation)>,
    submitted_states: Option<(Generation, Option<u64>, String)>,
    submitted_diff: Option<(u64, u64, Generation)>,
    submitted_iocs: Option<Generation>,
    submitted_ioc_hits: Option<(Generation, Option<u64>)>,
    submitted_sources: Option<Generation>,
    outstanding: HashSet<u64>,
    cache: BlockCache,
    chip_editor: ChipEditorState,
    strip_state: StripState,
    facets_state: FacetsState,
    table_state: TableState,
    dashboard_state: DashboardState,
    hosts_state: HostsState,
    lanes_state: LanesState,
    trace_state: TraceState,
    state_state: StateViewState,
    diff_state: DiffViewState,
    ioc_state: IocViewState,
    collect_state: CollectViewState,
    export_dialog: ExportDialog,
    palette: PaletteState,
    help_open: bool,
    focus_search_next: bool,
}

fn apply_theme(name: ThemeName, ui_scale: f32, ctx: &egui::Context) {
    match name {
        ThemeName::Dark => Theme::dark(),
        ThemeName::Light => Theme::light(),
        ThemeName::HighContrast => Theme::high_contrast(),
    }
    .apply(ctx);
    ctx.set_zoom_factor(ui_scale.clamp(0.8, 1.6));
}

fn parse_theme(name: &str) -> Option<ThemeName> {
    match name.to_lowercase().replace(['_', '-'], " ").as_str() {
        "dark" => Some(ThemeName::Dark),
        "light" => Some(ThemeName::Light),
        "high contrast" | "hc" => Some(ThemeName::HighContrast),
        _ => None,
    }
}

fn view_from_label(label: &str) -> Option<View> {
    View::ALL
        .iter()
        .find(|v| v.label().eq_ignore_ascii_case(label))
        .copied()
}

fn downcast<T: 'static>(
    res: Result<crate::worker::Payload, QueryError>,
    what: &str,
) -> Result<T, String> {
    match res {
        Ok(payload) => payload
            .downcast::<T>()
            .map(|b| *b)
            .map_err(|_| format!("internal: {what} payload mismatch")),
        Err(QueryError::Message(m)) => Err(m),
        Err(QueryError::Interrupted) => Err("interrupted".to_string()),
    }
}

fn facet_sub(facet: Facet) -> &'static str {
    match facet {
        Facet::Kind => "kind",
        Facet::Source => "source",
        Facet::Host => "host",
        Facet::Confidence => "confidence",
        Facet::Run => "run",
    }
}

fn facet_from_sub(sub: &str) -> Option<Facet> {
    match sub {
        "kind" => Some(Facet::Kind),
        "source" => Some(Facet::Source),
        "host" => Some(Facet::Host),
        "confidence" => Some(Facet::Confidence),
        "run" => Some(Facet::Run),
        _ => None,
    }
}

fn facet_minus_self_filter(
    base: &fp_query::TimelineFilter,
    facet: Facet,
) -> fp_query::TimelineFilter {
    let mut f = base.clone();
    match facet {
        Facet::Kind => {
            f.kinds.clear();
            f.kinds_not.clear();
        }
        Facet::Source => {
            f.sources.clear();
            f.sources_not.clear();
        }
        Facet::Host => {
            f.hosts.clear();
            f.hosts_not.clear();
        }
        Facet::Confidence => f.confidence.clear(),
        Facet::Run => f.run_id = None,
    }
    f
}

impl Shell {
    pub fn new(
        case: PathBuf,
        tz: fp_query::Tz,
        settings: Settings,
        settings_warning: Option<String>,
        ctx: &egui::Context,
    ) -> Self {
        apply_theme(settings.theme, settings.ui_scale, ctx);
        let worker = Worker::spawn(&case, 3, {
            let ctx = ctx.clone();
            move || ctx.request_repaint()
        });
        // an unreadable case is not a crash: the window opens with an error card
        let startup_error = worker.as_ref().err().map(|e| e.to_string());
        let case_missing = !case.exists();

        let mut model = Model::new();
        if !case_missing && let Some(view) = settings.last_view.as_deref().and_then(view_from_label)
        {
            model.nav.navigate(view, Selection::None);
        }
        Self {
            ctx: ctx.clone(),
            case,
            tz,
            theme: settings.theme,
            settings,
            settings_warning,
            model,
            worker: worker.ok(),
            startup_error,
            search: String::new(),
            case_missing,
            submitted: None,
            submitted_hist_gen: None,
            submitted_detail_hist: None,
            submitted_facets: HashSet::new(),
            submitted_runs: false,
            submitted_detail: None,
            submitted_groups: None,
            submitted_seek: None,
            submitted_dashboard: None,
            submitted_dashboard_hosts: None,
            submitted_hosts: None,
            submitted_lanes: None,
            submitted_trace: None,
            outstanding: HashSet::new(),
            cache: BlockCache::new(),
            chip_editor: ChipEditorState::default(),
            strip_state: StripState::default(),
            facets_state: FacetsState::default(),
            table_state: TableState::default(),
            dashboard_state: DashboardState::default(),
            hosts_state: HostsState::default(),
            lanes_state: LanesState::default(),
            trace_state: TraceState::default(),
            state_state: StateViewState::default(),
            diff_state: DiffViewState::default(),
            ioc_state: IocViewState::default(),
            collect_state: CollectViewState::default(),
            export_dialog: ExportDialog::default(),
            palette: PaletteState::default(),
            help_open: false,
            submitted_states: None,
            submitted_diff: None,
            submitted_iocs: None,
            submitted_ioc_hits: None,
            submitted_sources: None,
            focus_search_next: false,
        }
    }

    pub fn model(&self) -> &Model {
        &self.model
    }

    pub fn apply(&mut self, action: Action) {
        let is_refresh = matches!(action, Action::RefreshCase);
        let is_ioc_ok = matches!(action, Action::IocWriteDone(Ok(_)));
        if matches!(action, Action::StartCollect) {
            self.start_collect();
            return;
        }
        if matches!(action, Action::CancelCollect) {
            fp_query::cancel_collect();
            return;
        }
        if let Action::SetTheme(name) = &action {
            if let Some(theme) = parse_theme(name) {
                self.theme = theme;
                let ctx = self.ctx.clone();
                apply_theme(self.theme, self.settings.ui_scale, &ctx);
            }
            return;
        }
        if matches!(action, Action::OpenExport) {
            self.export_dialog.open_default(&self.case);
            return;
        }
        if let Action::ExportScope {
            path,
            format,
            redact,
        } = &action
        {
            let rx = spawn_export(
                self.case.clone(),
                fp_store::EventFilter::from(&self.model.scope.filter()),
                format.clone(),
                path.clone(),
                *redact,
                self.tz,
            );
            self.export_dialog.running = Some(rx);
            return;
        }
        self.model.apply(action);
        if let Some(request) = self.model.pending_ioc_write.take() {
            self.submit_ioc_write(request);
        }
        if is_refresh || is_ioc_ok {
            self.reset_submission_markers();
        }
    }

    fn submit_ioc_write(&mut self, request: crate::model::IocWriteRequest) {
        let Some(worker) = &self.worker else {
            self.model
                .apply(Action::IocWriteDone(Err("worker not running".to_string())));
            return;
        };
        let generation = self.model.generation;
        let id = worker.submit_writer(ReqKind::IocWrite, generation, move |store| {
            let message = match &request {
                crate::model::IocWriteRequest::Add { ioc_type, value } => {
                    fp_query::iocs::add_ioc(store, ioc_type, value, None)
                        .map_err(|e| QueryError::Message(e.to_string()))?;
                    format!("Added IOC {ioc_type}:{value}")
                }
                crate::model::IocWriteRequest::Import { content } => {
                    let n = fp_query::iocs::import_iocs(store, content)
                        .map_err(|e| QueryError::Message(e.to_string()))?;
                    format!("Imported {n} IOCs")
                }
                crate::model::IocWriteRequest::Remove { id } => {
                    if !store.delete_ioc(*id)? {
                        return Err(QueryError::Message(format!("IOC #{id} not found")));
                    }
                    // rebuild the pass so the removed IOC's links disappear too
                    fp_correlate::correlate_iocs(store)?;
                    format!("Removed IOC #{id}")
                }
            };
            Ok(Box::new(message))
        });
        self.outstanding.insert(id);
    }

    fn start_collect(&mut self) {
        if self.collect_state.run.is_some() {
            return;
        }
        let params = self.collect_state.params();
        let case = self.case.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = fp_query::run_collect(&case, &params, Some(&tx));
            if let Err(err) = result {
                let _ = tx.send(fp_query::CollectEvent::Failed(err.to_string()));
            }
        });
        self.collect_state.run = Some(CollectRun {
            rx,
            planned_total: None,
            done: 0,
            rows: Vec::new(),
            totals: fp_query::IngestTotals::default(),
            finished: None,
            run_id: None,
        });
    }

    fn drain_collect(&mut self) {
        let Some(run) = self.collect_state.run.as_mut() else {
            return;
        };
        let mut finished = None;
        for event in run.rx.try_iter() {
            match event {
                fp_query::CollectEvent::Planned { total } => run.planned_total = Some(total),
                fp_query::CollectEvent::InstanceStarted { label } => run.rows.push(InstanceRow {
                    label,
                    state: InstanceState::Reading,
                }),
                fp_query::CollectEvent::InstanceDone {
                    label,
                    totals,
                    warnings,
                    status,
                } => {
                    run.done += 1;
                    run.totals.events_new += totals.events_new;
                    run.totals.events_known += totals.events_known;
                    run.totals.states += totals.states;
                    run.totals.artifacts += totals.artifacts;
                    if let Some(row) = run.rows.iter_mut().find(|r| r.label == label) {
                        row.state = InstanceState::Done {
                            events_new: totals.events_new,
                            status: status.to_string(),
                            warnings,
                        };
                    }
                }
                fp_query::CollectEvent::InstanceError { label, error } => {
                    run.done += 1;
                    if let Some(row) = run.rows.iter_mut().find(|r| r.label == label) {
                        row.state = InstanceState::Error(error);
                    }
                }
                fp_query::CollectEvent::CorrelateFailed(err) => {
                    run.rows.push(InstanceRow {
                        label: "correlation".to_string(),
                        state: InstanceState::Error(err),
                    });
                }
                fp_query::CollectEvent::Finished(summary) => {
                    run.run_id = Some(summary.run_id);
                    finished = Some(format!(
                        "done: {} sources, +{} new, {} errors",
                        summary.sources_read,
                        summary.totals.events_new,
                        summary.errors.len()
                    ));
                }
                fp_query::CollectEvent::Failed(err) => {
                    finished = Some(format!("collect failed: {err}"));
                }
            }
        }
        if let Some(message) = finished {
            run.finished = Some(message);
            // the case changed under the pool's readers: drop every stale marker
            // so the next ensure_requests round re-queries it
            self.reset_submission_markers();
        }
    }

    fn apply_all(&mut self, actions: Vec<Action>) {
        for action in actions {
            self.apply(action);
        }
    }

    fn active_theme(&self) -> Theme {
        match self.theme {
            ThemeName::Dark => Theme::dark(),
            ThemeName::Light => Theme::light(),
            ThemeName::HighContrast => Theme::high_contrast(),
        }
    }

    fn drain_worker(&mut self) {
        let responses = self.worker.as_ref().map(|w| w.poll()).unwrap_or_default();
        for resp in responses {
            self.outstanding.remove(&resp.id);
            match resp.kind {
                ReqKind::Count => {
                    let result = match resp.result {
                        Ok(payload) => payload
                            .downcast::<(Counts, u64)>()
                            .map(|boxed| *boxed)
                            .map_err(|_| "internal: counts payload mismatch".to_string()),
                        Err(QueryError::Message(m)) => Err(m),
                        Err(QueryError::Interrupted) => continue,
                    };
                    let generation = resp.generation;
                    match result {
                        Ok((counts, untimed)) => {
                            self.apply(Action::Counts {
                                generation,
                                result: Ok(counts),
                            });
                            self.apply(Action::Untimed {
                                generation,
                                result: Ok(untimed),
                            });
                        }
                        Err(message) => {
                            self.apply(Action::Counts {
                                generation,
                                result: Err(message.clone()),
                            });
                            self.apply(Action::Untimed {
                                generation,
                                result: Err(message),
                            });
                        }
                    }
                }
                ReqKind::Bounds => {
                    let result = match resp.result {
                        Ok(payload) => payload
                            .downcast::<Option<(i64, i64)>>()
                            .map(|boxed| *boxed)
                            .map_err(|_| "internal: bounds payload mismatch".to_string()),
                        Err(QueryError::Message(m)) => Err(m),
                        Err(QueryError::Interrupted) => continue,
                    };
                    self.apply(Action::Bounds {
                        generation: resp.generation,
                        result,
                    });
                }
                ReqKind::Watch => {
                    let Some(version) = resp
                        .result
                        .ok()
                        .and_then(|p| p.downcast::<i64>().map(|b| *b).ok())
                    else {
                        continue;
                    };
                    if self.case_missing {
                        // the case appeared after startup: leave the empty state and load it
                        self.case_missing = false;
                        self.reset_submission_markers();
                    }
                    self.apply(Action::Observed { version });
                }
                ReqKind::Histogram => {
                    let result =
                        downcast::<fp_query::overview::Histogram>(resp.result, "histogram");
                    match resp.sub.as_deref() {
                        Some("detail") => self.apply(Action::DetailHistogram {
                            generation: resp.generation,
                            result,
                        }),
                        Some("ioc") => self.apply(Action::IocHistogram {
                            generation: resp.generation,
                            result,
                        }),
                        _ => self.apply(Action::Histogram {
                            generation: resp.generation,
                            result,
                        }),
                    }
                }
                ReqKind::Runs => {
                    let result = downcast::<Vec<RunMarkerVm>>(resp.result, "runs");
                    self.apply(Action::Runs {
                        generation: resp.generation,
                        result,
                    });
                }
                ReqKind::Facets => match resp.sub.as_deref() {
                    Some("category") => {
                        let result = downcast::<[u64; fp_query::category::Category::ALL.len()]>(
                            resp.result,
                            "category",
                        );
                        self.apply(Action::Category {
                            generation: resp.generation,
                            result,
                        });
                    }
                    Some("timing") => {
                        let result =
                            downcast::<fp_query::overview::TimingCounts>(resp.result, "timing");
                        self.apply(Action::Timing {
                            generation: resp.generation,
                            result,
                        });
                    }
                    Some("ioc_count") => {
                        let result = downcast::<u64>(resp.result, "ioc_count");
                        self.apply(Action::IocCount {
                            generation: resp.generation,
                            result,
                        });
                    }
                    Some(sub) => {
                        let Some(facet) = facet_from_sub(sub) else {
                            continue;
                        };
                        let result = downcast::<fp_store::FacetPage>(resp.result, "facet");
                        self.apply(Action::Facets {
                            facet,
                            generation: resp.generation,
                            result,
                        });
                    }
                    None => {}
                },
                ReqKind::Page => {
                    let Some(sub) = resp.sub.as_deref() else {
                        continue;
                    };
                    let Some(key) = BlockCache::decode_sub(sub) else {
                        continue;
                    };
                    match downcast::<Vec<RowVm>>(resp.result, "page") {
                        Ok(rows) => self.cache.insert(key, rows),
                        Err(msg) => self.cache.record_error(key, resp.generation, msg),
                    }
                }
                ReqKind::Seek => {
                    let result = downcast::<u64>(resp.result, "seek");
                    self.apply(Action::SeekResolved {
                        generation: resp.generation,
                        offset: result,
                    });
                }
                ReqKind::Detail => {
                    let result = downcast::<Option<Box<DetailVm>>>(resp.result, "detail");
                    self.apply(Action::Detail {
                        generation: resp.generation,
                        result,
                    });
                }
                ReqKind::Around => {
                    let result = downcast::<Vec<RowVm>>(resp.result, "around");
                    self.apply(Action::Around {
                        generation: resp.generation,
                        result,
                    });
                }
                ReqKind::Groups => match resp.sub.as_deref() {
                    Some("lanes") => {
                        let result = downcast::<LanesDataVm>(resp.result, "lanes");
                        self.apply(Action::Lanes {
                            generation: resp.generation,
                            result,
                        });
                    }
                    _ => {
                        let result = downcast::<Vec<GroupRowVm>>(resp.result, "groups");
                        self.apply(Action::Groups {
                            generation: resp.generation,
                            result,
                        });
                    }
                },
                ReqKind::Hosts => {
                    let result = downcast::<Vec<HostSummaryVm>>(resp.result, "hosts");
                    match resp.sub.as_deref() {
                        Some("dashboard") => self.apply(Action::DashboardHosts {
                            generation: resp.generation,
                            result,
                        }),
                        _ => self.apply(Action::Hosts {
                            generation: resp.generation,
                            result,
                        }),
                    }
                }
                ReqKind::IocWrite => {
                    let result = downcast::<String>(resp.result, "ioc_write");
                    self.apply(Action::IocWriteDone(result));
                }
                ReqKind::Trace => {
                    let result = downcast::<TraceVm>(resp.result, "trace");
                    self.apply(Action::Trace {
                        generation: resp.generation,
                        result,
                    });
                }
                ReqKind::State => {
                    let result = downcast::<Vec<StateRowVm>>(resp.result, "state");
                    self.apply(Action::States {
                        generation: resp.generation,
                        result,
                    });
                }
                ReqKind::Diff => {
                    let result = downcast::<Vec<DiffEntryVm>>(resp.result, "diff");
                    self.apply(Action::Diff {
                        generation: resp.generation,
                        result,
                    });
                }
                ReqKind::Iocs => {
                    let result = downcast::<Vec<crate::vm::IocSummaryVm>>(resp.result, "iocs");
                    self.apply(Action::IocList {
                        generation: resp.generation,
                        result,
                    });
                }
                ReqKind::IocHits => {
                    let result = downcast::<Vec<crate::vm::IocHitVm>>(resp.result, "ioc_hits");
                    self.apply(Action::IocHits {
                        generation: resp.generation,
                        result,
                    });
                }
                ReqKind::Sources => {
                    // "dashboard" is the summary payload; anything else is the Sources view
                    if resp.sub.as_deref() == Some("dashboard") {
                        let result =
                            downcast::<DashboardSummaryVm>(resp.result, "dashboard_summary");
                        self.apply(Action::DashboardSummary {
                            generation: resp.generation,
                            result,
                        });
                    } else {
                        let result = downcast::<crate::vm::SourcesDataVm>(resp.result, "sources");
                        self.apply(Action::SourcesView {
                            generation: resp.generation,
                            result,
                        });
                    }
                }
                _ => {}
            }
        }
    }

    fn reset_submission_markers(&mut self) {
        self.cache.clear();
        self.submitted = None;
        self.submitted_hist_gen = None;
        self.submitted_detail_hist = None;
        self.submitted_facets.clear();
        self.submitted_runs = false;
        self.submitted_detail = None;
        self.submitted_groups = None;
        self.submitted_seek = None;
        self.submitted_dashboard = None;
        self.submitted_dashboard_hosts = None;
        self.submitted_hosts = None;
        self.submitted_lanes = None;
        self.submitted_trace = None;
        self.submitted_states = None;
        self.submitted_diff = None;
        self.submitted_iocs = None;
        self.submitted_ioc_hits = None;
        self.submitted_sources = None;
    }

    fn ensure_requests(&mut self) {
        if self.case_missing || self.startup_error.is_some() || self.worker.is_none() {
            return;
        }
        let generation = self.model.generation;
        if self.submitted != Some(generation) {
            self.submitted = Some(generation);
            self.cache.prune_stale_pending(generation);
            self.submit_scope_queries(generation);
        }
        self.submit_histogram_queries(generation);
        self.submit_facet_queries(generation);
        self.submit_page_queries(generation);
        self.submit_detail_queries();
        self.submit_group_and_seek();
        self.submit_view_queries(generation);
        self.submit_trace_queries();
    }

    fn submit_trace_queries(&mut self) {
        let Some(worker) = &self.worker else {
            return;
        };
        let Some(target) = self.model.pending_trace.clone() else {
            return;
        };
        let generation = self.model.trace_gen;
        if self
            .submitted_trace
            .as_ref()
            .is_some_and(|(t, g)| *t == target && *g == generation)
        {
            return;
        }
        self.submitted_trace = Some((target.clone(), generation));
        let id = worker.submit(
            ReqKind::Trace,
            Some(target.clone()),
            generation,
            move |store| {
                let tree = fp_correlate::trace::trace_tree(store, &target)?;
                Ok(Box::new(TraceVm::from_tree(&tree)))
            },
        );
        self.outstanding.insert(id);
    }

    fn submit_view_queries(&mut self, generation: Generation) {
        let Some(worker) = &self.worker else {
            return;
        };
        let filter = self.model.scope.filter();
        match self.model.view() {
            View::Dashboard => {
                if self.submitted_dashboard != Some(generation) {
                    self.submitted_dashboard = Some(generation);
                    let f = filter.clone();
                    let id = worker.submit(
                        ReqKind::Sources,
                        Some("dashboard".to_string()),
                        generation,
                        move |store| {
                            let summary = build_dashboard_summary(store, &f)?;
                            Ok(Box::new(summary))
                        },
                    );
                    self.outstanding.insert(id);
                }
                if self.submitted_dashboard_hosts != Some(generation) {
                    self.submitted_dashboard_hosts = Some(generation);
                    let id = worker.submit(
                        ReqKind::Hosts,
                        Some("dashboard".to_string()),
                        generation,
                        move |store| {
                            let rows = fp_query::overview::hosts_overview(
                                store,
                                &filter,
                                None,
                                HostSort::EventsDesc,
                                10,
                            )
                            .map_err(|e| QueryError::Message(e.to_string()))?;
                            let vms: Vec<HostSummaryVm> =
                                rows.iter().map(HostSummaryVm::from_overview).collect();
                            Ok(Box::new(vms))
                        },
                    );
                    self.outstanding.insert(id);
                }
            }
            View::Hosts => {
                let search = self.hosts_state.submitted_search.clone();
                let key = (generation, self.model.hosts_gen, search.clone());
                if self.submitted_hosts.as_ref() != Some(&key) {
                    self.submitted_hosts = Some(key);
                    let sort = self.model.host_sort;
                    let hosts_gen = self.model.hosts_gen;
                    let id = worker.submit(
                        ReqKind::Hosts,
                        Some("table".to_string()),
                        hosts_gen,
                        move |store| {
                            let substr = (!search.is_empty()).then_some(search.as_str());
                            let rows = fp_query::overview::hosts_overview(
                                store, &filter, substr, sort, HOSTS_CAP,
                            )
                            .map_err(|e| QueryError::Message(e.to_string()))?;
                            let vms: Vec<HostSummaryVm> =
                                rows.iter().map(HostSummaryVm::from_overview).collect();
                            Ok(Box::new(vms))
                        },
                    );
                    self.outstanding.insert(id);
                }
            }
            View::Lanes => {
                if self.model.bounds.status(generation) != LoadStatus::Ready {
                    return;
                }
                let bounds = self.model.bounds.value().copied().flatten();
                let width_px = self.lanes_state.last_width_px.max(64);
                let key = (generation, self.model.lanes_gen, width_px);
                if self.submitted_lanes != Some(key) {
                    self.submitted_lanes = Some(key);
                    let mode = self.model.lanes_mode;
                    let lanes_gen = self.model.lanes_gen;
                    let id = worker.submit(
                        ReqKind::Groups,
                        Some("lanes".to_string()),
                        lanes_gen,
                        move |store| {
                            let data = build_lanes_data(store, &filter, bounds, mode, width_px)?;
                            Ok(Box::new(data))
                        },
                    );
                    self.outstanding.insert(id);
                }
            }
            View::State => {
                let run = self.state_state.run;
                let search = self.state_state.submitted_search.clone();
                let states_gen = self.model.states_gen;
                let key = (states_gen, run, search.clone());
                if self.submitted_states.as_ref() != Some(&key) {
                    self.submitted_states = Some(key);
                    let id = worker.submit(ReqKind::State, None, states_gen, move |store| {
                        let run_id = match run {
                            Some(id) => id,
                            None => store.last_run_id()?.unwrap_or(0),
                        };
                        let match_text = (!search.is_empty()).then_some(search.as_str());
                        let rows = fp_query::state_records(store, Some(run_id), None, match_text)?;
                        let vms: Vec<StateRowVm> = rows.iter().map(StateRowVm::from_view).collect();
                        Ok(Box::new(vms))
                    });
                    self.outstanding.insert(id);
                }
            }
            View::Diff => {
                let Some((a, b)) = self.model.pending_diff else {
                    return;
                };
                let diff_gen = self.model.diff_gen;
                if self.submitted_diff != Some((a, b, diff_gen)) {
                    self.submitted_diff = Some((a, b, diff_gen));
                    let id = worker.submit(ReqKind::Diff, None, diff_gen, move |store| {
                        let entries = fp_query::diff_runs(store, a, b)?;
                        let vms: Vec<DiffEntryVm> =
                            entries.iter().map(DiffEntryVm::from_entry).collect();
                        Ok(Box::new(vms))
                    });
                    self.outstanding.insert(id);
                }
            }
            View::Ioc => {
                if self.submitted_iocs != Some(generation) {
                    self.submitted_iocs = Some(generation);
                    let id = worker.submit(ReqKind::Iocs, None, generation, |store| {
                        let rows = fp_query::iocs::ioc_summary(store)?;
                        let vms: Vec<crate::vm::IocSummaryVm> = rows
                            .iter()
                            .map(crate::vm::IocSummaryVm::from_summary)
                            .collect();
                        Ok(Box::new(vms))
                    });
                    self.outstanding.insert(id);
                }
                let selected = match self.model.selection() {
                    Selection::Ioc(id) => Some(id),
                    _ => None,
                };
                if self.submitted_ioc_hits != Some((generation, selected)) {
                    self.submitted_ioc_hits = Some((generation, selected));
                    let id = worker.submit(ReqKind::IocHits, None, generation, move |store| {
                        let hits = fp_query::iocs::ioc_hits_page(store, selected, 0, 500)?;
                        let iocs = fp_query::iocs::ioc_summary(store)?;
                        let vms: Vec<crate::vm::IocHitVm> = hits
                            .iter()
                            .map(|hit| {
                                let (kind, label) = resolve_hit_target(store, hit);
                                crate::vm::IocHitVm {
                                    ioc_id: hit.ioc_id,
                                    ioc_value: iocs
                                        .iter()
                                        .find(|s| s.ioc.id == hit.ioc_id)
                                        .map(|s| SafeText::new(&s.ioc.value))
                                        .unwrap_or_else(|| SafeText::new("?")),
                                    target_type: SafeText::new(&hit.target_type),
                                    target_id: hit.target_id,
                                    target_kind: SafeText::new(&kind),
                                    target_label: SafeText::new(&label),
                                    why: hit.why.as_deref().map(SafeText::new),
                                }
                            })
                            .collect();
                        Ok(Box::new(vms))
                    });
                    self.outstanding.insert(id);
                }
            }
            View::Sources if self.submitted_sources != Some(generation) => {
                self.submitted_sources = Some(generation);
                let id = worker.submit(ReqKind::Sources, None, generation, |store| {
                    let data = build_sources_view(store)?;
                    Ok(Box::new(data))
                });
                self.outstanding.insert(id);
            }
            _ => {}
        }
    }

    fn submit_scope_queries(&mut self, generation: Generation) {
        let Some(worker) = &self.worker else {
            return;
        };
        let filter = self.model.scope.filter();
        let count_filter = filter.clone();
        let id = worker.submit(ReqKind::Count, None, generation, move |store| {
            let in_case = store.event_count()?;
            let in_scope = store.count_events(&fp_store::EventFilter::from(&count_filter))?;
            let untimed_filter = fp_query::TimelineFilter {
                since: None,
                until: None,
                timing: fp_store::EventTiming::Untimed,
                ..count_filter.clone()
            };
            let untimed = store.count_events(&fp_store::EventFilter::from(&untimed_filter))?;
            Ok(Box::new((Counts { in_scope, in_case }, untimed)))
        });
        self.outstanding.insert(id);

        let id = worker.submit(ReqKind::Bounds, None, generation, |store| {
            Ok(Box::new(store.time_bounds()?))
        });
        self.outstanding.insert(id);

        if !self.submitted_runs {
            self.submitted_runs = true;
            let id = worker.submit(ReqKind::Runs, None, generation, |store| {
                let markers: Vec<RunMarkerVm> = fp_query::runs(store)?
                    .iter()
                    .map(RunMarkerVm::from_view)
                    .collect();
                Ok(Box::new(markers))
            });
            self.outstanding.insert(id);
        }

        let cat_filter = filter.clone();
        let id = worker.submit(
            ReqKind::Facets,
            Some("category".to_string()),
            generation,
            move |store| {
                let counts = fp_query::overview::category_counts(store, &cat_filter)
                    .map_err(|e| QueryError::Message(e.to_string()))?;
                Ok(Box::new(counts))
            },
        );
        self.outstanding.insert(id);

        let timing_filter = fp_query::TimelineFilter {
            timing: fp_store::EventTiming::Any,
            ..filter.clone()
        };
        let id = worker.submit(
            ReqKind::Facets,
            Some("timing".to_string()),
            generation,
            move |store| {
                let counts = fp_query::overview::timing_counts(store, &timing_filter)
                    .map_err(|e| QueryError::Message(e.to_string()))?;
                Ok(Box::new(counts))
            },
        );
        self.outstanding.insert(id);

        let ioc_filter = fp_query::TimelineFilter {
            ioc_only: true,
            ..filter
        };
        let id = worker.submit(
            ReqKind::Facets,
            Some("ioc_count".to_string()),
            generation,
            move |store| {
                let count = store.count_events(&fp_store::EventFilter::from(&ioc_filter))?;
                Ok(Box::new(count))
            },
        );
        self.outstanding.insert(id);
    }

    fn submit_histogram_queries(&mut self, generation: Generation) {
        let Some(Some((case_start, case_end))) = self.model.bounds.value().copied() else {
            return;
        };
        let Some(worker) = &self.worker else {
            return;
        };
        let width_px = self.strip_state.last_width_px.max(64);
        let filter = self.model.scope.filter();

        if self.submitted_hist_gen != Some((generation, width_px)) {
            self.submitted_hist_gen = Some((generation, width_px));
            let base = fp_query::TimelineFilter {
                since: None,
                until: None,
                ..filter.clone()
            };
            let window = TimeWindow {
                start_utc: case_start,
                end_utc: case_end.max(case_start),
            };
            let base_clone = base.clone();
            let id = worker.submit(
                ReqKind::Histogram,
                Some("minimap".to_string()),
                generation,
                move |store| {
                    let h = fp_query::overview::histogram(store, &base_clone, window, width_px)
                        .map_err(|e| QueryError::Message(e.to_string()))?;
                    Ok(Box::new(h))
                },
            );
            self.outstanding.insert(id);

            let ioc_base = fp_query::TimelineFilter {
                ioc_only: true,
                ..base
            };
            let id = worker.submit(
                ReqKind::Histogram,
                Some("ioc".to_string()),
                generation,
                move |store| {
                    let h = fp_query::overview::histogram(store, &ioc_base, window, width_px)
                        .map_err(|e| QueryError::Message(e.to_string()))?;
                    Ok(Box::new(h))
                },
            );
            self.outstanding.insert(id);
        }

        if filter.since.is_some() || filter.until.is_some() {
            let ws = filter.since.unwrap_or(case_start);
            let we = filter.until.unwrap_or(case_end).max(ws);
            let key = (generation, ws, we, width_px);
            if self.submitted_detail_hist != Some(key) {
                self.submitted_detail_hist = Some(key);
                let window = TimeWindow {
                    start_utc: ws,
                    end_utc: we,
                };
                let id = worker.submit(
                    ReqKind::Histogram,
                    Some("detail".to_string()),
                    generation,
                    move |store| {
                        let h = fp_query::overview::histogram(store, &filter, window, width_px)
                            .map_err(|e| QueryError::Message(e.to_string()))?;
                        Ok(Box::new(h))
                    },
                );
                self.outstanding.insert(id);
            }
        }
    }

    fn submit_facet_queries(&mut self, generation: Generation) {
        if self.settings.panels.facets_collapsed {
            return;
        }
        let Some(worker) = &self.worker else {
            return;
        };
        let base_filter = self.model.scope.filter();
        for facet in self.facets_state.expanded_facets() {
            if !self.submitted_facets.insert((generation, facet)) {
                continue;
            }
            let scoped = facet_minus_self_filter(&base_filter, facet);
            let sub = facet_sub(facet).to_string();
            let id = worker.submit(ReqKind::Facets, Some(sub), generation, move |store| {
                let page = fp_query::overview::facets(
                    store,
                    &scoped,
                    facet,
                    fp_query::overview::MAX_FACET_ROWS,
                )
                .map_err(|e| QueryError::Message(e.to_string()))?;
                Ok(Box::new(page))
            });
            self.outstanding.insert(id);
        }
    }

    fn submit_page_queries(&mut self, generation: Generation) {
        if self.model.view() != View::Timeline || self.model.group_by.is_some() {
            return;
        }
        let Some(worker) = &self.worker else {
            return;
        };
        let scope_hash = self.model.scope.hash();
        let reverse = self.model.reverse;
        let blocks = std::mem::take(&mut self.table_state.needed_blocks);
        for block_idx in blocks {
            let key = (scope_hash, reverse, block_idx);
            if !self.cache.mark_pending(key, generation) {
                continue;
            }
            let cursor = self.cache.prev_block_cursor(scope_hash, reverse, block_idx);
            let mut filter = self.model.scope.filter();
            filter.reverse = reverse;
            let sub = BlockCache::encode_sub(key);
            let id = worker.submit(ReqKind::Page, Some(sub), generation, move |store| {
                let page = match cursor {
                    Some(c) => fp_query::timeline_after(store, &filter, Some(c), BLOCK_SIZE)?,
                    None => {
                        fp_query::timeline_page(store, &filter, block_idx * BLOCK_SIZE, BLOCK_SIZE)?
                    }
                };
                let rows: Vec<RowVm> = page.rows.iter().map(RowVm::from_row).collect();
                Ok(Box::new(rows))
            });
            self.outstanding.insert(id);
        }
    }

    fn submit_detail_queries(&mut self) {
        let sel_gen = self.model.selection_gen;
        if self.submitted_detail == Some(sel_gen) {
            return;
        }
        self.submitted_detail = Some(sel_gen);
        let Selection::Event(id) = self.model.inspected() else {
            return;
        };
        let Some(worker) = &self.worker else {
            return;
        };
        let req_id = worker.submit(
            ReqKind::Detail,
            Some(id.to_string()),
            sel_gen,
            move |store| {
                let view = fp_query::event_detail(store, id)?;
                Ok(Box::new(
                    view.as_ref().map(|v| Box::new(DetailVm::from_view(v))),
                ))
            },
        );
        self.outstanding.insert(req_id);

        let scope = self.model.scope.filter();
        let req_id = worker.submit(
            ReqKind::Around,
            Some(id.to_string()),
            sel_gen,
            move |store| {
                let anchor_ts = store.get_event(id)?.map(|e| e.ts_utc).unwrap_or(0);
                let rows = fp_query::events_around(store, id, 25, 25, Some(&scope))?;
                let nearby: Vec<RowVm> = rows
                    .iter()
                    .filter(|r| {
                        r.ts_precision != "order_only"
                            && r.ts_utc.abs_diff(anchor_ts) <= 300_000_000
                    })
                    .map(RowVm::from_row)
                    .collect();
                Ok(Box::new(nearby))
            },
        );
        self.outstanding.insert(req_id);
    }

    fn submit_group_and_seek(&mut self) {
        let Some(worker) = &self.worker else {
            return;
        };
        if let Some(by) = self.model.group_by
            && self.submitted_groups != Some(self.model.group_gen)
        {
            self.submitted_groups = Some(self.model.group_gen);
            let filter = self.model.scope.filter();
            let tz = self.tz;
            let group_gen = self.model.group_gen;
            let sub = format!("{by:?}");
            let id = worker.submit(ReqKind::Groups, Some(sub), group_gen, move |store| {
                let rows = fp_query::overview::group(store, &filter, by, Some(500), tz)
                    .map_err(|e| QueryError::Message(e.to_string()))?;
                let vms: Vec<GroupRowVm> = rows.iter().map(GroupRowVm::from_row).collect();
                Ok(Box::new(vms))
            });
            self.outstanding.insert(id);
        }

        if let Some(ts) = self.model.pending_seek
            && self.submitted_seek != Some(self.model.seek_gen)
        {
            self.submitted_seek = Some(self.model.seek_gen);
            let filter = self.model.scope.filter();
            let reverse = self.model.reverse;
            let seek_gen = self.model.seek_gen;
            let id = worker.submit(ReqKind::Seek, None, seek_gen, move |store| {
                let offset = if reverse {
                    let after_ts = ts.saturating_add(1);
                    let since = filter.since.map_or(after_ts, |s| s.max(after_ts));
                    let f = fp_query::TimelineFilter {
                        since: Some(since),
                        ..filter
                    };
                    store.count_events(&fp_store::EventFilter::from(&f))?
                } else {
                    fp_query::seek_offset(store, &filter, ts)?
                };
                Ok(Box::new(offset))
            });
            self.outstanding.insert(id);
        }
    }

    fn shortcuts(&mut self, ui: &egui::Ui) {
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            if self.chip_editor.is_open() {
                self.chip_editor.close();
            } else if self.model.selection() != Selection::None {
                self.apply(Action::Select(Selection::None));
            }
        }
        if ui.ctx().egui_wants_keyboard_input() {
            return;
        }
        let pressed =
            |ui: &egui::Ui, key: egui::Key| ui.input(|i| i.key_pressed(key) && i.modifiers.ctrl);
        if pressed(ui, egui::Key::Z) {
            self.apply(Action::UndoScope);
        }
        if pressed(ui, egui::Key::Y) {
            self.apply(Action::RedoScope);
        }
        if ui.input(|i| i.key_pressed(egui::Key::C) && i.modifiers.ctrl && i.modifiers.shift) {
            ui.ctx().copy_text(self.model.scope.to_cli());
        }
        if ui
            .input(|i| i.key_pressed(egui::Key::Backspace) && i.modifiers.ctrl && i.modifiers.shift)
        {
            self.apply(Action::Scope(ScopeAction::Clear));
        }
        if ui.input(|i| i.key_pressed(egui::Key::Slash) && !i.modifiers.any()) {
            self.focus_search_next = true;
        }
        if ui.input(|i| i.key_pressed(egui::Key::F) && !i.modifiers.any()) {
            self.settings.panels.facets_collapsed = !self.settings.panels.facets_collapsed;
        }
        if ui.input(|i| i.key_pressed(egui::Key::I) && !i.modifiers.any()) {
            self.settings.panels.inspector_collapsed = !self.settings.panels.inspector_collapsed;
        }
        if ui.input(|i| i.key_pressed(egui::Key::K) && i.modifiers.ctrl && !i.modifiers.shift) {
            palette_toggle(&mut self.palette);
        }
        if ui.input(|i| i.key_pressed(egui::Key::F1))
            || ui.input(|i| i.key_pressed(egui::Key::Slash) && i.modifiers.shift)
        {
            self.help_open = !self.help_open;
        }
        if ui.input(|i| i.modifiers.alt && i.key_pressed(egui::Key::ArrowLeft)) {
            self.apply(Action::NavBack);
        }
        if ui.input(|i| i.modifiers.alt && i.key_pressed(egui::Key::ArrowRight)) {
            self.apply(Action::NavForward);
        }
        if ui.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::Minus)) {
            self.zoom_by(ui, -0.1);
        }
        if ui.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::Equals)) {
            self.zoom_by(ui, 0.1);
        }
        if ui.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::Num0)) {
            self.settings.ui_scale = 1.0;
            apply_theme(self.theme, self.settings.ui_scale, ui.ctx());
        }
        for (i, key) in [
            egui::Key::Num1,
            egui::Key::Num2,
            egui::Key::Num3,
            egui::Key::Num4,
            egui::Key::Num5,
            egui::Key::Num6,
            egui::Key::Num7,
            egui::Key::Num8,
            egui::Key::Num9,
            egui::Key::Num0,
        ]
        .iter()
        .enumerate()
        {
            if ui.input(|i| !i.modifiers.ctrl && !i.modifiers.alt && i.key_pressed(*key))
                && let Some(view) = View::from_shortcut(i)
            {
                self.apply(Action::Go(view));
            }
        }
    }

    fn zoom_by(&mut self, ui: &egui::Ui, delta: f32) {
        self.settings.ui_scale = (self.settings.ui_scale + delta).clamp(0.8, 1.6);
        apply_theme(self.theme, self.settings.ui_scale, ui.ctx());
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("footprint").strong());
            let name = self
                .case
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| self.case.display().to_string());
            if ui
                .button(name)
                .on_hover_text(format!("{} — click to copy the path", self.case.display()))
                .clicked()
            {
                ui.ctx().copy_text(self.case.display().to_string());
            }
            ui.add_enabled_ui(false, |ui| {
                let _ = ui.button("run: all");
            })
            .response
            .on_hover_text("run filtering arrives with the sources view");
            let response = ui.add_sized(
                [280.0, 20.0],
                egui::TextEdit::singleline(&mut self.search)
                    .hint_text("search subjects, hosts, commands… (/)"),
            );
            if std::mem::take(&mut self.focus_search_next) {
                response.request_focus();
            }
            if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                let text = (!self.search.is_empty()).then(|| self.search.clone());
                self.apply(Action::Scope(ScopeAction::SetText(text)));
                self.search.clear();
                response.surrender_focus();
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button("Collect")
                    .on_hover_text("collect arrives with the collect view")
                    .clicked()
                {
                    self.apply(Action::Go(View::Collect));
                }
                let theme_label = match self.theme {
                    ThemeName::Dark => "theme: dark",
                    ThemeName::Light => "theme: light",
                    ThemeName::HighContrast => "theme: contrast",
                };
                if ui
                    .button(theme_label)
                    .on_hover_text("switch theme")
                    .clicked()
                {
                    self.theme = match self.theme {
                        ThemeName::Dark => ThemeName::Light,
                        ThemeName::Light => ThemeName::HighContrast,
                        ThemeName::HighContrast => ThemeName::Dark,
                    };
                    self.settings.theme = self.theme;
                    apply_theme(self.theme, self.settings.ui_scale, ui.ctx());
                }
                let tz_resp = egui::ComboBox::from_id_salt("tz_combo")
                    .selected_text(self.tz_label())
                    .width(74.0)
                    .show_ui(ui, |ui| {
                        for preset in ["local", "UTC", "+03:30", "+00:00", "-05:00", "+09:00"] {
                            let selected = self.tz_label() == preset;
                            if ui.selectable_label(selected, preset).clicked()
                                && let Ok(parsed) = fp_query::parse_tz(preset)
                            {
                                self.tz = parsed;
                                self.settings.tz = preset.to_string();
                                self.model.group_gen += 1;
                            }
                        }
                    });
                tz_resp.response.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::Role::ComboBox, true, "Time zone")
                });
                if ui
                    .small_button("export")
                    .on_hover_text("write the events in the current scope to a file")
                    .clicked()
                {
                    self.export_dialog.open_default(&self.case);
                }
                if ui
                    .selectable_label(!self.settings.panels.inspector_collapsed, "inspector")
                    .on_hover_text("toggle inspector (i)")
                    .clicked()
                {
                    self.settings.panels.inspector_collapsed =
                        !self.settings.panels.inspector_collapsed;
                }
                if self.model.view() == View::Timeline
                    && ui
                        .selectable_label(!self.settings.panels.facets_collapsed, "facets")
                        .on_hover_text("toggle facets (f)")
                        .clicked()
                {
                    self.settings.panels.facets_collapsed = !self.settings.panels.facets_collapsed;
                }
            });
        });
    }

    fn tz_label(&self) -> String {
        match self.tz {
            fp_query::Tz::Local => "local".to_string(),
            fp_query::Tz::Utc => "UTC".to_string(),
            fp_query::Tz::Fixed(offset) => offset.to_string(),
        }
    }

    fn scope_bar(&mut self, ui: &mut egui::Ui) {
        let counts = self.model.counts.value().map(|c| (c.in_scope, c.in_case));
        let mut actions = Vec::new();
        show_scope_bar(
            ui,
            &self.model.scope,
            counts,
            self.tz,
            &mut self.chip_editor,
            &mut actions,
        );
        self.apply_all(actions);
    }

    fn banners(&mut self, ui: &mut egui::Ui) {
        let settings_warning = self.settings_warning.clone();
        ui.horizontal_wrapped(|ui| {
            if let Some(warning) = settings_warning {
                ui.colored_label(ui.visuals().warn_fg_color, warning);
                if ui.small_button("×").clicked() {
                    self.settings_warning = None;
                }
            } else if self.model.watch.case_writing() {
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    "case is being written; views refresh when the run finishes",
                );
            } else if self.model.watch.case_changed() {
                ui.colored_label(ui.visuals().warn_fg_color, "case changed (new run)");
                if ui.button("Refresh").clicked() {
                    self.apply(Action::RefreshCase);
                    self.reset_submission_markers();
                }
            } else if let Some(error) = self.fresh_error() {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
        });
    }

    /// a query error for the generation currently on screen, if any
    fn fresh_error(&self) -> Option<String> {
        let generation = self.model.generation;
        let counts = &self.model.counts;
        let bounds = &self.model.bounds;
        for (slot_generation, status) in [
            (counts.generation(), counts.status(generation)),
            (bounds.generation(), bounds.status(generation)),
        ] {
            if slot_generation == generation
                && let LoadStatus::Error(message) = status
            {
                return Some(message);
            }
        }
        self.model.seek_error.clone()
    }

    fn has_banners(&self) -> bool {
        self.settings_warning.is_some()
            || self.model.watch.case_writing()
            || self.model.watch.case_changed()
            || self.fresh_error().is_some()
    }

    fn rail(&mut self, ui: &mut egui::Ui) {
        for (i, view) in View::ALL.iter().enumerate() {
            let selected = self.model.view() == *view;
            if ui
                .selectable_label(selected, format!("{}  {}", i + 1, view.label()))
                .clicked()
            {
                self.apply(Action::Go(*view));
            }
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            match self.model.counts.status(self.model.generation) {
                LoadStatus::Loading => {
                    ui.spinner();
                    ui.label("counting events…");
                }
                LoadStatus::Stale => {
                    ui.spinner();
                    if let Some(counts) = self.model.counts.value() {
                        ui.label(
                            egui::RichText::new(format!(
                                "{} events in scope · {} in case",
                                thousands(counts.in_scope),
                                thousands(counts.in_case)
                            ))
                            .weak(),
                        );
                    }
                }
                LoadStatus::Ready => {
                    if let Some(counts) = self.model.counts.value() {
                        ui.label(format!(
                            "{} events in scope · {} in case",
                            thousands(counts.in_scope),
                            thousands(counts.in_case)
                        ));
                    }
                }
                LoadStatus::Error(_) => {
                    ui.label(egui::RichText::new("count failed").weak());
                }
            }
            if let Some(hidden) = self.model.untimed_hidden() {
                ui.label(format!("· {hidden} untimed hidden by the time window"));
                if ui
                    .link("show")
                    .on_hover_text("drop the window and view the untimed events")
                    .clicked()
                {
                    self.apply(Action::Scope(ScopeAction::ShowUntimed));
                }
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.outstanding.is_empty() {
                    ui.label(egui::RichText::new("worker idle").weak());
                } else {
                    ui.spinner();
                    ui.label("worker busy");
                }
            });
        });
    }

    fn main(&mut self, ui: &mut egui::Ui) {
        if self.case_missing {
            ui.centered_and_justified(|ui| {
                ui.vertical_centered(|ui| {
                    ui.heading("no case yet");
                    ui.label(format!("{}", self.case.display()));
                    ui.label(
                        "run `footprint collect` to create it — the GUI picks it up automatically",
                    );
                });
            });
            return;
        }
        if self.model.counts.value().is_some_and(|c| c.in_case == 0) {
            ui.centered_and_justified(|ui| {
                ui.vertical_centered(|ui| {
                    ui.heading("this case has no events yet");
                    ui.label("run `footprint collect`, then watch the totals appear");
                });
            });
            return;
        }
        match self.model.view() {
            View::Dashboard => {
                let theme = self.active_theme();
                let ctx = crate::widgets::ViewCtx {
                    model: &self.model,
                    theme: &theme,
                    tz: self.tz,
                };
                let case_size = std::fs::metadata(&self.case).ok().map(|m| m.len());
                let mut actions = Vec::new();
                show_dashboard(
                    ui,
                    &ctx,
                    &self.case,
                    case_size,
                    &mut self.dashboard_state,
                    &mut actions,
                );
                self.apply_all(actions);
            }
            View::Timeline => {
                let theme = self.active_theme();
                let ctx = crate::widgets::ViewCtx {
                    model: &self.model,
                    theme: &theme,
                    tz: self.tz,
                };
                let mut actions = Vec::new();
                crate::views::timeline::show_timeline(
                    ui,
                    &ctx,
                    &mut self.cache,
                    &mut self.settings,
                    &mut self.strip_state,
                    &mut self.table_state,
                    &mut actions,
                );
                self.apply_all(actions);
            }
            View::Hosts => {
                let theme = self.active_theme();
                let ctx = crate::widgets::ViewCtx {
                    model: &self.model,
                    theme: &theme,
                    tz: self.tz,
                };
                let mut actions = Vec::new();
                show_hosts(ui, &ctx, &mut self.hosts_state, &mut actions);
                self.apply_all(actions);
            }
            View::Lanes => {
                let theme = self.active_theme();
                let ctx = crate::widgets::ViewCtx {
                    model: &self.model,
                    theme: &theme,
                    tz: self.tz,
                };
                let mut actions = Vec::new();
                show_lanes(ui, &ctx, &mut self.lanes_state, &mut actions);
                self.apply_all(actions);
            }
            View::Trace => {
                let theme = self.active_theme();
                let ctx = crate::widgets::ViewCtx {
                    model: &self.model,
                    theme: &theme,
                    tz: self.tz,
                };
                let mut actions = Vec::new();
                show_trace(ui, &ctx, &mut self.trace_state, &mut actions);
                self.apply_all(actions);
            }
            View::State => {
                let theme = self.active_theme();
                let ctx = crate::widgets::ViewCtx {
                    model: &self.model,
                    theme: &theme,
                    tz: self.tz,
                };
                let mut actions = Vec::new();
                show_state(ui, &ctx, &mut self.state_state, &mut actions);
                self.apply_all(actions);
            }
            View::Diff => {
                let theme = self.active_theme();
                let ctx = crate::widgets::ViewCtx {
                    model: &self.model,
                    theme: &theme,
                    tz: self.tz,
                };
                let mut actions = Vec::new();
                show_diff(ui, &ctx, &mut self.diff_state, &mut actions);
                self.apply_all(actions);
            }
            View::Ioc => {
                let theme = self.active_theme();
                let ctx = crate::widgets::ViewCtx {
                    model: &self.model,
                    theme: &theme,
                    tz: self.tz,
                };
                let mut actions = Vec::new();
                show_iocs(ui, &ctx, &mut self.ioc_state, &mut actions);
                self.apply_all(actions);
            }
            View::Sources => {
                let theme = self.active_theme();
                let ctx = crate::widgets::ViewCtx {
                    model: &self.model,
                    theme: &theme,
                    tz: self.tz,
                };
                let mut actions = Vec::new();
                show_sources(ui, &ctx, &mut actions);
                self.apply_all(actions);
            }
            View::Collect => {
                let theme = self.active_theme();
                let ctx = crate::widgets::ViewCtx {
                    model: &self.model,
                    theme: &theme,
                    tz: self.tz,
                };
                let mut actions = Vec::new();
                show_collect(ui, &ctx, &mut self.collect_state, &mut actions);
                self.apply_all(actions);
            }
        }
    }

    pub fn show_ui(&mut self, ui: &mut egui::Ui) {
        self.drain_worker();
        self.drain_collect();
        if let Some(error) = self.startup_error.clone() {
            ui.centered_and_justified(|ui| {
                ui.vertical_centered(|ui| {
                    ui.colored_label(ui.visuals().error_fg_color, "cannot open the case");
                    ui.monospace(self.case.display().to_string());
                    ui.label(error);
                    ui.label(
                        "run `footprint collect` to start a new case, or open a different one",
                    );
                });
            });
            return;
        }
        self.ensure_requests();
        self.shortcuts(ui);

        egui::Panel::top("top_bar").show(ui, |ui| self.top_bar(ui));
        egui::Panel::top("scope_bar").show(ui, |ui| self.scope_bar(ui));
        if self.has_banners() {
            egui::Panel::top("banners").show(ui, |ui| self.banners(ui));
        }
        egui::Panel::bottom("status_bar").show(ui, |ui| self.status_bar(ui));
        egui::Panel::left("rail")
            .resizable(false)
            .default_size(96.0)
            .show(ui, |ui| self.rail(ui));

        let show_facets_panel = self.model.view() == View::Timeline
            && !self.settings.panels.facets_collapsed
            && !self.case_missing;
        if show_facets_panel {
            let theme = self.active_theme();
            let resp = egui::Panel::left("facets")
                .resizable(true)
                .default_size(self.settings.panels.facets_width)
                .min_size(160.0)
                .max_size(420.0)
                .show(ui, |ui| {
                    let mut actions = Vec::new();
                    show_facets(
                        ui,
                        &self.model,
                        &theme,
                        &mut self.facets_state,
                        &mut actions,
                    );
                    actions
                });
            self.settings.panels.facets_width = resp.response.rect.width();
            self.apply_all(resp.inner);
        }

        if !self.settings.panels.inspector_collapsed && !self.case_missing {
            let theme = self.active_theme();
            let resp = egui::Panel::right("inspector")
                .resizable(true)
                .default_size(self.settings.panels.inspector_width)
                .min_size(240.0)
                .max_size(560.0)
                .show(ui, |ui| {
                    let mut actions = Vec::new();
                    show_inspector(ui, &self.model, &theme, self.tz, &mut actions);
                    actions
                });
            self.settings.panels.inspector_width = resp.response.rect.width();
            self.apply_all(resp.inner);
        }

        egui::CentralPanel::default().show(ui, |ui| self.main(ui));
        {
            let theme = self.active_theme();
            let mut actions = Vec::new();
            let in_scope = self.model.counts.value().map(|c| c.in_scope);
            show_export_dialog(
                ui.ctx(),
                &theme,
                &mut self.export_dialog,
                in_scope,
                &mut actions,
            );
            let hosts: Vec<String> = self
                .model
                .hosts
                .value()
                .map(|rows| rows.iter().map(|h| h.host.as_str().to_string()).collect())
                .unwrap_or_default();
            show_palette(ui.ctx(), &theme, &mut self.palette, &hosts, &mut actions);
            show_help(ui.ctx(), &theme, &mut self.help_open);
            self.apply_all(actions);
        }
        self.ensure_requests();
    }
}

/// Resolves an IOC hit target to (kind, label) for display; unresolved targets
/// fall back to the target type and an empty label.
fn resolve_hit_target(store: &fp_store::Store, hit: &fp_store::IocHitLink) -> (String, String) {
    if hit.target_type == "event"
        && let Ok(Some(ev)) = store.get_event(hit.target_id)
    {
        return (ev.kind, ev.subject.unwrap_or_default());
    }
    (hit.target_type.clone(), String::new())
}

/// Coverage cards plus the per-source, per-run health matrix for the Sources view.
fn build_sources_view(store: &fp_store::Store) -> Result<SourcesDataVm, QueryError> {
    let runs = store.list_runs()?;
    let run_ids: Vec<u64> = runs.iter().map(|r| r.id).collect();
    let all_sources = fp_query::sources(store, None)?;
    let coverage = fp_query::coverage(&all_sources)
        .into_iter()
        .map(|row| SourcesDataVm::coverage_card(&row))
        .collect();
    let mut rows: Vec<SourceHealthVm> = Vec::new();
    for source in &all_sources {
        if let Some(existing) = rows
            .iter_mut()
            .find(|r| r.label.as_str() == source.label() && r.kind.as_str() == source.kind)
        {
            if let Some(pos) = run_ids.iter().position(|id| *id == source.run_id) {
                existing.cells[pos] = source.status.clone();
            }
        } else {
            let mut cells = vec![None; run_ids.len()];
            if let Some(pos) = run_ids.iter().position(|id| *id == source.run_id) {
                cells[pos] = source.status.clone();
            }
            rows.push(SourceHealthVm {
                source_id: source.id,
                label: SafeText::new(&source.label()),
                kind: SafeText::new(&source.kind),
                cells,
            });
        }
    }
    Ok(SourcesDataVm {
        coverage,
        health_rows: rows,
        run_ids,
    })
}

fn build_dashboard_summary(
    store: &fp_store::Store,
    filter: &fp_query::TimelineFilter,
) -> Result<DashboardSummaryVm, QueryError> {
    let runs = store.list_runs()?;
    let host_name = runs
        .last()
        .and_then(|r| r.host.as_deref())
        .map(SafeText::new);
    let user_name = runs
        .last()
        .and_then(|r| r.user.as_deref())
        .map(SafeText::new);

    let ioc_filter = fp_query::TimelineFilter {
        ioc_only: true,
        ..filter.clone()
    };
    let ioc_event_hits = store.count_events(&fp_store::EventFilter::from(&ioc_filter))?;
    let sample_ioc_hosts =
        fp_query::overview::hosts_overview(store, &ioc_filter, None, HostSort::EventsDesc, 5)
            .map_err(|e| QueryError::Message(e.to_string()))?
            .into_iter()
            .map(|h| SafeText::new(&h.summary.host))
            .collect();

    let state_rows = fp_query::state_records(store, filter.run_id, None, None)
        .map_err(|e| QueryError::Message(e.to_string()))?;
    let mut flagged_state_count = 0u64;
    let mut flagged_state_kinds = Vec::new();
    for row in &state_rows {
        let has_flags = row
            .parsed
            .as_ref()
            .and_then(|v| v.get("flags"))
            .and_then(|f| f.as_array())
            .is_some_and(|a| !a.is_empty());
        if has_flags {
            flagged_state_count += 1;
            if !flagged_state_kinds
                .iter()
                .any(|k: &SafeText| k.as_str() == row.kind)
            {
                flagged_state_kinds.push(SafeText::new(&row.kind));
            }
        }
    }

    let source_rows =
        fp_query::sources(store, filter.run_id).map_err(|e| QueryError::Message(e.to_string()))?;
    let mut source_warnings = Vec::new();
    for src in &source_rows {
        if let Some(status) = &src.status
            && !matches!(
                status,
                fp_core::Availability::Present
                    | fp_core::Availability::NotInstalled
                    | fp_core::Availability::Empty
            )
        {
            source_warnings.push(SourceIssueVm::new(&src.label(), &status.to_string()));
        }
    }
    let cov = fp_query::coverage(&source_rows);
    let coverage_total = cov.len();
    let coverage_covered = cov
        .iter()
        .filter(|c| matches!(c.status, fp_query::CoverageStatus::Covered))
        .count();

    let last_diff = if runs.len() >= 2 {
        let r_a = runs[runs.len() - 2].id;
        let r_b = runs[runs.len() - 1].id;
        let diff =
            fp_query::diff_runs(store, r_a, r_b).map_err(|e| QueryError::Message(e.to_string()))?;
        let mut added = 0;
        let mut removed = 0;
        let mut changed = 0;
        for entry in diff {
            match entry.change {
                fp_query::DiffChange::Added => added += 1,
                fp_query::DiffChange::Removed => removed += 1,
                fp_query::DiffChange::Changed => changed += 1,
            }
        }
        Some((r_a, r_b, added, removed, changed))
    } else {
        None
    };

    Ok(DashboardSummaryVm {
        host_name,
        user_name,
        ioc_event_hits,
        sample_ioc_hosts,
        flagged_state_count,
        flagged_state_kinds,
        source_warnings,
        coverage_covered,
        coverage_total,
        last_diff,
    })
}

fn build_lanes_data(
    store: &fp_store::Store,
    filter: &fp_query::TimelineFilter,
    bounds: Option<(i64, i64)>,
    mode: LaneMode,
    width_px: usize,
) -> Result<LanesDataVm, QueryError> {
    const MAX_INDIVIDUAL_EVENTS: u64 = 5_000;
    let (case_start, case_end) = bounds.unwrap_or((0, 1));
    let span_end = case_end.max(case_start.saturating_add(1));
    let win_start = filter.since.unwrap_or(case_start);
    let win_end = filter
        .until
        .unwrap_or(span_end)
        .max(win_start.saturating_add(1));
    let window = TimeWindow {
        start_utc: win_start,
        end_utc: win_end,
    };

    let timed_filter = fp_query::TimelineFilter {
        timing: fp_store::EventTiming::Timed,
        reverse: false,
        ..filter.clone()
    };
    let total_timed = store.count_events(&fp_store::EventFilter::from(&timed_filter))?;

    let untimed_filter = fp_query::TimelineFilter {
        since: None,
        until: None,
        timing: fp_store::EventTiming::Untimed,
        ..filter.clone()
    };
    let untimed_total = store.count_events(&fp_store::EventFilter::from(&untimed_filter))?;
    let untimed_lanes = if mode == LaneMode::Category {
        let counts = fp_query::overview::category_counts(store, &untimed_filter)
            .map_err(|e| QueryError::Message(e.to_string()))?;
        fp_query::category::Category::ALL
            .iter()
            .enumerate()
            .filter(|&(i, _)| counts[i] > 0)
            .map(|(i, c)| UntimedLaneVm::new(c.label(), counts[i], true))
            .collect()
    } else {
        let page = fp_query::overview::facets(
            store,
            &untimed_filter,
            Facet::Source,
            fp_query::overview::MAX_FACET_ROWS,
        )
        .map_err(|e| QueryError::Message(e.to_string()))?;
        page.values
            .into_iter()
            .map(|r| UntimedLaneVm::new(r.key.as_deref().unwrap_or("(none)"), r.count, false))
            .collect()
    };

    if total_timed > MAX_INDIVIDUAL_EVENTS {
        let hist = fp_query::overview::lane_histogram(store, filter, window, width_px, mode)
            .map_err(|e| QueryError::Message(e.to_string()))?;
        let density_lanes = hist.lanes.iter().map(LaneRowDensityVm::from_row).collect();
        return Ok(LanesDataVm {
            mode,
            window,
            total_timed,
            truncated_to_density: true,
            bucket_bounds: hist.bucket_bounds,
            density_lanes,
            events: Vec::new(),
            untimed_total,
            untimed_lanes,
        });
    }

    let list_filter = fp_query::TimelineFilter {
        limit: Some(MAX_INDIVIDUAL_EVENTS as usize),
        ..timed_filter
    };
    let raw = fp_query::timeline(store, &list_filter)?;
    let events = raw.iter().map(RowVm::from_row).collect();
    Ok(LanesDataVm {
        mode,
        window,
        total_timed,
        truncated_to_density: false,
        bucket_bounds: Vec::new(),
        density_lanes: Vec::new(),
        events,
        untimed_total,
        untimed_lanes,
    })
}

impl eframe::App for Shell {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.show_ui(ui);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.settings.theme = self.theme;
        self.settings.last_view = Some(self.model.view().label().to_lowercase());
        // a failure to write settings is non-fatal
        let _ = self.settings.save();
    }
}
