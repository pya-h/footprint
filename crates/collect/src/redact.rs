//! Shell and REPL history hold the occasional password typed inline (`ALTER USER … PASSWORD '…'`,
//! `PGPASSWORD=… psql`, `https://user:pass@host`). They are masked before anything is stored.

use regex::Regex;
use std::borrow::Cow;
use std::sync::LazyLock;

static RULES: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        // NAME=value where NAME mentions a secret: env assignments, `password=`, `--api-key=`
        (
            r#"(?i)\b([a-z0-9_-]*(?:password|passwd|secret|token|api[_-]?key)[a-z0-9_-]*)=('[^']*'|"[^"]*"|\S+)"#,
            "${1}=***",
        ),
        // sql: PASSWORD '…', IDENTIFIED BY '…'
        (r#"(?i)\b(password|passwd|identified\s+by)\s+('[^']*'|"[^"]*")"#, "${1} '***'"),
        // --password value, --token value
        (r"(?i)(--(?:password|passwd|token|secret|api-key))\s+(\S+)", "${1} ***"),
        // scheme://user:pass@host
        (r"(?i)\b([a-z][a-z0-9+.-]*://[^/\s:@]+):[^@\s/]+@", "${1}:***@"),
        // Authorization: Bearer …
        (r"(?i)(authorization:\s*(?:bearer|basic|token)\s+)[^\s'\x22]+", "${1}***"),
    ]
    .into_iter()
    .map(|(pattern, replacement)| (Regex::new(pattern).expect("static pattern compiles"), replacement))
    .collect()
});

pub fn redact_secrets(text: &str) -> Cow<'_, str> {
    let mut out = Cow::Borrowed(text);
    for (re, replacement) in RULES.iter() {
        if let Cow::Owned(changed) = re.replace_all(&out, *replacement) {
            out = Cow::Owned(changed);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_inline_secrets() {
        for (input, expected) in [
            (
                "ALTER USER postgres WITH password='hunter2=0';",
                "ALTER USER postgres WITH password=***;",
            ),
            (
                "ALTER USER postgres WITH PASSWORD 'hunter2';",
                "ALTER USER postgres WITH PASSWORD '***';",
            ),
            (
                "CREATE USER x IDENTIFIED BY \"pw\"",
                "CREATE USER x IDENTIFIED BY '***'",
            ),
            ("PGPASSWORD=hunter2 psql -h db", "PGPASSWORD=*** psql -h db"),
            ("export GITHUB_TOKEN=ghp_abc123", "export GITHUB_TOKEN=***"),
            (
                "tool --api-key=abc --verbose",
                "tool --api-key=*** --verbose",
            ),
            (
                "mysqldump --password hunter2 db",
                "mysqldump --password *** db",
            ),
            (
                "git clone https://me:ghp_abc@github.com/x/y",
                "git clone https://me:***@github.com/x/y",
            ),
            (
                "curl -H 'Authorization: Bearer eyJhbGci.x.y' https://api",
                "curl -H 'Authorization: Bearer ***' https://api",
            ),
        ] {
            assert_eq!(redact_secrets(input), expected, "{input}");
        }
    }

    #[test]
    fn leaves_ordinary_commands_alone() {
        for input in [
            "sudo apt install ./zoom_amd64.deb",
            "passwd",
            "cd ~/src && pwd",
            "ssh paya@host -p 2222",
            "grep -r token src/",
            "https://zoom.us/download",
        ] {
            assert!(matches!(redact_secrets(input), Cow::Borrowed(_)), "{input}");
        }
    }
}
