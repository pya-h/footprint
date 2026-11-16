//! Session-only story board and named saved views. Nothing here writes to the
//! case database or to `gui.json`.

use std::fmt::Write as _;
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use fp_query::TimelineFilter;

use crate::model::{Selection, View};
use crate::vm::SafeText;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardDraft {
    pub target: Selection,
    pub kind: String,
    pub when: Option<String>,
    pub summary: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardItem {
    pub id: u64,
    pub target: Selection,
    pub kind: SafeText,
    pub when: Option<String>,
    pub summary: SafeText,
    pub detail: SafeText,
    pub note: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BoardExportFormat {
    #[default]
    Markdown,
    Html,
}

impl BoardExportFormat {
    pub fn ext(self) -> &'static str {
        match self {
            Self::Markdown => "md",
            Self::Html => "html",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Markdown => "markdown",
            Self::Html => "html",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Board {
    items: Vec<BoardItem>,
    next_id: u64,
}

impl Board {
    pub fn items(&self) -> &[BoardItem] {
        &self.items
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn contains(&self, target: &Selection) -> bool {
        *target != Selection::None && self.items.iter().any(|item| &item.target == target)
    }

    /// Appends a sanitized item if `target` is not already pinned; returns true when added.
    pub fn pin(&mut self, draft: BoardDraft) -> bool {
        if draft.target == Selection::None || self.contains(&draft.target) {
            return false;
        }
        self.next_id += 1;
        self.items.push(BoardItem {
            id: self.next_id,
            target: draft.target,
            kind: SafeText::new(&draft.kind),
            when: draft.when.map(|w| SafeText::new(&w).as_str().to_string()),
            summary: SafeText::new(&draft.summary),
            detail: SafeText::new(&draft.detail),
            note: String::new(),
        });
        true
    }

    pub fn remove(&mut self, id: u64) -> bool {
        let before = self.items.len();
        self.items.retain(|item| item.id != id);
        self.items.len() != before
    }

    pub fn remove_target(&mut self, target: &Selection) -> bool {
        let before = self.items.len();
        self.items.retain(|item| &item.target != target);
        self.items.len() != before
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }

    pub fn move_item(&mut self, index: usize, delta: isize) -> bool {
        if index >= self.items.len() || delta == 0 {
            return false;
        }
        let Some(next) = index.checked_add_signed(delta) else {
            return false;
        };
        if next >= self.items.len() {
            return false;
        }
        self.items.swap(index, next);
        true
    }

    pub fn set_note(&mut self, id: u64, note: String) {
        if let Some(item) = self.items.iter_mut().find(|item| item.id == id) {
            item.note = note;
        }
    }

    pub fn render_markdown(&self, case_name: &str) -> String {
        let safe_case = SafeText::new(case_name);
        let mut out = format!("# Case Board — {}\n\n", safe_case.as_str());
        if self.items.is_empty() {
            out.push_str("_No pinned items._\n");
            return out;
        }
        for (idx, item) in self.items.iter().enumerate() {
            let when = item.when.as_deref().unwrap_or("untimed");
            let target = target_label(&item.target);
            let _ = writeln!(
                out,
                "{}. **{}** (`{}` · {} · {})",
                idx + 1,
                collapse_newlines(item.summary.as_str()),
                item.kind.as_str(),
                when,
                target
            );
            if !item.detail.is_empty() {
                let _ = writeln!(
                    out,
                    "   - Detail: {}",
                    collapse_newlines(item.detail.as_str())
                );
            }
            let safe_note = SafeText::new(item.note.trim());
            if !safe_note.is_empty() {
                let _ = writeln!(out, "   - Note: {}", safe_note.as_str());
            }
            out.push('\n');
        }
        out
    }

    pub fn render_html(&self, case_name: &str) -> String {
        let safe_case = fp_render::html::escape_html(SafeText::new(case_name).as_str());
        let mut out = format!(
            "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
             <meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; style-src 'unsafe-inline'\">\n\
             <title>Case Board - {safe_case}</title>\n\
             <style>body{{font-family:sans-serif;max-width:900px;margin:2rem auto;padding:0 1rem;color:#1f2328}}\
             .item{{border:1px solid #d0d7de;border-radius:8px;padding:12px 14px;margin:12px 0}}\
             .meta{{color:#59636e;font-size:.85rem;font-family:monospace;margin-bottom:4px}}\
             .summary{{font-weight:600;margin:4px 0}}.detail{{font-family:monospace;font-size:.9rem;color:#424a53}}\
             .note{{margin-top:8px;padding:8px;background:#f6f8fa;border-left:3px solid #0969da;white-space:pre-wrap}}</style>\n\
             </head>\n<body>\n<h1>Case Board - {safe_case}</h1>\n"
        );
        if self.items.is_empty() {
            out.push_str("<p><em>No pinned items.</em></p>\n");
        } else {
            for (idx, item) in self.items.iter().enumerate() {
                let when = fp_render::html::escape_html(item.when.as_deref().unwrap_or("untimed"));
                let kind = fp_render::html::escape_html(item.kind.as_str());
                let target = fp_render::html::escape_html(&target_label(&item.target));
                let summary = fp_render::html::escape_html(item.summary.as_str());
                let detail = fp_render::html::escape_html(item.detail.as_str());
                let safe_note = SafeText::new(item.note.trim());
                let note = fp_render::html::escape_html(safe_note.as_str());
                let _ = write!(
                    out,
                    "<div class=\"item\"><div class=\"meta\">#{} &middot; {kind} &middot; {when} &middot; {target}</div>\
                     <div class=\"summary\">{summary}</div>",
                    idx + 1
                );
                if !detail.is_empty() {
                    let _ = write!(out, "<div class=\"detail\">{detail}</div>");
                }
                if !note.is_empty() {
                    let _ = write!(out, "<div class=\"note\">{note}</div>");
                }
                out.push_str("</div>\n");
            }
        }
        out.push_str("</body>\n</html>\n");
        out
    }

    pub fn write_export(
        &self,
        case_name: &str,
        format: BoardExportFormat,
        path: &Path,
    ) -> Result<usize, String> {
        let body = match format {
            BoardExportFormat::Markdown => self.render_markdown(case_name),
            BoardExportFormat::Html => self.render_html(case_name),
        };
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|e| format!("create dir: {e}"))?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| format!("open {}: {e}", path.display()))?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("chmod 0600 {}: {e}", path.display()))?;
        file.write_all(body.as_bytes())
            .map_err(|e| format!("write {}: {e}", path.display()))?;
        Ok(self.items.len())
    }
}

pub fn target_label(target: &Selection) -> String {
    match target {
        Selection::None => "none".to_string(),
        Selection::Event(id) => format!("event #{id}"),
        Selection::State(id) => format!("state #{id}"),
        Selection::Ioc(id) => format!("ioc #{id}"),
        Selection::Source(id) => format!("source #{id}"),
        Selection::TraceNode(id) => format!("trace #{id}"),
    }
}

pub fn view_for_selection(target: &Selection) -> View {
    match target {
        Selection::State(_) => View::State,
        Selection::Ioc(_) => View::Ioc,
        Selection::Source(_) => View::Sources,
        Selection::Event(_) | Selection::TraceNode(_) | Selection::None => View::Timeline,
    }
}

fn collapse_newlines(text: &str) -> String {
    text.replace(['\r', '\n'], " ")
}

#[derive(Debug, Clone, PartialEq)]
pub struct SavedView {
    pub id: u64,
    pub name: String,
    pub view: View,
    pub filter: TimelineFilter,
}

#[derive(Debug, Clone, Default)]
pub struct SavedViews {
    views: Vec<SavedView>,
    next_id: u64,
}

impl SavedViews {
    pub fn list(&self) -> &[SavedView] {
        &self.views
    }

    pub fn is_empty(&self) -> bool {
        self.views.is_empty()
    }

    pub fn len(&self) -> usize {
        self.views.len()
    }

    /// Saves a session view under `name` (replacing an existing view with the same name).
    pub fn save(&mut self, name: String, view: View, filter: TimelineFilter) -> Option<u64> {
        let trimmed = SafeText::new(name.trim()).as_str().trim().to_string();
        if trimmed.is_empty() {
            return None;
        }
        if let Some(existing) = self.views.iter_mut().find(|v| v.name == trimmed) {
            existing.view = view;
            existing.filter = filter;
            return Some(existing.id);
        }
        self.next_id += 1;
        let id = self.next_id;
        self.views.push(SavedView {
            id,
            name: trimmed,
            view,
            filter,
        });
        Some(id)
    }

    pub fn get(&self, id: u64) -> Option<&SavedView> {
        self.views.iter().find(|v| v.id == id)
    }

    pub fn rename(&mut self, id: u64, new_name: String) -> bool {
        let trimmed = SafeText::new(new_name.trim()).as_str().trim().to_string();
        if trimmed.is_empty() {
            return false;
        }
        if self.views.iter().any(|v| v.id != id && v.name == trimmed) {
            return false;
        }
        if let Some(sv) = self.views.iter_mut().find(|v| v.id == id) {
            sv.name = trimmed;
            return true;
        }
        false
    }

    pub fn remove(&mut self, id: u64) -> bool {
        let before = self.views.len();
        self.views.retain(|v| v.id != id);
        self.views.len() != before
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn sample_draft(id: u64, summary: &str) -> BoardDraft {
        BoardDraft {
            target: Selection::Event(id),
            kind: "web.visit".to_string(),
            when: Some("2026-10-16 20:42:00".to_string()),
            summary: summary.to_string(),
            detail: "us09-web-zoom.com".to_string(),
        }
    }

    #[test]
    fn board_pins_dedups_reorders_and_sanitizes() {
        let mut board = Board::default();
        assert!(board.pin(sample_draft(10, "evil\u{202e}fdp.exe")));
        assert!(!board.pin(sample_draft(10, "duplicate ignored")));
        assert!(!board.pin(BoardDraft {
            target: Selection::None,
            ..sample_draft(0, "none ignored")
        }));
        assert!(board.pin(sample_draft(20, "second")));
        assert_eq!(board.len(), 2);

        // Move item 1 up to slot 0, then verify note + bounds clamping.
        assert!(!board.move_item(0, -1));
        assert!(board.move_item(1, -1));
        assert_eq!(board.items()[0].target, Selection::Event(20));
        assert_eq!(board.items()[1].summary.as_str(), "evilfdp.exe");

        let id = board.items()[1].id;
        board.set_note(id, "<script>alert(1)</script>\u{1b}[31m".to_string());
        let md = board.render_markdown("case.db");
        assert!(!md.contains('\u{202e}'));
        assert!(!md.contains('\u{1b}'));
        assert!(md.contains("evilfdp.exe"));

        let html = board.render_html("case.db");
        assert!(html.contains("default-src 'none'"));
        assert!(!html.contains("<script>"));
        assert!(html.contains("&lt;script&gt;"));

        let dir = tempfile::tempdir().expect("tempdir");
        let out = dir.path().join("board.html");
        let count = board
            .write_export("case.db", BoardExportFormat::Html, &out)
            .expect("write export");
        assert_eq!(count, 2);
        let mode = std::fs::metadata(&out).expect("stat").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn saved_views_save_replace_rename_and_remove() {
        let mut views = SavedViews::default();
        let f1 = TimelineFilter {
            hosts: vec!["zoom.com".to_string()],
            ..Default::default()
        };
        let id1 = views
            .save(" Zoom hosts ".to_string(), View::Timeline, f1)
            .expect("saved");
        assert_eq!(views.list()[0].name, "Zoom hosts");

        // Saving under the same name updates in place.
        let f2 = TimelineFilter {
            ioc_only: true,
            ..Default::default()
        };
        let id1_again = views
            .save("Zoom hosts".to_string(), View::Hosts, f2.clone())
            .expect("replaced");
        assert_eq!(id1, id1_again);
        assert_eq!(views.len(), 1);
        assert_eq!(views.list()[0].view, View::Hosts);
        assert!(views.list()[0].filter.ioc_only);

        assert!(views.rename(id1, "IOC Hosts".to_string()));
        assert_eq!(views.list()[0].name, "IOC Hosts");
        assert!(views.remove(id1));
        assert!(views.is_empty());
    }
}
