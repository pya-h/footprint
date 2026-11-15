//! View-models: the only types widgets draw. Every string crosses the
//! sanitization boundary here, so hostile evidence cannot reach a `Ui::label`
//! by construction.

use std::fmt;

use fp_core::Confidence;
use fp_query::category::Category;
use fp_query::overview::{GroupRow, HostOverview, LaneHistogramRow, LaneMode, TimeWindow};
use fp_query::{EventDetailView, LinkDirection, LinkView, RunView, TimelineRow};

/// Text that has passed through `fp_render::sanitize`; widgets only ever see this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafeText(String);

impl SafeText {
    pub fn new(raw: &str) -> Self {
        Self(fp_render::sanitize(raw))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Display for SafeText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone)]
pub struct RowVm {
    pub id: u64,
    pub run_id: u64,
    pub ts_utc: i64,
    /// order-only events have no instant; they render as "untimed" everywhere
    pub untimed: bool,
    pub category: fp_query::category::Category,
    pub kind: SafeText,
    /// `app/profile` for browser-like sources, the app or kind otherwise
    pub source: SafeText,
    pub subject: SafeText,
    pub subject_host: SafeText,
    pub summary: SafeText,
    pub confidence: Confidence,
    pub ioc_hits: Vec<SafeText>,
}

impl RowVm {
    pub fn from_row(row: &TimelineRow) -> Self {
        let kind = SafeText::new(&row.kind);
        Self {
            id: row.id,
            run_id: row.run_id,
            ts_utc: row.ts_utc,
            untimed: row.ts_precision == "order_only",
            category: fp_query::category::category(kind.as_str()),
            source: SafeText::new(&row.source_label()),
            subject: SafeText::new(row.subject.as_deref().unwrap_or_default()),
            subject_host: SafeText::new(row.subject_host.as_deref().unwrap_or_default()),
            summary: SafeText::new(row.summary.as_deref().unwrap_or_default()),
            confidence: confidence(&row.confidence),
            ioc_hits: row.ioc_hits.iter().map(|h| SafeText::new(h)).collect(),
            kind,
        }
    }

    /// The one line a timeline shows: summary, falling back to the subject.
    pub fn display_text(&self) -> &str {
        if self.summary.is_empty() {
            self.subject.as_str()
        } else {
            self.summary.as_str()
        }
    }
}

fn confidence(raw: &str) -> Confidence {
    match raw {
        "direct" => Confidence::Direct,
        "derived" => Confidence::Derived,
        _ => Confidence::Heuristic,
    }
}

pub fn is_bidi_or_control(c: char) -> bool {
    matches!(
        c,
        '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
    ) || (c.is_control() && !matches!(c, '\t' | '\n' | '\r'))
}

pub fn has_bidi_or_control(raw: &str) -> bool {
    raw.chars().any(is_bidi_or_control)
}

/// Preserves exact code-point sequence in printable ASCII so bidi overrides
/// and control bytes can be inspected without executing visually.
pub fn escape_codepoints(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        if is_bidi_or_control(c) || !c.is_ascii() {
            out.push_str(&format!("\\u{{{:04X}}}", c as u32));
        } else {
            out.push(c);
        }
    }
    out
}

#[derive(Debug, Clone)]
pub enum SafeJson {
    Null,
    Bool(bool),
    Number(SafeText),
    String(SafeText),
    Array(Vec<SafeJson>),
    Object(Vec<(SafeText, SafeJson)>),
}

impl SafeJson {
    pub fn from_value(value: &serde_json::Value) -> Self {
        match value {
            serde_json::Value::Null => Self::Null,
            serde_json::Value::Bool(b) => Self::Bool(*b),
            serde_json::Value::Number(n) => Self::Number(SafeText::new(&n.to_string())),
            serde_json::Value::String(s) => Self::String(SafeText::new(s)),
            serde_json::Value::Array(items) => {
                Self::Array(items.iter().map(Self::from_value).collect())
            }
            serde_json::Value::Object(map) => Self::Object(
                map.iter()
                    .map(|(k, v)| (SafeText::new(k), Self::from_value(v)))
                    .collect(),
            ),
        }
    }

    pub fn raw_contains_bidi(value: &serde_json::Value) -> bool {
        match value {
            serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {
                false
            }
            serde_json::Value::String(s) => has_bidi_or_control(s),
            serde_json::Value::Array(items) => items.iter().any(Self::raw_contains_bidi),
            serde_json::Value::Object(map) => map
                .iter()
                .any(|(k, v)| has_bidi_or_control(k) || Self::raw_contains_bidi(v)),
        }
    }

    pub fn to_pretty_string(&self) -> String {
        fn write_json(node: &SafeJson, indent: usize, out: &mut String) {
            let pad = "  ".repeat(indent);
            match node {
                SafeJson::Null => out.push_str("null"),
                SafeJson::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
                SafeJson::Number(n) => out.push_str(n.as_str()),
                SafeJson::String(s) => {
                    if let Ok(quoted) = serde_json::to_string(s.as_str()) {
                        out.push_str(&quoted);
                    }
                }
                SafeJson::Array(items) => {
                    if items.is_empty() {
                        out.push_str("[]");
                        return;
                    }
                    out.push_str("[\n");
                    for (i, item) in items.iter().enumerate() {
                        out.push_str(&"  ".repeat(indent + 1));
                        write_json(item, indent + 1, out);
                        if i + 1 < items.len() {
                            out.push(',');
                        }
                        out.push('\n');
                    }
                    out.push_str(&pad);
                    out.push(']');
                }
                SafeJson::Object(entries) => {
                    if entries.is_empty() {
                        out.push_str("{}");
                        return;
                    }
                    out.push_str("{\n");
                    for (i, (k, v)) in entries.iter().enumerate() {
                        out.push_str(&"  ".repeat(indent + 1));
                        if let Ok(quoted_key) = serde_json::to_string(k.as_str()) {
                            out.push_str(&quoted_key);
                        }
                        out.push_str(": ");
                        write_json(v, indent + 1, out);
                        if i + 1 < entries.len() {
                            out.push(',');
                        }
                        out.push('\n');
                    }
                    out.push_str(&pad);
                    out.push('}');
                }
            }
        }
        let mut out = String::new();
        write_json(self, 0, &mut out);
        out
    }
}

#[derive(Debug, Clone)]
pub struct LinkVm {
    pub rel: SafeText,
    pub direction: LinkDirection,
    pub target_type: SafeText,
    pub target_id: u64,
    pub target_label: SafeText,
    pub confidence: Confidence,
    pub why: Option<SafeText>,
}

impl LinkVm {
    pub fn from_view(view: &LinkView) -> Self {
        Self {
            rel: SafeText::new(&view.rel),
            direction: view.direction,
            target_type: SafeText::new(&view.target_type),
            target_id: view.target_id,
            target_label: SafeText::new(&view.target_label),
            confidence: confidence(&view.confidence),
            why: view.why.as_deref().map(SafeText::new),
        }
    }
}

#[derive(Debug, Clone)]
pub struct DetailVm {
    pub id: u64,
    pub run_id: u64,
    pub ts_utc: i64,
    pub untimed: bool,
    pub ts_precision: SafeText,
    pub ts_raw: Option<SafeText>,
    pub ts_raw_format: Option<SafeText>,
    pub ts_tz_assumed: Option<SafeText>,
    pub category: fp_query::category::Category,
    pub kind: SafeText,
    pub source: SafeText,
    pub subject: Option<SafeText>,
    pub subject_host: Option<SafeText>,
    pub summary: Option<SafeText>,
    pub confidence: Confidence,
    pub actor: Option<SafeJson>,
    pub attrs: Option<SafeJson>,
    pub prov_file: Option<SafeText>,
    pub prov_sha256: Option<SafeText>,
    pub locator: Option<SafeText>,
    pub links: Vec<LinkVm>,
    /// Code-point-preserving ASCII escapes when bidi/control sequences appear.
    pub escaped_warnings: Vec<(&'static str, String)>,
}

impl DetailVm {
    pub fn from_view(view: &EventDetailView) -> Self {
        let kind = SafeText::new(&view.kind);
        let mut escaped_warnings = Vec::new();
        let mut check = |name: &'static str, raw: Option<&str>| {
            if let Some(s) = raw
                && has_bidi_or_control(s)
            {
                escaped_warnings.push((name, escape_codepoints(s)));
            }
        };
        check("kind", Some(&view.kind));
        check("subject", view.subject.as_deref());
        check("host", view.subject_host.as_deref());
        check("summary", view.summary.as_deref());
        check("provenance file", view.prov_file.as_deref());
        check("locator", view.locator.as_deref());
        if let Some(attrs) = &view.attrs
            && SafeJson::raw_contains_bidi(attrs)
        {
            escaped_warnings.push(("attributes", escape_codepoints(&attrs.to_string())));
        }
        if let Some(actor) = &view.actor
            && SafeJson::raw_contains_bidi(actor)
        {
            escaped_warnings.push(("actor", escape_codepoints(&actor.to_string())));
        }

        Self {
            id: view.id,
            run_id: view.run_id,
            ts_utc: view.ts_utc,
            untimed: view.ts_precision == "order_only",
            ts_precision: SafeText::new(&view.ts_precision),
            ts_raw: view.ts_raw.as_deref().map(SafeText::new),
            ts_raw_format: view.ts_raw_format.as_deref().map(SafeText::new),
            ts_tz_assumed: view.ts_tz_assumed.as_deref().map(SafeText::new),
            category: fp_query::category::category(kind.as_str()),
            kind,
            source: SafeText::new(&view.source_label()),
            subject: view.subject.as_deref().map(SafeText::new),
            subject_host: view.subject_host.as_deref().map(SafeText::new),
            summary: view.summary.as_deref().map(SafeText::new),
            confidence: confidence(&view.confidence),
            actor: view.actor.as_ref().map(SafeJson::from_value),
            attrs: view.attrs.as_ref().map(SafeJson::from_value),
            prov_file: view.prov_file.as_deref().map(SafeText::new),
            prov_sha256: view.prov_sha256.as_deref().map(SafeText::new),
            locator: view.locator.as_deref().map(SafeText::new),
            links: view.links.iter().map(LinkVm::from_view).collect(),
            escaped_warnings,
        }
    }
}

#[derive(Debug, Clone)]
pub struct GroupRowVm {
    pub label: SafeText,
    raw_key: String,
    pub count: u64,
    pub first_ts: Option<i64>,
    pub last_ts: Option<i64>,
}

impl GroupRowVm {
    pub fn from_row(row: &GroupRow) -> Self {
        Self {
            label: SafeText::new(&row.key),
            raw_key: row.key.clone(),
            count: row.count,
            first_ts: row.first_ts,
            last_ts: row.last_ts,
        }
    }

    /// Raw value for filter scoping (not rendered directly in widgets).
    pub fn filter_key(&self) -> &str {
        &self.raw_key
    }
}

#[derive(Debug, Clone)]
pub struct RunMarkerVm {
    pub id: u64,
    pub started_utc: i64,
    pub finished_utc: Option<i64>,
    pub args: Option<SafeText>,
    pub events_new: u64,
    pub error_count: usize,
}

impl RunMarkerVm {
    pub fn from_view(view: &RunView) -> Self {
        Self {
            id: view.id,
            started_utc: view.started_utc,
            finished_utc: view.finished_utc,
            args: view.args.as_deref().map(SafeText::new),
            events_new: view.events_new,
            error_count: view.errors.len(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct HostSummaryVm {
    pub host: SafeText,
    raw_host: String,
    pub events: u64,
    pub first_in_scope: Option<i64>,
    pub last_in_scope: Option<i64>,
    pub first_in_case: Option<i64>,
    pub distinct_kinds: u64,
    pub distinct_sources: u64,
    pub category_counts: [u64; Category::ALL.len()],
    pub sources: Vec<SafeText>,
    pub has_ioc: bool,
}

impl HostSummaryVm {
    pub fn from_overview(ov: &HostOverview) -> Self {
        Self {
            host: SafeText::new(&ov.summary.host),
            raw_host: ov.summary.host.clone(),
            events: ov.summary.events,
            first_in_scope: ov.summary.first_in_scope,
            last_in_scope: ov.summary.last_in_scope,
            first_in_case: ov.summary.first_in_case,
            distinct_kinds: ov.summary.distinct_kinds,
            distinct_sources: ov.summary.distinct_sources,
            category_counts: ov.category_counts,
            sources: ov.sources.iter().map(|s| SafeText::new(s)).collect(),
            has_ioc: ov.has_ioc,
        }
    }

    pub fn host_key(&self) -> &str {
        &self.raw_host
    }

    /// True when the host's earliest timed event in the whole case falls inside `window`.
    pub fn is_new_in_window(&self, window: Option<(i64, i64)>) -> bool {
        matches!(
            (self.first_in_case, window),
            (Some(first), Some((start, end))) if first >= start && first <= end
        )
    }
}

#[derive(Debug, Clone)]
pub struct SourceIssueVm {
    pub label: SafeText,
    pub status: SafeText,
}

impl SourceIssueVm {
    pub fn new(label: &str, status: &str) -> Self {
        Self {
            label: SafeText::new(label),
            status: SafeText::new(status),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct DashboardSummaryVm {
    pub host_name: Option<SafeText>,
    pub user_name: Option<SafeText>,
    pub ioc_event_hits: u64,
    pub sample_ioc_hosts: Vec<SafeText>,
    pub flagged_state_count: u64,
    pub flagged_state_kinds: Vec<SafeText>,
    pub source_warnings: Vec<SourceIssueVm>,
    pub coverage_covered: usize,
    pub coverage_total: usize,
    /// `(older_run_id, newer_run_id, added, removed, changed)` across the latest two runs.
    pub last_diff: Option<(u64, u64, usize, usize, usize)>,
}

#[derive(Debug, Clone)]
pub struct LaneRowDensityVm {
    pub label: SafeText,
    raw_key: String,
    pub total: u64,
    pub category_hint: Category,
    pub buckets: Vec<u64>,
}

impl LaneRowDensityVm {
    pub fn from_row(row: &LaneHistogramRow) -> Self {
        Self {
            label: SafeText::new(&row.label),
            raw_key: row.label.clone(),
            total: row.total,
            category_hint: row.category_hint,
            buckets: row.buckets.clone(),
        }
    }

    pub fn lane_key(&self) -> &str {
        &self.raw_key
    }
}

#[derive(Debug, Clone)]
pub struct UntimedLaneVm {
    pub label: SafeText,
    raw_key: String,
    pub count: u64,
    pub is_category: bool,
}

impl UntimedLaneVm {
    pub fn new(raw_key: &str, count: u64, is_category: bool) -> Self {
        Self {
            label: SafeText::new(raw_key),
            raw_key: raw_key.to_string(),
            count,
            is_category,
        }
    }

    pub fn lane_key(&self) -> &str {
        &self.raw_key
    }
}

#[derive(Debug, Clone)]
pub struct LanesDataVm {
    pub mode: LaneMode,
    pub window: TimeWindow,
    pub total_timed: u64,
    pub truncated_to_density: bool,
    pub bucket_bounds: Vec<(i64, i64)>,
    pub density_lanes: Vec<LaneRowDensityVm>,
    pub events: Vec<RowVm>,
    pub untimed_total: u64,
    pub untimed_lanes: Vec<UntimedLaneVm>,
}

#[derive(Debug, Clone)]
pub enum TraceHeadVm {
    Artifact {
        id: u64,
        path: SafeText,
        sha256: Option<SafeText>,
        size: Option<i64>,
    },
    Event {
        id: u64,
        ts_utc: i64,
        source: SafeText,
        kind: SafeText,
        subject: SafeText,
    },
}

#[derive(Debug, Clone)]
pub struct TraceRowVm {
    pub arrow: fp_correlate::trace::TraceArrow,
    pub rel: SafeText,
    pub ts_utc: Option<i64>,
    pub source: SafeText,
    pub text: SafeText,
    pub confidence: Option<Confidence>,
    pub event_id: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct TraceVm {
    pub head: TraceHeadVm,
    pub rows: Vec<TraceRowVm>,
}

#[derive(Debug, Clone)]
pub struct StateRowVm {
    pub rowid: i64,
    pub run_id: u64,
    pub kind: SafeText,
    pub key: SafeText,
    /// parsed value; `Null` when the stored JSON could not be parsed
    pub value: SafeJson,
    /// the raw value_json for unparsed rows
    pub raw: SafeText,
    /// one-line `state_details` strings, sanitized
    pub details: Vec<String>,
    pub flags: Vec<SafeText>,
    pub integrity: Option<SafeText>,
    pub locator: Option<SafeText>,
}

impl StateRowVm {
    pub fn from_view(row: &fp_query::StateView) -> Self {
        let parsed = row.parsed.as_ref();
        let flags = parsed
            .and_then(|v| v.get("flags"))
            .and_then(|f| f.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|s| s.as_str())
                    .map(SafeText::new)
                    .collect()
            })
            .unwrap_or_default();
        let integrity = parsed
            .and_then(|v| v.get("integrity"))
            .and_then(|i| i.as_str())
            .map(SafeText::new);
        Self {
            rowid: row.rowid,
            run_id: row.run_id,
            kind: SafeText::new(&row.kind),
            key: SafeText::new(&row.key),
            value: SafeJson::from_value(parsed.unwrap_or(&serde_json::Value::Null)),
            raw: SafeText::new(&row.value_json),
            details: fp_query::state_details(parsed)
                .into_iter()
                .map(|d| fp_render::sanitize(&d))
                .collect(),
            flags,
            integrity,
            locator: row.locator.as_deref().map(SafeText::new),
        }
    }
}

/// Plain-language explanation for a state flag or integrity verdict; `None` for
/// vocabulary the GUI does not know, so the caller shows the raw word alone.
pub fn state_flag_help(word: &str) -> Option<&'static str> {
    Some(match word {
        "unowned" => "no installed package claims this file",
        "hash_mismatch" => "the file differs from the package's recorded hash",
        "mismatch" => "the file differs from the package's recorded hash",
        "missing" => "the package lists this file but it is gone",
        "unverified" => "ownership is unknown or the hash was not checked",
        "mtime_in_window" => "the file was modified inside the collected window",
        "world_writable_path" => "a $PATH entry can be written by anyone",
        "uid0_non_root" => "a non-root account has uid 0",
        "unowned_module" => "a loaded kernel module has no owning package",
        "deleted_binary" => "the process binary was deleted after it started",
        "exec_in_hidden_dir" => "the process ran from a hidden directory",
        "exec_in_tmp" => "the process ran from a temporary directory",
        "listening" => "something is listening on this socket",
        "tainted" => "the kernel reports a taint flag",
        "firewall_disabled" => "the host firewall is off",
        _ => return None,
    })
}

#[derive(Debug, Clone)]
pub struct DiffEntryVm {
    pub change: fp_query::DiffChange,
    pub kind: SafeText,
    pub key: SafeText,
    pub detail: SafeText,
    pub run_a: Option<SafeJson>,
    pub run_b: Option<SafeJson>,
}

impl DiffEntryVm {
    pub fn from_entry(entry: &fp_query::DiffEntry) -> Self {
        Self {
            change: entry.change,
            kind: SafeText::new(&entry.kind),
            key: SafeText::new(&entry.key),
            detail: SafeText::new(&entry.detail),
            run_a: entry.run_a.as_ref().map(SafeJson::from_value),
            run_b: entry.run_b.as_ref().map(SafeJson::from_value),
        }
    }
}

impl TraceVm {
    pub fn from_tree(tree: &fp_correlate::trace::TraceTree) -> Self {
        use fp_correlate::trace::TraceHead;
        let head = match &tree.head {
            TraceHead::Artifact {
                id,
                path,
                sha256,
                size,
            } => TraceHeadVm::Artifact {
                id: *id,
                path: SafeText::new(path),
                sha256: sha256.as_deref().map(SafeText::new),
                size: *size,
            },
            TraceHead::Event {
                id,
                ts_utc,
                source,
                kind,
                subject,
            } => TraceHeadVm::Event {
                id: *id,
                ts_utc: *ts_utc,
                source: SafeText::new(source),
                kind: SafeText::new(kind),
                subject: SafeText::new(subject),
            },
        };
        let rows = tree
            .rows
            .iter()
            .map(|row| TraceRowVm {
                arrow: row.arrow,
                rel: SafeText::new(&row.rel),
                ts_utc: row.ts_utc,
                source: SafeText::new(&row.source),
                text: SafeText::new(&row.text),
                confidence: row.confidence.as_deref().map(confidence),
                event_id: row.event_id,
            })
            .collect();
        Self { head, rows }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hostile_row() -> TimelineRow {
        TimelineRow {
            id: 1,
            run_id: 1,
            ts_utc: 1_700_000_000_000_000,
            ts_precision: "exact".into(),
            kind: "web.\u{202e}visit".into(),
            source_kind: "chromium".into(),
            app: Some("brave\x07browser".into()),
            profile: Some("Default".into()),
            subject: Some("https://evil\u{1b}[31m.com/\u{202e}fdp.exe".into()),
            subject_host: Some("us09-web-zoom.com\u{200f}".into()),
            summary: Some("download \u{1b}[1;41mzoom\u{0c} installer".into()),
            confidence: "direct".into(),
            ioc_hits: vec!["zoom\u{202e}.com".into()],
        }
    }

    fn assert_clean(text: &SafeText) {
        assert!(
            text.as_str().chars().all(|c| !c.is_control()),
            "control char survived in {text:?}"
        );
        assert!(
            !text.as_str().chars().any(|c| matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')),
            "bidi override survived in {text:?}"
        );
        assert!(
            !text.as_str().contains('\u{1b}'),
            "ANSI survived in {text:?}"
        );
    }

    #[test]
    fn hostile_strings_die_at_the_vm_boundary() {
        let vm = RowVm::from_row(&hostile_row());
        for text in [
            &vm.kind,
            &vm.source,
            &vm.subject,
            &vm.subject_host,
            &vm.summary,
            &vm.ioc_hits[0],
        ] {
            assert_clean(text);
        }
        assert!(!vm.kind.as_str().contains('\u{202e}'));
        assert!(!vm.subject.as_str().contains("1b"), "raw escape text kept");
        assert!(vm.subject.as_str().contains("https://evil.com/"));
    }

    #[test]
    fn fields_survive_sanitization() {
        let vm = RowVm::from_row(&hostile_row());
        assert_eq!(vm.id, 1);
        assert_eq!(vm.category, fp_query::category::Category::Web);
        assert_eq!(vm.confidence, Confidence::Direct);
        assert_eq!(vm.source.as_str(), "bravebrowser/Default");
        assert_eq!(vm.subject_host.as_str(), "us09-web-zoom.com");
        assert!(!vm.untimed);
        assert_eq!(vm.display_text(), "download zoom installer");
    }

    #[test]
    fn order_only_precision_marks_the_row_untimed() {
        let row = TimelineRow {
            ts_precision: "order_only".into(),
            ts_utc: 0,
            ..hostile_row()
        };
        assert!(RowVm::from_row(&row).untimed);
    }

    #[test]
    fn plain_strings_pass_through_unchanged() {
        let vm = RowVm::from_row(&TimelineRow {
            summary: Some("pkg install zoom".into()),
            ..hostile_row()
        });
        assert_eq!(vm.display_text(), "pkg install zoom");
    }

    #[test]
    fn detail_vm_sanitizes_and_records_bidi_escapes() {
        let view = EventDetailView {
            id: 7,
            run_id: 2,
            ts_utc: 1_700_000_000_000_000,
            ts_precision: "exact".into(),
            ts_raw: Some("1700000000".into()),
            ts_raw_format: Some("UnixSeconds".into()),
            ts_tz_assumed: Some("UTC".into()),
            kind: "file.created".into(),
            source_kind: "filesystem".into(),
            app: None,
            profile: None,
            subject: Some("/tmp/fdp\u{202e}gnp.exe".into()),
            subject_host: None,
            summary: Some("created file".into()),
            confidence: "derived".into(),
            actor: None,
            attrs: Some(serde_json::json!({ "k\u{202e}ey": "v\u{1b}[31mal" })),
            prov_file: Some("/var/log/a\u{0007}b".into()),
            prov_sha256: Some("abc".into()),
            locator: Some("line 1".into()),
            links: vec![LinkView {
                rel: "downloaded_as".into(),
                direction: LinkDirection::Incoming,
                target_type: "event".into(),
                target_id: 3,
                target_label: "zoom\u{202e}.deb".into(),
                confidence: "heuristic".into(),
                why: Some("same hash".into()),
            }],
        };
        let detail = DetailVm::from_view(&view);
        assert_clean(detail.subject.as_ref().expect("subject"));
        assert_clean(&detail.links[0].target_label);
        assert!(detail.escaped_warnings.iter().any(|(f, _)| *f == "subject"));
        assert!(
            detail
                .escaped_warnings
                .iter()
                .any(|(f, esc)| *f == "subject" && esc.contains("\\u{202E}"))
        );
        assert!(
            detail
                .escaped_warnings
                .iter()
                .any(|(f, _)| *f == "attributes")
        );
        assert!(
            detail
                .escaped_warnings
                .iter()
                .any(|(f, _)| *f == "provenance file")
        );
        let pretty = detail.attrs.expect("attrs").to_pretty_string();
        assert!(!pretty.contains('\u{202e}'));
        assert!(!pretty.contains('\u{1b}'));
    }

    #[test]
    fn g4_view_models_sanitize_hostile_strings_and_compute_new_badge() {
        let overview = HostOverview {
            summary: fp_store::HostSummary {
                host: "bad\u{202e}host\x1b[31m.example".into(),
                events: 4,
                first_in_scope: Some(200),
                last_in_scope: Some(300),
                first_in_case: Some(200),
                distinct_kinds: 1,
                distinct_sources: 1,
            },
            category_counts: [4, 0, 0, 0, 0, 0, 0, 0, 0],
            sources: vec!["src\u{0007}x".into()],
            has_ioc: true,
        };
        let vm = HostSummaryVm::from_overview(&overview);
        assert_clean(&vm.host);
        assert_clean(&vm.sources[0]);
        assert!(vm.is_new_in_window(Some((100, 300))));
        assert!(!vm.is_new_in_window(Some((250, 300))));
        assert!(!vm.is_new_in_window(None));

        let issue = SourceIssueVm::new("src\u{202e}1", "err\x1b[0mor");
        assert_clean(&issue.label);
        assert_clean(&issue.status);

        let lane = LaneRowDensityVm::from_row(&LaneHistogramRow {
            label: "lane\x1b[1m".into(),
            total: 3,
            category_hint: Category::Web,
            buckets: vec![3],
        });
        assert_clean(&lane.label);
        let untimed = UntimedLaneVm::new("bash\u{202e}", 2, false);
        assert_clean(&untimed.label);
    }
}
