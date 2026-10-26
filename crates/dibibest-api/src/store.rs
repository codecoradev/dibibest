//! SQLite saved-projects store. One connection behind a mutex.
// ponytail: single mutex-guarded connection; use a pool if write contention shows up.

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::sync::Mutex;

pub const MAX_PROJECT_BYTES: usize = 512 * 1024;
pub const MAX_PROJECTS_PER_USER: i64 = 100;

#[derive(Serialize, Debug)]
pub struct Summary {
    pub id: String,
    pub name: String,
    pub updated_at: String,
}

#[derive(Serialize, Debug)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub erdjson: serde_json::Value,
    pub updated_at: String,
}

pub enum CreateErr {
    LimitReached,
    Db(rusqlite::Error),
}

pub struct Store(Mutex<Connection>);

impl Store {
    pub fn open(path: &str) -> rusqlite::Result<Self> {
        let c = if path == ":memory:" {
            Connection::open_in_memory()?
        } else {
            Connection::open(path)?
        };
        c.execute_batch(
            "CREATE TABLE IF NOT EXISTS projects(
               id TEXT PRIMARY KEY, owner_email TEXT NOT NULL, name TEXT NOT NULL,
               erdjson TEXT NOT NULL, updated_at TEXT NOT NULL);
             CREATE INDEX IF NOT EXISTS projects_owner ON projects(owner_email);",
        )?;
        Ok(Self(Mutex::new(c)))
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn list(&self, owner: &str) -> rusqlite::Result<Vec<Summary>> {
        let c = self.conn();
        let mut st = c.prepare("SELECT id,name,updated_at FROM projects WHERE owner_email=?1 ORDER BY updated_at DESC, id")?;
        let rows = st.query_map([owner], |r| {
            Ok(Summary {
                id: r.get(0)?,
                name: r.get(1)?,
                updated_at: r.get(2)?,
            })
        })?;
        rows.collect()
    }

    pub fn create(
        &self,
        owner: &str,
        id: &str,
        name: &str,
        erdjson: &str,
    ) -> Result<(), CreateErr> {
        let c = self.conn();
        let n: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM projects WHERE owner_email=?1",
                [owner],
                |r| r.get(0),
            )
            .map_err(CreateErr::Db)?;
        if n >= MAX_PROJECTS_PER_USER {
            return Err(CreateErr::LimitReached);
        }
        c.execute(
            "INSERT INTO projects VALUES(?1,?2,?3,?4,strftime('%Y-%m-%dT%H:%M:%SZ','now'))",
            params![id, owner, name, erdjson],
        )
        .map_err(CreateErr::Db)?;
        Ok(())
    }

    pub fn get(&self, owner: &str, id: &str) -> rusqlite::Result<Option<Project>> {
        self.conn()
            .query_row(
                "SELECT id,name,erdjson,updated_at FROM projects WHERE id=?1 AND owner_email=?2",
                params![id, owner],
                |r| {
                    let raw: String = r.get(2)?;
                    Ok(Project {
                        id: r.get(0)?,
                        name: r.get(1)?,
                        erdjson: serde_json::from_str(&raw).unwrap_or_default(),
                        updated_at: r.get(3)?,
                    })
                },
            )
            .optional()
    }

    /// true if a row was updated.
    pub fn update(
        &self,
        owner: &str,
        id: &str,
        name: &str,
        erdjson: &str,
    ) -> rusqlite::Result<bool> {
        Ok(self.conn().execute(
            "UPDATE projects SET name=?3, erdjson=?4, updated_at=strftime('%Y-%m-%dT%H:%M:%SZ','now') WHERE id=?1 AND owner_email=?2",
            params![id, owner, name, erdjson],
        )? > 0)
    }

    pub fn delete(&self, owner: &str, id: &str) -> rusqlite::Result<bool> {
        Ok(self.conn().execute(
            "DELETE FROM projects WHERE id=?1 AND owner_email=?2",
            params![id, owner],
        )? > 0)
    }
}
