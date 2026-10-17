use crate::collector::{CollectCtx, Collector, Env, Sink};
use crate::probe::{DistroFamily, Probe};
use crate::utils::{guard, in_window};
use anyhow::Result;
use fp_core::{Availability, Provenance, SourceInstance, StateRecord};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;

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
            let verifier = PkgVerifier::new(Path::new("/"));
            let mut snapshot = UserPersistSnapshot::new(&inst.path, ctx, verifier);
            snapshot.run(sink);
        }
        Ok(())
    }
}

pub struct PkgVerifier {
    distro: DistroFamily,
    root: PathBuf,
    has_dpkg_query: bool,
    has_rpm: bool,
    has_pacman: bool,
}

impl PkgVerifier {
    pub fn new(root: &Path) -> Self {
        let is_host_root = root == Path::new("/");
        let probe_env = Env {
            home: PathBuf::from("/"),
            root: root.to_path_buf(),
            uid: 0,
            euid: 0,
            user_targets: Vec::new(),
        };
        Self {
            distro: Probe::distro(&probe_env),
            root: root.to_path_buf(),
            has_dpkg_query: is_host_root && Probe::has_tool(&probe_env, "dpkg-query"),
            has_rpm: is_host_root && Probe::has_tool(&probe_env, "rpm"),
            has_pacman: is_host_root && Probe::has_tool(&probe_env, "pacman"),
        }
    }

    /// Queries package ownership for a slice of paths in batches.
    pub fn lookup_owners(&self, paths: &[&Path]) -> HashMap<PathBuf, Option<String>> {
        let mut out = HashMap::new();
        if paths.is_empty() {
            return out;
        }

        if self.distro == DistroFamily::Debian && self.has_dpkg_query && self.root == Path::new("/")
        {
            for chunk in paths.chunks(64) {
                let mut cmd = Command::new("dpkg-query");
                cmd.arg("-S");
                for p in chunk {
                    cmd.arg(p);
                }
                if let Ok(output) = cmd.output() {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    for line in stdout.lines() {
                        let line = line.trim();
                        if let Some((pkgs, path_str)) = line.split_once(": ") {
                            let pkg = pkgs.split(',').next().unwrap_or(pkgs).trim();
                            out.insert(PathBuf::from(path_str.trim()), Some(pkg.to_string()));
                        }
                    }
                }
            }
        } else if self.distro == DistroFamily::RedHat && self.has_rpm && self.root == Path::new("/")
        {
            for chunk in paths.chunks(64) {
                let mut cmd = Command::new("rpm");
                cmd.arg("-qf");
                for p in chunk {
                    cmd.arg(p);
                }
                if let Ok(output) = cmd.output() {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    for (line, &p) in stdout.lines().zip(chunk.iter()) {
                        let line = line.trim();
                        if !line.is_empty() && !line.contains("is not owned by any package") {
                            out.insert(p.to_path_buf(), Some(line.to_string()));
                        }
                    }
                }
            }
        } else if self.distro == DistroFamily::Arch
            && self.has_pacman
            && self.root == Path::new("/")
        {
            for chunk in paths.chunks(64) {
                let mut cmd = Command::new("pacman");
                cmd.arg("-Qo");
                for p in chunk {
                    cmd.arg(p);
                }
                if let Ok(output) = cmd.output() {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    for line in stdout.lines() {
                        if let Some((path_part, pkg_part)) = line.split_once(" is owned by ") {
                            let p = path_part.trim();
                            let pkg = pkg_part.split_whitespace().next().unwrap_or(pkg_part);
                            out.insert(PathBuf::from(p), Some(pkg.to_string()));
                        }
                    }
                }
            }
        }

        // Fill remaining paths as None
        for &p in paths {
            out.entry(p.to_path_buf()).or_insert(None);
        }

        out
    }

    /// Verifies package integrity using package manager verification tools.
    pub fn verify_package(&self, pkg: &str, file: &Path) -> &'static str {
        if self.distro == DistroFamily::Debian && self.has_dpkg_query && self.root == Path::new("/")
        {
            let mut cmd = Command::new("dpkg");
            cmd.arg("--verify").arg(pkg);
            if let Ok(output) = cmd.output() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let target_str = file.to_string_lossy();
                for line in stdout.lines() {
                    if line.contains(target_str.as_ref()) {
                        return "mismatch";
                    }
                }
                return "ok";
            }
        } else if self.distro == DistroFamily::RedHat && self.has_rpm && self.root == Path::new("/")
        {
            let mut cmd = Command::new("rpm");
            cmd.arg("-V").arg(pkg);
            if let Ok(output) = cmd.output() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let target_str = file.to_string_lossy();
                for line in stdout.lines() {
                    if line.contains(target_str.as_ref()) {
                        return "mismatch";
                    }
                }
                return "ok";
            }
        } else if self.distro == DistroFamily::Arch
            && self.has_pacman
            && self.root == Path::new("/")
        {
            let mut cmd = Command::new("pacman");
            cmd.arg("-Qkk").arg(pkg);
            if let Ok(output) = cmd.output() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let target_str = file.to_string_lossy();
                for line in stdout.lines() {
                    if line.contains(target_str.as_ref()) && line.contains("failed") {
                        return "mismatch";
                    }
                }
                return "ok";
            }
        }
        "ok"
    }
}

struct SystemPersistSnapshot<'a> {
    root: &'a Path,
    ctx: &'a CollectCtx,
    verifier: PkgVerifier,
}

impl<'a> SystemPersistSnapshot<'a> {
    fn new(root: &'a Path, ctx: &'a CollectCtx, verifier: PkgVerifier) -> Self {
        Self {
            root,
            ctx,
            verifier,
        }
    }

    fn run(&mut self, sink: &mut Sink) {
        self.collect_preload(sink);
        self.collect_accounts(sink);
        self.collect_sudoers(sink);
        self.collect_root_ssh(sink);
        self.collect_cron(sink);
        self.collect_systemd(sink);
        self.collect_autostart(sink);
        self.collect_profile(sink);
        self.collect_pam(sink);
        self.collect_udev_and_net(sink);
        self.collect_apt_config(sink);
        self.collect_suid(sink);
    }

    fn collect_preload(&mut self, sink: &mut Sink) {
        let preload = self.root.join("etc/ld.so.preload");
        if preload.is_file() {
            self.emit_file_state("persist.preload", &preload, json!({}), sink);
        }
    }

    fn collect_accounts(&mut self, sink: &mut Sink) {
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

            self.emit_file_state_with_extra("persist.account", &passwd_path, details, flags, sink);
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
                sink,
            );
        }
    }

    fn collect_sudoers(&mut self, sink: &mut Sink) {
        let sudoers = self.root.join("etc/sudoers");
        if sudoers.exists() {
            self.emit_file_state("persist.sudoers", &sudoers, json!({}), sink);
        }
        let sudoers_d = self.root.join("etc/sudoers.d");
        if sudoers_d.is_dir()
            && let Ok(entries) = fs::read_dir(sudoers_d)
        {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    self.emit_file_state("persist.sudoers", &path, json!({}), sink);
                }
            }
        }
    }

    fn collect_root_ssh(&mut self, sink: &mut Sink) {
        let root_auth = self.root.join("root/.ssh/authorized_keys");
        if root_auth.exists() {
            self.emit_file_state("persist.ssh_authorized_keys", &root_auth, json!({}), sink);
        }
    }

    fn collect_cron(&mut self, sink: &mut Sink) {
        let crontab = self.root.join("etc/crontab");
        if crontab.exists() {
            self.emit_file_state("persist.cron", &crontab, json!({}), sink);
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
                        self.emit_file_state("persist.cron", &path, json!({}), sink);
                    }
                }
            }
        }
    }

    fn collect_systemd(&mut self, sink: &mut Sink) {
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
                self.emit_file_state("persist.systemd", &path, json!({}), sink);
            }
        }
    }

    fn collect_autostart(&mut self, sink: &mut Sink) {
        let dir = self.root.join("etc/xdg/autostart");
        if dir.is_dir()
            && let Ok(entries) = fs::read_dir(dir)
        {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    let exec_line = read_desktop_exec(&path);
                    self.emit_file_state(
                        "persist.autostart",
                        &path,
                        json!({ "exec": exec_line }),
                        sink,
                    );
                }
            }
        }
    }

    fn collect_profile(&mut self, sink: &mut Sink) {
        let profile_d = self.root.join("etc/profile.d");
        if profile_d.is_dir()
            && let Ok(entries) = fs::read_dir(profile_d)
        {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    self.inspect_shell_rc(&path, sink);
                }
            }
        }
        let etc_profile = self.root.join("etc/profile");
        if etc_profile.is_file() {
            self.inspect_shell_rc(&etc_profile, sink);
        }
    }

    fn inspect_shell_rc(&mut self, path: &Path, sink: &mut Sink) {
        let (details, flags) = inspect_shell_rc_file(path);
        self.emit_file_state_with_extra("persist.shell_rc", path, details, flags, sink);
    }

    fn collect_pam(&mut self, sink: &mut Sink) {
        let pam_d = self.root.join("etc/pam.d");
        if !pam_d.is_dir() {
            return;
        }

        let Ok(entries) = fs::read_dir(pam_d) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }

            let mut referenced_modules = Vec::new();
            let mut unowned_modules = Vec::new();

            if let Ok(file) = File::open(&path) {
                let reader = BufReader::new(file);
                for line in reader.lines().map_while(Result::ok) {
                    let line = line.trim();
                    if line.starts_with('#') || line.is_empty() {
                        continue;
                    }
                    for token in line.split_whitespace() {
                        if token.ends_with(".so") {
                            referenced_modules.push(token.to_string());
                            let mod_path = self.resolve_pam_module(token);
                            if let Some(mp) = mod_path {
                                let owners = self.verifier.lookup_owners(&[&mp]);
                                if owners.get(&mp).and_then(|o| o.as_ref()).is_none() {
                                    unowned_modules.push(mp.display().to_string());
                                }
                            } else {
                                unowned_modules.push(token.to_string());
                            }
                        }
                    }
                }
            }

            let mut flags = Vec::new();
            if !unowned_modules.is_empty() {
                flags.push("unowned_pam_module".to_string());
            }

            let details = json!({
                "referenced_modules": referenced_modules,
                "unowned_modules": unowned_modules,
            });

            self.emit_file_state_with_extra("persist.pam", &path, details, flags, sink);
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

    fn collect_udev_and_net(&mut self, sink: &mut Sink) {
        let udev_dir = self.root.join("etc/udev/rules.d");
        if udev_dir.is_dir()
            && let Ok(entries) = fs::read_dir(udev_dir)
        {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    self.emit_file_state("persist.udev", &path, json!({}), sink);
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
                    self.emit_file_state("persist.net_dispatcher", &path, json!({}), sink);
                }
            }
        }
    }

    fn collect_apt_config(&mut self, sink: &mut Sink) {
        let apt_conf_d = self.root.join("etc/apt/apt.conf.d");
        if apt_conf_d.is_dir()
            && let Ok(entries) = fs::read_dir(apt_conf_d)
        {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    self.emit_file_state("persist.apt_config", &path, json!({}), sink);
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
                    self.emit_file_state("persist.apt_config", &path, json!({}), sink);
                }
            }
        }
    }

    fn collect_suid(&mut self, sink: &mut Sink) {
        let search_dirs = [
            "bin",
            "sbin",
            "usr/bin",
            "usr/sbin",
            "usr/local/bin",
            "usr/local/sbin",
        ];
        let mut suid_files = Vec::new();

        for dir_name in &search_dirs {
            let dir = self.root.join(dir_name);
            if !dir.is_dir() {
                continue;
            }
            if let Ok(entries) = fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if let Ok(meta) = entry.metadata() {
                        let mode = meta.mode();
                        if meta.is_file() && (mode & (0o4000 | 0o2000)) != 0 {
                            suid_files.push(path);
                        }
                    }
                }
            }
        }

        let owners = self
            .verifier
            .lookup_owners(&suid_files.iter().map(|p| p.as_path()).collect::<Vec<_>>());

        for path in suid_files {
            let owner = owners.get(&path).and_then(|o| o.as_ref());
            if owner.is_none() {
                let mut flags = vec!["unowned".to_string(), "unowned_suid".to_string()];
                if let Ok(meta) = fs::metadata(&path) {
                    let mtime_us = meta.mtime() * 1_000_000 + meta.mtime_nsec() / 1_000;
                    if in_window(self.ctx, mtime_us) {
                        flags.push("mtime_in_window".to_string());
                    }
                }
                self.emit_file_state_with_extra(
                    "persist.suid",
                    &path,
                    json!({ "unowned_suid": true }),
                    flags,
                    sink,
                );
            }
        }
    }

    fn emit_file_state(
        &mut self,
        kind: &str,
        path: &Path,
        details: serde_json::Value,
        sink: &mut Sink,
    ) {
        self.emit_file_state_with_extra(kind, path, details, Vec::new(), sink);
    }

    fn emit_file_state_with_extra(
        &mut self,
        kind: &str,
        path: &Path,
        details: serde_json::Value,
        extra_flags: Vec<String>,
        sink: &mut Sink,
    ) {
        emit_state_entry(
            kind,
            path,
            details,
            extra_flags,
            &self.verifier,
            self.ctx,
            sink,
        );
    }
}

struct UserPersistSnapshot<'a> {
    home: &'a Path,
    ctx: &'a CollectCtx,
    verifier: PkgVerifier,
}

impl<'a> UserPersistSnapshot<'a> {
    fn new(home: &'a Path, ctx: &'a CollectCtx, verifier: PkgVerifier) -> Self {
        Self {
            home,
            ctx,
            verifier,
        }
    }

    fn run(&mut self, sink: &mut Sink) {
        self.collect_ssh_keys(sink);
        self.collect_systemd_user(sink);
        self.collect_autostart(sink);
        self.collect_shell_rc(sink);
        self.collect_git_hooks(sink);
    }

    fn collect_ssh_keys(&mut self, sink: &mut Sink) {
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
                    sink,
                );
            }
        }
    }

    fn collect_systemd_user(&mut self, sink: &mut Sink) {
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
                self.emit_file_state("persist.systemd", &path, json!({}), sink);
            }
        }
    }

    fn collect_autostart(&mut self, sink: &mut Sink) {
        let dir = self.home.join(".config/autostart");
        if dir.is_dir()
            && let Ok(entries) = fs::read_dir(dir)
        {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    let exec_line = read_desktop_exec(&path);
                    self.emit_file_state(
                        "persist.autostart",
                        &path,
                        json!({ "exec": exec_line }),
                        sink,
                    );
                }
            }
        }
    }

    fn collect_shell_rc(&mut self, sink: &mut Sink) {
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
                self.emit_file_state_with_extra("persist.shell_rc", &path, details, flags, sink);
            }
        }
    }

    fn collect_git_hooks(&mut self, sink: &mut Sink) {
        if !self.home.is_dir() {
            return;
        }
        if let Ok(entries) = fs::read_dir(self.home) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let hook_dir = path.join(".git/hooks");
                    if hook_dir.is_dir()
                        && let Ok(hooks) = fs::read_dir(&hook_dir)
                    {
                        for hook in hooks.flatten() {
                            let hp = hook.path();
                            let name = hook.file_name();
                            let name_str = name.to_string_lossy();
                            if hp.is_file() && !name_str.ends_with(".sample") {
                                self.emit_file_state("persist.git_hook", &hp, json!({}), sink);
                            }
                        }
                    }
                }
            }
        }
    }

    fn emit_file_state(
        &mut self,
        kind: &str,
        path: &Path,
        details: serde_json::Value,
        sink: &mut Sink,
    ) {
        self.emit_file_state_with_extra(kind, path, details, Vec::new(), sink);
    }

    fn emit_file_state_with_extra(
        &mut self,
        kind: &str,
        path: &Path,
        details: serde_json::Value,
        extra_flags: Vec<String>,
        sink: &mut Sink,
    ) {
        emit_state_entry(
            kind,
            path,
            details,
            extra_flags,
            &self.verifier,
            self.ctx,
            sink,
        );
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
    kind: &str,
    path: &Path,
    details: serde_json::Value,
    extra_flags: Vec<String>,
    verifier: &PkgVerifier,
    ctx: &CollectCtx,
    sink: &mut Sink,
) {
    if guard(path).is_err() {
        return;
    }

    let Ok(meta) = fs::metadata(path) else {
        return;
    };

    let sha256 = hash_file(path);
    let mtime_us = meta.mtime() * 1_000_000 + meta.mtime_nsec() / 1_000;
    let ctime_us = meta.ctime() * 1_000_000 + meta.ctime_nsec() / 1_000;
    let size = meta.len() as i64;

    let owners = verifier.lookup_owners(&[path]);
    let owner_pkg = owners.get(path).and_then(|o| o.clone());

    let integrity = if let Some(ref pkg) = owner_pkg {
        verifier.verify_package(pkg, path)
    } else {
        "unowned"
    };

    let mut flags: HashSet<String> = extra_flags.into_iter().collect();
    if owner_pkg.is_none() {
        flags.insert("unowned".to_string());
    }
    if integrity == "mismatch" {
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

fn hash_file(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = file.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Some(hex::encode(hasher.finalize()))
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
    fn test_hash_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "footprint-test").unwrap();
        let h = hash_file(&file).unwrap();
        assert!(!h.is_empty());
    }

    #[test]
    fn test_world_writable_path_detection() {
        let tmp = Path::new("/tmp/test_dir");
        // /tmp is world-writable
        assert!(is_world_writable_or_has_world_writable_parent(tmp));
    }
}
