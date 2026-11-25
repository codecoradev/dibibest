//! Live editing over a WebSocket (spike, issue #88). See `docs/spikes/live-editing.md`.
//!
//! `GET /v1/live/{project}?since_seq=N&actor=name` upgrades to a WebSocket whose frames are in [`frames`]. The
//! room actor in [`room`] orders the changes, [`gate`] decides what is let in, [`text`] rebases. Authentication
//! is a stub here: the actor is a query parameter. The real build takes it from the session.

pub mod frames;
pub mod gate;
pub mod room;
pub mod text;

use crate::app::AppState;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use dibibest_store::SqliteStore;
use frames::{ClientFrame, RejectCode, ServerFrame};
use futures_util::{SinkExt, StreamExt};
use gate::Actor;
use room::{Next, RoomConfig, RoomHandle};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use text::Change;

/// The rooms of one server, one per project, started when first used.
///
/// The spike never stops a room (no idle eviction) and holds the map's lock while a room loads from the log.
pub struct Rooms {
    cfg: RoomConfig,
    map: tokio::sync::Mutex<HashMap<String, RoomHandle>>,
}

impl Rooms {
    pub fn new(cfg: RoomConfig) -> Self {
        Self {
            cfg,
            map: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    pub async fn get(
        &self,
        store: &Arc<SqliteStore>,
        project: &str,
    ) -> Result<RoomHandle, dibibest_store::StoreError> {
        let mut map = self.map.lock().await;
        if let Some(h) = map.get(project) {
            return Ok(h.clone());
        }
        let h = RoomHandle::open(store.clone(), project, self.cfg.clone()).await?;
        map.insert(project.to_string(), h.clone());
        Ok(h)
    }

    pub async fn len(&self) -> usize {
        self.map.lock().await.len()
    }

    pub async fn is_empty(&self) -> bool {
        self.len().await == 0
    }
}

pub fn routes() -> Router<AppState> {
    routes_with(RoomConfig::default())
}

/// The routes with a room config of their own (the tests use small windows and queues).
pub fn routes_with(cfg: RoomConfig) -> Router<AppState> {
    routes_with_rooms(Arc::new(Rooms::new(cfg)))
}

/// The routes over rooms the caller holds, so that a test (or a future MCP entry point in the same process) can
/// reach a room without a socket.
pub fn routes_with_rooms(rooms: Arc<Rooms>) -> Router<AppState> {
    Router::new()
        .route("/v1/live/{project}", get(connect))
        .route("/v1/live/{project}/agent-changes", post(agent_changes))
        .layer(Extension(rooms))
}

#[derive(Deserialize)]
struct ConnectQuery {
    since_seq: Option<u64>,
    actor: Option<String>,
}

fn store_error(e: dibibest_store::StoreError) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({ "error": e.to_string() })),
    )
        .into_response()
}

async fn connect(
    ws: WebSocketUpgrade,
    Path(project): Path<String>,
    Query(q): Query<ConnectQuery>,
    State(state): State<AppState>,
    Extension(rooms): Extension<Arc<Rooms>>,
) -> Response {
    let room = match rooms.get(&state.store, &project).await {
        Ok(r) => r,
        Err(e) => return store_error(e),
    };
    let actor = Actor::human(q.actor.unwrap_or_else(|| "anon".into()));
    ws.max_message_size(1 << 20)
        .on_upgrade(move |socket| session(socket, room, actor, q.since_seq))
}

/// One connection: a writer task drains the queue the room fills, this task reads frames and hands them to the
/// room. Neither waits for the other.
async fn session(socket: WebSocket, room: RoomHandle, actor: Actor, since: Option<u64>) {
    let (mut sink, mut stream) = socket.split();
    let queue = room.new_queue();
    let Some(info) = room.join(&actor.name, since, queue.clone()).await else {
        return;
    };
    let conn = info.conn;
    let writer = {
        let (queue, room) = (queue.clone(), room.clone());
        tokio::spawn(async move {
            loop {
                match queue.next().await {
                    Next::Frame(f) => {
                        if sink
                            .send(Message::Text(f.to_string().into()))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Next::NeedResync => room.resync(conn).await,
                    Next::Closed => break,
                }
            }
            let _ = sink.close().await;
        })
    };
    while let Some(Ok(msg)) = stream.next().await {
        let Message::Text(t) = msg else {
            if matches!(msg, Message::Close(_)) {
                break;
            }
            continue;
        };
        match serde_json::from_str::<ClientFrame>(&t) {
            Ok(ClientFrame::Submit {
                key,
                base_seq,
                changes,
            }) => {
                room.submit_from(conn, actor.clone(), key, base_seq, changes)
                    .await
            }
            Ok(ClientFrame::Presence { cursor, selection }) => {
                room.presence(conn, cursor, selection).await
            }
            Err(e) => {
                // Straight into the queue: not worth a trip to the room.
                let f = ServerFrame::Rejected {
                    key: None,
                    code: RejectCode::BadFrame,
                    message: e.to_string(),
                    seq: 0,
                };
                queue.push_control_public(Arc::from(serde_json::to_string(&f).unwrap_or_default()));
            }
        }
    }
    room.leave(conn).await;
    writer.abort();
}

#[derive(Deserialize)]
struct AgentChanges {
    actor: String,
    key: String,
    base_seq: u64,
    changes: Vec<Change>,
}

/// The entry point that is not a socket: what an MCP tool or a REST client would call. The actor is a body
/// field in the spike; the real build takes it from the grant.
async fn agent_changes(
    Path(project): Path<String>,
    State(state): State<AppState>,
    Extension(rooms): Extension<Arc<Rooms>>,
    Json(body): Json<AgentChanges>,
) -> Response {
    let room = match rooms.get(&state.store, &project).await {
        Ok(r) => r,
        Err(e) => return store_error(e),
    };
    match room
        .submit(
            Actor::agent(body.actor),
            body.key,
            body.base_seq,
            body.changes,
        )
        .await
    {
        Ok(a) => {
            Json(serde_json::json!({ "seq": a.seq, "duplicate": a.duplicate })).into_response()
        }
        Err(r) => {
            let status = match r.code {
                RejectCode::Refused => StatusCode::UNPROCESSABLE_ENTITY,
                RejectCode::TooOld | RejectCode::BaseInFuture | RejectCode::KeyConflict => {
                    StatusCode::CONFLICT
                }
                RejectCode::Busy => StatusCode::SERVICE_UNAVAILABLE,
                RejectCode::BadFrame | RejectCode::BadChange => StatusCode::BAD_REQUEST,
                RejectCode::Internal => StatusCode::INTERNAL_SERVER_ERROR,
            };
            (
                status,
                Json(serde_json::json!({ "code": r.code, "message": r.message, "seq": r.seq })),
            )
                .into_response()
        }
    }
}
