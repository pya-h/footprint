use crate::collector::{CollectCtx, Collector, Env, Sink};
use crate::event::{Origin, Stamp, new_event};
use crate::utils::{in_window, read_evidence, with_rotations};
use anyhow::Result;
use fp_core::time::TimeFormat;
use fp_core::{Actor, EventKind, SourceInstance};
use serde_json::json;

const DPKG_LOG: &str = "/var/log/dpkg.log";
const APT_HISTORY: &str = "/var/log/apt/history.log";
const LOCAL_FORMAT: &str = "%Y-%m-%d %H:%M:%S";
/// `YYYY-MM-DD HH:MM:SS`
const DPKG_STAMP_LEN: usize = 19;
const SUMMARY_PACKAGES: usize = 3;

fn discover_rotations(env: &Env, kind: &str, app: &str, log: &str) -> Vec<SourceInstance> {
    with_rotations(&env.system_path(log))
        .into_iter()
        .map(|path| SourceInstance::new(kind, app, None, path))
        .collect()
}

fn local_format() -> TimeFormat {
    TimeFormat::LocalNaive(LOCAL_FORMAT.to_string())
}

pub struct DpkgCollector;

impl Collector for DpkgCollector {
    fn kind(&self) -> &'static str {
        "pkg.dpkg"
    }

    fn discover(&self, env: &Env) -> Vec<SourceInstance> {
        discover_rotations(env, self.kind(), "dpkg", DPKG_LOG)
    }

    fn collect(&self, inst: &SourceInstance, ctx: &CollectCtx, sink: &mut Sink) -> Result<()> {
        let file = read_evidence(&inst.path)?;
        let origin = Origin {
            file: &inst.path,
            sha256: Some(&file.sha256),
        };
        let text = String::from_utf8_lossy(&file.bytes);
        let mut skipped = Vec::new();

        for (idx, line) in text.lines().enumerate() {
            let line_no = idx + 1;
            let (Some(raw), Some(rest)) =
                (line.get(..DPKG_STAMP_LEN), line.get(DPKG_STAMP_LEN + 1..))
            else {
                skipped.push(line_no);
                continue;
            };
            let mut parts = rest.split_whitespace();
            let (kind, action) = match parts.next() {
                Some(a @ "install") => (EventKind::PkgInstall, a),
                Some(a @ "upgrade") => (EventKind::PkgUpgrade, a),
                Some(a @ ("remove" | "purge")) => (EventKind::PkgRemove, a),
                _ => continue,
            };
            let Ok(stamp) = Stamp::parse(raw, local_format(), ctx.assume_tz.as_deref()) else {
                skipped.push(line_no);
                continue;
            };
            if !in_window(ctx, stamp.ts_utc) {
                continue;
            }
            let package = parts.next().unwrap_or("?");
            let old_version = parts.next().filter(|v| *v != "<none>");
            let new_version = parts.next().filter(|v| *v != "<none>");
            let summary = match (kind, new_version) {
                (EventKind::PkgRemove, _) => {
                    format!("{action} {package} {}", old_version.unwrap_or(""))
                }
                (_, Some(v)) => format!("{package} {v}"),
                (_, None) => package.to_string(),
            };
            sink.event(fp_core::Event {
                actor: Some(Actor::app("dpkg")),
                attrs: json!({
                    "action": action,
                    "package": package,
                    "old_version": old_version,
                    "new_version": new_version,
                }),
                ..new_event(
                    ctx,
                    inst,
                    stamp,
                    kind,
                    package.to_string(),
                    summary.trim_end().to_string(),
                    origin.at(format!("line {line_no}")),
                )
            });
        }
        warn_skipped(sink, inst, &skipped);
        Ok(())
    }
}

pub struct AptCollector;

#[derive(Default)]
struct AptBlock {
    start: Option<String>,
    commandline: Option<String>,
    requested_by: Option<String>,
}

impl Collector for AptCollector {
    fn kind(&self) -> &'static str {
        "pkg.apt"
    }

    fn discover(&self, env: &Env) -> Vec<SourceInstance> {
        discover_rotations(env, self.kind(), "apt", APT_HISTORY)
    }

    fn collect(&self, inst: &SourceInstance, ctx: &CollectCtx, sink: &mut Sink) -> Result<()> {
        let file = read_evidence(&inst.path)?;
        let origin = Origin {
            file: &inst.path,
            sha256: Some(&file.sha256),
        };
        let text = String::from_utf8_lossy(&file.bytes);
        let mut block = AptBlock::default();
        let mut skipped = Vec::new();

        for (idx, line) in text.lines().enumerate() {
            let line_no = idx + 1;
            let Some((key, value)) = line.split_once(": ") else {
                continue;
            };
            let kind = match key {
                "Start-Date" => {
                    block = AptBlock {
                        start: Some(value.to_string()),
                        ..AptBlock::default()
                    };
                    continue;
                }
                "Commandline" => {
                    block.commandline = Some(value.to_string());
                    continue;
                }
                "Requested-By" => {
                    block.requested_by = Some(value.to_string());
                    continue;
                }
                "Install" | "Reinstall" => EventKind::PkgInstall,
                "Upgrade" | "Downgrade" => EventKind::PkgUpgrade,
                "Remove" | "Purge" => EventKind::PkgRemove,
                _ => continue,
            };
            let Some(start) = &block.start else {
                skipped.push(line_no);
                continue;
            };
            // apt writes two spaces between date and time
            let normalized = start.split_whitespace().collect::<Vec<_>>().join(" ");
            let Ok(mut stamp) = Stamp::parse(normalized, local_format(), ctx.assume_tz.as_deref())
            else {
                skipped.push(line_no);
                continue;
            };
            if !in_window(ctx, stamp.ts_utc) {
                continue;
            }
            stamp.raw = start.clone();

            let packages = parse_packages(value);
            let explicit: Vec<&str> = packages
                .iter()
                .filter(|p| !p.automatic)
                .map(|p| p.name)
                .collect();
            let shown = if explicit.is_empty() {
                packages.iter().map(|p| p.name).collect()
            } else {
                explicit
            };
            let more = packages
                .len()
                .saturating_sub(shown.len().min(SUMMARY_PACKAGES));
            let mut summary = format!(
                "{}: {}",
                key.to_lowercase(),
                shown[..shown.len().min(SUMMARY_PACKAGES)].join(", ")
            );
            if more > 0 {
                summary.push_str(&format!(" (+{more} more)"));
            }
            if let Some(cmd) = &block.commandline {
                summary.push_str(&format!(" ← {cmd}"));
            }

            sink.event(fp_core::Event {
                actor: Some(requested_by(block.requested_by.as_deref())),
                attrs: json!({
                    "action": key,
                    "commandline": block.commandline,
                    "requested_by": block.requested_by,
                    "packages": packages.iter().map(|p| json!({
                        "name": p.name, "version": p.version, "automatic": p.automatic,
                    })).collect::<Vec<_>>(),
                }),
                ..new_event(
                    ctx,
                    inst,
                    stamp,
                    kind,
                    block
                        .commandline
                        .clone()
                        .unwrap_or_else(|| value.to_string()),
                    summary,
                    origin.at(format!("line {line_no}")),
                )
            });
        }
        warn_skipped(sink, inst, &skipped);
        Ok(())
    }
}

fn warn_skipped(sink: &mut Sink, inst: &SourceInstance, lines: &[usize]) {
    if let Some(first) = lines.first() {
        sink.warn(format!(
            "{}: skipped {} unparsable line(s), first at line {first}",
            inst.path.display(),
            lines.len()
        ));
    }
}

struct Package<'a> {
    name: &'a str,
    version: &'a str,
    automatic: bool,
}

/// `zoom:amd64 (7.2.1.5760), libxcb-xtest0:amd64 (1.15-1, automatic)`
fn parse_packages(list: &str) -> Vec<Package<'_>> {
    list.split("), ")
        .filter_map(|item| {
            let (name, rest) = item.split_once(" (")?;
            let rest = rest.trim_end_matches(')');
            let mut fields = rest.split(", ");
            let version = fields.next().unwrap_or_default();
            let automatic = fields.any(|f| f == "automatic");
            Some(Package {
                name: name.trim(),
                version,
                automatic,
            })
        })
        .collect()
}

/// `paya (1000)`
fn requested_by(value: Option<&str>) -> Actor {
    let mut actor = Actor::app("apt");
    if let Some((user, uid)) = value.and_then(|v| v.split_once(" (")) {
        actor.user = Some(user.to_string());
        actor.uid = uid.trim_end_matches(')').parse().ok();
    }
    actor
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> CollectCtx {
        CollectCtx {
            run_id: 1,
            since: None,
            until: None,
            assume_tz: Some("+03:30".into()),
            temp_dir: std::env::temp_dir(),
        }
    }

    fn run(collector: &dyn Collector, name: &str, text: &str) -> Sink {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        std::fs::write(&path, text).unwrap();
        let inst = SourceInstance::new(collector.kind(), "x", None, path);
        let mut sink = Sink::default();
        collector.collect(&inst, &ctx(), &mut sink).unwrap();
        sink
    }

    #[test]
    fn dpkg_actions_and_bad_lines() {
        let sink = run(
            &DpkgCollector,
            "dpkg.log",
            "2026-10-16 21:08:00 startup archives unpack\n\
             2026-10-16 21:08:00 install zoom:amd64 <none> 7.2.1.5760\n\
             garbage line that is long enough to slice\n\
             2026-10-16 21:08:01 status installed zoom:amd64 7.2.1.5760\n\
             2026-10-16 21:09:00 upgrade curl:amd64 8.5.0-2ubuntu10.6 8.5.0-2ubuntu10.7\n\
             2026-10-16 21:10:00 remove foo:amd64 1.0 <none>\n\
             ÿÿÿÿÿÿÿÿÿÿÿÿÿÿÿÿÿÿÿÿÿ\n",
        );
        let got: Vec<_> = sink
            .events
            .iter()
            .map(|e| (e.kind, e.summary.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                (EventKind::PkgInstall, "zoom:amd64 7.2.1.5760"),
                (EventKind::PkgUpgrade, "curl:amd64 8.5.0-2ubuntu10.7"),
                (EventKind::PkgRemove, "remove foo:amd64 1.0"),
            ]
        );
        assert_eq!(sink.events[0].ts_utc, 1_792_172_280_000_000);
        assert_eq!(sink.events[0].provenance.locator, "line 2");
        assert_eq!(sink.warnings.len(), 1);
    }

    #[test]
    fn apt_double_space_and_commandline() {
        let sink = run(
            &AptCollector,
            "history.log",
            "\nStart-Date: 2026-10-16  21:07:58\n\
             Commandline: apt install ./zoom_amd64.deb\n\
             Requested-By: paya (1000)\n\
             Install: zoom:amd64 (7.2.1.5760), libxcb-xtest0:amd64 (1.15-1, automatic)\n\
             End-Date: 2026-10-16  21:08:05\n",
        );
        assert_eq!(sink.events.len(), 1);
        let e = &sink.events[0];
        assert_eq!(e.kind, EventKind::PkgInstall);
        assert_eq!(e.ts_raw, "2026-10-16  21:07:58");
        assert_eq!(e.ts_utc, 1_792_172_278_000_000);
        assert_eq!(e.subject, "apt install ./zoom_amd64.deb");
        assert_eq!(
            e.summary,
            "install: zoom:amd64 (+1 more) ← apt install ./zoom_amd64.deb"
        );
        let actor = e.actor.as_ref().unwrap();
        assert_eq!(
            (actor.user.as_deref(), actor.uid),
            (Some("paya"), Some(1000))
        );
        assert!(sink.warnings.is_empty());
    }

    #[test]
    fn rotations_are_discovered() {
        let root = tempfile::tempdir().unwrap();
        let log = root.path().join("var/log/apt");
        std::fs::create_dir_all(&log).unwrap();
        std::fs::write(log.join("history.log"), "").unwrap();
        std::fs::write(log.join("history.log.1.gz"), "").unwrap();
        let env = Env {
            home: root.path().into(),
            root: root.path().into(),
            uid: 0,
            euid: 0,
        };
        assert_eq!(AptCollector.discover(&env).len(), 2);
        assert!(DpkgCollector.discover(&env).is_empty());
    }
}
