use crate::collector::{CollectCtx, Collector, Env, Sink};
use crate::event::{Stamp, new_event};
use crate::utils::{guard, in_window};
use anyhow::Result;
use fp_core::{
    Artifact, Availability, Event, EventKind, Provenance, SourceInstance, time::TimeFormat,
};
use serde_json::json;
use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

pub struct FilesystemCollector;

const NOISE_DIR_NAMES: &[&str] = &[
    ".cache",
    "Cache",
    "GPUCache",
    "Code Cache",
    "node_modules",
    "target",
    ".rustup",
    ".cargo",
    "logs",
    ".var",
    "snap",
];

impl FilesystemCollector {
    fn roots(env: &Env) -> Vec<(String, PathBuf)> {
        let mut r = Vec::new();
        for target in &env.user_targets {
            if target.home.is_dir() {
                r.push((format!("home:{}", target.user), target.home.clone()));
            }
        }
        if r.is_empty() && env.home.is_dir() {
            r.push(("home".to_string(), env.home.clone()));
        }
        for sys_dir in &["/tmp", "/var/tmp", "/dev/shm", "/etc"] {
            let p = env.system_path(sys_dir);
            if p.exists() {
                r.push((sys_dir.trim_start_matches('/').to_string(), p));
            }
        }
        r
    }
}

impl Collector for FilesystemCollector {
    fn kind(&self) -> &'static str {
        "fs.statx"
    }

    fn discover(&self, env: &Env) -> Vec<SourceInstance> {
        let mut out = Vec::new();
        for (name, path) in Self::roots(env) {
            let mut inst = SourceInstance::new(self.kind(), &name, None, path);
            if !inst.path.exists() {
                inst.status = Availability::NotInstalled;
            }
            out.push(inst);
        }
        out
    }

    fn collect(&self, inst: &SourceInstance, ctx: &CollectCtx, sink: &mut Sink) -> Result<()> {
        if !inst.path.exists() {
            return Ok(());
        }

        if inst.path.is_file() {
            inspect_file(&inst.path, inst, ctx, sink);
            return Ok(());
        }

        let Ok(root_meta) = fs::metadata(&inst.path) else {
            return Ok(());
        };
        let root_dev = root_meta.dev();

        walk_dir(&inst.path, root_dev, 0, inst, ctx, sink);
        Ok(())
    }
}

pub(crate) fn is_noise_dir(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    if NOISE_DIR_NAMES.contains(&name) {
        return true;
    }
    if name == "objects"
        && path
            .parent()
            .and_then(|p| p.file_name())
            .is_some_and(|p| p == ".git")
    {
        return true;
    }
    false
}

fn walk_dir(
    dir: &Path,
    root_dev: u64,
    depth: usize,
    inst: &SourceInstance,
    ctx: &CollectCtx,
    sink: &mut Sink,
) {
    if depth > 15 {
        return;
    }
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        // our own staging copies (browser databases being read right now) and the case being
        // written are not activity
        if guard(&path).is_err()
            || is_noise_dir(&path)
            || path == ctx.temp_dir
            || ctx.own_files.contains(&path)
        {
            continue;
        }

        let Ok(ft) = entry.file_type() else {
            continue;
        };

        if ft.is_dir() {
            if let Ok(meta) = entry.metadata() {
                // Stay on same filesystem (-xdev)
                if meta.dev() == root_dev {
                    walk_dir(&path, root_dev, depth + 1, inst, ctx, sink);
                }
            }
            continue;
        }

        inspect_file(&path, inst, ctx, sink);
    }
}

struct StatInfo {
    size: i64,
    mtime_ns: i64,
    ctime_ns: i64,
    btime_ns: Option<i64>,
}

/// Saturating: a crafted or far-future timestamp (ext4 goes to 2446) must not overflow i64 ns.
fn file_time_ns(sec: i64, nsec: i64) -> i64 {
    sec.saturating_mul(1_000_000_000).saturating_add(nsec)
}

fn stat_entry(path: &Path) -> Option<StatInfo> {
    let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: statx only writes into the zeroed buffer we pass and reads the NUL-terminated path
    let mut buf: libc::statx = unsafe { std::mem::zeroed() };
    let res = unsafe {
        libc::statx(
            libc::AT_FDCWD,
            c_path.as_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
            libc::STATX_BASIC_STATS | libc::STATX_BTIME,
            &mut buf,
        )
    };
    if res == 0 {
        let ts = |t: libc::statx_timestamp| file_time_ns(t.tv_sec, i64::from(t.tv_nsec));
        return Some(StatInfo {
            size: buf.stx_size as i64,
            mtime_ns: ts(buf.stx_mtime),
            ctime_ns: ts(buf.stx_ctime),
            btime_ns: (buf.stx_mask & libc::STATX_BTIME != 0).then(|| ts(buf.stx_btime)),
        });
    }
    let meta = fs::symlink_metadata(path).ok()?;
    Some(StatInfo {
        size: meta.len() as i64,
        mtime_ns: file_time_ns(meta.mtime(), meta.mtime_nsec()),
        ctime_ns: file_time_ns(meta.ctime(), meta.ctime_nsec()),
        btime_ns: None,
    })
}

fn inspect_file(path: &Path, inst: &SourceInstance, ctx: &CollectCtx, sink: &mut Sink) {
    let Some(info) = stat_entry(path) else {
        return;
    };
    let created = info.btime_ns.filter(|&bt| in_window(ctx, bt / 1_000));
    // a modification is only news when it came after the creation
    let modified = Some(info.mtime_ns).filter(|&mt| {
        in_window(ctx, mt / 1_000) && info.btime_ns.is_none_or(|bt| mt / 1_000 > bt / 1_000)
    });
    if created.is_none() && modified.is_none() {
        return;
    }

    sink.artifact(Artifact {
        path: path.to_path_buf(),
        sha256: None,
        size: Some(info.size),
        mtime_ns: Some(info.mtime_ns),
        ctime_ns: Some(info.ctime_ns),
        btime_ns: info.btime_ns,
        owner_pkg: None,
    });
    let path_str = path.display().to_string();
    let changes = [
        (created, EventKind::FileCreated, "created", "statx:btime:"),
        (modified, EventKind::FileModified, "modified", "statx:"),
    ];
    for (ts_ns, kind, verb, locator) in changes {
        let Some(stamp) =
            ts_ns.and_then(|ns| Stamp::parse(ns.to_string(), TimeFormat::FileTime, None).ok())
        else {
            continue;
        };
        sink.event(Event {
            attrs: json!({
                "size": info.size,
                "mtime_ns": info.mtime_ns,
                "ctime_ns": info.ctime_ns,
                "btime_ns": info.btime_ns,
            }),
            ..new_event(
                ctx,
                inst,
                stamp,
                kind,
                path_str.clone(),
                format!("{path_str} ({verb})"),
                Provenance {
                    file: path.to_path_buf(),
                    file_sha256: None,
                    locator: format!("{locator}{path_str}"),
                },
            )
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walk_skips_the_runs_own_staging_dir_and_case() {
        let root = tempfile::tempdir().unwrap();
        let stage = root.path().join("footprint-abc");
        std::fs::create_dir_all(stage.join("src-1")).unwrap();
        std::fs::write(stage.join("src-1/History"), b"copy").unwrap();
        std::fs::write(root.path().join("dropper.sh"), b"x").unwrap();
        let case = root.path().join("case.db");
        std::fs::write(&case, b"db").unwrap();
        std::fs::write(root.path().join("case.db-wal"), b"wal").unwrap();
        let ctx = CollectCtx {
            run_id: 1,
            since: None,
            until: None,
            assume_tz: None,
            temp_dir: stage,
            own_files: vec![case.clone(), root.path().join("case.db-wal")],
        };
        let inst = SourceInstance::new("fs.statx", "tmp", None, root.path().to_path_buf());
        let mut sink = Sink::default();
        FilesystemCollector.collect(&inst, &ctx, &mut sink).unwrap();
        let subjects: Vec<_> = sink.events.iter().map(|e| e.subject.as_str()).collect();
        assert!(
            subjects.iter().any(|s| s.ends_with("dropper.sh")),
            "{subjects:?}"
        );
        assert!(
            !subjects
                .iter()
                .any(|s| s.contains("footprint-abc") || s.contains("case.db")),
            "{subjects:?}"
        );
    }

    #[test]
    fn far_future_file_times_saturate() {
        // year 2300 is valid on ext4 but past i64 nanoseconds
        assert_eq!(file_time_ns(10_414_285_200, 5), i64::MAX);
        assert_eq!(file_time_ns(1_792_172_917, 5), 1_792_172_917_000_000_005);
    }

    #[test]
    fn test_stat_entry_on_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        fs::write(&file_path, "hello").unwrap();

        let info = stat_entry(&file_path).unwrap();
        assert_eq!(info.size, 5);
        assert!(info.mtime_ns > 0);
    }

    #[test]
    fn test_noise_dir_detection() {
        assert!(is_noise_dir(Path::new("/home/user/.cache")));
        assert!(is_noise_dir(Path::new("/home/user/project/target")));
        assert!(is_noise_dir(Path::new("/home/user/project/node_modules")));
        assert!(is_noise_dir(Path::new("/home/user/project/.git/objects")));
        assert!(!is_noise_dir(Path::new("/home/user/project/.git/hooks")));
        assert!(!is_noise_dir(Path::new("/home/user/Downloads")));
    }
}
