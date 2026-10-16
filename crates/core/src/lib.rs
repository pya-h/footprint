pub mod time;

use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Confidence {
    Direct,
    Derived,
    Heuristic,
}

impl fmt::Display for Confidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Confidence::Direct => write!(f, "direct"),
            Confidence::Derived => write!(f, "derived"),
            Confidence::Heuristic => write!(f, "heuristic"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimePrecision {
    Exact,
    Second,
    Minute,
    Day,
    OrderOnly,
}

impl fmt::Display for TimePrecision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TimePrecision::Exact => write!(f, "exact"),
            TimePrecision::Second => write!(f, "second"),
            TimePrecision::Minute => write!(f, "minute"),
            TimePrecision::Day => write!(f, "day"),
            TimePrecision::OrderOnly => write!(f, "order_only"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Actor {
    pub uid: Option<u32>,
    pub user: Option<String>,
    pub pid: Option<u32>,
    pub exe: Option<PathBuf>,
    pub app: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    pub file: PathBuf,
    pub file_sha256: Option<String>,
    pub locator: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub run_id: u64,
    pub ts_utc: i64,
    pub ts_precision: TimePrecision,
    pub ts_raw: String,
    pub ts_raw_format: time::TimeFormat,
    pub ts_tz_assumed: Option<String>,
    pub source_kind: String,
    pub source_inst: u64,
    pub kind: EventKind,
    pub actor: Option<Actor>,
    pub subject: String,
    pub subject_host: Option<String>,
    pub summary: String,
    pub attrs: serde_json::Value,
    pub provenance: Provenance,
    pub confidence: Confidence,
}

impl Event {
    pub fn dedup_key(&self) -> String {
        format!(
            "{}|{}|{}",
            self.source_kind, self.source_inst, self.provenance.locator
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateRecord {
    pub run_id: u64,
    pub kind: String,
    pub key: String,
    pub value: serde_json::Value,
    pub provenance: Provenance,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Artifact {
    pub path: PathBuf,
    pub sha256: Option<String>,
    pub size: Option<i64>,
    pub mtime_ns: Option<i64>,
    pub ctime_ns: Option<i64>,
    pub btime_ns: Option<i64>,
    pub owner_pkg: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkType {
    Event,
    Artifact,
    State,
}

impl fmt::Display for LinkType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LinkType::Event => write!(f, "event"),
            LinkType::Artifact => write!(f, "artifact"),
            LinkType::State => write!(f, "state"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Link {
    pub src_type: LinkType,
    pub src_id: u64,
    pub rel: String,
    pub dst_type: LinkType,
    pub dst_id: u64,
    pub confidence: Confidence,
    pub why: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventKind {
    WebVisit,
    WebRedirect,
    WebDownload,
    WebPermissionChange,
    WebSearch,
    PkgInstall,
    PkgUpgrade,
    PkgRemove,
    PrivSudo,
    PrivSu,
    AuthLogin,
    AuthSsh,
    ShellCommand,
    FileOpened,
    FileCreated,
    FileModified,
    PersistUnitAdded,
    PersistAutostart,
    ExtInstalled,
    DlManagerDownload,
    NetListen,
    ProcExec,
    #[serde(rename = "other", other)]
    Other,
}

impl fmt::Display for EventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            EventKind::WebVisit => "web.visit",
            EventKind::WebRedirect => "web.redirect",
            EventKind::WebDownload => "web.download",
            EventKind::WebPermissionChange => "web.permission_change",
            EventKind::WebSearch => "web.search",
            EventKind::PkgInstall => "pkg.install",
            EventKind::PkgUpgrade => "pkg.upgrade",
            EventKind::PkgRemove => "pkg.remove",
            EventKind::PrivSudo => "priv.sudo",
            EventKind::PrivSu => "priv.su",
            EventKind::AuthLogin => "auth.login",
            EventKind::AuthSsh => "auth.ssh",
            EventKind::ShellCommand => "shell.command",
            EventKind::FileOpened => "file.opened",
            EventKind::FileCreated => "file.created",
            EventKind::FileModified => "file.modified",
            EventKind::PersistUnitAdded => "persist.unit_added",
            EventKind::PersistAutostart => "persist.autostart",
            EventKind::ExtInstalled => "ext.installed",
            EventKind::DlManagerDownload => "dl.manager_download",
            EventKind::NetListen => "net.listen",
            EventKind::ProcExec => "proc.exec",
            EventKind::Other => "other",
        };
        write!(f, "{s}")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Privilege {
    User,
    Better,
    Required,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceStatus {
    Ok,
    Partial,
    Unavailable,
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceInstance {
    pub id: u64,
    pub kind: String,
    pub app: String,
    pub profile: Option<String>,
    pub path: PathBuf,
    pub status: SourceStatus,
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectStats {
    pub events: usize,
    pub states: usize,
    pub artifacts: usize,
    pub errors: Vec<String>,
}

impl CollectStats {
    pub fn empty() -> Self {
        Self {
            events: 0,
            states: 0,
            artifacts: 0,
            errors: Vec::new(),
        }
    }
}
