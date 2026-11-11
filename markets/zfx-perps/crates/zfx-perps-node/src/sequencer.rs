//! The sequencer: one engine thread, one journal thread.
//!
//! ```text
//! gateway tasks --try_send--> [engine thread] --> [journal thread] --> replies + publisher
//!                              assign seq          append, group fsync
//!                              apply command       then release
//! ```
//!
//! The engine thread applies each command as soon as it arrives (speculatively) and hands the
//! serialized command, its events and its reply to the journal thread. The journal thread drains
//! whatever has queued up, appends it, fsyncs once for the whole group, and only then sends replies
//! and publishes events. A client therefore never sees an ack or a fill that a crash could undo:
//! after a restart the engine is rebuilt from the journal, which holds exactly the acknowledged
//! commands plus possibly some unacknowledged ones at the tail.
//!
//! Reads (`query`) run on the engine thread and are released through the journal thread too, so a
//! read issued after a write's ack always observes that write.

use crate::config::Config;
use crossbeam_channel::{Receiver, Sender, TrySendError, bounded, unbounded};
use zfx_perps_engine::{Ack, Command, Engine, Event, Reject};
use zfx_perps_journal::{Hash, Journal, chain};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::JoinHandle;
use tokio::sync::oneshot;

pub type Reply = Result<Ack, Reject>;
type Release = Box<dyn FnOnce() + Send>;
type QueryFn = Box<dyn FnOnce(&Engine) -> Release + Send>;

pub enum Req {
    Cmd(Command, Option<oneshot::Sender<Reply>>),
    /// Cut a settlement batch at the current journal position.
    Cut,
    Query(QueryFn),
    Shutdown(std::sync::mpsc::Sender<()>),
}

enum JItem {
    Rec {
        seq: u64,
        payload: Vec<u8>,
        events: Vec<Event>,
        reply: Option<(oneshot::Sender<Reply>, Reply)>,
    },
    Release(Release),
    Snapshot {
        seq: u64,
        hash: Hash,
        bytes: Vec<u8>,
    },
    Shutdown(std::sync::mpsc::Sender<()>),
}

/// Durable, ordered output of the sequencer: (seq, events of that command).
pub type Published = (u64, Vec<Event>);

#[derive(Default)]
pub struct Stats {
    pub last_seq: AtomicU64,
    pub durable_seq: AtomicU64,
    pub fsyncs: AtomicU64,
}

#[derive(Clone)]
pub struct Handle {
    tx: Sender<Req>,
    pub stats: Arc<Stats>,
}

#[derive(Debug)]
pub enum SubmitError {
    Overloaded,
    Stopped,
}

impl Handle {
    /// Submit a command and wait until it is durable.
    pub async fn submit(&self, c: Command) -> Result<Reply, SubmitError> {
        let (tx, rx) = oneshot::channel();
        self.send(Req::Cmd(c, Some(tx)))?;
        rx.await.map_err(|_| SubmitError::Stopped)
    }

    /// Run a read against the engine. Ordered after every command submitted before it.
    pub async fn query<T: Send + 'static>(&self, f: impl FnOnce(&Engine) -> T + Send + 'static) -> Result<T, SubmitError> {
        let (tx, rx) = oneshot::channel();
        self.send(Req::Query(Box::new(move |e| {
            let v = f(e);
            Box::new(move || {
                let _ = tx.send(v);
            })
        })))?;
        rx.await.map_err(|_| SubmitError::Stopped)
    }

    pub fn cut(&self) -> Result<(), SubmitError> {
        self.send(Req::Cut)
    }

    fn send(&self, r: Req) -> Result<(), SubmitError> {
        self.tx.try_send(r).map_err(|e| match e {
            TrySendError::Full(_) => SubmitError::Overloaded,
            TrySendError::Disconnected(_) => SubmitError::Stopped,
        })
    }

    /// Stop accepting work, flush, write a final snapshot. Blocks.
    pub fn shutdown(&self) {
        let (tx, rx) = std::sync::mpsc::channel();
        if self.tx.send(Req::Shutdown(tx)).is_ok() {
            let _ = rx.recv();
        }
    }
}

pub struct Started {
    pub handle: Handle,
    /// Engine state right after recovery and bootstrap, for building read models.
    pub initial: Engine,
    pub events: Receiver<Published>,
    pub recovered_records: usize,
    pub threads: Vec<JoinHandle<()>>,
}

/// Recover from the data dir, apply config bootstrap (assets/markets not yet in the engine), and
/// start the engine and journal threads.
pub fn start(cfg: &Config) -> Result<Started, String> {
    let dir: PathBuf = cfg.data_dir.join("journal");
    let (mut journal, rec) = Journal::open(&dir, cfg.fsync).map_err(|e| format!("journal: {e}"))?;
    if rec.truncated > 0 {
        tracing::warn!(bytes = rec.truncated, "dropped torn journal tail");
    }
    let mut engine = match &rec.snapshot {
        Some((_, _, bytes)) => Engine::from_snapshot(bytes)?,
        None => Engine::new(cfg.engine_config()),
    };
    let mut scratch = vec![];
    for (_, payload) in &rec.records {
        let c: Command = bincode::deserialize(payload).map_err(|e| format!("journal record: {e}"))?;
        scratch.clear();
        let _ = engine.apply(&c, &mut scratch);
    }
    let recovered_records = rec.records.len();
    let (mut seq, mut hash) = (rec.last_seq, rec.last_hash);
    tracing::info!(
        snapshot = rec.snapshot.as_ref().map(|s| s.0),
        replayed = recovered_records,
        seq,
        "recovered"
    );

    // Bootstrap: add configured assets and markets the engine doesn't have yet. Journaled like
    // any other command so replay reproduces them.
    let mut boot: Vec<Command> = vec![];
    for a in &cfg.assets {
        if !engine.asset_by_ref.contains_key(&a.token) {
            let id = (engine.assets.len() + boot.iter().filter(|c| matches!(c, Command::AddAsset(_))).count()) as u16;
            boot.push(Command::AddAsset(cfg.asset_spec(id, a)));
        }
    }
    for c in &boot {
        seq = append_apply(&mut journal, &mut engine, &mut hash, seq, c)?;
    }
    let mut boot = vec![];
    for m in &cfg.markets {
        if engine.market_id(&m.symbol).is_none() {
            let id = (engine.markets.len() + boot.len()) as u16;
            let spec = cfg.market_spec(id, m, |s| engine.assets.iter().find(|a| a.symbol == s).map(|a| a.id))?;
            boot.push(Command::AddMarket(spec));
        }
    }
    for c in &boot {
        seq = append_apply(&mut journal, &mut engine, &mut hash, seq, c)?;
    }
    if let Some(c) = &cfg.clearing
        && engine.clearing.is_none()
    {
        seq = append_apply(&mut journal, &mut engine, &mut hash, seq, &Command::ClearingInit(c.engine()))?;
    }
    journal.sync().map_err(|e| e.to_string())?;

    let stats = Arc::new(Stats::default());
    stats.last_seq.store(seq, Ordering::Relaxed);
    stats.durable_seq.store(seq, Ordering::Relaxed);
    let initial = engine.clone();
    let (tx, rx) = bounded::<Req>(1 << 16);
    let (jtx, jrx) = unbounded::<JItem>();
    let (ptx, prx) = unbounded::<Published>();
    let snapshot_every = cfg.snapshot_every.max(1);
    let st = stats.clone();
    let engine_thread = std::thread::Builder::new()
        .name("zfx-perps-engine".into())
        .spawn(move || engine_loop(engine, rx, jtx, seq, hash, snapshot_every, st))
        .map_err(|e| e.to_string())?;
    let st = stats.clone();
    let journal_thread = std::thread::Builder::new()
        .name("zfx-perps-journal".into())
        .spawn(move || journal_loop(journal, jrx, ptx, st))
        .map_err(|e| e.to_string())?;
    Ok(Started {
        handle: Handle { tx, stats },
        initial,
        events: prx,
        recovered_records,
        threads: vec![engine_thread, journal_thread],
    })
}

fn append_apply(j: &mut Journal, e: &mut Engine, hash: &mut Hash, seq: u64, c: &Command) -> Result<u64, String> {
    let payload = bincode::serialize(c).map_err(|e| e.to_string())?;
    let seq = seq + 1;
    *hash = j.append(seq, &payload).map_err(|e| e.to_string())?;
    let mut out = vec![];
    e.apply(c, &mut out).map_err(|r| format!("bootstrap {c:?}: {r:?}"))?;
    Ok(seq)
}

fn engine_loop(
    mut engine: Engine,
    rx: Receiver<Req>,
    jtx: Sender<JItem>,
    mut seq: u64,
    mut hash: Hash,
    snapshot_every: u64,
    stats: Arc<Stats>,
) {
    let mut since_snap = 0u64;
    while let Ok(req) = rx.recv() {
        let cmd = match req {
            Req::Cmd(c, reply) => Some((c, reply)),
            Req::Cut => Some((
                Command::CutBatch {
                    journal_seq: seq,
                    journal_hash: hash,
                },
                None,
            )),
            Req::Query(f) => {
                let _ = jtx.send(JItem::Release(f(&engine)));
                None
            }
            Req::Shutdown(ack) => {
                let _ = jtx.send(JItem::Snapshot {
                    seq,
                    hash,
                    bytes: engine.to_snapshot(),
                });
                let _ = jtx.send(JItem::Shutdown(ack));
                return;
            }
        };
        let Some((c, reply)) = cmd else { continue };
        let payload = bincode::serialize(&c).expect("command serializes");
        seq += 1;
        hash = chain(&hash, seq, &payload);
        let mut events = Vec::with_capacity(8);
        let r = engine.apply(&c, &mut events);
        stats.last_seq.store(seq, Ordering::Relaxed);
        if jtx
            .send(JItem::Rec {
                seq,
                payload,
                events,
                reply: reply.map(|t| (t, r)),
            })
            .is_err()
        {
            return;
        }
        since_snap += 1;
        if since_snap >= snapshot_every {
            since_snap = 0;
            let _ = jtx.send(JItem::Snapshot {
                seq,
                hash,
                bytes: engine.to_snapshot(),
            });
        }
    }
}

fn journal_loop(mut journal: Journal, rx: Receiver<JItem>, ptx: Sender<Published>, stats: Arc<Stats>) {
    let dir = journal.dir().to_path_buf();
    let mut group: Vec<JItem> = Vec::with_capacity(4096);
    while let Ok(first) = rx.recv() {
        group.push(first);
        while group.len() < 8192 {
            match rx.try_recv() {
                Ok(i) => group.push(i),
                Err(_) => break,
            }
        }
        let mut wrote = false;
        let mut last = None;
        for item in &group {
            if let JItem::Rec { seq, payload, .. } = item {
                journal.append(*seq, payload).expect("journal append failed; stopping");
                wrote = true;
                last = Some(*seq);
            }
        }
        if wrote {
            journal.sync().expect("journal sync failed; stopping");
            stats.fsyncs.fetch_add(1, Ordering::Relaxed);
        }
        if let Some(s) = last {
            stats.durable_seq.store(s, Ordering::Relaxed);
        }
        for item in group.drain(..) {
            match item {
                JItem::Rec { seq, events, reply, .. } => {
                    if !events.is_empty() {
                        let _ = ptx.send((seq, events));
                    }
                    if let Some((tx, r)) = reply {
                        let _ = tx.send(r);
                    }
                }
                JItem::Release(f) => f(),
                JItem::Snapshot { seq, hash, bytes } => match Journal::write_snapshot(&dir, seq, &hash, &bytes, 3) {
                    Ok(p) => tracing::info!(seq, path = %p.display(), "snapshot"),
                    Err(e) => tracing::error!(%e, "snapshot failed"),
                },
                JItem::Shutdown(ack) => {
                    let _ = journal.sync();
                    let _ = ack.send(());
                    return;
                }
            }
        }
    }
}
