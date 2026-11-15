//! The worker pool: reader threads owning read-only stores, one lazy writer thread,
//! a latest-wins queue keyed by request kind, and per-kind cancellation through
//! SQLite interruption. The UI thread only submits closures and polls responses.

mod proto;

pub use proto::{Generation, Payload, QueryError, ReqId, ReqKind, Resp, WriterJob};

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

struct State {
    next_id: ReqId,
    quit: bool,
    /// only the newest pending request per kind runs; older ones are dropped here
    pending: HashMap<ReqKind, proto::Req>,
    /// kind → (request id, that reader's interrupt handle) while it runs
    inflight: HashMap<ReqKind, (ReqId, fp_store::InterruptHandle)>,
    writer_queue: VecDeque<WriterJob>,
    writer_running: bool,
}

impl State {
    fn pop_next(&mut self) -> Option<proto::Req> {
        let (&kind, _) = self.pending.iter().min_by_key(|(_, req)| req.id)?;
        self.pending.remove(&kind)
    }
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

pub struct Worker {
    case: PathBuf,
    shared: Arc<Shared>,
    resp_tx: std::sync::mpsc::Sender<Resp>,
    resp_rx: std::sync::mpsc::Receiver<Resp>,
    on_response: Arc<dyn Fn() + Send + Sync>,
    readers: Vec<JoinHandle<()>>,
    watcher: JoinHandle<()>,
    writer: Mutex<Option<JoinHandle<()>>>,
}

/// How often the watcher samples `PRAGMA data_version`.
pub const WATCH_INTERVAL: Duration = Duration::from_secs(2);

fn lock<T>(state: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Worker {
    /// Opens the case once to fail early, then spawns `reader_threads` readers that
    /// each own a read-only store. `on_response` runs after every response is queued
    /// (the GUI passes `ctx.request_repaint`; tests count).
    pub fn spawn(
        case: impl AsRef<Path>,
        reader_threads: usize,
        on_response: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self, fp_store::StoreError> {
        let case = case.as_ref().to_path_buf();
        // a missing case is not a spawn error: the GUI shows its own empty-state panel and
        // the writer can create the file later. an existing but unreadable case fails here.
        if case.exists() {
            drop(fp_store::Store::open(&case)?);
        }
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                next_id: 0,
                quit: false,
                pending: HashMap::new(),
                inflight: HashMap::new(),
                writer_queue: VecDeque::new(),
                writer_running: false,
            }),
            wake: Condvar::new(),
        });
        let (resp_tx, resp_rx) = std::sync::mpsc::channel();
        let on_response: Arc<dyn Fn() + Send + Sync> = Arc::new(on_response);
        let mut readers = Vec::new();
        for _ in 0..reader_threads.max(1) {
            readers.push(spawn_reader(
                case.clone(),
                shared.clone(),
                resp_tx.clone(),
                on_response.clone(),
            ));
        }
        let watcher = spawn_watcher(
            case.clone(),
            shared.clone(),
            resp_tx.clone(),
            on_response.clone(),
        );
        Ok(Self {
            case,
            shared,
            resp_tx,
            resp_rx,
            on_response,
            readers,
            watcher,
            writer: Mutex::new(None),
        })
    }

    /// Queues a read. Supersedes any pending request of the same kind.
    pub fn submit(
        &self,
        kind: ReqKind,
        generation: Generation,
        run: impl FnOnce(&fp_store::Store) -> Result<Payload, QueryError> + Send + 'static,
    ) -> ReqId {
        let mut state = lock(&self.shared.state);
        state.next_id += 1;
        let id = state.next_id;
        state.pending.insert(
            kind,
            proto::Req {
                id,
                generation,
                kind,
                run: Box::new(run),
            },
        );
        self.shared.wake.notify_one();
        id
    }

    /// Queues a write (IOC add/remove now; collect later). Writes never supersede
    /// each other and run in order on the single writer thread.
    pub fn submit_writer(
        &self,
        kind: ReqKind,
        generation: Generation,
        run: impl FnOnce(&mut fp_store::Store) -> Result<Payload, QueryError> + Send + 'static,
    ) -> ReqId {
        let mut state = lock(&self.shared.state);
        state.next_id += 1;
        let id = state.next_id;
        state.writer_queue.push_back(WriterJob {
            id,
            generation,
            kind,
            run: Box::new(run),
        });
        if !state.writer_running {
            state.writer_running = true;
            let case = self.case.clone();
            let shared = self.shared.clone();
            let resp_tx = self.resp_tx.clone();
            let on_response = self.on_response.clone();
            *lock(&self.writer) = Some(std::thread::spawn(move || {
                writer_loop(case, shared, resp_tx, on_response);
            }));
        }
        self.shared.wake.notify_one();
        id
    }

    /// Interrupts the running query of this kind, if any. The interrupted query
    /// produces no response; a pending one is simply superseded later.
    pub fn cancel(&self, kind: ReqKind) {
        let state = lock(&self.shared.state);
        if let Some((_, handle)) = state.inflight.get(&kind) {
            handle.interrupt();
        }
    }

    /// Drains finished responses; called once per frame on the UI thread.
    pub fn poll(&self) -> Vec<Resp> {
        self.resp_rx.try_iter().collect()
    }

    /// Stops the pool and waits for every thread. Dropping without this is safe too
    /// (the threads only hold read-only stores), but tests prefer a clean stop.
    pub fn shutdown(self) {
        {
            let mut state = lock(&self.shared.state);
            state.quit = true;
        }
        self.shared.wake.notify_all();
        for reader in self.readers {
            let _ = reader.join();
        }
        let _ = self.watcher.join();
        let writer = lock(&self.writer).take();
        if let Some(writer) = writer {
            let _ = writer.join();
        }
    }
}

fn spawn_reader(
    case: PathBuf,
    shared: Arc<Shared>,
    resp_tx: std::sync::mpsc::Sender<Resp>,
    on_response: Arc<dyn Fn() + Send + Sync>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        // the case may not exist yet (GUI empty state; the writer can create it later):
        // keep trying instead of dying, so a case that appears later is served
        let store = loop {
            match fp_store::Store::open(&case) {
                Ok(store) => break store,
                Err(_) => {
                    let state = lock(&shared.state);
                    if state.quit {
                        return;
                    }
                    drop(state);
                    std::thread::sleep(Duration::from_millis(500));
                }
            }
        };
        loop {
            let req = {
                let mut state = lock(&shared.state);
                loop {
                    if state.quit {
                        return;
                    }
                    if let Some(req) = state.pop_next() {
                        break req;
                    }
                    state = shared.wake.wait(state).unwrap_or_else(|p| p.into_inner());
                }
            };
            let id = req.id;
            let generation = req.generation;
            let kind = req.kind;
            let interrupt = store.interrupt_handle();
            {
                let mut state = lock(&shared.state);
                state.inflight.insert(kind, (id, interrupt));
            }
            // a panicking query must not take the reader down with it
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (req.run)(&store)))
                    .unwrap_or_else(|panic| {
                        Err(QueryError::Message(format!(
                            "worker panic: {}",
                            panic
                                .downcast_ref::<&str>()
                                .copied()
                                .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
                                .unwrap_or("unknown")
                        )))
                    });
            {
                let mut state = lock(&shared.state);
                state.inflight.remove(&kind);
            }
            // Interrupted: superseded while running; the newer request answers instead.
            if matches!(result, Err(QueryError::Interrupted)) {
                continue;
            }
            let _ = resp_tx.send(Resp {
                id,
                generation,
                kind,
                result,
            });
            on_response();
        }
    })
}

fn spawn_watcher(
    case: PathBuf,
    shared: Arc<Shared>,
    resp_tx: std::sync::mpsc::Sender<Resp>,
    on_response: Arc<dyn Fn() + Send + Sync>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        // watch responses are not submitted requests; start their ids away from the
        // submit counter so they can never collide on the shared response channel
        let mut next_id = u64::MAX / 2;
        let mut store: Option<fp_store::Store> = None;
        let mut last: Option<i64> = None;
        let mut last_poll = Instant::now() - WATCH_INTERVAL;
        loop {
            if lock(&shared.state).quit {
                return;
            }
            if last_poll.elapsed() < WATCH_INTERVAL {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            last_poll = Instant::now();
            if store.is_none() {
                store = fp_store::Store::open(&case).ok();
                last = None;
            }
            let Some(store) = &store else {
                continue;
            };
            let Ok(version) = store.data_version() else {
                continue;
            };
            if last == Some(version) {
                continue;
            }
            last = Some(version);
            let _ = resp_tx.send(Resp {
                id: next_id,
                generation: 0,
                kind: ReqKind::Watch,
                result: Ok(Box::new(version)),
            });
            next_id += 1;
            on_response();
        }
    })
}

fn writer_loop(
    case: PathBuf,
    shared: Arc<Shared>,
    resp_tx: std::sync::mpsc::Sender<Resp>,
    on_response: Arc<dyn Fn() + Send + Sync>,
) {
    let Ok(mut store) = fp_store::Store::open_or_create(&case) else {
        let mut state = lock(&shared.state);
        state.writer_running = false;
        return;
    };
    loop {
        let job = {
            let mut state = lock(&shared.state);
            loop {
                if state.quit {
                    return;
                }
                if let Some(job) = state.writer_queue.pop_front() {
                    break job;
                }
                state = shared.wake.wait(state).unwrap_or_else(|p| p.into_inner());
            }
        };
        let id = job.id;
        let generation = job.generation;
        let kind = job.kind;
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (job.run)(&mut store)))
                .unwrap_or_else(|panic| {
                    Err(QueryError::Message(format!(
                        "writer panic: {}",
                        panic
                            .downcast_ref::<&str>()
                            .copied()
                            .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
                            .unwrap_or("unknown")
                    )))
                });
        let _ = resp_tx.send(Resp {
            id,
            generation,
            kind,
            result,
        });
        on_response();
    }
}

#[cfg(test)]
mod tests;
