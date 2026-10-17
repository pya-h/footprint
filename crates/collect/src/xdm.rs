use crate::collector::{CollectCtx, Collector, Env, Sink};
use crate::event::{Origin, Stamp, new_event};
use crate::utils::{
    copy_db_set, extract_host, human_size, in_window, open_snapshot, stage_dir, table_has_columns,
};
use anyhow::{Result, bail};
use fp_core::time::TimeFormat;
use fp_core::{Actor, Artifact, Availability, EventKind, SourceInstance};
use serde_json::json;
use std::path::PathBuf;

const XDM_DB: &str = ".xdm-app-data/downloads.db";
const XDM_JAVA_DIR: &str = ".xdman";
/// Only these are read; the same table also stores site and proxy passwords (`pass`, `proxy_pass`).
const COLUMNS: &[&str] = &[
    "id",
    "name",
    "primary_url",
    "referer_url",
    "targetdir",
    "size",
    "date_added",
    "completed",
];

pub struct XdmCollector;

impl Collector for XdmCollector {
    fn kind(&self) -> &'static str {
        "dlmgr.xdm"
    }

    fn discover(&self, env: &Env) -> Vec<SourceInstance> {
        let mut instances = Vec::new();

        let java_path = env.home.join(XDM_JAVA_DIR);
        if java_path.is_dir() {
            let mut inst = SourceInstance::new(self.kind(), "xdm-java", None, java_path);
            inst.status = Availability::Unsupported {
                reason: "older Java XDM (~/.xdman) not yet supported".to_string(),
            };
            instances.push(inst);
        }

        let path = env.home.join(XDM_DB);
        if !path.exists() {
            let mut inst = SourceInstance::new(self.kind(), "xdm", None, path);
            inst.status = Availability::NotInstalled;
            instances.push(inst);
            return instances;
        }

        let mut inst = SourceInstance::new(self.kind(), "xdm", None, path.clone());
        let meta = std::fs::metadata(&path);
        if meta.as_ref().is_ok_and(|m| m.len() == 0) {
            inst.status = Availability::Empty;
        } else {
            match rusqlite::Connection::open_with_flags(
                &path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                    | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
            ) {
                Ok(conn) => match table_has_columns(&conn, "downloads", COLUMNS) {
                    Ok(true) => inst.status = Availability::Present,
                    Ok(false) => {
                        inst.status = Availability::Unsupported {
                            reason: "table `downloads` missing expected columns".to_string(),
                        };
                    }
                    Err(e) => {
                        inst.status = Availability::Error(format!("cannot read schema: {e:#}"));
                    }
                },
                Err(e) => {
                    inst.status = Availability::Error(format!("cannot open database: {e:#}"));
                }
            }
        }
        instances.push(inst);
        instances
    }

    fn collect(&self, inst: &SourceInstance, ctx: &CollectCtx, sink: &mut Sink) -> Result<()> {
        let snapshot = copy_db_set(&inst.path, &stage_dir(ctx, inst.id)?)?;
        let conn = open_snapshot(&snapshot)?;
        if !table_has_columns(&conn, "downloads", COLUMNS)? {
            bail!("unsupported XDM schema: table `downloads` lacks one of {COLUMNS:?}");
        }
        let origin = Origin {
            file: &inst.path,
            sha256: Some(&snapshot.sha256),
        };

        let mut stmt = conn.prepare(&format!("SELECT {} FROM downloads", COLUMNS.join(", ")))?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            let name: String = row.get::<_, Option<String>>(1)?.unwrap_or_default();
            let url: String = row.get::<_, Option<String>>(2)?.unwrap_or_default();
            let referer: Option<String> =
                row.get::<_, Option<String>>(3)?.filter(|s| !s.is_empty());
            let targetdir: Option<String> =
                row.get::<_, Option<String>>(4)?.filter(|s| !s.is_empty());
            let size: Option<i64> = row.get(5)?;
            let date_added: i64 = row.get(6)?;
            let completed: Option<i64> = row.get(7)?;

            let stamp = Stamp::parse(date_added.to_string(), TimeFormat::DotNetBinary, None)?;
            if !in_window(ctx, stamp.ts_utc) {
                continue;
            }
            let target = targetdir
                .as_ref()
                .filter(|_| !name.is_empty())
                .map(|d| PathBuf::from(d).join(&name));
            let size_text = human_size(size.unwrap_or(0));
            let summary = match &target {
                Some(t) => format!("{} ← {url} ({size_text})", t.display()),
                None => format!("{name} ← {url} ({size_text})"),
            };
            if let Some(t) = &target {
                sink.artifact(Artifact::referenced(t.clone(), size));
            }
            sink.event(fp_core::Event {
                actor: Some(Actor::app("xdm")),
                subject_host: extract_host(&url),
                attrs: json!({
                    "id": id,
                    "name": name,
                    "targetdir": targetdir,
                    "referer_url": referer,
                    "size": size,
                    "completed": completed.map(|c| c != 0),
                }),
                ..new_event(
                    ctx,
                    inst,
                    stamp,
                    EventKind::DlManagerDownload,
                    url,
                    summary,
                    origin.at(format!("downloads.id={id}")),
                )
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_not_installed_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::test(dir.path().to_path_buf(), PathBuf::from("/"));
        let instances = XdmCollector.discover(&env);
        assert_eq!(instances.len(), 1);
        assert_eq!(instances[0].status, Availability::NotInstalled);
    }

    #[test]
    fn discovers_java_xdm_as_unsupported() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".xdman")).unwrap();
        let env = Env::test(dir.path().to_path_buf(), PathBuf::from("/"));
        let instances = XdmCollector.discover(&env);
        let java = instances.iter().find(|i| i.app == "xdm-java").unwrap();
        assert!(matches!(java.status, Availability::Unsupported { .. }));
    }

    #[test]
    fn checks_schema_and_marks_unsupported_on_missing_column() {
        let dir = tempfile::tempdir().unwrap();
        let db_dir = dir.path().join(".xdm-app-data");
        std::fs::create_dir_all(&db_dir).unwrap();
        let db_path = db_dir.join("downloads.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch("CREATE TABLE downloads (id TEXT, name TEXT);")
            .unwrap();

        let env = Env::test(dir.path().to_path_buf(), PathBuf::from("/"));
        let instances = XdmCollector.discover(&env);
        let xdm = instances.iter().find(|i| i.app == "xdm").unwrap();
        assert!(matches!(xdm.status, Availability::Unsupported { .. }));
    }
}
