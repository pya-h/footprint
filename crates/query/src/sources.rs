use anyhow::Result;
use fp_core::Availability;
use fp_store::Store;

#[derive(Debug, Clone)]
pub struct SourceView {
    pub id: u64,
    pub run_id: u64,
    pub kind: String,
    pub app: Option<String>,
    pub profile: Option<String>,
    pub path: Option<String>,
    pub status: Option<Availability>,
    pub note: Option<String>,
    pub event_count: u64,
}

impl SourceView {
    pub fn label(&self) -> String {
        match (&self.app, &self.profile) {
            (Some(a), Some(p)) => format!("{a}/{p}"),
            (Some(a), None) => a.clone(),
            _ => self.kind.clone(),
        }
    }

    pub fn status_display(&self) -> String {
        self.status
            .as_ref()
            .map(|s| s.to_string())
            .unwrap_or_else(|| "present".into())
    }
}

pub fn sources(store: &Store, run_id: Option<u64>) -> Result<Vec<SourceView>> {
    Ok(store
        .sources(run_id)?
        .into_iter()
        .map(|s| SourceView {
            id: s.id,
            run_id: s.run_id,
            kind: s.kind,
            app: s.app,
            profile: s.profile,
            path: s.path,
            status: s
                .status
                .as_deref()
                .and_then(|raw| serde_json::from_str(raw).ok()),
            note: s.note,
            event_count: s.event_count,
        })
        .collect())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoverageStatus {
    Covered,
    Partial,
    Gap,
}

#[derive(Debug, Clone)]
pub struct CoverageRow {
    pub category: &'static str,
    pub status: CoverageStatus,
    /// sources that answered, with their event counts
    pub answering: Vec<String>,
    /// missing / unsupported / failed sources and why
    pub notes: Vec<String>,
}

/// Which sources serve which evidence category, in priority order.
const CATEGORIES: &[(&str, &[&str])] = &[
    ("Browser history", &["chromium.history", "firefox.history"]),
    ("Downloads", &["dlmgr.xdm", "dlmgr.xattr", "dlmgr.detect"]),
    (
        "Packages",
        &[
            "pkg.dpkg",
            "pkg.apt",
            "pkg.pacman",
            "pkg.dnf",
            "pkg.zypper",
            "pkg.snap",
            "pkg.flatpak",
        ],
    ),
    ("Privilege", &["auth.journald", "auth.log", "auth.wtmp"]),
    ("Shell", &["shell.history"]),
    (
        "Desktop activity",
        &[
            "desktop.recent",
            "desktop.kde",
            "desktop.kactivity",
            "desktop.zeitgeist",
        ],
    ),
];

pub fn coverage(rows: &[SourceView]) -> Vec<CoverageRow> {
    CATEGORIES
        .iter()
        .map(|&(category, kinds)| {
            let matching: Vec<&SourceView> = rows
                .iter()
                .filter(|s| kinds.contains(&s.kind.as_str()))
                .collect();
            let mut answering = Vec::new();
            let mut notes = Vec::new();

            for s in &matching {
                let app = s.label();
                match s.status.as_ref().unwrap_or(&Availability::Present) {
                    Availability::Present => {
                        answering.push(format!("{app} ({} events)", s.event_count));
                    }
                    Availability::NotInstalled => {
                        notes.push(format!("{app} not installed"));
                    }
                    Availability::Empty => {
                        notes.push(format!("{app} empty"));
                    }
                    Availability::Unsupported { reason } => {
                        notes.push(format!("{app} unsupported ({reason})"));
                    }
                    Availability::Unreadable { needs_root } => {
                        notes.push(format!(
                            "{app} unreadable{}",
                            if *needs_root { " (needs root)" } else { "" }
                        ));
                    }
                    Availability::Partial => {
                        answering.push(format!("{app} (partial, {} events)", s.event_count));
                    }
                    Availability::Error(e) => {
                        notes.push(format!("{app} error: {e}"));
                    }
                }
            }

            let has_events = matching.iter().any(|s| s.event_count > 0);
            let has_present = matching
                .iter()
                .any(|s| s.status.as_ref() == Some(&Availability::Present));
            let status = if has_events {
                CoverageStatus::Covered
            } else if has_present {
                CoverageStatus::Partial
            } else {
                CoverageStatus::Gap
            };

            CoverageRow {
                category,
                status,
                answering,
                notes,
            }
        })
        .collect()
}
