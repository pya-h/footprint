use crate::ExportReport;
use anyhow::Result;
use std::io::Write;

pub fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

pub fn render_html<W: Write>(writer: &mut W, report: &ExportReport) -> Result<()> {
    let title = format!(
        "footprint Forensic Report - {}",
        escape_html(&report.case_path)
    );
    let total_events = report.events.len();
    let total_iocs = report.ioc_hits.len();
    let events_with_iocs = report
        .events
        .iter()
        .filter(|e| !e.ioc_hits.is_empty())
        .count();

    // Unique hosts
    let mut unique_hosts = std::collections::HashSet::new();
    for e in &report.events {
        if let Some(ref h) = e.host {
            unique_hosts.insert(h.as_str());
        }
    }
    let total_hosts = unique_hosts.len();

    let (time_start, time_end) = if let (Some(min_ev), Some(max_ev)) = (
        report.events.iter().min_by_key(|e| e.ts_utc),
        report.events.iter().max_by_key(|e| e.ts_utc),
    ) {
        (min_ev.ts_local.as_str(), max_ev.ts_local.as_str())
    } else {
        ("-", "-")
    };

    writeln!(
        writer,
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<style>
:root {{
  --bg-primary: #0d1117;
  --bg-secondary: #161b22;
  --bg-tertiary: #21262d;
  --border-color: #30363d;
  --text-primary: #e6edf3;
  --text-secondary: #8b949e;
  --accent: #58a6ff;
  --danger: #f85149;
  --success: #3fb950;
  --warning: #d29922;
  --purple: #bc8cff;
  --orange: #f0883e;
  --cyan: #39c5cf;
}}
* {{ box-sizing: border-box; margin: 0; padding: 0; }}
body {{
  font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif;
  background-color: var(--bg-primary);
  color: var(--text-primary);
  line-height: 1.5;
  padding: 24px;
}}
.container {{ max-width: 1400px; margin: 0 auto; }}
header {{
  display: flex;
  justify-content: space-between;
  align-items: center;
  border-bottom: 1px solid var(--border-color);
  padding-bottom: 20px;
  margin-bottom: 24px;
}}
h1 {{ font-size: 24px; font-weight: 600; display: flex; align-items: center; gap: 10px; }}
.badge-brand {{ background: #238636; color: #fff; font-size: 12px; padding: 2px 8px; border-radius: 12px; }}
.meta-tag {{ color: var(--text-secondary); font-size: 14px; }}

.metrics-grid {{
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
  gap: 16px;
  margin-bottom: 24px;
}}
.metric-card {{
  background: var(--bg-secondary);
  border: 1px solid var(--border-color);
  border-radius: 8px;
  padding: 16px;
}}
.metric-value {{ font-size: 28px; font-weight: 700; color: var(--text-primary); }}
.metric-label {{ font-size: 13px; color: var(--text-secondary); text-transform: uppercase; letter-spacing: 0.5px; }}

.alert-card {{
  background: rgba(248, 81, 73, 0.1);
  border: 1px solid var(--danger);
  border-radius: 8px;
  padding: 16px;
  margin-bottom: 24px;
}}
.alert-title {{ color: var(--danger); font-weight: 600; font-size: 16px; margin-bottom: 8px; display: flex; align-items: center; gap: 8px; }}

.controls {{
  display: flex;
  flex-wrap: wrap;
  gap: 12px;
  margin-bottom: 16px;
  align-items: center;
}}
.search-input {{
  flex: 1;
  min-width: 280px;
  background: var(--bg-secondary);
  border: 1px solid var(--border-color);
  color: var(--text-primary);
  padding: 10px 14px;
  border-radius: 6px;
  font-size: 14px;
  outline: none;
}}
.search-input:focus {{ border-color: var(--accent); }}
.filter-btn {{
  background: var(--bg-secondary);
  border: 1px solid var(--border-color);
  color: var(--text-secondary);
  padding: 8px 14px;
  border-radius: 6px;
  cursor: pointer;
  font-size: 13px;
  font-weight: 500;
  transition: all 0.15s ease;
}}
.filter-btn:hover, .filter-btn.active {{
  background: var(--bg-tertiary);
  color: var(--text-primary);
  border-color: var(--accent);
}}

table {{
  width: 100%;
  border-collapse: collapse;
  background: var(--bg-secondary);
  border: 1px solid var(--border-color);
  border-radius: 8px;
  overflow: hidden;
  font-size: 13px;
}}
th {{
  background: var(--bg-tertiary);
  color: var(--text-secondary);
  text-align: left;
  padding: 12px 14px;
  font-weight: 600;
  border-bottom: 1px solid var(--border-color);
}}
td {{
  padding: 10px 14px;
  border-bottom: 1px solid var(--border-color);
  vertical-align: top;
}}
tr.event-row {{ cursor: pointer; }}
tr.event-row:hover {{ background: rgba(88, 166, 255, 0.05); }}
tr.ioc-flagged {{ background: rgba(248, 81, 73, 0.07); }}
tr.ioc-flagged:hover {{ background: rgba(248, 81, 73, 0.12); }}

.badge {{
  display: inline-block;
  padding: 2px 7px;
  border-radius: 10px;
  font-size: 11px;
  font-weight: 600;
  text-transform: uppercase;
}}
.badge-web {{ background: rgba(88, 166, 255, 0.2); color: var(--accent); }}
.badge-pkg {{ background: rgba(63, 185, 80, 0.2); color: var(--success); }}
.badge-priv {{ background: rgba(188, 140, 255, 0.2); color: var(--purple); }}
.badge-dl {{ background: rgba(240, 136, 62, 0.2); color: var(--orange); }}
.badge-file {{ background: rgba(57, 197, 207, 0.2); color: var(--cyan); }}
.badge-shell {{ background: rgba(210, 153, 34, 0.2); color: var(--warning); }}
.badge-ioc {{ background: var(--danger); color: #fff; font-weight: 700; }}

.details-row {{ display: none; background: var(--bg-tertiary); }}
.details-content {{
  padding: 14px;
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(320px, 1fr));
  gap: 16px;
  font-family: monospace;
  font-size: 12px;
}}
.details-box {{ background: var(--bg-secondary); padding: 10px; border-radius: 6px; border: 1px solid var(--border-color); }}
.details-box pre {{ white-space: pre-wrap; word-break: break-all; color: var(--text-primary); }}
.box-title {{ color: var(--text-secondary); font-size: 11px; margin-bottom: 6px; font-weight: 600; text-transform: uppercase; }}

footer {{
  margin-top: 40px;
  text-align: center;
  color: var(--text-secondary);
  font-size: 12px;
  border-top: 1px solid var(--border-color);
  padding-top: 20px;
}}
</style>
</head>
<body>
<div class="container">
<header>
  <div>
    <h1><span>footprint</span> <span class="badge-brand">Report</span></h1>
    <div class="meta-tag">Case: {}</div>
  </div>
  <div class="meta-tag" style="text-align: right;">
    Generated: {}<br>
    Span: {} &rarr; {}
  </div>
</header>

<div class="metrics-grid">
  <div class="metric-card">
    <div class="metric-value">{}</div>
    <div class="metric-label">Total Events</div>
  </div>
  <div class="metric-card">
    <div class="metric-value" style="color: {};">{}</div>
    <div class="metric-label">IOC Hits ({})</div>
  </div>
  <div class="metric-card">
    <div class="metric-value">{}</div>
    <div class="metric-label">Unique Hosts</div>
  </div>
  <div class="metric-card">
    <div class="metric-value">{}</div>
    <div class="metric-label">Evidence Runs</div>
  </div>
</div>
"#,
        escape_html(&report.case_path),
        escape_html(
            &chrono::Utc::now()
                .format("%Y-%m-%d %H:%M:%S UTC")
                .to_string()
        ),
        escape_html(time_start),
        escape_html(time_end),
        total_events,
        if total_iocs > 0 {
            "var(--danger)"
        } else {
            "var(--text-primary)"
        },
        total_iocs,
        events_with_iocs,
        total_hosts,
        report.runs.len(),
    )?;

    // Alert section for IOC hits
    if !report.ioc_hits.is_empty() {
        writeln!(
            writer,
            r#"<div class="alert-card">
  <div class="alert-title">&excl; Indicator of Compromise - IOC Detections ({} Matches)</div>
  <table style="background: transparent; border: none;">
    <thead>
      <tr>
        <th style="background: rgba(0,0,0,0.2);">IOC Type</th>
        <th style="background: rgba(0,0,0,0.2);">IOC Value</th>
        <th style="background: rgba(0,0,0,0.2);">Target</th>
        <th style="background: rgba(0,0,0,0.2);">Matched Field</th>
        <th style="background: rgba(0,0,0,0.2);">Note</th>
      </tr>
    </thead>
    <tbody>"#,
            report.ioc_hits.len()
        )?;

        for hit in &report.ioc_hits {
            writeln!(
                writer,
                r#"<tr>
  <td><span class="badge badge-ioc">{}</span></td>
  <td><code>{}</code></td>
  <td>{}</td>
  <td><code>{}: {}</code></td>
  <td>{}</td>
</tr>"#,
                escape_html(&hit.ioc_type),
                escape_html(&hit.ioc_value),
                escape_html(&hit.target_desc),
                escape_html(&hit.matched_field),
                escape_html(&hit.matched_value),
                escape_html(hit.ioc_note.as_deref().unwrap_or("-")),
            )?;
        }

        writeln!(writer, r#"</tbody></table></div>"#)?;
    }

    // Controls and timeline table
    writeln!(
        writer,
        r#"<div class="controls">
  <input type="text" id="searchInput" class="search-input" placeholder="Search timeline by ID, kind, host, subject, summary, or IOC...">
  <button class="filter-btn active" data-filter="all">All ({})</button>
  <button class="filter-btn" data-filter="web">Web</button>
  <button class="filter-btn" data-filter="dl">Downloads</button>
  <button class="filter-btn" data-filter="pkg">Packages</button>
  <button class="filter-btn" data-filter="priv">Privilege</button>
  <button class="filter-btn" data-filter="file">Files</button>
  <button class="filter-btn" data-filter="shell">Shell</button>
  <button class="filter-btn" data-filter="ioc" style="color: var(--danger);">IOC Hits ({})</button>
</div>

<table id="eventsTable">
  <thead>
    <tr>
      <th style="width: 70px;">ID</th>
      <th style="width: 170px;">Time (Local)</th>
      <th style="width: 150px;">Source</th>
      <th style="width: 130px;">Kind</th>
      <th>Subject / Summary</th>
      <th style="width: 90px;">Confidence</th>
    </tr>
  </thead>
  <tbody>"#,
        total_events, events_with_iocs
    )?;

    for ev in &report.events {
        let is_ioc = !ev.ioc_hits.is_empty();
        let ioc_class = if is_ioc { " ioc-flagged" } else { "" };
        let kind_badge_class = if ev.kind.starts_with("web.") {
            "badge-web"
        } else if ev.kind.starts_with("pkg.") {
            "badge-pkg"
        } else if ev.kind.starts_with("priv.") {
            "badge-priv"
        } else if ev.kind.starts_with("dl.") {
            "badge-dl"
        } else if ev.kind.starts_with("file.") {
            "badge-file"
        } else if ev.kind.starts_with("shell.") {
            "badge-shell"
        } else {
            "badge-web"
        };

        let source = match (&ev.app, &ev.profile) {
            (Some(a), Some(p)) => format!("{a}/{p}"),
            (Some(a), None) => a.clone(),
            _ => ev.source_kind.clone(),
        };

        let summary_text = ev
            .summary
            .as_deref()
            .or(ev.subject.as_deref())
            .unwrap_or("-");

        let ioc_badge_html = if is_ioc {
            format!(
                r#" <span class="badge badge-ioc">IOC: {}</span>"#,
                escape_html(&ev.ioc_hits.join(", "))
            )
        } else {
            String::new()
        };

        let attrs_pretty = ev
            .attrs
            .as_ref()
            .map(|a| serde_json::to_string_pretty(a).unwrap_or_default())
            .unwrap_or_else(|| "{}".to_string());

        writeln!(
            writer,
            r#"<tr class="event-row{ioc_class}" data-kind="{}" data-ioc="{}">
  <td>#{}</td>
  <td><code>{}</code></td>
  <td>{}</td>
  <td><span class="badge {}">{}</span></td>
  <td>{}{ioc_badge_html}</td>
  <td><span class="meta-tag">{}</span></td>
</tr>
<tr class="details-row">
  <td colspan="6">
    <div class="details-content">
      <div class="details-box">
        <div class="box-title">Subject &amp; Host</div>
        <div><strong>Host:</strong> {}</div>
        <div><strong>Subject:</strong> <code>{}</code></div>
        <div style="margin-top: 8px;"><strong>Provenance:</strong> {}</div>
        <div><strong>SHA256:</strong> <code>{}</code></div>
        <div><strong>Locator:</strong> {}</div>
      </div>
      <div class="details-box">
        <div class="box-title">Attributes JSON</div>
        <pre>{}</pre>
      </div>
    </div>
  </td>
</tr>"#,
            escape_html(&ev.kind),
            if is_ioc { "true" } else { "false" },
            ev.id,
            escape_html(&ev.ts_local),
            escape_html(&source),
            kind_badge_class,
            escape_html(&ev.kind),
            escape_html(summary_text),
            escape_html(&ev.confidence),
            escape_html(ev.host.as_deref().unwrap_or("-")),
            escape_html(ev.subject.as_deref().unwrap_or("-")),
            escape_html(ev.prov_file.as_deref().unwrap_or("-")),
            escape_html(ev.prov_sha256.as_deref().unwrap_or("-")),
            escape_html(ev.locator.as_deref().unwrap_or("-")),
            escape_html(&attrs_pretty),
        )?;
    }

    writeln!(
        writer,
        r#"  </tbody>
</table>

<footer>
  Generated by <strong>footprint</strong> &bull; Read-only Linux timeline and provenance engine &bull; Mode 0600
</footer>
</div>

<script>
document.addEventListener("DOMContentLoaded", () => {{
  const searchInput = document.getElementById("searchInput");
  const filterBtns = document.querySelectorAll(".filter-btn");
  const table = document.getElementById("eventsTable");
  const eventRows = table.querySelectorAll("tbody tr.event-row");

  let currentFilter = "all";

  function applyFilters() {{
    const query = searchInput.value.toLowerCase().trim();

    eventRows.forEach(row => {{
      const nextRow = row.nextElementSibling;
      const kind = row.getAttribute("data-kind") || "";
      const isIoc = row.getAttribute("data-ioc") === "true";
      const text = (row.innerText + " " + (nextRow ? nextRow.innerText : "")).toLowerCase();

      let matchesFilter = true;
      if (currentFilter === "ioc") {{
        matchesFilter = isIoc;
      }} else if (currentFilter !== "all") {{
        matchesFilter = kind.startsWith(currentFilter);
      }}

      const matchesSearch = !query || text.includes(query);

      if (matchesFilter && matchesSearch) {{
        row.style.display = "";
      }} else {{
        row.style.display = "none";
        if (nextRow && nextRow.classList.contains("details-row")) {{
          nextRow.style.display = "none";
        }}
      }}
    }});
  }}

  searchInput.addEventListener("input", applyFilters);

  filterBtns.forEach(btn => {{
    btn.addEventListener("click", () => {{
      filterBtns.forEach(b => b.classList.remove("active"));
      btn.classList.add("active");
      currentFilter = btn.getAttribute("data-filter");
      applyFilters();
    }});
  }});

  eventRows.forEach(row => {{
    row.addEventListener("click", () => {{
      const details = row.nextElementSibling;
      if (details && details.classList.contains("details-row")) {{
        const isVisible = window.getComputedStyle(details).display !== "none";
        details.style.display = isVisible ? "none" : "table-row";
      }}
    }});
  }});
}});
</script>
</body>
</html>"#
    )?;

    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ExportEvent;

    #[test]
    fn test_render_html() {
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
            attrs: Some(serde_json::json!({"visit_id": 1})),
            locator: Some("1".into()),
            prov_file: Some("/tmp/History".into()),
            prov_sha256: None,
            confidence: "direct".into(),
            ioc_hits: vec!["zoom.us".into()],
        }];

        let report = ExportReport {
            case_path: "/tmp/case.db".into(),
            runs: vec![],
            ioc_hits: vec![],
            events,
        };

        let mut buf = Vec::new();
        render_html(&mut buf, &report).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("<!DOCTYPE html>"));
        assert!(s.contains("footprint Forensic Report"));
        assert!(s.contains("IOC: zoom.us"));
        assert!(s.contains("visit_id"));
    }
}
