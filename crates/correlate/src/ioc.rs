use anyhow::Result;
use fp_core::net::extract_host;
use fp_core::{Confidence, Link, LinkType};
use fp_store::{EventDetail, IocRow, Store};
use std::net::IpAddr;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct IocHit {
    pub ioc_id: u64,
    pub ioc_type: String,
    pub ioc_value: String,
    pub ioc_note: Option<String>,
    pub target_type: String,
    pub target_id: u64,
    pub target_desc: String,
    pub matched_field: String,
    pub matched_value: String,
}

pub fn check_iocs(store: &Store) -> Result<Vec<IocHit>> {
    let iocs = store.list_iocs()?;
    if iocs.is_empty() {
        return Ok(Vec::new());
    }

    let mut hits = Vec::new();

    // 1. Check Events
    let events = store.get_events_by_kinds(&[])?;
    for ev in &events {
        for ioc in &iocs {
            if let Some((field, val)) = match_event(ev, ioc) {
                hits.push(IocHit {
                    ioc_id: ioc.id,
                    ioc_type: ioc.ioc_type.clone(),
                    ioc_value: ioc.value.clone(),
                    ioc_note: ioc.note.clone(),
                    target_type: "event".to_string(),
                    target_id: ev.id,
                    target_desc: format!(
                        "event {} ({}) {}",
                        ev.id,
                        ev.kind,
                        ev.subject.as_deref().unwrap_or("-")
                    ),
                    matched_field: field,
                    matched_value: val,
                });
            }
        }
    }

    // 2. Check Artifacts
    let artifacts = store.list_artifacts()?;
    for art in &artifacts {
        for ioc in &iocs {
            if let Some((field, val)) = match_artifact(art, ioc) {
                hits.push(IocHit {
                    ioc_id: ioc.id,
                    ioc_type: ioc.ioc_type.clone(),
                    ioc_value: ioc.value.clone(),
                    ioc_note: ioc.note.clone(),
                    target_type: "artifact".to_string(),
                    target_id: art.id,
                    target_desc: format!("artifact {} {}", art.id, art.path),
                    matched_field: field,
                    matched_value: val,
                });
            }
        }
    }

    // 3. Check State Records
    let state_rows = store.list_state(None, None, None)?;
    for st in &state_rows {
        for ioc in &iocs {
            if let Some((field, val)) = match_state(st, ioc) {
                hits.push(IocHit {
                    ioc_id: ioc.id,
                    ioc_type: ioc.ioc_type.clone(),
                    ioc_value: ioc.value.clone(),
                    ioc_note: ioc.note.clone(),
                    target_type: "state".to_string(),
                    target_id: st.rowid as u64,
                    target_desc: format!("state {} {}", st.kind, st.key),
                    matched_field: field,
                    matched_value: val,
                });
            }
        }
    }

    Ok(hits)
}

pub fn correlate_iocs(store: &mut Store) -> Result<usize> {
    let hits = check_iocs(store)?;
    let mut links = Vec::new();

    for hit in &hits {
        let (src_type, src_id) = match hit.target_type.as_str() {
            "event" => (LinkType::Event, hit.target_id),
            "artifact" => (LinkType::Artifact, hit.target_id),
            "state" => (LinkType::State, hit.target_id),
            _ => continue,
        };

        links.push(Link {
            src_type,
            src_id,
            rel: "ioc_match".to_string(),
            dst_type: LinkType::State,
            dst_id: hit.ioc_id,
            confidence: Confidence::Direct,
            why: Some(format!(
                "{}: {} (matched on {})",
                hit.ioc_type, hit.ioc_value, hit.matched_field
            )),
        });
    }

    store.insert_links(&links).map_err(Into::into)
}

pub fn match_event_iocs(
    host: Option<&str>,
    subject: Option<&str>,
    summary: Option<&str>,
    iocs: &[IocRow],
) -> Vec<String> {
    let mut hits = Vec::new();
    for ioc in iocs {
        let ioc_type = ioc.ioc_type.to_lowercase();
        let target = ioc.value.trim();

        if ioc_type == "domain" {
            if let Some(h) = host
                && domain_matches(target, h)
            {
                hits.push(target.to_string());
                continue;
            }
            if let Some(s) = subject
                && let Some(h) = extract_host(s)
                && domain_matches(target, &h)
            {
                hits.push(target.to_string());
                continue;
            }
        } else if ioc_type == "ip" || ioc_type == "cidr" {
            if let Some(h) = host
                && ip_matches_cidr(target, h)
            {
                hits.push(target.to_string());
                continue;
            }
        } else if ioc_type == "url" {
            if let Some(s) = subject
                && s.contains(target)
            {
                hits.push(target.to_string());
                continue;
            }
            if let Some(sm) = summary
                && sm.contains(target)
            {
                hits.push(target.to_string());
                continue;
            }
        } else if ioc_type == "path"
            && let Some(s) = subject
            && path_matches(target, s)
        {
            hits.push(target.to_string());
            continue;
        }
    }
    hits
}

fn match_event(ev: &EventDetail, ioc: &IocRow) -> Option<(String, String)> {
    let ioc_type = ioc.ioc_type.to_lowercase();
    let target = ioc.value.trim();

    match ioc_type.as_str() {
        "domain" => {
            if let Some(ref h) = ev.subject_host
                && domain_matches(target, h)
            {
                return Some(("subject_host".to_string(), h.clone()));
            }
            if let Some(ref subj) = ev.subject
                && let Some(h) = extract_host(subj)
                && domain_matches(target, &h)
            {
                return Some(("subject.host".to_string(), h));
            }
            if let Some(ref attrs) = ev.attrs_json
                && let Ok(v) = serde_json::from_str::<serde_json::Value>(attrs)
                && let Some(url_str) = v.get("url").and_then(|u| u.as_str())
                && let Some(h) = extract_host(url_str)
                && domain_matches(target, &h)
            {
                return Some(("attrs.url".to_string(), h));
            }
        }
        "ip" | "cidr" => {
            if let Some(ref h) = ev.subject_host
                && ip_matches_cidr(target, h)
            {
                return Some(("subject_host".to_string(), h.clone()));
            }
            if let Some(ref attrs) = ev.attrs_json
                && let Ok(v) = serde_json::from_str::<serde_json::Value>(attrs)
            {
                for field in ["peer", "remote_ip", "ip"] {
                    if let Some(ip_str) = v.get(field).and_then(|ip| ip.as_str()) {
                        let clean_ip = ip_str.split(':').next().unwrap_or(ip_str);
                        if ip_matches_cidr(target, clean_ip) {
                            return Some((format!("attrs.{field}"), clean_ip.to_string()));
                        }
                    }
                }
            }
        }
        "sha256" => {
            if let Some(ref sha) = ev.prov_sha256
                && sha.eq_ignore_ascii_case(target)
            {
                return Some(("prov_sha256".to_string(), sha.clone()));
            }
            if let Some(ref attrs) = ev.attrs_json
                && let Ok(v) = serde_json::from_str::<serde_json::Value>(attrs)
                && let Some(sha) = v.get("sha256").and_then(|s| s.as_str())
                && sha.eq_ignore_ascii_case(target)
            {
                return Some(("attrs.sha256".to_string(), sha.to_string()));
            }
        }
        "path" => {
            if let Some(ref file) = ev.prov_file
                && path_matches(target, file)
            {
                return Some(("prov_file".to_string(), file.clone()));
            }
            if let Some(ref subj) = ev.subject
                && path_matches(target, subj)
            {
                return Some(("subject".to_string(), subj.clone()));
            }
        }
        "url" => {
            if let Some(ref subj) = ev.subject
                && subj.contains(target)
            {
                return Some(("subject".to_string(), subj.clone()));
            }
            if let Some(ref attrs) = ev.attrs_json
                && let Ok(v) = serde_json::from_str::<serde_json::Value>(attrs)
                && let Some(url_str) = v.get("url").and_then(|u| u.as_str())
                && url_str.contains(target)
            {
                return Some(("attrs.url".to_string(), url_str.to_string()));
            }
            if let Some(ref sm) = ev.summary
                && sm.contains(target)
            {
                return Some(("summary".to_string(), sm.clone()));
            }
        }
        _ => {}
    }

    None
}

fn match_artifact(art: &fp_store::ArtifactRow, ioc: &IocRow) -> Option<(String, String)> {
    let ioc_type = ioc.ioc_type.to_lowercase();
    let target = ioc.value.trim();

    match ioc_type.as_str() {
        "sha256" => {
            if let Some(ref sha) = art.sha256
                && sha.eq_ignore_ascii_case(target)
            {
                return Some(("artifact.sha256".to_string(), sha.clone()));
            }
        }
        "path" if path_matches(target, &art.path) => {
            return Some(("artifact.path".to_string(), art.path.clone()));
        }
        _ => {}
    }
    None
}

fn match_state(st: &fp_store::StateRow, ioc: &IocRow) -> Option<(String, String)> {
    let ioc_type = ioc.ioc_type.to_lowercase();
    let target = ioc.value.trim();

    match ioc_type.as_str() {
        "domain" => {
            if domain_matches(target, &st.key) {
                return Some(("state.key".to_string(), st.key.clone()));
            }
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&st.value_json)
                && let Some(origin) = v.get("origin").and_then(|o| o.as_str())
                && let Some(h) = extract_host(origin)
                && domain_matches(target, &h)
            {
                return Some(("state.value.origin".to_string(), h));
            }
        }
        "path" => {
            if path_matches(target, &st.key) {
                return Some(("state.key".to_string(), st.key.clone()));
            }
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&st.value_json)
                && let Some(p) = v.get("path").and_then(|p| p.as_str())
                && path_matches(target, p)
            {
                return Some(("state.value.path".to_string(), p.to_string()));
            }
        }
        "sha256" => {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&st.value_json)
                && let Some(sha) = v.get("sha256").and_then(|s| s.as_str())
                && sha.eq_ignore_ascii_case(target)
            {
                return Some(("state.value.sha256".to_string(), sha.to_string()));
            }
        }
        _ => {}
    }
    None
}

pub fn domain_matches(ioc_domain: &str, candidate_host: &str) -> bool {
    let ioc = ioc_domain.trim().to_lowercase();
    let host = candidate_host.trim().to_lowercase();
    host == ioc || host.ends_with(&format!(".{ioc}"))
}

pub fn ip_matches_cidr(ioc: &str, candidate: &str) -> bool {
    let candidate = candidate.trim();
    let Ok(cand_ip) = candidate.parse::<IpAddr>() else {
        return false;
    };

    if let Some((net_str, prefix_str)) = ioc.split_once('/') {
        let Ok(net_ip) = net_str.trim().parse::<IpAddr>() else {
            return false;
        };
        let Ok(prefix) = prefix_str.trim().parse::<u8>() else {
            return false;
        };
        match (net_ip, cand_ip) {
            (IpAddr::V4(net), IpAddr::V4(cand)) => {
                if prefix > 32 {
                    return false;
                }
                let mask = if prefix == 0 {
                    0
                } else {
                    (!0u32) << (32 - prefix)
                };
                (u32::from(net) & mask) == (u32::from(cand) & mask)
            }
            (IpAddr::V6(net), IpAddr::V6(cand)) => {
                if prefix > 128 {
                    return false;
                }
                let mask = if prefix == 0 {
                    0
                } else {
                    (!0u128) << (128 - prefix)
                };
                (u128::from(net) & mask) == (u128::from(cand) & mask)
            }
            _ => false,
        }
    } else if let Ok(target_ip) = ioc.trim().parse::<IpAddr>() {
        target_ip == cand_ip
    } else {
        false
    }
}

pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p_bytes = pattern.as_bytes();
    let t_bytes = text.as_bytes();
    let (mut px, mut tx) = (0, 0);
    let (mut next_px, mut next_tx) = (0, 0);
    let mut star = false;

    while tx < t_bytes.len() {
        if px < p_bytes.len() && (p_bytes[px] == b'?' || p_bytes[px] == t_bytes[tx]) {
            px += 1;
            tx += 1;
        } else if px < p_bytes.len() && p_bytes[px] == b'*' {
            star = true;
            px += 1;
            next_px = px;
            next_tx = tx + 1;
        } else if star {
            px = next_px;
            tx = next_tx;
            next_tx += 1;
        } else {
            return false;
        }
    }

    while px < p_bytes.len() && p_bytes[px] == b'*' {
        px += 1;
    }
    px == p_bytes.len()
}

pub fn path_matches(pattern: &str, text: &str) -> bool {
    let pattern = pattern.trim();
    let text = text.trim();
    if pattern.is_empty() || text.is_empty() {
        return false;
    }
    if glob_match(pattern, text) {
        return true;
    }
    for raw_token in text.split_whitespace() {
        let token = raw_token.trim_matches(['"', '\'', ';', '(', ')', '<', '>', '&', '|']);
        if token.is_empty() {
            continue;
        }
        if glob_match(pattern, token) {
            return true;
        }
        if !pattern.contains('/')
            && !pattern.contains('*')
            && !pattern.contains('?')
            && (token == pattern || token.ends_with(&format!("/{pattern}")))
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_domain_matches() {
        assert!(domain_matches("zoom.us", "zoom.us"));
        assert!(domain_matches("zoom.us", "cdn.zoom.us"));
        assert!(!domain_matches("zoom.us", "us09-web-zoom.us"));
        assert!(!domain_matches("zoom.us", "zoom.com"));
        assert!(!domain_matches("zoom.us", "fakezoom.us.evil.com"));
    }

    #[test]
    fn test_ip_cidr() {
        assert!(ip_matches_cidr("192.168.1.1", "192.168.1.1"));
        assert!(!ip_matches_cidr("192.168.1.1", "192.168.1.2"));
        assert!(ip_matches_cidr("192.168.1.0/24", "192.168.1.42"));
        assert!(!ip_matches_cidr("192.168.1.0/24", "192.168.2.1"));
        assert!(ip_matches_cidr("10.0.0.0/8", "10.254.254.1"));
    }

    #[test]
    fn test_glob_match() {
        assert!(glob_match("*.deb", "/tmp/zoom_amd64.deb"));
        assert!(glob_match("*/zoom*", "/home/user/zoom.deb"));
        assert!(glob_match("/etc/cron*", "/etc/cron.d/job"));
        assert!(!glob_match("*.rpm", "/tmp/zoom.deb"));
    }

    #[test]
    fn test_path_matches() {
        assert!(path_matches("/tmp/evil.sh", "/tmp/evil.sh"));
        assert!(path_matches("/tmp/evil.sh", "bash /tmp/evil.sh"));
        assert!(path_matches(
            "zoom_amd64.deb",
            "apt install ./zoom_amd64.deb"
        ));
        assert!(path_matches(
            "zoom_amd64.deb",
            "sudo dpkg -i /tmp/zoom_amd64.deb"
        ));
        assert!(path_matches("/etc/cron*", "cat /etc/cron.d/malware;"));
        assert!(!path_matches("/tmp/evil.sh", "echo hello world"));
        assert!(!path_matches("evil.sh", "bash other_evil.sh"));
    }

    #[test]
    fn test_check_and_correlate_iocs() {
        use fp_core::{
            Actor, Confidence, Event, EventKind, Provenance, SourceInstance, TimePrecision,
            time::TimeFormat,
        };
        use fp_store::{Batch, NewRun};
        use serde_json::json;
        use std::path::PathBuf;

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

        let inst = SourceInstance::new(
            "chromium.history",
            "brave",
            Some("Default".into()),
            PathBuf::from("/home/u/.config/Brave"),
        );
        let inst_id = store.insert_source(run_id, &inst).unwrap();

        let ev1 = Event {
            run_id,
            source_kind: "chromium.history".into(),
            ts_utc: 100,
            ts_precision: TimePrecision::Second,
            ts_raw: "100".into(),
            ts_raw_format: TimeFormat::UnixSeconds,
            ts_tz_assumed: None,
            source_inst: inst_id,
            kind: EventKind::WebVisit,
            actor: Some(Actor::app("Brave")),
            subject: "https://bad-domain.com/login".into(),
            subject_host: Some("bad-domain.com".into()),
            summary: "visit bad domain".into(),
            attrs: json!({"remote_ip": "192.168.1.50:443"}),
            provenance: Provenance {
                file: PathBuf::from("/tmp/test"),
                file_sha256: None,
                locator: "1".into(),
            },
            confidence: Confidence::Direct,
        };

        let art = fp_core::Artifact {
            path: PathBuf::from("/tmp/evil.bin"),
            sha256: Some("abcd1234deadbeef".into()),
            size: Some(1024),
            mtime_ns: Some(100),
            ctime_ns: None,
            btime_ns: None,
            owner_pkg: None,
        };

        store
            .ingest(
                run_id,
                &Batch {
                    source_identity: "chromium.history|brave|Default|/home/u/.config/Brave",
                    events: &[ev1],
                    states: &[],
                    artifacts: &[art],
                },
            )
            .unwrap();

        // Insert IOCs
        store
            .insert_ioc("domain", "bad-domain.com", Some("C2 server"))
            .unwrap();
        store
            .insert_ioc("cidr", "192.168.1.0/24", Some("evil subnet"))
            .unwrap();
        store
            .insert_ioc("sha256", "ABCD1234DEADBEEF", Some("malware hash"))
            .unwrap();
        store
            .insert_ioc("path", "/tmp/evil.*", Some("evil temp"))
            .unwrap();

        let hits = check_iocs(&store).unwrap();
        assert!(!hits.is_empty());
        assert!(
            hits.iter()
                .any(|h| h.ioc_type == "domain" && h.target_type == "event")
        );
        assert!(
            hits.iter()
                .any(|h| h.ioc_type == "cidr" && h.target_type == "event")
        );
        assert!(
            hits.iter()
                .any(|h| h.ioc_type == "sha256" && h.target_type == "artifact")
        );
        assert!(
            hits.iter()
                .any(|h| h.ioc_type == "path" && h.target_type == "artifact")
        );

        // Correlate links
        let link_count = correlate_iocs(&mut store).unwrap();
        assert_eq!(link_count, hits.len());

        let links = store.list_links().unwrap();
        assert!(links.iter().any(|l| l.rel == "ioc_match"));

        // Match event IOCs inline
        let iocs = store.list_iocs().unwrap();
        let inline = match_event_iocs(
            Some("bad-domain.com"),
            Some("https://bad-domain.com/login"),
            None,
            &iocs,
        );
        assert_eq!(inline, vec!["bad-domain.com"]);
    }
}
