//! Package manager fallbacks: pacman, rpm/dnf, zypper, snap, and flatpak.

use crate::collector::{CollectCtx, Collector, Env, Sink};
use crate::event::{Origin, Stamp, new_event};
use crate::probe::Probe;
use crate::utils::{in_window, read_evidence, with_rotations};
use anyhow::Result;
use fp_core::time::TimeFormat;
use fp_core::{Actor, Availability, EventKind, SourceInstance};
use serde_json::json;
use std::process::{Command, Stdio};

const PACMAN_LOG: &str = "/var/log/pacman.log";
const DNF_RPM_LOG: &str = "/var/log/dnf.rpm.log";
const DNF_LOG: &str = "/var/log/dnf.log";
const ZYPPER_HISTORY: &str = "/var/log/zypp/history";

pub struct PacmanCollector;

impl Collector for PacmanCollector {
    fn kind(&self) -> &'static str {
        "pkg.pacman"
    }

    fn discover(&self, env: &Env) -> Vec<SourceInstance> {
        let path = env.system_path(PACMAN_LOG);
        if !path.exists() {
            let mut inst = SourceInstance::new(self.kind(), "pacman", None, path);
            inst.status = Availability::NotInstalled;
            return vec![inst];
        }
        with_rotations(&path)
            .into_iter()
            .map(|p| SourceInstance::new(self.kind(), "pacman", None, p))
            .collect()
    }

    fn collect(&self, inst: &SourceInstance, ctx: &CollectCtx, sink: &mut Sink) -> Result<()> {
        let file = read_evidence(&inst.path)?;
        let origin = Origin {
            file: &inst.path,
            sha256: Some(&file.sha256),
        };
        let text = String::from_utf8_lossy(&file.bytes);

        for (idx, line) in text.lines().enumerate() {
            let line_no = idx + 1;
            // e.g. [2026-10-16T21:08:00+0330] [ALPM] installed zoom (7.2.1-1)
            let Some(rest) = line.strip_prefix('[') else {
                continue;
            };
            let Some((raw_ts, after_ts)) = rest.split_once(']') else {
                continue;
            };
            let Ok(stamp) = Stamp::parse(raw_ts, TimeFormat::Rfc3339, None) else {
                continue;
            };
            if !in_window(ctx, stamp.ts_utc) {
                continue;
            }

            let after_ts = after_ts.trim();
            let Some(alpm_rest) = after_ts.strip_prefix("[ALPM]").map(|s| s.trim()) else {
                continue;
            };

            let mut parts = alpm_rest.split_whitespace();
            let (kind, action) = match parts.next() {
                Some("installed") => (EventKind::PkgInstall, "install"),
                Some("upgraded") => (EventKind::PkgUpgrade, "upgrade"),
                Some("removed") => (EventKind::PkgRemove, "remove"),
                _ => continue,
            };

            let pkg = parts.next().unwrap_or("?");
            let summary = format!("{action} {pkg}");

            sink.event(fp_core::Event {
                actor: Some(Actor::app("pacman")),
                attrs: json!({
                    "action": action,
                    "package": pkg,
                    "raw": alpm_rest,
                }),
                ..new_event(
                    ctx,
                    inst,
                    stamp,
                    kind,
                    pkg.to_string(),
                    summary,
                    origin.at(format!("line {line_no}")),
                )
            });
        }
        Ok(())
    }
}

pub struct DnfCollector;

impl Collector for DnfCollector {
    fn kind(&self) -> &'static str {
        "pkg.dnf"
    }

    fn discover(&self, env: &Env) -> Vec<SourceInstance> {
        let dnf_rpm = env.system_path(DNF_RPM_LOG);
        let dnf = env.system_path(DNF_LOG);
        let path = if dnf_rpm.exists() {
            dnf_rpm
        } else if dnf.exists() {
            dnf
        } else {
            let mut inst = SourceInstance::new(self.kind(), "dnf", None, dnf_rpm);
            inst.status = Availability::NotInstalled;
            return vec![inst];
        };

        with_rotations(&path)
            .into_iter()
            .map(|p| SourceInstance::new(self.kind(), "dnf", None, p))
            .collect()
    }

    fn collect(&self, inst: &SourceInstance, ctx: &CollectCtx, sink: &mut Sink) -> Result<()> {
        let file = read_evidence(&inst.path)?;
        let origin = Origin {
            file: &inst.path,
            sha256: Some(&file.sha256),
        };
        let text = String::from_utf8_lossy(&file.bytes);

        for (idx, line) in text.lines().enumerate() {
            let line_no = idx + 1;
            // e.g. 2026-10-16T21:08:00+03:30 SUBDEBUG Installed: zoom-7.2.1.x86_64
            // or 2026-10-16 21:08:00 INFO Installed: zoom-7.2.1.x86_64
            let (raw_ts, rest) = if let Some(i) = line
                .find("Installed:")
                .or_else(|| line.find("Upgraded:"))
                .or_else(|| line.find("Erased:"))
            {
                let (ts_part, action_part) = line.split_at(i);
                (ts_part.trim(), action_part)
            } else {
                continue;
            };

            let Some(stamp) = parse_dnf_stamp(raw_ts, ctx.assume_tz.as_deref()) else {
                continue;
            };

            if !in_window(ctx, stamp.ts_utc) {
                continue;
            }

            let (action, pkg) = match rest.split_once(':') {
                Some((a, p)) => (a.trim(), p.trim()),
                None => continue,
            };

            let kind = match action {
                "Installed" => EventKind::PkgInstall,
                "Upgraded" => EventKind::PkgUpgrade,
                "Erased" => EventKind::PkgRemove,
                _ => continue,
            };

            let summary = format!("{action}: {pkg}");
            sink.event(fp_core::Event {
                actor: Some(Actor::app("dnf")),
                attrs: json!({
                    "action": action,
                    "package": pkg,
                }),
                ..new_event(
                    ctx,
                    inst,
                    stamp,
                    kind,
                    pkg.to_string(),
                    summary,
                    origin.at(format!("line {line_no}")),
                )
            });
        }
        Ok(())
    }
}

pub struct ZypperCollector;

impl Collector for ZypperCollector {
    fn kind(&self) -> &'static str {
        "pkg.zypper"
    }

    fn discover(&self, env: &Env) -> Vec<SourceInstance> {
        let path = env.system_path(ZYPPER_HISTORY);
        if !path.exists() {
            let mut inst = SourceInstance::new(self.kind(), "zypper", None, path);
            inst.status = Availability::NotInstalled;
            return vec![inst];
        }
        vec![SourceInstance::new(self.kind(), "zypper", None, path)]
    }

    fn collect(&self, inst: &SourceInstance, ctx: &CollectCtx, sink: &mut Sink) -> Result<()> {
        let file = read_evidence(&inst.path)?;
        let origin = Origin {
            file: &inst.path,
            sha256: Some(&file.sha256),
        };
        let text = String::from_utf8_lossy(&file.bytes);

        for (idx, line) in text.lines().enumerate() {
            let line_no = idx + 1;
            // format: 2026-10-16 21:08:00|install|zoom|7.2.1|...
            let parts: Vec<&str> = line.split('|').collect();
            if parts.len() < 4 {
                continue;
            }
            let Ok(stamp) = Stamp::parse(
                parts[0],
                TimeFormat::LocalNaive("%Y-%m-%d %H:%M:%S".into()),
                ctx.assume_tz.as_deref(),
            ) else {
                continue;
            };
            if !in_window(ctx, stamp.ts_utc) {
                continue;
            }

            let action = parts[1];
            let pkg = parts[2];
            let ver = parts[3];

            let (kind, action_name) = match action {
                "install" => (EventKind::PkgInstall, "install"),
                "remove" => (EventKind::PkgRemove, "remove"),
                "upgrade" => (EventKind::PkgUpgrade, "upgrade"),
                _ => continue,
            };

            let summary = format!("{action_name} {pkg} {ver}");
            sink.event(fp_core::Event {
                actor: Some(Actor::app("zypper")),
                attrs: json!({
                    "action": action_name,
                    "package": pkg,
                    "version": ver,
                }),
                ..new_event(
                    ctx,
                    inst,
                    stamp,
                    kind,
                    pkg.to_string(),
                    summary,
                    origin.at(format!("line {line_no}")),
                )
            });
        }
        Ok(())
    }
}

pub struct SnapCollector;

impl Collector for SnapCollector {
    fn kind(&self) -> &'static str {
        "pkg.snap"
    }

    fn discover(&self, env: &Env) -> Vec<SourceInstance> {
        let path =
            Probe::tool_path(env, "snap").unwrap_or_else(|| env.system_path("/usr/bin/snap"));
        let mut inst = SourceInstance::new(self.kind(), "snap", None, path);
        if !inst.path.is_file() {
            inst.status = Availability::NotInstalled;
        }
        vec![inst]
    }

    fn collect(&self, inst: &SourceInstance, ctx: &CollectCtx, sink: &mut Sink) -> Result<()> {
        let output = Command::new(&inst.path)
            .args(["changes"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output();
        let Ok(out) = output else {
            return Ok(());
        };
        if !out.status.success() {
            return Ok(());
        }

        let origin = Origin {
            file: &inst.path,
            sha256: None,
        };
        let text = String::from_utf8_lossy(&out.stdout);
        for (idx, line) in text.lines().enumerate() {
            // 1  Done  2026-10-16T21:08:00+03:30  2026-10-16T21:08:05+03:30  install  Install "zoom-client" snap
            let mut parts = line.split_whitespace();
            if parts.next().is_none_or(|id| id.parse::<u64>().is_err()) {
                continue; // the header row
            }
            let _status = parts.next();
            let Some(spawn) = parts.next() else {
                continue;
            };
            let _ready = parts.next();
            let Some(kind) = parts.next() else {
                continue;
            };
            let summary = parts.collect::<Vec<_>>().join(" ");
            let (kind, action) = match kind {
                "install" => (EventKind::PkgInstall, "install"),
                "refresh" => (EventKind::PkgUpgrade, "refresh"),
                "remove" => (EventKind::PkgRemove, "remove"),
                _ => continue,
            };
            let Ok(stamp) = Stamp::parse(spawn, TimeFormat::Rfc3339, None).or_else(|_| {
                Stamp::parse(
                    spawn,
                    TimeFormat::LocalNaive("%Y-%m-%dT%H:%M:%S".into()),
                    ctx.assume_tz.as_deref(),
                )
            }) else {
                continue;
            };
            if !in_window(ctx, stamp.ts_utc) {
                continue;
            }
            sink.event(fp_core::Event {
                actor: Some(Actor::app("snap")),
                attrs: json!({
                    "action": action,
                    "summary": summary,
                }),
                ..new_event(
                    ctx,
                    inst,
                    stamp,
                    kind,
                    summary.clone(),
                    format!("{action}: {summary}"),
                    origin.at(format!("changes id, line {}", idx + 1)),
                )
            });
        }
        Ok(())
    }
}

pub struct FlatpakCollector;

impl Collector for FlatpakCollector {
    fn kind(&self) -> &'static str {
        "pkg.flatpak"
    }

    fn discover(&self, env: &Env) -> Vec<SourceInstance> {
        let tool = Probe::tool_path(env, "flatpak");
        let path = tool.unwrap_or_else(|| env.system_path("/usr/bin/flatpak"));
        let mut inst = SourceInstance::new(self.kind(), "flatpak", None, path);
        if !inst.path.is_file() {
            inst.status = Availability::NotInstalled;
        }
        vec![inst]
    }

    fn collect(&self, inst: &SourceInstance, ctx: &CollectCtx, sink: &mut Sink) -> Result<()> {
        let output = Command::new(&inst.path)
            .args(["history", "--columns=time,change,application"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output();

        let Ok(out) = output else {
            return Ok(());
        };
        if !out.status.success() {
            return Ok(());
        }

        let origin = Origin {
            file: &inst.path,
            sha256: None,
        };
        let text = String::from_utf8_lossy(&out.stdout);

        for (idx, line) in text.lines().enumerate() {
            let line_no = idx + 1;
            let parts: Vec<&str> = line.split('\t').map(|s| s.trim()).collect();
            if parts.len() < 3 {
                continue;
            }
            let Ok(stamp) = Stamp::parse(
                parts[0],
                TimeFormat::LocalNaive("%Y-%m-%d %H:%M:%S".into()),
                ctx.assume_tz.as_deref(),
            ) else {
                continue;
            };
            if !in_window(ctx, stamp.ts_utc) {
                continue;
            }
            let change = parts[1];
            let app = parts[2];
            let kind = match change {
                "install" | "update" => EventKind::PkgInstall,
                "uninstall" => EventKind::PkgRemove,
                _ => EventKind::PkgInstall,
            };
            let summary = format!("{change} {app}");
            sink.event(fp_core::Event {
                actor: Some(Actor::app("flatpak")),
                attrs: json!({
                    "change": change,
                    "application": app,
                }),
                ..new_event(
                    ctx,
                    inst,
                    stamp,
                    kind,
                    app.to_string(),
                    summary,
                    origin.at(format!("row {line_no}")),
                )
            });
        }
        Ok(())
    }
}

/// dnf stamps come in three shapes: `2026-10-16T21:08:00+0330` (offset without colon),
/// `2026-10-16T21:08:00+03:30`, and `2026-10-16 21:08:00 INFO …` (level word follows the time).
fn parse_dnf_stamp(text: &str, assume_tz: Option<&str>) -> Option<Stamp> {
    let mut words = text.split_whitespace();
    let first = words.next()?;
    let mut candidates = vec![normalize_offset(first)];
    if let Some(second) = words.next() {
        candidates.push(format!("{first} {second}"));
    }
    for candidate in candidates {
        if let Ok(stamp) = Stamp::parse(
            candidate.clone(),
            TimeFormat::LocalNaive("%Y-%m-%d %H:%M:%S".into()),
            assume_tz,
        ) {
            return Some(stamp);
        }
        if let Ok(stamp) = Stamp::parse(candidate, TimeFormat::Rfc3339, None) {
            return Some(stamp);
        }
    }
    None
}

/// chrono's RFC 3339 needs `±HH:MM`; dnf writes `±HHMM`.
fn normalize_offset(ts: &str) -> String {
    let Some(pos) = ts.rfind(['+', '-']) else {
        return ts.to_string();
    };
    let digits: String = ts[pos + 1..]
        .chars()
        .filter(|c| c.is_ascii_digit())
        .collect();
    if digits.len() == 4 {
        format!(
            "{}{}{}:{}",
            &ts[..pos],
            &ts[pos..pos + 1],
            &digits[..2],
            &digits[2..]
        )
    } else {
        ts.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dnf_log_with_space_separated_stamp() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("dnf.log");
        std::fs::write(
            &log,
            "2026-10-16 21:08:00 INFO Installed: zoom-7.2.1.x86_64\n\
             2026-10-16T21:09:00+0330 SUBDEBUG Upgraded: curl-8.2.0.x86_64\n",
        )
        .unwrap();

        let inst = SourceInstance::new("pkg.dnf", "dnf", None, log);
        let ctx = CollectCtx {
            run_id: 1,
            since: None,
            until: None,
            assume_tz: Some("+03:30".into()),
            temp_dir: dir.path().join("tmp"),
        };
        let mut sink = Sink::default();
        DnfCollector.collect(&inst, &ctx, &mut sink).unwrap();

        assert_eq!(sink.events.len(), 2);
        assert_eq!(sink.events[0].kind, EventKind::PkgInstall);
        // the time of day must not be dropped: 21:08:00 +03:30
        assert_eq!(sink.events[0].ts_utc, 1_792_172_280_000_000);
        // the colon-less `+0330` offset is normalized: 21:09:00 +03:30
        assert_eq!(sink.events[1].kind, EventKind::PkgUpgrade);
        assert_eq!(sink.events[1].ts_utc, 1_792_172_340_000_000);
    }

    #[test]
    fn parses_snap_changes() {
        let dir = tempfile::tempdir().unwrap();
        let snap = dir.path().join("snap");
        std::fs::write(
            &snap,
            "#!/bin/sh\nprintf '%s\n' \
             'ID   Status  Spawn                     Ready                     Kind       Summary' \
             '1    Done    2026-10-16T21:08:00+03:30  2026-10-16T21:08:05+03:30  install    Install \"zoom-client\" snap' \
             '2    Done    2026-10-16T21:10:00+03:30  2026-10-16T21:10:03+03:30  refresh    Refresh snap \"curl\"' \
             '3    Error   2026-10-16T21:12:00+03:30  -                         remove     Remove snap \"nano\"'\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&snap, std::fs::Permissions::from_mode(0o755)).unwrap();

        let inst = SourceInstance::new("pkg.snap", "snap", None, snap);
        let ctx = CollectCtx {
            run_id: 1,
            since: None,
            until: None,
            assume_tz: None,
            temp_dir: dir.path().join("tmp"),
        };
        let mut sink = Sink::default();
        SnapCollector.collect(&inst, &ctx, &mut sink).unwrap();

        let got: Vec<_> = sink
            .events
            .iter()
            .map(|e| (e.kind, e.ts_utc, e.summary.as_str()))
            .collect();
        assert!(
            got.contains(&(
                EventKind::PkgInstall,
                1_792_172_280_000_000,
                "install: Install \"zoom-client\" snap"
            )),
            "{got:?}"
        );
        assert!(
            got.contains(&(
                EventKind::PkgUpgrade,
                1_792_172_400_000_000,
                "refresh: Refresh snap \"curl\""
            )),
            "{got:?}"
        );
        assert!(
            got.contains(&(
                EventKind::PkgRemove,
                1_792_172_520_000_000,
                "remove: Remove snap \"nano\""
            )),
            "{got:?}"
        );
    }

    #[test]
    fn parses_pacman_log() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("pacman.log");
        std::fs::write(
            &log,
            "[2026-10-16T21:08:00+0330] [ALPM] installed zoom (7.2.1-1)\n\
             [2026-10-16T21:09:00+0330] [ALPM] upgraded curl (8.1.0 -> 8.2.0)\n\
             [2026-10-16T21:10:00+0330] [ALPM] removed nano (7.2-1)\n",
        )
        .unwrap();

        let inst = SourceInstance::new("pkg.pacman", "pacman", None, log);
        let ctx = CollectCtx {
            run_id: 1,
            since: None,
            until: None,
            assume_tz: None,
            temp_dir: dir.path().join("tmp"),
        };
        let mut sink = Sink::default();
        PacmanCollector.collect(&inst, &ctx, &mut sink).unwrap();

        assert_eq!(sink.events.len(), 3);
        assert_eq!(sink.events[0].kind, EventKind::PkgInstall);
        assert_eq!(sink.events[0].subject, "zoom");
        assert_eq!(sink.events[1].kind, EventKind::PkgUpgrade);
        assert_eq!(sink.events[2].kind, EventKind::PkgRemove);
    }
}
