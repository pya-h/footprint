use anyhow::Result;
use chrono::Local;
use clap::{Parser, Subcommand};
use fp_collect::{chromium::ChromiumCollector, CollectCtx, Collector, Env};
use fp_store::Store;
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

    let collectors: Vec<Box<dyn Collector>> = if sources_arg == "all" {
        vec![Box::new(ChromiumCollector::new())]
    } else {
        let wanted: std::collections::HashSet<&str> = sources_arg.split(',').collect();
        let mut out: Vec<Box<dyn Collector>> = Vec::new();
        if wanted.contains("chromium") {
            out.push(Box::new(ChromiumCollector::new()));
        }
        out
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

    for collector in &collectors {
        for inst in &all_sources {
            if inst.kind != collector.kind() {
                continue;
            }
            let _stats = collector.collect(inst, &ctx, &fp_collect::Sink::new())?;
        }
    }

    store.finish_run(run_id, &[])?;
    println!("run {run_id}: collected into {}", store.path().display());
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

