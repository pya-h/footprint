use anyhow::Result;
use fp_core::{Artifact, Event, Privilege, SourceInstance, StateRecord};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

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

/// One `/etc/passwd` line: (user, uid, home, shell).
fn passwd_entries(passwd: &Path) -> Result<Vec<(String, u32, PathBuf, String)>> {
    let contents = std::fs::read_to_string(passwd)?;
    Ok(contents
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split(':').collect();
            let [user, _, uid, _, _, home, shell] = parts.as_slice() else {
                return None;
            };
            Some((
                user.to_string(),
                uid.parse().ok()?,
                PathBuf::from(home),
                shell.to_string(),
            ))
        })
        .collect())
}

/// Under `sudo`, the user who ran it: their home is the one to read, not root's.
fn sudo_invoker() -> Option<UserTarget> {
    // SAFETY: geteuid cannot fail and touches no memory
    if unsafe { libc::geteuid() } != 0 {
        return None;
    }
    let uid: u32 = std::env::var("SUDO_UID").ok()?.parse().ok()?;
    if uid == 0 {
        return None;
    }
    let (user, _, home, _) = passwd_entries(Path::new("/etc/passwd"))
        .ok()?
        .into_iter()
        .find(|(_, u, _, _)| *u == uid)?;
    Some(UserTarget { uid, user, home })
}

impl Env {
    pub fn current() -> Self {
        // SAFETY: getuid/geteuid cannot fail and touch no memory
        let uid = unsafe { libc::getuid() };
        let euid = unsafe { libc::geteuid() };
        let target = sudo_invoker().unwrap_or_else(|| UserTarget {
            uid,
            user: std::env::var("USER").unwrap_or_default(),
            home: dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")),
        });
        Self {
            home: target.home.clone(),
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

    /// The same run, narrowed to one user's home (for collectors that read one home at a time).
    pub fn for_user(&self, target: &UserTarget) -> Self {
        Self {
            home: target.home.clone(),
            root: self.root.clone(),
            uid: self.uid,
            euid: self.euid,
            user_targets: vec![target.clone()],
        }
    }

    /// Adds a home; without an explicit owner, the directory's owner (named via passwd) is used.
    pub fn add_user_home(&mut self, home: PathBuf, uid: Option<u32>, user: Option<String>) {
        if self.user_targets.iter().any(|t| t.home == home) {
            return;
        }
        let uid = uid
            .or_else(|| std::fs::metadata(&home).ok().map(|m| m.uid()))
            .unwrap_or(self.uid);
        let user = user
            .or_else(|| {
                passwd_entries(&self.system_path("/etc/passwd"))
                    .ok()?
                    .into_iter()
                    .find(|(_, u, _, _)| *u == uid)
                    .map(|(name, ..)| name)
            })
            .unwrap_or_else(|| uid.to_string());
        self.user_targets.push(UserTarget { uid, user, home });
    }

    /// Enumerates `/etc/passwd` to discover homes of users with uid >= 1000.
    pub fn load_all_users(&mut self) -> Result<()> {
        for (user, uid, home, shell) in passwd_entries(&self.system_path("/etc/passwd"))? {
            if uid < 1000 || shell.ends_with("/nologin") || shell.ends_with("/false") {
                continue;
            }
            if home.is_dir() {
                self.add_user_home(home, Some(uid), Some(user));
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
    /// the case database and its side files: the tool's own writes, never activity
    pub own_files: Vec<PathBuf>,
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
    /// Reads one home (`Env::home`); the runner discovers it once per user target and stamps
    /// that user on its events. System-wide collectors walk `Env::user_targets` themselves.
    fn per_user(&self) -> bool {
        false
    }
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
