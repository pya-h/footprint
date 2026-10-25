use anyhow::Result;
use fp_core::{Artifact, Event, Privilege, SourceInstance, StateRecord};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserTarget {
    pub uid: u32,
    pub user: String,
    pub home: PathBuf,
}

pub struct Env {
    pub home: PathBuf,
    /// prefix for system paths (`/var/log`, the journal); `/` on a live host, a mount point for an image
    pub root: PathBuf,
    pub uid: u32,
    pub euid: u32,
    pub user_targets: Vec<UserTarget>,
}

impl Env {
    pub fn current() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        // SAFETY: getuid/geteuid cannot fail and touch no memory
        let uid = unsafe { libc::getuid() };
        let euid = unsafe { libc::geteuid() };
        let user = std::env::var("USER").unwrap_or_default();
        let target = UserTarget {
            uid,
            user,
            home: home.clone(),
        };
        Self {
            home,
            root: PathBuf::from("/"),
            uid,
            euid,
            user_targets: vec![target],
        }
    }

    pub fn system_path(&self, absolute: &str) -> PathBuf {
        self.root.join(absolute.trim_start_matches('/'))
    }

    pub fn test(home: PathBuf, root: PathBuf) -> Self {
        let target = UserTarget {
            uid: 1000,
            user: "test".into(),
            home: home.clone(),
        };
        Self {
            home,
            root,
            uid: 1000,
            euid: 1000,
            user_targets: vec![target],
        }
    }

    pub fn add_user_home(&mut self, home: PathBuf, uid: Option<u32>, user: Option<String>) {
        let uid = uid.unwrap_or(self.uid);
        let user = user.unwrap_or_else(|| self.uid.to_string());
        if !self.user_targets.iter().any(|t| t.home == home) {
            self.user_targets.push(UserTarget { uid, user, home });
        }
    }

    /// Enumerates `/etc/passwd` to discover homes of users with uid >= 1000.
    pub fn load_all_users(&mut self) -> Result<()> {
        let passwd_path = self.system_path("/etc/passwd");
        let contents = std::fs::read_to_string(&passwd_path)?;
        for line in contents.lines() {
            let parts: Vec<&str> = line.split(':').collect();
            if parts.len() < 7 {
                continue;
            }
            let user = parts[0];
            let Ok(uid) = parts[2].parse::<u32>() else {
                continue;
            };
            let home_dir = parts[5];
            let shell = parts[6];

            if uid < 1000 || shell.ends_with("/nologin") || shell.ends_with("/false") {
                continue;
            }

            let path = PathBuf::from(home_dir);
            if path.is_dir() {
                self.add_user_home(path, Some(uid), Some(user.to_string()));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_users_from_passwd() {
        let dir = tempfile::tempdir().unwrap();
        let etc = dir.path().join("etc");
        std::fs::create_dir_all(&etc).unwrap();
        let home_u = dir.path().join("home/paya");
        std::fs::create_dir_all(&home_u).unwrap();

        std::fs::write(
            etc.join("passwd"),
            format!(
                "root:x:0:0:root:/root:/bin/bash\n\
                 daemon:x:1:1:daemon:/usr/sbin:/usr/sbin/nologin\n\
                 paya:x:1000:1000:Paya:{}:/bin/bash\n",
                home_u.display()
            ),
        )
        .unwrap();

        let mut env = Env::test(dir.path().to_path_buf(), dir.path().to_path_buf());
        env.load_all_users().unwrap();

        let users = &env.user_targets;
        assert!(users.iter().any(|u| u.user == "paya" && u.uid == 1000));
        assert!(!users.iter().any(|u| u.user == "daemon"));
    }
}
