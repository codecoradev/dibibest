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
use config::Config;
use dibibest_store::{SqliteStore, StoreError};
use lock::{DataLock, LockError};
use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

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
        serve_until(listener, state, grace, shutdown).await
    }
}

async fn serve_until(
    listener: TcpListener,
    state: AppState,
    grace: Duration,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), ServerError> {
    let app = router(state);
    let (begun_tx, begun_rx) = oneshot::channel::<()>();
    let serve = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        shutdown.await;
        let _ = begun_tx.send(());
    });
    let mut serve = std::pin::pin!(std::future::IntoFuture::into_future(serve));
    tokio::select! {
        r = &mut serve => r.map_err(ServerError::from),
        () = async {
            if begun_rx.await.is_ok() {
                tokio::time::sleep(grace).await;
            } else {
                std::future::pending::<()>().await;
            }
        } => {
            tracing::warn!("grace period over, closing connections that are still open");
            Ok(())
        }
    }
}
