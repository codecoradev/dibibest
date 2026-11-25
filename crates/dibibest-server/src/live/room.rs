//! The room: one task per project that owns the text and the order (spike, issue #88).
//!
//! Everything that changes a project goes through [`RoomHandle::submit`] or a WebSocket `submit`, and both land
//! in the same place: the actor. It rebases the change over the ops accepted since the version it was made on,
//! asks the gate, appends to the store (the durable log, the only source of `seq`), and only then tells anyone.
//! The text lives in memory for speed; the log is the truth, and a new actor rebuilds the text from it.

use super::frames::{RejectCode, ServerFrame};
use super::gate::{gate, Actor};
use super::text::{from_utf16, transform, Change, Op};
use dibibest_store::{OpInput, SqliteStore, StoreError};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot, Notify};

const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone)]
pub struct RoomConfig {
    /// Ops the room keeps in memory to rebase and replay over. Older ones are in the store but not served.
    pub window: usize,
    /// Frames a client's queue holds before it is given a resync instead.
    pub queue_frames: usize,
    /// Bytes a client's queue holds before the same.
    pub queue_bytes: usize,
}

impl Default for RoomConfig {
    fn default() -> Self {
        Self {
            window: 4096,
            queue_frames: 1024,
            queue_bytes: 4 << 20,
        }
    }
}

/// What the stored payload of an op holds. The author and the key are columns of the log.
#[derive(serde::Serialize, serde::Deserialize)]
struct Payload {
    changes: Vec<Change>,
}

// ---------------------------------------------------------------------------------------------------------
// The queue of one connection
// ---------------------------------------------------------------------------------------------------------

/// What the writer of a connection takes from its queue.
pub enum Next {
    Frame(Arc<str>),
    /// The queue overflowed. The writer asks the room for a resync ([`RoomHandle::resync`]) and keeps reading.
    NeedResync,
    Closed,
}

struct QueueState {
    frames: VecDeque<(Arc<str>, bool)>,
    bytes: usize,
    /// Durable frames were dropped; the client needs the whole text. Further frames are dropped until it has it.
    needs_resync: bool,
    resync_asked: bool,
    closed: bool,
}

/// A bounded queue between the room and the task that writes to one socket. The room never waits on it: a push
/// either fits, or the client is marked as needing a resync and its queue is emptied.
pub struct ClientQueue {
    state: Mutex<QueueState>,
    wake: Notify,
    max_frames: usize,
    max_bytes: usize,
}

impl ClientQueue {
    pub fn new(max_frames: usize, max_bytes: usize) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(QueueState {
                frames: VecDeque::new(),
                bytes: 0,
                needs_resync: false,
                resync_asked: false,
                closed: false,
            }),
            wake: Notify::new(),
            max_frames,
            max_bytes,
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, QueueState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A durable frame. Overflow empties the queue and marks the client.
    fn push_durable(&self, frame: &Arc<str>) {
        let mut s = self.lock();
        if s.needs_resync || s.closed {
            return;
        }
        if s.frames.len() >= self.max_frames || s.bytes + frame.len() > self.max_bytes {
            s.frames.clear();
            s.bytes = 0;
            s.needs_resync = true;
        } else {
            s.bytes += frame.len();
            s.frames.push_back((frame.clone(), true));
        }
        drop(s);
        self.wake.notify_one();
    }

    /// An ephemeral frame: dropped when the queue is more than half full, so presence can never be the reason a
    /// client is given a resync.
    fn push_ephemeral(&self, frame: &Arc<str>) {
        let mut s = self.lock();
        if s.needs_resync || s.closed || s.frames.len() >= self.max_frames / 2 {
            return;
        }
        s.bytes += frame.len();
        s.frames.push_back((frame.clone(), false));
        drop(s);
        self.wake.notify_one();
    }

    /// A frame that must get through (the hello, a rejection, a resync), whatever the state of the queue.
    pub fn push_control_public(&self, frame: Arc<str>) {
        self.push_control(frame);
    }

    fn push_control(&self, frame: Arc<str>) {
        let mut s = self.lock();
        s.bytes += frame.len();
        s.frames.push_back((frame, true));
        drop(s);
        self.wake.notify_one();
    }

    /// Replaces everything queued by a resync and lets durable frames in again.
    fn reset_with(&self, frame: Arc<str>) {
        let mut s = self.lock();
        s.frames.clear();
        s.bytes = frame.len();
        s.frames.push_back((frame, true));
        s.needs_resync = false;
        s.resync_asked = false;
        drop(s);
        self.wake.notify_one();
    }

    pub fn close(&self) {
        self.lock().closed = true;
        self.wake.notify_one();
    }

    pub fn len(&self) -> usize {
        self.lock().frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub async fn next(&self) -> Next {
        loop {
            {
                let mut s = self.lock();
                if s.needs_resync && !s.resync_asked {
                    s.resync_asked = true;
                    return Next::NeedResync;
                }
                if let Some((f, _)) = s.frames.pop_front() {
                    s.bytes -= f.len();
                    return Next::Frame(f);
                }
                if s.closed {
                    return Next::Closed;
                }
            }
            self.wake.notified().await;
        }
    }
}

// ---------------------------------------------------------------------------------------------------------
// The actor
// ---------------------------------------------------------------------------------------------------------

/// A change that was accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Accepted {
    pub seq: u64,
    /// True when the key was already applied: nothing new was written.
    pub duplicate: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejection {
    pub code: RejectCode,
    pub message: String,
    pub seq: u64,
}

pub struct JoinInfo {
    pub conn: u64,
    pub actor: String,
}

enum Cmd {
    Join {
        actor: String,
        since: Option<u64>,
        queue: Arc<ClientQueue>,
        reply: oneshot::Sender<JoinInfo>,
    },
    Leave {
        conn: u64,
    },
    Submit {
        /// The connection that sent it, for an ack in the stream. `None` for a call that is not on a socket.
        conn: Option<u64>,
        actor: Actor,
        key: String,
        base_seq: u64,
        changes: Vec<Change>,
        reply: Option<oneshot::Sender<Result<Accepted, Rejection>>>,
    },
    Presence {
        conn: u64,
        cursor: Option<u32>,
        selection: Option<(u32, u32)>,
    },
    Resync {
        conn: u64,
    },
    Text {
        reply: oneshot::Sender<(u64, String)>,
    },
    Stats {
        reply: oneshot::Sender<Stats>,
    },
}

/// Counters of one room since it started.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    pub accepted: u64,
    /// Changes that were made on an older version and had to be rebased.
    pub rebased: u64,
    /// The most ops one change was rebased over.
    pub max_rebase_depth: u64,
    pub clients: usize,
    pub window: usize,
}

struct Client {
    actor: String,
    queue: Arc<ClientQueue>,
}

struct WindowOp {
    seq: u64,
    key: String,
    actor: String,
    base_len: usize,
    op: Op,
}

struct Room {
    project: String,
    store: Arc<SqliteStore>,
    cfg: RoomConfig,
    text: Vec<u16>,
    seq: u64,
    window: VecDeque<WindowOp>,
    keys: HashMap<String, u64>,
    clients: HashMap<u64, Client>,
    next_conn: u64,
    stats: Stats,
}

/// A way to talk to a room. Cheap to clone.
#[derive(Clone)]
pub struct RoomHandle {
    tx: mpsc::Sender<Cmd>,
    cfg: RoomConfig,
}

impl RoomHandle {
    /// Loads (or creates) the project from the store and starts its actor.
    pub async fn open(
        store: Arc<SqliteStore>,
        project: &str,
        cfg: RoomConfig,
    ) -> Result<RoomHandle, StoreError> {
        let (s, p, c) = (store.clone(), project.to_string(), cfg.clone());
        let room = tokio::task::spawn_blocking(move || Room::load(s, p, c))
            .await
            .map_err(|e| StoreError::Io(e.to_string()))??;
        let (tx, rx) = mpsc::channel(1024);
        tokio::spawn(room.run(rx));
        Ok(RoomHandle { tx, cfg })
    }

    pub fn config(&self) -> &RoomConfig {
        &self.cfg
    }

    /// A new queue sized by this room's config.
    pub fn new_queue(&self) -> Arc<ClientQueue> {
        ClientQueue::new(self.cfg.queue_frames, self.cfg.queue_bytes)
    }

    pub async fn join(
        &self,
        actor: &str,
        since: Option<u64>,
        queue: Arc<ClientQueue>,
    ) -> Option<JoinInfo> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Cmd::Join {
                actor: actor.to_string(),
                since,
                queue,
                reply,
            })
            .await
            .ok()?;
        rx.await.ok()
    }

    pub async fn leave(&self, conn: u64) {
        let _ = self.tx.send(Cmd::Leave { conn }).await;
    }

    /// A submit from a socket: the answer is an `ack` or a `rejected` frame in the connection's stream.
    pub async fn submit_from(
        &self,
        conn: u64,
        actor: Actor,
        key: String,
        base_seq: u64,
        changes: Vec<Change>,
    ) {
        let _ = self
            .tx
            .send(Cmd::Submit {
                conn: Some(conn),
                actor,
                key,
                base_seq,
                changes,
                reply: None,
            })
            .await;
    }

    /// The entry point that is not a socket: a change from an agent over REST or MCP, or any caller that wants
    /// the answer. It passes the same gate and gets the same order as a socket's submit.
    pub async fn submit(
        &self,
        actor: Actor,
        key: String,
        base_seq: u64,
        changes: Vec<Change>,
    ) -> Result<Accepted, Rejection> {
        let (reply, rx) = oneshot::channel();
        let sent = self
            .tx
            .send(Cmd::Submit {
                conn: None,
                actor,
                key,
                base_seq,
                changes,
                reply: Some(reply),
            })
            .await;
        match (sent, rx.await) {
            (Ok(()), Ok(r)) => r,
            _ => Err(Rejection {
                code: RejectCode::Internal,
                message: "the room is gone".into(),
                seq: 0,
            }),
        }
    }

    pub async fn presence(&self, conn: u64, cursor: Option<u32>, selection: Option<(u32, u32)>) {
        let _ = self
            .tx
            .send(Cmd::Presence {
                conn,
                cursor,
                selection,
            })
            .await;
    }

    /// The connection's queue overflowed: give it the whole text.
    pub async fn resync(&self, conn: u64) {
        let _ = self.tx.send(Cmd::Resync { conn }).await;
    }

    pub async fn stats(&self) -> Option<Stats> {
        let (reply, rx) = oneshot::channel();
        self.tx.send(Cmd::Stats { reply }).await.ok()?;
        rx.await.ok()
    }

    /// The current `(seq, text)`.
    pub async fn text(&self) -> Option<(u64, String)> {
        let (reply, rx) = oneshot::channel();
        self.tx.send(Cmd::Text { reply }).await.ok()?;
        rx.await.ok()
    }
}

fn frame(f: &ServerFrame) -> Arc<str> {
    Arc::from(serde_json::to_string(f).expect("a frame serialises"))
}

fn reject(code: RejectCode, message: impl Into<String>, seq: u64) -> Rejection {
    Rejection {
        code,
        message: message.into(),
        seq,
    }
}

impl Room {
    /// Rebuilds the text from the log (the spike has no snapshots: it replays from 1).
    fn load(store: Arc<SqliteStore>, project: String, cfg: RoomConfig) -> Result<Room, StoreError> {
        use dibibest_store::Store;
        match store.latest_seq(&project) {
            Err(StoreError::NotFound) => {
                // The spike creates a project on first use. The real build has projects from stage 1.
                match store.create_project(&project, SCHEMA_VERSION) {
                    Ok(()) | Err(StoreError::AlreadyExists) => {}
                    Err(e) => return Err(e),
                }
            }
            Err(e) => return Err(e),
            Ok(_) => {}
        }
        let mut room = Room {
            project: project.clone(),
            store: store.clone(),
            cfg,
            text: Vec::new(),
            seq: 0,
            window: VecDeque::new(),
            keys: HashMap::new(),
            clients: HashMap::new(),
            next_conn: 1,
            stats: Stats::default(),
        };
        loop {
            let ops = store.ops_after(&project, room.seq, 1000)?;
            if ops.is_empty() {
                break;
            }
            for o in ops {
                let p: Payload = serde_json::from_str(&o.payload)
                    .map_err(|e| StoreError::Io(format!("op {}: {e}", o.seq)))?;
                let op = Op::from_changes(&p.changes, room.text.len())
                    .map_err(|e| StoreError::Io(format!("op {}: {e}", o.seq)))?;
                room.text = op
                    .apply(&room.text)
                    .map_err(|e| StoreError::Io(format!("op {}: {e}", o.seq)))?;
                room.seq = o.seq;
                room.remember(o.seq, o.key, o.actor, op);
            }
        }
        Ok(room)
    }

    fn remember(&mut self, seq: u64, key: String, actor: String, op: Op) {
        self.keys.insert(key.clone(), seq);
        self.window.push_back(WindowOp {
            seq,
            key,
            actor,
            base_len: op.base_len(),
            op,
        });
        while self.window.len() > self.cfg.window {
            if let Some(old) = self.window.pop_front() {
                self.keys.remove(&old.key);
            }
        }
    }

    /// The first `seq` the window can serve ops for; a client at `base_seq` needs `base_seq + 1` to be in it.
    fn window_start(&self) -> u64 {
        self.window.front().map_or(self.seq + 1, |w| w.seq)
    }

    async fn run(mut self, mut rx: mpsc::Receiver<Cmd>) {
        while let Some(cmd) = rx.recv().await {
            match cmd {
                Cmd::Join {
                    actor,
                    since,
                    queue,
                    reply,
                } => {
                    let conn = self.next_conn;
                    self.next_conn += 1;
                    self.join(conn, &actor, since, &queue);
                    self.clients.insert(
                        conn,
                        Client {
                            actor: actor.clone(),
                            queue,
                        },
                    );
                    let _ = reply.send(JoinInfo { conn, actor });
                }
                Cmd::Leave { conn } => {
                    if let Some(c) = self.clients.remove(&conn) {
                        c.queue.close();
                        let f = frame(&ServerFrame::PresenceLeft {
                            conn,
                            actor: c.actor,
                        });
                        self.fan_out_ephemeral(&f, None);
                    }
                }
                Cmd::Submit {
                    conn,
                    actor,
                    key,
                    base_seq,
                    changes,
                    reply,
                } => {
                    let r = self.submit(&actor, &key, base_seq, changes, conn).await;
                    if let (Err(rej), Some(c)) = (&r, conn.and_then(|c| self.clients.get(&c))) {
                        c.queue.push_control(frame(&ServerFrame::Rejected {
                            key: Some(key.clone()),
                            code: rej.code,
                            message: rej.message.clone(),
                            seq: rej.seq,
                        }));
                        if rej.code == RejectCode::TooOld {
                            c.queue.push_control(self.resync_frame());
                        }
                    }
                    if let Some(reply) = reply {
                        let _ = reply.send(r);
                    }
                }
                Cmd::Presence {
                    conn,
                    cursor,
                    selection,
                } => {
                    if let Some(c) = self.clients.get(&conn) {
                        let f = frame(&ServerFrame::Presence {
                            conn,
                            actor: c.actor.clone(),
                            cursor,
                            selection,
                        });
                        self.fan_out_ephemeral(&f, Some(conn));
                    }
                }
                Cmd::Resync { conn } => {
                    let f = self.resync_frame();
                    if let Some(c) = self.clients.get(&conn) {
                        c.queue.reset_with(f);
                    }
                }
                Cmd::Text { reply } => {
                    let _ = reply.send((self.seq, from_utf16(&self.text)));
                }
                Cmd::Stats { reply } => {
                    let _ = reply.send(Stats {
                        clients: self.clients.len(),
                        window: self.window.len(),
                        ..self.stats
                    });
                }
            }
        }
        tracing::debug!(project = %self.project, "room stopped");
    }

    fn resync_frame(&self) -> Arc<str> {
        frame(&ServerFrame::Resync {
            seq: self.seq,
            text: from_utf16(&self.text),
        })
    }

    fn join(&self, conn: u64, actor: &str, since: Option<u64>, queue: &Arc<ClientQueue>) {
        let hello = |text: Option<String>| ServerFrame::Hello {
            seq: self.seq,
            conn,
            actor: actor.to_string(),
            text,
        };
        match since {
            None => queue.push_control(frame(&hello(Some(from_utf16(&self.text))))),
            Some(s) => {
                let missed = self.seq.saturating_sub(s) as usize;
                let can_replay = s <= self.seq
                    && (s == self.seq || s + 1 >= self.window_start())
                    && missed <= self.cfg.queue_frames / 2;
                if can_replay {
                    queue.push_control(frame(&hello(None)));
                    for w in self.window.iter().filter(|w| w.seq > s) {
                        queue.push_control(self.op_frame(w));
                    }
                } else {
                    // Too far behind (or ahead of this room): no hello, the whole text as a resync.
                    queue.push_control(self.resync_frame());
                }
            }
        }
    }

    fn op_frame(&self, w: &WindowOp) -> Arc<str> {
        frame(&ServerFrame::Op {
            seq: w.seq,
            actor: w.actor.clone(),
            key: w.key.clone(),
            changes: w.op.to_changes(),
        })
    }

    fn fan_out_ephemeral(&self, f: &Arc<str>, except: Option<u64>) {
        for (id, c) in &self.clients {
            if Some(*id) != except {
                c.queue.push_ephemeral(f);
            }
        }
    }

    async fn submit(
        &mut self,
        actor: &Actor,
        key: &str,
        base_seq: u64,
        changes: Vec<Change>,
        conn: Option<u64>,
    ) -> Result<Accepted, Rejection> {
        // A key the room still remembers was applied: say so again, apply nothing.
        if let Some(&seq) = self.keys.get(key) {
            if let Some(c) = conn.and_then(|c| self.clients.get(&c)) {
                c.queue.push_control(frame(&ServerFrame::Ack {
                    key: key.to_string(),
                    seq,
                }));
            }
            return Ok(Accepted {
                seq,
                duplicate: true,
            });
        }
        if base_seq > self.seq {
            return Err(reject(
                RejectCode::BaseInFuture,
                format!("base_seq {base_seq} is ahead of the room ({})", self.seq),
                self.seq,
            ));
        }
        if base_seq < self.seq && base_seq + 1 < self.window_start() {
            return Err(reject(
                RejectCode::TooOld,
                format!(
                    "base_seq {base_seq} is older than the replay window (starts at {})",
                    self.window_start()
                ),
                self.seq,
            ));
        }
        // The text the change was made on has this length: the current one, undone by the ops since.
        let base_len = self
            .window
            .iter()
            .find(|w| w.seq == base_seq + 1)
            .map_or(self.text.len(), |w| w.base_len);
        let mut op = Op::from_changes(&changes, base_len)
            .map_err(|e| reject(RejectCode::BadChange, e.to_string(), self.seq))?;
        let depth = self.seq - base_seq;
        // Rebase: the accepted ops go first, so a late change moves behind them, never the other way round.
        for w in self.window.iter().filter(|w| w.seq > base_seq) {
            op = transform(&w.op, &op)
                .map_err(|e| reject(RejectCode::Internal, e.to_string(), self.seq))?
                .1;
        }
        let new_text = op
            .apply(&self.text)
            .map_err(|e| reject(RejectCode::Internal, e.to_string(), self.seq))?;
        if let Err(r) = gate(actor, new_text.len(), || from_utf16(&new_text)) {
            return Err(reject(RejectCode::Refused, r.message, self.seq));
        }
        let payload = serde_json::to_string(&Payload {
            changes: op.to_changes(),
        })
        .map_err(|e| reject(RejectCode::Internal, e.to_string(), self.seq))?;
        let input = OpInput {
            key: key.to_string(),
            actor: actor.name.clone(),
            payload,
            schema_version: SCHEMA_VERSION,
        };
        let (store, project) = (self.store.clone(), self.project.clone());
        use dibibest_store::Store;
        let appended = tokio::task::spawn_blocking(move || store.append(&project, input))
            .await
            .map_err(|e| reject(RejectCode::Internal, e.to_string(), self.seq))?
            .map_err(|e| match e {
                StoreError::Busy => reject(RejectCode::Busy, "the store is busy", self.seq),
                StoreError::KeyConflict(m) => reject(RejectCode::KeyConflict, m, self.seq),
                other => reject(RejectCode::Internal, other.to_string(), self.seq),
            })?;
        if appended.duplicate || appended.seq != self.seq + 1 {
            // Another writer is appending to this project, or the log moved under us: the room's picture is
            // wrong and it must not guess. The spike stops here.
            return Err(reject(
                RejectCode::Internal,
                format!(
                    "the log answered {} (duplicate {}) while the room is at {}",
                    appended.seq, appended.duplicate, self.seq
                ),
                self.seq,
            ));
        }
        // Durable: now it is the new text, and now everyone hears.
        self.text = new_text;
        self.seq = appended.seq;
        self.stats.accepted += 1;
        if depth > 0 {
            self.stats.rebased += 1;
            self.stats.max_rebase_depth = self.stats.max_rebase_depth.max(depth);
        }
        let op_frame = frame(&ServerFrame::Op {
            seq: self.seq,
            actor: actor.name.clone(),
            key: key.to_string(),
            changes: op.to_changes(),
        });
        let ack = frame(&ServerFrame::Ack {
            key: key.to_string(),
            seq: self.seq,
        });
        for (id, c) in &self.clients {
            if Some(*id) == conn {
                c.queue.push_durable(&ack);
            } else {
                c.queue.push_durable(&op_frame);
            }
        }
        self.remember(self.seq, key.to_string(), actor.name.clone(), op);
        Ok(Accepted {
            seq: self.seq,
            duplicate: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(n: usize) -> Arc<str> {
        Arc::from("x".repeat(n))
    }

    async fn drain(q: &ClientQueue) -> Vec<Arc<str>> {
        let mut v = Vec::new();
        while !q.is_empty() {
            match q.next().await {
                Next::Frame(x) => v.push(x),
                _ => break,
            }
        }
        v
    }

    #[tokio::test]
    async fn a_full_queue_is_emptied_and_marks_the_client_for_a_resync() {
        let q = ClientQueue::new(4, 1 << 20);
        for _ in 0..4 {
            q.push_durable(&f(10));
        }
        assert_eq!(q.len(), 4);
        q.push_durable(&f(10)); // the fifth does not fit
        assert_eq!(q.len(), 0);
        assert!(matches!(q.next().await, Next::NeedResync));
        // Until the resync is in, nothing durable is queued: it would be older than the resync.
        q.push_durable(&f(10));
        assert_eq!(q.len(), 0);
        q.reset_with(f(3));
        q.push_durable(&f(10));
        assert_eq!(drain(&q).await.len(), 2);
    }

    #[tokio::test]
    async fn ephemeral_frames_are_dropped_early_and_never_cause_a_resync() {
        let q = ClientQueue::new(10, 1 << 20);
        for _ in 0..100 {
            q.push_ephemeral(&f(10));
        }
        assert_eq!(q.len(), 5, "presence may fill half the queue and no more");
        // No resync was asked for, and a durable frame still fits.
        q.push_durable(&f(10));
        assert_eq!(q.len(), 6);
        let frames = drain(&q).await;
        assert_eq!(frames.len(), 6);
    }

    #[tokio::test]
    async fn the_byte_limit_counts_too() {
        let q = ClientQueue::new(1000, 100);
        q.push_durable(&f(60));
        q.push_durable(&f(60));
        assert!(matches!(q.next().await, Next::NeedResync));
    }
}
