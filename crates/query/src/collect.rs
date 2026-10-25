//! The collect orchestration: discovery, per-instance collection, ingest, correlation.
//! Both the CLI and the TUI drive it, optionally receiving progress events.

use anyhow::{Context, Result, bail};
use fp_collect::collector::UserTarget;
use fp_collect::{CollectCtx, Collector, Env};
use fp_core::{Availability, SourceInstance};
use fp_store::{Batch, Store};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

#[derive(Debug, Clone, Default)]
pub struct CollectParams {
    /// raw time expressions, parsed with [`crate::time`]
    pub since: Option<String>,
    pub until: Option<String>,
    /// source kinds or families (`chromium`, `pkg`, `shell.history`); `all` selects everything
    pub sources: Vec<String>,
    pub exclude: Vec<String>,
    pub assume_tz: Option<String>,
    pub homes: Vec<PathBuf>,
    pub all_users: bool,
    pub root: PathBuf,
    /// opt-in collectors: excluded from `--sources all`
    pub with_live: bool,
    pub with_scanners: bool,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct IngestTotals {
    pub events_new: usize,
    pub events_known: usize,
    pub states: usize,
    pub artifacts: usize,
}

#[derive(Debug, Clone)]
pub struct CollectSummary {
    pub run_id: u64,
    pub totals: IngestTotals,
    pub sources_read: usize,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum CollectEvent {
    InstanceStarted {
        label: String,
    },
    InstanceDone {
        label: String,
        totals: IngestTotals,
        warnings: Vec<String>,
        status: Availability,
    },
    InstanceError {
        label: String,
        error: String,
    },
    CorrelateFailed(String),
    Finished(CollectSummary),
    /// the run itself failed (bad arguments, an unusable case); no `Finished` follows
    Failed(String),
}

/// `chromium` selects `chromium.history`; `all` selects everything.
pub fn selected(kind: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|p| {
        p == "all"
            || kind == p
            || kind
                .strip_prefix(p.as_str())
                .is_some_and(|rest| rest.starts_with('.'))
    })
}

fn label(inst: &SourceInstance) -> String {
    match &inst.profile {
        Some(p) => format!("{} {}/{p}", inst.kind, inst.app),
        None => format!("{} {} ({})", inst.kind, inst.app, inst.path.display()),
    }
}

fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}

fn build_env(params: &CollectParams) -> Result<Env> {
    let mut env = Env::current();
    // first: homes are named through the root's own /etc/passwd
    if !params.root.as_os_str().is_empty() {
        env.root = params.root.clone();
    }
    if !params.homes.is_empty() {
        env.home = params.homes[0].clone();
        env.user_targets.clear();
        for h in &params.homes {
            env.add_user_home(h.clone(), None, None);
        }
    }
    if params.all_users {
        if env.euid != 0 && env.uid != 0 {
            bail!("--all-users requires root privileges");
        }
        env.load_all_users()?;
    }
    Ok(env)
}

/// One source instance to collect; `owner` is the user whose home it was found in.
struct Job {
    collector: usize,
    inst: SourceInstance,
    owner: Option<UserTarget>,
}

/// What one worker collected; the main thread is the single writer (ingest, status).
struct Outcome {
    inst: SourceInstance,
    owner: Option<UserTarget>,
    label: String,
    result: std::result::Result<fp_collect::Sink, String>,
}

/// Collects one source instance on a worker. No store access: the source row was
/// inserted during discovery, so the id is already assigned for the staging dir.
fn collect_only(
    job: Job,
    collector: &dyn Collector,
    ctx: &CollectCtx,
    progress: Option<&Sender<CollectEvent>>,
) -> Outcome {
    let label = label(&job.inst);
    let result = if crate::interrupt::interrupted() {
        Err(NOT_COLLECTED.to_string())
    } else {
        if let Some(p) = progress {
            let _ = p.send(CollectEvent::InstanceStarted {
                label: label.clone(),
            });
        }
        let mut sink = fp_collect::Sink::default();
        // evidence is hostile: a parser panic fails this source, not the run
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            collector.collect(&job.inst, ctx, &mut sink)
        })) {
            Ok(Ok(())) => Ok(sink),
            Ok(Err(e)) => Err(format!("{e:#}")),
            Err(panic) => Err(format!("collector panicked: {}", panic_text(&panic))),
        }
    };
    Outcome {
        inst: job.inst,
        owner: job.owner,
        label,
        result,
    }
}

const NOT_COLLECTED: &str = "not collected: interrupted";

fn panic_text(panic: &Box<dyn std::any::Any + Send>) -> &str {
    panic
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("unknown panic")
}

/// Events found in a user's home belong to that user unless the source said otherwise.
fn stamp_owner(events: &mut [fp_core::Event], owner: &UserTarget) {
    for event in events {
        let actor = event.actor.get_or_insert_with(fp_core::Actor::default);
        actor.uid.get_or_insert(owner.uid);
        actor.user.get_or_insert_with(|| owner.user.clone());
    }
}

/// Runs one collection into `case`, reporting per-instance progress when a sender is given.
pub fn run_collect(
    case: &Path,
    params: &CollectParams,
    progress: Option<&Sender<CollectEvent>>,
) -> Result<CollectSummary> {
    let since = params
        .since
        .as_deref()
        .map(crate::time::parse)
        .transpose()?;
    let until = params
        .until
        .as_deref()
        .map(crate::time::parse)
        .transpose()?;
    if let (Some(lo), Some(hi)) = (since, until)
        && lo > hi
    {
        bail!("--since is after --until");
    }
    // an empty selection means everything, like the CLI's `--sources all` default
    let sources = if params.sources.is_empty() {
        vec!["all".to_string()]
    } else {
        params.sources.clone()
    };
    let explicit = |kind: &str| {
        sources
            .iter()
            .filter(|s| *s != "all")
            .any(|s| selected(kind, std::slice::from_ref(s)))
    };
    let collectors: Vec<Box<dyn Collector>> = fp_collect::registry()
        .into_iter()
        .filter(|c| match c.kind() {
            "live.state" => params.with_live || explicit("live.state"),
            "scan.import" => params.with_scanners || explicit("scan.import"),
            _ => true,
        })
        .filter(|c| selected(c.kind(), &sources) && !selected(c.kind(), &params.exclude))
        .collect();
    if collectors.is_empty() {
        bail!("no source kind matches --sources/--exclude");
    }
    let env = build_env(params)?;

    let mut store = Store::open_or_create(case)?;
    hand_case_to_sudo_uid(case);
    let _interrupt = crate::interrupt::arm();
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let run_id = store.insert_run(&fp_store::NewRun {
        host: &hostname(),
        user: &std::env::var("USER").unwrap_or_default(),
        uid: env.uid,
        euid: env.euid,
        version: env!("CARGO_PKG_VERSION"),
        tz: &chrono::Local::now().offset().to_string(),
        args: &argv.join(" "),
    })?;

    let temp_dir = tempfile::Builder::new()
        .prefix("footprint-")
        .tempdir()
        .context("create temp dir")?;
    std::fs::set_permissions(temp_dir.path(), std::fs::Permissions::from_mode(0o700))?;
    let ctx = CollectCtx {
        run_id,
        since,
        until,
        assume_tz: params.assume_tz.clone(),
        temp_dir: temp_dir.path().to_path_buf(),
    };

    // discovery stays sequential (it is cheap) and assigns every source row up front,
    // so workers never touch the store; collection of instances runs in parallel and
    // the main thread is the single writer
    let mut jobs = Vec::new();
    for (ci, collector) in collectors.iter().enumerate() {
        let scopes: Vec<(Option<Env>, Option<UserTarget>)> = if collector.per_user() {
            env.user_targets
                .iter()
                .map(|t| (Some(env.for_user(t)), Some(t.clone())))
                .collect()
        } else {
            vec![(None, None)]
        };
        for (scope, owner) in scopes {
            for mut inst in collector.discover(scope.as_ref().unwrap_or(&env)) {
                inst.id = store.insert_source(run_id, &inst)?;
                if inst.status == Availability::Present {
                    jobs.push(Job {
                        collector: ci,
                        inst,
                        owner: owner.clone(),
                    });
                }
            }
        }
    }
    let read = jobs.len();

    let (job_tx, job_rx) = std::sync::mpsc::channel::<Job>();
    let (outcome_tx, outcome_rx) = std::sync::mpsc::channel::<Outcome>();
    let job_rx = std::sync::Arc::new(std::sync::Mutex::new(job_rx));
    let collectors = std::sync::Arc::new(collectors);
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .clamp(1, 8)
        .min(read.max(1));
    let mut handles = Vec::new();
    for _ in 0..workers {
        let job_rx = job_rx.clone();
        let outcome_tx = outcome_tx.clone();
        let progress_tx = progress.cloned();
        let ctx = ctx.clone();
        let collectors = collectors.clone();
        handles.push(std::thread::spawn(move || {
            loop {
                let job = job_rx
                    .lock()
                    .map(|rx| rx.recv())
                    .ok()
                    .and_then(|received| received.ok());
                let Some(job) = job else {
                    break;
                };
                let collector = collectors[job.collector].as_ref();
                let _ = outcome_tx.send(collect_only(job, collector, &ctx, progress_tx.as_ref()));
            }
        }));
    }
    drop(job_rx);
    // workers check the interrupt flag before each job, so queuing them all up front is fine
    for job in jobs {
        let _ = job_tx.send(job);
    }
    drop(job_tx);
    drop(outcome_tx); // workers hold the only remaining senders; iter() ends when they exit

    let mut errors = Vec::new();
    let mut totals = IngestTotals::default();
    let mut interrupted = false;
    for outcome in outcome_rx.iter() {
        let Outcome {
            inst,
            owner,
            label,
            result,
        } = outcome;
        let mut sink = match result {
            Ok(sink) => sink,
            Err(error) => {
                store.update_source_status(inst.id, &Availability::Error(error.clone()), None)?;
                if error == NOT_COLLECTED {
                    interrupted = true;
                    continue;
                }
                if let Some(p) = progress {
                    let _ = p.send(CollectEvent::InstanceError {
                        label: label.clone(),
                        error: error.clone(),
                    });
                }
                errors.push(format!("{label}: {error}"));
                continue;
            }
        };
        if let Some(owner) = &owner {
            stamp_owner(&mut sink.events, owner);
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
        let status = if !sink.warnings.is_empty() {
            store.update_source_status(
                inst.id,
                &Availability::Partial,
                Some(&sink.warnings.join("; ")),
            )?;
            Availability::Partial
        } else if stats.events_new == 0
            && stats.events_known == 0
            && stats.states == 0
            && stats.artifacts == 0
        {
            store.update_source_status(inst.id, &Availability::Empty, Some("empty"))?;
            Availability::Empty
        } else {
            Availability::Present
        };
        if let Some(p) = progress {
            let _ = p.send(CollectEvent::InstanceDone {
                label,
                totals: IngestTotals {
                    events_new: stats.events_new,
                    events_known: stats.events_known,
                    states: stats.states,
                    artifacts: stats.artifacts,
                },
                warnings: sink.warnings,
                status,
            });
        }
    }
    for handle in handles {
        let _ = handle.join();
    }
    if interrupted {
        errors.push("interrupted by user; some sources were not collected".into());
    }

    if let Err(e) = fp_correlate::correlate(&mut store, &env.root) {
        if let Some(p) = progress {
            let _ = p.send(CollectEvent::CorrelateFailed(format!("{e:#}")));
        }
        errors.push(format!("correlate: {e:#}"));
    }

    store.finish_run(run_id, &errors)?;
    let summary = CollectSummary {
        run_id,
        totals,
        sources_read: read,
        errors,
    };
    if let Some(p) = progress {
        let _ = p.send(CollectEvent::Finished(summary.clone()));
    }
    Ok(summary)
}

/// Running under sudo, the case must belong to the user who invoked it.
fn hand_case_to_sudo_uid(case: &Path) {
    use std::os::unix::ffi::OsStrExt;
    let Ok(sudo_uid) = std::env::var("SUDO_UID") else {
        return;
    };
    let Ok(uid) = sudo_uid.parse::<libc::uid_t>() else {
        return;
    };
    // only root can chown; as a regular user this is a no-op
    if unsafe { libc::geteuid() } != 0 {
        return;
    }
    let path = std::ffi::CString::new(case.as_os_str().as_bytes()).ok();
    if let Some(path) = path {
        let _ = unsafe { libc::chown(path.as_ptr(), uid, u32::MAX) };
    }
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
