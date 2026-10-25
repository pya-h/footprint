//! ratatui widgets for each view. Colors come from [`Theme`] so `NO_COLOR` renders plain.

use crate::app::{App, CollectLine, FILTER_FIELDS, View};
use fp_query::{CoverageStatus, DiffChange, TraceArrow, TraceHead, Tz};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, List, ListItem, Paragraph, Row, Table, Wrap};

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub colors: bool,
}

impl Theme {
    fn style(self, color: Color) -> Style {
        if self.colors {
            Style::default().fg(color)
        } else {
            Style::default()
        }
    }

    pub fn accent(self) -> Style {
        self.style(Color::Cyan)
    }

    pub fn highlight(self) -> Style {
        let base = self.style(Color::Black).bg(if self.colors {
            Color::Cyan
        } else {
            Color::Gray
        });
        base.add_modifier(Modifier::BOLD)
    }

    pub fn danger(self) -> Style {
        self.style(Color::Red)
    }

    pub fn warn(self) -> Style {
        self.style(Color::Yellow)
    }

    pub fn ok(self) -> Style {
        self.style(Color::Green)
    }

    pub fn dim(self) -> Style {
        self.style(Color::DarkGray)
    }
}

fn confidence_style(theme: Theme, confidence: Option<&str>) -> Style {
    match confidence {
        Some("direct") => theme.ok(),
        Some("derived") => theme.accent(),
        Some("heuristic") => theme.warn(),
        _ => Style::default(),
    }
}

pub fn draw(frame: &mut Frame, app: &App, theme: Theme) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(frame.area());
    frame.render_widget(Paragraph::new(title_line(app, theme)), chunks[0]);
    match app.view {
        View::Timeline => draw_timeline(frame, app, theme, chunks[1]),
        View::Detail => draw_detail(frame, app, theme, chunks[1]),
        View::Trace => draw_trace(frame, app, theme, chunks[1]),
        View::State => draw_state(frame, app, theme, chunks[1]),
        View::Diff => draw_diff(frame, app, theme, chunks[1]),
        View::Ioc => draw_ioc(frame, app, theme, chunks[1]),
        View::Sources => draw_sources(frame, app, theme, chunks[1]),
        View::Collect => draw_collect(frame, app, theme, chunks[1]),
    }
    frame.render_widget(
        Paragraph::new(app.status.as_str()).style(theme.dim()),
        chunks[2],
    );
    if app.help {
        draw_help(frame, theme);
    }
}

fn title_line(app: &App, theme: Theme) -> Line<'static> {
    let mut tabs: Vec<Span<'static>> = Vec::new();
    for (i, view) in View::ALL.iter().enumerate() {
        let name = format!(" {}:{} ", i + 1, view.title());
        let style = if *view == app.view {
            theme.highlight()
        } else {
            theme.dim()
        };
        tabs.push(Span::styled(name, style));
    }
    let tz = match app.tz {
        Tz::Local => "local",
        Tz::Utc => "UTC",
    };
    tabs.push(Span::styled(
        format!("  t:{tz}  ? help  q quit"),
        theme.dim(),
    ));
    Line::from(tabs)
}

fn draw_timeline(frame: &mut Frame, app: &App, theme: Theme, area: ratatui::layout::Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(area);
    frame.render_widget(Paragraph::new(filter_bar(app, theme)), chunks[0]);

    let items: Vec<ListItem> = app
        .rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let time =
                fp_query::format_time(row.ts_utc, &row.ts_precision, app.tz, "%Y-%m-%d %H:%M:%S");
            let mut spans = vec![
                Span::raw(format!(" {:>6} ", row.id)),
                Span::styled(format!("{time:<19} "), theme.accent()),
                Span::styled(
                    format!("{:<18}", truncate(&row.source_label(), 18)),
                    theme.dim(),
                ),
                Span::raw(format!("{:<20}", truncate(&row.kind, 20))),
            ];
            if !row.ioc_hits.is_empty() {
                spans.push(Span::styled("⚑ ", theme.danger()));
            }
            let mut text = truncate(&row.display_text(), 120);
            if !row.ioc_hits.is_empty() {
                text = format!("{text}  [IOC: {}]", row.ioc_hits.join(", "));
            }
            spans.push(Span::raw(truncate(&text, 140)));
            let item = ListItem::new(Line::from(spans));
            if i == app.selected {
                item.style(theme.highlight())
            } else {
                item
            }
        })
        .collect();
    let more = if app.has_more {
        " (more on scroll)"
    } else {
        ""
    };
    let list = List::new(items).block(Block::default().borders(Borders::NONE).title(format!(
        "timeline{}  i filter · Enter detail · r trace",
        more
    )));
    let mut state = ratatui::widgets::ListState::default();
    state.select(Some(app.selected));
    frame.render_stateful_widget(list, chunks[1], &mut state);
}

fn filter_bar(app: &App, theme: Theme) -> Line<'static> {
    let mut spans = vec![Span::styled("filter ", theme.dim())];
    for (i, field) in FILTER_FIELDS.iter().enumerate() {
        let value = app.draft.get(i);
        let editing = app.editing && app.draft.active == i;
        let label = format!("{field}:");
        spans.push(Span::styled(label, theme.dim()));
        let shown = if value.is_empty() {
            "·".to_string()
        } else {
            value.to_string()
        };
        let style = if editing {
            theme.highlight()
        } else if value.is_empty() {
            theme.dim()
        } else {
            Style::default()
        };
        spans.push(Span::styled(format!("{shown:<16} "), style));
    }
    Line::from(spans)
}

fn draw_detail(frame: &mut Frame, app: &App, theme: Theme, area: ratatui::layout::Rect) {
    let Some(detail) = &app.detail else {
        frame.render_widget(
            Paragraph::new("no event selected — Enter on a timeline row"),
            area,
        );
        return;
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(area);

    let time = fp_query::format_time(
        detail.ts_utc,
        &detail.ts_precision,
        app.tz,
        "%Y-%m-%d %H:%M:%S%.6f %:z",
    );
    let dash = |v: &Option<String>| v.clone().unwrap_or_else(|| "-".into());
    let fields = [
        ("time", time),
        ("precision", detail.ts_precision.clone()),
        (
            "raw time",
            format!("{} ({})", dash(&detail.ts_raw), dash(&detail.ts_raw_format)),
        ),
        ("tz assumed", dash(&detail.ts_tz_assumed)),
        ("kind", detail.kind.clone()),
        (
            "source",
            format!("{} {}", detail.source_kind, detail.source_label()),
        ),
        ("subject", dash(&detail.subject)),
        ("host", dash(&detail.subject_host)),
        ("confidence", detail.confidence.clone()),
        ("file", dash(&detail.prov_file)),
        ("sha256", dash(&detail.prov_sha256)),
        ("locator", dash(&detail.locator)),
        ("summary", dash(&detail.summary)),
    ];
    let mut lines: Vec<Line> = fields
        .iter()
        .map(|(k, v)| {
            Line::from(vec![
                Span::styled(format!("{k:<12}"), theme.dim()),
                Span::raw(truncate(v, 200)),
            ])
        })
        .collect();
    lines.push(Line::from(""));
    lines.push(Line::from(vec![Span::styled("attrs", theme.dim())]));
    if let Some(attrs) = &detail.attrs {
        let pretty = serde_json::to_string_pretty(attrs).unwrap_or_default();
        for l in pretty.lines() {
            lines.push(Line::raw(truncate(l, 220)));
        }
    }
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().title("event").borders(Borders::NONE))
            .wrap(Wrap { trim: false }),
        chunks[0],
    );

    let link_items: Vec<ListItem> = detail
        .links
        .iter()
        .enumerate()
        .map(|(i, link)| {
            let arrow = match link.direction {
                fp_query::LinkDirection::Outgoing => "->",
                fp_query::LinkDirection::Incoming => "<-",
            };
            let label = format!(
                " {:<18} {} {} {}",
                link.rel, arrow, link.target_type, link.target_label
            );
            let item = ListItem::new(Line::from(vec![
                Span::styled(label, confidence_style(theme, Some(&link.confidence))),
                Span::styled(
                    link.why
                        .as_ref()
                        .map(|w| format!("  ({w})"))
                        .unwrap_or_default(),
                    theme.dim(),
                ),
            ]));
            if i == app.detail_link {
                item.style(theme.highlight())
            } else {
                item
            }
        })
        .collect();
    let links = List::new(link_items).block(
        Block::default()
            .title("links — Enter jumps to an event, Esc back, r trace")
            .borders(Borders::NONE),
    );
    let mut state = ratatui::widgets::ListState::default();
    state.select(Some(
        app.detail_link.min(detail.links.len().saturating_sub(1)),
    ));
    frame.render_stateful_widget(links, chunks[1], &mut state);
}

fn draw_trace(frame: &mut Frame, app: &App, theme: Theme, area: ratatui::layout::Rect) {
    let Some(trace) = &app.trace else {
        frame.render_widget(Paragraph::new("no trace — press r on a timeline row"), area);
        return;
    };
    let head = match &trace.head {
        TraceHead::Artifact {
            path, sha256, size, ..
        } => {
            let sha = sha256
                .as_ref()
                .map(|h| format!(" sha256 {}…", &h[..8.min(h.len())]))
                .unwrap_or_default();
            let size = size.map(|s| format!(" {s} B")).unwrap_or_default();
            format!("artifact  {path}{sha}{size}")
        }
        TraceHead::Event {
            id,
            ts_utc,
            source,
            kind,
            subject,
        } => format!(
            "event {id}  {}  {source}  {kind}  {subject}",
            fp_query::format_time(*ts_utc, "exact", app.tz, "%Y-%m-%d %H:%M:%S")
        ),
    };
    let items: Vec<ListItem> = trace
        .rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let arrow = match row.arrow {
                TraceArrow::Back => "◄",
                TraceArrow::Forward => "►",
                TraceArrow::Note => "◄",
            };
            let line = if row.arrow == TraceArrow::Note {
                Line::from(vec![
                    Span::raw(format!(" {arrow} ")),
                    Span::styled(row.text.clone(), theme.dim()),
                ])
            } else {
                let time = row
                    .ts_utc
                    .map(|t| fp_query::format_time(t, "second", app.tz, "%H:%M:%S"))
                    .unwrap_or_else(|| "        ".into());
                Line::from(vec![
                    Span::raw(format!(" {arrow} ")),
                    Span::styled(
                        format!("{:<16} ", row.rel),
                        confidence_style(theme, row.confidence.as_deref()),
                    ),
                    Span::styled(format!("{time} "), theme.accent()),
                    Span::styled(format!("{:<18} ", truncate(&row.source, 18)), theme.dim()),
                    Span::raw(truncate(&row.text, 130)),
                    Span::styled(
                        row.confidence
                            .as_ref()
                            .map(|c| format!("  [{c}]"))
                            .unwrap_or_default(),
                        confidence_style(theme, row.confidence.as_deref()),
                    ),
                ])
            };
            let item = ListItem::new(line);
            if i == app.trace_selected {
                item.style(theme.highlight())
            } else {
                item
            }
        })
        .collect();
    let list = List::new(items).block(
        Block::default()
            .title(format!("trace — {head}"))
            .borders(Borders::NONE),
    );
    let mut state = ratatui::widgets::ListState::default();
    state.select(Some(
        app.trace_selected.min(trace.rows.len().saturating_sub(1)),
    ));
    frame.render_stateful_widget(list, area, &mut state);
}

fn draw_state(frame: &mut Frame, app: &App, theme: Theme, area: ratatui::layout::Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(area);
    let kind = if app.state_editing {
        format!("kind filter: {}▏", app.state_kind_draft)
    } else {
        format!(
            "kind filter: {}  (k to edit)",
            app.state_kind.as_deref().unwrap_or("·")
        )
    };
    frame.render_widget(Paragraph::new(kind), chunks[0]);

    let rows: Vec<Row> = app
        .state_rows
        .iter()
        .enumerate()
        .map(|(i, rec)| {
            let details = fp_query::state_details(rec.parsed.as_ref()).join("; ");
            let flag_style = rec
                .parsed
                .as_ref()
                .and_then(|v| v.get("flags"))
                .and_then(|f| f.as_array())
                .is_some_and(|f| !f.is_empty());
            let mut cells = vec![
                Cell::from(format!("{:>5}", rec.run_id)),
                Cell::from(rec.kind.clone()),
                Cell::from(truncate(&rec.key, 60)),
                Cell::from(truncate(&details, 80)),
            ];
            if flag_style {
                for c in &mut cells {
                    *c = c.clone().style(theme.warn());
                }
            }
            let row = Row::new(cells);
            if i == app.state_selected {
                row.style(theme.highlight())
            } else {
                row
            }
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Length(6),
            Constraint::Length(24),
            Constraint::Percentage(50),
            Constraint::Percentage(40),
        ],
    )
    .block(Block::default().borders(Borders::NONE));
    let mut state = ratatui::widgets::TableState::default();
    state.select(Some(
        app.state_selected
            .min(app.state_rows.len().saturating_sub(1)),
    ));
    frame.render_stateful_widget(table, chunks[1], &mut state);
}

fn draw_diff(frame: &mut Frame, app: &App, theme: Theme, area: ratatui::layout::Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(area);
    let field = |value: &str, editing: bool| {
        if editing {
            Span::styled(format!("{value}▏"), theme.highlight())
        } else {
            Span::raw(value.to_string())
        }
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("run A: "),
            field(&app.diff_a, app.diff_editing && app.diff_field == 0),
            Span::raw("  run B: "),
            field(&app.diff_b, app.diff_editing && app.diff_field == 1),
            Span::raw("  (e edit, Enter compute, Tab switches field)"),
        ])),
        chunks[0],
    );

    let rows: Vec<Row> = app
        .diff_entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let (sign, style) = match e.change {
                DiffChange::Added => ("+", theme.ok()),
                DiffChange::Removed => ("-", theme.danger()),
                DiffChange::Changed => ("~", theme.warn()),
            };
            let row = Row::new(vec![
                Cell::from(sign).style(style),
                Cell::from(e.kind.clone()),
                Cell::from(truncate(&e.key, 70)),
                Cell::from(truncate(&e.detail, 60)),
            ]);
            if i == app.diff_selected {
                row.style(theme.highlight())
            } else {
                row
            }
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Length(3),
            Constraint::Length(24),
            Constraint::Percentage(55),
            Constraint::Percentage(35),
        ],
    )
    .block(Block::default().borders(Borders::NONE));
    let mut state = ratatui::widgets::TableState::default();
    state.select(Some(
        app.diff_selected
            .min(app.diff_entries.len().saturating_sub(1)),
    ));
    frame.render_stateful_widget(table, chunks[1], &mut state);
}

fn draw_ioc(frame: &mut Frame, app: &App, theme: Theme, area: ratatui::layout::Rect) {
    let items: Vec<ListItem> = app
        .ioc_hits
        .iter()
        .enumerate()
        .map(|(i, hit)| {
            let line = Line::from(vec![
                Span::styled(format!("{:<8}", hit.ioc_type), theme.danger()),
                Span::raw(format!(" {:<32} ", truncate(&hit.ioc_value, 32))),
                Span::raw(truncate(&hit.target_desc, 80)),
                Span::styled(format!("  (matched on {})", hit.matched_field), theme.dim()),
                Span::styled(
                    hit.ioc_note
                        .as_ref()
                        .map(|n| format!("  {n}"))
                        .unwrap_or_default(),
                    theme.warn(),
                ),
            ]);
            let item = ListItem::new(line);
            if i == app.ioc_selected {
                item.style(theme.highlight())
            } else {
                item
            }
        })
        .collect();
    let list = List::new(items).block(
        Block::default()
            .title(format!(
                "{} IOC(s) configured, {} hit(s) — Enter opens the event",
                app.iocs.len(),
                app.ioc_hits.len()
            ))
            .borders(Borders::NONE),
    );
    let mut state = ratatui::widgets::ListState::default();
    state.select(Some(
        app.ioc_selected.min(app.ioc_hits.len().saturating_sub(1)),
    ));
    frame.render_stateful_widget(list, area, &mut state);
}

fn draw_sources(frame: &mut Frame, app: &App, theme: Theme, area: ratatui::layout::Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(area);

    let cov_rows: Vec<Row> = app
        .coverage
        .iter()
        .map(|row| {
            let (status, style) = match row.status {
                CoverageStatus::Covered => ("Covered", theme.ok()),
                CoverageStatus::Partial => ("Partial", theme.warn()),
                CoverageStatus::Gap => ("Gap", theme.danger()),
            };
            Row::new(vec![
                Cell::from(row.category.to_string()),
                Cell::from(status).style(style),
                Cell::from(if row.answering.is_empty() {
                    "none".to_string()
                } else {
                    row.answering.join(", ")
                }),
                Cell::from(if row.notes.is_empty() {
                    "-".to_string()
                } else {
                    row.notes.join("; ")
                }),
            ])
        })
        .collect();
    let cov = Table::new(
        cov_rows,
        [
            Constraint::Length(18),
            Constraint::Length(9),
            Constraint::Percentage(45),
            Constraint::Percentage(40),
        ],
    )
    .block(Block::default().title("coverage").borders(Borders::NONE));
    frame.render_widget(cov, chunks[0]);

    let src_rows: Vec<Row> = app
        .sources
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let failed = matches!(s.status, Some(fp_core::Availability::Error(_)));
            let row = Row::new(vec![
                Cell::from(format!("{:>5}", s.id)),
                Cell::from(s.kind.clone()),
                Cell::from(truncate(&s.label(), 24)),
                Cell::from(s.status_display()),
                Cell::from(format!("{:>6}", s.event_count)),
                Cell::from(truncate(s.path.as_deref().unwrap_or("-"), 60)),
                Cell::from(s.note.as_ref().map(|n| truncate(n, 60)).unwrap_or_default()),
            ]);
            let row = if failed {
                row.style(theme.danger())
            } else {
                row
            };
            if i == app.source_selected {
                row.style(theme.highlight())
            } else {
                row
            }
        })
        .collect();
    let src = Table::new(
        src_rows,
        [
            Constraint::Length(6),
            Constraint::Length(18),
            Constraint::Length(24),
            Constraint::Length(24),
            Constraint::Length(7),
            Constraint::Percentage(40),
            Constraint::Percentage(30),
        ],
    )
    .block(
        Block::default()
            .title("sources — all runs")
            .borders(Borders::NONE),
    );
    let mut state = ratatui::widgets::TableState::default();
    state.select(Some(
        app.source_selected.min(app.sources.len().saturating_sub(1)),
    ));
    frame.render_stateful_widget(src, chunks[1], &mut state);
}

fn draw_collect(frame: &mut Frame, app: &App, theme: Theme, area: ratatui::layout::Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(1)])
        .split(area);
    let field = |name: &str, value: &str, active: bool, editing: bool| {
        let shown = if value.is_empty() {
            "·".to_string()
        } else {
            value.to_string()
        };
        let style = if editing && active {
            theme.highlight()
        } else if value.is_empty() {
            theme.dim()
        } else {
            Style::default()
        };
        vec![
            Span::styled(format!("{name}: "), theme.dim()),
            Span::styled(format!("{shown:<20} "), style),
        ]
    };
    let mut form: Vec<Span> = Vec::new();
    for (i, name) in ["since", "until", "sources"].iter().enumerate() {
        form.extend(field(
            name,
            app.collect.field(i),
            app.collect.active_field == i,
            app.collect.editing,
        ));
    }
    form.push(Span::styled(
        if app.collect.running {
            " collecting…".to_string()
        } else {
            " e/Tab edit · Enter start".to_string()
        },
        theme.dim(),
    ));
    frame.render_widget(Paragraph::new(vec![Line::from(form)]), chunks[0]);

    let items: Vec<ListItem> = app
        .collect
        .lines
        .iter()
        .map(|CollectLine { label, outcome }| {
            ListItem::new(Line::from(vec![
                Span::raw(format!("{label}  ")),
                Span::styled(outcome.clone(), theme.dim()),
            ]))
        })
        .collect();
    let list = List::new(items).block(
        Block::default()
            .title("collect progress")
            .borders(Borders::NONE),
    );
    frame.render_widget(list, chunks[1]);
}

fn draw_help(frame: &mut Frame, theme: Theme) {
    let area = centered_rect(70, 80, frame.area());
    let text = vec![
        Line::from(Span::styled("footprint keys", theme.accent())),
        Line::from(""),
        Line::from(
            "  1-8        switch view (timeline detail trace state diff ioc sources collect)",
        ),
        Line::from("  t          toggle local/UTC time"),
        Line::from("  ?          this help    q quit"),
        Line::from(""),
        Line::from(Span::styled("timeline", theme.accent())),
        Line::from("  ↑/↓ PgUp/PgDn  move (loads more pages at the end)"),
        Line::from(
            "  i or /     edit the filter bar: Tab between fields, Enter applies, Esc cancels",
        ),
        Line::from("  Enter      event detail    r trace the row's subject (or id)"),
        Line::from(""),
        Line::from(Span::styled("detail", theme.accent())),
        Line::from("  ↑/↓ links, Enter jumps to a linked event, r trace this event, Esc back"),
        Line::from(""),
        Line::from(Span::styled("trace", theme.accent())),
        Line::from("  ↑/↓ rows, Enter opens the event behind a row, Esc back"),
        Line::from("  edge colors: green direct · blue derived · yellow heuristic"),
        Line::from(""),
        Line::from(Span::styled("state / diff / ioc / sources", theme.accent())),
        Line::from("  k edit kind filter (state) · e edit runs (diff) · Enter opens a hit (ioc)"),
        Line::from(""),
        Line::from(Span::styled("collect", theme.accent())),
        Line::from(
            "  e/Tab edit since/until/sources · Enter starts collection · progress streams here",
        ),
    ];
    frame.render_widget(
        Paragraph::new(text)
            .block(Block::default().title("help").borders(Borders::ALL))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn centered_rect(
    percent_x: u16,
    percent_y: u16,
    area: ratatui::layout::Rect,
) -> ratatui::layout::Rect {
    let popup = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup[1])[1]
}

fn truncate(s: &str, max: usize) -> String {
    // evidence is untrusted: control chars never reach the terminal
    let clean = fp_render::sanitize(s);
    let mut chars = clean.chars();
    let taken: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        format!("{taken}…")
    } else {
        taken
    }
}
