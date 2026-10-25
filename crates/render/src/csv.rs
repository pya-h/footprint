use crate::ExportEvent;
use anyhow::Result;
use std::io::Write;

pub fn escape_csv(val: &str) -> String {
    // a cell starting with = + - @ is a formula to spreadsheets; evidence must not run as one
    let val = if val.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        format!("'{val}")
    } else {
        val.to_string()
    };
    if val.contains(',') || val.contains('"') || val.contains('\n') || val.contains('\r') {
        format!("\"{}\"", val.replace('"', "\"\""))
    } else {
        val
    }
}

pub fn render_csv<W: Write>(writer: &mut W, events: &[ExportEvent]) -> Result<()> {
    writeln!(
        writer,
        "id,time_utc,time_local,source,kind,host,subject,summary,confidence,ioc_hits,provenance_file,provenance_sha256"
    )?;

    for ev in events {
        let source = match (&ev.app, &ev.profile) {
            (Some(a), Some(p)) => format!("{a}/{p}"),
            (Some(a), None) => a.clone(),
            _ => ev.source_kind.clone(),
        };
        let host = ev.host.as_deref().unwrap_or("-");
        let subject = ev.subject.as_deref().unwrap_or("-");
        let summary = ev.summary.as_deref().unwrap_or("-");
        let ioc_hits = if ev.ioc_hits.is_empty() {
            "-".to_string()
        } else {
            ev.ioc_hits.join(";")
        };
        let prov_file = ev.prov_file.as_deref().unwrap_or("-");
        let prov_sha = ev.prov_sha256.as_deref().unwrap_or("-");

        writeln!(
            writer,
            "{},{},{},{},{},{},{},{},{},{},{},{}",
            ev.id,
            ev.ts_utc,
            escape_csv(&ev.ts_local),
            escape_csv(&source),
            escape_csv(&ev.kind),
            escape_csv(host),
            escape_csv(subject),
            escape_csv(summary),
            escape_csv(&ev.confidence),
            escape_csv(&ioc_hits),
            escape_csv(prov_file),
            escape_csv(prov_sha),
        )?;
    }

    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_escape_csv() {
        assert_eq!(escape_csv("hello"), "hello");
        assert_eq!(escape_csv("hello, world"), "\"hello, world\"");
        assert_eq!(escape_csv("quote \"test\""), "\"quote \"\"test\"\"\"");
    }

    #[test]
    fn test_render_csv() {
        let events = vec![ExportEvent {
            id: 1,
            run_id: 1,
            ts_utc: 100,
            ts_local: "2026-10-16 21:00:00".into(),
            source_kind: "web".into(),
            app: Some("brave".into()),
            profile: Some("Default".into()),
            kind: "web.visit".into(),
            host: Some("zoom.us".into()),
            subject: Some("https://zoom.us/test?a=1,2".into()),
            summary: Some("visit \"zoom\"".into()),
            attrs: None,
            locator: None,
            prov_file: None,
            prov_sha256: None,
            confidence: "direct".into(),
            ioc_hits: vec!["zoom.us".into()],
        }];

        let mut buf = Vec::new();
        render_csv(&mut buf, &events).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.starts_with("id,time_utc,time_local"));
        assert!(s.contains("\"visit \"\"zoom\"\"\""));
        assert!(s.contains("\"https://zoom.us/test?a=1,2\""));
    }
}
