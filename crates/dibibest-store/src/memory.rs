//! The in-memory adapter: the reference for what a store does, and the double used in tests.

use crate::{Appended, OpInput, Result, Snapshot, Store, StoreError, StoredOp};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Default)]
struct Project {
    ops: Vec<StoredOp>,
    snapshots: Vec<Snapshot>,
}

#[derive(Default)]
pub struct MemoryStore {
    projects: Mutex<HashMap<String, Project>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn with<T>(&self, id: &str, f: impl FnOnce(&mut Project) -> Result<T>) -> Result<T> {
        let mut projects = self.projects.lock().unwrap();
        f(projects.get_mut(id).ok_or(StoreError::NotFound)?)
    }
}

/// Milliseconds since the Unix epoch (0 if the clock is before it).
pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

impl Store for MemoryStore {
    fn create_project(&self, id: &str, _schema_version: u32) -> Result<()> {
        let mut projects = self.projects.lock().unwrap();
        if projects.contains_key(id) {
            return Err(StoreError::AlreadyExists);
        }
        projects.insert(id.to_string(), Project::default());
        Ok(())
    }

    fn append(&self, project: &str, op: OpInput) -> Result<Appended> {
        self.with(project, |p| {
            if let Some(old) = p.ops.iter().find(|o| o.key == op.key) {
                if old.actor != op.actor {
                    return Err(StoreError::KeyConflict("the key belongs to another actor"));
                }
                if old.payload != op.payload {
                    return Err(StoreError::KeyConflict(
                        "the key was used for other content",
                    ));
                }
                return Ok(Appended {
                    seq: old.seq,
                    duplicate: true,
                });
            }
            let seq = p.ops.len() as u64 + 1;
            p.ops.push(StoredOp {
                seq,
                key: op.key,
                actor: op.actor,
                payload: op.payload,
                schema_version: op.schema_version,
                created_at_ms: now_ms(),
            });
            Ok(Appended {
                seq,
                duplicate: false,
            })
        })
    }

    fn ops_after(&self, project: &str, after: u64, limit: usize) -> Result<Vec<StoredOp>> {
        self.with(project, |p| {
            Ok(p.ops
                .iter()
                .filter(|o| o.seq > after)
                .take(limit)
                .cloned()
                .collect())
        })
    }

    fn latest_seq(&self, project: &str) -> Result<u64> {
        self.with(project, |p| Ok(p.ops.len() as u64))
    }

    fn save_snapshot(
        &self,
        project: &str,
        seq: u64,
        state: &str,
        schema_version: u32,
    ) -> Result<()> {
        self.with(project, |p| {
            if seq > p.ops.len() as u64 {
                return Err(StoreError::SnapshotRefused(
                    "the snapshot is ahead of the log",
                ));
            }
            if p.snapshots.last().is_some_and(|s| seq <= s.seq) {
                return Err(StoreError::SnapshotRefused(
                    "the snapshot is not newer than the latest one",
                ));
            }
            p.snapshots.push(Snapshot {
                seq,
                state: state.to_string(),
                schema_version,
                created_at_ms: now_ms(),
            });
            Ok(())
        })
    }

    fn latest_snapshot(&self, project: &str) -> Result<Option<Snapshot>> {
        self.with(project, |p| Ok(p.snapshots.last().cloned()))
    }
}
