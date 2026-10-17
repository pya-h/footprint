use anyhow::{Context, Result, bail};
use chrono::{DateTime, Local};
use clap::{Args, Parser, Subcommand};
use fp_collect::{CollectCtx, Collector, Env, Sink};
use fp_core::{Availability, SourceInstance};
use fp_store::{Batch, EventFilter, EventRow, NewRun, Store};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

mod time;

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
}

#[derive(Args)]
struct CollectArgs {
    #[arg(short, long, allow_hyphen_values = true)]
    since: Option<String>,
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
}

#[derive(Args)]
struct ListArgs {
    #[arg(short, long, allow_hyphen_values = true)]
    since: Option<String>,
    #[arg(short, long, allow_hyphen_values = true)]
    until: Option<String>,
    /// event kinds; `web.*` matches a family
    #[arg(short, long, value_delimiter = ',')]
    kind: Vec<String>,
    /// source kinds or apps
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

#[derive(Clone, Copy, clap::ValueEnum)]
enum Format {
    Table,
    Jsonl,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Command::Collect(args) => collect(&cli.case, args),
        Command::Sources {
            run,
            all_runs,
            coverage,
        } => sources(&cli.case, run, all_runs, coverage),
        Command::List(args) => list(&cli.case, args),
        Command::Show { id } => show(&cli.case, id),
        Command::Search { query } => {
            let store = Store::open(&cli.case)?;
            print_rows(store.search_events(&query)?, Format::Table)
        }
        Command::Trace { target } => trace(&cli.case, &target),
        Command::State(args) => state(&cli.case, args),
        Command::Diff(args) => diff(&cli.case, args),
    }
}

/// `chromium` selects `chromium.history`; `all` selects everything.
fn selected(kind: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|p| {
        p == "all"
            || kind == p
            || kind
                .strip_prefix(p.as_str())
                .is_some_and(|rest| rest.starts_with('.'))
    })
}

fn collect(case: &PathBuf, args: CollectArgs) -> Result<()> {
    let since = args.since.as_deref().map(time::parse).transpose()?;
    let until = args.until.as_deref().map(time::parse).transpose()?;
    if let (Some(lo), Some(hi)) = (since, until)
        && lo > hi
    {
        bail!("--since is after --until");
    }
    let collectors: Vec<Box<dyn Collector>> = fp_collect::registry()
        .into_iter()
        .filter(|c| selected(c.kind(), &args.sources) && !selected(c.kind(), &args.exclude))
        .collect();
    if collectors.is_empty() {
        bail!("no source kind matches --sources/--exclude");
    }

    let mut env = Env::current();
    if !args.home.is_empty() {
        env.home = args.home[0].clone();
        env.user_targets.clear();
        for h in &args.home {
            env.add_user_home(h.clone(), None, None);
        }
    }
    if args.all_users {
        if env.euid != 0 && env.uid != 0 {
            bail!("--all-users requires root privileges");
        }
        env.load_all_users()?;
    }
    env.root = args.root;

    let mut store = Store::open_or_create(case)?;
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let run_id = store.insert_run(&NewRun {
        host: &hostname(),
        user: &std::env::var("USER").unwrap_or_default(),
        uid: env.uid,
        euid: env.euid,
        version: env!("CARGO_PKG_VERSION"),
        tz: &Local::now().offset().to_string(),
        args: &argv.join(" "),
    })?;

    let temp_dir = tempfile::Builder::new().prefix("footprint-").tempdir()?;
    fs::set_permissions(temp_dir.path(), fs::Permissions::from_mode(0o700))?;
    let ctx = CollectCtx {
        run_id,
        since,
        until,
        assume_tz: args.assume_tz,
        temp_dir: temp_dir.path().to_path_buf(),
    };

    let mut errors = Vec::new();
    let mut totals = fp_store::IngestStats::default();
    let mut read = 0;
    for collector in &collectors {
        for mut inst in collector.discover(&env) {
            inst.id = store.insert_source(run_id, &inst)?;
            if inst.status != Availability::Present {
                continue;
            }
            read += 1;
            let mut sink = Sink::default();
            if let Err(e) = collector.collect(&inst, &ctx, &mut sink) {
                let msg = format!("{}: {e:#}", label(&inst));
                store.update_source_status(
                    inst.id,
                    &Availability::Error(format!("{e:#}")),
                    None,
                )?;
                eprintln!("error: {msg}");
                errors.push(msg);
                continue;
            }
            let stats = store.ingest(
                run_id,
                &Batch {
                    source_identity: &inst.identity(),
                    events: &sink.events,
                    states: &sink.states,
                    artifacts: &sink.artifacts,
                },
            )?;
            totals.events_new += stats.events_new;
            totals.events_known += stats.events_known;
            totals.states += stats.states;
            totals.artifacts += stats.artifacts;
            if !sink.warnings.is_empty() {
                for w in &sink.warnings {
                    eprintln!("warning: {}: {w}", label(&inst));
                }
                store.update_source_status(
                    inst.id,
                    &Availability::Partial,
                    Some(&sink.warnings.join("; ")),
                )?;
            } else if stats.events_new == 0
                && stats.events_known == 0
                && stats.states == 0
                && stats.artifacts == 0
            {
                store.update_source_status(inst.id, &Availability::Empty, Some("empty"))?;
            }
        }
    }

    if let Err(e) = fp_correlate::correlate(&mut store, &env.root) {
        errors.push(format!("correlate: {e:#}"));
    }

    store.finish_run(run_id, &errors)?;
    println!(
        "run {run_id}: {} new events ({} already in the case), {} states, {} artifacts from {read} sources{} → {}",
        totals.events_new,
        totals.events_known,
        totals.states,
        totals.artifacts,
        if errors.is_empty() {
            String::new()
        } else {
            format!(", {} failed", errors.len())
        },
        store.path().display()
    );
    Ok(())
}

fn label(inst: &SourceInstance) -> String {
    match &inst.profile {
        Some(p) => format!("{} {}/{p}", inst.kind, inst.app),
        None => format!("{} {} ({})", inst.kind, inst.app, inst.path.display()),
    }
}

fn sources(case: &PathBuf, run: Option<u64>, all_runs: bool, coverage: bool) -> Result<()> {
    let store = Store::open(case)?;
    let run = match (run, all_runs) {
        (_, true) => None,
        (Some(r), _) => Some(r),
        (None, false) => store.last_run_id()?,
    };
    let rows = store.sources(run)?;
    if coverage {
        print_coverage(&rows);
        return Ok(());
    }

    for s in rows {
        let status: Availability = s
            .status
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .context("unreadable source status")?
            .unwrap_or(Availability::Present);
        let app = match (&s.app, &s.profile) {
            (Some(a), Some(p)) => format!("{a}/{p}"),
            (Some(a), None) => a.clone(),
            _ => "-".into(),
        };
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
            app,
            status.to_string(),
            count_str,
            s.path.as_deref().unwrap_or("-"),
            s.note.map(|n| format!("  ({n})")).unwrap_or_default()
        );
    }
    Ok(())
}

fn print_coverage(sources: &[fp_store::SourceRow]) {
    println!(
        "{:<18} {:<10} {:<32} NOTES / FALLBACKS",
        "CATEGORY", "STATUS", "ANSWERING SOURCES"
    );
    let categories: &[(&str, &[&str])] = &[
        ("Browser history", &["chromium.history", "firefox.history"]),
        ("Downloads", &["dlmgr.xdm", "dlmgr.xattr", "dlmgr.detect"]),
        (
            "Packages",
            &[
                "pkg.dpkg",
                "pkg.apt",
                "pkg.pacman",
                "pkg.dnf",
                "pkg.zypper",
                "pkg.flatpak",
            ],
        ),
        ("Privilege", &["auth.journald", "auth.log"]),
        ("Shell", &["shell.history"]),
        (
            "Desktop activity",
            &["desktop.recent", "desktop.kde", "desktop.zeitgeist"],
        ),
    ];

    for &(cat_name, kinds) in categories {
        let matching: Vec<&fp_store::SourceRow> = sources
            .iter()
            .filter(|s| kinds.contains(&s.kind.as_str()))
            .collect();
        let mut answering = Vec::new();
        let mut notes = Vec::new();

        for s in &matching {
            let app = match (&s.app, &s.profile) {
                (Some(a), Some(p)) => format!("{a}/{p}"),
                (Some(a), None) => a.clone(),
                _ => s.kind.clone(),
            };
            let status: Availability = s
                .status
                .as_deref()
                .and_then(|st| serde_json::from_str(st).ok())
                .unwrap_or(Availability::Present);

            match status {
                Availability::Present if s.event_count > 0 => {
                    answering.push(format!("{app} ({} events)", s.event_count));
                }
                Availability::Present => {
                    answering.push(format!("{app} (0 events)"));
                }
                Availability::NotInstalled => {
                    notes.push(format!("{app} not installed"));
                }
                Availability::Empty => {
                    notes.push(format!("{app} empty"));
                }
                Availability::Unsupported { reason } => {
                    notes.push(format!("{app} unsupported ({reason})"));
                }
                Availability::Unreadable { needs_root } => {
                    notes.push(format!(
                        "{app} unreadable{}",
                        if needs_root { " (needs root)" } else { "" }
                    ));
                }
                Availability::Partial => {
                    answering.push(format!("{app} (partial, {} events)", s.event_count));
                }
                Availability::Error(ref e) => {
                    notes.push(format!("{app} error: {e}"));
                }
            }
        }

        let has_events = matching.iter().any(|s| s.event_count > 0);
        let has_present = matching.iter().any(|s| {
            s.status
                .as_deref()
                .and_then(|st| serde_json::from_str::<Availability>(st).ok())
                == Some(Availability::Present)
        });

        let status_str = if has_events {
            "Covered"
        } else if has_present {
            "Partial"
        } else {
            "Gap"
        };

        let ans_str = if answering.is_empty() {
            "none".to_string()
        } else {
            answering.join(", ")
        };

        let note_str = if notes.is_empty() {
            "-".to_string()
        } else {
            notes.join("; ")
        };

        println!(
            "{:<18} {:<10} {:<32} {}",
            cat_name, status_str, ans_str, note_str
        );
    }
}

fn list(case: &PathBuf, args: ListArgs) -> Result<()> {
    let store = Store::open(case)?;
    let filter = EventFilter {
        since: args.since.as_deref().map(time::parse).transpose()?,
        until: args.until.as_deref().map(time::parse).transpose()?,
        kinds: args.kind,
        hosts: args.host,
        match_text: args.r#match,
        limit: if args.group_by.is_some() {
            None
        } else {
            args.limit
        },
        reverse: args.reverse,
    };
    let mut rows = store.list_events(&filter)?;

    if !args.source.is_empty() {
        rows.retain(|r| {
            let app = r.app.as_deref().unwrap_or("");
            let source_str = match (&r.app, &r.profile) {
                (Some(a), Some(p)) => format!("{a}/{p}"),
                (Some(a), None) => a.clone(),
                _ => String::new(),
            };
            args.source.iter().any(|s| {
                s == &r.kind || s == app || s == &source_str || r.kind.starts_with(s.as_str())
            })
        });
    }

    if let Some(gb) = args.group_by {
        print_grouped(rows, gb, args.format, args.limit)
    } else {
        print_rows(rows, args.format)
    }
}

fn print_grouped(
    rows: Vec<EventRow>,
    group_by: GroupBy,
    format: Format,
    limit: Option<usize>,
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
            GroupBy::Source => match (&r.app, &r.profile) {
                (Some(a), Some(p)) => format!("{a}/{p}"),
                (Some(a), None) => a.clone(),
                _ => r.kind.clone(),
            },
            GroupBy::Minute => local_time(r.ts_utc, &r.ts_precision, "%Y-%m-%d %H:%M"),
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
    }
    Ok(())
}

fn local_time(ts_utc: i64, precision: &str, pattern: &str) -> String {
    if precision == "order_only" {
        return "(untimed)".to_string();
    }
    DateTime::from_timestamp_micros(ts_utc)
        .map(|t| t.with_timezone(&Local).format(pattern).to_string())
        .unwrap_or_else(|| format!("@{ts_utc}µs"))
}

fn print_rows(rows: Vec<EventRow>, format: Format) -> Result<()> {
    for row in rows {
        match format {
            Format::Jsonl => println!("{}", serde_json::to_string(&row)?),
            Format::Table => {
                let source = match (&row.app, &row.profile) {
                    (Some(a), Some(p)) => format!("{a}/{p}"),
                    (Some(a), None) => a.clone(),
                    _ => "-".into(),
                };
                println!(
                    "{:>6}  {:<19}  {:<18} {:<21} {}",
                    row.id,
                    local_time(row.ts_utc, &row.ts_precision, "%Y-%m-%d %H:%M:%S"),
                    source,
                    row.kind,
                    row.summary.or(row.subject).unwrap_or_default()
                );
            }
        }
    }
    Ok(())
}

fn show(case: &PathBuf, id: u64) -> Result<()> {
    let store = Store::open(case)?;
    let ev = store
        .get_event(id)?
        .with_context(|| format!("event {id} not found"))?;
    let field = |name: &str, value: &str| println!("{name:<12} {value}");
    let dash = |v: &Option<String>| v.clone().unwrap_or_else(|| "-".into());
    field("id", &ev.id.to_string());
    field("run", &ev.run_id.to_string());
    field(
        "time",
        &local_time(ev.ts_utc, &ev.ts_precision, "%Y-%m-%d %H:%M:%S%.6f %:z"),
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
    field("subject", &dash(&ev.subject));
    field("host", &dash(&ev.subject_host));
    field("summary", &dash(&ev.summary));
    field("confidence", &ev.confidence);
    field("file", &dash(&ev.prov_file));
    field("sha256", &dash(&ev.prov_sha256));
    field("locator", &dash(&ev.locator));
    field("actor", &dash(&ev.actor_json));
    field("attrs", &dash(&ev.attrs_json));

    let links = store.get_links_for("event", ev.id)?;
    if !links.is_empty() {
        println!("links:");
        for l in links {
            let target = if l.src_type == "event" && l.src_id == ev.id {
                format!("{} {}", l.dst_type, l.dst_id)
            } else {
                format!("{} {}", l.src_type, l.src_id)
            };
            let why_str = l.why.map(|w| format!(" ({w})")).unwrap_or_default();
            println!(
                "  - {:<16} {:<16} [{}{}]",
                l.rel, target, l.confidence, why_str
            );
        }
    }
    Ok(())
}

fn trace(case: &PathBuf, target: &str) -> Result<()> {
    let mut store = Store::open(case)?;
    let _ = fp_correlate::correlate(&mut store, std::path::Path::new("/"));
    let rendered = fp_correlate::render_trace(&store, target)?;
    print!("{rendered}");
    Ok(())
}

fn state(case: &PathBuf, args: StateArgs) -> Result<()> {
    let store = Store::open(case)?;
    let run_id = match args.run {
        Some(r) => Some(r),
        None => store.last_run_id()?,
    };
    let rows = store.list_state(run_id, args.kind.as_deref(), args.r#match.as_deref())?;

    match args.format {
        Format::Jsonl => {
            for row in rows {
                println!("{}", serde_json::to_string(&row)?);
            }
        }
        Format::Table => {
            if rows.is_empty() {
                println!("no state records found");
                return Ok(());
            }
            println!("{:>5}  {:<24} {:<42} DETAILS", "RUN", "KIND", "KEY");
            for row in rows {
                let parsed: Option<serde_json::Value> = serde_json::from_str(&row.value_json).ok();
                let mut details_parts = Vec::new();

                if let Some(ref val) = parsed {
                    if let Some(flags) = val.get("flags").and_then(|f| f.as_array())
                        && !flags.is_empty()
                    {
                        let flag_strs: Vec<&str> =
                            flags.iter().filter_map(|s| s.as_str()).collect();
                        details_parts.push(format!("flags: [{}]", flag_strs.join(", ")));
                    }
                    if let Some(pkg) = val.get("owner_pkg").and_then(|p| p.as_str()) {
                        details_parts.push(format!("pkg: {pkg}"));
                    }
                    if let Some(integ) = val.get("integrity").and_then(|i| i.as_str())
                        && integ != "ok"
                        && integ != "unowned"
                    {
                        details_parts.push(format!("integrity: {integ}"));
                    }
                    if let Some(decision) = val.get("decision").and_then(|d| d.as_str()) {
                        details_parts.push(format!("decision: {decision}"));
                    }
                    if let Some(ww) = val
                        .get("details")
                        .and_then(|d| d.get("world_writable_path_entries"))
                        .and_then(|w| w.as_array())
                        && !ww.is_empty()
                    {
                        let paths: Vec<&str> = ww.iter().filter_map(|p| p.as_str()).collect();
                        details_parts.push(format!("world-writable: {}", paths.join(", ")));
                    }
                    if let Some(u0) = val
                        .get("details")
                        .and_then(|d| d.get("uid0_users"))
                        .and_then(|u| u.as_array())
                    {
                        let non_root: Vec<&str> = u0
                            .iter()
                            .filter_map(|u| u.as_str())
                            .filter(|&u| u != "root")
                            .collect();
                        if !non_root.is_empty() {
                            details_parts.push(format!("uid0 non-root: {}", non_root.join(", ")));
                        }
                    }
                }

                let details = if details_parts.is_empty() {
                    "-".to_string()
                } else {
                    details_parts.join("; ")
                };

                println!(
                    "{:>5}  {:<24} {:<42} {}",
                    row.run_id, row.kind, row.key, details
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

fn diff(case: &PathBuf, args: DiffArgs) -> Result<()> {
    let store = Store::open(case)?;
    let run_a = resolve_run_id(&store, &args.run_a)?;
    let run_b = resolve_run_id(&store, &args.run_b)?;

    let states_a = store.list_state(Some(run_a), None, None)?;
    let states_b = store.list_state(Some(run_b), None, None)?;

    let mut map_a: std::collections::HashMap<(&str, &str), &fp_store::StateRow> =
        std::collections::HashMap::new();
    for row in &states_a {
        map_a.insert((&row.kind, &row.key), row);
    }

    let mut map_b: std::collections::HashMap<(&str, &str), &fp_store::StateRow> =
        std::collections::HashMap::new();
    for row in &states_b {
        map_b.insert((&row.kind, &row.key), row);
    }

    let mut all_keys: std::collections::BTreeSet<(&str, &str)> = std::collections::BTreeSet::new();
    for k in map_a.keys() {
        all_keys.insert(*k);
    }
    for k in map_b.keys() {
        all_keys.insert(*k);
    }

    match args.format {
        Format::Jsonl => {
            for (kind, key) in all_keys {
                match (map_a.get(&(kind, key)), map_b.get(&(kind, key))) {
                    (None, Some(b)) => {
                        let val_b: serde_json::Value =
                            serde_json::from_str(&b.value_json).unwrap_or(serde_json::Value::Null);
                        println!(
                            "{}",
                            serde_json::json!({
                                "diff": "+",
                                "kind": kind,
                                "key": key,
                                "run_a": null,
                                "run_b": val_b,
                            })
                        );
                    }
                    (Some(a), None) => {
                        let val_a: serde_json::Value =
                            serde_json::from_str(&a.value_json).unwrap_or(serde_json::Value::Null);
                        println!(
                            "{}",
                            serde_json::json!({
                                "diff": "-",
                                "kind": kind,
                                "key": key,
                                "run_a": val_a,
                                "run_b": null,
                            })
                        );
                    }
                    (Some(a), Some(b)) => {
                        if a.value_json != b.value_json {
                            let val_a: serde_json::Value = serde_json::from_str(&a.value_json)
                                .unwrap_or(serde_json::Value::Null);
                            let val_b: serde_json::Value = serde_json::from_str(&b.value_json)
                                .unwrap_or(serde_json::Value::Null);
                            println!(
                                "{}",
                                serde_json::json!({
                                    "diff": "~",
                                    "kind": kind,
                                    "key": key,
                                    "run_a": val_a,
                                    "run_b": val_b,
                                })
                            );
                        }
                    }
                    (None, None) => {}
                }
            }
        }
        Format::Table => {
            let mut diff_count = 0;
            println!("{:<4} {:<24} {:<42} CHANGE", "DIFF", "KIND", "KEY");
            for (kind, key) in all_keys {
                match (map_a.get(&(kind, key)), map_b.get(&(kind, key))) {
                    (None, Some(b)) => {
                        diff_count += 1;
                        let detail = describe_state_summary(&b.value_json, "added");
                        println!("{:<4} {:<24} {:<42} {}", "+", kind, key, detail);
                    }
                    (Some(_a), None) => {
                        diff_count += 1;
                        println!("{:<4} {:<24} {:<42} removed", "-", kind, key);
                    }
                    (Some(a), Some(b)) => {
                        if a.value_json != b.value_json {
                            diff_count += 1;
                            let change_desc = describe_diff_change(&a.value_json, &b.value_json);
                            println!("{:<4} {:<24} {:<42} {}", "~", kind, key, change_desc);
                        }
                    }
                    (None, None) => {}
                }
            }
            if diff_count == 0 {
                println!("no differences between run {run_a} and run {run_b}");
            }
        }
    }
    Ok(())
}

fn describe_state_summary(val_str: &str, prefix: &str) -> String {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(val_str) else {
        return prefix.to_string();
    };
    if let Some(flags) = v.get("flags").and_then(|f| f.as_array())
        && !flags.is_empty()
    {
        let flag_strs: Vec<&str> = flags.iter().filter_map(|s| s.as_str()).collect();
        return format!("{prefix} [{}]", flag_strs.join(", "));
    }
    prefix.to_string()
}

fn describe_diff_change(old_str: &str, new_str: &str) -> String {
    let Ok(old_v) = serde_json::from_str::<serde_json::Value>(old_str) else {
        return "content changed".to_string();
    };
    let Ok(new_v) = serde_json::from_str::<serde_json::Value>(new_str) else {
        return "content changed".to_string();
    };

    let mut changes = Vec::new();
    if old_v.get("sha256") != new_v.get("sha256") {
        changes.push("sha256 changed");
    }
    if old_v.get("decision") != new_v.get("decision") {
        let old_d = old_v
            .get("decision")
            .and_then(|d| d.as_str())
            .unwrap_or("none");
        let new_d = new_v
            .get("decision")
            .and_then(|d| d.as_str())
            .unwrap_or("none");
        return format!("decision: {old_d} -> {new_d}");
    }
    if old_v.get("flags") != new_v.get("flags") {
        changes.push("flags changed");
    }
    if old_v.get("owner_pkg") != new_v.get("owner_pkg") {
        changes.push("package owner changed");
    }
    if changes.is_empty() {
        "metadata changed".to_string()
    } else {
        changes.join(", ")
    }
}

fn hostname() -> String {
    fs::read_to_string("/etc/hostname")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_selection() {
        let all = ["all".to_string()];
        let pkg = ["pkg".to_string()];
        let exact = ["shell.history".to_string()];
        assert!(selected("chromium.history", &all));
        assert!(selected("pkg.apt", &pkg) && selected("pkg.dpkg", &pkg));
        assert!(!selected("pkgx.foo", &pkg));
        assert!(selected("shell.history", &exact) && !selected("shell.historyx", &exact));
        assert!(!selected("auth.journald", &[]));
    }
}
