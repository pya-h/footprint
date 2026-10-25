//! Mutation smoke tests for the evidence parsers:
//! seeds in the real formats are corrupted deterministically, and every variant must give
//! events or an error, never a panic.

use crate::auth_fallbacks::{AuthLogCollector, WtmpCollector};
use crate::collector::{CollectCtx, Collector, Sink};
use crate::desktop::DesktopCollector;
use crate::pkg::{AptCollector, DpkgCollector};
use crate::pkg_fallbacks::{DnfCollector, PacmanCollector, ZypperCollector};
use crate::shell::ShellCollector;
use fp_core::SourceInstance;
use std::path::Path;

const ROUNDS: usize = 400;
/// bytes that steer parsers into their edge cases: separators, escapes, multibyte, NUL
const SPICE: &[u8] = b":;#\\\n\r\t '\"<>&%0\x00\x83\xff\xe2\x80\xae9-+@";

/// xorshift64*: deterministic, so a failure reproduces from the round number alone
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn mutate(seed: &[u8], rng: &mut Rng) -> Vec<u8> {
    let mut out = seed.to_vec();
    for _ in 0..=rng.below(8) {
        let at = rng.below(out.len() + 1);
        match rng.below(5) {
            0 if !out.is_empty() => {
                let end = (at + rng.below(16)).min(out.len());
                out.drain(at.min(end)..end);
            }
            1 => out.insert(at, SPICE[rng.below(SPICE.len())]),
            2 if at < out.len() => out[at] = rng.next() as u8,
            3 => {
                let start = rng.below(out.len());
                let chunk: Vec<u8> = out[start..(start + rng.below(32)).min(out.len())].to_vec();
                out.splice(at.min(out.len())..at.min(out.len()), chunk);
            }
            _ => out.truncate(at),
        }
    }
    out
}

fn ctx(dir: &Path) -> CollectCtx {
    CollectCtx {
        run_id: 1,
        since: None,
        until: None,
        assume_tz: Some("+03:30".into()),
        temp_dir: dir.join("tmp"),
    }
}

fn survive(collector: &dyn Collector, kind: &str, app: &str, file: &str, seed: &[u8]) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("tmp")).expect("stage dir");
    let path = dir.path().join(file);
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15 ^ seed.len() as u64);
    for round in 0..ROUNDS {
        let input = mutate(seed, &mut rng);
        std::fs::write(&path, &input).expect("write input");
        let inst = SourceInstance::new(kind, app, None, path.clone());
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = collector.collect(&inst, &ctx(dir.path()), &mut Sink::default());
        }));
        assert!(
            outcome.is_ok(),
            "{kind}/{app} panicked in round {round} on {:?}",
            String::from_utf8_lossy(&input)
        );
    }
}

#[test]
fn package_logs_never_panic() {
    survive(
        &DpkgCollector,
        "pkg.dpkg",
        "dpkg",
        "dpkg.log",
        b"2026-10-16 21:08:00 install zoom:amd64 <none> 7.2.1.5760\n2026-10-16 21:08:01 status installed zoom:amd64 7.2.1.5760\n",
    );
    survive(
        &AptCollector,
        "pkg.apt",
        "apt",
        "history.log",
        b"Start-Date: 2026-10-16  21:07:58\nCommandline: apt install ./zoom_amd64.deb\nRequested-By: paya (1000)\nInstall: zoom:amd64 (7.2.1.5760), libxcb-xtest0:amd64 (1.15-1, automatic)\nEnd-Date: 2026-10-16  21:08:05\n",
    );
    survive(
        &PacmanCollector,
        "pkg.pacman",
        "pacman",
        "pacman.log",
        b"[2026-10-16T21:08:00+0330] [ALPM] installed zoom (7.2.1-1)\n[2026-10-16T21:09:00+0330] [ALPM] upgraded curl (8.1.0 -> 8.2.0)\n",
    );
    survive(
        &DnfCollector,
        "pkg.dnf",
        "dnf",
        "dnf.rpm.log",
        b"2026-10-16T21:08:00+0330 SUBDEBUG Installed: zoom-7.2.1.x86_64\n2026-10-16 21:09:00 INFO Upgraded: curl-8.2\n",
    );
    survive(
        &ZypperCollector,
        "pkg.zypper",
        "zypper",
        "history",
        b"2026-10-16 21:08:00|install|zoom|7.2.1|x86_64||repo|\n",
    );
}

#[test]
fn shell_histories_never_panic() {
    survive(
        &ShellCollector,
        "shell.history",
        "bash",
        ".bash_history",
        b"#1792172917\nsudo apt install ./zoom_amd64.deb\n#text\nls\n",
    );
    survive(
        &ShellCollector,
        "shell.history",
        "zsh",
        ".zsh_history",
        b": 1792172917:0;curl -s https://x | sh\n: 1792172918:0;echo \\\ncontinued\n\x83\xa3meta\n",
    );
    survive(
        &ShellCollector,
        "shell.history",
        "fish",
        "fish_history",
        b"- cmd: wget -qO- https://x \\\\| sh\n  when: 1792172917\n  paths:\n    - ~/Downloads\n",
    );
    survive(
        &ShellCollector,
        "shell.history",
        "sqlite3",
        ".sqlite_history",
        b"_HiStOrY_V2_\nselect\\040*\\040from\\040t;\n",
    );
}

#[test]
fn desktop_and_auth_logs_never_panic() {
    survive(
        &DesktopCollector,
        "desktop.recent",
        "gtk-recent",
        "recently-used.xbel",
        b"<?xml version=\"1.0\"?><xbel><bookmark href=\"file:///h/a%20b.pdf\" added=\"2026-10-16T17:48:37Z\" modified=\"2026-10-16T17:48:37Z\"><info><metadata><mime:mime-type type=\"application/pdf\"/><bookmark:applications><bookmark:application name=\"evince\" exec=\"evince %u\" modified=\"2026-10-16T17:48:37Z\" count=\"1\"/></bookmark:applications></metadata></info></bookmark></xbel>",
    );
    survive(
        &AuthLogCollector,
        "auth.log",
        "authlog",
        "auth.log",
        b"Oct 16 21:07:56 host sudo:     paya : TTY=pts/1 ; PWD=/home/paya ; USER=root ; COMMAND=/usr/bin/apt install ./x.deb\n2026-10-16T21:05:00.000000+03:30 host sshd[1]: Accepted publickey for paya from 192.0.2.1 port 22 ssh2\n",
    );
    let mut record = vec![0u8; 384];
    record[0] = 7;
    record[44..48].copy_from_slice(b"paya");
    record[340..344].copy_from_slice(&1_792_172_917i32.to_le_bytes());
    survive(
        &WtmpCollector,
        "auth.wtmp",
        "wtmp",
        "wtmp",
        &record.repeat(2),
    );
}
