pub mod anchoring;
pub mod chains;
pub mod downloads;
pub mod install;
pub mod ioc;
pub mod permissions;
pub mod trace;

use anyhow::Result;
use fp_store::{EventDetail, Store};
use serde_json::Value;
use std::path::Path;

pub use ioc::{IocHit, check_iocs, correlate_iocs, match_event_iocs};
pub use trace::{
    TraceArrow, TraceHead, TraceRow, TraceTree, render_trace, render_tree, trace_tree,
};

#[derive(Debug, Default, Clone, Copy)]
pub struct CorrelateStats {
    pub chains: usize,
    pub downloads: usize,
    pub installs: usize,
    pub anchored: usize,
    pub permissions: usize,
    pub iocs: usize,
}

/// Runs all correlation passes over the case database.
///
/// Passes are idempotent:
/// 1. Navigation chains from browser `from_visit`
/// 2. Download -> artifact stat/hash and XDM ↔ browser heuristic link
/// 3. Artifact -> install command and install command -> pkg.install
/// 4. Bash command anchoring against apt/sudo
/// 5. Site -> permission join
/// 6. IOC match against iocs table
pub fn correlate(store: &mut Store, root: &Path) -> Result<CorrelateStats> {
    let chains = chains::correlate_navigation_chains(store)?;
    let downloads = downloads::correlate_downloads(store, root)?;
    let installs = install::correlate_install(store, root)?;
    let anchored = anchoring::correlate_anchoring(store)?;
    let permissions = permissions::correlate_site_permissions(store)?;
    let iocs = ioc::correlate_iocs(store)?;

    Ok(CorrelateStats {
        chains,
        downloads,
        installs,
        anchored,
        permissions,
        iocs,
    })
}

/// The command an event carries: sudo's `COMMAND=`, apt's `Commandline:`, or the subject.
pub(crate) fn command_text(ev: &EventDetail) -> String {
    ev.attrs_json
        .as_deref()
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .and_then(|attrs| {
            attrs
                .get("command")
                .or_else(|| attrs.get("commandline"))
                .and_then(Value::as_str)
                .map(String::from)
        })
        .or_else(|| ev.subject.clone())
        .unwrap_or_default()
}

/// A command line as compared across sources: `sudo` and its flags dropped, the program reduced
/// to its name, so sudo's `/usr/bin/apt install ./x.deb` equals the shell's `sudo apt install ./x.deb`.
pub(crate) fn normalize_command(cmd: &str) -> String {
    let mut words = cmd.split_whitespace().peekable();
    if words
        .peek()
        .is_some_and(|w| *w == "sudo" || w.ends_with("/sudo"))
    {
        words.next();
        while words.peek().is_some_and(|w| w.starts_with('-')) {
            words.next();
        }
    }
    let Some(program) = words.next() else {
        return String::new();
    };
    let program = program.rsplit('/').next().unwrap_or(program);
    std::iter::once(program)
        .chain(words)
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use fp_core::{
        Actor, Confidence, Event, EventKind, Provenance, SourceInstance, TimePrecision,
        time::TimeFormat,
    };
    use fp_store::{Batch, NewRun};
    use serde_json::json;
    use std::path::PathBuf;

    #[test]
    fn commands_compare_across_sudo_and_shell() {
        assert_eq!(
            normalize_command("/usr/bin/apt install ./zoom_amd64.deb"),
            "apt install ./zoom_amd64.deb"
        );
        assert_eq!(
            normalize_command("sudo -E apt   install ./zoom_amd64.deb"),
            "apt install ./zoom_amd64.deb"
        );
        assert_eq!(normalize_command("  "), "");
    }

    fn sample_event(
        inst_id: u64,
        kind: EventKind,
        subj: &str,
        ts_utc: i64,
        attrs: serde_json::Value,
    ) -> Event {
        Event {
            run_id: 1,
            source_kind: "chromium.history".into(),
            ts_utc,
            ts_precision: TimePrecision::Second,
            ts_raw: ts_utc.to_string(),
            ts_raw_format: TimeFormat::UnixSeconds,
            ts_tz_assumed: None,
            source_inst: inst_id,
            kind,
            actor: Some(Actor::app("Brave")),
            subject: subj.to_string(),
            subject_host: Some("example.com".to_string()),
            summary: subj.to_string(),
            attrs,
            provenance: Provenance {
                file: PathBuf::from("/tmp/test"),
                file_sha256: None,
                locator: "1".to_string(),
            },
            confidence: Confidence::Direct,
        }
    }

    #[test]
    fn test_correlate_pipeline() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("case.db");
        let mut store = Store::open_or_create(&db_path).unwrap();

        let run_id = store
            .insert_run(&NewRun {
                host: "testhost",
                user: "testuser",
                uid: 1000,
                euid: 1000,
                version: "0.1.0",
                tz: "UTC",
                args: "",
            })
            .unwrap();

        let browser_inst = SourceInstance::new(
            "chromium.history",
            "brave",
            Some("Default".into()),
            PathBuf::from("/home/u/.config/Brave"),
        );
        let inst_id = store.insert_source(run_id, &browser_inst).unwrap();

        // Visit 1
        let v1 = sample_event(
            inst_id,
            EventKind::WebVisit,
            "https://example.com/page1",
            100,
            json!({
                "visit_id": 1,
                "from_visit": 0,
            }),
        );
        // Visit 2
        let v2 = sample_event(
            inst_id,
            EventKind::WebRedirect,
            "https://example.com/page2",
            105,
            json!({
                "visit_id": 2,
                "from_visit": 1,
                "transition_qualifiers": ["SERVER_REDIRECT"],
            }),
        );

        store
            .ingest(
                run_id,
                &Batch {
                    source_identity: "chromium.history|brave|Default|/home/u/.config/Brave",
                    events: &[v1, v2],
                    states: &[],
                    artifacts: &[],
                },
            )
            .unwrap();

        let stats = correlate(&mut store, Path::new("/")).unwrap();
        assert_eq!(stats.chains, 1);

        let links = store.list_links().unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].rel, "redirected_from");
        assert_eq!(links[0].confidence, "direct");
    }
}
