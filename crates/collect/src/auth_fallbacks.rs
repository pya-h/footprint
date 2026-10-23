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
    if let Some(ts_part) = line.get(..15) {
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

/// Login records from `wtmp`/`btmp` (glibc `struct utmp`, 384-byte records, little-endian).
pub struct WtmpCollector;

/// `struct utmp` field offsets on glibc x86_64/aarch64 (verified against `<utmp.h>`).
mod utmp {
    pub const RECORD_SIZE: usize = 384;
    pub const TYPE: usize = 0; // i16
    pub const PID: usize = 4; // i32
    pub const LINE: usize = 8; // [u8; 32]
    pub const USER: usize = 44; // [u8; 32]
    pub const HOST: usize = 76; // [u8; 256]
    pub const TV_SEC32: usize = 340; // i32 (glibc's 32-bit timeval)
    pub const TV_SEC64: usize = 344; // i64 (libcs with a 64-bit timeval)
    pub const USER_PROCESS: i16 = 7;
    pub const LOGIN_PROCESS: i16 = 6;
}

const WTMB_LOGS: &[&str] = &["/var/log/wtmp", "/var/log/btmp"];

fn field_str(rec: &[u8], offset: usize, len: usize) -> String {
    let raw = rec.get(offset..offset + len).unwrap_or_default();
    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    String::from_utf8_lossy(&raw[..end]).into_owned()
}

fn read_i16(rec: &[u8], offset: usize) -> i16 {
    rec.get(offset..offset + 2)
        .map_or(0, |b| i16::from_le_bytes([b[0], b[1]]))
}

fn plausible_tv_sec(sec: i64) -> bool {
    // after 2001-09-09 and not more than a day in the future
    (1_000_000_000..=chrono::Utc::now().timestamp() + 86_400).contains(&sec)
}

/// The record timestamp: glibc stores a 32-bit `timeval`, other libcs a 64-bit one.
fn record_tv_sec(rec: &[u8]) -> Option<i64> {
    let sec32 = rec
        .get(utmp::TV_SEC32..utmp::TV_SEC32 + 4)
        .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as i64);
    if sec32.is_some_and(plausible_tv_sec) {
        return sec32;
    }
    let sec64 = rec
        .get(utmp::TV_SEC64..utmp::TV_SEC64 + 8)
        .and_then(|b| <[u8; 8]>::try_from(b).ok())
        .map(i64::from_le_bytes);
    sec64.filter(|&s| plausible_tv_sec(s))
}

impl Collector for WtmpCollector {
    fn kind(&self) -> &'static str {
        "auth.wtmp"
    }

    fn discover(&self, env: &Env) -> Vec<SourceInstance> {
        let mut out = Vec::new();
        for &log in WTMB_LOGS {
            for path in with_rotations(&env.system_path(log)) {
                out.push(SourceInstance::new(self.kind(), "wtmp", None, path));
            }
        }
        if out.is_empty() {
            let mut inst =
                SourceInstance::new(self.kind(), "wtmp", None, env.system_path("/var/log/wtmp"));
            inst.status = Availability::NotInstalled;
            out.push(inst);
        }
        out
    }

    fn collect(&self, inst: &SourceInstance, ctx: &CollectCtx, sink: &mut Sink) -> Result<()> {
        let file = read_evidence(&inst.path)?;
        if file.bytes.is_empty() {
            return Ok(());
        }
        let origin = Origin {
            file: &inst.path,
            sha256: Some(&file.sha256),
        };
        let failed_logins = inst
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("btmp"));
        let mut skipped = 0usize;

        if file.bytes.len() % utmp::RECORD_SIZE != 0 {
            sink.warn(format!(
                "{}: {} trailing bytes are not a whole utmp record",
                inst.path.display(),
                file.bytes.len() % utmp::RECORD_SIZE
            ));
        }

        for (idx, rec) in file
            .bytes
            .chunks(utmp::RECORD_SIZE)
            .take_while(|c| c.len() == utmp::RECORD_SIZE)
            .enumerate()
        {
            let rec_type = read_i16(rec, utmp::TYPE);
            let wanted = if failed_logins {
                rec_type == utmp::LOGIN_PROCESS
            } else {
                rec_type == utmp::USER_PROCESS
            };
            if !wanted {
                continue;
            }
            let user = field_str(rec, utmp::USER, 32);
            if user.is_empty() {
                continue;
            }
            let Some(tv_sec) = record_tv_sec(rec) else {
                skipped += 1;
                continue;
            };
            let line = field_str(rec, utmp::LINE, 32);
            let host = field_str(rec, utmp::HOST, 256);
            let pid = rec
                .get(utmp::PID..utmp::PID + 4)
                .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as u32);

            if !in_window(ctx, tv_sec * 1_000_000) {
                continue;
            }
            let stamp = Stamp::parse(tv_sec.to_string(), TimeFormat::UnixSeconds, None)?;
            let verb = if failed_logins {
                "failed login"
            } else {
                "login"
            };
            let from = if host.is_empty() {
                String::new()
            } else {
                format!(" from {host}")
            };
            let tty = if line.is_empty() {
                String::new()
            } else {
                format!(" ({line})")
            };

            sink.event(fp_core::Event {
                actor: Some(Actor {
                    user: Some(user.clone()),
                    pid,
                    ..Actor::default()
                }),
                subject_host: host.parse::<std::net::IpAddr>().ok().map(|_| host.clone()),
                attrs: json!({
                    "user": user,
                    "host": host,
                    "line": line,
                    "failed": failed_logins,
                    "utmp_type": rec_type,
                }),
                ..new_event(
                    ctx,
                    inst,
                    stamp,
                    EventKind::AuthLogin,
                    user.clone(),
                    format!("{verb}: {user}{from}{tty}"),
                    origin.at(format!("record {}", idx + 1)),
                )
            });
        }
        if skipped > 0 {
            sink.warn(format!(
                "{}: skipped {skipped} record(s) with implausible timestamps",
                inst.path.display()
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utmp_record(rec_type: i16, user: &str, host: &str, line: &str, tv_sec: i64) -> Vec<u8> {
        let mut rec = vec![0u8; 384];
        rec[0..2].copy_from_slice(&rec_type.to_le_bytes());
        rec[4..8].copy_from_slice(&1234i32.to_le_bytes());
        rec[8..8 + line.len()].copy_from_slice(line.as_bytes());
        rec[44..44 + user.len()].copy_from_slice(user.as_bytes());
        rec[76..76 + host.len()].copy_from_slice(host.as_bytes());
        rec[340..344].copy_from_slice(&(tv_sec as i32).to_le_bytes());
        rec
    }

    fn collect_records(name: &str, bytes: &[u8]) -> Sink {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        let inst = SourceInstance::new("auth.wtmp", "wtmp", None, path);
        let ctx = CollectCtx {
            run_id: 1,
            since: None,
            until: None,
            assume_tz: None,
            temp_dir: dir.path().join("tmp"),
        };
        let mut sink = Sink::default();
        WtmpCollector.collect(&inst, &ctx, &mut sink).unwrap();
        sink
    }

    #[test]
    fn parses_wtmp_logins_and_skips_noise() {
        let mut bytes = utmp_record(7, "paya", "203.0.113.7", "pts/2", 1_792_172_917);
        bytes.extend(utmp_record(8, "paya", "", "pts/2", 1_792_173_200)); // DEAD_PROCESS: logout
        bytes.extend(utmp_record(2, "", "", "", 1_792_172_000)); // BOOT_TIME: no user
        let sink = collect_records("wtmp", &bytes);

        assert_eq!(sink.events.len(), 1);
        let e = &sink.events[0];
        assert_eq!(e.kind, EventKind::AuthLogin);
        assert_eq!(e.subject, "paya");
        assert_eq!(e.summary, "login: paya from 203.0.113.7 (pts/2)");
        assert_eq!(e.subject_host.as_deref(), Some("203.0.113.7"));
        assert_eq!(e.ts_utc, 1_792_172_917_000_000);
        assert_eq!(e.actor.as_ref().unwrap().pid, Some(1234));
    }

    #[test]
    fn parses_btmp_failed_logins() {
        let bytes = utmp_record(6, "root", "198.51.100.9", "ssh:notty", 1_792_172_900);
        let sink = collect_records("btmp", &bytes);

        assert_eq!(sink.events.len(), 1);
        let e = &sink.events[0];
        assert_eq!(e.kind, EventKind::AuthLogin);
        assert_eq!(
            e.summary,
            "failed login: root from 198.51.100.9 (ssh:notty)"
        );
        assert_eq!(e.attrs["failed"], true);
    }

    #[test]
    fn falls_back_to_64bit_timeval_layout() {
        let mut rec = utmp_record(7, "paya", "", "pts/0", 0); // implausible 32-bit stamp
        rec[344..352].copy_from_slice(&1_792_172_917i64.to_le_bytes());
        let sink = collect_records("wtmp", &rec);
        assert_eq!(sink.events.len(), 1);
        assert_eq!(sink.events[0].ts_utc, 1_792_172_917_000_000);
    }

    #[test]
    fn non_ascii_prefix_does_not_panic() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("auth.log");
        // a multibyte char makes byte 15 split a char boundary; slicing must not panic
        std::fs::write(&log, "Øctøber entry without timestamp\n").unwrap();
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
        assert!(sink.events.is_empty());
    }

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
