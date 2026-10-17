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
                let first_word = ts_part.split_whitespace().next().unwrap_or_default();
                (first_word, action_part)
            } else {
                continue;
            };

            let Ok(stamp) = Stamp::parse(raw_ts, TimeFormat::Rfc3339, None).or_else(|_| {
                Stamp::parse(
                    raw_ts,
                    TimeFormat::LocalNaive("%Y-%m-%d".into()),
                    ctx.assume_tz.as_deref(),
                )
            }) else {
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

#[cfg(test)]
mod tests {
    use super::*;

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
