use crate::ExportEvent;
use anyhow::Result;
use std::io::Write;

pub fn render_jsonl<W: Write>(writer: &mut W, events: &[ExportEvent]) -> Result<()> {
    for ev in events {
        serde_json::to_writer(&mut *writer, ev)?;
        writer.write_all(b"\n")?;
    }
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_jsonl() {
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
            subject: Some("https://zoom.us".into()),
            summary: Some("visit zoom".into()),
            attrs: None,
            locator: None,
            prov_file: None,
            prov_sha256: None,
            confidence: "direct".into(),
            ioc_hits: vec![],
        }];

        let mut buf = Vec::new();
        render_jsonl(&mut buf, &events).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("\"kind\":\"web.visit\""));
        assert!(s.ends_with('\n'));
    }
}
