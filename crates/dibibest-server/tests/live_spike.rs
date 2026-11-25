//! Spike for live editing (issue #88, DIB-10): real WebSocket clients against the room actor, the log in a real
//! SQLite file. See `docs/spikes/live-editing.md` for what each test shows.
//!
//! The test client below is the browser's job in miniature: it keeps the text it shows, the version it is synced
//! to and the changes the server has not confirmed, and it rebases the pending ones over what comes in, as
//! `@codemirror/collab` does. It checks the stream as it reads it: every durable frame must be the next number.
//!
//! No test sleeps a fixed time and asserts: they poll against `CEILING`.

use dibibest_server::app::{router, AppState};
use dibibest_server::config::Config;
use dibibest_server::live::frames::{ClientFrame, RejectCode, ServerFrame};
use dibibest_server::live::gate::Actor;
use dibibest_server::live::room::RoomConfig;
use dibibest_server::live::text::{from_utf16, to_utf16, transform, Change, Op};
use dibibest_server::live::{self, Rooms};
use dibibest_server::serve_until;
use dibibest_store::{SqliteStore, Store};
use futures_util::{SinkExt, StreamExt};
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

const CEILING: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------------------------------------
// A server on a real socket
// ---------------------------------------------------------------------------------------------------------

struct Server {
    addr: SocketAddr,
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<Result<(), dibibest_server::ServerError>>,
    store: Arc<SqliteStore>,
    rooms: Arc<Rooms>,
}

impl Server {
    /// Serves only the live routes (with `cfg`), through the real accept loop.
    async fn start(dir: &Path, cfg: RoomConfig) -> Server {
        let rooms = Arc::new(Rooms::new(cfg));
        let r = rooms.clone();
        Self::start_with(
            dir,
            rooms,
            |_| {},
            move |state| {
                axum::Router::new()
                    .merge(live::routes_with_rooms(r))
                    .with_state(state)
            },
        )
        .await
    }

    /// Serves the whole router: every middleware in front of the live routes.
    async fn start_full(dir: &Path) -> Server {
        // The default routes inside `router` hold rooms of their own: this one is reached over sockets only.
        Self::start_with(
            dir,
            Arc::new(Rooms::new(RoomConfig::default())),
            |c| c.request_timeout = Duration::from_millis(400),
            router,
        )
        .await
    }

    async fn start_with(
        dir: &Path,
        rooms: Arc<Rooms>,
        tweak: impl FnOnce(&mut Config),
        build: impl FnOnce(AppState) -> axum::Router,
    ) -> Server {
        let mut config = Config {
            addr: "127.0.0.1:0".parse().unwrap(),
            data_dir: dir.join("data"),
            web_dir: dir.join("web"),
            ..Config::default()
        };
        tweak(&mut config);
        std::fs::create_dir_all(&config.data_dir).unwrap();
        std::fs::create_dir_all(&config.web_dir).unwrap();
        let store = Arc::new(SqliteStore::open(config.data_dir.join("dibibest.db")).unwrap());
        let state = AppState::new(store.clone(), &config);
        let listener = TcpListener::bind(config.addr).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (stop, stopped) = oneshot::channel::<()>();
        let task = tokio::spawn(serve_until(
            listener,
            build(state),
            config.header_timeout,
            config.max_connections,
            Duration::from_millis(300),
            async move {
                let _ = stopped.await;
            },
        ));
        Server {
            addr,
            stop: Some(stop),
            task,
            store,
            rooms,
        }
    }

    async fn room(&self, project: &str) -> dibibest_server::live::room::RoomHandle {
        self.rooms.get(&self.store, project).await.unwrap()
    }

    async fn stop(mut self) {
        let _ = self.stop.take().unwrap().send(());
        timeout(CEILING, self.task).await.unwrap().unwrap().unwrap();
    }
}

fn small() -> RoomConfig {
    RoomConfig::default()
}

// ---------------------------------------------------------------------------------------------------------
// A client that behaves like the browser's collab extension
// ---------------------------------------------------------------------------------------------------------

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

struct Client {
    name: String,
    ws: Option<Ws>,
    /// The text on screen: the synced text with the pending changes on top.
    doc: Vec<u16>,
    /// The text at `version`, as far as this client has confirmed.
    synced: Vec<u16>,
    version: u64,
    /// Changes not confirmed yet, each on top of the previous one. Only the first is ever in flight.
    pending: VecDeque<(String, Op)>,
    inflight: bool,
    counter: u64,
    resyncs: u32,
    dropped_on_resync: u32,
    rejected: Vec<(Option<String>, RejectCode, String)>,
    presence: Vec<ServerFrame>,
    got_hello: bool,
    /// When each key was sent, and how long its ack took.
    sent_at: Option<(String, Instant)>,
    latencies: Vec<Duration>,
    /// Pending changes transformed over a remote one.
    rebased: u64,
}

impl Client {
    async fn connect(addr: SocketAddr, project: &str, name: &str, since: Option<u64>) -> Client {
        let mut url = format!("ws://{addr}/v1/live/{project}?actor={name}");
        if let Some(s) = since {
            url.push_str(&format!("&since_seq={s}"));
        }
        let (ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();
        Client {
            name: name.to_string(),
            ws: Some(ws),
            doc: Vec::new(),
            synced: Vec::new(),
            version: 0,
            pending: VecDeque::new(),
            inflight: false,
            counter: 0,
            resyncs: 0,
            dropped_on_resync: 0,
            rejected: Vec::new(),
            presence: Vec::new(),
            got_hello: false,
            sent_at: None,
            latencies: Vec::new(),
            rebased: 0,
        }
    }

    /// Drops the connection without closing it politely, as a lost network does.
    fn disconnect(&mut self) {
        self.ws = None;
        self.inflight = false;
    }

    /// Reconnects with the version this client has, keeping what it knows (a browser keeps its editor state).
    /// Whatever was in flight is simply sent again once the stream has caught up.
    async fn reconnect(&mut self, addr: SocketAddr, project: &str) {
        let since = self.version;
        let fresh = Client::connect(addr, project, &self.name.clone(), Some(since)).await;
        self.ws = fresh.ws;
        self.got_hello = false;
    }

    fn text(&self) -> String {
        from_utf16(&self.doc)
    }

    /// A local edit: applied to the screen at once, queued to be sent. Returns its key.
    fn edit(&mut self, changes: &[Change]) -> String {
        let op = Op::from_changes(changes, self.doc.len()).unwrap();
        self.doc = op.apply(&self.doc).unwrap();
        self.counter += 1;
        let key = format!("{}-{}", self.name, self.counter);
        self.pending.push_back((key.clone(), op));
        key
    }

    fn insert(&mut self, at: usize, s: &str) -> String {
        self.edit(&[(at as u32, at as u32, s.to_string())])
    }

    /// Sends the first pending change if none is in flight, on the version this client is synced to.
    async fn flush(&mut self) {
        if self.inflight {
            return;
        }
        let Some((key, op)) = self.pending.front() else {
            return;
        };
        let frame = ClientFrame::Submit {
            key: key.clone(),
            base_seq: self.version,
            changes: op.to_changes(),
        };
        self.sent_at = Some((key.clone(), Instant::now()));
        self.inflight = true;
        self.send(&frame).await;
    }

    async fn send(&mut self, f: &ClientFrame) {
        self.raw(serde_json::to_string(f).unwrap()).await;
    }

    async fn raw(&mut self, text: String) {
        // A send on a socket the other side has closed is not the test's business.
        let _ = self
            .ws
            .as_mut()
            .expect("connected")
            .send(Message::text(text))
            .await;
    }

    /// Reads one frame if one arrives within `wait`. Returns whether it did.
    async fn step(&mut self, wait: Duration) -> bool {
        match timeout(wait, self.ws.as_mut().expect("connected").next()).await {
            Ok(Some(Ok(Message::Text(t)))) => {
                let f: ServerFrame = serde_json::from_str(&t).expect("a frame the client knows");
                self.apply(f);
                true
            }
            Ok(Some(Ok(_))) => true,
            Ok(Some(Err(_))) | Ok(None) => false,
            Err(_) => false,
        }
    }

    /// Reads whatever is already there.
    async fn drain(&mut self) {
        while self.step(Duration::from_millis(1)).await {}
    }

    fn confirm_first(&mut self, seq: u64) {
        let (_, op) = self.pending.pop_front().expect("something to confirm");
        // The pending change has been rebased over everything the server put before it, so it is the change the
        // server applied: the synced text moves by it.
        self.synced = op.apply(&self.synced).unwrap();
        self.version = seq;
        self.inflight = false;
        if let Some((_, t)) = self.sent_at.take() {
            self.latencies.push(t.elapsed());
        }
    }

    fn apply(&mut self, f: ServerFrame) {
        match f {
            ServerFrame::Hello { seq, text, .. } => {
                self.got_hello = true;
                // `None`: continue from where we are, the missed ops follow.
                if let Some(t) = text {
                    self.doc = to_utf16(&t);
                    self.synced = self.doc.clone();
                    self.version = seq;
                }
            }
            ServerFrame::Op {
                seq, key, changes, ..
            } => {
                assert_eq!(
                    seq,
                    self.version + 1,
                    "{}: durable frames must come in order, with no gap and no repeat",
                    self.name
                );
                if self.pending.front().is_some_and(|(k, _)| *k == key) {
                    // Our own change, seen again (a replay after a reconnect): it is the confirmation.
                    self.confirm_first(seq);
                    return;
                }
                let mut r = Op::from_changes(&changes, self.synced.len()).unwrap();
                self.synced = r.apply(&self.synced).unwrap();
                for (_, p) in self.pending.iter_mut() {
                    self.rebased += 1;
                    // The server's change goes first: that is the order it was accepted in.
                    let (r2, p2) = transform(&r, p).unwrap();
                    r = r2;
                    *p = p2;
                }
                self.doc = r.apply(&self.doc).unwrap();
                self.version = seq;
            }
            ServerFrame::Ack { key, seq } => {
                if self.pending.front().is_some_and(|(k, _)| *k == key) {
                    assert_eq!(seq, self.version + 1, "{}: ack out of order", self.name);
                    self.confirm_first(seq);
                } // else: a repeated ack for a change we already saw confirmed
            }
            ServerFrame::Rejected {
                key, code, message, ..
            } => {
                self.rejected.push((key, code, message));
                if code == RejectCode::Busy {
                    self.inflight = false; // the same submit is sent again
                } else {
                    self.pending.pop_front();
                    self.inflight = false;
                }
            }
            ServerFrame::Resync { seq, text } => {
                self.resyncs += 1;
                self.dropped_on_resync += self.pending.len() as u32;
                self.pending.clear();
                self.inflight = false;
                self.doc = to_utf16(&text);
                self.synced = self.doc.clone();
                self.version = seq;
            }
            f @ (ServerFrame::Presence { .. } | ServerFrame::PresenceLeft { .. }) => {
                self.presence.push(f)
            }
        }
    }

    /// Reads until `pred` holds, sending pending changes as it goes.
    async fn until(&mut self, what: &str, mut pred: impl FnMut(&Client) -> bool) {
        let started = Instant::now();
        while !pred(self) {
            assert!(
                started.elapsed() < CEILING,
                "{}: timed out: {what}",
                self.name
            );
            self.flush().await;
            self.step(Duration::from_millis(20)).await;
        }
    }

    async fn settle(&mut self) {
        self.until("nothing pending", |c| c.pending.is_empty())
            .await;
    }
}

/// Tiny deterministic generator, so a failing run can be repeated.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// The text the log holds, rebuilt from the store by an independent path (the test's own replay).
fn text_from_log(store: &SqliteStore, project: &str) -> (u64, String) {
    let mut text: Vec<u16> = Vec::new();
    let mut seq = 0;
    loop {
        let ops = store.ops_after(project, seq, 500).unwrap();
        if ops.is_empty() {
            break;
        }
        for o in ops {
            let v: serde_json::Value = serde_json::from_str(&o.payload).unwrap();
            let changes: Vec<Change> = serde_json::from_value(v["changes"].clone()).unwrap();
            text = Op::from_changes(&changes, text.len())
                .unwrap()
                .apply(&text)
                .unwrap();
            assert_eq!(o.seq, seq + 1, "the log has no gaps");
            seq = o.seq;
        }
    }
    (seq, from_utf16(&text))
}

async fn observer(addr: SocketAddr, project: &str) -> Client {
    let mut c = Client::connect(addr, project, "observer", None).await;
    c.until("hello", |c| c.got_hello).await;
    c
}

async fn joined(addr: SocketAddr, project: &str, name: &str) -> Client {
    let mut c = Client::connect(addr, project, name, None).await;
    c.until("hello", |c| c.got_hello).await;
    c
}

// ---------------------------------------------------------------------------------------------------------
// 1, 2: the protocol and the order
// ---------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_change_is_acked_to_the_sender_and_broadcast_to_the_other() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(dir.path(), small()).await;
    let mut a = joined(server.addr, "p", "alice").await;
    let mut b = joined(server.addr, "p", "bob").await;
    assert_eq!((a.version, b.version), (0, 0));

    a.insert(0, "hello");
    a.settle().await;
    b.until("op", |c| c.version == 1).await;
    assert_eq!(a.version, 1);
    assert_eq!(b.text(), "hello");
    assert_eq!(a.text(), "hello");
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_change_made_on_an_old_version_is_rebased_by_the_room() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(dir.path(), small()).await;
    let mut a = joined(server.addr, "p", "alice").await;
    let mut b = joined(server.addr, "p", "bob").await;
    a.insert(0, "abcdef");
    a.settle().await;
    b.until("sync", |c| c.version == 1).await;

    // Both edit on version 1 and neither has seen the other's change when it sends.
    a.insert(3, "[A]");
    b.insert(3, "[B]");
    a.flush().await;
    b.flush().await;
    a.settle().await;
    b.settle().await;
    a.until("both", |c| c.version == 3).await;
    b.until("both", |c| c.version == 3).await;
    assert_eq!(a.text(), b.text());
    // Alice's change was accepted first, so it is first at the shared position.
    assert!(a.text() == "abc[A][B]def" || a.text() == "abc[B][A]def");
    let (seq, log) = text_from_log(&server.store, "p");
    assert_eq!((seq, log), (3, a.text()));
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_changes_and_bad_frames_are_rejected_and_the_connection_lives() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(dir.path(), small()).await;
    let mut a = joined(server.addr, "p", "alice").await;
    // Out of bounds, built by hand: the client type would refuse to build it.
    a.send(&ClientFrame::Submit {
        key: "bad".into(),
        base_seq: 0,
        changes: vec![(5, 9, "x".into())],
    })
    .await;
    a.raw("{not json".into()).await;
    a.send(&ClientFrame::Submit {
        key: "future".into(),
        base_seq: 99,
        changes: vec![],
    })
    .await;
    a.until("three rejections", |c| c.rejected.len() == 3).await;
    let codes: Vec<RejectCode> = a.rejected.iter().map(|r| r.1).collect();
    assert!(codes.contains(&RejectCode::BadChange));
    assert!(codes.contains(&RejectCode::BadFrame));
    assert!(codes.contains(&RejectCode::BaseInFuture));
    // Still working.
    a.insert(0, "ok");
    a.settle().await;
    assert_eq!(a.version, 1);
    server.stop().await;
}

// ---------------------------------------------------------------------------------------------------------
// 3: two clients, hundreds of interleaved changes, nothing lost
// ---------------------------------------------------------------------------------------------------------

/// Types `n` markers at random places, sometimes catching up in between and sometimes not, so that the pending
/// list grows and the rebase over remote changes runs with several changes pending.
async fn type_markers(mut c: Client, n: usize, seed: u64, deletes: bool) -> Client {
    let mut rng = Rng(seed);
    for i in 0..n {
        // Only between markers: an insert in the middle of another marker would split it, which is the text
        // doing what text does, and would make "every marker is there" untestable by counting.
        let bounds: Vec<usize> = std::iter::once(0)
            .chain(
                c.doc
                    .iter()
                    .enumerate()
                    .filter(|(_, u)| **u == 0x27E7)
                    .map(|(i, _)| i + 1),
            )
            .collect();
        let at = bounds[rng.below(bounds.len())];
        if deletes && c.doc.len() > 12 && rng.below(3) == 0 {
            let from = rng.below(c.doc.len() - 4);
            let to = from + 1 + rng.below(4);
            c.edit(&[(from as u32, to as u32, String::new())]);
        } else {
            c.insert(at, &format!("⟦{}:{i}⟧", c.name));
        }
        c.flush().await;
        match rng.below(3) {
            0 => {} // keep typing, do not look at the socket
            1 => c.drain().await,
            _ => {
                c.step(Duration::from_millis(2)).await;
            }
        }
        if rng.below(40) == 0 {
            tokio::task::yield_now().await;
        }
    }
    c
}

#[tokio::test(flavor = "multi_thread")]
async fn two_clients_type_concurrently_and_every_marker_is_there_once() {
    const N: usize = 300;
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(dir.path(), small()).await;
    let a = joined(server.addr, "p", "alice").await;
    let b = joined(server.addr, "p", "bob").await;
    let (a, b) = tokio::join!(
        tokio::spawn(type_markers(a, N, 0x9E3779B97F4A7C15, false)),
        tokio::spawn(type_markers(b, N, 0xD1B54A32D192ED03, false)),
    );
    let (mut a, mut b) = (a.unwrap(), b.unwrap());
    let total = (2 * N) as u64;
    let (ra, rb) = tokio::join!(
        async {
            a.until("all in", |c| c.pending.is_empty() && c.version == total)
                .await;
            a
        },
        async {
            b.until("all in", |c| c.pending.is_empty() && c.version == total)
                .await;
            b
        },
    );
    let (a, b) = (ra, rb);

    let obs = observer(server.addr, "p").await;
    let (seq, log) = text_from_log(&server.store, "p");
    assert_eq!(seq, total, "one number per change, none lost, none doubled");
    assert_eq!(a.text(), log, "alice shows what the log holds");
    assert_eq!(b.text(), log, "bob shows what the log holds");
    assert_eq!(obs.text(), log, "a fresh client is given the same text");
    assert_eq!(
        a.synced, a.doc,
        "no pending change is left on alice's screen"
    );
    for who in ["alice", "bob"] {
        for i in 0..N {
            let m = format!("⟦{who}:{i}⟧");
            assert_eq!(
                log.matches(&m).count(),
                1,
                "{m} is in the text exactly once"
            );
        }
    }
    let stats = server.room("p").await.stats().await.unwrap();
    assert_eq!(stats.accepted, total);
    assert!(
        stats.rebased > 0 && a.rebased + b.rebased > 0,
        "the run must really have been concurrent: {stats:?}"
    );
    println!(
        "two_clients: room {stats:?}, client-side transforms {}",
        a.rebased + b.rebased
    );
    println!(
        "two_clients: {} changes in log, text {} units, alice dropped on resync {}, bob {}",
        seq,
        log.encode_utf16().count(),
        a.dropped_on_resync,
        b.dropped_on_resync
    );
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_inserts_and_deletes_converge() {
    const N: usize = 250;
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(dir.path(), small()).await;
    let a = joined(server.addr, "p", "alice").await;
    let b = joined(server.addr, "p", "bob").await;
    let (a, b) = tokio::join!(
        tokio::spawn(type_markers(a, N, 7, true)),
        tokio::spawn(type_markers(b, N, 11, true)),
    );
    let (mut a, mut b) = (a.unwrap(), b.unwrap());
    let total = (2 * N) as u64;
    a.until("a in", |c| c.pending.is_empty()).await;
    b.until("b in", |c| c.pending.is_empty()).await;
    a.until("a caught up", |c| c.version == total).await;
    b.until("b caught up", |c| c.version == total).await;
    let (seq, log) = text_from_log(&server.store, "p");
    assert_eq!(seq, total);
    assert_eq!(a.text(), log);
    assert_eq!(b.text(), log);
    server.stop().await;
}

// ---------------------------------------------------------------------------------------------------------
// 4: reconnect, replay, retry, resync
// ---------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_client_that_reconnects_gets_the_missed_ops_in_order_once() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(dir.path(), small()).await;
    let mut a = joined(server.addr, "p", "alice").await;
    let mut b = joined(server.addr, "p", "bob").await;
    for i in 0..10 {
        a.insert(a.doc.len(), &format!("a{i};"));
        a.settle().await;
    }
    b.until("caught up", |c| c.version == 10).await;

    // Bob goes away; Alice keeps typing.
    b.disconnect();
    for i in 10..40 {
        a.insert(a.doc.len(), &format!("a{i};"));
        a.settle().await;
    }

    // Bob comes back with his version. `apply` asserts that each op is the next number: no gap, no repeat.
    b.reconnect(server.addr, "p").await;
    b.until("replayed", |c| c.version == 40).await;
    assert_eq!(b.text(), a.text());
    assert_eq!(b.resyncs, 0, "close enough to replay: no resync");
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unacked_submit_is_safe_to_send_again() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(dir.path(), small()).await;
    let mut a = joined(server.addr, "p", "alice").await;
    let mut b = joined(server.addr, "p", "bob").await;
    a.insert(0, "base;");
    a.settle().await;
    b.until("sync", |c| c.version == 1).await;

    // Case 1: the change reached the room, the ack never reached Bob. Alice sees it, so it is certainly in.
    let key = b.insert(b.doc.len(), "[bob-1]");
    b.flush().await;
    a.until("sees bob's change", |c| c.version == 2).await;
    b.disconnect();
    b.reconnect(server.addr, "p").await;
    // The replay carries his own key: it confirms the pending change, and nothing is sent twice.
    b.until("replayed", |c| c.version == 2).await;
    assert!(b.pending.is_empty(), "the replayed op confirmed {key}");

    // Case 2: the client does not wait for the replay and sends the same key again (a retry loop). The room
    // remembers the key, answers with the old number, and writes nothing.
    let key = b.insert(b.doc.len(), "[bob-2]");
    b.flush().await;
    b.settle().await;
    assert_eq!(b.version, 3);
    let before = server.store.latest_seq("p").unwrap();
    let base = b.version - 1; // the version the change was made on
    b.send(&ClientFrame::Submit {
        key: key.clone(),
        base_seq: base,
        changes: vec![(
            (b.doc.len() - 7) as u32,
            (b.doc.len() - 7) as u32,
            "[bob-2]".into(),
        )],
    })
    .await;
    b.drain().await;
    assert_eq!(
        server.store.latest_seq("p").unwrap(),
        before,
        "nothing was written again"
    );
    a.until("alice caught up", |c| c.version == 3).await;
    assert_eq!(a.text().matches("[bob-2]").count(), 1);
    assert_eq!(a.text().matches("[bob-1]").count(), 1);

    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_too_far_behind_gets_a_resync_with_the_whole_text() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = RoomConfig {
        window: 8,
        queue_frames: 64,
        ..RoomConfig::default()
    };
    let server = Server::start(dir.path(), cfg).await;
    let mut a = joined(server.addr, "p", "alice").await;
    let mut b = joined(server.addr, "p", "bob").await;
    a.insert(0, "start;");
    a.settle().await;
    b.until("sync", |c| c.version == 1).await;
    b.disconnect();
    for i in 0..30 {
        a.insert(a.doc.len(), &format!("a{i};"));
        a.settle().await;
    }
    // 29 ops missed, the window holds 8.
    b.reconnect(server.addr, "p").await;
    b.until("resync", |c| c.resyncs == 1 && c.version == 31)
        .await;
    assert_eq!(b.text(), a.text());

    // A client that claims a version the room never had gets the same.
    let mut c = Client::connect(server.addr, "p", "carol", Some(9999)).await;
    c.until("resync", |c| c.resyncs == 1).await;
    assert_eq!(c.text(), a.text());

    // A submit made on a version outside the window is refused, and a resync follows on that connection.
    let mut d = joined(server.addr, "p", "dave").await;
    d.version = 3;
    d.synced = to_utf16("x");
    d.doc = d.synced.clone();
    d.send(&ClientFrame::Submit {
        key: "old".into(),
        base_seq: 3,
        changes: vec![(0, 0, "!".into())],
    })
    .await;
    d.until("too old and resync", |c| {
        c.resyncs == 1 && !c.rejected.is_empty()
    })
    .await;
    assert_eq!(d.rejected[0].1, RejectCode::TooOld);
    assert_eq!(d.text(), a.text());
    server.stop().await;
}

// ---------------------------------------------------------------------------------------------------------
// 5: a client that does not read
// ---------------------------------------------------------------------------------------------------------

fn percentile(v: &[Duration], p: f64) -> Duration {
    let mut v = v.to_vec();
    v.sort();
    v[((v.len() as f64 - 1.0) * p).round() as usize]
}

/// Sends `n` changes that each replace the whole text with `size` units, one after the other, and returns the
/// time each took from the send to its ack. Big frames, so that a client that does not read fills its socket.
async fn big_replacements(c: &mut Client, n: usize, size: usize) -> Vec<Duration> {
    c.latencies.clear();
    for i in 0..n {
        let body = format!("{i:06}").repeat(size / 6);
        let len = c.doc.len() as u32;
        c.edit(&[(0, len, body)]);
        c.settle().await;
    }
    c.latencies.clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_that_does_not_read_gets_a_resync_and_slows_nobody() {
    const N: usize = 120;
    const SIZE: usize = 240_000;
    let dir = tempfile::tempdir().unwrap();
    let cfg = RoomConfig {
        queue_frames: 64,
        queue_bytes: 1 << 20,
        ..RoomConfig::default()
    };
    let server = Server::start(dir.path(), cfg).await;

    // The same load twice, in two projects: once with a reader that is stuck, once without.
    let mut results = Vec::new();
    for (project, with_stuck) in [("quiet", false), ("stuck", true)] {
        let mut writer = joined(server.addr, project, "writer").await;
        let mut stuck = if with_stuck {
            Some(joined(server.addr, project, "stuck").await)
        } else {
            None
        };
        // A second reader that does read, in its own task.
        let mut reader = joined(server.addr, project, "reader").await;
        let total = N as u64;
        let reader_task = tokio::spawn(async move {
            reader
                .until("reader sees all", |c| c.version == total)
                .await;
            reader
        });
        let lat = big_replacements(&mut writer, N, SIZE).await;
        let reader = reader_task.await.unwrap();
        assert_eq!(reader.text(), writer.text());
        assert_eq!(
            reader.resyncs, 0,
            "a client that reads is never given a resync"
        );
        println!(
            "slow_client[{project}]: writer ack latency p50 {:?} p99 {:?} max {:?} ({} changes of {} KB)",
            percentile(&lat, 0.5),
            percentile(&lat, 0.99),
            lat.iter().max().unwrap(),
            N,
            SIZE * 2 / 1000
        );
        results.push(percentile(&lat, 0.5));

        if let Some(s) = stuck.as_mut() {
            // The room did not wait for it: everything is in the log. Now it reads again.
            assert_eq!(server.store.latest_seq(project).unwrap(), total);
            s.until("catches up", |c| c.version == total && c.resyncs >= 1)
                .await;
            assert!(
                s.resyncs >= 1,
                "its queue overflowed: it must have been told"
            );
            assert_eq!(
                s.text(),
                writer.text(),
                "after the resync it shows the same text"
            );
            println!(
                "slow_client[stuck]: the stuck client got {} resync(s)",
                s.resyncs
            );
        }
    }
    // Generous: the point is that it is the same order of magnitude, not a benchmark.
    assert!(
        results[1] < results[0] * 20 + Duration::from_millis(200),
        "a stuck reader changed the writer's latency: {results:?}"
    );
    server.stop().await;
}

// ---------------------------------------------------------------------------------------------------------
// 6: presence is ephemeral
// ---------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn presence_is_live_only_never_stored_never_replayed() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(dir.path(), small()).await;
    let mut a = joined(server.addr, "p", "alice").await;
    let mut b = joined(server.addr, "p", "bob").await;
    a.insert(0, "text");
    a.settle().await;
    b.until("sync", |c| c.version == 1).await;

    a.send(&ClientFrame::Presence {
        cursor: Some(2),
        selection: Some((1, 3)),
    })
    .await;
    b.until("presence", |c| !c.presence.is_empty()).await;
    match &b.presence[0] {
        ServerFrame::Presence {
            actor,
            cursor,
            selection,
            ..
        } => {
            assert_eq!(actor, "alice");
            assert_eq!((*cursor, *selection), (Some(2), Some((1, 3))));
        }
        other => panic!("{other:?}"),
    }
    assert!(
        a.presence.is_empty(),
        "a client does not get its own presence"
    );
    assert_eq!(
        server.store.latest_seq("p").unwrap(),
        1,
        "presence takes no number"
    );
    assert_eq!(server.store.ops_after("p", 0, 100).unwrap().len(), 1);

    // Bob reconnects: the replay has the change and no presence.
    b.disconnect();
    b.presence.clear();
    b.reconnect(server.addr, "p").await;
    b.until("hello", |c| c.got_hello).await;
    for _ in 0..20 {
        b.step(Duration::from_millis(5)).await;
    }
    assert!(b.presence.is_empty(), "presence is not replayed");

    // Alice leaves: Bob is told.
    a.disconnect();
    b.until("left", |c| {
        c.presence
            .iter()
            .any(|f| matches!(f, ServerFrame::PresenceLeft { actor, .. } if actor == "alice"))
    })
    .await;
    server.stop().await;
}

// ---------------------------------------------------------------------------------------------------------
// 7: the gate, one function for the socket and for the agent
// ---------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn the_gate_refuses_an_agent_change_that_does_not_parse_and_lets_a_person_edit_mid_sentence()
{
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(dir.path(), small()).await;
    let mut human = joined(server.addr, "p", "alice").await;
    let room = server.room("p").await;
    let valid = "Table users {\n  id integer\n}\n";
    human.insert(0, valid);
    human.settle().await;

    // An agent leaves a bracket open. Refused, and the text and the number are as they were.
    let at = valid.find("integer").unwrap() as u32 + 7;
    let breaks = vec![(at, at, " [pk".to_string())];
    let err = room
        .submit(Actor::agent("claude"), "agent-1".into(), 1, breaks.clone())
        .await
        .unwrap_err();
    assert_eq!(err.code, RejectCode::Refused);
    println!("gate refusal message: {}", err.message);
    assert!(
        err.message.contains("DBML"),
        "names what failed: {}",
        err.message
    );
    assert_eq!(room.text().await.unwrap(), (1, valid.to_string()));
    assert_eq!(server.store.latest_seq("p").unwrap(), 1);

    // The same change from a person over the socket is accepted: the editor must keep working mid-edit.
    human.edit(&[(at, at, " [pk".to_string())]);
    human.settle().await;
    assert_eq!(human.version, 2);
    assert!(human.rejected.is_empty());
    assert!(room.text().await.unwrap().1.contains("[pk"));

    // The gate looks at the result, not at the agent's intent: a change that mends the text is let in.
    let at = (valid.find("integer").unwrap() + 7 + 4) as u32;
    let mends = vec![(at, at, "]".to_string())];
    let ok = room
        .submit(Actor::agent("claude"), "agent-2".into(), 2, mends)
        .await
        .unwrap();
    assert_eq!(ok.seq, 3);
    human.until("sees the agent", |c| c.version == 3).await;
    assert_eq!(human.text(), "Table users {\n  id integer [pk]\n}\n");
    // The log names who did it.
    let ops = server.store.ops_after("p", 0, 10).unwrap();
    assert_eq!(
        ops.iter().map(|o| o.actor.as_str()).collect::<Vec<_>>(),
        ["alice", "alice", "claude"]
    );

    // The size limit is part of the same gate and binds a person too.
    human.insert(0, &"x".repeat(600_000));
    human.settle().await;
    human.insert(0, &"y".repeat(600_000));
    human.until("refused", |c| !c.rejected.is_empty()).await;
    assert_eq!(human.rejected[0].1, RejectCode::Refused);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_rest_like_route_goes_through_the_same_gate() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(dir.path(), small()).await;
    let mut human = joined(server.addr, "p", "alice").await;
    let http = reqwest::Client::new();
    let url = format!("http://{}/v1/live/p/agent-changes", server.addr);

    let post = |key: &str, base: u64, changes: serde_json::Value| {
        http.post(&url)
            .header("content-type", "application/json")
            .body(
                serde_json::json!({
                    "actor": "claude", "key": key, "base_seq": base, "changes": changes
                })
                .to_string(),
            )
    };
    async fn body(r: reqwest::Response) -> serde_json::Value {
        serde_json::from_str(&r.text().await.unwrap()).unwrap()
    }
    let r = post(
        "k1",
        0,
        serde_json::json!([[0, 0, "Table t {\n  id int\n}\n"]]),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(body(r).await["seq"], 1);
    // Sent again: the same number, written once.
    let r = post(
        "k1",
        0,
        serde_json::json!([[0, 0, "Table t {\n  id int\n}\n"]]),
    )
    .send()
    .await
    .unwrap();
    let v = body(r).await;
    assert_eq!(
        (v["seq"].clone(), v["duplicate"].clone()),
        (1.into(), true.into())
    );
    // A change that leaves broken text: 422 with the reason.
    let at = "Table t {\n  id int".len();
    let r = post("k2", 1, serde_json::json!([[at, at, " [pk"]]))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 422);
    let v = body(r).await;
    assert_eq!(v["code"], "refused");
    assert!(v["message"].as_str().unwrap().contains("DBML"));
    // The person on the socket saw the first change live, with the agent named.
    human.until("agent op", |c| c.version == 1).await;
    assert!(human.text().starts_with("Table t"));
    server.stop().await;
}

// ---------------------------------------------------------------------------------------------------------
// 8: restart
// ---------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_restart_serves_the_same_text_and_the_same_numbers() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(dir.path(), small()).await;
    let mut a = joined(server.addr, "p", "alice").await;
    let mut b = joined(server.addr, "p", "bob").await;
    for i in 0..12 {
        a.insert(a.doc.len(), &format!("{i};"));
        a.settle().await;
    }
    b.until("sync", |c| c.version == 12).await;
    b.disconnect();
    for i in 12..20 {
        b.version = 12; // bob's own state does not move while he is away
        a.insert(a.doc.len(), &format!("{i};"));
        a.settle().await;
    }
    let (text, seq) = (a.text(), a.version);
    assert_eq!(seq, 20);
    a.disconnect();
    server.stop().await;

    // Same directory, new process state: new rooms, same database.
    let server = Server::start(dir.path(), small()).await;
    let fresh = joined(server.addr, "p", "carol").await;
    assert_eq!((fresh.version, fresh.text()), (seq, text.clone()));
    // Bob's replay works from the rebuilt window.
    b.reconnect(server.addr, "p").await;
    b.until("replayed", |c| c.version == 20).await;
    assert_eq!(b.text(), text);
    assert_eq!(b.resyncs, 0);
    // And a new change is number 21 on top of the old log.
    let mut c = fresh;
    c.insert(c.doc.len(), "after;");
    c.settle().await;
    assert_eq!(c.version, 21);
    assert_eq!(text_from_log(&server.store, "p").0, 21);
    server.stop().await;
}

// ---------------------------------------------------------------------------------------------------------
// The whole router in front of it
// ---------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn the_socket_works_behind_the_real_router_and_outlives_the_request_timeout() {
    let dir = tempfile::tempdir().unwrap();
    // The request timeout is 400 ms here: it bounds the time to the 101, not the life of the socket.
    let server = Server::start_full(dir.path()).await;
    let mut a = joined(server.addr, "p", "alice").await;
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(900) {
        a.step(Duration::from_millis(50)).await;
    }
    a.insert(0, "still here");
    a.settle().await;
    assert_eq!(a.version, 1);
    let health = reqwest::get(format!("http://{}/healthz", server.addr))
        .await
        .unwrap();
    assert_eq!(health.status(), 200);
    a.disconnect();
    server.stop().await;
}

// ---------------------------------------------------------------------------------------------------------
// Measurements (run with `cargo test -p dibibest-server --test live_spike -- --ignored --nocapture`)
// ---------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "a measurement, not a check"]
async fn measure_throughput_of_one_room() {
    for clients in [1usize, 4, 16] {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(dir.path(), small()).await;
        let per = 400 / clients.min(4) * 2;
        let mut tasks = Vec::new();
        let started = Instant::now();
        for i in 0..clients {
            let mut c = joined(server.addr, "bench", &format!("c{i}")).await;
            tasks.push(tokio::spawn(async move {
                for n in 0..per {
                    let at = c.doc.len();
                    c.insert(at, &format!("{n};"));
                    c.settle().await;
                }
                c
            }));
        }
        let mut lat = Vec::new();
        for t in tasks {
            lat.extend(t.await.unwrap().latencies);
        }
        let secs = started.elapsed().as_secs_f64();
        let total = clients * per;
        println!(
            "throughput: {clients:>2} clients, {total} changes in {secs:.2}s = {:.0} ops/s, ack p50 {:?} p99 {:?}",
            total as f64 / secs,
            percentile(&lat, 0.5),
            percentile(&lat, 0.99)
        );
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "a measurement, not a check"]
async fn measure_memory_per_room() {
    fn rss_kb() -> u64 {
        let out = std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", &std::process::id().to_string()])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
    }
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(dir.path(), small()).await;
    // Warm up the allocator and the store.
    for i in 0..20 {
        server.room(&format!("warm{i}")).await;
    }
    let before = rss_kb();
    const ROOMS: usize = 2000;
    for i in 0..ROOMS {
        server.room(&format!("r{i}")).await;
    }
    let after = rss_kb();
    println!(
        "memory: {ROOMS} empty rooms cost {} KB of RSS = {} bytes per room",
        after - before,
        (after - before) * 1024 / ROOMS as u64
    );
    // One room with a 10 KB text and a 1000-op window.
    let h = server.room("big").await;
    let before = rss_kb();
    for i in 0..1000u64 {
        h.submit(
            Actor::human("m"),
            format!("k{i}"),
            i,
            vec![(0, 0, "0123456789".into())],
        )
        .await
        .unwrap();
    }
    println!(
        "memory: one room with {} units of text and a window of {} ops: +{} KB",
        10_000,
        1000,
        rss_kb().saturating_sub(before)
    );
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_with_an_open_socket_ends_and_tells_what_it_cost() {
    let dir = tempfile::tempdir().unwrap();
    let mut server = Server::start(dir.path(), small()).await;
    let mut a = joined(server.addr, "p", "alice").await;
    a.insert(0, "open");
    a.settle().await;
    let started = Instant::now();
    let _ = server.stop.take().unwrap().send(());
    let result = timeout(CEILING, &mut server.task).await;
    println!(
        "shutdown with one open socket: the server returned after {:?} (grace is 300 ms)",
        started.elapsed()
    );
    assert!(
        result.is_ok(),
        "the server must stop even with a socket open"
    );
    // Is the socket cut by the server going away, or does it live on as a detached task? Say which.
    let ws = a.ws.as_mut().unwrap();
    let outcome = match timeout(Duration::from_secs(2), ws.next()).await {
        Ok(None) | Ok(Some(Err(_))) | Ok(Some(Ok(Message::Close(_)))) => "closed by the server",
        Ok(Some(Ok(_))) => "a frame arrived",
        Err(_) => "still open after 2 s",
    };
    println!("shutdown with one open socket: the client's socket was {outcome}");
}
