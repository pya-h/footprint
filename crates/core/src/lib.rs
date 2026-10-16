pub mod deny;
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
        f.write_str(match self {
            Confidence::Direct => "direct",
            Confidence::Derived => "derived",
            Confidence::Heuristic => "heuristic",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimePrecision {
    Exact,
    Second,
    Minute,
    Day,
    OrderOnly,
}

impl fmt::Display for TimePrecision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            TimePrecision::Exact => "exact",
            TimePrecision::Second => "second",
            TimePrecision::Minute => "minute",
            TimePrecision::Day => "day",
            TimePrecision::OrderOnly => "order_only",
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Actor {
    pub uid: Option<u32>,
    pub user: Option<String>,
    pub pid: Option<u32>,
    pub exe: Option<PathBuf>,
    pub app: Option<String>,
}

impl Actor {
    pub fn app(name: impl Into<String>) -> Self {
        Self {
            app: Some(name.into()),
            ..Self::default()
        }
    }
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
    /// Stable across runs: built from the source's identity, never from the run-local `sources.id`.
    pub fn dedup_key(&self, source_identity: &str) -> String {
        format!(
            "{source_identity}|{}|{}|{}|{}",
            self.kind, self.ts_utc, self.provenance.locator, self.subject
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

impl Artifact {
    pub fn referenced(path: PathBuf, size: Option<i64>) -> Self {
        Self {
            path,
            sha256: None,
            size,
            mtime_ns: None,
            ctime_ns: None,
            btime_ns: None,
            owner_pkg: None,
        }
    }
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
        f.write_str(match self {
            LinkType::Event => "event",
            LinkType::Artifact => "artifact",
            LinkType::State => "state",
        })
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventKind {
    WebVisit,
    WebRedirect,
    WebDownload,
    WebPermissionChange,
    /// a one-time grant ("allow this time"): chromium records only `last_used`, nothing persisted
    WebPermissionUsed,
    WebSearch,
    PkgInstall,
    PkgUpgrade,
    PkgRemove,
    PrivSudo,
    PrivSu,
    AuthLogin,
    AuthSsh,
    /// pam session open/close lines that accompany sudo/su; context, not a privileged command
    AuthSession,
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

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::WebVisit => "web.visit",
            EventKind::WebRedirect => "web.redirect",
            EventKind::WebDownload => "web.download",
            EventKind::WebPermissionChange => "web.permission_change",
            EventKind::WebPermissionUsed => "web.permission_used",
            EventKind::WebSearch => "web.search",
            EventKind::PkgInstall => "pkg.install",
            EventKind::PkgUpgrade => "pkg.upgrade",
            EventKind::PkgRemove => "pkg.remove",
            EventKind::PrivSudo => "priv.sudo",
            EventKind::PrivSu => "priv.su",
            EventKind::AuthLogin => "auth.login",
            EventKind::AuthSsh => "auth.ssh",
            EventKind::AuthSession => "auth.session",
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
        }
    }
}

impl fmt::Display for EventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
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

impl SourceInstance {
    pub fn new(kind: &str, app: &str, profile: Option<String>, path: PathBuf) -> Self {
        Self {
            id: 0,
            kind: kind.to_string(),
            app: app.to_string(),
            profile,
            path,
            status: SourceStatus::Ok,
            note: None,
        }
    }

    /// Stable across runs; rotated logs (`dpkg.log.1`, `.2.gz`) share the identity of their base file.
    pub fn identity(&self) -> String {
        format!(
            "{}|{}|{}|{}",
            self.kind,
            self.app,
            self.profile.as_deref().unwrap_or(""),
            rotation_base(&self.path.to_string_lossy())
        )
    }
}

fn rotation_base(path: &str) -> &str {
    let path = path.strip_suffix(".gz").unwrap_or(path);
    match path.rsplit_once('.') {
        Some((base, n)) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => base,
        _ => path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_ignores_run_id_and_rotation() {
        let mut a = SourceInstance::new("pkg.dpkg", "dpkg", None, "/var/log/dpkg.log".into());
        let mut b = SourceInstance::new("pkg.dpkg", "dpkg", None, "/var/log/dpkg.log.2.gz".into());
        a.id = 7;
        b.id = 42;
        assert_eq!(a.identity(), b.identity());
        let c = SourceInstance::new("pkg.dpkg", "dpkg", None, "/var/log/dpkg.log.1".into());
        assert_eq!(a.identity(), c.identity());
    }

    #[test]
    fn identity_keeps_profiles_apart() {
        let a = SourceInstance::new(
            "chromium.history",
            "Brave",
            Some("Default".into()),
            "/b/Default".into(),
        );
        let b = SourceInstance::new(
            "chromium.history",
            "Brave",
            Some("Profile 1".into()),
            "/b/Profile 1".into(),
        );
        assert_ne!(a.identity(), b.identity());
    }

    #[test]
    fn rotation_base_keeps_dotted_names() {
        assert_eq!(
            rotation_base("/h/.xdm-app-data/downloads.db"),
            "/h/.xdm-app-data/downloads.db"
        );
        assert_eq!(
            rotation_base("/var/log/apt/history.log.1.gz"),
            "/var/log/apt/history.log"
        );
    }
}
