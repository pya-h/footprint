use crate::{command_text, normalize_command};
use anyhow::Result;
use fp_core::{Confidence, Link, LinkType};
use fp_store::{EventDetail, Store};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

/// Identical commands logged this close together are one run: sudo's line, then one apt
/// event per action line (`Install:`, `Upgrade:`…) of the same `Start-Date` block. In µs.
const SAME_RUN_US: i64 = 60_000_000;

/// A history file across runs: the source kind and the file the entries came from.
type HistoryFile = (String, Option<String>);

/// One execution of a command, as the timed logs saw it.
struct Run<'a> {
    /// the earliest witness (sudo logs before apt starts)
    anchor: &'a EventDetail,
    ids: Vec<u64>,
}

/// Anchors untimed shell entries against timed apt/sudo commands with the same command line.
///
/// A repeated command (a daily `sudo apt update`) has several runs to choose from. A history
/// file is in time order and loses its oldest lines first, like the journal, so each file is
/// walked newest first: an entry takes the latest unused run that is not after the entry
/// below it. Entries left without a run stay untimed rather than borrowing a wrong one.
pub fn correlate_anchoring(store: &mut Store) -> Result<usize> {
    let anchor_events = store.get_events_by_kinds(&["pkg.install", "pkg.upgrade", "priv.sudo"])?;
    // events come oldest first, so runs are built in time order
    let mut runs: HashMap<String, Vec<Run>> = HashMap::new();
    for anchor in &anchor_events {
        let cmd = normalize_command(&command_text(anchor));
        if cmd.is_empty() {
            continue;
        }
        let list = runs.entry(cmd).or_default();
        match list.last_mut() {
            Some(run) if anchor.ts_utc - run.anchor.ts_utc <= SAME_RUN_US => {
                run.ids.push(anchor.id);
            }
            _ => list.push(Run {
                anchor,
                ids: vec![anchor.id],
            }),
        }
    }

    // earlier passes are kept: their entries are fixed points and their runs are taken
    let mut anchored: HashMap<u64, u64> = HashMap::new();
    for link in store.list_links()? {
        if link.rel == "anchored_to" && link.src_type == "event" {
            anchored.insert(link.src_id, link.dst_id);
        }
    }
    // a run is named by its first witness
    let taken: HashSet<u64> = anchored.values().copied().collect();
    let mut used: HashSet<u64> = HashSet::new();
    for run in runs.values().flatten() {
        if run.ids.iter().any(|id| taken.contains(id)) {
            used.insert(run.anchor.id);
        }
    }

    // each history file in its own order, across runs (the file path, not the run's source row)
    let mut files: HashMap<HistoryFile, Vec<(u64, EventDetail)>> = HashMap::new();
    for shell_ev in store.get_events_by_kinds(&["shell.command"])? {
        let order = shell_ev
            .attrs_json
            .as_deref()
            .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
            .and_then(|attrs| attrs.get("order").and_then(Value::as_u64))
            .unwrap_or(0);
        files
            .entry((shell_ev.source_kind.clone(), shell_ev.prov_file.clone()))
            .or_default()
            .push((order, shell_ev));
    }

    let mut links = Vec::new();
    for entries in files.values_mut() {
        entries.sort_by_key(|(order, ev)| (*order, ev.id));
        // timed and earlier-anchored entries are fixed points: nothing below one may be later
        // than it, nothing above it earlier
        let fixed = |ev: &EventDetail| ev.ts_precision != "order_only";
        let mut floors = Vec::with_capacity(entries.len());
        let mut floor = i64::MIN;
        for (_, ev) in entries.iter() {
            floors.push(floor);
            if fixed(ev) {
                floor = floor.max(ev.ts_utc);
            }
        }
        let mut ceiling = i64::MAX;
        for (k, (_, shell_ev)) in entries.iter().enumerate().rev() {
            if fixed(shell_ev) {
                ceiling = ceiling.min(shell_ev.ts_utc);
                continue;
            }
            let Some(cmd) = shell_ev.subject.as_deref().map(normalize_command) else {
                continue;
            };
            let Some(list) = runs.get(&cmd) else {
                continue;
            };
            let Some(i) = (0..list.len()).rev().find(|&i| {
                let run = &list[i];
                (floors[k]..=ceiling).contains(&run.anchor.ts_utc) && !used.contains(&run.anchor.id)
            }) else {
                continue;
            };
            used.insert(list[i].anchor.id);
            let anchor = list[i].anchor;
            ceiling = anchor.ts_utc;
            store.update_event_anchoring(shell_ev.id, anchor.ts_utc, "second", "derived")?;
            let why = if list.len() > 1 {
                format!(
                    "anchored against {} commandline (run {} of {} identical; placed by history order)",
                    anchor.kind,
                    i + 1,
                    list.len()
                )
            } else {
                format!("anchored against {} commandline", anchor.kind)
            };
            links.push(Link {
                src_type: LinkType::Event,
                src_id: shell_ev.id,
                rel: "anchored_to".to_string(),
                dst_type: LinkType::Event,
                dst_id: anchor.id,
                confidence: Confidence::Derived,
                why: Some(why),
            });
        }
    }

    Ok(store.insert_links(&links)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fp_core::time::TimeFormat;
    use fp_core::{Actor, Event, EventKind, Provenance, SourceInstance, TimePrecision};
    use fp_store::{Batch, NewRun};
    use serde_json::json;
    use std::path::PathBuf;

    const HOUR: i64 = 3_600_000_000;

    fn timed(inst: u64, kind: EventKind, command: &str, ts_utc: i64) -> Event {
        Event {
            run_id: 1,
            source_kind: "auth.journald".into(),
            ts_utc,
            ts_precision: TimePrecision::Second,
            ts_raw: ts_utc.to_string(),
            ts_raw_format: TimeFormat::UnixSeconds,
            ts_tz_assumed: None,
            source_inst: inst,
            kind,
            actor: Some(Actor::app("sudo")),
            subject: command.into(),
            subject_host: None,
            summary: command.into(),
            attrs: json!({ "command": command }),
            provenance: Provenance {
                file: PathBuf::from("/var/log/journal"),
                file_sha256: None,
                locator: format!("t={ts_utc}"),
            },
            confidence: Confidence::Direct,
        }
    }

    fn untimed(inst: u64, command: &str, order: usize) -> Event {
        Event {
            source_kind: "shell.bash".into(),
            ts_utc: 0,
            ts_precision: TimePrecision::OrderOnly,
            ts_raw: String::new(),
            ts_raw_format: TimeFormat::None,
            kind: EventKind::ShellCommand,
            actor: Some(Actor::app("bash")),
            attrs: json!({ "order": order }),
            provenance: Provenance {
                file: PathBuf::from("/home/u/.bash_history"),
                file_sha256: None,
                locator: format!("line {order}"),
            },
            ..timed(inst, EventKind::ShellCommand, command, 0)
        }
    }

    fn ingest(store: &mut Store, kind: &str, path: &str, events: &[Event]) {
        let run = store
            .insert_run(&NewRun {
                host: "h",
                user: "u",
                uid: 1000,
                euid: 1000,
                version: "0",
                tz: "UTC",
                args: "",
            })
            .unwrap();
        let inst = SourceInstance::new(kind, "x", None, PathBuf::from(path));
        let id = store.insert_source(run, &inst).unwrap();
        let events: Vec<Event> = events
            .iter()
            .map(|e| Event {
                source_inst: id,
                ..e.clone()
            })
            .collect();
        store
            .ingest(
                run,
                &Batch {
                    source_identity: &inst.identity(),
                    events: &events,
                    states: &[],
                    artifacts: &[],
                },
            )
            .unwrap();
    }

    fn anchored_at(store: &Store) -> Vec<(String, i64)> {
        let mut out: Vec<_> = store
            .get_events_by_kinds(&["shell.command"])
            .unwrap()
            .into_iter()
            .map(|e| (e.locator.unwrap_or_default(), e.ts_utc))
            .collect();
        out.sort();
        out
    }

    #[test]
    fn repeated_commands_are_placed_by_history_order() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open_or_create(dir.path().join("case.db")).unwrap();
        // three runs of `sudo apt update`; the 3h one also left an apt record, which is the
        // same run, not a fourth
        ingest(
            &mut store,
            "auth.journald",
            "/var/log/journal",
            &[
                timed(0, EventKind::PrivSudo, "/usr/bin/apt update", HOUR),
                timed(0, EventKind::PrivSudo, "/usr/bin/apt update", 2 * HOUR),
                timed(
                    0,
                    EventKind::PrivSudo,
                    "/usr/bin/apt install ./x.deb",
                    2 * HOUR + HOUR / 2,
                ),
                timed(0, EventKind::PrivSudo, "/usr/bin/apt update", 3 * HOUR),
                timed(0, EventKind::PkgUpgrade, "apt update", 3 * HOUR + 5_000_000),
            ],
        );
        // the history lost its oldest line: update, install, update
        ingest(
            &mut store,
            "shell.bash",
            "/home/u/.bash_history",
            &[
                untimed(0, "sudo apt update", 0),
                untimed(0, "sudo apt install ./x.deb", 1),
                untimed(0, "sudo apt update", 2),
            ],
        );
        assert_eq!(correlate_anchoring(&mut store).unwrap(), 3);
        assert_eq!(
            anchored_at(&store),
            [
                ("line 0".to_string(), 2 * HOUR),
                ("line 1".to_string(), 2 * HOUR + HOUR / 2),
                ("line 2".to_string(), 3 * HOUR),
            ]
        );
        let why = store
            .list_links()
            .unwrap()
            .into_iter()
            .filter_map(|l| l.why)
            .collect::<Vec<_>>();
        assert!(
            why.iter().any(|w| w.contains("run 3 of 3 identical")),
            "{why:?}"
        );

        // a second pass changes nothing; a newer line can only take a newer run
        assert_eq!(correlate_anchoring(&mut store).unwrap(), 0);
        ingest(
            &mut store,
            "shell.bash",
            "/home/u/.bash_history",
            &[untimed(0, "sudo apt update", 3)],
        );
        assert_eq!(correlate_anchoring(&mut store).unwrap(), 0);
        assert_eq!(anchored_at(&store).last(), Some(&("line 3".to_string(), 0)));
    }
}
