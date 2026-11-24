//! The SQLite adapter: one writer thread, many readers, WAL.
//!
//! SQLite allows one writer at a time. Letting every caller try for the write lock gives `SQLITE_BUSY` under load
//! (measured in another project of ours: N naive writers fail, one writer fed by a queue does not). So:
//!
//! - **One writer thread** owns the only write connection. Callers put a job on a bounded queue and wait for the
//!   answer. A full queue answers [`StoreError::Busy`] at once: nothing was written and the caller may retry.
//! - **Group commit.** The jobs that are waiting are written in one transaction, a savepoint per job so a refused
//!   job does not fail the others. A caller is answered only after `COMMIT`, so an acknowledged op is durable.
//! - **Readers** are separate read-only connections. WAL lets them run while the writer writes.
//! - **`synchronous=FULL`.** With `NORMAL` a power cut can lose a commit that clients were already told about,
//!   and its sequence number would be given to another op. Group commit makes the cost small.
//! - The SQL stays portable (no rowid order, time as integer milliseconds), so a Postgres adapter stays possible.

use crate::memory::now_ms;
use crate::{Appended, OpInput, Result, Snapshot, Store, StoreError, StoredOp};
use rusqlite::{params, Connection, ErrorCode, OpenFlags, OptionalExtension};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{
    sync_channel, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError,
};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

/// `PRAGMA user_version` of the schema below. A database with a higher number is from a newer build.
const SCHEMA_VERSION: i64 = 1;

const MIGRATION_1: &str = "
CREATE TABLE projects (
  id             TEXT PRIMARY KEY,
  schema_version INTEGER NOT NULL CHECK (schema_version >= 1),
  seq            INTEGER NOT NULL DEFAULT 0,
  created_at_ms  INTEGER NOT NULL
);

-- The op log is the source of truth. Append-only; the idempotency key is unique per project.
CREATE TABLE ops (
  project_id      TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  seq             INTEGER NOT NULL,
  idempotency_key TEXT NOT NULL,
  actor           TEXT NOT NULL,
  payload         TEXT NOT NULL,
  schema_version  INTEGER NOT NULL CHECK (schema_version >= 1),
  created_at_ms   INTEGER NOT NULL,
  PRIMARY KEY (project_id, seq),
  UNIQUE (project_id, idempotency_key)
);

CREATE TRIGGER ops_append_only BEFORE UPDATE ON ops
BEGIN
  SELECT RAISE(ABORT, 'ops are append-only');
END;

-- Periodic snapshots of the resolved state: fast load, and the fallback when a replay is older than the kept log.
CREATE TABLE snapshots (
  project_id     TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  seq            INTEGER NOT NULL,
  state          TEXT NOT NULL,
  schema_version INTEGER NOT NULL CHECK (schema_version >= 1),
  created_at_ms  INTEGER NOT NULL,
  PRIMARY KEY (project_id, seq)
);
";

#[derive(Debug, Clone)]
pub struct Options {
    /// Jobs that may wait for the writer. A full queue answers [`StoreError::Busy`].
    pub queue_capacity: usize,
    /// Most jobs written in one transaction.
    pub max_batch: usize,
    /// Read-only connections.
    pub readers: usize,
    /// Cap on the size of the database in pages; writes past it fail with [`StoreError::Full`]. For tests and
    /// for a hard limit on a small disk.
    pub max_page_count: Option<u32>,
    /// A quiet writer checkpoints the WAL after this long.
    pub idle_checkpoint: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            queue_capacity: 1024,
            max_batch: 128,
            readers: 4,
            max_page_count: None,
            idle_checkpoint: Duration::from_secs(2),
        }
    }
}

/// What the writer connection is really configured with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pragmas {
    pub journal_mode: String,
    /// 0 OFF, 1 NORMAL, 2 FULL.
    pub synchronous: i64,
    pub foreign_keys: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    /// Jobs the writer took.
    pub jobs: u64,
    /// Transactions it committed.
    pub commits: u64,
    /// Jobs waiting right now.
    pub queued: usize,
}

#[derive(Default)]
struct Shared {
    jobs: AtomicU64,
    commits: AtomicU64,
    queued: AtomicUsize,
    read_only: AtomicBool,
}

enum Work {
    Create {
        id: String,
        schema_version: u32,
    },
    Append {
        project: String,
        op: OpInput,
    },
    Snapshot {
        project: String,
        seq: u64,
        state: String,
        schema_version: u32,
    },
    Pragmas,
    Pause {
        started: SyncSender<()>,
        release: Receiver<()>,
    },
}

enum Reply {
    Done,
    Appended(Appended),
    Pragmas(Pragmas),
}

struct Job {
    work: Work,
    reply: SyncSender<Result<Reply>>,
}

/// Holds the writer still while it lives. For tests that need a queue to fill up.
#[doc(hidden)]
pub struct PauseGuard {
    _release: SyncSender<()>,
}

pub struct SqliteStore {
    tx: Option<SyncSender<Job>>,
    writer: Option<JoinHandle<()>>,
    readers: Vec<Mutex<Connection>>,
    next_reader: AtomicUsize,
    shared: Arc<Shared>,
}

fn map_err(e: rusqlite::Error) -> StoreError {
    if let rusqlite::Error::SqliteFailure(f, _) = &e {
        match f.code {
            ErrorCode::DiskFull => return StoreError::Full,
            ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked => return StoreError::Busy,
            _ => {}
        }
    }
    StoreError::Io(e.to_string())
}

fn to_i64(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

impl SqliteStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with(path, Options::default())
    }

    pub fn open_with(path: impl AsRef<Path>, opts: Options) -> Result<Self> {
        let path = path.as_ref();
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )
        .map_err(map_err)?;
        configure_writer(&conn, &opts)?;

        let mut readers = Vec::new();
        for _ in 0..opts.readers.max(1) {
            let r = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
                .map_err(map_err)?;
            r.busy_timeout(Duration::from_secs(5)).map_err(map_err)?;
            readers.push(Mutex::new(r));
        }

        let shared = Arc::new(Shared::default());
        let (tx, rx) = sync_channel::<Job>(opts.queue_capacity.max(1));
        let writer = {
            let shared = shared.clone();
            std::thread::Builder::new()
                .name("dibibest-store-writer".into())
                .spawn(move || writer_loop(conn, rx, &shared, &opts))
                .map_err(|e| StoreError::Io(e.to_string()))?
        };
        Ok(Self {
            tx: Some(tx),
            writer: Some(writer),
            readers,
            next_reader: AtomicUsize::new(0),
            shared,
        })
    }

    fn submit(&self, work: Work) -> Result<Reply> {
        let (reply, answer) = sync_channel(1);
        let tx = self.tx.as_ref().expect("the sender lives until drop");
        self.shared.queued.fetch_add(1, Ordering::SeqCst);
        match tx.try_send(Job { work, reply }) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                self.shared.queued.fetch_sub(1, Ordering::SeqCst);
                return Err(StoreError::Busy);
            }
            Err(TrySendError::Disconnected(_)) => {
                self.shared.queued.fetch_sub(1, Ordering::SeqCst);
                return Err(StoreError::Io("the writer stopped".into()));
            }
        }
        answer
            .recv()
            .unwrap_or_else(|_| Err(StoreError::Io("the writer stopped".into())))
    }

    fn read<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T> {
        let i = self.next_reader.fetch_add(1, Ordering::Relaxed) % self.readers.len();
        let conn = self.readers[i]
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        f(&conn).map_err(map_err)
    }

    /// What the writer connection is configured with (the journal mode is the one SQLite reports).
    pub fn pragmas(&self) -> Result<Pragmas> {
        match self.submit(Work::Pragmas)? {
            Reply::Pragmas(p) => Ok(p),
            _ => unreachable!("a pragmas job answers with pragmas"),
        }
    }

    pub fn stats(&self) -> Stats {
        Stats {
            jobs: self.shared.jobs.load(Ordering::SeqCst),
            commits: self.shared.commits.load(Ordering::SeqCst),
            queued: self.shared.queued.load(Ordering::SeqCst),
        }
    }

    /// True while the newest write hit a full disk: reads work, writes are refused. It clears itself when a
    /// later write succeeds.
    pub fn read_only(&self) -> bool {
        self.shared.read_only.load(Ordering::SeqCst)
    }

    /// Writes a consistent copy of the database to a new file (`VACUUM INTO`). It never overwrites a file.
    /// This is the way to back up: copying the `.db` file alone misses what is still in the `-wal` file.
    pub fn backup_to(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if path.exists() {
            return Err(StoreError::Io(format!(
                "{} exists; a backup does not overwrite",
                path.display()
            )));
        }
        let target = path.to_string_lossy().into_owned();
        self.read(|c| c.execute("VACUUM INTO ?1", [target]).map(|_| ()))
    }

    /// Stops the writer after the job it is on, and lets it go when the guard is dropped.
    #[doc(hidden)]
    pub fn pause_writer(&self) -> PauseGuard {
        let (started_tx, started_rx) = sync_channel(1);
        let (release_tx, release_rx) = sync_channel(0);
        let (reply, _answer) = sync_channel(1);
        self.shared.queued.fetch_add(1, Ordering::SeqCst);
        self.tx
            .as_ref()
            .expect("the sender lives until drop")
            .send(Job {
                work: Work::Pause {
                    started: started_tx,
                    release: release_rx,
                },
                reply,
            })
            .expect("the writer is running");
        started_rx.recv().expect("the writer reached the pause");
        PauseGuard {
            _release: release_tx,
        }
    }
}

impl Drop for SqliteStore {
    fn drop(&mut self) {
        drop(self.tx.take());
        if let Some(w) = self.writer.take() {
            let _ = w.join();
        }
    }
}

fn configure_writer(conn: &Connection, opts: &Options) -> Result<()> {
    conn.busy_timeout(Duration::from_secs(5)).map_err(map_err)?;
    let mode: String = conn
        .query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))
        .map_err(map_err)?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(StoreError::Io(format!(
            "could not turn on wal: the journal mode is {mode}"
        )));
    }
    conn.pragma_update(None, "synchronous", "FULL")
        .map_err(map_err)?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(map_err)?;
    conn.pragma_update(None, "journal_size_limit", 64 * 1024 * 1024)
        .map_err(map_err)?;
    migrate(conn)?;
    if let Some(pages) = opts.max_page_count {
        conn.pragma_update(None, "max_page_count", pages)
            .map_err(map_err)?;
    }
    Ok(())
}

fn migrate(conn: &Connection) -> Result<()> {
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(map_err)?;
    if version > SCHEMA_VERSION {
        return Err(StoreError::Io(format!(
            "the database is from a newer version (schema {version}, this build knows {SCHEMA_VERSION})"
        )));
    }
    if version < 1 {
        conn.execute_batch(&format!(
            "BEGIN IMMEDIATE;{MIGRATION_1}PRAGMA user_version = 1;COMMIT;"
        ))
        .map_err(map_err)?;
    }
    Ok(())
}

fn writer_loop(conn: Connection, rx: Receiver<Job>, shared: &Shared, opts: &Options) {
    let mut dirty = false;
    loop {
        let first = match rx.recv_timeout(opts.idle_checkpoint) {
            Ok(job) => job,
            Err(RecvTimeoutError::Timeout) => {
                if dirty {
                    checkpoint(&conn);
                    dirty = false;
                }
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => {
                checkpoint(&conn);
                return;
            }
        };
        shared.queued.fetch_sub(1, Ordering::SeqCst);
        if let Work::Pause { .. } = first.work {
            pause(first);
            continue;
        }
        let mut batch = vec![first];
        let mut deferred = None;
        while batch.len() < opts.max_batch.max(1) {
            match rx.try_recv() {
                Ok(job) => {
                    shared.queued.fetch_sub(1, Ordering::SeqCst);
                    if let Work::Pause { .. } = job.work {
                        deferred = Some(job);
                        break;
                    }
                    batch.push(job);
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        run_batch(&conn, batch, shared);
        dirty = true;
        if let Some(job) = deferred {
            pause(job);
        }
    }
}

fn pause(job: Job) {
    if let Work::Pause { started, release } = job.work {
        let _ = started.send(());
        let _ = release.recv(); // returns when the guard is dropped
    }
}

fn checkpoint(conn: &Connection) {
    let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
}

/// Writes a batch in one transaction and answers every caller after the commit.
fn run_batch(conn: &Connection, batch: Vec<Job>, shared: &Shared) {
    shared.jobs.fetch_add(batch.len() as u64, Ordering::SeqCst);
    if let Err(e) = conn.execute_batch("BEGIN IMMEDIATE") {
        let e = map_err(e);
        shared
            .read_only
            .store(e == StoreError::Full, Ordering::SeqCst);
        for job in batch {
            let _ = job.reply.send(Err(e.clone()));
        }
        return;
    }
    let mut results: Vec<(SyncSender<Result<Reply>>, Result<Reply>)> =
        Vec::with_capacity(batch.len());
    // Set when SQLite threw the whole transaction away (it may, on a full disk or an I/O error).
    let mut lost: Option<StoreError> = None;
    for job in batch {
        if let Some(e) = &lost {
            results.push((job.reply, Err(e.clone())));
            continue;
        }
        if let Err(e) = conn.execute_batch("SAVEPOINT job") {
            lost = Some(map_err(e));
            results.push((job.reply, Err(lost.clone().expect("just set"))));
            continue;
        }
        let r = execute(conn, job.work);
        match &r {
            Ok(_) => {
                let _ = conn.execute_batch("RELEASE job");
            }
            Err(e) if conn.is_autocommit() => {
                lost = Some(e.clone());
                for (_, earlier) in &mut results {
                    if earlier.is_ok() {
                        *earlier = Err(e.clone());
                    }
                }
            }
            Err(_) => {
                let _ = conn.execute_batch("ROLLBACK TO job; RELEASE job");
            }
        }
        results.push((job.reply, r));
    }
    let mut committed = false;
    if lost.is_none() {
        match conn.execute_batch("COMMIT") {
            Ok(()) => {
                committed = true;
                shared.commits.fetch_add(1, Ordering::SeqCst);
            }
            Err(e) => {
                let e = map_err(e);
                let _ = conn.execute_batch("ROLLBACK");
                for (_, r) in &mut results {
                    if r.is_ok() {
                        *r = Err(e.clone());
                    }
                }
            }
        }
    }
    let full = results
        .iter()
        .any(|(_, r)| matches!(r, Err(StoreError::Full)));
    if full {
        shared.read_only.store(true, Ordering::SeqCst);
    } else if committed && results.iter().any(|(_, r)| r.is_ok()) {
        shared.read_only.store(false, Ordering::SeqCst);
    }
    for (reply, r) in results {
        let _ = reply.send(r);
    }
}

fn project_seq(conn: &Connection, id: &str) -> Result<u64> {
    let seq: Option<i64> = conn
        .query_row("SELECT seq FROM projects WHERE id = ?1", [id], |r| r.get(0))
        .optional()
        .map_err(map_err)?;
    seq.map(|n| n as u64).ok_or(StoreError::NotFound)
}

fn execute(conn: &Connection, work: Work) -> Result<Reply> {
    match work {
        Work::Create { id, schema_version } => {
            let exists: bool = conn
                .query_row("SELECT 1 FROM projects WHERE id = ?1", [&id], |_| Ok(true))
                .optional()
                .map_err(map_err)?
                .unwrap_or(false);
            if exists {
                return Err(StoreError::AlreadyExists);
            }
            conn.execute(
                "INSERT INTO projects (id, schema_version, seq, created_at_ms) VALUES (?1, ?2, 0, ?3)",
                params![id, schema_version, now_ms()],
            )
            .map_err(map_err)?;
            Ok(Reply::Done)
        }
        Work::Append { project, op } => {
            let seq = project_seq(conn, &project)?;
            let old: Option<(i64, String, String)> = conn
                .query_row(
                    "SELECT seq, actor, payload FROM ops WHERE project_id = ?1 AND idempotency_key = ?2",
                    params![project, op.key],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()
                .map_err(map_err)?;
            if let Some((old_seq, actor, payload)) = old {
                if actor != op.actor {
                    return Err(StoreError::KeyConflict("the key belongs to another actor"));
                }
                if payload != op.payload {
                    return Err(StoreError::KeyConflict(
                        "the key was used for other content",
                    ));
                }
                return Ok(Reply::Appended(Appended {
                    seq: old_seq as u64,
                    duplicate: true,
                }));
            }
            let next = seq + 1;
            conn.execute(
                "INSERT INTO ops (project_id, seq, idempotency_key, actor, payload, schema_version, created_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![project, to_i64(next), op.key, op.actor, op.payload, op.schema_version, now_ms()],
            )
            .map_err(map_err)?;
            conn.execute(
                "UPDATE projects SET seq = ?2 WHERE id = ?1",
                params![project, to_i64(next)],
            )
            .map_err(map_err)?;
            Ok(Reply::Appended(Appended {
                seq: next,
                duplicate: false,
            }))
        }
        Work::Snapshot {
            project,
            seq,
            state,
            schema_version,
        } => {
            let latest = project_seq(conn, &project)?;
            if seq > latest {
                return Err(StoreError::SnapshotRefused(
                    "the snapshot is ahead of the log",
                ));
            }
            let newest: Option<i64> = conn
                .query_row(
                    "SELECT MAX(seq) FROM snapshots WHERE project_id = ?1",
                    [&project],
                    |r| r.get(0),
                )
                .map_err(map_err)?;
            if newest.is_some_and(|n| to_i64(seq) <= n) {
                return Err(StoreError::SnapshotRefused(
                    "the snapshot is not newer than the latest one",
                ));
            }
            conn.execute(
                "INSERT INTO snapshots (project_id, seq, state, schema_version, created_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![project, to_i64(seq), state, schema_version, now_ms()],
            )
            .map_err(map_err)?;
            Ok(Reply::Done)
        }
        Work::Pragmas => {
            let journal_mode: String = conn
                .query_row("PRAGMA journal_mode", [], |r| r.get(0))
                .map_err(map_err)?;
            let synchronous = conn
                .query_row("PRAGMA synchronous", [], |r| r.get(0))
                .map_err(map_err)?;
            let foreign_keys = conn
                .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
                .map_err(map_err)?;
            Ok(Reply::Pragmas(Pragmas {
                journal_mode,
                synchronous,
                foreign_keys,
            }))
        }
        Work::Pause { .. } => unreachable!("a pause is handled before a batch"),
    }
}

impl Store for SqliteStore {
    fn create_project(&self, id: &str, schema_version: u32) -> Result<()> {
        self.submit(Work::Create {
            id: id.to_string(),
            schema_version,
        })
        .map(|_| ())
    }

    fn append(&self, project: &str, op: OpInput) -> Result<Appended> {
        match self.submit(Work::Append {
            project: project.to_string(),
            op,
        })? {
            Reply::Appended(a) => Ok(a),
            _ => unreachable!("an append answers with its number"),
        }
    }

    fn ops_after(&self, project: &str, after: u64, limit: usize) -> Result<Vec<StoredOp>> {
        self.latest_seq(project)?; // NotFound for an unknown project
        self.read(|c| {
            let mut stmt = c.prepare_cached(
                "SELECT seq, idempotency_key, actor, payload, schema_version, created_at_ms
                 FROM ops WHERE project_id = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3",
            )?;
            let rows = stmt.query_map(
                params![
                    project,
                    to_i64(after),
                    i64::try_from(limit).unwrap_or(i64::MAX)
                ],
                |r| {
                    Ok(StoredOp {
                        seq: r.get::<_, i64>(0)? as u64,
                        key: r.get(1)?,
                        actor: r.get(2)?,
                        payload: r.get(3)?,
                        schema_version: r.get(4)?,
                        created_at_ms: r.get(5)?,
                    })
                },
            )?;
            rows.collect()
        })
    }

    fn latest_seq(&self, project: &str) -> Result<u64> {
        let found: Option<i64> = self.read(|c| {
            c.query_row("SELECT seq FROM projects WHERE id = ?1", [project], |r| {
                r.get(0)
            })
            .optional()
        })?;
        found.map(|n| n as u64).ok_or(StoreError::NotFound)
    }

    fn save_snapshot(
        &self,
        project: &str,
        seq: u64,
        state: &str,
        schema_version: u32,
    ) -> Result<()> {
        self.submit(Work::Snapshot {
            project: project.to_string(),
            seq,
            state: state.to_string(),
            schema_version,
        })
        .map(|_| ())
    }

    fn latest_snapshot(&self, project: &str) -> Result<Option<Snapshot>> {
        self.latest_seq(project)?;
        self.read(|c| {
            c.query_row(
                "SELECT seq, state, schema_version, created_at_ms FROM snapshots
                 WHERE project_id = ?1 ORDER BY seq DESC LIMIT 1",
                [project],
                |r| {
                    Ok(Snapshot {
                        seq: r.get::<_, i64>(0)? as u64,
                        state: r.get(1)?,
                        schema_version: r.get(2)?,
                        created_at_ms: r.get(3)?,
                    })
                },
            )
            .optional()
        })
    }
}
