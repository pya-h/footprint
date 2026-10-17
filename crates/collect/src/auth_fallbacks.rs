//! Privilege and login fallbacks: syslog auth logs (/var/log/auth.log* and /var/log/secure*).

use crate::collector::{CollectCtx, Collector, Env, Sink};
use crate::event::{Origin, Stamp, new_event};
use crate::redact::redact_secrets;
use crate::utils::{in_window, read_evidence, with_rotations};
use anyhow::Result;
use chrono::{Datelike, Local};
use fp_core::time::TimeFormat;
use fp_core::{Actor, Availability, EventKind, SourceInstance};
use serde_json::json;

const AUTH_LOGS: &[&str] = &["/var/log/auth.log", "/var/log/secure"];

pub struct AuthLogCollector;

impl Collector for AuthLogCollector {
    fn kind(&self) -> &'static str {
        "auth.log"
    }

    fn discover(&self, env: &Env) -> Vec<SourceInstance> {
        let mut out = Vec::new();
        let mut any_found = false;

        for &log in AUTH_LOGS {
            let path = env.system_path(log);
            let files = with_rotations(&path);
            if !files.is_empty() {
                any_found = true;
                for p in files {
                    out.push(SourceInstance::new(self.kind(), "authlog", None, p));
                }
            }
        }

        if !any_found {
            let mut inst = SourceInstance::new(
                self.kind(),
                "authlog",
                None,
                env.system_path("/var/log/auth.log"),
            );
            inst.status = Availability::NotInstalled;
            return vec![inst];
        }
        out
    }

    fn collect(&self, inst: &SourceInstance, ctx: &CollectCtx, sink: &mut Sink) -> Result<()> {
        let file = read_evidence(&inst.path)?;
        let origin = Origin {
            file: &inst.path,
            sha256: Some(&file.sha256),
        };
        let text = String::from_utf8_lossy(&file.bytes);
        let year = Local::now().year();

        for (idx, line) in text.lines().enumerate() {
            let line_no = idx + 1;
            let (raw_ts, after_ts) = match parse_log_timestamp(line, year) {
                Some(pair) => pair,
                None => continue,
            };

            let Ok(stamp) = Stamp::parse(
                raw_ts.clone(),
                TimeFormat::LocalNaive("%Y-%m-%d %H:%M:%S".into()),
                ctx.assume_tz.as_deref(),
            )
            .or_else(|_| Stamp::parse(raw_ts, TimeFormat::Rfc3339, None)) else {
                continue;
            };

            if !in_window(ctx, stamp.ts_utc) {
                continue;
            }

            // check for sudo or sshd
            if let Some(sudo_msg) = after_ts.find("sudo:").map(|i| after_ts[i + 5..].trim()) {
                if sudo_msg.contains("pam_unix(sudo:session):") {
                    continue; // session noise
                }
                parse_sudo_line(ctx, inst, &origin, line_no, stamp, sudo_msg, sink);
            } else if let Some(msg) = after_ts
                .find("sshd[")
                .and_then(|i| after_ts[i + 5..].split_once("]: ").map(|(_, msg)| msg))
            {
                parse_ssh_line(ctx, inst, &origin, line_no, stamp, msg.trim(), sink);
            }
        }
        Ok(())
    }
}

fn parse_log_timestamp(line: &str, current_year: i32) -> Option<(String, &str)> {
    // 1. ISO format: 2026-10-16T21:07:56.123456+03:30 ...
    if line.len() > 19 && line.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        let (ts, rest) = line.split_once(' ')?;
        return Some((ts.to_string(), rest));
    }
    // 2. Syslog format: Oct 16 21:07:56 ...
    if line.len() >= 15 {
        let ts_part = &line[..15];
        let rest = &line[15..].trim_start();
        // convert "Oct 16 21:07:56" with year
        let dt = chrono::NaiveDateTime::parse_from_str(
            &format!("{current_year} {ts_part}"),
            "%Y %b %e %H:%M:%S",
        )
        .ok()?;
        return Some((dt.format("%Y-%m-%d %H:%M:%S").to_string(), rest));
    }
    None
}

fn parse_sudo_line(
    ctx: &CollectCtx,
    inst: &SourceInstance,
    origin: &Origin,
    line_no: usize,
    stamp: Stamp,
    msg: &str,
    sink: &mut Sink,
) {
    // format: invoker : TTY=pts/1 ; PWD=... ; USER=root ; COMMAND=/usr/bin/apt ...
    let Some((invoker, rest)) = msg.split_once(':') else {
        return;
    };
    let invoker = invoker.trim();
    let mut target_user = "root";
    let mut command = "";
    let mut pwd = "";
    let mut tty = "";

    for part in rest.split(';') {
        let part = part.trim();
        if let Some(u) = part.strip_prefix("USER=") {
            target_user = u;
        } else if let Some(cmd) = part.strip_prefix("COMMAND=") {
            command = cmd;
        } else if let Some(p) = part.strip_prefix("PWD=") {
            pwd = p;
        } else if let Some(t) = part.strip_prefix("TTY=") {
            tty = t;
        }
    }

    if command.is_empty() {
        return;
    }

    let redacted_cmd = redact_secrets(command);
    let summary = format!("{invoker}: {redacted_cmd} ({tty})");

    sink.event(fp_core::Event {
        actor: Some(Actor {
            user: Some(invoker.to_string()),
            ..Actor::default()
        }),
        attrs: json!({
            "invoker": invoker,
            "target_user": target_user,
            "command": redacted_cmd,
            "pwd": pwd,
            "tty": tty,
        }),
        ..new_event(
            ctx,
            inst,
            stamp,
            EventKind::PrivSudo,
            redacted_cmd.into_owned(),
            summary,
            origin.at(format!("line {line_no}")),
        )
    });
}

fn parse_ssh_line(
    ctx: &CollectCtx,
    inst: &SourceInstance,
    origin: &Origin,
    line_no: usize,
    stamp: Stamp,
    msg: &str,
    sink: &mut Sink,
) {
    if !msg.starts_with("Accepted ") && !msg.starts_with("Failed ") {
        return;
    }
    let is_accepted = msg.starts_with("Accepted ");
    let summary = msg.to_string();

    sink.event(fp_core::Event {
        actor: Some(Actor::app("sshd")),
        attrs: json!({
            "accepted": is_accepted,
            "message": msg,
        }),
        ..new_event(
            ctx,
            inst,
            stamp,
            EventKind::AuthSsh,
            msg.to_string(),
            summary,
            origin.at(format!("line {line_no}")),
        )
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_auth_log_sudo_and_ssh() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("auth.log");
        std::fs::write(
            &log,
            "2026-10-16T21:07:56.000000+03:30 host sudo: paya : TTY=pts/1 ; PWD=/home/paya ; USER=root ; COMMAND=/usr/bin/apt install ./zoom_amd64.deb\n\
             2026-10-16T21:05:00.000000+03:30 host sshd[1234]: Accepted publickey for paya from 192.168.1.1 port 22 ssh2\n",
        )
        .unwrap();

        let inst = SourceInstance::new("auth.log", "authlog", None, log);
        let ctx = CollectCtx {
            run_id: 1,
            since: None,
            until: None,
            assume_tz: None,
            temp_dir: dir.path().join("tmp"),
        };
        let mut sink = Sink::default();
        AuthLogCollector.collect(&inst, &ctx, &mut sink).unwrap();

        assert_eq!(sink.events.len(), 2);
        assert_eq!(sink.events[0].kind, EventKind::PrivSudo);
        assert_eq!(
            sink.events[0].subject,
            "/usr/bin/apt install ./zoom_amd64.deb"
        );
        assert_eq!(sink.events[1].kind, EventKind::AuthSsh);
    }
}
