# footprint

A local, read-only activity timeline and provenance tool for Linux. It reads the records your apps already keep (browser history, download managers, package logs, the system journal, shell history, recently opened files, startup locations), turns them into one normalized timeline, and answers the questions you ask after something suspicious happens:

- **What happened** between 20:40 and 21:30?
- **What touched** this domain, IP, or file?
- **Where did this file come from?** Email link → redirectors → page → download → install.
- **What changed** on the system since the last time I looked?

It never runs in the background, never modifies the files it reads, never opens secrets (cookies, saved passwords, wallets, keys), and never touches the network.

> Status: early development. The workspace, time model, case store and Chromium pipeline are in; the pkg, shell and desktop collectors are in with open fixes. See [Roadmap](#roadmap).

## Why

Most of what a person does on a Linux desktop is already written down in small SQLite, JSON and log files: a browser keeps every visit and redirect, a download manager keeps the source URL of every file, apt keeps every install with the exact command line, and the journal keeps every `sudo`. After a fake-recruiter "update your Zoom SDK" scam, reconstructing the whole story from those files took dozens of hand-written `sqlite3`, `journalctl`, `dpkg` and `find` commands, along with a few traps: locked databases, timestamps counted from 1601, a browser living under `~/snap/`, a download manager that hid downloads from the browser, a `find` that was silently `bfs`.

`footprint` turns that investigation into a repeatable command.

## Features

| | |
|---|---|
| **Collectors** | Chromium family (Chrome, Chromium, Brave, Edge, Vivaldi, Opera; native, snap and flatpak; all profiles): visits, redirects, downloads, site permissions, extensions · XDM download manager · dpkg / apt · journald (`sudo`, `su`, `pkexec`, ssh) · bash / zsh / fish history · GTK recently used files |
| **Planned collectors** | Firefox / Zen / LibreWolf · rpm / dnf, pacman, zypper, snap, flatpak · auth.log / secure · KDE recent documents · filesystem timeline (statx) · persistence snapshot · live processes and sockets · clamscan / rkhunter / chkrootkit import |
| **Availability-aware** | Every source is probed first. An app that isn't installed is a coverage note, not an error, and each kind of evidence has a fallback chain (e.g. downloads: browser history → download managers → file xattrs → shell `curl`/`wget` → file times) |
| **One time model** | WebKit, PRTime, Unix, journald, .NET binary, local-naive and RFC 3339 timestamps are all normalized to UTC; the raw value is always kept |
| **Case database** | A single SQLite file (`0600`) with full-text search, idempotent re-runs, and provenance for every event (source file, its sha256, and the exact row or line) |
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
2026-10-16 21:06:20  xdm            dl.manager_download  ~/Downloads/Programs/zoom_amd64.deb ← https://cdn.zoom.us/prod/7.2.1.5760/zoom_amd64.deb
2026-10-16 21:07:56  journald       priv.sudo            paya: /usr/bin/apt install ./zoom_amd64.deb
2026-10-16 21:08:00  dpkg           pkg.install          zoom:amd64 7.2.1.5760
```

## Commands

| Command | |
|---|---|
| `collect [--since T] [--until T] [--sources LIST] [--home DIR] [--assume-tz TZ]` | discover sources and collect them into the case |
| `sources` | discovered sources, their availability and errors |
| `list [--since T] [--until T] [--kind K,..] [--host H] [--match TEXT] [--format table\|jsonl]` | merged timeline |
| `show <id>` | one event with provenance and raw timestamp |
| `search <query>` | full-text search over subjects, summaries and attributes |
| `trace`, `state`, `diff`, `ioc`, `export`, `explain` | upcoming, see the roadmap |

Times accept `2026-10-16T20:40`, `20:40` (today), `-24h`, `-30d`, `yesterday`, `@1792172917`.
The case file defaults to `./footprint.db`; pass `-c PATH` to use another.

## Build

```bash
cargo build --release
./target/release/footprint --help
```

It runs as your user. Some sources (the full journal, root's crontab, `/root`) need root; without it they are reported as partial instead of failing.

## Safety

- **Read-only**: source databases are copied to a private temp dir (`0700`, one subdir per source) and opened read-only. Originals are never written, locked or vacuumed.
- **No secrets**: cookies, saved logins, `Web Data`, extension storage, keyrings, SSH and GPG private keys and wallet files are on an enforced deny-list.
- **Evidence is untrusted**: size limits, no symlink escapes, and control characters stripped before printing. Nothing found is ever executed.
- **No network**, ever.
- **The case file is private** (`0600`): it holds your browsing history.

## Layout

```
crates/
  core      Event, StateRecord, Artifact, Provenance, time normalization
  store     SQLite case schema, dedup, queries, FTS5
  collect   Collector trait + one module per source family
  cli       footprint binary (clap)
```

## Roadmap

- workspace, time model, case store, `sources`
- Chromium collector, `list` / `show` / `search`
- XDM, dpkg/apt, journald, shell, desktop (in, fixes in progress) — planned
- correlation and `trace` — planned
- filesystem timeline, persistence snapshot, `state`, `diff` — planned
- IOCs and `export` — planned
- Firefox family, live state, scanner import, polish — planned

Linux only for now. The collector interface keeps Windows and macOS open, but they aren't being built.
