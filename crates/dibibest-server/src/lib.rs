//! The dibibest server: one process behind one origin (epic #84, issue #91).
//!
//! It serves the built web app and, as their stages land, the API, the live WebSocket and the MCP endpoint. It
//! runs `dibibest-core` natively. [`bind`] takes the data-directory lock, opens the store and the socket;
//! [`Bound::run`] serves until told to stop, then lets the store flush and releases the lock.

pub mod app;
pub mod auth;
pub mod config;
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
use tokio::sync::{watch, OwnedSemaphorePermit, Semaphore};
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
}

/// A server that has everything it needs and is not serving yet.
pub struct Bound {
    listener: TcpListener,
    state: AppState,
    grace: Duration,
    // Declared last so it is released last, after the store has flushed.
    _lock: DataLock,
}

/// Takes the lock on the data directory, opens the database and binds the address, in that order: a second
/// process stops at the lock and never touches the database.
pub async fn bind(config: Config) -> Result<Bound, ServerError> {
    let lock = DataLock::acquire(&config.data_dir)?;
    let store = SqliteStore::open(config.data_dir.join("dibibest.db"))?;
    let listener = TcpListener::bind(config.addr).await?;
    Ok(Bound {
        listener,
        state: AppState::new(Arc::new(store), &config),
        grace: config.shutdown_grace,
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
            grace,
            _lock,
        } = self;
        // Everything that holds the store lives inside `serve_until`; when it returns they are all gone, and
        // `_lock` is released after that, at the end of this function.
        let (header_timeout, max_connections) = (state.header_timeout, state.max_connections);
        serve_until(
            listener,
            router(state),
            header_timeout,
            max_connections,
            grace,
            shutdown,
        )
        .await
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
/// When `shutdown` completes the listener is dropped and every connection is told to finish: a request in flight
/// is answered, an idle connection that has completed a request is closed at once, and one that has sent nothing
/// yet is treated as a request in flight (hyper cannot tell it from a slow one) and waited for. After `grace`,
/// whatever is still open is dropped. The
/// connection tasks are all gone, and with them their copies of `app`, when this returns.
///
/// Only HTTP/1.1 is spoken: the server sits behind a reverse proxy that terminates TLS and talks HTTP/1.1 to it,
/// and `hyper-util`'s protocol-sniffing builder reads the first bytes of a connection with no timeout at all,
/// which would leave the slowloris hole open for exactly the connections it was meant to close.
pub async fn serve_until(
    listener: TcpListener,
    app: Router,
    header_timeout: Duration,
    max_connections: usize,
    grace: Duration,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), ServerError> {
    let (drain_tx, drain_rx) = watch::channel(());
    let mut connections = JoinSet::new();
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
                    connections.spawn(serve_connection(
                        stream,
                        peer,
                        app.clone(),
                        header_timeout,
                        drain_rx.clone(),
                        place,
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
    let drained = tokio::time::timeout(grace, async {
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
    header_timeout: Duration,
    mut drain: watch::Receiver<()>,
    _place: OwnedSemaphorePermit,
) {
    let service = service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
        let mut app = app.clone();
        async move {
            let mut req = req.map(axum::body::Body::new);
            req.extensions_mut().insert(ConnectInfo(peer));
            app.call(req).await
        }
    });
    let connection = http1::Builder::new()
        .timer(TokioTimer::new())
        .header_read_timeout(header_timeout)
        .serve_connection(TokioIo::new(stream), service)
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
