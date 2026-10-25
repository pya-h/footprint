use crate::collector::{CollectCtx, Collector, Env, Sink};
use crate::event::{Origin, Stamp, new_event};
use crate::redact::redact_secrets;
use crate::utils::in_window;
use anyhow::{Context, Result, bail};
use fp_core::time::TimeFormat;
use fp_core::{Actor, Event, EventKind, SourceInstance};
use serde::Deserialize;
use serde_json::json;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const JOURNALCTL: &str = "/usr/bin/journalctl";
const PERSISTENT_JOURNAL: &str = "/var/log/journal";
const VOLATILE_JOURNAL: &str = "/run/log/journal";
/// sudo, su and pkexec by process name, the ssh daemon (both unit names), and logind sessions.
const MATCHES: &[&str] = &[
    "_COMM=sudo",
    "_COMM=su",
    "_COMM=pkexec",
    "+",
    "_SYSTEMD_UNIT=ssh.service",
    "+",
    "_SYSTEMD_UNIT=sshd.service",
    "+",
    "_COMM=systemd-logind",
];
pub(crate) const SSH_KEPT_PREFIXES: &[&str] = &[
    "Accepted ",
    "Failed ",
    "Invalid user ",
    "Disconnected from user ",
];

use crate::probe::Probe;

pub struct AuthCollector;

impl Collector for AuthCollector {
    fn kind(&self) -> &'static str {
        "auth.journald"
    }

    fn discover(&self, env: &Env) -> Vec<SourceInstance> {
        let persistent = env.system_path(PERSISTENT_JOURNAL);
        let volatile = env.system_path(VOLATILE_JOURNAL);
        let live = env.root == Path::new("/");

        let (path, is_volatile) = if persistent.is_dir() || !live {
            (persistent, false)
        } else if volatile.is_dir() {
            (volatile, true)
        } else {
            (persistent, false)
        };

        let mut inst = SourceInstance::new(self.kind(), "journald", None, path);
        let tool = Probe::tool_path(env, "journalctl");
        if tool.is_none() {
            inst.status = fp_core::Availability::NotInstalled;
            inst.note = Some(format!("{JOURNALCTL} not found"));
            return vec![inst];
        }

        if !inst.path.is_dir() {
            inst.status = fp_core::Availability::NotInstalled;
            inst.note = Some("no journal directory".into());
            return vec![inst];
        }

        let mut notes = Vec::new();
        if is_volatile {
            notes.push("volatile storage (/run/log/journal)".to_string());
        }
        if !Probe::can_read_system_journal(env) {
            notes.push("user entries only (not in adm or systemd-journal group)".to_string());
        }
        if !notes.is_empty() {
            inst.note = Some(notes.join("; "));
        }
        vec![inst]
    }

    fn collect(&self, inst: &SourceInstance, ctx: &CollectCtx, sink: &mut Sink) -> Result<()> {
        let mut cmd = Command::new(JOURNALCTL);
        cmd.args(["--output=json", "--no-pager", "--quiet"]);
        if !inst.path.starts_with(VOLATILE_JOURNAL) && !inst.path.starts_with(PERSISTENT_JOURNAL) {
            cmd.arg("--directory").arg(&inst.path);
        }
        if let Some(since) = ctx.since {
            cmd.arg(format!("--since=@{}", since.div_euclid(1_000_000)));
        }
        if let Some(until) = ctx.until {
            cmd.arg(format!("--until=@{}", until.div_euclid(1_000_000) + 1));
        }
        cmd.args(MATCHES)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd.spawn().with_context(|| format!("spawn {JOURNALCTL}"))?;
        let stdout = child.stdout.take().context("journalctl stdout")?;
        let mut stderr = child.stderr.take().context("journalctl stderr")?;
        let stderr_reader = std::thread::spawn(move || {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text);
            text
        });

        let origin = Origin {
            file: &inst.path,
            sha256: None,
        };
        for line in BufReader::new(stdout).lines() {
            let line = line?;
            let Ok(entry) = serde_json::from_str::<JournalEntry>(&line) else {
                sink.warn("journalctl: unparsable JSON entry");
                continue;
            };
            // journalctl's window is whole seconds; the exact bounds are applied here
            if let Some(event) = entry
                .into_event(ctx, inst, &origin)?
                .filter(|e| in_window(ctx, e.ts_utc))
            {
                sink.event(event);
            }
        }

        let status = child.wait()?;
        let stderr = stderr_reader.join().unwrap_or_default();
        if !status.success() {
            bail!("journalctl exited {status}: {}", stderr.trim());
        }
        // a user outside `adm`/`systemd-journal` only sees their own entries; journalctl says so on stderr
        if !stderr.trim().is_empty() {
            sink.warn(format!("journalctl: {}", stderr.trim()));
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct JournalEntry {
    #[serde(rename = "__REALTIME_TIMESTAMP")]
    realtime: String,
    #[serde(rename = "__CURSOR")]
    cursor: String,
    #[serde(rename = "_COMM")]
    comm: Option<String>,
    #[serde(rename = "_SYSTEMD_UNIT")]
    unit: Option<String>,
    #[serde(rename = "_EXE")]
    exe: Option<String>,
    #[serde(rename = "_PID")]
    pid: Option<String>,
    #[serde(rename = "_UID")]
    uid: Option<String>,
    /// journald stores a non-UTF-8 message as an array of bytes
    #[serde(rename = "MESSAGE", default, deserialize_with = "message_text")]
    message: String,
}

fn message_text<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Message {
        Text(String),
        Bytes(Vec<u8>),
        Other(serde::de::IgnoredAny),
    }
    Ok(match Message::deserialize(d)? {
        Message::Text(s) => s,
        Message::Bytes(b) => String::from_utf8_lossy(&b).into_owned(),
        Message::Other(_) => String::new(),
    })
}

/// A parsed sudo `MESSAGE`: `paya : TTY=/dev/pts/1 ; PWD=/home/paya ; USER=root ; COMMAND=/usr/bin/apt install ./x.deb`
#[derive(Debug, Default, PartialEq)]
pub(crate) struct SudoLine<'a> {
    pub(crate) invoker: &'a str,
    pub(crate) target: Option<&'a str>,
    pub(crate) tty: Option<&'a str>,
    pub(crate) pwd: Option<&'a str>,
    pub(crate) command: Option<&'a str>,
    /// `3 incorrect password attempts`, `user NOT in sudoers`, …
    pub(crate) problem: Option<&'a str>,
}

impl SudoLine<'_> {
    /// `paya: /usr/bin/apt install ./x.deb (as bob) (pts/1) [3 incorrect password attempts]`
    pub(crate) fn summary(&self) -> String {
        let mut summary = format!("{}: {}", self.invoker, self.command.unwrap_or("?"));
        if let Some(target) = self.target.filter(|t| *t != "root") {
            summary.push_str(&format!(" (as {target})"));
        }
        if let Some(tty) = self.tty {
            summary.push_str(&format!(" ({tty})"));
        }
        if let Some(problem) = self.problem {
            summary.push_str(&format!(" [{problem}]"));
        }
        summary
    }
}

pub(crate) fn parse_sudo(message: &str) -> Option<SudoLine<'_>> {
    let (invoker, rest) = message.split_once(" : ")?;
    let (fields, command) = match rest.split_once("COMMAND=") {
        Some((fields, cmd)) => (fields, Some(cmd.trim())),
        None => (rest, None),
    };
    let mut line = SudoLine {
        invoker: invoker.trim(),
        command,
        ..SudoLine::default()
    };
    for field in fields
        .split(" ; ")
        .map(str::trim)
        .filter(|f| !f.is_empty() && *f != ";")
    {
        match field.split_once('=') {
            Some(("TTY", v)) => line.tty = Some(v.strip_prefix("/dev/").unwrap_or(v)),
            Some(("PWD", v)) => line.pwd = Some(v),
            Some(("USER", v)) => line.target = Some(v),
            Some(_) => {}
            None => line.problem = Some(field.trim_end_matches(" ;")),
        }
    }
    Some(line)
}

/// `from 203.0.113.7 port 51234`
pub(crate) fn ssh_peer(message: &str) -> Option<String> {
    let after = message.split_once(" from ")?.1;
    Some(after.split_whitespace().next()?.to_string())
}

/// `New session 3 of user paya.` -> the user; logouts are not events
fn parse_logind_session(message: &str) -> Option<&str> {
    let after = message
        .strip_prefix("New session ")?
        .split_once(" of user ")?
        .1;
    Some(after.trim_end_matches('.').trim())
}

impl JournalEntry {
    fn into_event(
        self,
        ctx: &CollectCtx,
        inst: &SourceInstance,
        origin: &Origin,
    ) -> Result<Option<Event>> {
        let stamp = Stamp::parse(self.realtime.as_str(), TimeFormat::JournaldRealtime, None)?;
        let comm = self.comm.as_deref().unwrap_or_default();
        let message = redact_secrets(&self.message);
        let message = message.as_ref();
        let is_ssh = self
            .unit
            .as_deref()
            .is_some_and(|u| u == "ssh.service" || u == "sshd.service");
        let mut actor = Actor {
            uid: self.uid.as_deref().and_then(|u| u.parse().ok()),
            pid: self.pid.as_deref().and_then(|p| p.parse().ok()),
            exe: self.exe.as_ref().map(PathBuf::from),
            ..Actor::app(comm)
        };
        let mut attrs = json!({ "comm": comm, "unit": self.unit, "message": message });
        let mut subject_host = None;

        let (kind, subject, summary) = if message.starts_with("pam_") {
            (
                EventKind::AuthSession,
                comm.to_string(),
                message.to_string(),
            )
        } else if is_ssh {
            if !SSH_KEPT_PREFIXES.iter().any(|p| message.starts_with(p)) {
                return Ok(None);
            }
            subject_host = ssh_peer(message);
            (
                EventKind::AuthSsh,
                subject_host.clone().unwrap_or_default(),
                message.to_string(),
            )
        } else if comm == "systemd-logind" {
            let Some(user) = parse_logind_session(message) else {
                return Ok(None);
            };
            actor.user = Some(user.to_string());
            (EventKind::AuthLogin, user.to_string(), message.to_string())
        } else if comm == "sudo" {
            let Some(sudo) = parse_sudo(message) else {
                return Ok(None);
            };
            actor.user = Some(sudo.invoker.to_string());
            let summary = sudo.summary();
            attrs = json!({
                "comm": comm,
                "message": message,
                "invoker": sudo.invoker,
                "target_user": sudo.target,
                "tty": sudo.tty,
                "pwd": sudo.pwd,
                "problem": sudo.problem,
            });
            (
                EventKind::PrivSudo,
                sudo.command.unwrap_or_default().to_string(),
                summary,
            )
        } else {
            (
                EventKind::PrivSu,
                comm.to_string(),
                format!("{comm}: {message}"),
            )
        };

        Ok(Some(Event {
            actor: Some(actor),
            subject_host,
            attrs,
            ..new_event(
                ctx,
                inst,
                stamp,
                kind,
                subject,
                summary,
                origin.at(self.cursor),
            )
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sudo_fields() {
        let got = parse_sudo("paya : TTY=/dev/pts/1 ; PWD=/home/paya/Downloads ; USER=root ; COMMAND=/usr/bin/apt install ./zoom_amd64.deb").unwrap();
        assert_eq!(
            got,
            SudoLine {
                invoker: "paya",
                target: Some("root"),
                tty: Some("pts/1"),
                pwd: Some("/home/paya/Downloads"),
                command: Some("/usr/bin/apt install ./zoom_amd64.deb"),
                problem: None,
            }
        );
    }

    #[test]
    fn sudo_failure() {
        let got = parse_sudo("eve : 3 incorrect password attempts ; TTY=pts/0 ; PWD=/tmp ; USER=root ; COMMAND=/bin/sh -c id; whoami").unwrap();
        assert_eq!(got.problem, Some("3 incorrect password attempts"));
        assert_eq!(got.command, Some("/bin/sh -c id; whoami"));
        assert_eq!(got.invoker, "eve");
    }

    fn entry(json: &str) -> Option<Event> {
        let ctx = CollectCtx {
            run_id: 1,
            since: None,
            until: None,
            assume_tz: None,
            temp_dir: "/tmp".into(),
            own_files: Vec::new(),
        };
        let inst =
            SourceInstance::new("auth.journald", "journald", None, "/var/log/journal".into());
        let origin = Origin {
            file: &inst.path,
            sha256: None,
        };
        serde_json::from_str::<JournalEntry>(json)
            .unwrap()
            .into_event(&ctx, &inst, &origin)
            .unwrap()
    }

    #[test]
    fn journald_logind_session() {
        let e = entry(r#"{"__REALTIME_TIMESTAMP":"1","__CURSOR":"c","_COMM":"systemd-logind","MESSAGE":"New session 3 of user paya."}"#)
        .unwrap();
        assert_eq!(e.kind, EventKind::AuthLogin);
        assert_eq!(e.subject, "paya");
        assert!(e.actor.as_ref().unwrap().user.as_deref() == Some("paya"));
        // logouts and unrelated logind chatter are not events
        assert!(
            entry(r#"{"__REALTIME_TIMESTAMP":"1","__CURSOR":"c","_COMM":"systemd-logind","MESSAGE":"Removed session 3."}"#)
            .is_none()
        );
        assert!(
            entry(r#"{"__REALTIME_TIMESTAMP":"1","__CURSOR":"c","_COMM":"systemd-logind","MESSAGE":"Watching system buttons on /dev/input/event0"}"#)
            .is_none()
        );
    }

    #[test]
    fn journald_sudo_entry() {
        let e = entry(r#"{"__REALTIME_TIMESTAMP":"1792172276000000","__CURSOR":"s=abc;i=1","_COMM":"sudo","_UID":"1000","_PID":"4242","MESSAGE":"paya : TTY=/dev/pts/1 ; PWD=/home/paya ; USER=root ; COMMAND=/usr/bin/apt install ./zoom_amd64.deb"}"#).unwrap();
        assert_eq!(e.kind, EventKind::PrivSudo);
        assert_eq!(
            e.summary,
            "paya: /usr/bin/apt install ./zoom_amd64.deb (pts/1)"
        );
        assert_eq!(e.subject, "/usr/bin/apt install ./zoom_amd64.deb");
        assert_eq!(e.actor.as_ref().unwrap().user.as_deref(), Some("paya"));
        assert_eq!(e.provenance.locator, "s=abc;i=1");
    }

    #[test]
    fn journald_pam_is_session_not_sudo() {
        let e = entry(r#"{"__REALTIME_TIMESTAMP":"1","__CURSOR":"c","_COMM":"sudo","MESSAGE":"pam_unix(sudo:session): session opened for user root(uid=0) by paya(uid=1000)"}"#).unwrap();
        assert_eq!(e.kind, EventKind::AuthSession);
    }

    #[test]
    fn journald_ssh_keeps_logins_only() {
        let e = entry(r#"{"__REALTIME_TIMESTAMP":"1","__CURSOR":"c","_COMM":"sshd","_SYSTEMD_UNIT":"sshd.service","MESSAGE":"Accepted publickey for paya from 203.0.113.7 port 51234 ssh2"}"#).unwrap();
        assert_eq!(
            (e.kind, e.subject_host.as_deref()),
            (EventKind::AuthSsh, Some("203.0.113.7"))
        );
        assert!(entry(r#"{"__REALTIME_TIMESTAMP":"1","__CURSOR":"c","_COMM":"sshd","_SYSTEMD_UNIT":"ssh.service","MESSAGE":"Received disconnect from 203.0.113.7"}"#).is_none());
    }

    #[test]
    fn journald_binary_message() {
        let e = entry(
            r#"{"__REALTIME_TIMESTAMP":"1","__CURSOR":"c","_COMM":"su","MESSAGE":[40,116,111,41]}"#,
        )
        .unwrap();
        assert_eq!(e.summary, "su: (to)");
    }
}
