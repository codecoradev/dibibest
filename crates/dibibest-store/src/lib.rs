//! Durable storage for projects (epic #84, issue #92).
//!
//! A project has an append-only log of ops with one total order (`seq`, starting at 1) and, now and then, a
//! snapshot of its resolved state. The store does not interpret an op: the payload is opaque text (JSON) and
//! every row carries the `schema_version` of the shape it was written in, so a later change can migrate old data
//! on purpose instead of guessing.
//!
//! [`Store`] is the seam. [`MemoryStore`] is the reference for behaviour and the double used in tests; the SQLite
//! adapter follows in `sqlite`. Every adapter runs the same cases, see [`contract`].

pub mod contract;
mod memory;
mod sqlite;

pub use memory::MemoryStore;
pub use sqlite::{Options, PauseGuard, Pragmas, SqliteStore, Stats};

/// An op as the caller hands it over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpInput {
    /// Idempotency key, unique per project. A retry with the same key and the same content is not written twice.
    pub key: String,
    /// Who made the op (a person or an agent grant).
    pub actor: String,
    /// The op itself, opaque to the store.
    pub payload: String,
    pub schema_version: u32,
}

/// An op as the log returns it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredOp {
    pub seq: u64,
    pub key: String,
    pub actor: String,
    pub payload: String,
    pub schema_version: u32,
    pub created_at_ms: i64,
}

/// The answer to an append.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Appended {
    pub seq: u64,
    /// True when the key was already in the log: nothing was written and `seq` is the number it got then.
    pub duplicate: bool,
}

/// A snapshot of the resolved state of a project at `seq`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub seq: u64,
    pub state: String,
    pub schema_version: u32,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum StoreError {
    #[error("project not found")]
    NotFound,
    #[error("project already exists")]
    AlreadyExists,
    /// The idempotency key was used before by another actor, or for different content.
    #[error("idempotency key conflict: {0}")]
    KeyConflict(&'static str),
    /// A snapshot that is ahead of the log, or not newer than the latest one.
    #[error("snapshot refused: {0}")]
    SnapshotRefused(&'static str),
    /// The write queue is full. Retryable: nothing was written.
    #[error("store is busy")]
    Busy,
    /// The disk is full. Reads still work; writes are refused until space is freed.
    #[error("disk full")]
    Full,
    #[error("store error: {0}")]
    Io(String),
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// Projects, their op log and snapshots. Adapters are shared between threads.
pub trait Store: Send + Sync {
    fn create_project(&self, id: &str, schema_version: u32) -> Result<()>;
    /// Adds an op at the end of the log of `project`. The returned number is final: the op is durable.
    fn append(&self, project: &str, op: OpInput) -> Result<Appended>;
    /// At most `limit` ops with a sequence number above `after`, in order.
    fn ops_after(&self, project: &str, after: u64, limit: usize) -> Result<Vec<StoredOp>>;
    /// The number of the newest op, 0 for an empty log.
    fn latest_seq(&self, project: &str) -> Result<u64>;
    /// Keeps `state` as the snapshot at `seq`. `seq` must not be ahead of the log and must be newer than the
    /// latest snapshot. A snapshot never changes the log.
    fn save_snapshot(
        &self,
        project: &str,
        seq: u64,
        state: &str,
        schema_version: u32,
    ) -> Result<()>;
    /// The snapshot with the highest `seq`, if any.
    fn latest_snapshot(&self, project: &str) -> Result<Option<Snapshot>>;
}
