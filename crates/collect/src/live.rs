//! Live state (opt-in, `--with-live`): processes, sockets, kernel modules, tainted flag.
//! Everything is a state record; attention items are flags, not verdicts.

use crate::collector::{CollectCtx, Collector, Env, Sink};
use crate::persist::PkgVerifier;
use anyhow::Result;
use fp_core::{Availability, Provenance, SourceInstance, StateRecord};
use serde_json::json;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

pub struct LiveCollector;

/// `/proc/<pid>/fd/<fd>` links look like `socket:[12345]`
fn socket_inode(link: &Path) -> Option<String> {
    let text = fs::read_link(link).ok()?.to_string_lossy().into_owned();
    let inner = text.strip_prefix("socket:[")?.strip_suffix(']')?;
    Some(inner.to_string())
}

fn is_hidden(path: &Path) -> bool {
    path.components().any(|c| {
        let s = c.as_os_str().to_string_lossy();
        s.starts_with('.') && s.len() > 1
    })
}

fn proc_flags(exe: &Path) -> Vec<String> {
    let text = exe.to_string_lossy();
    let mut flags = Vec::new();
    if text.contains("(deleted)") {
        flags.push("deleted_binary".to_string());
    }
    if text.starts_with("/tmp") || text.starts_with("/var/tmp") || text.starts_with("/dev/shm") {
        flags.push("exec_in_tmp".to_string());
    }
    if is_hidden(exe) {
        flags.push("exec_in_hidden_dir".to_string());
    }
    flags
}

fn collect_processes(root: &Path, ctx: &CollectCtx, sink: &mut Sink) {
    let proc = root.join("proc");
    let Ok(entries) = fs::read_dir(&proc) else {
        return;
    };
    for entry in entries.flatten() {
        let pid = match entry.file_name().to_string_lossy().parse::<u32>() {
            Ok(p) => p,
            Err(_) => continue,
        };
        let dir = entry.path();
        let exe = fs::read_link(dir.join("exe")).ok();
        let cmdline = fs::read(dir.join("cmdline"))
            .ok()
            .map(|bytes| {
                let text: String = bytes
                    .split(|&b| b == 0)
                    .filter_map(|part| std::str::from_utf8(part).ok())
                    .collect::<Vec<_>>()
                    .join(" ");
                text.trim().to_string()
            })
            .filter(|s| !s.is_empty());
        let user = fs::read_to_string(dir.join("status"))
            .ok()
            .and_then(|status| {
                status.lines().find_map(|l| {
                    l.strip_prefix("Uid:")
                        .and_then(|v| v.split_whitespace().next())
                        .map(|u| u.to_string())
                })
            });

        let flags = exe
            .as_ref()
            .map(|e| proc_flags(e))
            .unwrap_or_else(|| vec!["no_exe".to_string()]);
        let exe_display = exe
            .as_ref()
            .map(|e| e.to_string_lossy().into_owned())
            .unwrap_or_default();

        let value = json!({
            "pid": pid,
            "exe": exe_display,
            "cmdline": cmdline,
            "uid": user,
            "flags": flags,
        });
        emit_state("live.process", &pid.to_string(), value, &proc, ctx, sink);
    }
}

fn parse_socket_addr(field: &str, ipv6: bool) -> Option<String> {
    let (addr_hex, port_hex) = field.split_once(':')?;
    let port = u16::from_str_radix(port_hex, 16).ok()?;
    if ipv6 {
        // four 32-bit words printed left to right, each little-endian on the wire
        let raw = u128::from_str_radix(addr_hex, 16).ok()?;
        let mut bytes = [0u8; 16];
        for i in 0..4 {
            let word = (raw >> ((3 - i) * 32)) as u32;
            bytes[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        let addr = std::net::Ipv6Addr::from(bytes);
        Some(format!("[{addr}]:{port}"))
    } else {
        let raw = u32::from_str_radix(addr_hex, 16).ok()?;
        let addr = std::net::Ipv4Addr::from(raw.to_le_bytes());
        Some(format!("{addr}:{port}"))
    }
}

/// `include/net/tcp_states.h`; udp sockets reuse 07 (CLOSE) when unconnected. Others stay hex.
fn socket_state(hex: &str) -> String {
    match hex.to_ascii_uppercase().as_str() {
        "01" => "ESTABLISHED",
        "02" => "SYN_SENT",
        "03" => "SYN_RECV",
        "04" => "FIN_WAIT1",
        "05" => "FIN_WAIT2",
        "06" => "TIME_WAIT",
        "07" => "CLOSE",
        "08" => "CLOSE_WAIT",
        "09" => "LAST_ACK",
        "0A" => "LISTEN",
        "0B" => "CLOSING",
        "0C" => "NEW_SYN_RECV",
        _ => return hex.to_string(),
    }
    .to_string()
}

/// inode -> (pid, comm) for socket lookup
fn socket_owners(root: &Path) -> HashMap<String, (u32, String)> {
    let mut owners = HashMap::new();
    let proc = root.join("proc");
    let Ok(entries) = fs::read_dir(&proc) else {
        return owners;
    };
    for entry in entries.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let comm = fs::read_to_string(entry.path().join("comm"))
            .map(|c| c.trim().to_string())
            .unwrap_or_default();
        let Ok(fds) = fs::read_dir(entry.path().join("fd")) else {
            continue;
        };
        for fd in fds.flatten() {
            if let Some(inode) = socket_inode(&fd.path()) {
                owners.entry(inode).or_insert((pid, comm.clone()));
            }
        }
    }
    owners
}

fn collect_sockets(root: &Path, ctx: &CollectCtx, sink: &mut Sink) {
    let owners = socket_owners(root);
    for (file, proto, ipv6) in [
        ("net/tcp", "tcp", false),
        ("net/tcp6", "tcp6", true),
        ("net/udp", "udp", false),
        ("net/udp6", "udp6", true),
    ] {
        let Ok(text) = fs::read_to_string(root.join("proc").join(file)) else {
            continue;
        };
        for line in text.lines().skip(1) {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() < 10 {
                continue;
            }
            let (Some(local), Some(remote)) = (
                parse_socket_addr(fields[1], ipv6),
                parse_socket_addr(fields[2], ipv6),
            ) else {
                continue;
            };
            let state = socket_state(fields[3]);
            let inode = fields[9];
            let (owner, mut flags) = match owners.get(inode) {
                Some((pid, comm)) => (json!({ "pid": pid, "comm": comm }), Vec::new()),
                // NFS/lockd and friends hold sockets no pid owns: label, don't alarm
                None => (serde_json::Value::Null, vec!["kernel_socket".to_string()]),
            };
            if state == "LISTEN" {
                flags.push("listening".to_string());
            }
            let value = json!({
                "proto": proto,
                "local": local,
                "remote": remote,
                "state": state,
                "owner": owner,
                "flags": flags,
            });
            emit_state(
                "live.socket",
                &format!("{proto}:{inode}"),
                value,
                &root.join("proc"),
                ctx,
                sink,
            );
        }
    }
}

/// module name -> file, from the running kernel's tree only (`/lib/modules/<release>`)
fn module_paths(root: &Path) -> HashMap<String, PathBuf> {
    let mut map = HashMap::new();
    let Ok(release) = fs::read_to_string(root.join("proc/sys/kernel/osrelease")) else {
        return map;
    };
    let mut stack = vec![root.join("lib/modules").join(release.trim())];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            if file_type.is_dir() {
                stack.push(entry.path());
            } else if let Some((stem, _)) = name.split_once(".ko") {
                // the kernel names modules with `_`; files often use `-` (snd-hda-intel.ko)
                map.entry(stem.replace('-', "_")).or_insert(entry.path());
            }
        }
    }
    map
}

fn collect_modules(root: &Path, ctx: &CollectCtx, sink: &mut Sink) {
    let Ok(text) = fs::read_to_string(root.join("proc/modules")) else {
        return;
    };
    let paths = module_paths(root);
    let verifier = PkgVerifier::new(root);
    let mut batch: Vec<PathBuf> = Vec::new();
    let mut rows = Vec::new();
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let (Some(name), Some(size)) = (parts.next(), parts.next()) else {
            continue;
        };
        let path = paths.get(name).cloned();
        if let Some(p) = &path {
            batch.push(p.clone());
        }
        rows.push((name.to_string(), size.to_string(), path));
    }
    let owners = verifier.lookup_owners(&batch.iter().map(|p| p.as_path()).collect::<Vec<_>>());
    for (name, size, path) in rows {
        let owner_pkg = path
            .as_ref()
            .and_then(|p| owners.get(p).and_then(|o| o.clone()));
        let mut flags = Vec::new();
        if owner_pkg.is_none() {
            flags.push("unowned_module".to_string());
        }
        let value = json!({
            "name": name,
            "size": size,
            "path": path.map(|p| p.to_string_lossy().into_owned()),
            "owner_pkg": owner_pkg,
            "flags": flags,
        });
        emit_state("live.module", &name, value, &root.join("proc"), ctx, sink);
    }
}

fn collect_kernel(root: &Path, ctx: &CollectCtx, sink: &mut Sink) {
    let tainted = fs::read_to_string(root.join("proc/sys/kernel/tainted"))
        .ok()
        .and_then(|t| t.trim().parse::<u64>().ok());
    if let Some(t) = tainted {
        let flags: Vec<&str> = if t == 0 {
            Vec::new()
        } else {
            vec!["kernel_tainted"]
        };
        emit_state(
            "live.kernel",
            "tainted",
            json!({ "tainted": t, "nonzero": t != 0, "flags": flags }),
            &root.join("proc"),
            ctx,
            sink,
        );
    }
}

fn emit_state(
    kind: &str,
    key: &str,
    value: serde_json::Value,
    prov_file: &Path,
    ctx: &CollectCtx,
    sink: &mut Sink,
) {
    sink.state(StateRecord {
        run_id: ctx.run_id,
        kind: kind.to_string(),
        key: key.to_string(),
        value,
        provenance: Provenance {
            file: prov_file.to_path_buf(),
            file_sha256: None,
            locator: format!("live:{kind}:{key}"),
        },
    });
}

impl Collector for LiveCollector {
    fn kind(&self) -> &'static str {
        "live.state"
    }

    fn discover(&self, env: &Env) -> Vec<SourceInstance> {
        let mut inst = SourceInstance::new(self.kind(), "live", None, env.system_path("/proc"));
        if !inst.path.is_dir() {
            inst.status = Availability::NotInstalled;
        }
        vec![inst]
    }

    fn collect(&self, inst: &SourceInstance, ctx: &CollectCtx, sink: &mut Sink) -> Result<()> {
        let root = if inst.path == Path::new("/proc") {
            Path::new("/")
        } else {
            inst.path.parent().unwrap_or(Path::new("/"))
        };
        collect_processes(root, ctx, sink);
        collect_sockets(root, ctx, sink);
        collect_modules(root, ctx, sink);
        collect_kernel(root, ctx, sink);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_socket_addresses() {
        assert_eq!(
            parse_socket_addr("0100007F:0035", false).as_deref(),
            Some("127.0.0.1:53")
        );
        assert_eq!(
            parse_socket_addr("00000000:0016", false).as_deref(),
            Some("0.0.0.0:22")
        );
        // ::1 is four LE words 00000000...01000000
        assert_eq!(
            parse_socket_addr("00000000000000000000000001000000:1F90", true).as_deref(),
            Some("[::1]:8080")
        );
        assert!(parse_socket_addr("not-hex:1F90", false).is_none());
    }

    #[test]
    fn names_socket_states() {
        assert_eq!(socket_state("01"), "ESTABLISHED");
        assert_eq!(socket_state("0A"), "LISTEN");
        assert_eq!(socket_state("0a"), "LISTEN");
        assert_eq!(socket_state("FE"), "FE");
    }

    #[test]
    fn flags_suspicious_executables() {
        assert!(
            proc_flags(Path::new("/tmp/.hidden/malware (deleted)"))
                .iter()
                .eq(["deleted_binary", "exec_in_tmp", "exec_in_hidden_dir"])
        );
        assert!(proc_flags(Path::new("/usr/bin/bash")).is_empty());
    }

    #[test]
    fn live_state_from_a_synthetic_proc() {
        let root = tempfile::tempdir().unwrap();
        let w = |rel: &str, body: &str| {
            let path = root.path().join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, body).unwrap();
        };
        w("proc/1/cmdline", "/sbin/init\0splash\0");
        w("proc/1/status", "Name:\tsystemd\nUid:\t0\t0\t0\t0\n");
        w("proc/1/comm", "systemd\n");
        w(
            "proc/net/tcp",
            "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
             0: 0100007F:0277 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 4242 1\n",
        );
        w(
            "proc/modules",
            "snd_hda_intel 61440 3 - Live 0x0\nrootkit 16384 0 - Live 0x0\n",
        );
        w("proc/sys/kernel/tainted", "4096\n");
        w("proc/sys/kernel/osrelease", "7.1.5-test\n");
        w(
            "lib/modules/7.1.5-test/kernel/sound/snd-hda-intel.ko.zst",
            "",
        );
        w(
            "var/lib/dpkg/info/linux-image.list",
            "/lib/modules/7.1.5-test/kernel/sound/snd-hda-intel.ko.zst\n",
        );

        let env = Env::test(root.path().join("home"), root.path().to_path_buf());
        let instances = LiveCollector.discover(&env);
        assert_eq!(instances[0].status, Availability::Present);
        let ctx = CollectCtx {
            run_id: 1,
            since: None,
            until: None,
            assume_tz: None,
            temp_dir: root.path().join("tmp"),
        };
        let mut sink = Sink::default();
        LiveCollector
            .collect(&instances[0], &ctx, &mut sink)
            .unwrap();
        let state = |kind: &str, key: &str| {
            sink.states
                .iter()
                .find(|s| s.kind == kind && s.key == key)
                .unwrap_or_else(|| panic!("{kind} {key}"))
        };

        assert_eq!(
            state("live.process", "1").value["cmdline"],
            "/sbin/init splash"
        );
        // the socket has no owning pid: a kernel socket, labelled rather than alarming
        let socket = &state("live.socket", "tcp:4242").value;
        assert_eq!(socket["local"], "127.0.0.1:631");
        assert_eq!(socket["state"], "LISTEN");
        assert!(socket["flags"].to_string().contains("kernel_socket"));
        // `snd_hda_intel` is the file `snd-hda-intel.ko.zst`, owned through the image's dpkg lists
        assert_eq!(
            state("live.module", "snd_hda_intel").value["owner_pkg"],
            "linux-image"
        );
        assert!(
            state("live.module", "rootkit").value["flags"]
                .to_string()
                .contains("unowned_module")
        );
        assert_eq!(state("live.kernel", "tainted").value["nonzero"], true);
    }
}
