# footprint

A local, read-only activity timeline and provenance tool for Linux. It reads the records your apps already keep (browser history, download managers, package logs, the system journal, shell history, recently opened files, startup locations), turns them into one normalized timeline, and answers the questions you ask after something suspicious happens:

- **What happened** between 20:40 and 21:30?
- **What touched** this domain, IP, or file?
- **Where did this file come from?** Email link → redirectors → page → download → install.
- **What changed** on the system since the last time I looked?

It never runs in the background, never modifies the files it reads, never opens secrets (cookies, saved passwords, wallets, keys), and never touches the network.

> Status: early development. Everything through the interactive TUI is done. See [Roadmap](#roadmap).

## Why

Most of what a person does on a Linux desktop is already written down in small SQLite, JSON and log files: a browser keeps every visit and redirect, a download manager keeps the source URL of every file, apt keeps every install with the exact command line, and the journal keeps every `sudo`. After a fake-recruiter "update your Zoom SDK" scam, reconstructing the whole story from those files took dozens of hand-written `sqlite3`, `journalctl`, `dpkg` and `find` commands, along with a few traps: locked databases, timestamps counted from 1601, a browser living under `~/snap/`, a download manager that hid downloads from the browser, a `find` that was silently `bfs`.

`footprint` turns that investigation into a repeatable command.

## Features

| | |
|---|---|
| **Collectors** | Chromium family (Chrome, Chromium, Brave, Edge, Vivaldi, Opera; native, snap and flatpak; all profiles): visits, server and client redirects, downloads, searches, site permissions (granted and one-time use), extensions · XDM download manager · dpkg / apt, including rotated `.gz` logs · rpm/dnf, pacman, zypper, snap, flatpak · journald (`sudo`, `su`, `pkexec`, ssh logins) · auth.log / secure · wtmp/btmp logins · bash / zsh / fish history · python / psql / mysql / sqlite3 / node REPL history · GTK recently used files · KDE recent documents · filesystem timeline (native statx walker) · persistence snapshot (systemd, cron, autostart, rc files, PAM, SUID, git hooks) |
| **Planned collectors** | Firefox / Zen / LibreWolf · live processes and sockets · clamscan / rkhunter / chkrootkit import |
| **Fault-tolerant** | A source that fails is recorded as `error` and the run goes on; skipped lines mark it `partial`. Every source will be probed first: an app that isn't installed is a coverage note, and each kind of evidence gets a fallback chain (e.g. downloads: browser history → download managers → file xattrs → shell `curl`/`wget` → file times) |
| **One time model** | WebKit, PRTime, Unix, journald, .NET binary, local-naive and RFC 3339 timestamps are all normalized to UTC; the raw value is always kept |
| **Case database** | A single SQLite file (`0600`) with full-text search, idempotent re-runs (a second `collect` adds only what is new), and provenance for every event (source file, its sha256, and the exact row, cursor or line) |
| **Correlation** | Redirect chains, download → file → install links, untimed shell commands anchored to apt and sudo logs |
| **Trace** | `footprint trace <file>` prints the full provenance chain with a confidence on every edge |
| **State and diff** | Site permissions, extensions, startup units, cron, rc files, PAM, SUID files; diff two runs |
| **IOCs** | Domains, IPs/CIDRs, hashes, path globs, matched against everything collected |

## Example

```bash
footprint collect --since 2026-10-16T00:00
footprint list --since 20:40 --until 21:30 --kind web.redirect,dl.manager_download,priv.sudo,pkg.install
footprint search us09-web-zoom.com
footprint show 1832
```

```text
2026-10-16 21:03:17  Brave/Default  web.redirect         https://zoom.us/download
2026-10-16 21:06:20  xdm            dl.manager_download  ~/Downloads/Programs/zoom_amd64.deb ← https://cdn.zoom.us/prod/7.2.1.5760/zoom_amd64.deb (288.8 MiB)
2026-10-16 21:07:56  journald       priv.sudo            paya: /usr/bin/apt install ./zoom_amd64.deb (pts/1)
2026-10-16 21:07:59  apt            pkg.install          install: zoom:amd64 (+1 more) ← apt install ./zoom_amd64.deb
2026-10-16 21:08:00  dpkg           pkg.install          zoom:amd64 7.2.1.5760
```

## Commands

| Command | |
|---|---|
| `collect [--since T] [--until T] [--sources LIST] [--exclude LIST] [--home DIR] [--root DIR] [--assume-tz TZ]` | discover sources and collect them into the case. `LIST` takes kinds or families (`chromium`, `pkg`, `shell.history`) |
| `sources [--run R \| --all-runs]` | sources of the last run with status (`ok`, `partial`, `unavailable`, `error: …`) |
| `list [--since T] [--until T] [--kind K,..] [--host H,..] [--match TEXT] [--limit N] [--reverse] [--format table\|jsonl]` | merged timeline; `--kind web.*` matches a family, `--host zoom.us` also matches its subdomains |
| `show <id>` | one event with provenance and raw timestamp |
| `search <query>` | full-text search over subjects, summaries and attributes |
| `trace <path\|url\|event-id>` | provenance chain with a confidence on every edge |
| `state [--kind K] [--run R]`, `diff <runA> <runB>` | captured state and its changes between runs |
| `ioc add\|import\|check`, `export --format jsonl\|csv\|html [--redact]` | indicators of compromise and reports |
| `tui` | interactive terminal UI: timeline with live filters, detail, trace tree, state/diff, IOC hits, sources/coverage, collect with per-source progress (`NO_COLOR` respected) |

Times accept `2026-10-16T20:40`, `20:40` (today), `-24h`, `-30d`, `yesterday`, `@1792172917`.
The case file defaults to `./footprint.db`; pass `-c PATH` to use another. Read commands never create one.
`--root` points system sources (`/var/log`, the journal) at a mounted disk image; `--assume-tz` fixes the
offset for logs that store local time (by default the host's zone *at that date* is used).

## Build

```bash
cargo build --release
./target/release/footprint --help
cargo test   # includes the fake-zoom scenario, built from synthetic fixtures
```

It runs as your user. Some sources (the full journal, root's crontab, `/root`) need root; without it they are reported as partial instead of failing.

## Safety

- **Read-only**: source databases are copied to a private temp dir (`0700`, one subdir per source) and opened read-only. Originals are never written, locked or vacuumed.
- **No secrets**: cookies, saved logins, `Web Data`, extension storage, keyrings, SSH and GPG private keys, wallet files and `.env` files are on a deny-list enforced by the one helper every read goes through.
- **Redaction**: passwords typed inline in shell/REPL history or sudo command lines (`PASSWORD '…'`, `PGPASSWORD=…`, `--password …`, `user:pass@host`, `Authorization: …`) are masked as `***` before they are stored.
- **Evidence is untrusted**: size limits (including gunzip), and nothing found is ever executed. symlink-escape checks and stripping control characters before printing.
- **No network**, ever.
- **The case file is private** (`0600`): it holds your browsing history.

## Limitations

- **Untimed history** (bash without `HISTTIMEFORMAT`, REPLs) is stored as `(untimed)` and doesn't show up in
  time-filtered lists until anchoring against apt/sudo lands. `list --kind shell.command` shows it.
- **journald visibility**: outside the `adm`/`systemd-journal` groups you only see your own entries, and this is
  not yet reported as `partial`.
- Recently-used files cover GTK apps only; KDE comes with the fallbacks.

## Layout

```
crates/
  core      Event, StateRecord, Artifact, Provenance, time normalization
  store     SQLite case schema, dedup, queries, FTS5
  collect   Collector trait + one module per source family
  correlate links (redirect chains, download→install, anchoring), IOC matching, trace tree
  render    jsonl / csv / html export with redaction
  query     the read model + collect runner the CLI and TUI call; the only place with SQL
  tui       interactive terminal UI (ratatui); terminal-independent state machine, tested headless
  cli       footprint binary (clap)
  fixtures  synthetic evidence in the real formats, for tests (never real data)
```

## Roadmap

- workspace, time model, case store, `sources`
- Chromium collector, `list` / `show` / `search`
- XDM, dpkg/apt, journald, shell and REPL history, desktop
- fixes for bugs found on a real host
- availability checks, coverage report, fallback sources
- correlation and `trace`
- filesystem timeline, persistence snapshot, `state`, `diff`
- IOCs and `export`
- interactive TUI (`footprint tui`)
- Firefox family, live state, scanner import, polish — planned

Linux only for now. The collector interface keeps Windows and macOS open, but they aren't being built.
