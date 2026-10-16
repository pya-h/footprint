use anyhow::Result;
use fp_core::{Artifact, Event, Privilege, SourceInstance, StateRecord};
use std::path::PathBuf;

pub struct Env {
    pub home: PathBuf,
    /// prefix for system paths (`/var/log`, the journal); `/` on a live host, a mount point for an image
    pub root: PathBuf,
    pub uid: u32,
    pub euid: u32,
}

impl Env {
    pub fn current() -> Self {
        Self {
            home: dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")),
            root: PathBuf::from("/"),
            // SAFETY: getuid/geteuid cannot fail and touch no memory
            uid: unsafe { libc::getuid() },
            euid: unsafe { libc::geteuid() },
        }
    }

    pub fn system_path(&self, absolute: &str) -> PathBuf {
        self.root.join(absolute.trim_start_matches('/'))
    }
}

pub struct CollectCtx {
    pub run_id: u64,
    pub since: Option<i64>,
    pub until: Option<i64>,
    pub assume_tz: Option<String>,
    /// private (`0700`) staging dir; each source instance gets its own subdir
    pub temp_dir: PathBuf,
}

/// Everything one source instance produced; drained by the caller after each instance.
#[derive(Default)]
pub struct Sink {
    pub events: Vec<Event>,
    pub states: Vec<StateRecord>,
    pub artifacts: Vec<Artifact>,
    /// problems that didn't stop the source (a skipped line, an unreadable side file)
    pub warnings: Vec<String>,
}

impl Sink {
    pub fn event(&mut self, event: Event) {
        self.events.push(event);
    }

    pub fn state(&mut self, record: StateRecord) {
        self.states.push(record);
    }

    pub fn artifact(&mut self, artifact: Artifact) {
        self.artifacts.push(artifact);
    }

    pub fn warn(&mut self, msg: impl Into<String>) {
        self.warnings.push(msg.into());
    }
}

pub trait Collector: Send + Sync {
    fn kind(&self) -> &'static str;
    fn discover(&self, env: &Env) -> Vec<SourceInstance>;
    /// An `Err` fails this instance only; the run continues with the next one.
    fn collect(&self, inst: &SourceInstance, ctx: &CollectCtx, sink: &mut Sink) -> Result<()>;
    fn privilege(&self) -> Privilege {
        Privilege::User
    }
}
