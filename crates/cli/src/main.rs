use anyhow::Result;
use chrono::{DateTime, Local, Utc};
use clap::{Parser, Subcommand};
use fp_collect::{
    auth::AuthCollector,
    chromium::ChromiumCollector,
    desktop::DesktopCollector,
    pkg::PkgCollector,
    shell::ShellCollector,
    xdm::XdmCollector,
    CollectCtx, Collector, Env,
};
use fp_store::{EventFilter, Store};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

mod time;

#[derive(Parser)]
#[command(name = "footprint")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    cmd: Command,
}

#[derive(Subcommand)]
enum Command {
    Collect {
        #[arg(short, long, default_value = "footprint.db")]
        case: PathBuf,
        #[arg(short, long)]
        since: Option<String>,
        #[arg(short, long)]
        until: Option<String>,
        #[arg(long, default_value = "all")]
        sources: String,
        #[arg(long, value_delimiter = ',')]
        exclude: Vec<String>,
        #[arg(long)]
        assume_tz: Option<String>,
        #[arg(long)]
        home: Option<PathBuf>,
    },
    Sources {
        #[arg(short, long, default_value = "footprint.db")]
        case: PathBuf,
    },
    List {
        #[arg(short, long, default_value = "footprint.db")]
        case: PathBuf,
        #[arg(short, long)]
        since: Option<String>,
        #[arg(short, long)]
        until: Option<String>,
        #[arg(long, value_delimiter = ',')]
        kind: Vec<String>,
        #[arg(long)]
        host: Option<String>,
        #[arg(long)]
        r#match: Option<String>,
        #[arg(short, long)]
        limit: Option<usize>,
        #[arg(long)]
        reverse: bool,
        #[arg(long, default_value = "table")]
        format: String,
    },
    Show {
        #[arg(short, long, default_value = "footprint.db")]
        case: PathBuf,
        id: u64,
    },
    Search {
        #[arg(short, long, default_value = "footprint.db")]
        case: PathBuf,
        query: String,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Command::Collect {
            case,
            since,
            until,
            sources,
            exclude,
            assume_tz,
            home,
        } => collect(
            case,
            since.as_deref(),
            until.as_deref(),
            &sources,
            &exclude,
            assume_tz,
            home,
        ),
        Command::Sources { case } => sources(case),
        Command::List {
            case,
            since,
            until,
            kind,
            host,
            r#match,
            limit,
            reverse,
            format,
        } => list(case, since, until, kind, host, r#match, limit, reverse, format),
        Command::Show { case, id } => show(case, id),
        Command::Search { case, query } => search(case, query),
    }
}

fn collect(
    case: PathBuf,
    since: Option<&str>,
    until: Option<&str>,
    sources_arg: &str,
    _exclude: &[String],
    assume_tz: Option<String>,
    home: Option<PathBuf>,
) -> Result<()> {
    let since_us = since.map(time::parse).transpose()?;
    let until_us = until.map(time::parse).transpose()?;

    let mut store = if case.exists() {
        Store::open(&case)?
    } else {
        Store::create(&case)?
    };

    let env = match home {
        Some(h) => Env {
            home: h,
            uid: unsafe { libc::getuid() },
            euid: unsafe { libc::geteuid() },
        },
        None => Env::current(),
    };
    let run_id = store.insert_run(
        &hostname(),
        &current_user(),
        env.uid,
        env.euid,
        env!("CARGO_PKG_VERSION"),
        &Local::now().offset().to_string(),
        "collect",
    )?;

    let temp_dir = tempfile::tempdir()?;
    fs::set_permissions(temp_dir.path(), fs::Permissions::from_mode(0o700))?;

    let ctx = CollectCtx {
        run_id,
        since: since_us,
        until: until_us,
        assume_tz,
        temp_dir: temp_dir.path().to_path_buf(),
        deny_list: fp_collect::secret_deny_list(),
    };

    let all_collectors: Vec<Box<dyn Collector>> = vec![
        Box::new(ChromiumCollector::new()),
        Box::new(XdmCollector::new()),
        Box::new(PkgCollector::new()),
        Box::new(AuthCollector::new()),
        Box::new(ShellCollector::new()),
        Box::new(DesktopCollector::new()),
    ];

    let collectors: Vec<Box<dyn Collector>> = if sources_arg == "all" {
        all_collectors
    } else {
        let wanted: std::collections::HashSet<&str> = sources_arg.split(',').collect();
        all_collectors
            .into_iter()
            .filter(|c| wanted.contains(c.kind()))
            .collect()
    };

    let mut all_sources = Vec::new();
    for collector in &collectors {
        let mut discovered = collector.discover(&env);
        for inst in &mut discovered {
            let db_id = store.insert_source(run_id, inst)?;
            inst.id = db_id;
        }
        all_sources.extend(discovered);
    }

    let sink = fp_collect::Sink::new();
    for collector in &collectors {
        for inst in &all_sources {
            if inst.kind != collector.kind() {
                continue;
            }
            collector.collect(inst, &ctx, &sink)?;
        }
    }

    let stats = sink.clone().into_stats();

    for event in sink.drain_events() {
        store.insert_event(&event)?;
    }
    for state in sink.drain_states() {
        store.insert_state(&state)?;
    }
    for artifact in sink.drain_artifacts() {
        store.insert_artifact(run_id, &artifact)?;
    }

    store.finish_run(run_id, &stats.errors)?;
    println!(
        "run {run_id}: {} events, {} states, {} artifacts into {}",
        stats.events,
        stats.states,
        stats.artifacts,
        store.path().display()
    );
    Ok(())
}

fn sources(case: PathBuf) -> Result<()> {
    let store = Store::open(&case)?;
    let rows = store.sources(None)?;
    for (id, inst) in rows {
        let status = serde_json::to_string(&inst.status)?;
        println!(
            "{}\t{}\t{}\t{}\t{}\t{}",
            id,
            inst.kind,
            inst.app,
            inst.profile.as_deref().unwrap_or("-"),
            inst.path.display(),
            status
        );
    }
    Ok(())
}

fn hostname() -> String {
    fs::read_to_string("/etc/hostname")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}

fn current_user() -> String {
    std::env::var("USER").unwrap_or_else(|_| "unknown".to_string())
}

fn list(
    case: PathBuf,
    since: Option<String>,
    until: Option<String>,
    kinds: Vec<String>,
    host: Option<String>,
    match_text: Option<String>,
    limit: Option<usize>,
    reverse: bool,
    format: String,
) -> Result<()> {
    let store = Store::open(&case)?;
    let filter = EventFilter {
        since: since.map(|s| time::parse(&s)).transpose()?,
        until: until.map(|s| time::parse(&s)).transpose()?,
        kinds,
        host,
        match_text,
        limit,
        reverse,
    };
    let mut rows = store.list_events(&filter)?;
    if filter.reverse {
        rows.reverse();
    }
    if format == "jsonl" {
        for row in rows {
            println!("{}", serde_json::to_string(&row)?);
        }
    } else {
        for row in rows {
            let local = DateTime::<Utc>::from_timestamp_micros(row.ts_utc)
                .unwrap_or_else(|| Utc::now())
                .with_timezone(&Local);
            let source = format_source(&row.app, &row.profile);
            println!(
                "{}\t{}\t{}\t{}",
                local.format("%Y-%m-%d %H:%M:%S"),
                source,
                row.kind,
                row.summary.as_deref().unwrap_or(&row.subject.unwrap_or_default())
            );
        }
    }
    Ok(())
}

fn show(case: PathBuf, id: u64) -> Result<()> {
    let store = Store::open(&case)?;
    let ev = store.get_event(id)?.ok_or_else(|| anyhow::anyhow!("event not found"))?;
    let local = DateTime::<Utc>::from_timestamp_micros(ev.ts_utc)
        .unwrap_or_else(|| Utc::now())
        .with_timezone(&Local);
    println!("id:          {}", ev.id);
    println!("run_id:      {}", ev.run_id);
    println!("time (utc):  {}", local.format("%Y-%m-%d %H:%M:%S%.6f %:z"));
    println!("kind:        {}", ev.kind);
    println!("source:      {}", format_source(&ev.app, &ev.profile));
    println!("path:        {}", ev.path.as_deref().unwrap_or("-"));
    println!("subject:     {}", ev.subject.as_deref().unwrap_or("-"));
    println!("host:        {}", ev.subject_host.as_deref().unwrap_or("-"));
    println!("summary:     {}", ev.summary.as_deref().unwrap_or("-"));
    println!("confidence:  {}", ev.confidence);
    println!("raw:         {} {}", ev.ts_raw_format, ev.ts_raw);
    println!("locator:     {}", ev.locator);
    println!("actor:       {}", ev.actor_json);
    println!("attrs:       {}", ev.attrs_json);
    Ok(())
}

fn search(case: PathBuf, query: String) -> Result<()> {
    let store = Store::open(&case)?;
    let rows = store.search_events(&query)?;
    for row in rows {
        let local = DateTime::<Utc>::from_timestamp_micros(row.ts_utc)
            .unwrap_or_else(|| Utc::now())
            .with_timezone(&Local);
        let source = format_source(&row.app, &row.profile);
        println!(
            "{}\t{}\t{}\t{}",
            local.format("%Y-%m-%d %H:%M:%S"),
            source,
            row.kind,
            row.summary.as_deref().unwrap_or(&row.subject.unwrap_or_default())
        );
    }
    Ok(())
}

fn format_source(app: &Option<String>, profile: &Option<String>) -> String {
    match (app, profile) {
        (Some(a), Some(p)) => format!("{a}/{p}"),
        (Some(a), None) => a.clone(),
        _ => "-".to_string(),
    }
}

