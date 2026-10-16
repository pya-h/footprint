use crate::collector::CollectCtx;
use fp_core::time::{self, TimeError, TimeFormat};
use fp_core::{Confidence, Event, EventKind, Provenance, SourceInstance, TimePrecision};
use std::path::Path;

/// A raw timestamp together with its normalized form.
pub struct Stamp {
    pub raw: String,
    pub format: TimeFormat,
    pub ts_utc: i64,
    pub precision: TimePrecision,
    pub tz_assumed: Option<String>,
}

impl Stamp {
    pub fn parse(
        raw: impl Into<String>,
        format: TimeFormat,
        assume_tz: Option<&str>,
    ) -> Result<Self, TimeError> {
        let raw = raw.into();
        let norm = time::normalize(&raw, &format, assume_tz)?;
        Ok(Self {
            raw,
            format,
            ts_utc: norm.ts_utc,
            precision: norm.precision,
            tz_assumed: norm.tz_assumed,
        })
    }

    /// No timestamp at all: the position in the file is the only ordering we have.
    pub fn untimed() -> Self {
        Self {
            raw: String::new(),
            format: TimeFormat::None,
            ts_utc: 0,
            precision: TimePrecision::OrderOnly,
            tz_assumed: None,
        }
    }

    pub fn is_timed(&self) -> bool {
        self.precision != TimePrecision::OrderOnly
    }
}

pub struct Origin<'a> {
    pub file: &'a Path,
    pub sha256: Option<&'a str>,
}

impl Origin<'_> {
    pub fn at(&self, locator: impl Into<String>) -> Provenance {
        Provenance {
            file: self.file.to_path_buf(),
            file_sha256: self.sha256.map(String::from),
            locator: locator.into(),
        }
    }
}

/// An event with the common fields filled; callers override `actor`, `subject_host`, `attrs`… with struct update.
pub fn new_event(
    ctx: &CollectCtx,
    inst: &SourceInstance,
    stamp: Stamp,
    kind: EventKind,
    subject: String,
    summary: String,
    provenance: Provenance,
) -> Event {
    let confidence = if stamp.is_timed() {
        Confidence::Direct
    } else {
        Confidence::Derived
    };
    Event {
        run_id: ctx.run_id,
        ts_utc: stamp.ts_utc,
        ts_precision: stamp.precision,
        ts_raw: stamp.raw,
        ts_raw_format: stamp.format,
        ts_tz_assumed: stamp.tz_assumed,
        source_kind: inst.kind.clone(),
        source_inst: inst.id,
        kind,
        actor: None,
        subject,
        subject_host: None,
        summary,
        attrs: serde_json::Value::Null,
        provenance,
        confidence,
    }
}
