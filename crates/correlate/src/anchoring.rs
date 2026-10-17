use anyhow::Result;
use fp_core::{Confidence, Link, LinkType};
use fp_store::Store;
use serde_json::Value;

/// Anchors untimed shell entries against timed apt/sudo commands.
pub fn correlate_anchoring(store: &mut Store) -> Result<usize> {
    let shell_events = store.get_events_by_kinds(&["shell.command"])?;
    let timed_commands = store.get_events_by_kinds(&["pkg.install", "pkg.upgrade", "priv.sudo"])?;

    let mut links = Vec::new();

    for shell_ev in &shell_events {
        if shell_ev.ts_precision != "order_only" {
            continue;
        }

        let Some(raw_cmd) = &shell_ev.subject else {
            continue;
        };
        let norm_shell = normalize_cmd(raw_cmd);
        if norm_shell.is_empty() {
            continue;
        }

        for anchor in &timed_commands {
            let anchor_cmd = extract_command(anchor);
            let norm_anchor = normalize_cmd(&anchor_cmd);
            if norm_anchor == norm_shell {
                // Update the shell event timestamp in the database
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
                break;
            }
        }
    }

    Ok(store.insert_links(&links)?)
}

fn normalize_cmd(cmd: &str) -> String {
    let trimmed = cmd.trim();
    if let Some(rest) = trimmed.strip_prefix("sudo ") {
        rest.trim().to_string()
    } else {
        trimmed.to_string()
    }
}

fn extract_command(ev: &fp_store::EventDetail) -> String {
    if let Some(attrs_str) = &ev.attrs_json
        && let Ok(attrs) = serde_json::from_str::<Value>(attrs_str)
        && let Some(cmd) = attrs
            .get("command")
            .or_else(|| attrs.get("commandline"))
            .and_then(Value::as_str)
    {
        return cmd.to_string();
    }
    ev.subject.clone().unwrap_or_default()
}
