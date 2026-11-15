//! The request/response protocol between the UI thread and the worker pool.
//! Requests are closures over a read-only [`fp_store::Store`] so the pool stays
//! generic: new queries do not change this file.

use std::any::Any;

pub type ReqId = u64;
/// Bumped by the model on every scope change; responses with an older generation
/// than the UI currently holds are stale and dropped there.
pub type Generation = u64;

/// What a request asks for. The queue keeps only the latest pending request per
/// kind, so this list doubles as the supersede key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReqKind {
    Count,
    Bounds,
    Page,
    Histogram,
    Facets,
    Hosts,
    Groups,
    Around,
    Detail,
    Trace,
    Iocs,
    IocHits,
    Runs,
    Sources,
    State,
    Diff,
    /// IOC add/remove; runs on the writer thread.
    IocWrite,
    /// collect; runs on the writer thread.
    Collect,
    /// case-watch sample (`i64` data_version); produced by the watcher thread,
    /// not by a reader.
    Watch,
}

/// Why a query failed. Interruption is not a failure: it means a newer request of
/// the same kind superseded this one while it ran.
#[derive(Debug)]
pub enum QueryError {
    Interrupted,
    Message(String),
}

impl From<fp_store::StoreError> for QueryError {
    fn from(error: fp_store::StoreError) -> Self {
        if error.is_interruption() {
            Self::Interrupted
        } else {
            Self::Message(error.to_string())
        }
    }
}

impl From<anyhow::Error> for QueryError {
    fn from(error: anyhow::Error) -> Self {
        Self::Message(format!("{error:#}"))
    }
}

/// The value a request produced; downcast by the UI to the concrete query type.
pub type Payload = Box<dyn Any + Send>;

pub type ReadJob = Box<dyn FnOnce(&fp_store::Store) -> Result<Payload, QueryError> + Send>;
pub type WriteJob = Box<dyn FnOnce(&mut fp_store::Store) -> Result<Payload, QueryError> + Send>;

pub struct Req {
    pub id: ReqId,
    pub generation: Generation,
    pub kind: ReqKind,
    /// Parallel requests of one kind (one per facet, per strip tier, per table
    /// block) are kept apart by this key; the queue supersedes per (kind, sub).
    pub sub: Option<String>,
    pub run: ReadJob,
}

pub struct Resp {
    pub id: ReqId,
    pub generation: Generation,
    pub kind: ReqKind,
    pub result: Result<Payload, QueryError>,
}

pub struct WriterJob {
    pub id: ReqId,
    pub generation: Generation,
    pub kind: ReqKind,
    pub run: WriteJob,
}
