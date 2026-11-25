//! The header-read timeout, the request timeout and shutdown, over real sockets (issue #104).
//!
//! The timeouts are a few hundred milliseconds here. Every assertion has room on both sides: a connection that
//! must be closed is closed well inside a generous ceiling, and one that must be left alone is left alone for
//! a good part of the timeout, so a slow machine does not fail the test and a missing timeout does.

use axum::body::Bytes;
use axum::extract::{ConnectInfo, Path, Request};
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;
use dibibest_server::app::{router_with, AppState};
use dibibest_server::config::Config;
use dibibest_server::conn::UpgradeSlot;
use dibibest_server::{bind, serve_until, ServeOptions, ServerError};
use dibibest_store::SqliteStore;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};

/// Longer than anything that should happen; a test that waits this long has failed.
const CEILING: Duration = Duration::from_secs(8);

/// The size of the `/big` body.
const BIG: usize = 16 * 1024 * 1024;

fn base_config(dir: &std::path::Path) -> Config {
    let web = dir.join("web");
    std::fs::create_dir_all(&web).unwrap();
    std::fs::write(web.join("index.html"), "<html>INDEX</html>").unwrap();
    Config {
        addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: dir.join("data"),
        web_dir: web,
        ..Config::default()
    }
}

/// Routes that exist only in these tests.
fn misbehaving() -> Router<AppState> {
    Router::new()
        // Never answers.
        .route("/slow", get(std::future::pending::<&'static str>))
        // Answers after `ms` milliseconds.
        .route(
            "/nap/{ms}",
            get(|Path(ms): Path<u64>| async move {
                sleep(Duration::from_millis(ms)).await;
                "done"
            }),
        )
        // Reads the whole body, so a slow body keeps the handler waiting.
        .route(
            "/echo",
            post(|body: Bytes| async move { format!("got {} bytes", body.len()) }),
        )
        // Does not read the body at all.
        .route("/ignore", post(|| async { "ignored" }))
        // A body far beyond what the kernel buffers hold.
        .route("/big", get(|| async { vec![7u8; BIG] }))
        // Upgrades the connection and echoes what it is sent; see `upgraded`.
        .route("/upgrade/{mode}", get(upgrade))
        // Tells what address the connection came from.
        .route(
            "/peer",
            get(|ConnectInfo(peer): ConnectInfo<SocketAddr>| async move { peer.to_string() }),
        )
}

struct Server {
    addr: SocketAddr,
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<Result<(), ServerError>>,
    _dir: TempDir,
}

impl Server {
    fn stop(&mut self) {
        let _ = self.stop.take().expect("not stopped yet").send(());
    }

    async fn finished(self) -> Duration {
        let started = Instant::now();
        let result = timeout(CEILING, self.task)
            .await
            .expect("the server must stop within the ceiling")
            .unwrap();
        result.unwrap();
        started.elapsed()
    }
}

/// Serves `config` with the extra routes, through the same accept loop as `Bound::run`.
async fn start(configure: impl FnOnce(&mut Config)) -> Server {
    let dir = tempfile::tempdir().unwrap();
    let mut config = base_config(dir.path());
    configure(&mut config);
    std::fs::create_dir_all(&config.data_dir).unwrap();
    let store = SqliteStore::open(config.data_dir.join("dibibest.db")).unwrap();
    let state = AppState::new(Arc::new(store), &config);
    let listener = TcpListener::bind(config.addr).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop, stopped) = oneshot::channel::<()>();
    let task = tokio::spawn(serve_until(
        listener,
        router_with(state, misbehaving()),
        ServeOptions::from_config(&config),
        async move {
            let _ = stopped.await;
        },
    ));
    Server {
        addr,
        stop: Some(stop),
        task,
        _dir: dir,
    }
}

fn short(config: &mut Config) {
    config.header_timeout = Duration::from_millis(400);
    config.request_timeout = Duration::from_millis(500);
}

/// Reads to the end of the connection, and says how long that took. A connection that is still open after the
/// ceiling is a failure.
async fn until_closed(s: &mut TcpStream) -> (String, Duration) {
    let started = Instant::now();
    let mut out = Vec::new();
    timeout(CEILING, s.read_to_end(&mut out))
        .await
        .expect("the connection must be closed")
        .ok();
    (
        String::from_utf8_lossy(&out).into_owned(),
        started.elapsed(),
    )
}

async fn http_get(addr: SocketAddr, path: &str) -> String {
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").as_bytes())
        .await
        .unwrap();
    until_closed(&mut s).await.0
}

// ── the header-read timeout ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn part_of_the_headers_and_then_silence_is_closed_within_the_header_timeout() {
    let mut server = start(|c| c.header_timeout = Duration::from_millis(400)).await;
    let mut s = TcpStream::connect(server.addr).await.unwrap();
    s.write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\n")
        .await
        .unwrap();
    let (reply, took) = until_closed(&mut s).await;
    assert!(
        reply.is_empty() || reply.starts_with("HTTP/1.1 408"),
        "only a timeout answer is acceptable: {reply}"
    );
    assert!(
        took >= Duration::from_millis(150),
        "closed after {took:?}, before the timeout could have run"
    );
    server.stop();
    server.finished().await;
}

#[tokio::test]
async fn a_connection_that_sends_nothing_is_closed_too() {
    let mut server = start(|c| c.header_timeout = Duration::from_millis(400)).await;
    let mut s = TcpStream::connect(server.addr).await.unwrap();
    let (reply, took) = until_closed(&mut s).await;
    assert!(
        reply.is_empty() || reply.starts_with("HTTP/1.1 408"),
        "{reply}"
    );
    assert!(took >= Duration::from_millis(150), "{took:?}");
    server.stop();
    server.finished().await;
}

#[tokio::test]
async fn headers_sent_a_byte_at_a_time_are_closed_even_though_every_byte_is_a_sign_of_life() {
    let mut server = start(|c| c.header_timeout = Duration::from_millis(500)).await;
    let (mut reader, mut writer) = TcpStream::connect(server.addr).await.unwrap().into_split();
    let closed = async {
        let mut sink = Vec::new();
        reader.read_to_end(&mut sink).await.ok();
    };
    // A header that never ends, one byte every 50 ms: no single pause is long, the whole is.
    let drip = async {
        let mut bytes = b"GET /healthz HTTP/1.1\r\nHost: x\r\nX-Pad: ".to_vec();
        bytes.extend(std::iter::repeat_n(b'a', 1000));
        for b in bytes {
            if writer.write_all(&[b]).await.is_err() {
                return;
            }
            sleep(Duration::from_millis(50)).await;
        }
    };
    let (closed_by_server, took) = {
        let started = Instant::now();
        tokio::select! {
            () = closed => (true, started.elapsed()),
            () = drip => (true, started.elapsed()),
            () = sleep(Duration::from_secs(4)) => (false, started.elapsed()),
        }
    };
    assert!(
        closed_by_server,
        "still open after {took:?}: a slow header holds the connection"
    );
    assert!(took < Duration::from_secs(4));
    server.stop();
    server.finished().await;
}

/// The same timer covers the wait for the next request on a kept-alive connection, so a client that answers
/// nothing after its first request cannot keep the connection either.
#[tokio::test]
async fn a_kept_alive_connection_that_stays_silent_is_closed_after_the_header_timeout() {
    let mut server = start(|c| c.header_timeout = Duration::from_millis(600)).await;
    let mut s = TcpStream::connect(server.addr).await.unwrap();
    s.write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    let (reply, took) = until_closed(&mut s).await;
    assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
    assert!(took >= Duration::from_millis(250), "closed after {took:?}");
    server.stop();
    server.finished().await;
}

#[tokio::test]
async fn headers_that_arrive_in_pieces_inside_the_timeout_are_served() {
    let mut server = start(|c| c.header_timeout = Duration::from_millis(1500)).await;
    let mut s = TcpStream::connect(server.addr).await.unwrap();
    s.write_all(b"GET /healthz HTTP/1.1\r\nHo").await.unwrap();
    sleep(Duration::from_millis(150)).await;
    s.write_all(b"st: x\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let (reply, _) = until_closed(&mut s).await;
    assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
    server.stop();
    server.finished().await;
}

// ── the request timeout ─────────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_handler_that_outlasts_the_request_timeout_answers_408_and_frees_the_connection() {
    let mut server = start(short).await;
    let mut s = TcpStream::connect(server.addr).await.unwrap();
    s.write_all(b"GET /slow HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    let (reply, took) = until_closed(&mut s).await;
    assert!(reply.starts_with("HTTP/1.1 408"), "{reply}");
    assert!(reply.contains("request_timeout"), "{reply}");
    assert!(
        reply.to_ascii_lowercase().contains("connection: close"),
        "the answer must end the connection: {reply}"
    );
    assert!(
        took >= Duration::from_millis(250),
        "answered after {took:?}, before the timeout could have run"
    );
    server.stop();
    server.finished().await;
}

#[tokio::test]
async fn a_body_that_arrives_too_slowly_answers_408_and_frees_the_connection() {
    let mut server = start(short).await;
    let mut s = TcpStream::connect(server.addr).await.unwrap();
    // The headers are complete at once; the body announces ten bytes and delivers three.
    s.write_all(b"POST /echo HTTP/1.1\r\nHost: x\r\nContent-Length: 10\r\n\r\nabc")
        .await
        .unwrap();
    let (reply, took) = until_closed(&mut s).await;
    assert!(reply.starts_with("HTTP/1.1 408"), "{reply}");
    assert!(took >= Duration::from_millis(250), "{took:?}");
    server.stop();
    server.finished().await;
}

#[tokio::test]
async fn a_body_dripped_a_byte_at_a_time_is_cut_off_too() {
    let mut server = start(short).await;
    let (mut reader, mut writer) = TcpStream::connect(server.addr).await.unwrap().into_split();
    writer
        .write_all(b"POST /echo HTTP/1.1\r\nHost: x\r\nContent-Length: 1000\r\n\r\n")
        .await
        .unwrap();
    let drip = async {
        for _ in 0..1000 {
            if writer.write_all(b"a").await.is_err() {
                return;
            }
            sleep(Duration::from_millis(40)).await;
        }
    };
    let mut reply = Vec::new();
    tokio::select! {
        _ = reader.read_to_end(&mut reply) => {}
        () = drip => {}
        () = sleep(Duration::from_secs(5)) => {}
    }
    let reply = String::from_utf8_lossy(&reply);
    assert!(reply.starts_with("HTTP/1.1 408"), "{reply}");
    server.stop();
    server.finished().await;
}

#[tokio::test]
async fn a_handler_that_never_reads_a_slow_body_does_not_hold_the_connection() {
    let mut server = start(short).await;
    let mut s = TcpStream::connect(server.addr).await.unwrap();
    s.write_all(b"POST /ignore HTTP/1.1\r\nHost: x\r\nContent-Length: 10\r\n\r\nabc")
        .await
        .unwrap();
    let (reply, _) = until_closed(&mut s).await;
    assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
    server.stop();
    server.finished().await;
}

#[tokio::test]
async fn requests_that_finish_in_time_are_not_touched() {
    let mut server = start(|c| {
        c.header_timeout = Duration::from_millis(500);
        c.request_timeout = Duration::from_millis(1500);
    })
    .await;
    let nap = http_get(server.addr, "/nap/200").await;
    assert!(
        nap.starts_with("HTTP/1.1 200") && nap.ends_with("done"),
        "{nap}"
    );
    let health = http_get(server.addr, "/healthz").await;
    assert!(health.starts_with("HTTP/1.1 200"), "{health}");
    let ready = http_get(server.addr, "/readyz").await;
    assert!(ready.starts_with("HTTP/1.1 200"), "{ready}");
    let home = http_get(server.addr, "/").await;
    assert!(home.contains("INDEX"), "{home}");
    let mut s = TcpStream::connect(server.addr).await.unwrap();
    s.write_all(
        b"POST /echo HTTP/1.1\r\nHost: x\r\nContent-Length: 3\r\nConnection: close\r\n\r\nabc",
    )
    .await
    .unwrap();
    let (echo, _) = until_closed(&mut s).await;
    assert!(
        echo.starts_with("HTTP/1.1 200") && echo.ends_with("got 3 bytes"),
        "{echo}"
    );
    server.stop();
    server.finished().await;
}

#[tokio::test]
async fn the_probes_answer_while_a_slow_request_is_still_waiting() {
    let mut server = start(|c| {
        c.request_timeout = Duration::from_secs(20);
        c.shutdown_grace = Duration::from_millis(300);
    })
    .await;
    let mut slow = TcpStream::connect(server.addr).await.unwrap();
    slow.write_all(b"GET /slow HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    sleep(Duration::from_millis(100)).await;
    for path in ["/healthz", "/readyz"] {
        let reply = http_get(server.addr, path).await;
        assert!(reply.starts_with("HTTP/1.1 200"), "{path}: {reply}");
    }
    server.stop();
    server.finished().await;
}

// ── what the server already did ─────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn the_body_limit_still_answers_413_over_a_real_socket() {
    let mut server = start(|c| c.max_body_bytes = 8).await;
    let mut s = TcpStream::connect(server.addr).await.unwrap();
    s.write_all(
        b"POST /echo HTTP/1.1\r\nHost: x\r\nContent-Length: 100\r\nConnection: close\r\n\r\n",
    )
    .await
    .unwrap();
    s.write_all(&[b'a'; 100]).await.ok();
    let (reply, _) = until_closed(&mut s).await;
    assert!(reply.starts_with("HTTP/1.1 413"), "{reply}");
    server.stop();
    server.finished().await;
}

#[tokio::test]
async fn the_rate_limit_answers_429_over_a_real_socket_and_spares_the_probes() {
    let mut server = start(|c| c.rate_per_minute = 2).await;
    for _ in 0..2 {
        assert!(http_get(server.addr, "/robots.txt")
            .await
            .starts_with("HTTP/1.1 404"));
    }
    let refused = http_get(server.addr, "/robots.txt").await;
    assert!(refused.starts_with("HTTP/1.1 429"), "{refused}");
    for _ in 0..5 {
        assert!(http_get(server.addr, "/healthz")
            .await
            .starts_with("HTTP/1.1 200"));
        assert!(http_get(server.addr, "/readyz")
            .await
            .starts_with("HTTP/1.1 200"));
    }
    server.stop();
    server.finished().await;
}

#[tokio::test]
async fn the_handler_sees_the_address_the_connection_came_from() {
    let mut server = start(|_| {}).await;
    let mut s = TcpStream::connect(server.addr).await.unwrap();
    let own = s.local_addr().unwrap();
    s.write_all(b"GET /peer HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let (reply, _) = until_closed(&mut s).await;
    assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
    assert!(reply.ends_with(&own.to_string()), "{own}: {reply}");
    server.stop();
    server.finished().await;
}

/// Two clients with different addresses must have two budgets. On this machine they differ only if the limiter
/// really is given the address of each connection: without it every client is counted as 127.0.0.1.
#[tokio::test]
async fn the_rate_limiter_is_given_each_clients_own_address() {
    if std::net::TcpListener::bind("[::1]:0").is_err() {
        eprintln!("skipped: this machine has no IPv6 loopback");
        return;
    }
    let mut server = start(|c| {
        c.addr = "[::]:0".parse().unwrap();
        c.rate_per_minute = 1;
    })
    .await;
    let port = server.addr.port();
    let (v4, v6): (SocketAddr, SocketAddr) = (
        format!("127.0.0.1:{port}").parse().unwrap(),
        format!("[::1]:{port}").parse().unwrap(),
    );
    if TcpStream::connect(v4).await.is_err() || TcpStream::connect(v6).await.is_err() {
        eprintln!("skipped: this machine has no dual-stack loopback");
        server.stop();
        server.finished().await;
        return;
    }
    assert!(http_get(v4, "/robots.txt")
        .await
        .starts_with("HTTP/1.1 404"));
    assert!(http_get(v4, "/robots.txt")
        .await
        .starts_with("HTTP/1.1 429"));
    let other = http_get(v6, "/robots.txt").await;
    assert!(
        other.starts_with("HTTP/1.1 404"),
        "a second client has its own budget: {other}"
    );
    server.stop();
    server.finished().await;
}

// ── shutdown ────────────────────────────────────────────────────────────────────────────────────────────

async fn run_bound(
    configure: impl FnOnce(&mut Config),
) -> (
    SocketAddr,
    Config,
    oneshot::Sender<()>,
    JoinHandle<Result<(), ServerError>>,
    TempDir,
) {
    let dir = tempfile::tempdir().unwrap();
    let mut config = base_config(dir.path());
    configure(&mut config);
    let server = bind(config.clone()).await.unwrap();
    let addr = server.local_addr();
    let (stop, stopped) = oneshot::channel::<()>();
    let task = tokio::spawn(server.run(async move {
        let _ = stopped.await;
    }));
    (addr, config, stop, task, dir)
}

#[tokio::test]
async fn an_idle_keep_alive_connection_does_not_hold_up_shutdown_and_the_lock_is_released() {
    // A long grace period, and a header timeout too long to be what ends the idle connection.
    let (addr, config, stop, task, _dir) = run_bound(|c| {
        c.shutdown_grace = Duration::from_secs(6);
        c.header_timeout = Duration::from_secs(60);
    })
    .await;
    let mut idle = TcpStream::connect(addr).await.unwrap();
    idle.write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    let mut seen = Vec::new();
    let mut buf = [0u8; 512];
    while !String::from_utf8_lossy(&seen).contains("\"status\":\"ok\"") {
        let n = timeout(CEILING, idle.read(&mut buf))
            .await
            .unwrap()
            .unwrap();
        assert!(n > 0, "closed before answering");
        seen.extend_from_slice(&buf[..n]);
    }
    // The connection is open and idle. Now stop.
    let started = Instant::now();
    stop.send(()).unwrap();
    timeout(Duration::from_secs(3), task)
        .await
        .expect("an idle connection must not make the server wait for the grace period")
        .unwrap()
        .unwrap();
    assert!(started.elapsed() < Duration::from_secs(3));
    let (rest, _) = until_closed(&mut idle).await;
    assert!(rest.is_empty(), "{rest}");
    assert!(
        TcpStream::connect(addr).await.is_err(),
        "no longer listening"
    );
    // The lock is free: the next start works.
    drop(bind(config).await.expect("the lock must be released"));
}

/// `bind` and `Bound::run`, which main uses, must hand the configured header timeout to the accept loop.
#[tokio::test]
async fn the_server_that_main_runs_closes_a_half_sent_request_after_the_configured_header_timeout()
{
    let (addr, _config, stop, task, _dir) = run_bound(|c| {
        c.header_timeout = Duration::from_millis(400);
        c.shutdown_grace = Duration::from_millis(300);
    })
    .await;
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\n")
        .await
        .unwrap();
    let (reply, took) = until_closed(&mut s).await;
    assert!(
        reply.is_empty() || reply.starts_with("HTTP/1.1 408"),
        "{reply}"
    );
    assert!(took >= Duration::from_millis(150), "{took:?}");
    stop.send(()).unwrap();
    timeout(CEILING, task).await.unwrap().unwrap().unwrap();
}

/// `bind` and `Bound::run` must also hand the configured cap on connections to the accept loop.
#[tokio::test]
async fn the_server_that_main_runs_honours_the_configured_cap_on_connections() {
    let (addr, _config, stop, task, _dir) = run_bound(|c| {
        c.max_connections = 1;
        c.shutdown_grace = Duration::from_millis(300);
    })
    .await;
    let mut first = TcpStream::connect(addr).await.unwrap();
    first
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    let mut buf = [0u8; 12];
    timeout(CEILING, first.read_exact(&mut buf))
        .await
        .unwrap()
        .unwrap();
    let mut second = TcpStream::connect(addr).await.unwrap();
    second
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut out = Vec::new();
    let early = timeout(Duration::from_millis(500), second.read_to_end(&mut out)).await;
    assert!(early.is_err(), "served beyond the cap: {out:?}");
    drop(first);
    let (reply, _) = until_closed(&mut second).await;
    assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
    stop.send(()).unwrap();
    timeout(CEILING, task).await.unwrap().unwrap().unwrap();
}

#[tokio::test]
async fn shutdown_waits_for_a_request_in_flight_and_lets_it_finish() {
    let mut server = start(|c| {
        c.shutdown_grace = Duration::from_secs(5);
        c.request_timeout = Duration::from_secs(20);
    })
    .await;
    let addr = server.addr;
    let request = tokio::spawn(async move { http_get(addr, "/nap/700").await });
    sleep(Duration::from_millis(200)).await;
    server.stop();
    let reply = timeout(CEILING, request).await.unwrap().unwrap();
    assert!(
        reply.starts_with("HTTP/1.1 200") && reply.ends_with("done"),
        "the request in flight was cut off: {reply}"
    );
    let took = server.finished().await;
    assert!(took < Duration::from_secs(4), "{took:?}");
}

#[tokio::test]
async fn shutdown_cuts_off_a_request_that_outlasts_the_grace_period() {
    let mut server = start(|c| {
        c.shutdown_grace = Duration::from_millis(500);
        c.request_timeout = Duration::from_secs(60);
    })
    .await;
    let mut s = TcpStream::connect(server.addr).await.unwrap();
    s.write_all(b"GET /slow HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    sleep(Duration::from_millis(150)).await;
    let stopped_at = Instant::now();
    server.stop();
    // The request is still being waited for, but nobody new is let in.
    sleep(Duration::from_millis(150)).await;
    assert!(
        TcpStream::connect(server.addr).await.is_err(),
        "the server still accepts connections during the grace period"
    );
    let (reply, _) = until_closed(&mut s).await;
    assert!(reply.is_empty(), "nothing was answered: {reply}");
    let took = server.finished().await;
    let total = stopped_at.elapsed();
    assert!(
        total >= Duration::from_millis(300),
        "stopped after {total:?}: the grace period was not waited for ({took:?})"
    );
    assert!(total < Duration::from_secs(5), "{total:?}");
}

#[tokio::test]
async fn shutdown_cuts_off_a_client_that_is_still_sending_its_headers() {
    let mut server = start(|c| {
        c.shutdown_grace = Duration::from_millis(500);
        c.header_timeout = Duration::from_secs(60);
    })
    .await;
    let mut s = TcpStream::connect(server.addr).await.unwrap();
    s.write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\n")
        .await
        .unwrap();
    sleep(Duration::from_millis(150)).await;
    let stopped_at = Instant::now();
    server.stop();
    let (_, _) = until_closed(&mut s).await;
    server.finished().await;
    let total = stopped_at.elapsed();
    assert!(
        total >= Duration::from_millis(300),
        "{total:?}: the grace period was not waited for"
    );
    assert!(total < Duration::from_secs(5), "{total:?}");
}

#[allow(dead_code)]
// ── the cap on open connections ─────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn connections_over_the_cap_wait_to_be_accepted_until_one_closes() {
    let mut server = start(|c| c.max_connections = 2).await;
    // two connections fill the cap; each has been served, so each is certainly accepted
    let mut held = Vec::new();
    for _ in 0..2 {
        let mut s = TcpStream::connect(server.addr).await.unwrap();
        s.write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();
        let mut buf = [0u8; 12];
        timeout(CEILING, s.read_exact(&mut buf))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&buf, b"HTTP/1.1 200");
        held.push(s);
    }
    // a third is let into the queue of the operating system but not served
    let mut third = TcpStream::connect(server.addr).await.unwrap();
    third
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut out = Vec::new();
    let early = timeout(Duration::from_millis(500), third.read_to_end(&mut out)).await;
    assert!(early.is_err(), "served while the cap was full: {out:?}");
    // one of the two goes away and the third is served
    drop(held.pop());
    let (reply, _) = until_closed(&mut third).await;
    assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
    server.stop();
    server.finished().await;
}

#[tokio::test]
async fn a_server_at_its_cap_still_stops_when_told_to() {
    let mut server = start(|c| c.max_connections = 1).await;
    let mut s = TcpStream::connect(server.addr).await.unwrap();
    s.write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    let mut buf = [0u8; 12];
    timeout(CEILING, s.read_exact(&mut buf))
        .await
        .unwrap()
        .unwrap();
    server.stop();
    server.finished().await;
}

#[tokio::test]
async fn a_cap_of_zero_or_beyond_what_a_semaphore_holds_is_put_in_range_not_a_hang_or_a_panic() {
    for cap in [0, usize::MAX] {
        let mut server = start(|c| c.max_connections = cap).await;
        let (reply, _) = {
            let mut s = TcpStream::connect(server.addr).await.unwrap();
            s.write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
            until_closed(&mut s).await
        };
        assert!(reply.starts_with("HTTP/1.1 200"), "cap {cap}: {reply}");
        server.stop();
        server.finished().await;
    }
}

// ── the write timeout (issue #109) ──────────────────────────────────────────────────────────────────────

/// A client whose receive buffer is tiny, so the server's response backs up at once.
async fn small_buffer_client(addr: SocketAddr) -> TcpStream {
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.set_recv_buffer_size(4096).unwrap();
    socket.connect(addr).await.unwrap()
}

#[tokio::test]
async fn a_client_that_never_reads_its_response_is_cut_off_and_frees_its_place() {
    let mut server = start(|c| {
        c.max_connections = 1;
        c.write_timeout = Duration::from_millis(400);
    })
    .await;
    let mut stuck = small_buffer_client(server.addr).await;
    stuck
        .write_all(b"GET /big HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    // the only place is held by `stuck` until the write timeout ends it; the second client is served after that
    let started = Instant::now();
    let mut other = TcpStream::connect(server.addr).await.unwrap();
    other
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let (reply, _) = until_closed(&mut other).await;
    assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
    assert!(
        started.elapsed() >= Duration::from_millis(300),
        "the place was free before the write timeout: {:?}",
        started.elapsed()
    );
    drop(stuck);
    server.stop();
    server.finished().await;
}

#[tokio::test]
async fn a_client_that_reads_slowly_but_steadily_is_not_cut_off() {
    let mut server = start(|c| c.write_timeout = Duration::from_millis(500)).await;
    let mut slow = small_buffer_client(server.addr).await;
    slow.write_all(b"GET /big HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    // 1 MiB every 100 ms: each step is well inside the timeout, the whole is several times longer than it
    let started = Instant::now();
    let mut got = Vec::new();
    let mut chunk = vec![0u8; 1024 * 1024];
    loop {
        sleep(Duration::from_millis(100)).await;
        let n = timeout(CEILING, slow.read(&mut chunk))
            .await
            .expect("the body keeps coming")
            .unwrap();
        if n == 0 {
            break;
        }
        got.extend_from_slice(&chunk[..n]);
    }
    assert!(
        started.elapsed() > Duration::from_millis(600),
        "the read was too fast to prove anything: {:?}",
        started.elapsed()
    );
    let head_end = got.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
    assert_eq!(got.len() - head_end, BIG, "the whole body arrived");
    server.stop();
    server.finished().await;
}

// ── the cap per address (issue #109) ────────────────────────────────────────────────────────────────────

/// Opens a connection and waits until it has been served, which proves it was accepted and is held open.
async fn held_connection(addr: SocketAddr) -> TcpStream {
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    let mut buf = [0u8; 12];
    timeout(CEILING, s.read_exact(&mut buf))
        .await
        .expect("a connection under the cap is served")
        .unwrap();
    assert_eq!(&buf, b"HTTP/1.1 200");
    s
}

/// Whether the server answers a fresh connection from this machine, trying for a while: a place is given back a
/// moment after the client closes, so the first try may still find it taken.
async fn eventually_served(addr: SocketAddr) -> bool {
    let deadline = Instant::now() + CEILING;
    while Instant::now() < deadline {
        let mut s = TcpStream::connect(addr).await.unwrap();
        let _ = s
            .write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
            .await;
        let mut out = Vec::new();
        let _ = timeout(Duration::from_secs(1), s.read_to_end(&mut out)).await;
        if out.starts_with(b"HTTP/1.1 200") {
            return true;
        }
        sleep(Duration::from_millis(50)).await;
    }
    false
}

#[tokio::test]
async fn a_connection_past_the_per_address_cap_is_closed_at_once_and_the_others_keep_working() {
    let mut server = start(|c| c.max_connections_per_ip = 2).await;
    let mut first = held_connection(server.addr).await;
    let _second = held_connection(server.addr).await;
    // the third is accepted at the TCP level and closed without a word
    let mut third = TcpStream::connect(server.addr).await.unwrap();
    let _ = third
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .await;
    let (reply, took) = until_closed(&mut third).await;
    assert!(
        reply.is_empty(),
        "a refused connection gets no response: {reply}"
    );
    assert!(
        took < Duration::from_secs(2),
        "closed at once, not queued: {took:?}"
    );
    // the two that were let in are still served (what is left of the first answer comes with the second)
    first
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let (reply, _) = until_closed(&mut first).await;
    assert_eq!(reply.matches("HTTP/1.1 200").count(), 1, "{reply}");
    // one goes away and a new one is accepted
    drop(first);
    assert!(
        eventually_served(server.addr).await,
        "the count was not released"
    );
    server.stop();
    server.finished().await;
}

#[tokio::test]
async fn the_per_address_count_is_released_when_a_connection_is_ended_by_the_server() {
    // the only place for this address is taken by a silent connection that the header timeout ends
    let mut server = start(|c| {
        c.max_connections_per_ip = 1;
        c.header_timeout = Duration::from_millis(300);
    })
    .await;
    let mut silent = TcpStream::connect(server.addr).await.unwrap();
    let (reply, _) = until_closed(&mut silent).await;
    assert!(reply.is_empty());
    assert!(
        eventually_served(server.addr).await,
        "the count was not released"
    );
    server.stop();
    server.finished().await;
}

#[tokio::test]
async fn the_global_cap_still_queues_with_a_per_address_cap_on() {
    let mut server = start(|c| {
        c.max_connections = 1;
        c.max_connections_per_ip = 5;
    })
    .await;
    let held = held_connection(server.addr).await;
    let mut queued = TcpStream::connect(server.addr).await.unwrap();
    queued
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut out = Vec::new();
    let early = timeout(Duration::from_millis(500), queued.read_to_end(&mut out)).await;
    assert!(
        early.is_err(),
        "served or closed past the global cap: {out:?}"
    );
    drop(held);
    let (reply, _) = until_closed(&mut queued).await;
    assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
    server.stop();
    server.finished().await;
}

#[tokio::test]
async fn without_a_per_address_cap_one_address_may_hold_many_connections() {
    let mut server = start(|_| {}).await;
    let mut held = Vec::new();
    for _ in 0..8 {
        held.push(held_connection(server.addr).await);
    }
    server.stop();
    server.finished().await;
}

// ── upgraded connections (issue #109) ───────────────────────────────────────────────────────────────────

/// What a WebSocket handler will do, with the bytes of the protocol left out: answer 101, take the connection
/// over in a task, echo. `mode` is how that task treats the shutdown signal: `echo` stops at once, `slowbye`
/// says goodbye after 300 ms and stops, `deaf` never looks at it.
async fn upgrade(Path(mode): Path<String>, mut req: Request) -> Response {
    let slot = req
        .extensions_mut()
        .remove::<UpgradeSlot>()
        .expect("every request carries the slot of its connection");
    let on = hyper::upgrade::on(req);
    slot.spawn(move |mut shutdown| async move {
        let Ok(up) = on.await else { return };
        let mut io = hyper_util::rt::TokioIo::new(up);
        let mut buf = [0u8; 1024];
        loop {
            tokio::select! {
                () = shutdown.wait(), if mode != "deaf" => {
                    if mode == "slowbye" {
                        sleep(Duration::from_millis(300)).await;
                        let _ = io.write_all(b"bye").await;
                    }
                    break;
                }
                n = io.read(&mut buf) => match n {
                    Ok(n) if n > 0 => {
                        if io.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                    }
                    _ => break,
                },
            }
        }
    });
    Response::builder()
        .status(101)
        .header("connection", "upgrade")
        .header("upgrade", "test")
        .body(axum::body::Body::empty())
        .unwrap()
}

/// Connects, upgrades and checks the 101.
async fn upgraded(addr: SocketAddr, mode: &str) -> TcpStream {
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(
        format!("GET /upgrade/{mode} HTTP/1.1\r\nHost: x\r\nConnection: Upgrade\r\nUpgrade: test\r\n\r\n")
            .as_bytes(),
    )
    .await
    .unwrap();
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        timeout(CEILING, s.read_exact(&mut byte))
            .await
            .expect("the answer to the upgrade")
            .unwrap();
        head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head);
    assert!(head.starts_with("HTTP/1.1 101"), "{head}");
    s
}

async fn echoes(s: &mut TcpStream, word: &[u8]) {
    s.write_all(word).await.unwrap();
    let mut got = vec![0u8; word.len()];
    timeout(CEILING, s.read_exact(&mut got))
        .await
        .expect("the echo")
        .unwrap();
    assert_eq!(got, word);
}

#[tokio::test]
async fn an_upgraded_connection_is_handed_over_and_echoes() {
    // with `.with_upgrades()` removed hyper cannot hand the connection to `hyper::upgrade::on`
    let mut server = start(|_| {}).await;
    let mut s = upgraded(server.addr, "echo").await;
    echoes(&mut s, b"ping").await;
    echoes(&mut s, b"pong").await;
    drop(s);
    server.stop();
    server.finished().await;
}

#[tokio::test]
async fn an_upgraded_connection_keeps_its_place_under_the_global_cap() {
    let mut server = start(|c| c.max_connections = 1).await;
    let up = upgraded(server.addr, "echo").await;
    let mut queued = TcpStream::connect(server.addr).await.unwrap();
    queued
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut out = Vec::new();
    let early = timeout(Duration::from_millis(500), queued.read_to_end(&mut out)).await;
    assert!(
        early.is_err(),
        "served beyond the cap while a connection was upgraded: {out:?}"
    );
    drop(up);
    let (reply, _) = until_closed(&mut queued).await;
    assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
    server.stop();
    server.finished().await;
}

#[tokio::test]
async fn an_upgraded_connection_keeps_its_place_under_the_per_address_cap() {
    let mut server = start(|c| c.max_connections_per_ip = 1).await;
    let up = upgraded(server.addr, "echo").await;
    let mut second = TcpStream::connect(server.addr).await.unwrap();
    let _ = second
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .await;
    let (reply, _) = until_closed(&mut second).await;
    assert!(reply.is_empty(), "the address was over its cap: {reply}");
    drop(up);
    assert!(
        eventually_served(server.addr).await,
        "the count was not released"
    );
    server.stop();
    server.finished().await;
}

#[tokio::test]
async fn shutdown_tells_an_upgraded_task_to_finish_and_waits_for_it() {
    let mut server = start(|c| c.shutdown_grace = Duration::from_secs(5)).await;
    let mut s = upgraded(server.addr, "slowbye").await;
    echoes(&mut s, b"ping").await;
    server.stop();
    let (reply, _) = until_closed(&mut s).await;
    assert_eq!(
        reply, "bye",
        "the task was told, and was let finish what it was saying"
    );
    let took = server.finished().await;
    assert!(
        took < Duration::from_secs(4),
        "it did not need the whole grace period: {took:?}"
    );
}

#[tokio::test]
async fn an_upgraded_task_that_ignores_the_signal_is_aborted_after_the_grace_period() {
    let mut server = start(|c| c.shutdown_grace = Duration::from_millis(400)).await;
    let mut s = upgraded(server.addr, "deaf").await;
    echoes(&mut s, b"ping").await;
    server.stop();
    let started = Instant::now();
    // the client is cut off once the task is aborted
    let (reply, _) = until_closed(&mut s).await;
    assert!(reply.is_empty());
    assert!(
        started.elapsed() >= Duration::from_millis(300),
        "it was waited for until the grace period: {:?}",
        started.elapsed()
    );
    server.finished().await;
}
