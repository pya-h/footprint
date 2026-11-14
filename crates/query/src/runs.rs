use anyhow::Result;
use fp_store::Store;

/// One collect run as the run pickers and the runs table show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunView {
    pub id: u64,
    pub started_utc: i64,
    pub finished_utc: Option<i64>,
    /// Wall time of a finished run in microseconds; `None` while it is still running.
    pub duration_utc: Option<i64>,
    pub args: Option<String>,
    pub errors: Vec<String>,
    /// Events this run was the first to store, not every event it observed.
    pub events_new: u64,
}

/// Every run, oldest first, with its stored-event count and parsed error list.
pub fn runs(store: &Store) -> Result<Vec<RunView>> {
    let counts: std::collections::HashMap<u64, u64> =
        store.events_stored_per_run()?.into_iter().collect();
    Ok(store
        .list_runs()?
        .into_iter()
        .map(|run| RunView {
            id: run.id,
            started_utc: run.started_utc,
            finished_utc: run.finished_utc,
            duration_utc: run
                .finished_utc
                .map(|finished| finished.saturating_sub(run.started_utc)),
            args: run.args,
            errors: run
                .errors_json
                .as_deref()
                .and_then(|json| serde_json::from_str(json).ok())
                .unwrap_or_default(),
            events_new: counts.get(&run.id).copied().unwrap_or(0),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_views_carry_duration_errors_and_event_counts() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open_or_create(dir.path().join("case.db")).unwrap();
        let run = store
            .insert_run(&fp_store::NewRun {
                host: "h",
                user: "u",
                uid: 1,
                euid: 1,
                version: "0",
                tz: "UTC",
                args: "collect --since -1d",
            })
            .unwrap();
        store
            .conn()
            .execute(
                "INSERT INTO sources (run_id, kind, status) VALUES (?1, 'shell.history', 'ok')",
                [run],
            )
            .unwrap();
        store
            .conn()
            .execute(
                "INSERT INTO events (run_id, source_id, ts_utc, ts_precision, kind, dedup_key)
                 VALUES (?1, 1, 10, 'exact', 'shell.command', 'a'),
                        (?1, 1, 20, 'exact', 'shell.command', 'b')",
                [run],
            )
            .unwrap();

        // unfinished run: no duration, no errors
        let views = runs(&store).unwrap();
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].events_new, 2);
        assert_eq!(views[0].duration_utc, None);
        assert!(views[0].errors.is_empty());

        store.finish_run(run, &["journald partial".into()]).unwrap();
        store
            .conn()
            .execute(
                "INSERT INTO events (run_id, source_id, ts_utc, ts_precision, kind, dedup_key)
                 VALUES (?1, 1, 30, 'exact', 'shell.command', 'c')",
                [run],
            )
            .unwrap();
        let views = runs(&store).unwrap();
        assert_eq!(views[0].events_new, 3);
        assert!(views[0].duration_utc.is_some_and(|d| d >= 0));
        assert_eq!(views[0].errors, ["journald partial"]);
    }
}
