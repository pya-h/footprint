use fp_core::{Artifact, Event, Link, SourceInstance, StateRecord};
use rusqlite::{params, Connection};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("rusqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("failed to set case file permissions")]
    Chmod,
}

pub struct Store {
    conn: Connection,
    path: PathBuf,
}

impl Store {
    pub fn create(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(|_| StoreError::Chmod)?;
        let mut store = Self { conn, path };
        store.init()?;
        Ok(store)
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref().to_path_buf();
        let conn = Connection::open(&path)?;
        let mut store = Self { conn, path };
        store.init()?;
        Ok(store)
    }

    fn init(&mut self) -> Result<(), StoreError> {
        self.conn.execute_batch(SCHEMA)?;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn insert_run(
        &mut self,
        host: &str,
        user: &str,
        uid: u32,
        euid: u32,
        version: &str,
        tz: &str,
        args: &str,
    ) -> Result<u64, StoreError> {
        let started = now_micros();
        self.conn.execute(
            "INSERT INTO runs (started_utc, host, user, uid, euid, tool_version, tz, args)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![started, host, user, uid, euid, version, tz, args],
        )?;
        Ok(self.conn.last_insert_rowid() as u64)
    }

    pub fn finish_run(&mut self, run_id: u64, errors: &[String]) -> Result<(), StoreError> {
        let finished = now_micros();
        let errors_json = serde_json::to_string(errors)?;
        self.conn.execute(
            "UPDATE runs SET finished_utc = ?1, errors_json = ?2 WHERE id = ?3",
            params![finished, errors_json, run_id],
        )?;
        Ok(())
    }

    pub fn insert_source(
        &mut self,
        run_id: u64,
        inst: &SourceInstance,
    ) -> Result<u64, StoreError> {
        let status = serde_json::to_string(&inst.status)?;
        self.conn.execute(
            "INSERT INTO sources (run_id, kind, app, profile, path, status, note)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                run_id,
                inst.kind,
                inst.app,
                inst.profile,
                inst.path.to_string_lossy(),
                status,
                inst.note,
            ],
        )?;
        Ok(self.conn.last_insert_rowid() as u64)
    }

    pub fn insert_event(&mut self, event: &Event) -> Result<u64, StoreError> {
        let actor_json = serde_json::to_string(&event.actor)?;
        let attrs_json = serde_json::to_string(&event.attrs)?;
        let dedup_key = event.dedup_key();

        self.conn.execute(
            "INSERT INTO events
             (run_id, source_id, ts_utc, ts_precision, ts_raw, ts_raw_format, ts_tz_assumed,
              kind, actor_json, subject, subject_host, summary, attrs_json, locator, confidence, dedup_key)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
             ON CONFLICT(dedup_key) DO NOTHING",
            params![
                event.run_id,
                event.source_inst,
                event.ts_utc,
                event.ts_precision.to_string(),
                event.ts_raw,
                event.ts_raw_format.to_string(),
                event.ts_tz_assumed,
                event.kind.to_string(),
                actor_json,
                event.subject,
                event.subject_host,
                event.summary,
                attrs_json,
                event.provenance.locator,
                event.confidence.to_string(),
                dedup_key,
            ],
        )?;
        Ok(self.conn.last_insert_rowid() as u64)
    }

    pub fn insert_artifact(
        &mut self,
        run_id: u64,
        artifact: &Artifact,
    ) -> Result<u64, StoreError> {
        self.conn.execute(
            "INSERT INTO artifacts
             (path, sha256, size, mtime_ns, ctime_ns, btime_ns, owner_pkg, first_seen_run)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(path) DO UPDATE SET
               sha256 = COALESCE(EXCLUDED.sha256, sha256),
               size = COALESCE(EXCLUDED.size, size),
               mtime_ns = COALESCE(EXCLUDED.mtime_ns, mtime_ns),
               ctime_ns = COALESCE(EXCLUDED.ctime_ns, ctime_ns),
               btime_ns = COALESCE(EXCLUDED.btime_ns, btime_ns),
               owner_pkg = COALESCE(EXCLUDED.owner_pkg, owner_pkg)",
            params![
                artifact.path.to_string_lossy(),
                artifact.sha256,
                artifact.size,
                artifact.mtime_ns,
                artifact.ctime_ns,
                artifact.btime_ns,
                artifact.owner_pkg,
                run_id,
            ],
        )?;
        Ok(self.conn.last_insert_rowid() as u64)
    }

    pub fn insert_state(&mut self, record: &StateRecord) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO state (run_id, kind, key, value_json, locator)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(run_id, kind, key) DO UPDATE SET
               value_json = EXCLUDED.value_json,
               locator = EXCLUDED.locator",
            params![
                record.run_id,
                record.kind,
                record.key,
                serde_json::to_string(&record.value)?,
                record.provenance.locator,
            ],
        )?;
        Ok(())
    }

    pub fn insert_link(&mut self, link: &Link) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO links (src_type, src_id, rel, dst_type, dst_id, confidence, why)
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

    pub fn sources(&self, run_id: Option<u64>) -> Result<Vec<(u64, SourceInstance)>, StoreError> {
        let sql = if run_id.is_some() {
            "SELECT id, kind, app, profile, path, status, note FROM sources WHERE run_id = ?1 ORDER BY id"
        } else {
            "SELECT id, kind, app, profile, path, status, note FROM sources ORDER BY run_id, id"
        };
        let mut stmt = self.conn.prepare(sql)?;
        let params: &[&dyn rusqlite::ToSql] = match run_id {
            Some(id) => &[&(id as i64)],
            None => &[],
        };
        let rows = stmt.query_map(params, |row| {
            let status: String = row.get(5)?;
            Ok((
                row.get::<_, i64>(0)? as u64,
                SourceInstance {
                    id: 0,
                    kind: row.get(1)?,
                    app: row.get(2)?,
                    profile: row.get(3)?,
                    path: PathBuf::from(row.get::<_, String>(4)?),
                    status: serde_json::from_str(&status).unwrap_or(fp_core::SourceStatus::Ok),
                    note: row.get(6)?,
                },
            ))
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn event_count(&self) -> Result<u64, StoreError> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))?;
        Ok(count as u64)
    }
}

fn now_micros() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as i64
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS runs (
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

CREATE TABLE IF NOT EXISTS sources (
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

CREATE TABLE IF NOT EXISTS events (
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
    confidence TEXT,
    dedup_key TEXT UNIQUE
);

CREATE INDEX IF NOT EXISTS ev_ts ON events(ts_utc);
CREATE INDEX IF NOT EXISTS ev_host ON events(subject_host);
CREATE INDEX IF NOT EXISTS ev_kind ON events(kind);

CREATE VIRTUAL TABLE IF NOT EXISTS events_fts USING fts5(
    subject, summary, attrs_json,
    content='events', content_rowid='id'
);

CREATE TRIGGER IF NOT EXISTS events_ai AFTER INSERT ON events BEGIN
    INSERT INTO events_fts(rowid, subject, summary, attrs_json)
    VALUES (NEW.id, NEW.subject, NEW.summary, NEW.attrs_json);
END;

CREATE TRIGGER IF NOT EXISTS events_ad AFTER DELETE ON events BEGIN
    INSERT INTO events_fts(events_fts, rowid, subject, summary, attrs_json)
    VALUES ('delete', OLD.id, OLD.subject, OLD.summary, OLD.attrs_json);
END;

CREATE TRIGGER IF NOT EXISTS events_au AFTER UPDATE ON events BEGIN
    INSERT INTO events_fts(events_fts, rowid, subject, summary, attrs_json)
    VALUES ('delete', OLD.id, OLD.subject, OLD.summary, OLD.attrs_json);
    INSERT INTO events_fts(rowid, subject, summary, attrs_json)
    VALUES (NEW.id, NEW.subject, NEW.summary, NEW.attrs_json);
END;

CREATE TABLE IF NOT EXISTS artifacts (
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

CREATE TABLE IF NOT EXISTS links (
    src_type TEXT,
    src_id INTEGER,
    rel TEXT,
    dst_type TEXT,
    dst_id INTEGER,
    confidence TEXT,
    why TEXT
);
CREATE INDEX IF NOT EXISTS links_src ON links(src_type, src_id);
CREATE INDEX IF NOT EXISTS links_dst ON links(dst_type, dst_id);

CREATE TABLE IF NOT EXISTS state (
    run_id INTEGER NOT NULL,
    kind TEXT NOT NULL,
    key TEXT NOT NULL,
    value_json TEXT,
    locator TEXT,
    PRIMARY KEY (run_id, kind, key)
);

CREATE TABLE IF NOT EXISTS iocs (
    id INTEGER PRIMARY KEY,
    type TEXT,
    value TEXT,
    note TEXT,
    added_utc INTEGER
);
"#;
