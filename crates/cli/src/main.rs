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
    /// hosts, each also matching its subdomains
    #[arg(long, value_delimiter = ',')]
    host: Vec<String>,
    /// substring of subject, summary or attributes
    #[arg(short, long)]
    r#match: Option<String>,
    #[arg(short, long)]
    limit: Option<usize>,
    /// newest first
    #[arg(short, long)]
    reverse: bool,
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
        limit: args.limit,
        reverse: args.reverse,
    };
    print_rows(store.list_events(&filter)?, args.format)
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
    Ok(())
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
