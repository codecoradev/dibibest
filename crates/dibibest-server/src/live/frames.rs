//! The wire frames of the live WebSocket (spike, issue #88): JSON text frames with a `type` tag.
//!
//! *Durable* frames carry a `seq`: they are in the op log, a client can ask for them again with `since_seq`, and
//! a client that cannot be given all of them is given a `resync` instead. *Ephemeral* frames (presence) carry no
//! `seq`, are never stored or replayed, and are dropped for a client that cannot keep up.

use super::text::Change;
use serde::{Deserialize, Serialize};

/// What a client sends.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientFrame {
    /// A change made on the text as it was at `base_seq`. `key` is the idempotency key: sending the same key
    /// again (after a reconnect, say) never applies the change twice. The author is the connection's actor, not
    /// a field a client can claim.
    Submit {
        key: String,
        base_seq: u64,
        changes: Vec<Change>,
    },
    /// Where this client's cursor and selection are. Ephemeral.
    Presence {
        cursor: Option<u32>,
        selection: Option<(u32, u32)>,
    },
}

/// Why a submit was not applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectCode {
    /// The frame was not valid JSON for a [`ClientFrame`].
    BadFrame,
    /// The replacements are out of bounds, out of order or overlap.
    BadChange,
    /// `base_seq` is older than the replay window: the room no longer has the ops to rebase over. A `resync`
    /// follows on the connection.
    TooOld,
    /// `base_seq` is ahead of the room: the client has a text this room never had.
    BaseInFuture,
    /// The gate refused the result (an agent's change that leaves a schema that does not parse, a text over the
    /// size limit).
    Refused,
    /// The key was used before for another actor.
    KeyConflict,
    /// The store could not take the write now. Nothing was written; the same submit can be sent again.
    Busy,
    Internal,
}

/// What the server sends.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerFrame {
    /// First frame of a connection. `text` is the whole text at `seq`, or `None` when the client asked to
    /// continue from `since_seq`: its text is valid at `since_seq` and the missed `op` frames follow, in order,
    /// up to and including `seq`.
    Hello {
        seq: u64,
        conn: u64,
        actor: String,
        text: Option<String>,
    },
    /// Durable. Change number `seq`, made by `actor`, already rebased: it applies to the text at `seq - 1`.
    /// Everyone but the author of a live submit gets it; the author gets an `ack` in the same place of the
    /// stream. A replayed op of one's own key is also a confirmation.
    Op {
        seq: u64,
        actor: String,
        key: String,
        changes: Vec<Change>,
    },
    /// Durable, to the sender of `key` only: it is change number `seq`. Sent again, unchanged, for a retried key.
    Ack { key: String, seq: u64 },
    /// A submit that was not applied. `seq` is the room's number now.
    Rejected {
        key: Option<String>,
        code: RejectCode,
        message: String,
        seq: u64,
    },
    /// Durable marker: the whole text at `seq`. The client replaces its text with it and drops what it had not
    /// had confirmed (it cannot rebase without the ops). Sent when replay is not possible or the client's queue
    /// overflowed.
    Resync { seq: u64, text: String },
    /// Ephemeral.
    Presence {
        conn: u64,
        actor: String,
        cursor: Option<u32>,
        selection: Option<(u32, u32)>,
    },
    /// Ephemeral: a connection went away.
    PresenceLeft { conn: u64, actor: String },
}

impl ServerFrame {
    pub fn is_durable(&self) -> bool {
        !matches!(
            self,
            ServerFrame::Presence { .. } | ServerFrame::PresenceLeft { .. }
        )
    }
}
