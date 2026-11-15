//! View-models: the only types widgets draw. Every string crosses the
//! sanitization boundary here, so hostile evidence cannot reach a `Ui::label`
//! by construction.

use std::fmt;

use fp_core::Confidence;
use fp_query::TimelineRow;

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
}
