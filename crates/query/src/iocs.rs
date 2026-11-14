use anyhow::{Result, bail};
use fp_store::{IocHitLink, IocRow, Store};

pub const IOC_TYPES: &[&str] = &["domain", "ip", "cidr", "sha256", "path", "url"];

/// One configured IOC and how many `ioc_match` links point at it. Counts come from the
/// last correlation pass, so they can be stale after an interrupted write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IocSummary {
    pub ioc: IocRow,
    pub hits: u64,
}

/// Every configured IOC with its link-based hit count (0 for IOCs with no hits).
pub fn ioc_summary(store: &Store) -> Result<Vec<IocSummary>> {
    let hits: std::collections::HashMap<u64, u64> = store.ioc_hit_counts()?.into_iter().collect();
    Ok(store
        .list_iocs()?
        .into_iter()
        .map(|ioc| {
            let count = hits.get(&ioc.id).copied().unwrap_or(0);
            IocSummary { ioc, hits: count }
        })
        .collect())
}

/// One page of `ioc_match` hits, `None` meaning every IOC. Links only carry target ids;
/// resolving a target's details is a separate lookup by `target_type`/`target_id`.
pub fn ioc_hits_page(
    store: &Store,
    ioc_id: Option<u64>,
    offset: usize,
    limit: usize,
) -> Result<Vec<IocHitLink>> {
    Ok(store.ioc_hits_page(ioc_id, offset, limit)?)
}

/// Validates the type, inserts the IOC and rebuilds the IOC pass so its hits exist before
/// the command returns. Adding an existing `(type, value)` keeps its id.
pub fn add_ioc(store: &mut Store, ioc_type: &str, value: &str, note: Option<&str>) -> Result<u64> {
    let ioc_type = ioc_type.to_lowercase();
    if !IOC_TYPES.contains(&ioc_type.as_str()) {
        bail!(
            "unknown ioc type `{ioc_type}`; expected one of {}",
            IOC_TYPES.join(", ")
        );
    }
    let id = store.insert_ioc(&ioc_type, value, note)?;
    fp_correlate::correlate_iocs(store)?;
    Ok(id)
}

/// Imports one IOC per non-empty, non-`#` line, then rebuilds the IOC pass once.
/// Lines are either `type value [note]`, comma-separated, or a bare value whose type
/// is auto-detected. Returns how many lines were imported.
pub fn import_iocs(store: &mut Store, content: &str) -> Result<usize> {
    let mut count = 0;
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (ioc_type, value, note) = parse_ioc_line(line);
        if !value.is_empty() {
            store.insert_ioc(&ioc_type, &value, note.as_deref())?;
            count += 1;
        }
    }
    fp_correlate::correlate_iocs(store)?;
    Ok(count)
}

pub fn parse_ioc_line(line: &str) -> (String, String, Option<String>) {
    // a comment starts at a `#` after whitespace; a URL's own `#fragment` is part of the value
    let comment = line
        .char_indices()
        .find(|&(i, c)| c == '#' && (i == 0 || line[..i].ends_with(char::is_whitespace)))
        .map(|(i, _)| i);
    let (body, note) = match comment {
        Some(i) => (line[..i].trim(), Some(line[i + 1..].trim().to_string())),
        None => (line.trim(), None),
    };

    let parts: Vec<&str> = if body.contains(',') {
        body.split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect()
    } else {
        body.split_whitespace().collect()
    };

    if parts.len() >= 2 && IOC_TYPES.contains(&parts[0].to_lowercase().as_str()) {
        let ioc_type = parts[0].to_lowercase();
        let val = parts[1].to_string();
        let note = if parts.len() > 2 {
            Some(parts[2..].join(" "))
        } else {
            note
        };
        (ioc_type, val, note)
    } else {
        let val = parts.first().copied().unwrap_or(body);
        let ioc_type = detect_ioc_type(val);
        (ioc_type.to_string(), val.to_string(), note)
    }
}

pub fn detect_ioc_type(val: &str) -> &'static str {
    if val.len() == 64 && val.chars().all(|c| c.is_ascii_hexdigit()) {
        "sha256"
    } else if val.contains("://") {
        // before the glob check: a URL's query has a `?`
        "url"
    } else if val.starts_with('/')
        || val.starts_with("./")
        || val.contains('*')
        || val.contains('?')
    {
        "path"
    } else if val.contains('/')
        && val
            .split('/')
            .next()
            .is_some_and(|ip| ip.parse::<std::net::IpAddr>().is_ok())
    {
        "cidr"
    } else if val.parse::<std::net::IpAddr>().is_ok() {
        "ip"
    } else if val.ends_with(".deb")
        || val.ends_with(".rpm")
        || val.ends_with(".tar.gz")
        || val.ends_with(".zip")
        || val.ends_with(".sh")
        || val.ends_with(".service")
        || val.ends_with(".desktop")
        || val.ends_with(".so")
        || val.ends_with(".exe")
    {
        "path"
    } else {
        "domain"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fp_core::{Actor, Confidence, Event, EventKind, Provenance, SourceInstance, TimePrecision};
    use fp_store::Batch;
    use serde_json::json;
    use std::path::PathBuf;

    #[test]
    fn detects_types_and_keeps_url_fragments() {
        assert_eq!(detect_ioc_type("us09-web-zoom.com"), "domain");
        assert_eq!(detect_ioc_type("192.168.1.1"), "ip");
        assert_eq!(detect_ioc_type("192.168.1.0/24"), "cidr");
        assert_eq!(detect_ioc_type("/tmp/evil.sh"), "path");
        assert_eq!(detect_ioc_type("https://evil.example/dl?id=1"), "url");

        assert_eq!(
            parse_ioc_line("url https://evil.example/#token  # phishing kit"),
            (
                "url".to_string(),
                "https://evil.example/#token".to_string(),
                Some("phishing kit".to_string())
            )
        );
        assert_eq!(
            parse_ioc_line("evil.example # c2"),
            (
                "domain".to_string(),
                "evil.example".to_string(),
                Some("c2".to_string())
            )
        );
    }

    fn case_with_event() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open_or_create(dir.path().join("case.db")).unwrap();
        let run_id = store
            .insert_run(&fp_store::NewRun {
                host: "h",
                user: "u",
                uid: 1000,
                euid: 1000,
                version: "0",
                tz: "UTC",
                args: "",
            })
            .unwrap();
        let inst_id = store
            .insert_source(
                run_id,
                &SourceInstance::new("chromium.history", "Brave", None, PathBuf::from("/h")),
            )
            .unwrap();
        store
            .ingest(
                run_id,
                &Batch {
                    source_identity: "chromium.history|Brave|/h",
                    events: &[Event {
                        run_id,
                        source_kind: "chromium.history".into(),
                        ts_utc: 100,
                        ts_precision: TimePrecision::Second,
                        ts_raw: "100".into(),
                        ts_raw_format: fp_core::time::TimeFormat::UnixSeconds,
                        ts_tz_assumed: None,
                        source_inst: inst_id,
                        kind: EventKind::WebVisit,
                        actor: Some(Actor::app("Brave")),
                        subject: "https://us09-web-zoom.com/j/1".into(),
                        subject_host: Some("us09-web-zoom.com".into()),
                        summary: "visit".into(),
                        attrs: json!({}),
                        provenance: Provenance {
                            file: PathBuf::from("/h/History"),
                            file_sha256: None,
                            locator: "1".into(),
                        },
                        confidence: Confidence::Direct,
                    }],
                    states: &[],
                    artifacts: &[],
                },
            )
            .unwrap();
        (dir, store)
    }

    #[test]
    fn add_import_and_summary_round_trip() {
        let (_dir, mut store) = case_with_event();

        let id = add_ioc(&mut store, "DOMAIN", "us09-web-zoom.com", Some("lure")).unwrap();
        let summary = ioc_summary(&store).unwrap();
        assert_eq!(summary.len(), 1);
        assert_eq!(summary[0].ioc.id, id);
        assert_eq!(summary[0].hits, 1);

        // adding the same (type, value) keeps its id and does not double the hits
        let again = add_ioc(&mut store, "domain", "us09-web-zoom.com", None).unwrap();
        assert_eq!(again, id);
        assert_eq!(ioc_summary(&store).unwrap()[0].hits, 1);

        // one miss and one more hit, imported from lines
        let count = import_iocs(
            &mut store,
            "page-redirect.com # first hop\n203.0.113.7 c2 ip\n\n# a comment\n",
        )
        .unwrap();
        assert_eq!(count, 2);
        let summary = ioc_summary(&store).unwrap();
        assert_eq!(summary.len(), 3);
        assert!(
            summary
                .iter()
                .any(|s| s.ioc.ioc_type == "ip" && s.ioc.value == "203.0.113.7")
        );

        let hits = ioc_hits_page(&store, Some(id), 0, 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(
            (hits[0].target_type.as_str(), hits[0].target_id),
            ("event", 1)
        );
        assert!(hits[0].why.as_deref().unwrap().contains("matched on"));

        let unknown = add_ioc(&mut store, "asn", "64496", None);
        assert!(unknown.is_err());
    }
}
