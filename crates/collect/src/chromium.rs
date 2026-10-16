use crate::collector::{Collector, CollectCtx, Env, Sink};
use anyhow::Result;
use fp_core::{CollectStats, Privilege, SourceInstance, SourceStatus};
use std::path::{Path, PathBuf};

const BROWSERS: &[(&str, &[&str])] = &[
    ("Chrome", &[".config/google-chrome"]),
    ("Chromium", &[".config/chromium", "snap/chromium/common/chromium"]),
    (
        "Brave",
        &[
            ".config/BraveSoftware/Brave-Browser",
            "snap/brave/current/.config/BraveSoftware/Brave-Browser",
        ],
    ),
    ("Edge", &[".config/microsoft-edge"]),
    ("Vivaldi", &[".config/vivaldi"]),
    ("Opera", &[".config/opera"]),
];

pub struct ChromiumCollector;

impl ChromiumCollector {
    pub fn new() -> Self {
        Self
    }

    fn roots(env: &Env) -> Vec<(String, PathBuf)> {
        let mut out = Vec::new();
        for (name, rels) in BROWSERS {
            for rel in *rels {
                let flatpak = env.home.join(format!(".var/app/*/config/{rel}"));
                if let Some(p) = first_glob(&flatpak) {
                    out.push((name.to_string(), p));
                }
                let native = env.home.join(rel);
                if native.is_dir() {
                    out.push((name.to_string(), native));
                }
            }
        }
        out
    }
}

impl Collector for ChromiumCollector {
    fn kind(&self) -> &'static str {
        "chromium.history"
    }

    fn discover(&self, env: &Env) -> Vec<SourceInstance> {
        let mut id = 1u64;
        let mut out = Vec::new();
        for (app, root) in Self::roots(env) {
            for profile in list_profiles(&root) {
                let path = root.join(&profile);
                let history = path.join("History");
                let status = if history.exists() {
                    SourceStatus::Ok
                } else {
                    SourceStatus::Unavailable
                };
                out.push(SourceInstance {
                    id,
                    kind: self.kind().to_string(),
                    app: app.clone(),
                    profile: Some(profile),
                    path,
                    status,
                    note: None,
                });
                id += 1;
            }
        }
        out
    }

    fn collect(
        &self,
        _inst: &SourceInstance,
        _ctx: &CollectCtx,
        _sink: &Sink,
    ) -> Result<CollectStats> {
        Ok(CollectStats::empty())
    }

    fn privilege(&self) -> Privilege {
        Privilege::User
    }
}

fn first_glob(pattern: &Path) -> Option<PathBuf> {
    glob::glob(pattern.to_string_lossy().as_ref())
        .ok()?
        .filter_map(Result::ok)
        .next()
}

fn list_profiles(root: &Path) -> Vec<String> {
    let mut profiles = Vec::new();
    if root.join("History").exists() {
        profiles.push("Default".to_string());
    }
    if root.join("Default").is_dir() {
        profiles.push("Default".to_string());
    }
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let s = name.to_string_lossy();
            if s.starts_with("Profile ") && entry.path().is_dir() {
                profiles.push(s.into_owned());
            }
        }
    }
    profiles.sort();
    profiles.dedup();
    profiles
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_profiles_finds_default_and_profiles() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir(root.join("Default")).unwrap();
        std::fs::create_dir(root.join("Profile 1")).unwrap();
        std::fs::create_dir(root.join("Profile 2")).unwrap();
        let got = list_profiles(root);
        assert_eq!(got, vec!["Default", "Profile 1", "Profile 2"]);
    }
}
