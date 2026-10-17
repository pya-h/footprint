use anyhow::Result;
use fp_core::{Confidence, Link, LinkType};
use fp_store::Store;
use serde_json::Value;
use std::path::Path;
use std::process::Command;

/// Links artifacts to install commands (sudo, apt) and install commands to package manager logs.
pub fn correlate_install(store: &mut Store, root: &Path) -> Result<usize> {
    let artifacts = store.list_artifacts()?;
    let cmd_events = store.get_events_by_kinds(&["priv.sudo", "pkg.install", "shell.command"])?;
    let pkg_events = store.get_events_by_kinds(&["pkg.install"])?;

    let mut links = Vec::new();

    for art in &artifacts {
        let path = Path::new(&art.path);
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };

        // Try extracting package info from deb if available
        let deb_pkg = inspect_deb(art.path.as_str(), root);

        // 1. Find install command events mentioning this artifact
        let mut matched_install_cmds = Vec::new();
        for ev in &cmd_events {
            let cmd_text = ev.subject.as_deref().unwrap_or("");
            let attrs_cmd = ev.attrs_json.as_deref().and_then(|a| {
                serde_json::from_str::<Value>(a).ok().and_then(|v| {
                    v.get("command")
                        .or_else(|| v.get("commandline"))
                        .and_then(Value::as_str)
                        .map(|s| s.to_string())
                })
            });

            let full_cmd = attrs_cmd.as_deref().unwrap_or(cmd_text);
            if full_cmd.contains(name) && is_install_cmd(full_cmd) {
                // If it's shell.command, only use it if there's no pkg.apt or priv.sudo
                if ev.kind == "shell.command"
                    && cmd_events.iter().any(|c| {
                        (c.kind == "priv.sudo" || c.source_kind == "pkg.apt")
                            && c.attrs_json.as_deref().is_some_and(|a| a.contains(name))
                    })
                {
                    continue;
                }

                matched_install_cmds.push(ev);
                links.push(Link {
                    src_type: LinkType::Artifact,
                    src_id: art.id,
                    rel: "installed_from".to_string(),
                    dst_type: LinkType::Event,
                    dst_id: ev.id,
                    confidence: Confidence::Direct,
                    why: Some(format!("commandline contains artifact basename {name}")),
                });
            }
        }

        // 2. Link install command to pkg.install events
        for install_ev in matched_install_cmds {
            // Find pkg.install events within 2 minutes after the install command
            let window_us = 120 * 1_000_000;
            for pkg_ev in &pkg_events {
                if pkg_ev.id != install_ev.id
                    && pkg_ev.ts_utc >= install_ev.ts_utc
                    && (pkg_ev.ts_utc - install_ev.ts_utc) <= window_us
                {
                    // Check if package matches
                    let pkg_name = pkg_ev.subject.as_deref().unwrap_or("");
                    let matches = if let Some((ref dp, _)) = deb_pkg {
                        pkg_name.starts_with(dp)
                    } else {
                        // Basename prefix heuristic (e.g. zoom_amd64.deb -> zoom)
                        let prefix = name.split('_').next().unwrap_or(name);
                        pkg_name.starts_with(prefix)
                    };

                    if matches {
                        links.push(Link {
                            src_type: LinkType::Event,
                            src_id: install_ev.id,
                            rel: "pkg.install".to_string(),
                            dst_type: LinkType::Event,
                            dst_id: pkg_ev.id,
                            confidence: Confidence::Direct,
                            why: Some(format!("package {pkg_name} installed following command")),
                        });
                    }
                }
            }
        }
    }

    Ok(store.insert_links(&links)?)
}

fn inspect_deb(target_path: &str, root: &Path) -> Option<(String, String)> {
    let p = Path::new(target_path);
    let candidate = if p.exists() {
        p.to_path_buf()
    } else if let Ok(s) = p.strip_prefix("/") {
        let r = root.join(s);
        if r.exists() {
            r
        } else {
            return None;
        }
    } else {
        return None;
    };

    if !candidate.extension().is_some_and(|e| e == "deb") {
        return None;
    }

    let out = Command::new("dpkg-deb")
        .arg("-f")
        .arg(&candidate)
        .arg("Package")
        .arg("Version")
        .output()
        .ok()?;

    if !out.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&out.stdout);
    let mut pkg = None;
    let mut ver = None;
    for line in text.lines() {
        if let Some(p) = line.strip_prefix("Package: ") {
            pkg = Some(p.trim().to_string());
        } else if let Some(v) = line.strip_prefix("Version: ") {
            ver = Some(v.trim().to_string());
        }
    }
    match (pkg, ver) {
        (Some(p), Some(v)) => Some((p, v)),
        _ => None,
    }
}

fn is_install_cmd(cmd: &str) -> bool {
    let c = cmd.trim();
    let stripped = c.strip_prefix("sudo ").unwrap_or(c).trim();
    stripped.starts_with("apt install")
        || stripped.starts_with("apt-get install")
        || stripped.starts_with("dpkg -i")
        || stripped.starts_with("dpkg --install")
        || stripped.starts_with("dnf install")
        || stripped.starts_with("pacman -U")
        || stripped.starts_with("zypper install")
}
