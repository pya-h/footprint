use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use fp_store::Store;
use std::path::{Path, PathBuf};
use std::sync::mpsc::channel;

#[derive(Parser)]
#[command(
    name = "footprint",
    version,
    about = "Local, read-only activity timeline and provenance for Linux"
)]
struct Cli {
    /// case file
    #[arg(short, long, global = true, default_value = "footprint.db")]
    case: PathBuf,
    /// render times in this zone (`local`, `UTC`, `+03:30`)
    #[arg(long, global = true, default_value = "local")]
    tz: String,
    #[command(subcommand)]
    cmd: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Discover sources and collect their events into the case
    Collect(CollectArgs),
    /// Sources seen by a run, with their status and errors
    Sources {
        /// a run id; defaults to the last run
        #[arg(long, conflicts_with = "all_runs")]
        run: Option<u64>,
        /// sources of every run, not just one
        #[arg(long)]
        all_runs: bool,
        /// show coverage report across evidence categories
        #[arg(long)]
        coverage: bool,
    },
    /// Merged timeline
    List(ListArgs),
    /// One event with its provenance
    Show { id: u64 },
    /// Full-text search over subjects, summaries and attributes (FTS5 syntax)
    Search { query: String },
    /// Walk provenance links backwards and forwards from an artifact path, URL, or event ID
    Trace { target: String },
    /// Captured state records (permissions, persistence, sockets)
    State(StateArgs),
    /// Diff state records between two runs
    Diff(DiffArgs),
    /// Manage and match Indicators of Compromise (domain, IP, CIDR, sha256, path, url)
    Ioc(IocArgs),
    /// Export timeline events to JSONL, CSV, or standalone HTML report
    Export(ExportArgs),
    /// Explain a scanner finding: known false positives and the proof
    Explain { finding: String },
    /// Interactive terminal UI (default: the case at --case)
    Tui,
}

#[derive(Args)]
struct CollectArgs {
    /// start of the window (`2026-10-16T20:40`, `20:40`, `-24h`, `yesterday`, `@epoch`)
    #[arg(short, long, allow_hyphen_values = true)]
    since: Option<String>,
    /// end of the window, in the same forms as `--since`
    #[arg(short, long, allow_hyphen_values = true)]
    until: Option<String>,
    /// source kinds or families to read (`chromium`, `pkg`, `shell.history`…)
    #[arg(long, value_delimiter = ',', default_value = "all")]
    sources: Vec<String>,
    /// source kinds or families to skip
    #[arg(long, value_delimiter = ',')]
    exclude: Vec<String>,
    /// offset for logs that store local time (`+03:30`, `UTC`); default: the host zone at each date
    #[arg(long)]
    assume_tz: Option<String>,
    /// home directory to read user sources from (can be repeated)
    #[arg(long = "home", action = clap::ArgAction::Append)]
    home: Vec<PathBuf>,
    /// discover and collect homes of all users (uid >= 1000) when running as root
    #[arg(long)]
    all_users: bool,
    /// root for system sources (`/var/log`, the journal), e.g. a mounted disk image
    #[arg(long, default_value = "/")]
    root: PathBuf,
    /// also capture live state: processes, sockets, kernel modules, tainted flag
    #[arg(long)]
    with_live: bool,
    /// also run clamscan / rkhunter / chkrootkit and import their findings
    #[arg(long)]
    with_scanners: bool,
}

#[derive(Args)]
struct ListArgs {
    /// start of the window (`2026-10-16T20:40`, `20:40`, `-24h`, `yesterday`, `@epoch`)
    #[arg(short, long, allow_hyphen_values = true)]
    since: Option<String>,
    /// end of the window, in the same forms as `--since`
    #[arg(short, long, allow_hyphen_values = true)]
    until: Option<String>,
    /// event kinds; `web.*` matches a family
    #[arg(short, long, value_delimiter = ',')]
    kind: Vec<String>,
    /// source kinds or families, apps, or `app/profile`
    #[arg(long, value_delimiter = ',')]
    source: Vec<String>,
    /// hosts, each also matching its subdomains
    #[arg(long, value_delimiter = ',')]
    host: Vec<String>,
    /// substring of subject, summary or attributes
    #[arg(short, long)]
    r#match: Option<String>,
    /// aggregate count of events by dimension
    #[arg(long, value_enum)]
    group_by: Option<GroupBy>,
    /// at most this many events
    #[arg(short, long)]
    limit: Option<usize>,
    /// newest first
    #[arg(short, long)]
    reverse: bool,
    #[arg(long, value_enum, default_value_t = Format::Table)]
    format: Format,
}

#[derive(Clone, Copy, clap::ValueEnum, PartialEq, Eq)]
enum GroupBy {
    Dir,
    Host,
    Source,
    Minute,
}

#[derive(Args)]
struct StateArgs {
    /// state kinds or pattern (e.g. `web.site_permission`, `persist.*`, `persist.systemd`)
    #[arg(short, long)]
    kind: Option<String>,
    /// a run id; defaults to the last run
    #[arg(short, long)]
    run: Option<u64>,
    /// substring of key or value
    #[arg(short, long)]
    r#match: Option<String>,
    #[arg(long, value_enum, default_value_t = Format::Table)]
    format: Format,
}

#[derive(Args)]
struct DiffArgs {
    /// first run ID (or 'prev')
    run_a: String,
    /// second run ID (or 'last')
    run_b: String,
    #[arg(long, value_enum, default_value_t = Format::Table)]
    format: Format,
}

#[derive(Args)]
struct IocArgs {
    #[command(subcommand)]
    cmd: IocCommand,
}

#[derive(Subcommand)]
enum IocCommand {
    /// Add an indicator of compromise
    Add {
        /// type of IOC: domain, ip, cidr, sha256, path, url
        ioc_type: String,
        /// value to match
        value: String,
        /// optional description / context note
        #[arg(long)]
        note: Option<String>,
    },
    /// Import indicators of compromise from a file
    Import {
        /// file containing IOCs (one per line, auto-detected or 'type value [note]')
        file: PathBuf,
    },
    /// Check the case database against all configured IOCs
    Check {
        #[arg(long, value_enum, default_value_t = Format::Table)]
        format: Format,
    },
    /// List all configured IOCs
    List,
}

#[derive(Args)]
struct ExportArgs {
    /// output format: jsonl, csv, html
    #[arg(short, long, value_enum)]
    format: ExportCliFormat,
    /// output file path
    #[arg(short, long)]
    out: PathBuf,
    /// hash sensitive query parameters and credentials in URLs and attributes
    #[arg(long)]
    redact: bool,

    /// start of the window (`2026-10-16T20:40`, `20:40`, `-24h`, `yesterday`, `@epoch`)
    #[arg(short, long, allow_hyphen_values = true)]
    since: Option<String>,
    /// end of the window, in the same forms as `--since`
    #[arg(short, long, allow_hyphen_values = true)]
    until: Option<String>,
    /// event kinds; `web.*` matches a family
    #[arg(short, long, value_delimiter = ',')]
    kind: Vec<String>,
    /// source kinds or families, apps, or `app/profile`
    #[arg(long, value_delimiter = ',')]
    source: Vec<String>,
    /// hosts, each also matching its subdomains
    #[arg(long, value_delimiter = ',')]
    host: Vec<String>,
    /// substring of subject, summary or attributes
    #[arg(short, long)]
    r#match: Option<String>,
    /// at most this many events
    #[arg(short, long)]
    limit: Option<usize>,
    /// newest first
    #[arg(short, long)]
    reverse: bool,
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum ExportCliFormat {
    Jsonl,
    Csv,
    Html,
}

#[derive(Clone, Copy, clap::ValueEnum, Debug, PartialEq, Eq)]
enum Format {
    Table,
    Jsonl,
    Csv,
}

/// Indicator types the matcher understands.
const IOC_TYPES: &[&str] = &["domain", "ip", "cidr", "sha256", "path", "url"];

/// Evidence is untrusted: nothing from it reaches the terminal unsanitized.
fn clean(text: &str) -> String {
    fp_render::sanitize(text)
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let tz = fp_query::parse_tz(&cli.tz)?;
    match cli.cmd {
        Command::Collect(args) => collect(&cli.case, args),
        Command::Sources {
            run,
            all_runs,
            coverage,
        } => sources(&cli.case, run, all_runs, coverage),
        Command::List(args) => list(&cli.case, args, tz),
        Command::Show { id } => show(&cli.case, id, tz),
        Command::Search { query } => search(&cli.case, &query, tz),
        Command::Trace { target } => trace(&cli.case, &target, tz),
        Command::State(args) => state(&cli.case, args),
        Command::Diff(args) => diff(&cli.case, args),
        Command::Ioc(args) => ioc(&cli.case, args, tz),
        Command::Export(args) => export(&cli.case, args),
        Command::Explain { finding } => explain(&finding),
        Command::Tui => fp_tui::run(&cli.case, tz),
    }
}

fn collect(case: &Path, args: CollectArgs) -> Result<()> {
    let params = fp_query::CollectParams {
        since: args.since,
        until: args.until,
        sources: args.sources,
        exclude: args.exclude,
        assume_tz: args.assume_tz,
        homes: args.home,
        all_users: args.all_users,
        root: args.root,
        with_live: args.with_live,
        with_scanners: args.with_scanners,
    };
    let (tx, rx) = channel();
    let case_owned = case.to_path_buf();
    let handle = std::thread::spawn(move || fp_query::run_collect(&case_owned, &params, Some(&tx)));
    while let Ok(event) = rx.recv() {
        match event {
            fp_query::CollectEvent::InstanceError { label, error } => {
                eprintln!("error: {label}: {error}");
            }
            fp_query::CollectEvent::InstanceDone {
                label, warnings, ..
            } => {
                for w in &warnings {
                    eprintln!("warning: {label}: {w}");
                }
            }
            fp_query::CollectEvent::Finished(summary) => {
                println!(
                    "run {}: {} new events ({} already in the case), {} states, {} artifacts from {} sources{} → {}",
                    summary.run_id,
                    summary.totals.events_new,
                    summary.totals.events_known,
                    summary.totals.states,
                    summary.totals.artifacts,
                    summary.sources_read,
                    if summary.errors.is_empty() {
                        String::new()
                    } else {
                        format!(", {} failed", summary.errors.len())
                    },
                    case.display()
                );
            }
            _ => {}
        }
    }
    handle.join().map_err(std::panic::resume_unwind)??;
    Ok(())
}

fn sources(case: &Path, run: Option<u64>, all_runs: bool, coverage: bool) -> Result<()> {
    let store = Store::open(case)?;
    let run = match (run, all_runs) {
        (_, true) => None,
        (Some(r), _) => Some(r),
        (None, false) => store.last_run_id()?,
    };
    let rows = fp_query::sources(&store, run)?;
    if coverage {
        let cov = fp_query::coverage(&rows);
        println!(
            "{:<18} {:<10} {:<32} NOTES / FALLBACKS",
            "CATEGORY", "STATUS", "ANSWERING SOURCES"
        );
        for row in &cov {
            let status = match row.status {
                fp_query::CoverageStatus::Covered => "Covered",
                fp_query::CoverageStatus::Partial => "Partial",
                fp_query::CoverageStatus::Gap => "Gap",
            };
            println!(
                "{:<18} {:<10} {:<32} {}",
                row.category,
                status,
                if row.answering.is_empty() {
                    "none".to_string()
                } else {
                    clean(&row.answering.join(", "))
                },
                if row.notes.is_empty() {
                    "-".to_string()
                } else {
                    clean(&row.notes.join("; "))
                }
            );
        }
        return Ok(());
    }

    for s in &rows {
        let count_str = if s.event_count > 0 {
            format!("{} events", s.event_count)
        } else {
            "-".to_string()
        };
        println!(
            "{:>5}  run {:<3} {:<18} {:<24} {:<14} {:<10} {}{}",
            s.id,
            s.run_id,
            s.kind,
            clean(&s.label()),
            clean(&s.status_display()),
            count_str,
            clean(s.path.as_deref().unwrap_or("-")),
            s.note
                .as_ref()
                .map(|n| format!("  ({})", clean(n)))
                .unwrap_or_default()
        );
    }
    Ok(())
}

fn list(case: &Path, args: ListArgs, tz: fp_query::Tz) -> Result<()> {
    let store = Store::open(case)?;
    let filter = fp_query::TimelineFilter {
        since: args
            .since
            .as_deref()
            .map(fp_query::time::parse)
            .transpose()?,
        until: args
            .until
            .as_deref()
            .map(fp_query::time::parse)
            .transpose()?,
        kinds: args.kind,
        hosts: args.host,
        sources: args.source,
        match_text: args.r#match,
        limit: if args.group_by.is_some() {
            None
        } else {
            args.limit
        },
        offset: None,
        reverse: args.reverse,
    };
    let rows = fp_query::timeline(&store, &filter)?;
    if let Some(gb) = args.group_by {
        print_grouped(rows, gb, args.format, args.limit, tz)
    } else {
        print_rows(rows, args.format, tz)
    }
}

fn print_grouped(
    rows: Vec<fp_query::TimelineRow>,
    group_by: GroupBy,
    format: Format,
    limit: Option<usize>,
    tz: fp_query::Tz,
) -> Result<()> {
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    for r in &rows {
        let key = match group_by {
            GroupBy::Dir => {
                let subj = r.subject.as_deref().unwrap_or("");
                if subj.contains('/') {
                    std::path::Path::new(subj)
                        .parent()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "/".into())
                } else {
                    "-".into()
                }
            }
            GroupBy::Host => r.subject_host.clone().unwrap_or_else(|| "-".into()),
            GroupBy::Source => r.source_label(),
            GroupBy::Minute => {
                fp_query::format_time(r.ts_utc, &r.ts_precision, tz, "%Y-%m-%d %H:%M")
            }
        };
        *counts.entry(key).or_insert(0) += 1;
    }

    let mut sorted: Vec<(String, usize)> = counts.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    if let Some(lim) = limit {
        sorted.truncate(lim);
    }

    match format {
        Format::Jsonl => {
            for (key, count) in sorted {
                println!(
                    "{}",
                    serde_json::json!({
                        "group": key,
                        "count": count,
                    })
                );
            }
        }
        Format::Table => {
            println!("{:>6}  GROUP", "COUNT");
            for (key, count) in sorted {
                println!("{:>6}  {}", count, key);
            }
        }
        Format::Csv => {
            println!("count,group");
            for (key, count) in sorted {
                println!("{},{}", count, fp_render::csv::escape_csv(&key));
            }
        }
    }
    Ok(())
}

fn print_rows(rows: Vec<fp_query::TimelineRow>, format: Format, tz: fp_query::Tz) -> Result<()> {
    if matches!(format, Format::Csv) {
        println!("id,time,source,kind,summary,ioc_hits");
    }
    for row in rows {
        let ioc_badge = if row.ioc_hits.is_empty() {
            String::new()
        } else {
            format!(" [IOC: {}]", row.ioc_hits.join(", "))
        };

        match format {
            Format::Jsonl => {
                let mut val = serde_json::to_value(&row)?;
                if !row.ioc_hits.is_empty() {
                    val["ioc_hits"] = serde_json::json!(row.ioc_hits);
                } else {
                    val.as_object_mut().map(|m| m.remove("ioc_hits"));
                }
                println!("{}", serde_json::to_string(&val)?);
            }
            Format::Table => {
                println!(
                    "{:>6}  {:<19}  {:<18} {:<21} {}{}",
                    row.id,
                    fp_query::format_time(row.ts_utc, &row.ts_precision, tz, "%Y-%m-%d %H:%M:%S"),
                    row.source_label(),
                    row.kind,
                    fp_render::sanitize(&row.display_text()),
                    ioc_badge
                );
            }
            Format::Csv => {
                let time_str =
                    fp_query::format_time(row.ts_utc, &row.ts_precision, tz, "%Y-%m-%d %H:%M:%S");
                println!(
                    "{},{},{},{},{},{}",
                    row.id,
                    fp_render::csv::escape_csv(&time_str),
                    fp_render::csv::escape_csv(&row.source_label()),
                    fp_render::csv::escape_csv(&row.kind),
                    fp_render::csv::escape_csv(&fp_render::sanitize(&row.display_text())),
                    fp_render::csv::escape_csv(&row.ioc_hits.join(";")),
                );
            }
        }
    }
    Ok(())
}

fn search(case: &Path, query: &str, tz: fp_query::Tz) -> Result<()> {
    let store = Store::open(case)?;
    print_rows(fp_query::search(&store, query)?, Format::Table, tz)
}

fn show(case: &Path, id: u64, tz: fp_query::Tz) -> Result<()> {
    let store = Store::open(case)?;
    let ev =
        fp_query::event_detail(&store, id)?.with_context(|| format!("event {id} not found"))?;
    let field = |name: &str, value: &str| println!("{name:<12} {value}");
    let dash = |v: &Option<String>| v.clone().unwrap_or_else(|| "-".into());
    field("id", &ev.id.to_string());
    field("run", &ev.run_id.to_string());
    field(
        "time",
        &fp_query::format_time(ev.ts_utc, &ev.ts_precision, tz, "%Y-%m-%d %H:%M:%S%.6f %:z"),
    );
    field("precision", &ev.ts_precision);
    field(
        "raw time",
        &format!("{} ({})", dash(&ev.ts_raw), dash(&ev.ts_raw_format)),
    );
    field("tz assumed", &dash(&ev.ts_tz_assumed));
    field("kind", &ev.kind);
    field(
        "source",
        &format!(
            "{} {}{}",
            ev.source_kind,
            dash(&ev.app),
            ev.profile.map(|p| format!("/{p}")).unwrap_or_default()
        ),
    );
    field("subject", &fp_render::sanitize(&dash(&ev.subject)));
    field("host", &clean(&dash(&ev.subject_host)));
    field("summary", &fp_render::sanitize(&dash(&ev.summary)));
    field("confidence", &ev.confidence);
    field("file", &clean(&dash(&ev.prov_file)));
    field("sha256", &dash(&ev.prov_sha256));
    field("locator", &clean(&dash(&ev.locator)));
    field(
        "actor",
        &clean(
            &ev.actor
                .as_ref()
                .map(|a| a.to_string())
                .unwrap_or_else(|| "-".into()),
        ),
    );
    field(
        "attrs",
        &fp_render::sanitize(
            &ev.attrs
                .as_ref()
                .map(|a| a.to_string())
                .unwrap_or_else(|| "-".into()),
        ),
    );

    if !ev.links.is_empty() {
        println!("links:");
        for l in &ev.links {
            let arrow = match l.direction {
                fp_query::LinkDirection::Outgoing => "->",
                fp_query::LinkDirection::Incoming => "<-",
            };
            let why_str = l
                .why
                .as_ref()
                .map(|w| format!(" ({w})"))
                .unwrap_or_default();
            println!(
                "  - {:<16} {} {} {} [{}]{}",
                l.rel,
                arrow,
                l.target_type,
                clean(&l.target_label),
                l.confidence,
                clean(&why_str)
            );
        }
    }
    Ok(())
}

fn trace(case: &Path, target: &str, tz: fp_query::Tz) -> Result<()> {
    // read-only: links were built by `collect`, which knew the run's --root
    let store = Store::open(case)?;
    let time = |ts| fp_query::format_time(ts, "second", tz, "%H:%M:%S");
    print!(
        "{}",
        clean(&fp_correlate::render_trace(&store, target, &time)?)
    );
    Ok(())
}

fn state(case: &Path, args: StateArgs) -> Result<()> {
    let store = Store::open(case)?;
    let run_id = match args.run {
        Some(r) => Some(r),
        None => store.last_run_id()?,
    };
    let rows = fp_query::state_records(
        &store,
        run_id,
        args.kind.as_deref(),
        args.r#match.as_deref(),
    )?;

    match args.format {
        Format::Jsonl => {
            for row in &rows {
                println!(
                    "{}",
                    serde_json::json!({
                        "rowid": row.rowid,
                        "run_id": row.run_id,
                        "kind": row.kind,
                        "key": row.key,
                        "value_json": row.value_json,
                        "locator": row.locator,
                    })
                );
            }
        }
        Format::Table => {
            if rows.is_empty() {
                println!("no state records found");
                return Ok(());
            }
            println!("{:>5}  {:<24} {:<42} DETAILS", "RUN", "KIND", "KEY");
            for row in &rows {
                let details_parts = fp_query::state_details(row.parsed.as_ref());
                let details = if details_parts.is_empty() {
                    "-".to_string()
                } else {
                    details_parts.join("; ")
                };
                println!(
                    "{:>5}  {:<24} {:<42} {}",
                    row.run_id,
                    row.kind,
                    clean(&row.key),
                    clean(&details)
                );
            }
        }
        Format::Csv => {
            println!("run,kind,key,value");
            for row in &rows {
                println!(
                    "{},{},{},{}",
                    row.run_id,
                    fp_render::csv::escape_csv(&row.kind),
                    fp_render::csv::escape_csv(&row.key),
                    fp_render::csv::escape_csv(&row.value_json),
                );
            }
        }
    }
    Ok(())
}

fn resolve_run_id(store: &Store, s: &str) -> Result<u64> {
    if let Ok(id) = s.parse::<u64>() {
        return Ok(id);
    }
    let last = store.last_run_id()?.context("no runs found in case")?;
    match s.to_lowercase().as_str() {
        "last" | "latest" => Ok(last),
        "prev" | "previous" => {
            if last <= 1 {
                bail!("no previous run before run {last}");
            }
            Ok(last - 1)
        }
        _ => bail!("invalid run id '{s}'; expected integer, 'last', or 'prev'"),
    }
}

fn diff(case: &Path, args: DiffArgs) -> Result<()> {
    let store = Store::open(case)?;
    let run_a = resolve_run_id(&store, &args.run_a)?;
    let run_b = resolve_run_id(&store, &args.run_b)?;
    let entries = fp_query::diff_runs(&store, run_a, run_b)?;

    match args.format {
        Format::Jsonl => {
            for e in &entries {
                let sign = match e.change {
                    fp_query::DiffChange::Added => "+",
                    fp_query::DiffChange::Removed => "-",
                    fp_query::DiffChange::Changed => "~",
                };
                println!(
                    "{}",
                    serde_json::json!({
                        "diff": sign,
                        "kind": e.kind,
                        "key": e.key,
                        "run_a": e.run_a,
                        "run_b": e.run_b,
                    })
                );
            }
        }
        Format::Table => {
            println!("{:<4} {:<24} {:<42} CHANGE", "DIFF", "KIND", "KEY");
            for e in &entries {
                let sign = match e.change {
                    fp_query::DiffChange::Added => "+",
                    fp_query::DiffChange::Removed => "-",
                    fp_query::DiffChange::Changed => "~",
                };
                println!(
                    "{:<4} {:<24} {:<42} {}",
                    sign,
                    e.kind,
                    clean(&e.key),
                    clean(&e.detail)
                );
            }
            if entries.is_empty() {
                println!("no differences between run {run_a} and run {run_b}");
            }
        }
        Format::Csv => {
            println!("diff,kind,key,detail");
            for e in &entries {
                let sign = match e.change {
                    fp_query::DiffChange::Added => "+",
                    fp_query::DiffChange::Removed => "-",
                    fp_query::DiffChange::Changed => "~",
                };
                println!(
                    "{},{},{},{}",
                    sign,
                    fp_render::csv::escape_csv(&e.kind),
                    fp_render::csv::escape_csv(&e.key),
                    fp_render::csv::escape_csv(&e.detail)
                );
            }
        }
    }
    Ok(())
}

fn ioc(case: &Path, args: IocArgs, tz: fp_query::Tz) -> Result<()> {
    match args.cmd {
        IocCommand::Add {
            ioc_type,
            value,
            note,
        } => {
            let ioc_type = ioc_type.to_lowercase();
            if !IOC_TYPES.contains(&ioc_type.as_str()) {
                bail!(
                    "unknown ioc type `{ioc_type}`; expected one of {}",
                    IOC_TYPES.join(", ")
                );
            }
            let mut store = Store::open_or_create(case)?;
            let id = store.insert_ioc(&ioc_type, &value, note.as_deref())?;
            fp_correlate::correlate_iocs(&mut store)?;
            println!(
                "added IOC #{id}: {ioc_type} {value}{}",
                note.map(|n| format!(" ({n})")).unwrap_or_default()
            );
        }
        IocCommand::Import { file } => {
            let mut store = Store::open_or_create(case)?;
            let content = std::fs::read_to_string(&file)
                .with_context(|| format!("failed to read IOC file {}", file.display()))?;
            let mut count = 0;
            for line in content.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                let (ioc_type, val, note) = parse_ioc_line(line);
                if !val.is_empty() {
                    store.insert_ioc(&ioc_type, &val, note.as_deref())?;
                    count += 1;
                }
            }
            fp_correlate::correlate_iocs(&mut store)?;
            println!("imported {count} IOCs from {}", file.display());
        }
        IocCommand::Check { format } => {
            let store = Store::open(case)?;
            let hits = fp_correlate::check_iocs(&store)?;
            match format {
                Format::Jsonl => {
                    for hit in hits {
                        println!("{}", serde_json::to_string(&hit)?);
                    }
                }
                Format::Table => {
                    if hits.is_empty() {
                        println!("No IOC hits found.");
                    } else {
                        println!(
                            "{:<28} {:<8} {:<30} {:<18} {:<30} NOTE",
                            "TARGET", "TYPE", "IOC VALUE", "FIELD", "MATCHED VALUE"
                        );
                        for h in hits {
                            println!(
                                "{:<28} {:<8} {:<30} {:<18} {:<30} {}",
                                fp_render::sanitize(&h.target_desc),
                                h.ioc_type,
                                fp_render::sanitize(&h.ioc_value),
                                h.matched_field,
                                fp_render::sanitize(&h.matched_value),
                                h.ioc_note.unwrap_or_else(|| "-".into())
                            );
                        }
                    }
                }
                Format::Csv => {
                    println!(
                        "target_type,target_id,target_desc,ioc_id,ioc_type,ioc_value,matched_field,matched_value,note"
                    );
                    for h in hits {
                        println!(
                            "{},{},{},{},{},{},{},{},{}",
                            fp_render::csv::escape_csv(&h.target_type),
                            h.target_id,
                            fp_render::csv::escape_csv(&h.target_desc),
                            h.ioc_id,
                            fp_render::csv::escape_csv(&h.ioc_type),
                            fp_render::csv::escape_csv(&h.ioc_value),
                            fp_render::csv::escape_csv(&h.matched_field),
                            fp_render::csv::escape_csv(&h.matched_value),
                            fp_render::csv::escape_csv(h.ioc_note.as_deref().unwrap_or("")),
                        );
                    }
                }
            }
        }
        IocCommand::List => {
            let store = Store::open(case)?;
            let iocs = store.list_iocs()?;
            if iocs.is_empty() {
                println!("No IOCs configured. Add one with 'footprint ioc add <type> <value>'.");
            } else {
                println!(
                    "{:<6} {:<8} {:<36} {:<20} NOTE",
                    "ID", "TYPE", "VALUE", "ADDED"
                );
                for ioc in iocs {
                    let added =
                        fp_query::format_time(ioc.added_utc, "second", tz, "%Y-%m-%d %H:%M:%S");
                    println!(
                        "{:<6} {:<8} {:<36} {:<20} {}",
                        ioc.id,
                        ioc.ioc_type,
                        ioc.value,
                        added,
                        ioc.note.unwrap_or_else(|| "-".into())
                    );
                }
            }
        }
    }
    Ok(())
}

fn export(case: &Path, args: ExportArgs) -> Result<()> {
    let store = Store::open(case)?;
    let filter = fp_store::EventFilter {
        since: args
            .since
            .as_deref()
            .map(fp_query::time::parse)
            .transpose()?,
        until: args
            .until
            .as_deref()
            .map(fp_query::time::parse)
            .transpose()?,
        kinds: args.kind,
        hosts: args.host,
        sources: args.source,
        match_text: args.r#match,
        limit: args.limit,
        offset: None,
        reverse: args.reverse,
    };

    let fmt = match args.format {
        ExportCliFormat::Jsonl => fp_render::ExportFormat::Jsonl,
        ExportCliFormat::Csv => fp_render::ExportFormat::Csv,
        ExportCliFormat::Html => fp_render::ExportFormat::Html,
    };

    let count = fp_render::export_case(&store, &filter, fmt, &args.out, args.redact)?;
    println!(
        "exported {count} events to {} ({:?}{})",
        args.out.display(),
        args.format,
        if args.redact { ", redacted" } else { "" }
    );
    Ok(())
}

fn explain(finding: &str) -> Result<()> {
    let hits = fp_query::explain(finding);
    if hits.is_empty() {
        println!("no known explanation for this finding; treat it as real until proven harmless");
        return Ok(());
    }
    for (i, hit) in hits.iter().enumerate() {
        if i > 0 {
            println!();
        }
        println!("{}", hit.title);
        println!("  why:   {}", hit.why);
        println!("  proof: {}", hit.proof);
    }
    Ok(())
}

fn parse_ioc_line(line: &str) -> (String, String, Option<String>) {
    let (body, note) = match line.split_once('#') {
        Some((b, n)) => (b.trim(), Some(n.trim().to_string())),
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
        (ioc_type, val.to_string(), note)
    }
}

fn detect_ioc_type(val: &str) -> String {
    if val.len() == 64 && val.chars().all(|c| c.is_ascii_hexdigit()) {
        "sha256".to_string()
    } else if val.starts_with('/')
        || val.starts_with("./")
        || val.contains('*')
        || val.contains('?')
    {
        "path".to_string()
    } else if val.contains("://") {
        "url".to_string()
    } else if val.contains('/')
        && val
            .split('/')
            .next()
            .is_some_and(|ip| ip.parse::<std::net::IpAddr>().is_ok())
    {
        "cidr".to_string()
    } else if val.parse::<std::net::IpAddr>().is_ok() {
        "ip".to_string()
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
        "path".to_string()
    } else {
        "domain".to_string()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn ioc_type_detection() {
        use super::detect_ioc_type;
        assert_eq!(detect_ioc_type("us09-web-zoom.com"), "domain");
        assert_eq!(detect_ioc_type("192.168.1.1"), "ip");
        assert_eq!(detect_ioc_type("192.168.1.0/24"), "cidr");
        assert_eq!(detect_ioc_type("/tmp/evil.sh"), "path");
    }
}
