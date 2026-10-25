use crate::{command_text, normalize_command};
use anyhow::Result;
use fp_core::{Confidence, Link, LinkType};
use fp_store::Store;
use std::collections::HashMap;

/// Anchors untimed shell entries against timed apt/sudo commands with the same command line.
pub fn correlate_anchoring(store: &mut Store) -> Result<usize> {
    let anchor_events = store.get_events_by_kinds(&["pkg.install", "pkg.upgrade", "priv.sudo"])?;
    // events come oldest first, so each command keeps its earliest witness
    let mut anchors = HashMap::new();
    for anchor in &anchor_events {
        let cmd = normalize_command(&command_text(anchor));
        if !cmd.is_empty() {
            anchors.entry(cmd).or_insert(anchor);
        }
    }

    let mut links = Vec::new();
    for shell_ev in store.get_events_by_kinds(&["shell.command"])? {
        if shell_ev.ts_precision != "order_only" {
            continue;
        }
        let Some(cmd) = shell_ev.subject.as_deref().map(normalize_command) else {
            continue;
        };
        let Some(anchor) = anchors.get(&cmd) else {
            continue;
        };
        store.update_event_anchoring(shell_ev.id, anchor.ts_utc, "second", "derived")?;
        links.push(Link {
            src_type: LinkType::Event,
            src_id: shell_ev.id,
            rel: "anchored_to".to_string(),
            dst_type: LinkType::Event,
            dst_id: anchor.id,
            confidence: Confidence::Derived,
            why: Some(format!("anchored against {} commandline", anchor.kind)),
        });
    }

    Ok(store.insert_links(&links)?)
}
