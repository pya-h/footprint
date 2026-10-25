use fp_core::{Artifact, Event, Link, SourceInstance, SourceStatus, StateRecord};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Row, params, params_from_iter};
use std::fs::OpenOptions;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;

/// Bumped whenever a table changes shape; older case files are refused rather than half-read.
const SCHEMA_VERSION: i64 = 2;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("case file not found: {0}")]
    NotFound(PathBuf),
    #[error(
        "case file schema v{found} is not supported (expected v{SCHEMA_VERSION}); start a new case"
    )]
    SchemaVersion { found: i64 },
}

pub struct Store {
    conn: Connection,
    path: PathBuf,
}

pub struct NewRun<'a> {
    pub host: &'a str,
    pub user: &'a str,
    pub uid: u32,
    pub euid: u32,
    pub version: &'a str,
    pub tz: &'a str,
    pub args: &'a str,
}

/// What one collected batch added to the case.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct IngestStats {
    pub events_new: usize,
    pub events_known: usize,
    pub states: usize,
    pub artifacts: usize,
}

pub struct Batch<'a> {
    pub source_identity: &'a str,
    pub events: &'a [Event],
    pub states: &'a [StateRecord],
    pub artifacts: &'a [Artifact],
}

impl Store {
    /// Opens the case, creating it with mode `0600` if it doesn't exist.
    pub fn open_or_create(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref();
        if !path.exists() {
            if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent)?;
            }
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)?;
        }
        Self::open_with(path, OpenFlags::SQLITE_OPEN_READ_WRITE)
    }

    /// Opens an existing case; never creates one.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref();
        if !path.exists() {
            return Err(StoreError::NotFound(path.to_path_buf()));
        }
        Self::open_with(path, OpenFlags::SQLITE_OPEN_READ_WRITE)
    }

    fn open_with(path: &Path, flags: OpenFlags) -> Result<Self, StoreError> {
        let conn = Connection::open_with_flags(path, flags | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
        // parallel collection writes from several connections; wait briefly instead of failing
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let store = Self {
            conn,
            path: path.to_path_buf(),
        };
        store.init()?;
        Ok(store)
    }

    fn init(&self) -> Result<(), StoreError> {
        let version: i64 = self
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))?;
        let has_tables: bool = self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE name = 'events')",
            [],
            |r| r.get(0),
        )?;
        match (version, has_tables) {
            (SCHEMA_VERSION, _) => {
                self.conn.execute(
                    "CREATE UNIQUE INDEX IF NOT EXISTS links_uniq ON links(src_type, src_id, rel, dst_type, dst_id)",
                    [],
                )?;
                Ok(())
            }
            (0, false) => {
                self.conn.execute_batch(SCHEMA)?;
                self.conn
                    .pragma_update(None, "user_version", SCHEMA_VERSION)?;
                Ok(())
            }
            (found, _) => Err(StoreError::SchemaVersion { found }),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn insert_run(&mut self, run: &NewRun) -> Result<u64, StoreError> {
        self.conn.execute(
            "INSERT INTO runs (started_utc, host, user, uid, euid, tool_version, tz, args)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                now_micros(),
                run.host,
                run.user,
                run.uid,
                run.euid,
                run.version,
                run.tz,
                run.args
            ],
        )?;
        Ok(self.conn.last_insert_rowid() as u64)
    }

    pub fn finish_run(&mut self, run_id: u64, errors: &[String]) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE runs SET finished_utc = ?1, errors_json = ?2 WHERE id = ?3",
            params![now_micros(), serde_json::to_string(errors)?, run_id],
        )?;
        Ok(())
    }

    pub fn insert_source(&mut self, run_id: u64, inst: &SourceInstance) -> Result<u64, StoreError> {
        self.conn.execute(
            "INSERT INTO sources (run_id, kind, app, profile, path, status, note)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                run_id,
                inst.kind,
                inst.app,
                inst.profile,
                inst.path.to_string_lossy(),
                serde_json::to_string(&inst.status)?,
                inst.note,
            ],
        )?;
        Ok(self.conn.last_insert_rowid() as u64)
    }

    pub fn update_source_status(
        &mut self,
        id: u64,
        status: &SourceStatus,
        note: Option<&str>,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE sources SET status = ?1, note = COALESCE(?2, note) WHERE id = ?3",
            params![serde_json::to_string(status)?, note, id],
        )?;
        Ok(())
    }

    /// Writes one source's output in a single transaction; events already in the case are skipped.
    pub fn ingest(&mut self, run_id: u64, batch: &Batch) -> Result<IngestStats, StoreError> {
        let tx = self.conn.transaction()?;
        let mut stats = IngestStats::default();
        {
            let mut ev = tx.prepare_cached(INSERT_EVENT)?;
            for event in batch.events {
                let inserted = ev.execute(params![
                    event.run_id,
                    event.source_inst,
                    event.ts_utc,
                    event.ts_precision.to_string(),
                    event.ts_raw,
                    event.ts_raw_format.to_string(),
                    event.ts_tz_assumed,
                    event.kind.as_str(),
                    serde_json::to_string(&event.actor)?,
                    event.subject,
                    event.subject_host,
                    event.summary,
                    serde_json::to_string(&event.attrs)?,
                    event.provenance.locator,
                    event.provenance.file.to_string_lossy(),
                    event.provenance.file_sha256,
                    event.confidence.to_string(),
                    event.dedup_key(batch.source_identity),
                ])?;
                if inserted == 1 {
                    stats.events_new += 1;
                } else {
                    stats.events_known += 1;
                }
            }
            let mut st = tx.prepare_cached(INSERT_STATE)?;
            for record in batch.states {
                st.execute(params![
                    record.run_id,
                    record.kind,
                    record.key,
                    serde_json::to_string(&record.value)?,
                    record.provenance.locator,
                ])?;
                stats.states += 1;
            }
            let mut art = tx.prepare_cached(INSERT_ARTIFACT)?;
            for artifact in batch.artifacts {
                art.execute(params![
                    artifact.path.to_string_lossy(),
                    artifact.sha256,
                    artifact.size,
                    artifact.mtime_ns,
                    artifact.ctime_ns,
                    artifact.btime_ns,
                    artifact.owner_pkg,
                    run_id,
                ])?;
                stats.artifacts += 1;
            }
        }
        tx.commit()?;
        Ok(stats)
    }

    pub fn insert_link(&mut self, link: &Link) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT OR IGNORE INTO links (src_type, src_id, rel, dst_type, dst_id, confidence, why)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                link.src_type.to_string(),
                link.src_id,
                link.rel,
                link.dst_type.to_string(),
                link.dst_id,
                link.confidence.to_string(),
                link.why,
            ],
        )?;
        Ok(())
    }

    pub fn insert_links(&mut self, links: &[Link]) -> Result<usize, StoreError> {
        let tx = self.conn.transaction()?;
        let mut count = 0;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT OR IGNORE INTO links (src_type, src_id, rel, dst_type, dst_id, confidence, why)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            for link in links {
                count += stmt.execute(params![
                    link.src_type.to_string(),
                    link.src_id,
                    link.rel,
                    link.dst_type.to_string(),
                    link.dst_id,
                    link.confidence.to_string(),
                    link.why,
                ])?;
            }
        }
        tx.commit()?;
        Ok(count)
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    pub fn conn_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    pub fn sources(&self, run_id: Option<u64>) -> Result<Vec<SourceRow>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT s.id, s.run_id, s.kind, s.app, s.profile, s.path, s.status, s.note,
                    (SELECT COUNT(*) FROM events e WHERE e.source_id = s.id)
             FROM sources s
             WHERE ?1 IS NULL OR s.run_id = ?1 ORDER BY s.run_id, s.id",
        )?;
        let rows = stmt.query_map([run_id], |row| {
            Ok(SourceRow {
                id: row.get(0)?,
                run_id: row.get(1)?,
                kind: row.get(2)?,
                app: row.get(3)?,
                profile: row.get(4)?,
                path: row.get(5)?,
                status: row.get(6)?,
                note: row.get(7)?,
                event_count: row.get(8)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn last_run_id(&self) -> Result<Option<u64>, StoreError> {
        Ok(self
            .conn
            .query_row("SELECT MAX(id) FROM runs", [], |r| r.get(0))?)
    }

    pub fn event_count(&self) -> Result<u64, StoreError> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))?)
    }

    fn build_events_query(
        &self,
        base_select: &str,
        filter: &EventFilter,
    ) -> (String, Vec<rusqlite::types::Value>) {
        let mut sql = format!("{base_select} WHERE 1=1");
        let mut args: Vec<rusqlite::types::Value> = Vec::new();

        if let Some(lo) = filter.since {
            sql.push_str(" AND e.ts_utc >= ?");
            args.push(lo.into());
        }
        if let Some(hi) = filter.until {
            sql.push_str(" AND e.ts_utc <= ?");
            args.push(hi.into());
        }
        if !filter.hosts.is_empty() {
            let clauses = vec![
                "(e.subject_host = ? OR e.subject_host LIKE ? ESCAPE '\\')";
                filter.hosts.len()
            ];
            sql.push_str(&format!(" AND ({})", clauses.join(" OR ")));
            for host in &filter.hosts {
                let host = host.to_lowercase();
                let subdomains = format!("%.{}", escape_like(&host));
                args.extend([host.into(), subdomains.into()]);
            }
        }
        if !filter.kinds.is_empty() {
            let clauses = vec!["e.kind LIKE ? ESCAPE '\\'"; filter.kinds.len()];
            sql.push_str(&format!(" AND ({})", clauses.join(" OR ")));
            for kind in &filter.kinds {
                args.push(kind_pattern(kind).into());
            }
        }
        if let Some(text) = &filter.match_text {
            sql.push_str(
                " AND (e.subject LIKE ? ESCAPE '\\' OR e.summary LIKE ? ESCAPE '\\' OR e.attrs_json LIKE ? ESCAPE '\\')",
            );
            let like = format!("%{}%", escape_like(text));
            args.extend([like.clone().into(), like.clone().into(), like.into()]);
        }
        sql.push_str(if filter.reverse {
            " ORDER BY e.ts_utc DESC, e.id DESC"
        } else {
            " ORDER BY e.ts_utc, e.id"
        });
        if let Some(limit) = filter.limit {
            sql.push_str(" LIMIT ?");
            args.push((limit as i64).into());
        }
        if let Some(offset) = filter.offset {
            sql.push_str(" OFFSET ?");
            args.push((offset as i64).into());
        }
        (sql, args)
    }

    pub fn list_events(&self, filter: &EventFilter) -> Result<Vec<EventRow>, StoreError> {
        let (sql, args) = self.build_events_query(EVENT_ROW_SELECT, filter);
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(args), EventRow::from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn list_events_detailed(
        &self,
        filter: &EventFilter,
    ) -> Result<Vec<EventDetail>, StoreError> {
        let (sql, args) = self.build_events_query(EVENT_DETAIL_SELECT, filter);
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(args), EventDetail::from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn search_events(&self, query: &str) -> Result<Vec<EventRow>, StoreError> {
        let mut stmt = self.conn.prepare(&format!(
            "{EVENT_ROW_SELECT} JOIN events_fts f ON f.rowid = e.id
             WHERE events_fts MATCH ?1 ORDER BY e.ts_utc, e.id"
        ))?;
        let rows = stmt.query_map([query], EventRow::from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn get_event(&self, id: u64) -> Result<Option<EventDetail>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT e.id, e.run_id, e.source_id, e.ts_utc, e.ts_precision, e.ts_raw, e.ts_raw_format,
                    e.ts_tz_assumed, e.kind, e.actor_json, e.subject, e.subject_host, e.summary,
                    e.attrs_json, e.locator, e.prov_file, e.prov_sha256, e.confidence,
                    s.kind, s.app, s.profile
             FROM events e JOIN sources s ON s.id = e.source_id
             WHERE e.id = ?1",
        )?;
        let row = stmt
            .query_row([id], |row| {
                Ok(EventDetail {
                    id: row.get(0)?,
                    run_id: row.get(1)?,
                    source_id: row.get(2)?,
                    ts_utc: row.get(3)?,
                    ts_precision: row.get(4)?,
                    ts_raw: row.get(5)?,
                    ts_raw_format: row.get(6)?,
                    ts_tz_assumed: row.get(7)?,
                    kind: row.get(8)?,
                    actor_json: row.get(9)?,
                    subject: row.get(10)?,
                    subject_host: row.get(11)?,
                    summary: row.get(12)?,
                    attrs_json: row.get(13)?,
                    locator: row.get(14)?,
                    prov_file: row.get(15)?,
                    prov_sha256: row.get(16)?,
                    confidence: row.get(17)?,
                    source_kind: row.get(18)?,
                    app: row.get(19)?,
                    profile: row.get(20)?,
                })
            })
            .optional()?;
        Ok(row)
    }

    pub fn get_all_events(&self) -> Result<Vec<EventDetail>, StoreError> {
        let sql = "SELECT e.id, e.run_id, e.source_id, e.ts_utc, e.ts_precision, e.ts_raw, e.ts_raw_format,
                    e.ts_tz_assumed, e.kind, e.actor_json, e.subject, e.subject_host, e.summary,
                    e.attrs_json, e.locator, e.prov_file, e.prov_sha256, e.confidence,
                    s.kind, s.app, s.profile
             FROM events e JOIN sources s ON s.id = e.source_id
             ORDER BY e.ts_utc, e.id";
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map([], |row| {
            Ok(EventDetail {
                id: row.get(0)?,
                run_id: row.get(1)?,
                source_id: row.get(2)?,
                ts_utc: row.get(3)?,
                ts_precision: row.get(4)?,
                ts_raw: row.get(5)?,
                ts_raw_format: row.get(6)?,
                ts_tz_assumed: row.get(7)?,
                kind: row.get(8)?,
                actor_json: row.get(9)?,
                subject: row.get(10)?,
                subject_host: row.get(11)?,
                summary: row.get(12)?,
                attrs_json: row.get(13)?,
                locator: row.get(14)?,
                prov_file: row.get(15)?,
                prov_sha256: row.get(16)?,
                confidence: row.get(17)?,
                source_kind: row.get(18)?,
                app: row.get(19)?,
                profile: row.get(20)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn get_events_by_kinds(&self, kinds: &[&str]) -> Result<Vec<EventDetail>, StoreError> {
        if kinds.is_empty() {
            return self.get_all_events();
        }
        let placeholders = kinds.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT e.id, e.run_id, e.source_id, e.ts_utc, e.ts_precision, e.ts_raw, e.ts_raw_format,
                    e.ts_tz_assumed, e.kind, e.actor_json, e.subject, e.subject_host, e.summary,
                    e.attrs_json, e.locator, e.prov_file, e.prov_sha256, e.confidence,
                    s.kind, s.app, s.profile
             FROM events e JOIN sources s ON s.id = e.source_id
             WHERE e.kind IN ({placeholders})
             ORDER BY e.ts_utc, e.id"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(kinds), |row| {
            Ok(EventDetail {
                id: row.get(0)?,
                run_id: row.get(1)?,
                source_id: row.get(2)?,
                ts_utc: row.get(3)?,
                ts_precision: row.get(4)?,
                ts_raw: row.get(5)?,
                ts_raw_format: row.get(6)?,
                ts_tz_assumed: row.get(7)?,
                kind: row.get(8)?,
                actor_json: row.get(9)?,
                subject: row.get(10)?,
                subject_host: row.get(11)?,
                summary: row.get(12)?,
                attrs_json: row.get(13)?,
                locator: row.get(14)?,
                prov_file: row.get(15)?,
                prov_sha256: row.get(16)?,
                confidence: row.get(17)?,
                source_kind: row.get(18)?,
                app: row.get(19)?,
                profile: row.get(20)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn list_artifacts(&self) -> Result<Vec<ArtifactRow>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, path, sha256, size, mtime_ns, ctime_ns, btime_ns, owner_pkg, first_seen_run
             FROM artifacts ORDER BY id",
        )?;
        let rows = stmt.query_map([], ArtifactRow::from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn get_artifact(&self, id: u64) -> Result<Option<ArtifactRow>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, path, sha256, size, mtime_ns, ctime_ns, btime_ns, owner_pkg, first_seen_run
             FROM artifacts WHERE id = ?1",
        )?;
        Ok(stmt.query_row([id], ArtifactRow::from_row).optional()?)
    }

    pub fn get_artifact_by_path(&self, path: &str) -> Result<Option<ArtifactRow>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, path, sha256, size, mtime_ns, ctime_ns, btime_ns, owner_pkg, first_seen_run
             FROM artifacts WHERE path = ?1",
        )?;
        Ok(stmt.query_row([path], ArtifactRow::from_row).optional()?)
    }

    pub fn update_artifact_metadata(
        &mut self,
        id: u64,
        meta: &ArtifactMeta<'_>,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE artifacts SET
                sha256 = COALESCE(?2, sha256),
                size = COALESCE(?3, size),
                mtime_ns = COALESCE(?4, mtime_ns),
                ctime_ns = COALESCE(?5, ctime_ns),
                btime_ns = COALESCE(?6, btime_ns),
                owner_pkg = COALESCE(?7, owner_pkg)
             WHERE id = ?1",
            params![
                id,
                meta.sha256,
                meta.size,
                meta.mtime_ns,
                meta.ctime_ns,
                meta.btime_ns,
                meta.owner_pkg
            ],
        )?;
        Ok(())
    }

    pub fn update_event_anchoring(
        &mut self,
        id: u64,
        ts_utc: i64,
        ts_precision: &str,
        confidence: &str,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE events SET ts_utc = ?2, ts_precision = ?3, confidence = ?4 WHERE id = ?1",
            params![id, ts_utc, ts_precision, confidence],
        )?;
        Ok(())
    }

    pub fn get_links_for(&self, typ: &str, id: u64) -> Result<Vec<LinkRow>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT src_type, src_id, rel, dst_type, dst_id, confidence, why
             FROM links
             WHERE (src_type = ?1 AND src_id = ?2) OR (dst_type = ?1 AND dst_id = ?2)",
        )?;
        let rows = stmt.query_map(params![typ, id], LinkRow::from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn list_links(&self) -> Result<Vec<LinkRow>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT src_type, src_id, rel, dst_type, dst_id, confidence, why
             FROM links",
        )?;
        let rows = stmt.query_map([], LinkRow::from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn list_state_by_kind(&self, kind: &str) -> Result<Vec<StateRow>, StoreError> {
        self.list_state(None, Some(kind), None)
    }

    pub fn list_state(
        &self,
        run_id: Option<u64>,
        kind: Option<&str>,
        match_text: Option<&str>,
    ) -> Result<Vec<StateRow>, StoreError> {
        let mut sql = String::from(
            "SELECT rowid, run_id, kind, key, value_json, locator
             FROM state WHERE 1=1",
        );
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        if let Some(r) = run_id {
            sql.push_str(" AND run_id = ?");
            params.push(Box::new(r));
        }

        if let Some(k) = kind {
            sql.push_str(" AND kind LIKE ? ESCAPE '\\'");
            params.push(Box::new(kind_pattern(k)));
        }

        if let Some(m) = match_text {
            sql.push_str(" AND (key LIKE ? ESCAPE '\\' OR value_json LIKE ? ESCAPE '\\')");
            let pat = format!("%{}%", escape_like(m));
            params.push(Box::new(pat.clone()));
            params.push(Box::new(pat));
        }

        sql.push_str(" ORDER BY kind ASC, key ASC");

        let mut stmt = self.conn.prepare(&sql)?;
        let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|b| b.as_ref()).collect();
        let rows = stmt.query_map(&param_refs[..], StateRow::from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn insert_ioc(
        &mut self,
        ioc_type: &str,
        value: &str,
        note: Option<&str>,
    ) -> Result<u64, StoreError> {
        let now = now_micros();
        self.conn.execute(
            "INSERT INTO iocs (type, value, note, added_utc) VALUES (?1, ?2, ?3, ?4)",
            params![ioc_type, value, note, now],
        )?;
        Ok(self.conn.last_insert_rowid() as u64)
    }

    pub fn list_iocs(&self) -> Result<Vec<IocRow>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, type, value, note, added_utc FROM iocs ORDER BY id ASC")?;
        let rows = stmt.query_map([], IocRow::from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn delete_ioc(&mut self, id: u64) -> Result<bool, StoreError> {
        let n = self
            .conn
            .execute("DELETE FROM iocs WHERE id = ?1", params![id])?;
        Ok(n > 0)
    }

    pub fn list_runs(&self) -> Result<Vec<RunRow>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, started_utc, finished_utc, host, user, uid, euid, tool_version, tz, args, errors_json
             FROM runs ORDER BY id ASC",
        )?;
        let rows = stmt.query_map([], RunRow::from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

/// `web.*` matches every `web.` kind; anything else matches exactly.
fn kind_pattern(kind: &str) -> String {
    match kind.strip_suffix('*') {
        Some(prefix) => format!("{}%", escape_like(prefix)),
        None => escape_like(kind),
    }
}

fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

#[derive(Default, Debug, Clone)]
pub struct EventFilter {
    pub since: Option<i64>,
    pub until: Option<i64>,
    pub kinds: Vec<String>,
    /// a host also matches its subdomains (`zoom.us` matches `cdn.zoom.us`)
    pub hosts: Vec<String>,
    pub match_text: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub reverse: bool,
}

const EVENT_ROW_SELECT: &str =
    "SELECT e.id, e.run_id, e.ts_utc, e.ts_precision, e.kind, s.kind, s.app, s.profile,
        e.subject, e.subject_host, e.summary, e.confidence
     FROM events e JOIN sources s ON s.id = e.source_id";

const EVENT_DETAIL_SELECT: &str =
    "SELECT e.id, e.run_id, e.source_id, e.ts_utc, e.ts_precision, e.ts_raw, e.ts_raw_format,
        e.ts_tz_assumed, e.kind, e.actor_json, e.subject, e.subject_host, e.summary,
        e.attrs_json, e.locator, e.prov_file, e.prov_sha256, e.confidence,
        s.kind, s.app, s.profile
     FROM events e JOIN sources s ON s.id = e.source_id";

#[derive(Debug, serde::Serialize)]
pub struct EventRow {
    pub id: u64,
    pub run_id: u64,
    pub ts_utc: i64,
    pub ts_precision: String,
    pub kind: String,
    pub source_kind: String,
    pub app: Option<String>,
    pub profile: Option<String>,
    pub subject: Option<String>,
    pub subject_host: Option<String>,
    pub summary: Option<String>,
    pub confidence: String,
}

impl EventRow {
    fn from_row(row: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            run_id: row.get(1)?,
            ts_utc: row.get(2)?,
            ts_precision: row.get(3)?,
            kind: row.get(4)?,
            source_kind: row.get(5)?,
            app: row.get(6)?,
            profile: row.get(7)?,
            subject: row.get(8)?,
            subject_host: row.get(9)?,
            summary: row.get(10)?,
            confidence: row.get(11)?,
        })
    }
}

#[derive(Debug, serde::Serialize)]
pub struct SourceRow {
    pub id: u64,
    pub run_id: u64,
    pub kind: String,
    pub app: Option<String>,
    pub profile: Option<String>,
    pub path: Option<String>,
    pub status: Option<String>,
    pub note: Option<String>,
    pub event_count: u64,
}

#[derive(Debug)]
pub struct EventDetail {
    pub id: u64,
    pub run_id: u64,
    pub source_id: u64,
    pub ts_utc: i64,
    pub ts_precision: String,
    pub ts_raw: Option<String>,
    pub ts_raw_format: Option<String>,
    pub ts_tz_assumed: Option<String>,
    pub kind: String,
    pub actor_json: Option<String>,
    pub subject: Option<String>,
    pub subject_host: Option<String>,
    pub summary: Option<String>,
    pub attrs_json: Option<String>,
    pub locator: Option<String>,
    pub prov_file: Option<String>,
    pub prov_sha256: Option<String>,
    pub confidence: String,
    pub source_kind: String,
    pub app: Option<String>,
    pub profile: Option<String>,
}

impl EventDetail {
    fn from_row(row: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            run_id: row.get(1)?,
            source_id: row.get(2)?,
            ts_utc: row.get(3)?,
            ts_precision: row.get(4)?,
            ts_raw: row.get(5)?,
            ts_raw_format: row.get(6)?,
            ts_tz_assumed: row.get(7)?,
            kind: row.get(8)?,
            actor_json: row.get(9)?,
            subject: row.get(10)?,
            subject_host: row.get(11)?,
            summary: row.get(12)?,
            attrs_json: row.get(13)?,
            locator: row.get(14)?,
            prov_file: row.get(15)?,
            prov_sha256: row.get(16)?,
            confidence: row.get(17)?,
            source_kind: row.get(18)?,
            app: row.get(19)?,
            profile: row.get(20)?,
        })
    }
}

#[derive(Default, Debug, Clone)]
pub struct ArtifactMeta<'a> {
    pub sha256: Option<&'a str>,
    pub size: Option<i64>,
    pub mtime_ns: Option<i64>,
    pub ctime_ns: Option<i64>,
    pub btime_ns: Option<i64>,
    pub owner_pkg: Option<&'a str>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ArtifactRow {
    pub id: u64,
    pub path: String,
    pub sha256: Option<String>,
    pub size: Option<i64>,
    pub mtime_ns: Option<i64>,
    pub ctime_ns: Option<i64>,
    pub btime_ns: Option<i64>,
    pub owner_pkg: Option<String>,
    pub first_seen_run: Option<u64>,
}

impl ArtifactRow {
    fn from_row(row: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            path: row.get(1)?,
            sha256: row.get(2)?,
            size: row.get(3)?,
            mtime_ns: row.get(4)?,
            ctime_ns: row.get(5)?,
            btime_ns: row.get(6)?,
            owner_pkg: row.get(7)?,
            first_seen_run: row.get(8)?,
        })
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct LinkRow {
    pub src_type: String,
    pub src_id: u64,
    pub rel: String,
    pub dst_type: String,
    pub dst_id: u64,
    pub confidence: String,
    pub why: Option<String>,
}

impl LinkRow {
    fn from_row(row: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            src_type: row.get(0)?,
            src_id: row.get(1)?,
            rel: row.get(2)?,
            dst_type: row.get(3)?,
            dst_id: row.get(4)?,
            confidence: row.get(5)?,
            why: row.get(6)?,
        })
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct StateRow {
    pub rowid: i64,
    pub run_id: u64,
    pub kind: String,
    pub key: String,
    pub value_json: String,
    pub locator: Option<String>,
}

impl StateRow {
    fn from_row(row: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            rowid: row.get(0)?,
            run_id: row.get(1)?,
            kind: row.get(2)?,
            key: row.get(3)?,
            value_json: row.get(4)?,
            locator: row.get(5)?,
        })
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct IocRow {
    pub id: u64,
    pub ioc_type: String,
    pub value: String,
    pub note: Option<String>,
    pub added_utc: i64,
}

impl IocRow {
    fn from_row(row: &Row) -> rusqlite::Result<Self> {
        let id_i64: i64 = row.get(0)?;
        Ok(Self {
            id: id_i64 as u64,
            ioc_type: row.get(1)?,
            value: row.get(2)?,
            note: row.get(3)?,
            added_utc: row.get(4)?,
        })
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct RunRow {
    pub id: u64,
    pub started_utc: i64,
    pub finished_utc: Option<i64>,
    pub host: Option<String>,
    pub user: Option<String>,
    pub uid: Option<u32>,
    pub euid: Option<u32>,
    pub tool_version: Option<String>,
    pub tz: Option<String>,
    pub args: Option<String>,
    pub errors_json: Option<String>,
}

impl RunRow {
    fn from_row(row: &Row) -> rusqlite::Result<Self> {
        let id_i64: i64 = row.get(0)?;
        Ok(Self {
            id: id_i64 as u64,
            started_utc: row.get(1)?,
            finished_utc: row.get(2)?,
            host: row.get(3)?,
            user: row.get(4)?,
            uid: row.get(5)?,
            euid: row.get(6)?,
            tool_version: row.get(7)?,
            tz: row.get(8)?,
            args: row.get(9)?,
            errors_json: row.get(10)?,
        })
    }
}

fn now_micros() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as i64
}

const INSERT_EVENT: &str = "INSERT INTO events
    (run_id, source_id, ts_utc, ts_precision, ts_raw, ts_raw_format, ts_tz_assumed, kind,
     actor_json, subject, subject_host, summary, attrs_json, locator, prov_file, prov_sha256,
     confidence, dedup_key)
    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
    ON CONFLICT(dedup_key) DO NOTHING";

const INSERT_STATE: &str = "INSERT INTO state (run_id, kind, key, value_json, locator)
    VALUES (?1, ?2, ?3, ?4, ?5)
    ON CONFLICT(run_id, kind, key) DO UPDATE SET
      value_json = EXCLUDED.value_json,
      locator = EXCLUDED.locator";

const INSERT_ARTIFACT: &str = "INSERT INTO artifacts
    (path, sha256, size, mtime_ns, ctime_ns, btime_ns, owner_pkg, first_seen_run)
    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
    ON CONFLICT(path) DO UPDATE SET
      sha256 = COALESCE(EXCLUDED.sha256, sha256),
      size = COALESCE(EXCLUDED.size, size),
      mtime_ns = COALESCE(EXCLUDED.mtime_ns, mtime_ns),
      ctime_ns = COALESCE(EXCLUDED.ctime_ns, ctime_ns),
      btime_ns = COALESCE(EXCLUDED.btime_ns, btime_ns),
      owner_pkg = COALESCE(EXCLUDED.owner_pkg, owner_pkg)";

const SCHEMA: &str = r#"
CREATE TABLE runs (
    id INTEGER PRIMARY KEY,
    started_utc INTEGER NOT NULL,
    finished_utc INTEGER,
    host TEXT,
    user TEXT,
    uid INTEGER,
    euid INTEGER,
    tool_version TEXT,
    tz TEXT,
    args TEXT,
    errors_json TEXT
);

CREATE TABLE sources (
    id INTEGER PRIMARY KEY,
    run_id INTEGER NOT NULL REFERENCES runs(id),
    kind TEXT NOT NULL,
    app TEXT,
    profile TEXT,
    path TEXT,
    sha256 TEXT,
    size INTEGER,
    status TEXT,
    note TEXT
);

CREATE TABLE events (
    id INTEGER PRIMARY KEY,
    run_id INTEGER NOT NULL REFERENCES runs(id),
    source_id INTEGER NOT NULL REFERENCES sources(id),
    ts_utc INTEGER NOT NULL,
    ts_precision TEXT NOT NULL,
    ts_raw TEXT,
    ts_raw_format TEXT,
    ts_tz_assumed TEXT,
    kind TEXT NOT NULL,
    actor_json TEXT,
    subject TEXT,
    subject_host TEXT,
    summary TEXT,
    attrs_json TEXT,
    locator TEXT,
    prov_file TEXT,
    prov_sha256 TEXT,
    confidence TEXT,
    dedup_key TEXT UNIQUE NOT NULL
);

CREATE INDEX ev_ts ON events(ts_utc);
CREATE INDEX ev_host ON events(subject_host);
CREATE INDEX ev_kind ON events(kind);

CREATE VIRTUAL TABLE events_fts USING fts5(
    subject, summary, attrs_json,
    content='events', content_rowid='id'
);

CREATE TRIGGER events_ai AFTER INSERT ON events BEGIN
    INSERT INTO events_fts(rowid, subject, summary, attrs_json)
    VALUES (NEW.id, NEW.subject, NEW.summary, NEW.attrs_json);
END;

CREATE TRIGGER events_ad AFTER DELETE ON events BEGIN
    INSERT INTO events_fts(events_fts, rowid, subject, summary, attrs_json)
    VALUES ('delete', OLD.id, OLD.subject, OLD.summary, OLD.attrs_json);
END;

CREATE TRIGGER events_au AFTER UPDATE ON events BEGIN
    INSERT INTO events_fts(events_fts, rowid, subject, summary, attrs_json)
    VALUES ('delete', OLD.id, OLD.subject, OLD.summary, OLD.attrs_json);
    INSERT INTO events_fts(rowid, subject, summary, attrs_json)
    VALUES (NEW.id, NEW.subject, NEW.summary, NEW.attrs_json);
END;

CREATE TABLE artifacts (
    id INTEGER PRIMARY KEY,
    path TEXT UNIQUE NOT NULL,
    sha256 TEXT,
    size INTEGER,
    mtime_ns INTEGER,
    ctime_ns INTEGER,
    btime_ns INTEGER,
    owner_pkg TEXT,
    first_seen_run INTEGER REFERENCES runs(id)
);

CREATE TABLE links (
    src_type TEXT,
    src_id INTEGER,
    rel TEXT,
    dst_type TEXT,
    dst_id INTEGER,
    confidence TEXT,
    why TEXT
);
CREATE INDEX links_src ON links(src_type, src_id);
CREATE INDEX links_dst ON links(dst_type, dst_id);
CREATE UNIQUE INDEX IF NOT EXISTS links_uniq ON links(src_type, src_id, rel, dst_type, dst_id);

CREATE TABLE state (
    run_id INTEGER NOT NULL,
    kind TEXT NOT NULL,
    key TEXT NOT NULL,
    value_json TEXT,
    locator TEXT,
    PRIMARY KEY (run_id, kind, key)
);

CREATE TABLE iocs (
    id INTEGER PRIMARY KEY,
    type TEXT,
    value TEXT,
    note TEXT,
    added_utc INTEGER
);
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use fp_core::{Actor, Confidence, EventKind, Provenance, TimePrecision, time::TimeFormat};
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt;

    fn event(
        run_id: u64,
        source: u64,
        ts: i64,
        kind: EventKind,
        subject: &str,
        host: Option<&str>,
        locator: &str,
    ) -> Event {
        Event {
            run_id,
            ts_utc: ts,
            ts_precision: TimePrecision::Exact,
            ts_raw: ts.to_string(),
            ts_raw_format: TimeFormat::UnixMillis,
            ts_tz_assumed: None,
            source_kind: "chromium.history".into(),
            source_inst: source,
            kind,
            actor: Some(Actor::app("Brave")),
            subject: subject.into(),
            subject_host: host.map(String::from),
            summary: format!("summary of {subject}"),
            attrs: json!({ "visit_id": locator }),
            provenance: Provenance {
                file: "/h/History".into(),
                file_sha256: Some("ab".into()),
                locator: locator.into(),
            },
            confidence: Confidence::Direct,
        }
    }

    fn run(store: &mut Store) -> (u64, u64) {
        let run_id = store
            .insert_run(&NewRun {
                host: "h",
                user: "u",
                uid: 1000,
                euid: 1000,
                version: "0",
                tz: "+00:00",
                args: "collect",
            })
            .unwrap();
        let inst = SourceInstance::new(
            "chromium.history",
            "Brave",
            Some("Default".into()),
            "/h".into(),
        );
        let source_id = store.insert_source(run_id, &inst).unwrap();
        (run_id, source_id)
    }

    fn sample(run_id: u64, source_id: u64) -> Vec<Event> {
        vec![
            event(
                run_id,
                source_id,
                300,
                EventKind::WebRedirect,
                "https://zoom.us/download",
                Some("zoom.us"),
                "visits.id=3",
            ),
            event(
                run_id,
                source_id,
                100,
                EventKind::WebVisit,
                "https://page-redirect.com/x",
                Some("page-redirect.com"),
                "visits.id=1",
            ),
            event(
                run_id,
                source_id,
                200,
                EventKind::WebVisit,
                "https://us09-web-zoom.com/j/1",
                Some("us09-web-zoom.com"),
                "visits.id=2",
            ),
            event(
                run_id,
                source_id,
                400,
                EventKind::WebDownload,
                "https://cdn.zoom.us/zoom_amd64.deb",
                Some("cdn.zoom.us"),
                "downloads.id=1",
            ),
        ]
    }

    fn ingest(store: &mut Store, run_id: u64, events: &[Event]) -> IngestStats {
        let batch = Batch {
            source_identity: "chromium.history|Brave|Default|/h",
            events,
            states: &[],
            artifacts: &[Artifact::referenced(
                "/h/Downloads/zoom_amd64.deb".into(),
                Some(10),
            )],
        };
        store.ingest(run_id, &batch).unwrap()
    }

    fn open_case() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_or_create(dir.path().join("case.db")).unwrap();
        (dir, store)
    }

    #[test]
    fn new_case_is_private() {
        let (dir, _store) = open_case();
        let mode = std::fs::metadata(dir.path().join("case.db"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn open_refuses_missing_case() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.db");
        assert!(matches!(Store::open(&path), Err(StoreError::NotFound(_))));
        assert!(!path.exists());
    }

    #[test]
    fn rerun_is_idempotent() {
        let (_dir, mut store) = open_case();
        let (r1, s1) = run(&mut store);
        let first = ingest(&mut store, r1, &sample(r1, s1));
        assert_eq!((first.events_new, first.events_known), (4, 0));

        let (r2, s2) = run(&mut store);
        let second = ingest(&mut store, r2, &sample(r2, s2));
        assert_eq!((second.events_new, second.events_known), (0, 4));
        assert_eq!(store.event_count().unwrap(), 4);
    }

    #[test]
    fn list_filters_and_orders() {
        let (_dir, mut store) = open_case();
        let (r, s) = run(&mut store);
        ingest(&mut store, r, &sample(r, s));

        let all = store.list_events(&EventFilter::default()).unwrap();
        assert_eq!(
            all.iter().map(|e| e.ts_utc).collect::<Vec<_>>(),
            [100, 200, 300, 400]
        );

        let window = EventFilter {
            since: Some(150),
            until: Some(300),
            ..Default::default()
        };
        assert_eq!(store.list_events(&window).unwrap().len(), 2);

        let web = EventFilter {
            kinds: vec!["web.v*".into()],
            ..Default::default()
        };
        assert_eq!(store.list_events(&web).unwrap().len(), 2);

        let zoom = EventFilter {
            hosts: vec!["zoom.us".into()],
            ..Default::default()
        };
        let hosts: Vec<_> = store
            .list_events(&zoom)
            .unwrap()
            .into_iter()
            .filter_map(|e| e.subject_host)
            .collect();
        assert_eq!(hosts, ["zoom.us", "cdn.zoom.us"]);

        let not_a_suffix = EventFilter {
            hosts: vec!["oom.us".into()],
            ..Default::default()
        };
        assert!(store.list_events(&not_a_suffix).unwrap().is_empty());

        let last = EventFilter {
            limit: Some(1),
            reverse: true,
            ..Default::default()
        };
        assert_eq!(store.list_events(&last).unwrap()[0].ts_utc, 400);

        let text = EventFilter {
            match_text: Some("us09".into()),
            ..Default::default()
        };
        assert_eq!(store.list_events(&text).unwrap().len(), 1);

        let literal = EventFilter {
            match_text: Some("100%".into()),
            ..Default::default()
        };
        assert!(store.list_events(&literal).unwrap().is_empty());
    }

    #[test]
    fn fts_search_and_detail() {
        let (_dir, mut store) = open_case();
        let (r, s) = run(&mut store);
        ingest(&mut store, r, &sample(r, s));

        let hits = store.search_events("\"us09-web-zoom.com\"").unwrap();
        assert_eq!(hits.len(), 1);
        let detail = store.get_event(hits[0].id).unwrap().unwrap();
        assert_eq!(detail.locator.as_deref(), Some("visits.id=2"));
        assert_eq!(detail.prov_file.as_deref(), Some("/h/History"));
        assert_eq!(detail.app.as_deref(), Some("Brave"));
        assert!(store.get_event(999).unwrap().is_none());
    }

    #[test]
    fn list_state_filters() {
        let (_dir, mut store) = open_case();
        let (r1, _) = run(&mut store);
        let batch1 = Batch {
            source_identity: "test",
            events: &[],
            states: &[
                StateRecord {
                    run_id: r1,
                    kind: "persist.cron".into(),
                    key: "/etc/crontab".into(),
                    value: json!({"owner": "cron"}),
                    provenance: Provenance {
                        file: "/etc/crontab".into(),
                        file_sha256: None,
                        locator: "test".into(),
                    },
                },
                StateRecord {
                    run_id: r1,
                    kind: "persist.systemd".into(),
                    key: "/etc/systemd/system/malicious.service".into(),
                    value: json!({"owner": null}),
                    provenance: Provenance {
                        file: "/etc/systemd/system/malicious.service".into(),
                        file_sha256: None,
                        locator: "test".into(),
                    },
                },
                StateRecord {
                    run_id: r1,
                    kind: "web.site_permission".into(),
                    key: "zoom.us|camera".into(),
                    value: json!({"decision": "allow"}),
                    provenance: Provenance {
                        file: "/h/Preferences".into(),
                        file_sha256: None,
                        locator: "test".into(),
                    },
                },
            ],
            artifacts: &[],
        };
        store.ingest(r1, &batch1).unwrap();

        let all = store.list_state(Some(r1), None, None).unwrap();
        assert_eq!(all.len(), 3);

        let persist_only = store.list_state(Some(r1), Some("persist.*"), None).unwrap();
        assert_eq!(persist_only.len(), 2);

        let systemd_only = store
            .list_state(Some(r1), Some("persist.systemd"), None)
            .unwrap();
        assert_eq!(systemd_only.len(), 1);
        assert_eq!(systemd_only[0].key, "/etc/systemd/system/malicious.service");

        let matched = store.list_state(Some(r1), None, Some("malicious")).unwrap();
        assert_eq!(matched.len(), 1);

        let non_existent_run = store.list_state(Some(999), None, None).unwrap();
        assert!(non_existent_run.is_empty());
    }

    #[test]
    fn ioc_crud() {
        let (_dir, mut store) = open_case();
        let id1 = store
            .insert_ioc("domain", "us09-web-zoom.com", Some("malicious meeting"))
            .unwrap();
        assert_eq!(id1, 1);
        let id2 = store.insert_ioc("sha256", "e9a522c7a40237", None).unwrap();
        assert_eq!(id2, 2);

        let iocs = store.list_iocs().unwrap();
        assert_eq!(iocs.len(), 2);
        assert_eq!(iocs[0].ioc_type, "domain");
        assert_eq!(iocs[0].value, "us09-web-zoom.com");
        assert_eq!(iocs[0].note.as_deref(), Some("malicious meeting"));
        assert!(iocs[0].added_utc > 0);

        let deleted = store.delete_ioc(id1).unwrap();
        assert!(deleted);
        let remaining = store.list_iocs().unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, id2);

        let not_found = store.delete_ioc(999).unwrap();
        assert!(!not_found);
    }

    #[test]
    fn refuses_old_schema() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.db");
        Connection::open(&path)
            .unwrap()
            .execute_batch("CREATE TABLE events (id INTEGER PRIMARY KEY);")
            .unwrap();
        assert!(matches!(
            Store::open(&path),
            Err(StoreError::SchemaVersion { found: 0 })
        ));
    }
}
