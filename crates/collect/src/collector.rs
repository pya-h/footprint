use anyhow::Result;
use fp_core::{CollectStats, Event, Privilege, SourceInstance, StateRecord};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub struct Env {
    pub home: PathBuf,
    pub uid: u32,
    pub euid: u32,
}

impl Env {
    pub fn current() -> Self {
        Self {
            home: dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")),
            uid: unsafe { libc::getuid() },
            euid: unsafe { libc::geteuid() },
        }
    }
}

pub struct CollectCtx {
    pub run_id: u64,
    pub since: Option<i64>,
    pub until: Option<i64>,
    pub assume_tz: Option<String>,
    pub temp_dir: PathBuf,
    pub deny_list: Vec<PathBuf>,
}

#[derive(Clone)]
pub struct Sink {
    events: Arc<Mutex<Vec<Event>>>,
    states: Arc<Mutex<Vec<StateRecord>>>,
    errors: Arc<Mutex<Vec<String>>>,
}

impl Sink {
    pub fn new() -> Self {
        Self {
            events: Arc::new(Mutex::new(Vec::new())),
            states: Arc::new(Mutex::new(Vec::new())),
            errors: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn push_event(&self, event: Event) {
        self.events.lock().unwrap().push(event);
    }

    pub fn push_state(&self, record: StateRecord) {
        self.states.lock().unwrap().push(record);
    }

    pub fn error(&self, msg: String) {
        self.errors.lock().unwrap().push(msg);
    }

    pub fn into_stats(self) -> CollectStats {
        CollectStats {
            events: self.events.lock().unwrap().len(),
            states: self.states.lock().unwrap().len(),
            artifacts: 0,
            errors: std::mem::take(&mut *self.errors.lock().unwrap()),
        }
    }

    pub fn drain_events(&self) -> Vec<Event> {
        std::mem::take(&mut *self.events.lock().unwrap())
    }

    pub fn drain_states(&self) -> Vec<StateRecord> {
        std::mem::take(&mut *self.states.lock().unwrap())
    }
}

pub trait Collector: Send + Sync {
    fn kind(&self) -> &'static str;
    fn discover(&self, env: &Env) -> Vec<SourceInstance>;
    fn collect(&self, inst: &SourceInstance, ctx: &CollectCtx, sink: &Sink) -> Result<CollectStats>;
    fn privilege(&self) -> Privilege {
        Privilege::User
    }
}

pub fn secret_deny_list() -> Vec<PathBuf> {
    [
        "Cookies",
        "Login Data",
        "Web Data",
        "Local Extension Settings",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect()
}

pub fn is_deny_listed(path: &Path, deny: &[PathBuf]) -> bool {
    deny.iter().any(|d| path.components().any(|c| {
        let s = c.as_os_str().to_string_lossy();
        s == d.to_string_lossy()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deny_list_matches_local_extension_settings() {
        let deny = secret_deny_list();
        let path = Path::new("/home/u/.config/brave/Default/Local Extension Settings/ab/cdef");
        assert!(is_deny_listed(path, &deny));
    }

    #[test]
    fn deny_list_allows_history() {
        let deny = secret_deny_list();
        let path = Path::new("/home/u/.config/brave/Default/History");
        assert!(!is_deny_listed(path, &deny));
    }
}
