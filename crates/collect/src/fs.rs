use crate::collector::{CollectCtx, Collector, Env, Sink};
use crate::event::{Stamp, new_event};
use crate::utils::{guard, in_window};
use anyhow::Result;
use fp_core::{
    Artifact, Availability, Event, EventKind, Provenance, SourceInstance, TimePrecision,
    time::TimeFormat,
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

fn is_noise_dir(path: &Path) -> bool {
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
        if guard(&path).is_err() {
            continue;
        }

        if is_noise_dir(&path) {
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
    mtime_us: i64,
    _ctime_us: i64,
    btime_us: Option<i64>,
    size: i64,
    mtime_ns: i64,
    ctime_ns: i64,
    btime_ns: Option<i64>,
}

fn stat_entry(path: &Path) -> Option<StatInfo> {
    let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut statx_buf: libc::statx = unsafe { std::mem::zeroed() };
    let res = unsafe {
        libc::statx(
            libc::AT_FDCWD,
            c_path.as_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
            libc::STATX_BASIC_STATS | libc::STATX_BTIME,
            &mut statx_buf,
        )
    };

    if res == 0 {
        let mtime_ns = (statx_buf.stx_mtime.tv_sec as i64) * 1_000_000_000
            + (statx_buf.stx_mtime.tv_nsec as i64);
        let ctime_ns = (statx_buf.stx_ctime.tv_sec as i64) * 1_000_000_000
            + (statx_buf.stx_ctime.tv_nsec as i64);
        let btime_ns = if statx_buf.stx_mask & libc::STATX_BTIME != 0 {
            Some(
                (statx_buf.stx_btime.tv_sec as i64) * 1_000_000_000
                    + (statx_buf.stx_btime.tv_nsec as i64),
            )
        } else {
            None
        };
        Some(StatInfo {
            mtime_us: mtime_ns / 1_000,
            _ctime_us: ctime_ns / 1_000,
            btime_us: btime_ns.map(|ns| ns / 1_000),
            size: statx_buf.stx_size as i64,
            mtime_ns,
            ctime_ns,
            btime_ns,
        })
    } else {
        // Fallback to symlink_metadata
        let meta = fs::symlink_metadata(path).ok()?;
        let mtime_ns = meta.mtime() * 1_000_000_000 + (meta.mtime_nsec() as i64);
        let ctime_ns = meta.ctime() * 1_000_000_000 + (meta.ctime_nsec() as i64);
        Some(StatInfo {
            mtime_us: mtime_ns / 1_000,
            _ctime_us: ctime_ns / 1_000,
            btime_us: None,
            size: meta.len() as i64,
            mtime_ns,
            ctime_ns,
            btime_ns: None,
        })
    }
}

fn inspect_file(path: &Path, inst: &SourceInstance, ctx: &CollectCtx, sink: &mut Sink) {
    let Some(info) = stat_entry(path) else {
        return;
    };

    let path_str = path.display().to_string();

    let btime_in_window = info.btime_us.is_some_and(|bt| in_window(ctx, bt));
    let mtime_in_window = in_window(ctx, info.mtime_us);

    if !btime_in_window && !mtime_in_window {
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

    if let Some(btime_us) = info.btime_us {
        if btime_in_window {
            let stamp = Stamp {
                ts_utc: btime_us,
                precision: TimePrecision::Exact,
                raw: btime_us.to_string(),
                format: TimeFormat::FileTime,
                tz_assumed: None,
            };
            sink.event(Event {
                actor: None,
                subject_host: None,
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
                    EventKind::FileCreated,
                    path_str.clone(),
                    format!("{path_str} (created)"),
                    Provenance {
                        file: path.to_path_buf(),
                        file_sha256: None,
                        locator: format!("statx:btime:{path_str}"),
                    },
                )
            });
        }

        if info.mtime_us > btime_us && mtime_in_window {
            let stamp = Stamp {
                ts_utc: info.mtime_us,
                precision: TimePrecision::Exact,
                raw: info.mtime_us.to_string(),
                format: TimeFormat::FileTime,
                tz_assumed: None,
            };
            sink.event(Event {
                actor: None,
                subject_host: None,
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
                    EventKind::FileModified,
                    path_str.clone(),
                    format!("{path_str} (modified)"),
                    Provenance {
                        file: path.to_path_buf(),
                        file_sha256: None,
                        locator: format!("statx:{path_str}"),
                    },
                )
            });
        }
    } else if mtime_in_window {
        let stamp = Stamp {
            ts_utc: info.mtime_us,
            precision: TimePrecision::Exact,
            raw: info.mtime_us.to_string(),
            format: TimeFormat::FileTime,
            tz_assumed: None,
        };
        sink.event(Event {
            actor: None,
            subject_host: None,
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
                EventKind::FileModified,
                path_str.clone(),
                format!("{path_str} (modified)"),
                Provenance {
                    file: path.to_path_buf(),
                    file_sha256: None,
                    locator: format!("statx:{path_str}"),
                },
            )
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stat_entry_on_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        fs::write(&file_path, "hello").unwrap();

        let info = stat_entry(&file_path).unwrap();
        assert_eq!(info.size, 5);
        assert!(info.mtime_us > 0);
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
