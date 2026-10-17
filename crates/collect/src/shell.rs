use crate::collector::{CollectCtx, Collector, Env, Sink};
use crate::event::{Origin, Stamp, new_event};
use crate::redact::redact_secrets;
use crate::utils::{in_window, read_evidence};
use anyhow::Result;
use fp_core::time::TimeFormat;
use fp_core::{Actor, EventKind, SourceInstance};
use regex::Regex;
use serde_json::json;
use std::borrow::Cow;
use std::sync::LazyLock;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Format {
    Bash,
    Zsh,
    Fish,
    /// one entry per line, oldest first (python, psql, sqlite3); libedit's `_HiStOrY_V2_` variant is detected
    Lines,
    /// one entry per line, newest first
    LinesNewestFirst,
}

/// (app, path under `$HOME`, format)
const HISTORIES: &[(&str, &str, Format)] = &[
    ("bash", ".bash_history", Format::Bash),
    ("zsh", ".zsh_history", Format::Zsh),
    ("zsh", ".histfile", Format::Zsh),
    ("fish", ".local/share/fish/fish_history", Format::Fish),
    ("python", ".python_history", Format::Lines),
    ("psql", ".psql_history", Format::Lines),
    ("mysql", ".mysql_history", Format::Lines),
    ("sqlite3", ".sqlite_history", Format::Lines),
    ("node", ".node_repl_history", Format::LinesNewestFirst),
];

/// zsh writes bytes `0x83..=0x9f` and NUL as `0x83` followed by the byte XOR `0x20`.
const ZSH_META: u8 = 0x83;
const LIBEDIT_HEADER: &str = "_HiStOrY_V2_";

static DANGEROUS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        (
            r"\b(curl|wget)\b[^|]*\|\s*(sudo\s+)?(ba|z|da)?sh\b",
            "pipe-to-shell",
        ),
        (r"\bbase64\s+(-d|--decode)\b", "base64-decode"),
        (r"(^|[;&|\s])eval\s", "eval"),
        (r"/dev/(tcp|udp)/", "dev-tcp"),
        (r"\bnohup\b.*&\s*$", "nohup-background"),
        (r"\bnpm\s+(i|install|ci)\b", "npm-install"),
        (r"\bchmod\s+\+?[0-7]*x", "chmod-exec"),
        (r"\b(python3?|node|perl)\s+-(c|e)\s", "inline-interpreter"),
    ]
    .into_iter()
    .map(|(pattern, flag)| (Regex::new(pattern).expect("static pattern compiles"), flag))
    .collect()
});

use fp_core::Availability;
use std::path::PathBuf;

pub struct ShellCollector;

impl Collector for ShellCollector {
    fn kind(&self) -> &'static str {
        "shell.history"
    }

    fn discover(&self, env: &Env) -> Vec<SourceInstance> {
        let mut candidates: Vec<(&str, PathBuf, Format)> = Vec::new();

        // 1. Check $HISTFILE if set
        if let Some(histfile) = std::env::var_os("HISTFILE").map(PathBuf::from)
            && histfile.is_file()
        {
            candidates.push(("bash", histfile, Format::Bash));
        }

        // 2. Check $ZDOTDIR if set
        if let Some(zdotdir) = std::env::var_os("ZDOTDIR").map(PathBuf::from) {
            let zsh_hist = zdotdir.join(".zsh_history");
            if zsh_hist.is_file() {
                candidates.push(("zsh", zsh_hist, Format::Zsh));
            }
        }

        // 3. Default known histories
        for &(app, rel, format) in HISTORIES {
            let path = env.home.join(rel);
            if !candidates.iter().any(|(_, p, _)| p == &path) && path.is_file() {
                candidates.push((app, path, format));
            }
        }

        candidates
            .into_iter()
            .map(|(app, path, _)| {
                let mut inst = SourceInstance::new(self.kind(), app, None, path);
                if let Ok(meta) = std::fs::metadata(&inst.path)
                    && meta.len() == 0
                {
                    inst.status = Availability::Empty;
                }
                inst
            })
            .collect()
    }

    fn collect(&self, inst: &SourceInstance, ctx: &CollectCtx, sink: &mut Sink) -> Result<()> {
        let format = match inst.app.as_str() {
            "bash" => Format::Bash,
            "zsh" => Format::Zsh,
            "fish" => Format::Fish,
            "node" => Format::LinesNewestFirst,
            _ => Format::Lines,
        };
        let file = read_evidence(&inst.path)?;
        let entries = match format {
            Format::Bash => parse_bash(&String::from_utf8_lossy(&file.bytes)),
            Format::Zsh => parse_zsh(&String::from_utf8_lossy(&unmetafy(&file.bytes))),
            Format::Fish => parse_fish(&String::from_utf8_lossy(&file.bytes)),
            Format::Lines => parse_lines(&String::from_utf8_lossy(&file.bytes), false),
            Format::LinesNewestFirst => parse_lines(&String::from_utf8_lossy(&file.bytes), true),
        };

        let origin = Origin {
            file: &inst.path,
            sha256: Some(&file.sha256),
        };
        for entry in entries {
            let stamp = match entry.when {
                Some(epoch) => Stamp::parse(epoch.to_string(), entry.format, None)?,
                None => Stamp::untimed(),
            };
            if stamp.is_timed() && !in_window(ctx, stamp.ts_utc) {
                continue;
            }
            let command = redact_secrets(&entry.command);
            let redacted = matches!(command, Cow::Owned(_));
            let flags: Vec<&str> = DANGEROUS
                .iter()
                .filter(|(re, _)| re.is_match(&command))
                .map(|(_, flag)| *flag)
                .collect();
            // timed entries are keyed by time; line numbers shift when the shell truncates its history
            let locator = match entry.when {
                Some(epoch) => format!("when={epoch}"),
                None => format!("line {}", entry.line),
            };
            sink.event(fp_core::Event {
                actor: Some(Actor::app(inst.app.as_str())),
                attrs: json!({
                    "line": entry.line,
                    "order": entry.order,
                    "flags": flags,
                    "redacted": redacted,
                    "paths": entry.paths,
                }),
                ..new_event(
                    ctx,
                    inst,
                    stamp,
                    EventKind::ShellCommand,
                    command.to_string(),
                    command.into_owned(),
                    origin.at(locator),
                )
            });
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
struct Entry {
    command: String,
    when: Option<i64>,
    format: TimeFormat,
    /// 1-based line where the entry starts
    line: usize,
    /// position in chronological order, for entries without a time
    order: usize,
    paths: Vec<String>,
}

impl Entry {
    fn new(command: String, when: Option<i64>, format: TimeFormat, line: usize) -> Self {
        Self {
            command,
            when,
            format,
            line,
            order: 0,
            paths: Vec::new(),
        }
    }
}

fn numbered(mut entries: Vec<Entry>) -> Vec<Entry> {
    entries.retain(|e| !e.command.trim().is_empty());
    for (i, e) in entries.iter_mut().enumerate() {
        e.order = i;
    }
    entries
}

/// `#1792172917` marks the time of the entry that follows; an entry may span lines until the next mark.
fn parse_bash(text: &str) -> Vec<Entry> {
    let epoch = |line: &str| {
        line.strip_prefix('#')
            .filter(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|d| d.parse::<i64>().ok())
    };
    let timed = text.lines().any(|l| epoch(l).is_some());
    let mut entries: Vec<Entry> = Vec::new();
    let mut pending: Option<i64> = None;
    for (idx, line) in text.lines().enumerate() {
        if let Some(ts) = epoch(line) {
            pending = Some(ts);
            continue;
        }
        match (timed, pending.take(), entries.last_mut()) {
            (true, None, Some(last)) if last.when.is_some() => {
                last.command.push('\n');
                last.command.push_str(line);
            }
            (_, when, _) => entries.push(Entry::new(
                line.to_string(),
                when,
                TimeFormat::BashHashEpoch,
                idx + 1,
            )),
        }
    }
    numbered(entries)
}

fn unmetafy(bytes: &[u8]) -> Cow<'_, [u8]> {
    if !bytes.contains(&ZSH_META) {
        return Cow::Borrowed(bytes);
    }
    let mut out = Vec::with_capacity(bytes.len());
    let mut iter = bytes.iter();
    while let Some(&b) = iter.next() {
        match (b, iter.as_slice().first()) {
            (ZSH_META, Some(&next)) => {
                out.push(next ^ 0x20);
                iter.next();
            }
            _ => out.push(b),
        }
    }
    Cow::Owned(out)
}

/// `: 1792172917:0;command`, a trailing `\` continues the command on the next line.
fn parse_zsh(text: &str) -> Vec<Entry> {
    let mut entries: Vec<Entry> = Vec::new();
    let mut continues = false;
    for (idx, line) in text.lines().enumerate() {
        if continues && let Some(last) = entries.last_mut() {
            last.command.push('\n');
            last.command.push_str(line);
        } else {
            entries.push(match zsh_extended(line) {
                Some((when, cmd)) => Entry::new(
                    cmd.to_string(),
                    Some(when),
                    TimeFormat::UnixSeconds,
                    idx + 1,
                ),
                None => Entry::new(line.to_string(), None, TimeFormat::None, idx + 1),
            });
        }
        continues = line.ends_with('\\');
    }
    numbered(entries)
}

fn zsh_extended(line: &str) -> Option<(i64, &str)> {
    let rest = line.strip_prefix(": ")?;
    let (stamp, cmd) = rest.split_once(';')?;
    let (when, _duration) = stamp.split_once(':')?;
    Some((when.parse().ok()?, cmd))
}

/// Entries start at `- cmd: `; `  when: ` and an optional `  paths:` list follow. No blank lines between entries.
fn parse_fish(text: &str) -> Vec<Entry> {
    let mut entries: Vec<Entry> = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        if let Some(cmd) = line.strip_prefix("- cmd: ") {
            entries.push(Entry::new(
                fish_unescape(cmd),
                None,
                TimeFormat::UnixSeconds,
                idx + 1,
            ));
        } else if let Some(last) = entries.last_mut() {
            if let Some(when) = line.strip_prefix("  when: ") {
                last.when = when.trim().parse().ok();
            } else if let Some(path) = line.strip_prefix("    - ") {
                last.paths.push(fish_unescape(path));
            }
        }
    }
    numbered(entries)
}

/// fish stores a newline as `\n` and a backslash as `\\`.
fn fish_unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match (c, chars.clone().next()) {
            ('\\', Some('n')) => {
                out.push('\n');
                chars.next();
            }
            ('\\', Some('\\')) => {
                out.push('\\');
                chars.next();
            }
            _ => out.push(c),
        }
    }
    out
}

fn parse_lines(text: &str, newest_first: bool) -> Vec<Entry> {
    let libedit = text.lines().next() == Some(LIBEDIT_HEADER);
    let mut entries: Vec<Entry> = text
        .lines()
        .enumerate()
        .skip(usize::from(libedit))
        .map(|(idx, line)| {
            let command = if libedit {
                libedit_unescape(line)
            } else {
                line.to_string()
            };
            Entry::new(command, None, TimeFormat::None, idx + 1)
        })
        .collect();
    if newest_first {
        entries.reverse();
    }
    numbered(entries)
}

/// libedit escapes bytes as three-digit octal: `\040` is a space.
fn libedit_unescape(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let octal = bytes
            .get(i + 1..i + 4)
            .filter(|d| d.iter().all(|b| (b'0'..=b'7').contains(b)))
            .and_then(|d| u8::from_str_radix(std::str::from_utf8(d).ok()?, 8).ok());
        match (bytes[i], octal) {
            (b'\\', Some(b)) => {
                out.push(b);
                i += 4;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commands(entries: &[Entry]) -> Vec<(&str, Option<i64>)> {
        entries
            .iter()
            .map(|e| (e.command.as_str(), e.when))
            .collect()
    }

    #[test]
    fn bash_timed_multiline_and_hash_commands() {
        let got = parse_bash(
            "#1792172870\ncd ~/Downloads\n#1792172876\nfor f in *.deb; do\n  echo $f\ndone\n#1792172880\n# just a comment\n",
        );
        assert_eq!(
            commands(&got),
            [
                ("cd ~/Downloads", Some(1792172870)),
                ("for f in *.deb; do\n  echo $f\ndone", Some(1792172876)),
                ("# just a comment", Some(1792172880)),
            ]
        );
        assert_eq!(got[1].line, 4);
    }

    #[test]
    fn bash_untimed_is_one_line_per_entry() {
        let got = parse_bash("ls\n\nsudo apt install ./zoom_amd64.deb\n#not-a-time\n");
        assert_eq!(
            commands(&got),
            [
                ("ls", None),
                ("sudo apt install ./zoom_amd64.deb", None),
                ("#not-a-time", None)
            ]
        );
        assert_eq!(got.iter().map(|e| e.order).collect::<Vec<_>>(), [0, 1, 2]);
    }

    #[test]
    fn zsh_metafied_and_continued() {
        let mut bytes = b": 1792172870:0;echo caf".to_vec();
        // 'é' is c3 a9; zsh metafies neither, but a 0x83 byte would be written as 83 a3
        bytes.extend([0xc3, 0xa9]);
        bytes.extend(b"\n: 1792172871:2;printf '");
        bytes.extend([ZSH_META, 0x83 ^ 0x20]);
        bytes.extend(b"'\n: 1792172872:0;ls \\\n-la\nplain\n");
        let got = parse_zsh(&String::from_utf8_lossy(&unmetafy(&bytes)));
        assert_eq!(got[0].command, "echo café");
        assert_eq!(got[1].command.as_bytes(), b"printf '\xef\xbf\xbd'");
        assert_eq!(
            commands(&got[2..]),
            [("ls \\\n-la", Some(1792172872)), ("plain", None)]
        );
    }

    #[test]
    fn fish_without_blank_lines() {
        let got = parse_fish(
            "- cmd: cd ~/Downloads\n  when: 1792172870\n- cmd: sudo apt install ./zoom_amd64.deb\n  when: 1792172876\n  paths:\n    - ./zoom_amd64.deb\n- cmd: echo a\\\\b\\nc\n  when: 1792172880\n",
        );
        assert_eq!(
            commands(&got),
            [
                ("cd ~/Downloads", Some(1792172870)),
                ("sudo apt install ./zoom_amd64.deb", Some(1792172876)),
                ("echo a\\b\nc", Some(1792172880)),
            ]
        );
        assert_eq!(got[1].paths, ["./zoom_amd64.deb"]);
    }

    #[test]
    fn repl_formats() {
        let mysql = parse_lines("_HiStOrY_V2_\nSELECT\\040*\\040FROM\\040users;\n", false);
        assert_eq!(commands(&mysql), [("SELECT * FROM users;", None)]);
        assert_eq!(mysql[0].line, 2);
        let node = parse_lines("newest\nolder\noldest\n", true);
        assert_eq!(
            node.iter().map(|e| e.command.as_str()).collect::<Vec<_>>(),
            ["oldest", "older", "newest"]
        );
    }

    #[test]
    fn flags_dangerous_patterns() {
        let flagged = |cmd: &str| -> Vec<&str> {
            DANGEROUS
                .iter()
                .filter(|(re, _)| re.is_match(cmd))
                .map(|(_, f)| *f)
                .collect()
        };
        assert_eq!(flagged("wget -qO- https://x.sh | sh"), ["pipe-to-shell"]);
        assert_eq!(
            flagged("curl -fsSL https://x | sudo bash"),
            ["pipe-to-shell"]
        );
        assert_eq!(flagged("echo aGk= | base64 -d"), ["base64-decode"]);
        assert!(flagged("sudo apt install ./zoom_amd64.deb").is_empty());
        assert!(flagged("evaluate --shell").is_empty());
    }

    #[test]
    fn discover_custom_histfile() {
        let dir = tempfile::tempdir().unwrap();
        let custom = dir.path().join("custom_hist");
        std::fs::write(&custom, "echo test\n").unwrap();
        unsafe {
            std::env::set_var("HISTFILE", &custom);
        }
        let env = Env::test(dir.path().to_path_buf(), PathBuf::from("/"));
        let instances = ShellCollector.discover(&env);
        unsafe {
            std::env::remove_var("HISTFILE");
        }
        assert!(
            instances
                .iter()
                .any(|i| i.path == custom && i.app == "bash")
        );
    }
}
