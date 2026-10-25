use crate::collector::{CollectCtx, Collector, Env, Sink};
use crate::probe::{DistroFamily, Probe};
use crate::utils::{guard, in_window, sha256_file};
use anyhow::Result;
use fp_core::{Availability, Provenance, SourceInstance, StateRecord};
use serde_json::json;
use std::cell::{OnceCell, RefCell};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub struct PersistCollector;

impl Collector for PersistCollector {
    fn kind(&self) -> &'static str {
        "persist.snapshot"
    }

    fn discover(&self, env: &Env) -> Vec<SourceInstance> {
        let mut out = Vec::new();
        let mut sys = SourceInstance::new(self.kind(), "system", None, env.root.clone());
        if !env.root.exists() {
            sys.status = Availability::NotInstalled;
        }
        out.push(sys);

        for target in &env.user_targets {
            let mut user = SourceInstance::new(
                self.kind(),
                "user",
                Some(target.user.clone()),
                target.home.clone(),
            );
            if !target.home.exists() {
                user.status = Availability::NotInstalled;
            }
            out.push(user);
        }
        out
    }

    fn collect(&self, inst: &SourceInstance, ctx: &CollectCtx, sink: &mut Sink) -> Result<()> {
        if inst.app == "system" {
            let verifier = PkgVerifier::new(&inst.path);
            let mut snapshot = SystemPersistSnapshot::new(&inst.path, ctx, verifier);
            snapshot.run(sink);
        } else if inst.app == "user" {
            // nothing under a home directory belongs to a package
            let mut snapshot = UserPersistSnapshot::new(&inst.path, ctx, PkgVerifier::unowned());
            snapshot.run(sink);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PkgManager {
    Dpkg,
    Rpm,
    Pacman,
}

/// The live host's package tool, run by absolute path: owners (rpm, pacman) and verification.
struct PkgTools {
    manager: PkgManager,
    tool: PathBuf,
}

/// Paths per owner query; keeps the argv far below the kernel limit.
const OWNER_BATCH: usize = 64;
/// dpkg's file lists, one `<package>[:<arch>].list` per package
const DPKG_INFO: &str = "var/lib/dpkg/info";

/// file -> problem, from one package's verify run (files without a problem are absent)
type VerifyReport = HashMap<PathBuf, &'static str>;

/// Package ownership and integrity. dpkg ownership is read from the root's own file lists,
/// so it works on a mounted image too; tools run only against the live host's database.
pub struct PkgVerifier {
    root: PathBuf,
    /// path inside the root -> package; built on first use
    dpkg_index: OnceCell<Option<HashMap<PathBuf, String>>>,
    tools: Option<PkgTools>,
    /// one verify run per package; `None` when the check itself could not run
    reports: RefCell<HashMap<String, Option<VerifyReport>>>,
}

impl PkgVerifier {
    pub fn new(root: &Path) -> Self {
        let env = Env {
            home: PathBuf::from("/"),
            root: root.to_path_buf(),
            uid: 0,
            euid: 0,
            user_targets: Vec::new(),
        };
        let tools = (root == Path::new("/"))
            .then(|| {
                let (manager, name) = match Probe::distro(&env) {
                    DistroFamily::Debian => (PkgManager::Dpkg, "dpkg"),
                    DistroFamily::RedHat | DistroFamily::Suse => (PkgManager::Rpm, "rpm"),
                    DistroFamily::Arch => (PkgManager::Pacman, "pacman"),
                    DistroFamily::Unknown => return None,
                };
                Some(PkgTools {
                    manager,
                    tool: Probe::tool_path(&env, name)?,
                })
            })
            .flatten();
        Self {
            root: root.to_path_buf(),
            dpkg_index: OnceCell::new(),
            tools,
            reports: RefCell::default(),
        }
    }

    /// For files no package manager tracks (home directories): ownership is never claimed.
    fn unowned() -> Self {
        Self {
            root: PathBuf::new(),
            dpkg_index: OnceCell::from(None),
            tools: None,
            reports: RefCell::default(),
        }
    }

    fn dpkg_index(&self) -> Option<&HashMap<PathBuf, String>> {
        self.dpkg_index
            .get_or_init(|| read_dpkg_index(&self.root.join(DPKG_INFO)))
            .as_ref()
    }

    /// False on a root without a readable package database: "no owner" means "unknown" then.
    pub fn resolves_owners(&self) -> bool {
        self.dpkg_index().is_some()
            || self
                .tools
                .as_ref()
                .is_some_and(|t| t.manager != PkgManager::Dpkg)
    }

    /// Owning package per path; `None` for unowned or unknown.
    pub fn lookup_owners(&self, paths: &[&Path]) -> HashMap<PathBuf, Option<String>> {
        if let Some(index) = self.dpkg_index() {
            return paths
                .iter()
                .map(|&path| {
                    let inside = Path::new("/").join(path.strip_prefix(&self.root).unwrap_or(path));
                    let owner = index
                        .get(&inside)
                        .or_else(|| usr_merge_alias(&inside).and_then(|alias| index.get(&alias)))
                        .cloned();
                    (path.to_path_buf(), owner)
                })
                .collect();
        }
        let mut out: HashMap<PathBuf, Option<String>> =
            paths.iter().map(|p| (p.to_path_buf(), None)).collect();
        let Some(tools) = self
            .tools
            .as_ref()
            .filter(|t| t.manager != PkgManager::Dpkg)
        else {
            return out;
        };
        let found = query_owners(tools, paths);
        let mut aliases = Vec::new();
        for &path in paths {
            match found.get(path) {
                Some(pkg) => {
                    out.insert(path.to_path_buf(), Some(pkg.clone()));
                }
                None => {
                    aliases.extend(usr_merge_alias(path).map(|alias| (path.to_path_buf(), alias)))
                }
            }
        }
        if !aliases.is_empty() {
            let alias_paths: Vec<&Path> =
                aliases.iter().map(|(_, alias)| alias.as_path()).collect();
            let found = query_owners(tools, &alias_paths);
            for (path, alias) in &aliases {
                if let Some(pkg) = found.get(alias) {
                    out.insert(path.clone(), Some(pkg.clone()));
                }
            }
        }
        out
    }

    /// `ok`, `mismatch`, `missing`, or `unverified` when the check could not run or read the file.
    pub fn integrity(&self, pkg: &str, path: &Path) -> &'static str {
        let mut reports = self.reports.borrow_mut();
        let report = reports
            .entry(pkg.to_string())
            .or_insert_with(|| self.tools.as_ref().and_then(|t| verify_report(t, pkg)));
        let Some(report) = report else {
            return "unverified";
        };
        report
            .get(path)
            .or_else(|| usr_merge_alias(path).and_then(|alias| report.get(&alias)))
            .copied()
            .unwrap_or("ok")
    }
}

/// Merged /usr: a config names `/lib/x.so` while the package lists `/usr/lib/x.so`, or the reverse.
fn usr_merge_alias(path: &Path) -> Option<PathBuf> {
    const MERGED: &[&str] = &["bin", "sbin", "lib", "lib32", "lib64", "libx32"];
    let relative = path.strip_prefix("/").ok()?;
    let mut parts = relative.components();
    let first = parts.next()?.as_os_str().to_str()?;
    if first == "usr" {
        let rest = parts.as_path();
        let top = rest.components().next()?.as_os_str().to_str()?;
        MERGED.contains(&top).then(|| Path::new("/").join(rest))
    } else {
        MERGED
            .contains(&first)
            .then(|| Path::new("/usr").join(relative))
    }
}

fn read_dpkg_index(info: &Path) -> Option<HashMap<PathBuf, String>> {
    let entries = fs::read_dir(info).ok()?;
    let mut index = HashMap::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(stem) = name.to_str().and_then(|n| n.strip_suffix(".list")) else {
            continue;
        };
        let package = stem.split(':').next().unwrap_or(stem);
        let Ok(file) = File::open(entry.path()) else {
            continue;
        };
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            if line.starts_with('/') && line != "/." {
                index
                    .entry(PathBuf::from(line))
                    .or_insert_with(|| package.to_string());
            }
        }
    }
    Some(index)
}

fn query_owners(tools: &PkgTools, paths: &[&Path]) -> HashMap<PathBuf, String> {
    let mut out = HashMap::new();
    for chunk in paths.chunks(OWNER_BATCH) {
        let mut cmd = Command::new(&tools.tool);
        match tools.manager {
            PkgManager::Rpm => cmd.args(["-qf", "--queryformat", "%{NAME}\\n"]),
            PkgManager::Pacman => cmd.args(["-Qoq"]),
            PkgManager::Dpkg => continue,
        };
        cmd.args(chunk).stdin(Stdio::null()).stderr(Stdio::null());
        // an unowned path makes the tool exit non-zero; the owned ones are still printed, in order
        let Ok(output) = cmd.output() else {
            continue;
        };
        for (line, &path) in String::from_utf8_lossy(&output.stdout).lines().zip(chunk) {
            let pkg = line.trim();
            if !pkg.is_empty() && !pkg.contains(' ') {
                out.insert(path.to_path_buf(), pkg.to_string());
            }
        }
    }
    out
}

fn verify_report(tools: &PkgTools, pkg: &str) -> Option<VerifyReport> {
    let flag = match tools.manager {
        PkgManager::Dpkg => "--verify",
        PkgManager::Rpm => "-V",
        PkgManager::Pacman => "-Qkk",
    };
    let output = Command::new(&tools.tool)
        .args([flag, pkg])
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .ok()?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    Some(parse_verify(tools.manager, &text))
}

fn parse_verify(manager: PkgManager, text: &str) -> VerifyReport {
    let mut report = HashMap::new();
    for line in text.lines() {
        let Some(slash) = line.find('/') else {
            continue;
        };
        let (head, rest) = line.split_at(slash);
        let (path, verdict) = match manager {
            PkgManager::Pacman => {
                let (path, why) = rest.split_once(" (").unwrap_or((rest, ""));
                let why = why.to_ascii_lowercase();
                let verdict = if why.contains("no such file") {
                    "missing"
                } else if why.contains("checksum") || why.contains("size") {
                    "mismatch"
                } else if why.contains("permission") {
                    "unverified"
                } else {
                    continue;
                };
                (path, verdict)
            }
            PkgManager::Dpkg | PkgManager::Rpm => {
                let flags = head.split_whitespace().next().unwrap_or_default();
                let verdict = if flags == "missing" {
                    "missing"
                } else if flags.as_bytes().get(2) == Some(&b'5')
                    || flags.as_bytes().first() == Some(&b'S')
                {
                    "mismatch"
                } else if flags.contains('?') {
                    "unverified"
                } else {
                    continue;
                };
                (rest, verdict)
            }
        };
        report.insert(PathBuf::from(path.trim()), verdict);
    }
    report
}

/// A file the snapshot records; owners are resolved for all of them in one batch.
struct Pending {
    kind: &'static str,
    path: PathBuf,
    details: serde_json::Value,
    flags: Vec<String>,
}

/// Resolves owners in one batch, then emits one state record per pending file.
fn flush_pending(pending: Vec<Pending>, verifier: &PkgVerifier, ctx: &CollectCtx, sink: &mut Sink) {
    let paths: Vec<&Path> = pending.iter().map(|p| p.path.as_path()).collect();
    let owners = verifier.lookup_owners(&paths);
    for p in pending {
        let owner_pkg = owners.get(&p.path).cloned().flatten();
        emit_state_entry(p, owner_pkg, verifier, ctx, sink);
    }
}

struct SystemPersistSnapshot<'a> {
    root: &'a Path,
    ctx: &'a CollectCtx,
    verifier: PkgVerifier,
    pending: Vec<Pending>,
}

impl<'a> SystemPersistSnapshot<'a> {
    fn new(root: &'a Path, ctx: &'a CollectCtx, verifier: PkgVerifier) -> Self {
        Self {
            root,
            ctx,
            verifier,
            pending: Vec::new(),
        }
    }

    fn run(&mut self, sink: &mut Sink) {
        self.collect_preload();
        self.collect_accounts();
        self.collect_sudoers();
        self.collect_root_ssh();
        self.collect_cron();
        self.collect_systemd();
        self.collect_autostart();
        self.collect_profile();
        self.collect_pam();
        self.collect_udev_and_net();
        self.collect_apt_config();
        self.collect_suid();
        flush_pending(
            std::mem::take(&mut self.pending),
            &self.verifier,
            self.ctx,
            sink,
        );
    }

    fn collect_preload(&mut self) {
        let preload = self.root.join("etc/ld.so.preload");
        if preload.is_file() {
            self.emit_file_state("persist.preload", &preload, json!({}));
        }
    }

    fn collect_accounts(&mut self) {
        let passwd_path = self.root.join("etc/passwd");
        if passwd_path.exists() {
            let mut uid0_users = Vec::new();
            let mut login_shells = Vec::new();

            if let Ok(file) = File::open(&passwd_path) {
                let reader = BufReader::new(file);
                for line in reader.lines().map_while(Result::ok) {
                    let parts: Vec<&str> = line.split(':').collect();
                    if parts.len() >= 7 {
                        let user = parts[0];
                        let uid_str = parts[2];
                        let shell = parts[6];

                        if uid_str == "0" {
                            uid0_users.push(user.to_string());
                        }

                        if !shell.ends_with("nologin")
                            && !shell.ends_with("false")
                            && !shell.is_empty()
                        {
                            login_shells.push(format!("{user}:{shell}"));
                        }
                    }
                }
            }

            let mut flags = Vec::new();
            let non_root_uid0: Vec<String> = uid0_users
                .iter()
                .filter(|u| *u != "root")
                .cloned()
                .collect();
            if !non_root_uid0.is_empty() {
                flags.push("uid0_non_root".to_string());
            }

            let details = json!({
                "uid0_users": uid0_users,
                "login_shells": login_shells,
            });

            self.emit_file_state_with_extra("persist.account", &passwd_path, details, flags);
        }

        let group_path = self.root.join("etc/group");
        if group_path.exists() {
            let mut priv_groups = HashMap::new();
            if let Ok(file) = File::open(&group_path) {
                let reader = BufReader::new(file);
                for line in reader.lines().map_while(Result::ok) {
                    let parts: Vec<&str> = line.split(':').collect();
                    if parts.len() >= 4 {
                        let group = parts[0];
                        if matches!(group, "sudo" | "wheel" | "root" | "adm") {
                            let members: Vec<String> = parts[3]
                                .split(',')
                                .map(|s| s.trim().to_string())
                                .filter(|s| !s.is_empty())
                                .collect();
                            priv_groups.insert(group.to_string(), members);
                        }
                    }
                }
            }
            self.emit_file_state(
                "persist.account",
                &group_path,
                json!({ "privileged_groups": priv_groups }),
            );
        }
    }

    fn collect_sudoers(&mut self) {
        let sudoers = self.root.join("etc/sudoers");
        if sudoers.exists() {
            self.emit_file_state("persist.sudoers", &sudoers, json!({}));
        }
        let sudoers_d = self.root.join("etc/sudoers.d");
        if sudoers_d.is_dir()
            && let Ok(entries) = fs::read_dir(sudoers_d)
        {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    self.emit_file_state("persist.sudoers", &path, json!({}));
                }
            }
        }
    }

    fn collect_root_ssh(&mut self) {
        let root_auth = self.root.join("root/.ssh/authorized_keys");
        if root_auth.exists() {
            self.emit_file_state("persist.ssh_authorized_keys", &root_auth, json!({}));
        }
    }

    fn collect_cron(&mut self) {
        let crontab = self.root.join("etc/crontab");
        if crontab.exists() {
            self.emit_file_state("persist.cron", &crontab, json!({}));
        }

        let cron_dirs = [
            "etc/cron.d",
            "etc/cron.daily",
            "etc/cron.hourly",
            "etc/cron.weekly",
            "etc/cron.monthly",
            "var/spool/cron/crontabs",
            "var/spool/cron",
        ];
        for dir_name in &cron_dirs {
            let dir = self.root.join(dir_name);
            if dir.is_dir()
                && let Ok(entries) = fs::read_dir(dir)
            {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_file() {
                        self.emit_file_state("persist.cron", &path, json!({}));
                    }
                }
            }
        }
    }

    fn collect_systemd(&mut self) {
        let dirs = [
            self.root.join("etc/systemd/system"),
            self.root.join("usr/lib/systemd/system"),
            self.root.join("usr/lib/systemd/user"),
        ];

        let mut unit_files = Vec::new();
        for dir in dirs {
            if dir.is_dir() {
                walk_files_shallow(&dir, 3, &mut unit_files);
            }
        }

        for path in unit_files {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or_default();
            if matches!(
                ext,
                "service" | "timer" | "socket" | "path" | "target" | "conf"
            ) {
                self.emit_file_state("persist.systemd", &path, json!({}));
            }
        }
    }

    fn collect_autostart(&mut self) {
        let dir = self.root.join("etc/xdg/autostart");
        if dir.is_dir()
            && let Ok(entries) = fs::read_dir(dir)
        {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    let exec_line = read_desktop_exec(&path);
                    self.emit_file_state("persist.autostart", &path, json!({ "exec": exec_line }));
                }
            }
        }
    }

    fn collect_profile(&mut self) {
        let profile_d = self.root.join("etc/profile.d");
        if profile_d.is_dir()
            && let Ok(entries) = fs::read_dir(profile_d)
        {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    self.inspect_shell_rc(&path);
                }
            }
        }
        let etc_profile = self.root.join("etc/profile");
        if etc_profile.is_file() {
            self.inspect_shell_rc(&etc_profile);
        }
    }

    fn inspect_shell_rc(&mut self, path: &Path) {
        let (details, flags) = inspect_shell_rc_file(path);
        self.emit_file_state_with_extra("persist.shell_rc", path, details, flags);
    }

    fn collect_pam(&mut self) {
        let Ok(entries) = fs::read_dir(self.root.join("etc/pam.d")) else {
            return;
        };
        // (pam file, module names as written, resolved module paths)
        let mut files = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let mut referenced = Vec::new();
            if let Ok(file) = File::open(&path) {
                for line in BufReader::new(file).lines().map_while(Result::ok) {
                    let line = line.trim();
                    if line.starts_with('#') {
                        continue;
                    }
                    referenced.extend(
                        line.split_whitespace()
                            .filter(|t| t.ends_with(".so"))
                            .map(String::from),
                    );
                }
            }
            let mut seen = HashSet::new();
            referenced.retain(|m| seen.insert(m.clone()));
            let resolved: Vec<Option<PathBuf>> = referenced
                .iter()
                .map(|m| self.resolve_pam_module(m))
                .collect();
            files.push((path, referenced, resolved));
        }

        let modules: Vec<&Path> = files
            .iter()
            .flat_map(|(_, _, resolved)| resolved.iter().flatten().map(PathBuf::as_path))
            .collect();
        let owners = self.verifier.lookup_owners(&modules);
        let knows_owners = self.verifier.resolves_owners();

        for (path, referenced, resolved) in files {
            // a module named in the config but absent on disk only fails that pam stack
            let missing: Vec<&String> = referenced
                .iter()
                .zip(&resolved)
                .filter(|(_, module)| module.is_none())
                .map(|(name, _)| name)
                .collect();
            let unowned: Vec<String> = resolved
                .iter()
                .flatten()
                .filter(|m| knows_owners && owners.get(*m).cloned().flatten().is_none())
                .map(|m| m.display().to_string())
                .collect();
            let flags = if unowned.is_empty() {
                Vec::new()
            } else {
                vec!["unowned_pam_module".to_string()]
            };
            let details = json!({
                "referenced_modules": referenced,
                "unowned_modules": unowned,
                "missing_modules": missing,
            });
            self.emit_file_state_with_extra("persist.pam", &path, details, flags);
        }
    }

    fn resolve_pam_module(&self, name: &str) -> Option<PathBuf> {
        if name.starts_with('/') {
            let p = self.root.join(name.trim_start_matches('/'));
            return if p.exists() { Some(p) } else { None };
        }
        let search_dirs = [
            "lib/x86_64-linux-gnu/security",
            "lib/aarch64-linux-gnu/security",
            "usr/lib/x86_64-linux-gnu/security",
            "lib64/security",
            "usr/lib64/security",
            "lib/security",
            "usr/lib/security",
        ];
        for dir in &search_dirs {
            let p = self.root.join(dir).join(name);
            if p.exists() {
                return Some(p);
            }
        }
        None
    }

    fn collect_udev_and_net(&mut self) {
        let udev_dir = self.root.join("etc/udev/rules.d");
        if udev_dir.is_dir()
            && let Ok(entries) = fs::read_dir(udev_dir)
        {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    self.emit_file_state("persist.udev", &path, json!({}));
                }
            }
        }

        let nm_dir = self.root.join("etc/NetworkManager/dispatcher.d");
        if nm_dir.is_dir()
            && let Ok(entries) = fs::read_dir(nm_dir)
        {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    self.emit_file_state("persist.net_dispatcher", &path, json!({}));
                }
            }
        }
    }

    fn collect_apt_config(&mut self) {
        let apt_conf_d = self.root.join("etc/apt/apt.conf.d");
        if apt_conf_d.is_dir()
            && let Ok(entries) = fs::read_dir(apt_conf_d)
        {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    self.emit_file_state("persist.apt_config", &path, json!({}));
                }
            }
        }

        let sources_d = self.root.join("etc/apt/sources.list.d");
        if sources_d.is_dir()
            && let Ok(entries) = fs::read_dir(sources_d)
        {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    self.emit_file_state("persist.apt_config", &path, json!({}));
                }
            }
        }
    }

    fn collect_suid(&mut self) {
        let search_dirs = [
            "bin",
            "sbin",
            "usr/bin",
            "usr/sbin",
            "usr/local/bin",
            "usr/local/sbin",
        ];
        // merged /usr makes /bin and /usr/bin the same directory; record each file once
        let mut seen = HashSet::new();
        let mut suid_files = Vec::new();
        for dir_name in &search_dirs {
            let Ok(dir) = fs::canonicalize(self.root.join(dir_name)) else {
                continue;
            };
            if !seen.insert(dir.clone()) {
                continue;
            }
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                if let Ok(meta) = entry.metadata()
                    && meta.is_file()
                    && meta.mode() & (0o4000 | 0o2000) != 0
                {
                    suid_files.push(entry.path());
                }
            }
        }

        // only the unowned ones are worth a record; a packaged suid binary is expected
        let owners = self
            .verifier
            .lookup_owners(&suid_files.iter().map(PathBuf::as_path).collect::<Vec<_>>());
        if !self.verifier.resolves_owners() {
            return;
        }
        for path in suid_files {
            if owners.get(&path).cloned().flatten().is_none() {
                self.emit_file_state_with_extra(
                    "persist.suid",
                    &path,
                    json!({ "unowned_suid": true }),
                    vec!["unowned_suid".to_string()],
                );
            }
        }
    }

    fn emit_file_state(&mut self, kind: &'static str, path: &Path, details: serde_json::Value) {
        self.emit_file_state_with_extra(kind, path, details, Vec::new());
    }

    fn emit_file_state_with_extra(
        &mut self,
        kind: &'static str,
        path: &Path,
        details: serde_json::Value,
        flags: Vec<String>,
    ) {
        self.pending.push(Pending {
            kind,
            path: path.to_path_buf(),
            details,
            flags,
        });
    }
}

struct UserPersistSnapshot<'a> {
    home: &'a Path,
    ctx: &'a CollectCtx,
    verifier: PkgVerifier,
    pending: Vec<Pending>,
}

impl<'a> UserPersistSnapshot<'a> {
    fn new(home: &'a Path, ctx: &'a CollectCtx, verifier: PkgVerifier) -> Self {
        Self {
            home,
            ctx,
            verifier,
            pending: Vec::new(),
        }
    }

    fn run(&mut self, sink: &mut Sink) {
        self.collect_ssh_keys();
        self.collect_systemd_user();
        self.collect_autostart();
        self.collect_shell_rc();
        self.collect_git_hooks();
        flush_pending(
            std::mem::take(&mut self.pending),
            &self.verifier,
            self.ctx,
            sink,
        );
    }

    fn collect_ssh_keys(&mut self) {
        for name in [".ssh/authorized_keys", ".ssh/authorized_keys2"] {
            let path = self.home.join(name);
            if path.is_file() {
                let key_count = if let Ok(file) = File::open(&path) {
                    BufReader::new(file)
                        .lines()
                        .map_while(Result::ok)
                        .filter(|l| {
                            let l = l.trim();
                            !l.is_empty() && !l.starts_with('#')
                        })
                        .count()
                } else {
                    0
                };
                self.emit_file_state(
                    "persist.ssh_authorized_keys",
                    &path,
                    json!({ "key_count": key_count }),
                );
            }
        }
    }

    fn collect_systemd_user(&mut self) {
        let dir = self.home.join(".config/systemd/user");
        if !dir.is_dir() {
            return;
        }
        let mut unit_files = Vec::new();
        walk_files_shallow(&dir, 3, &mut unit_files);
        for path in unit_files {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or_default();
            if matches!(
                ext,
                "service" | "timer" | "socket" | "path" | "target" | "conf"
            ) {
                self.emit_file_state("persist.systemd", &path, json!({}));
            }
        }
    }

    fn collect_autostart(&mut self) {
        let dir = self.home.join(".config/autostart");
        if dir.is_dir()
            && let Ok(entries) = fs::read_dir(dir)
        {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    let exec_line = read_desktop_exec(&path);
                    self.emit_file_state("persist.autostart", &path, json!({ "exec": exec_line }));
                }
            }
        }
    }

    fn collect_shell_rc(&mut self) {
        let rc_names = [
            ".bashrc",
            ".profile",
            ".zshrc",
            ".bash_profile",
            ".xprofile",
        ];
        for rc in &rc_names {
            let path = self.home.join(rc);
            if path.is_file() {
                let (details, flags) = inspect_shell_rc_file(&path);
                self.emit_file_state_with_extra("persist.shell_rc", &path, details, flags);
            }
        }
    }

    fn collect_git_hooks(&mut self) {
        if !self.home.is_dir() {
            return;
        }
        // repos live at ~/src/x, ~/work/y/...: find .git/hooks anywhere shallow
        let mut hooks = Vec::new();
        find_git_hooks(self.home, 0, &mut hooks);
        for hook in hooks {
            self.emit_file_state("persist.git_hook", &hook, json!({}));
        }
    }

    fn emit_file_state(&mut self, kind: &'static str, path: &Path, details: serde_json::Value) {
        self.emit_file_state_with_extra(kind, path, details, Vec::new());
    }

    fn emit_file_state_with_extra(
        &mut self,
        kind: &'static str,
        path: &Path,
        details: serde_json::Value,
        flags: Vec<String>,
    ) {
        self.pending.push(Pending {
            kind,
            path: path.to_path_buf(),
            details,
            flags,
        });
    }
}

fn find_git_hooks(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 5 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == ".git" {
            let hook_dir = path.join("hooks");
            if let Ok(hooks) = fs::read_dir(&hook_dir) {
                for hook in hooks.flatten() {
                    let hp = hook.path();
                    let hook_name = hook.file_name();
                    let hook_name = hook_name.to_string_lossy();
                    if hp.is_file() && !hook_name.ends_with(".sample") {
                        out.push(hp);
                    }
                }
            }
            continue; // never descend into .git itself
        }
        if crate::fs::is_noise_dir(&path) {
            continue;
        }
        find_git_hooks(&path, depth + 1, out);
    }
}

fn inspect_shell_rc_file(path: &Path) -> (serde_json::Value, Vec<String>) {
    let mut world_writable_paths = Vec::new();
    if let Ok(file) = File::open(path) {
        let reader = BufReader::new(file);
        let mut var_bindings: HashMap<String, String> = HashMap::new();

        for line in reader.lines().map_while(Result::ok) {
            let trimmed = line.trim();
            if trimmed.starts_with('#') {
                continue;
            }

            if let Some(rest) = trimmed
                .strip_prefix("export ")
                .or(Some(trimmed))
                .and_then(|s| s.split_once('='))
            {
                let var_name = rest.0.trim();
                let val = rest.1.trim().trim_matches('"').trim_matches('\'');
                if !var_name.is_empty() && !val.is_empty() {
                    var_bindings.insert(var_name.to_string(), val.to_string());
                }
            }

            if trimmed.contains("PATH=") {
                let path_val = trimmed
                    .split("PATH=")
                    .nth(1)
                    .unwrap_or("")
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .trim_matches('"')
                    .trim_matches('\'');

                for mut part in path_val.split(':') {
                    part = part.trim();
                    if part.is_empty() || part.contains("$PATH") || part.contains("${PATH}") {
                        continue;
                    }
                    let mut resolved = part.to_string();
                    for (k, v) in &var_bindings {
                        let var_ref = format!("${k}");
                        let var_ref2 = format!("${{{k}}}");
                        resolved = resolved.replace(&var_ref, v).replace(&var_ref2, v);
                    }

                    if resolved.starts_with('/') {
                        let p = PathBuf::from(&resolved);
                        if is_world_writable_or_has_world_writable_parent(&p) {
                            world_writable_paths.push(resolved);
                        }
                    }
                }
            }
        }
    }

    let mut flags = Vec::new();
    if !world_writable_paths.is_empty() {
        flags.push("world_writable_path".to_string());
    }

    let details = json!({
        "world_writable_path_entries": world_writable_paths,
    });

    (details, flags)
}

fn emit_state_entry(
    pending: Pending,
    owner_pkg: Option<String>,
    verifier: &PkgVerifier,
    ctx: &CollectCtx,
    sink: &mut Sink,
) {
    let Pending {
        kind,
        path,
        details,
        flags,
    } = pending;
    let path = path.as_path();
    if guard(path).is_err() {
        return;
    }
    let Ok(meta) = fs::metadata(path) else {
        return;
    };

    let sha256 = sha256_file(path).ok();
    let mtime_us = meta.mtime() * 1_000_000 + meta.mtime_nsec() / 1_000;
    let ctime_us = meta.ctime() * 1_000_000 + meta.ctime_nsec() / 1_000;
    let size = meta.len() as i64;

    let integrity = match &owner_pkg {
        Some(pkg) => verifier.integrity(pkg, path),
        None if verifier.resolves_owners() => "unowned",
        None => "unverified",
    };

    let mut flags: HashSet<String> = flags.into_iter().collect();
    if integrity == "unowned" {
        flags.insert("unowned".to_string());
    }
    if matches!(integrity, "mismatch" | "missing") {
        flags.insert("hash_mismatch".to_string());
    }
    if in_window(ctx, mtime_us) {
        flags.insert("mtime_in_window".to_string());
    }

    let mut flag_list: Vec<String> = flags.into_iter().collect();
    flag_list.sort();

    let path_str = path.display().to_string();
    let value = json!({
        "path": path_str,
        "sha256": sha256,
        "size": size,
        "mtime": mtime_us,
        "ctime": ctime_us,
        "mode": meta.mode(),
        "uid": meta.uid(),
        "gid": meta.gid(),
        "owner_pkg": owner_pkg,
        "integrity": integrity,
        "flags": flag_list,
        "details": details,
    });

    sink.state(StateRecord {
        run_id: ctx.run_id,
        kind: kind.to_string(),
        key: path_str.clone(),
        value,
        provenance: Provenance {
            file: path.to_path_buf(),
            file_sha256: sha256,
            locator: format!("persist:{kind}:{path_str}"),
        },
    });
}

fn walk_files_shallow(dir: &Path, max_depth: usize, out: &mut Vec<PathBuf>) {
    walk_recurse(dir, 0, max_depth, out);
}

fn walk_recurse(dir: &Path, depth: usize, max_depth: usize, out: &mut Vec<PathBuf>) {
    if depth > max_depth {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if guard(&path).is_err() {
            continue;
        }
        if let Ok(ft) = entry.file_type() {
            if ft.is_file() {
                out.push(path);
            } else if ft.is_dir() {
                walk_recurse(&path, depth + 1, max_depth, out);
            }
        }
    }
}

fn read_desktop_exec(path: &Path) -> Option<String> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);
    for line in reader.lines().map_while(Result::ok) {
        let trimmed = line.trim();
        if let Some(exec) = trimmed.strip_prefix("Exec=") {
            return Some(exec.trim().to_string());
        }
    }
    None
}

fn is_world_writable_or_has_world_writable_parent(path: &Path) -> bool {
    let mut curr = Some(path);
    while let Some(p) = curr {
        if let Ok(meta) = fs::metadata(p)
            && meta.mode() & 0o002 != 0
        {
            return true;
        }
        curr = p.parent();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dpkg_verify_lines() {
        // `?` is "could not check" (a root-only conffile read as a user), not a mismatch
        let report = parse_verify(
            PkgManager::Dpkg,
            "????????? c /etc/sudoers\n??5?????? c /etc/pam.d/common-auth\nmissing     /usr/bin/gone\n",
        );
        assert_eq!(report[Path::new("/etc/sudoers")], "unverified");
        assert_eq!(report[Path::new("/etc/pam.d/common-auth")], "mismatch");
        assert_eq!(report[Path::new("/usr/bin/gone")], "missing");
    }

    #[test]
    fn rpm_and_pacman_verify_lines() {
        let rpm = parse_verify(
            PkgManager::Rpm,
            "S.5....T.  c /etc/ssh/sshd_config\n.M.......    /usr/bin/ping\n",
        );
        assert_eq!(rpm[Path::new("/etc/ssh/sshd_config")], "mismatch");
        // a mode-only change is not an integrity failure
        assert!(!rpm.contains_key(Path::new("/usr/bin/ping")));

        let pacman = parse_verify(
            PkgManager::Pacman,
            "warning: curl: /usr/bin/curl (SHA256 checksum mismatch)\n\
             warning: curl: /usr/share/doc/x (Modification time mismatch)\n",
        );
        assert_eq!(pacman[Path::new("/usr/bin/curl")], "mismatch");
        assert!(!pacman.contains_key(Path::new("/usr/share/doc/x")));
    }

    #[test]
    fn usr_merge_aliases() {
        assert_eq!(
            usr_merge_alias(Path::new("/lib/x86_64-linux-gnu/security/pam_unix.so")).as_deref(),
            Some(Path::new("/usr/lib/x86_64-linux-gnu/security/pam_unix.so"))
        );
        assert_eq!(
            usr_merge_alias(Path::new("/usr/bin/su")).as_deref(),
            Some(Path::new("/bin/su"))
        );
        assert_eq!(usr_merge_alias(Path::new("/etc/passwd")), None);
        assert_eq!(usr_merge_alias(Path::new("/usr/share/x")), None);
    }

    #[test]
    fn image_ownership_comes_from_its_dpkg_lists() {
        let root = tempfile::tempdir().unwrap();
        let info = root.path().join(DPKG_INFO);
        fs::create_dir_all(&info).unwrap();
        fs::write(
            info.join("libpam-modules:amd64.list"),
            "/.\n/usr/lib/x86_64-linux-gnu/security/pam_unix.so\n",
        )
        .unwrap();
        let verifier = PkgVerifier::new(root.path());
        assert!(verifier.resolves_owners());
        let module = root
            .path()
            .join("lib/x86_64-linux-gnu/security/pam_unix.so");
        let stray = root.path().join("etc/systemd/system/evil.service");
        let owners = verifier.lookup_owners(&[&module, &stray]);
        assert_eq!(owners[&module].as_deref(), Some("libpam-modules"));
        assert_eq!(owners[&stray], None);
    }

    #[test]
    fn mounted_image_marks_ownership_unknown_not_unowned() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("etc/cron.d")).unwrap();
        fs::write(root.path().join("etc/crontab"), "* * * * * root true\n").unwrap();
        let ctx = CollectCtx {
            run_id: 1,
            since: None,
            until: None,
            assume_tz: None,
            temp_dir: root.path().join("tmp"),
            own_files: Vec::new(),
        };
        let mut sink = Sink::default();
        let verifier = PkgVerifier::new(root.path());
        SystemPersistSnapshot::new(root.path(), &ctx, verifier).run(&mut sink);
        let crontab = sink
            .states
            .iter()
            .find(|s| s.kind == "persist.cron")
            .unwrap();
        assert_eq!(crontab.value["integrity"], "unverified");
        assert!(!crontab.value["flags"].to_string().contains("unowned"));
    }

    #[test]
    fn test_world_writable_path_detection() {
        let tmp = Path::new("/tmp/test_dir");
        // /tmp is world-writable
        assert!(is_world_writable_or_has_world_writable_parent(tmp));
    }
}
