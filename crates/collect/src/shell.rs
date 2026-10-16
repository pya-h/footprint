use crate::collector::{Collector, CollectCtx, Env, Sink};
use crate::utils::in_window;
use anyhow::Result;
use fp_core::{Actor, Confidence, Event, EventKind, Provenance, SourceInstance, SourceStatus, time::TimeFormat};
use serde_json::json;
use std::fs;

const HISTORIES: &[(&str, &str, ShellFormat)] = &[
    ("bash", ".bash_history", ShellFormat::BashHashEpoch),
    ("zsh", ".zsh_history", ShellFormat::ZshExtended),
    ("fish", ".local/share/fish/fish_history", ShellFormat::FishYaml),
];

#[derive(Clone, Copy)]
enum ShellFormat {
    BashHashEpoch,
    ZshExtended,
    FishYaml,
}

pub struct ShellCollector;

impl ShellCollector {
    pub fn new() -> Self {
        Self
    }
}

impl Collector for ShellCollector {
    fn kind(&self) -> &'static str {
        "shell.history"
    }

    fn discover(&self, env: &Env) -> Vec<SourceInstance> {
        let mut id = 1u64;
        let mut out = Vec::new();
        for (shell, rel, _fmt) in HISTORIES {
            let path = env.home.join(rel);
            if path.exists() {
                out.push(SourceInstance {
                    id,
                    kind: self.kind().to_string(),
                    app: shell.to_string(),
                    profile: None,
                    path,
                    status: SourceStatus::Ok,
                    note: None,
                });
                id += 1;
            }
        }
        out
    }

    fn collect(
        &self,
        inst: &SourceInstance,
        ctx: &CollectCtx,
        sink: &Sink,
    ) -> Result<fp_core::CollectStats> {
        let text = fs::read_to_string(&inst.path)?;
        let format = match inst.app.as_str() {
            "bash" => ShellFormat::BashHashEpoch,
            "zsh" => ShellFormat::ZshExtended,
            "fish" => ShellFormat::FishYaml,
            _ => return Ok(fp_core::CollectStats::empty()),
        };

        match format {
            ShellFormat::BashHashEpoch => collect_bash(&text, inst, ctx, sink),
            ShellFormat::ZshExtended => collect_zsh(&text, inst, ctx, sink),
            ShellFormat::FishYaml => collect_fish(&text, inst, ctx, sink),
        }
    }
}

fn collect_bash(
    text: &str,
    inst: &SourceInstance,
    ctx: &CollectCtx,
    sink: &Sink,
) -> Result<fp_core::CollectStats> {
    let format = TimeFormat::BashHashEpoch;
    let mut pending_ts: Option<i64> = None;
    let mut line_no = 0;

    for line in text.lines() {
        line_no += 1;
        if let Some(ts_raw) = line.strip_prefix('#') {
            if let Ok(norm) = fp_core::time::normalize(ts_raw, &format, None) {
                pending_ts = Some(norm.ts_utc);
            }
            continue;
        }
        if line.is_empty() {
            continue;
        }

        let (ts_utc, precision, ts_raw, ts_format, tz_assumed) = match pending_ts {
            Some(ts) => (ts, fp_core::TimePrecision::Second, ts_raw_for(ts), TimeFormat::BashHashEpoch, None),
            None => (0, fp_core::TimePrecision::OrderOnly, line.to_string(), TimeFormat::None, None),
        };

        if precision != fp_core::TimePrecision::OrderOnly && !in_window(ctx, ts_utc) {
            continue;
        }

        let confidence = if precision == fp_core::TimePrecision::OrderOnly {
            Confidence::Derived
        } else {
            Confidence::Direct
        };

        push_shell_event(sink, ctx, inst, ts_utc, precision, ts_raw, ts_format, tz_assumed, line, line_no, confidence);
        pending_ts = None;
    }
    Ok(fp_core::CollectStats::empty())
}

fn collect_zsh(
    text: &str,
    inst: &SourceInstance,
    ctx: &CollectCtx,
    sink: &Sink,
) -> Result<fp_core::CollectStats> {
    let format = TimeFormat::ZshExtended;
    for (line_no, line) in text.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        let Ok(norm) = fp_core::time::normalize(line, &format, None) else {
            push_shell_event(sink, ctx, inst, 0, fp_core::TimePrecision::OrderOnly, line.to_string(), TimeFormat::None, None, line, line_no + 1, Confidence::Derived);
            continue;
        };
        if !in_window(ctx, norm.ts_utc) {
            continue;
        }
        let cmd = line.split_once(';').map(|(_, c)| c).unwrap_or(line);
        push_shell_event(sink, ctx, inst, norm.ts_utc, norm.precision, line.to_string(), TimeFormat::ZshExtended, norm.tz_assumed, cmd, line_no + 1, Confidence::Direct);
    }
    Ok(fp_core::CollectStats::empty())
}

fn collect_fish(
    text: &str,
    inst: &SourceInstance,
    ctx: &CollectCtx,
    sink: &Sink,
) -> Result<fp_core::CollectStats> {
    let format = TimeFormat::UnixSeconds;
    let mut cmd: Option<String> = None;
    let mut when: Option<i64> = None;
    let mut line_no = 0;

    for line in text.lines() {
        line_no += 1;
        if line.starts_with("- cmd: ") {
            cmd = Some(line.strip_prefix("- cmd: ").unwrap_or("").to_string());
        } else if line.starts_with("  when: ") {
            when = line.strip_prefix("  when: ").and_then(|s| s.parse().ok());
        } else if line.trim().is_empty() {
            if let (Some(c), Some(w)) = (cmd.take(), when.take()) {
                let norm = fp_core::time::normalize(&w.to_string(), &format, None)?;
                if !in_window(ctx, norm.ts_utc) {
                    continue;
                }
                push_shell_event(sink, ctx, inst, norm.ts_utc, norm.precision, w.to_string(), TimeFormat::UnixSeconds, None, &c, line_no, Confidence::Direct);
            }
        }
    }
    Ok(fp_core::CollectStats::empty())
}

fn push_shell_event(
    sink: &Sink,
    ctx: &CollectCtx,
    inst: &SourceInstance,
    ts_utc: i64,
    precision: fp_core::TimePrecision,
    ts_raw: String,
    ts_format: TimeFormat,
    tz_assumed: Option<String>,
    command: &str,
    line_no: usize,
    confidence: Confidence,
) {
    let flags = dangerous_flags(command);
    sink.push_event(Event {
        run_id: ctx.run_id,
        ts_utc,
        ts_precision: precision,
        ts_raw,
        ts_raw_format: ts_format,
        ts_tz_assumed: tz_assumed,
        source_kind: inst.kind.clone(),
        source_inst: inst.id,
        kind: EventKind::ShellCommand,
        actor: Some(Actor {
            uid: None,
            user: None,
            pid: None,
            exe: None,
            app: Some(inst.app.clone()),
        }),
        subject: command.to_string(),
        subject_host: None,
        summary: command.to_string(),
        attrs: json!({
            "line": line_no,
            "dangerous": !flags.is_empty(),
            "flags": flags,
        }),
        provenance: Provenance {
            file: inst.path.clone(),
            file_sha256: None,
            locator: format!("{}:line {}", inst.path.display(), line_no),
        },
        confidence,
    });
}

fn dangerous_flags(cmd: &str) -> Vec<&'static str> {
    let mut flags = Vec::new();
    if cmd.contains("curl ") && cmd.contains("| sh") { flags.push("curl-pipe-sh"); }
    if cmd.contains("wget ") && cmd.contains("| sh") { flags.push("wget-pipe-sh"); }
    if cmd.contains("base64 -d") || cmd.contains("base64 --decode") { flags.push("base64-decode"); }
    if cmd.contains("eval ") { flags.push("eval"); }
    if cmd.contains("/dev/tcp/") { flags.push("dev-tcp"); }
    if cmd.contains("nohup ") && cmd.contains("&") { flags.push("nohup-background"); }
    if cmd.contains("npm install") { flags.push("npm-install"); }
    flags
}

fn ts_raw_for(ts: i64) -> String {
    (ts / 1_000_000).to_string()
}
