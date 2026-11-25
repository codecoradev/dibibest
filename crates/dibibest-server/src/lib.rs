//! The dibibest server: one process behind one origin (epic #84, issue #91).
//!
//! It serves the built web app and, as their stages land, the API, the live WebSocket and the MCP endpoint. It
//! runs `dibibest-core` natively. [`bind`] takes the data-directory lock, opens the store and the socket;
//! [`Bound::run`] serves until told to stop, then lets the store flush and releases the lock.

pub mod app;
pub mod auth;
pub mod backup;
pub mod config;
pub mod conn;
pub mod limit;
pub mod live;
pub mod lock;
pub mod mcp;
pub mod static_files;

use app::{router, AppState};
use axum::extract::ConnectInfo;
use axum::Router;
use config::Config;
use dibibest_store::{SqliteStore, StoreError};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::{TokioIo, TokioTimer};
use lock::{DataLock, LockError};
use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::{watch, Semaphore};
use tokio::task::JoinSet;
use tower::Service;

/// Waits for a signal. If the signal cannot be waited for at all (the handler failed to register, the stream
/// closed) it logs that and never completes: a handler that could not be installed must not look like a request
/// to shut down.
pub async fn signal_or_never(
    name: &'static str,
    source: impl Future<Output = std::io::Result<()>>,
) {
    if let Err(e) = source.await {
        tracing::error!("cannot wait for {name}: {e}; it will not stop the server");
        std::future::pending::<()>().await;
    }
}

/// Completes on Ctrl-C or SIGTERM (what `docker stop` sends).
pub async fn shutdown_signal() {
    let ctrl_c = signal_or_never("Ctrl-C", tokio::signal::ctrl_c());
    #[cfg(unix)]
    let term = signal_or_never("SIGTERM", async {
        let mut s = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        s.recv().await.map_or_else(
            || Err(std::io::Error::other("the signal stream closed")),
            |()| Ok(()),
        )
    });
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! { () = ctrl_c => {}, () = term => {} }
    tracing::info!("shutting down");
}

#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error(transparent)]
    Lock(#[from] LockError),
    #[error("store: {0}")]
    Store(#[from] StoreError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("backup directory: {0}")]
    Backup(#[from] backup::BackupError),
}

/// A server that has everything it needs and is not serving yet.
pub struct Bound {
    listener: TcpListener,
    state: AppState,
    options: ServeOptions,
    backup: Option<backup::BackupConfig>,
    // Declared last so it is released last, after the store has flushed.
    _lock: DataLock,
}

/// Takes the lock on the data directory, opens the database and binds the address, in that order: a second
/// process stops at the lock and never touches the database.
pub async fn bind(config: Config) -> Result<Bound, ServerError> {
    let lock = DataLock::acquire(&config.data_dir)?;
    let store = SqliteStore::open(config.data_dir.join("dibibest.db"))?;
    // A backup directory that cannot be written to stops the start: a server that looks fine and never backs up
    // is the worse outcome.
    let backup = backup::BackupConfig::from_config(&config);
    if let Some(b) = &backup {
        backup::check_dir(&b.dir)?;
    }
    let listener = TcpListener::bind(config.addr).await?;
    Ok(Bound {
        listener,
        state: AppState::new(Arc::new(store), &config),
        options: ServeOptions::from_config(&config),
        backup,
        _lock: lock,
    })
}

impl Bound {
    pub fn local_addr(&self) -> SocketAddr {
        self.listener
            .local_addr()
            .expect("a bound listener has an address")
    }

    /// Serves until `shutdown` completes. Then it stops taking connections, waits up to the grace period for
    /// requests in flight, lets the store flush its writer, and only then returns and releases the lock.
    pub async fn run(
        self,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> Result<(), ServerError> {
        let Self {
            listener,
            state,
            options,
            backup,
            _lock,
        } = self;
        let grace = options.grace;
        // Everything that holds the store lives inside `serve_until` and the backup task; when both are over they
        // are all gone, and `_lock` is released after that, at the end of this function. The exception is a backup
        // that is still running when the grace period ends: it cannot be cancelled (it is inside SQLite), so the
        // process is about to exit with it, and the half-written copy it leaves is removed by the next run.
        let backup = backup.map(|cfg| backup::spawn(state.store.clone(), cfg));
        // The scheduler is told to stop when the shutdown begins, not when the requests are done: a backup in the
        // middle of its work then gets the whole grace period beside them, instead of a second one after them.
        let signal = backup.as_ref().map(backup::Backup::stop_signal);
        let begun = Arc::new(std::sync::OnceLock::<std::time::Instant>::new());
        let shutdown = {
            let (signal, begun) = (signal.clone(), begun.clone());
            async move {
                shutdown.await;
                let _ = begun.set(std::time::Instant::now());
                if let Some(signal) = signal {
                    let _ = signal.send(true);
                }
            }
        };
        let served = serve_until(listener, router(state), options, shutdown).await;
        if let Some(backup) = backup {
            // however serve_until ended, the scheduler must be told
            if let Some(signal) = signal {
                let _ = signal.send(true);
            }
            let left = grace.saturating_sub(begun.get().map_or(Duration::ZERO, |t| t.elapsed()));
            if tokio::time::timeout(left, backup.join()).await.is_err() {
                tracing::warn!(
                    "a backup was still running when the grace period ended; it is abandoned and its partial file is removed by the next run"
                );
            }
        }
        served
    }
}

/// The limits of the accept loop.
#[derive(Debug, Clone, Copy)]
pub struct ServeOptions {
    /// How long a connection has to deliver the headers of its next request.
    pub header_timeout: Duration,
    /// How long a write may make no progress at all before the connection is dropped.
    pub write_timeout: Duration,
    /// Connections served at once (clamped into what a semaphore can hold).
    pub max_connections: usize,
    /// Connections one address may hold at once; 0 is no limit.
    pub max_per_ip: usize,
    /// How long a shutdown waits for what is in flight.
    pub grace: Duration,
}

impl ServeOptions {
    pub fn from_config(c: &Config) -> Self {
        Self {
            header_timeout: c.header_timeout,
            write_timeout: c.write_timeout,
            max_connections: c.max_connections,
            max_per_ip: c.max_connections_per_ip,
            grace: c.shutdown_grace,
        }
    }
}

/// Serves `app` on `listener` until `shutdown` completes.
///
/// Each connection is an HTTP/1.1 connection of `hyper`, served by a task of its own. A connection that has not
/// delivered the complete headers of its next request within `header_timeout` is closed, whether it sent half a
/// request line, one byte at a time, or nothing: the timer runs for the whole head, not for a pause between
/// bytes, and it also ends an idle keep-alive connection. Every request carries the peer address as
/// `ConnectInfo<SocketAddr>`.
///
/// At most `max_connections` connections are served at once. At that number the loop stops calling `accept`, so
/// further connections wait in the queue of the operating system, and the places are given back as connections
/// close. A connection that holds its place for the full `header_timeout` while sending nothing is therefore the
/// cost of a client that opens many: the cap bounds it, and the timeout ends each one.
///
/// A write that makes no progress for `write_timeout` ends the connection, so a client that never reads its
/// response cannot hold a place. With `max_per_ip` above 0 an address that already holds that many connections has
/// the next one accepted and closed at once, with no response (it is not queued, so one address cannot fill the
/// queue). A request that upgrades its connection hands it to a task of its own through [`conn::UpgradeSlot`]; the
/// task keeps both places and is told to finish at shutdown.
///
/// When `shutdown` completes the listener is dropped and every connection is told to finish: a request in flight
/// is answered, an idle connection that has completed a request is closed at once, and one that has sent nothing
/// yet is treated as a request in flight (hyper cannot tell it from a slow one) and waited for. After `grace`,
/// whatever is still open is dropped (upgraded tasks share the same grace period). The
/// connection tasks are all gone, and with them their copies of `app`, when this returns.
///
/// Only HTTP/1.1 is spoken: the server sits behind a reverse proxy that terminates TLS and talks HTTP/1.1 to it,
/// and `hyper-util`'s protocol-sniffing builder reads the first bytes of a connection with no timeout at all,
/// which would leave the slowloris hole open for exactly the connections it was meant to close.
pub async fn serve_until(
    listener: TcpListener,
    app: Router,
    options: ServeOptions,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), ServerError> {
    let ServeOptions {
        header_timeout,
        write_timeout,
        max_connections,
        max_per_ip,
        grace,
    } = options;
    let per_ip = (max_per_ip > 0).then(|| conn::PerAddress::new(max_per_ip));
    let (drain_tx, drain_rx) = watch::channel(());
    let mut connections = JoinSet::new();
    let upgrades = conn::Upgrades::default();
    let mut shutdown = std::pin::pin!(shutdown);
    // 0 would never accept anything, and more than the semaphore can hold would panic: both are put in range
    // here because this function is public (the configuration is checked already, so this only matters to a caller
    // that skips it).
    let room = Arc::new(Semaphore::new(
        max_connections.clamp(1, Semaphore::MAX_PERMITS),
    ));
    loop {
        // A place first, then the connection: at the cap nothing is accepted, and the next connection waits in the
        // queue of the operating system (and is refused there once that is full) instead of being held here.
        let place = tokio::select! {
            () = &mut shutdown => break,
            // Finished connections are collected as they go, so the set stays as small as the open ones.
            Some(done) = connections.join_next(), if !connections.is_empty() => {
                log_if_panicked(done);
                continue;
            }
            place = room.clone().acquire_owned() => place.expect("the semaphore is never closed"),
        };
        tokio::select! {
            () = &mut shutdown => break,
            accepted = listener.accept() => match accepted {
                Ok((stream, peer)) => {
                    // An address at its limit is accepted and closed at once, with no response. It is not queued:
                    // a queue would let that one address fill the global one.
                    let ip_place = match per_ip.as_ref().map(|p| p.try_acquire(peer.ip())) {
                        Some(None) => {
                            tracing::debug!(%peer, "connection refused: the address holds the most it may");
                            continue;
                        }
                        Some(Some(permit)) => Some(permit),
                        None => None,
                    };
                    connections.spawn(serve_connection(
                        stream,
                        peer,
                        app.clone(),
                        (header_timeout, write_timeout),
                        conn::Connection::new(place, ip_place, upgrades.clone(), drain_rx.clone()),
                        drain_rx.clone(),
                    ));
                }
                Err(e) => {
                    // the pause is not a reason to stop later than asked
                    tokio::select! {
                        () = &mut shutdown => break,
                        () = accept_failed(&e) => {}
                    }
                }
            },
            Some(done) = connections.join_next(), if !connections.is_empty() => log_if_panicked(done),
        }
    }
    drop(listener);
    // Dropping the sender is the signal: every connection starts finishing.
    drop(drain_tx);
    tracing::info!(
        open = connections.len(),
        "no longer accepting; waiting for requests in flight"
    );
    // One grace period for everything that is still running, not one per kind of task.
    let deadline = tokio::time::Instant::now() + grace;
    let drained = tokio::time::timeout_at(deadline, async {
        while let Some(done) = connections.join_next().await {
            log_if_panicked(done);
        }
    })
    .await;
    if drained.is_err() {
        tracing::warn!(
            open = connections.len(),
            "grace period over, closing connections that are still open"
        );
        connections.abort_all();
        while let Some(done) = connections.join_next().await {
            log_if_panicked(done);
        }
    }
    // The connections that were upgraded (a WebSocket) left the set above when their 101 went out. Their tasks
    // were told with the same signal; they get what is left of the grace period and are then aborted. Every
    // connection task is gone here, so nothing can add to the set any more.
    let mut upgraded = upgrades.take();
    if !upgraded.is_empty() {
        tracing::info!(open = upgraded.len(), "waiting for upgraded connections");
        let drained = tokio::time::timeout_at(deadline, async {
            while let Some(done) = upgraded.join_next().await {
                log_if_panicked(done);
            }
        })
        .await;
        if drained.is_err() {
            tracing::warn!(
                open = upgraded.len(),
                "grace period over, closing upgraded connections that are still open"
            );
            upgraded.abort_all();
            while let Some(done) = upgraded.join_next().await {
                log_if_panicked(done);
            }
        }
    }
    // `app` and the state in it go here, after the last connection task has gone.
    drop(app);
    Ok(())
}

fn log_if_panicked(done: Result<(), tokio::task::JoinError>) {
    if let Err(e) = done {
        if e.is_panic() {
            tracing::error!("a connection task panicked: {e}");
        }
    }
}

/// A failed `accept`. A connection that was reset before it was taken is that client's affair and nothing
/// else. Anything else (usually too many open files) would fail again at once, so the loop waits a moment
/// instead of spinning.
async fn accept_failed(e: &std::io::Error) {
    use std::io::ErrorKind::{ConnectionAborted, ConnectionRefused, ConnectionReset};
    if matches!(
        e.kind(),
        ConnectionRefused | ConnectionAborted | ConnectionReset
    ) {
        return;
    }
    tracing::error!("accept failed: {e}");
    tokio::time::sleep(Duration::from_secs(1)).await;
}

/// One connection, until it closes, fails, or is told to finish (`drain` is closed) and has.
async fn serve_connection(
    stream: tokio::net::TcpStream,
    peer: SocketAddr,
    app: Router,
    (header_timeout, write_timeout): (Duration, Duration),
    connection: conn::Connection,
    mut drain: watch::Receiver<()>,
) {
    let service = service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
        let mut app = app.clone();
        let slot = connection.slot();
        async move {
            let mut req = req.map(axum::body::Body::new);
            req.extensions_mut().insert(ConnectInfo(peer));
            req.extensions_mut().insert(slot);
            app.call(req).await
        }
    });
    let connection = http1::Builder::new()
        .timer(TokioTimer::new())
        .header_read_timeout(header_timeout)
        .serve_connection(
            TokioIo::new(conn::WriteTimeout::new(stream, write_timeout)),
            service,
        )
        // Upgrades are how the live WebSocket will arrive.
        .with_upgrades();
    let mut connection = std::pin::pin!(connection);
    let mut finishing = false;
    let outcome = loop {
        tokio::select! {
            outcome = connection.as_mut() => break outcome,
            // `changed` fails once the sender is gone, which is the signal here.
            _ = drain.changed(), if !finishing => {
                finishing = true;
                connection.as_mut().graceful_shutdown();
            }
        }
    };
    if let Err(e) = outcome {
        tracing::debug!(%peer, "connection ended: {e}");
    }
}
