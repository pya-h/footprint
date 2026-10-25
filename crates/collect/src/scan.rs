//! External scanner import (opt-in, `--with-scanners`): clamscan, rkhunter, chkrootkit.
//! Findings become `scan.finding` state records; the tool's own false positives are
//! separated later by `footprint explain`.

use crate::collector::{CollectCtx, Collector, Env, Sink};
use crate::probe::Probe;
use crate::utils::read_capped;
use anyhow::{Context, Result};
use fp_core::{Availability, Provenance, SourceInstance, StateRecord};
use serde_json::json;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// one scanner per tool; the argv is fixed and no shell is involved
const SCANNERS: &[(&str, &str)] = &[
    ("clamscan", "/usr/bin/clamscan"),
    ("rkhunter", "/usr/bin/rkhunter"),
    ("chkrootkit", "/usr/bin/chkrootkit"),
];
/// scanner output above this size is truncated rather than buffered forever
const MAX_SCAN_OUTPUT: u64 = 64 << 20;

pub struct ScannerCollector;

fn run_tool(path: &std::path::Path, args: &[&str]) -> Result<String> {
    let mut child = Command::new(path)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("spawn {}", path.display()))?;
    let mut out = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        // capped: a full-disk scan can stream for a long time
        out = read_capped(stdout.take(MAX_SCAN_OUTPUT + 1))
            .with_context(|| format!("read {}", path.display()))?;
    }
    let _ = child.wait();
    Ok(String::from_utf8_lossy(&out).into_owned())
}

fn emit(tool: &str, finding: String, detail: serde_json::Value, sink: &mut Sink, ctx: &CollectCtx) {
    if finding.trim().is_empty() {
        return;
    }
    sink.state(StateRecord {
        run_id: ctx.run_id,
        kind: "scan.finding".into(),
        key: format!("{tool}:{}", &finding[..finding.len().min(180)]),
        value: json!({
            "tool": tool,
            "finding": finding,
            "flags": Vec::<String>::new(),
            "detail": detail,
        }),
        provenance: Provenance {
            file: PathBuf::from(tool),
            file_sha256: None,
            locator: format!("scan:{tool}"),
        },
    });
}

fn collect_clamscan(
    path: &std::path::Path,
    env: &Env,
    ctx: &CollectCtx,
    sink: &mut Sink,
) -> Result<()> {
    let mut targets: Vec<PathBuf> = env
        .user_targets
        .iter()
        .map(|t| t.home.clone())
        .filter(|h| h.is_dir())
        .collect();
    for sys in ["/tmp", "/var/tmp", "/dev/shm"] {
        let p = env.system_path(sys);
        if p.is_dir() {
            targets.push(p);
        }
    }
    if targets.is_empty() {
        return Ok(());
    }
    let mut args = vec!["--infected", "--no-summary", "-r"];
    for t in &targets {
        args.push(t.to_str().unwrap_or_default());
    }
    let out = run_tool(path, &args)?;
    for line in out.lines() {
        // /path/file: Win.Trojan.Signature FOUND
        let Some((file, rest)) = line.split_once(':') else {
            continue;
        };
        if !rest.contains("FOUND") {
            continue;
        }
        emit(
            "clamscan",
            line.trim().to_string(),
            json!({ "file": file.trim(), "signature": rest.replace("FOUND", "").trim() }),
            sink,
            ctx,
        );
    }
    Ok(())
}

fn collect_rkhunter(path: &std::path::Path, ctx: &CollectCtx, sink: &mut Sink) -> Result<()> {
    let out = run_tool(path, &["--check", "--sk", "--rwo"])?;
    for line in out.lines() {
        let finding = line.trim();
        if finding.is_empty() {
            continue;
        }
        emit(
            "rkhunter",
            finding.to_string(),
            json!({ "warning": finding }),
            sink,
            ctx,
        );
    }
    Ok(())
}

fn collect_chkrootkit(path: &std::path::Path, ctx: &CollectCtx, sink: &mut Sink) -> Result<()> {
    let out = run_tool(path, &["-q"])?;
    for line in out.lines() {
        let finding = line.trim();
        if finding.is_empty() {
            continue;
        }
        emit(
            "chkrootkit",
            finding.to_string(),
            json!({ "line": finding }),
            sink,
            ctx,
        );
    }
    Ok(())
}

impl Collector for ScannerCollector {
    fn kind(&self) -> &'static str {
        "scan.import"
    }

    fn discover(&self, env: &Env) -> Vec<SourceInstance> {
        let mut out = Vec::new();
        for &(tool, default) in SCANNERS {
            let path = Probe::tool_path(env, tool).unwrap_or_else(|| env.system_path(default));
            let mut inst = SourceInstance::new(self.kind(), tool, None, path);
            if !inst.path.is_file() {
                inst.status = Availability::NotInstalled;
            }
            out.push(inst);
        }
        out
    }

    fn collect(&self, inst: &SourceInstance, ctx: &CollectCtx, sink: &mut Sink) -> Result<()> {
        let env = Env::current();
        match inst.app.as_str() {
            "clamscan" => collect_clamscan(&inst.path, &env, ctx, sink),
            "rkhunter" => collect_rkhunter(&inst.path, ctx, sink),
            "chkrootkit" => collect_chkrootkit(&inst.path, ctx, sink),
            other => anyhow::bail!("unknown scanner {other}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::script_tool;

    fn ctx() -> CollectCtx {
        CollectCtx {
            run_id: 1,
            since: None,
            until: None,
            assume_tz: None,
            temp_dir: std::env::temp_dir(),
            own_files: Vec::new(),
        }
    }

    #[test]
    fn imports_clamscan_found_lines() {
        let dir = tempfile::tempdir().unwrap();
        let tool = script_tool(
            dir.path(),
            "clamscan",
            "echo '/home/u/Downloads/evil: Win.Trojan.X FOUND'; echo '/ok: clean'; echo '----------- SCAN SUMMARY -----------'",
        );
        let mut sink = Sink::default();
        let env = Env::test(dir.path().join("home"), PathBuf::from("/"));
        collect_clamscan(&tool, &env, &ctx(), &mut sink).unwrap();
        assert_eq!(sink.states.len(), 1);
        assert_eq!(sink.states[0].kind, "scan.finding");
        assert!(
            sink.states[0].value["finding"]
                .as_str()
                .unwrap()
                .contains("Win.Trojan.X")
        );
    }

    #[test]
    fn imports_rkhunter_warnings() {
        let dir = tempfile::tempdir().unwrap();
        let tool = script_tool(
            dir.path(),
            "rkhunter",
            "echo 'Warning: The following processes are using deleted files:'; echo 'Warning: lwp-request is a script'",
        );
        let mut sink = Sink::default();
        collect_rkhunter(&tool, &ctx(), &mut sink).unwrap();
        assert_eq!(sink.states.len(), 2);
        assert!(
            sink.states[1].value["finding"]
                .as_str()
                .unwrap()
                .contains("lwp-request")
        );
    }

    #[test]
    fn imports_chkrootkit_lines() {
        let dir = tempfile::tempdir().unwrap();
        let tool = script_tool(dir.path(), "chkrootkit", "echo 'basename: INFECTED'");
        let mut sink = Sink::default();
        collect_chkrootkit(&tool, &ctx(), &mut sink).unwrap();
        assert_eq!(sink.states.len(), 1);
        assert!(sink.states[0].key.contains("chkrootkit"));
    }

    #[test]
    fn discover_marks_missing_tools_not_installed() {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::test(dir.path().to_path_buf(), dir.path().to_path_buf());
        let instances = ScannerCollector.discover(&env);
        assert_eq!(instances.len(), 3);
        assert!(
            instances
                .iter()
                .all(|i| i.status == Availability::NotInstalled)
        );
    }
}
