use crate::downloads::resolve_path;
use crate::{command_text, normalize_command};
use anyhow::Result;
use fp_core::{Confidence, Link, LinkType};
use fp_store::{EventDetail, Store};
use std::path::Path;
use std::process::Command;

/// The host's tool for reading a `.deb`'s identity; never anything found in the evidence.
const DPKG_DEB: &str = "/usr/bin/dpkg-deb";
/// How long after an install command its package manager log line may follow.
const INSTALL_WINDOW_US: i64 = 120 * 1_000_000;

struct InstallCommand {
    ev: EventDetail,
    /// normalized; see [`normalize_command`]
    cmd: String,
}

/// Links artifacts to install commands (sudo, apt, shell) and install commands to package logs.
pub fn correlate_install(store: &mut Store, root: &Path) -> Result<usize> {
    let commands: Vec<InstallCommand> = store
        .get_events_by_kinds(&["priv.sudo", "pkg.install", "shell.command"])?
        .into_iter()
        .filter_map(|ev| {
            let cmd = normalize_command(&command_text(&ev));
            is_install_cmd(&cmd).then_some(InstallCommand { ev, cmd })
        })
        .collect();
    if commands.is_empty() {
        return Ok(0);
    }
    let pkg_events = store.get_events_by_kinds(&["pkg.install"])?;

    let mut links = Vec::new();
    for art in store.list_artifacts()? {
        let Some(name) = Path::new(&art.path).file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let mentioning: Vec<&InstallCommand> = commands
            .iter()
            .filter(|c| mentions_file(&c.cmd, name))
            .collect();
        if mentioning.is_empty() {
            continue;
        }
        // a shell line is the weakest witness: used only when no sudo or apt record names the file
        let logged = mentioning.iter().any(|c| c.ev.kind != "shell.command");
        let deb_package = inspect_deb(&art.path, root);

        for install in mentioning
            .into_iter()
            .filter(|c| !logged || c.ev.kind != "shell.command")
        {
            links.push(Link {
                src_type: LinkType::Artifact,
                src_id: art.id,
                rel: "installed_from".to_string(),
                dst_type: LinkType::Event,
                dst_id: install.ev.id,
                confidence: Confidence::Direct,
                why: Some(format!("commandline contains artifact basename {name}")),
            });

            let package_prefix = deb_package
                .as_deref()
                .unwrap_or_else(|| name.split('_').next().unwrap_or(name));
            for pkg_ev in pkg_events.iter().filter(|p| {
                p.id != install.ev.id
                    && (0..=INSTALL_WINDOW_US).contains(&(p.ts_utc - install.ev.ts_utc))
                    && p.subject
                        .as_deref()
                        .is_some_and(|s| s.starts_with(package_prefix))
            }) {
                links.push(Link {
                    src_type: LinkType::Event,
                    src_id: install.ev.id,
                    rel: "pkg.install".to_string(),
                    dst_type: LinkType::Event,
                    dst_id: pkg_ev.id,
                    confidence: Confidence::Direct,
                    why: Some(format!(
                        "package {} installed following command",
                        pkg_ev.subject.as_deref().unwrap_or_default()
                    )),
                });
            }
        }
    }

    Ok(store.insert_links(&links)?)
}

/// The file appears as a whole argument (`./zoom_amd64.deb`, `/tmp/zoom_amd64.deb`), not a substring.
fn mentions_file(cmd: &str, name: &str) -> bool {
    cmd.split_whitespace()
        .map(|w| w.trim_matches(['"', '\'']))
        .any(|w| w == name || w.rsplit('/').next() == Some(name))
}

/// `Package` of a `.deb` artifact, read with the host's `dpkg-deb -f` (fixed argv, no shell).
fn inspect_deb(recorded: &str, root: &Path) -> Option<String> {
    let candidate = resolve_path(recorded, root)?;
    // dpkg-deb would block on a FIFO named like a package
    if !candidate.extension().is_some_and(|e| e == "deb")
        || !std::fs::metadata(&candidate).is_ok_and(|m| m.is_file())
    {
        return None;
    }
    let out = Command::new(DPKG_DEB)
        .arg("-f")
        .arg(&candidate)
        .arg("Package")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let package = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!package.is_empty()).then_some(package)
}

fn is_install_cmd(cmd: &str) -> bool {
    const INSTALLS: &[&str] = &[
        "apt install",
        "apt-get install",
        "dpkg -i",
        "dpkg --install",
        "dnf install",
        "yum install",
        "rpm -i",
        "rpm -U",
        "pacman -U",
        "zypper install",
    ];
    INSTALLS.iter().any(|i| cmd.starts_with(i))
}
