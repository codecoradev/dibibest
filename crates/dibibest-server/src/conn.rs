//! Per-connection machinery of the accept loop: the write timeout, the per-address count and the hand-over of an
//! upgraded connection (issue #109).

use crate::limit::network_key;
use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::{watch, OwnedSemaphorePermit};
use tokio::task::JoinSet;
use tokio::time::{Instant, Sleep};

/// An IO wrapper that fails a write that makes no progress for `limit`.
///
/// hyper 1's HTTP/1 builder has no write timeout, so a client that sends a request and never reads the answer
/// would hold its connection, and its place under the cap, for ever. The timer runs only while a write is waiting
/// on the peer (it starts when a write first returns pending and is dropped when one completes), so an idle
/// keep-alive connection is not touched (the header-read timeout owns that) and a slow reader that keeps taking
/// bytes is not cut off: every accepted byte restarts the clock. Reads pass through.
pub struct WriteTimeout<T> {
    inner: T,
    limit: Duration,
    /// Set while a write has been pending since this instant.
    sleep: Pin<Box<Sleep>>,
    armed: bool,
}

impl<T> WriteTimeout<T> {
    pub fn new(inner: T, limit: Duration) -> Self {
        Self {
            inner,
            limit,
            sleep: Box::pin(tokio::time::sleep(limit)),
            armed: false,
        }
    }

    fn watch<R>(
        &mut self,
        cx: &mut Context<'_>,
        polled: Poll<io::Result<R>>,
    ) -> Poll<io::Result<R>> {
        match polled {
            Poll::Ready(r) => {
                self.armed = false;
                Poll::Ready(r)
            }
            Poll::Pending => {
                if !self.armed {
                    self.sleep.as_mut().reset(Instant::now() + self.limit);
                    self.armed = true;
                }
                if self.sleep.as_mut().poll(cx).is_ready() {
                    self.armed = false;
                    Poll::Ready(Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "the peer did not take the response in time",
                    )))
                } else {
                    Poll::Pending
                }
            }
        }
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for WriteTimeout<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for WriteTimeout<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let polled = Pin::new(&mut self.inner).poll_write(cx, buf);
        self.watch(cx, polled)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let polled = Pin::new(&mut self.inner).poll_write_vectored(cx, bufs);
        self.watch(cx, polled)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let polled = Pin::new(&mut self.inner).poll_flush(cx);
        self.watch(cx, polled)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let polled = Pin::new(&mut self.inner).poll_shutdown(cx);
        self.watch(cx, polled)
    }
}

/// How many connections each client holds. The key is the one the rate limiter uses (an IPv6 network is one
/// client). The table holds only clients with an open connection, so it is never larger than the global cap.
#[derive(Clone)]
pub struct PerAddress {
    max: usize,
    counts: Arc<Mutex<HashMap<IpAddr, usize>>>,
}

impl PerAddress {
    pub fn new(max: usize) -> Self {
        Self {
            max,
            counts: Arc::default(),
        }
    }

    /// A place for `ip`, or `None` when it already holds the most it may. The place is given back when the
    /// returned guard is dropped, however the connection ends.
    pub fn try_acquire(&self, ip: IpAddr) -> Option<AddressPermit> {
        let key = network_key(ip);
        let mut counts = self.counts.lock().unwrap_or_else(PoisonError::into_inner);
        let held = counts.get(&key).copied().unwrap_or(0);
        if held >= self.max {
            // nothing is inserted for a refusal, so the table holds only clients with an open connection
            return None;
        }
        counts.insert(key, held + 1);
        Some(AddressPermit {
            key,
            counts: self.counts.clone(),
        })
    }

    /// How many clients hold a connection.
    pub fn clients(&self) -> usize {
        self.counts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
}

/// One connection's place in a [`PerAddress`].
pub struct AddressPermit {
    key: IpAddr,
    counts: Arc<Mutex<HashMap<IpAddr, usize>>>,
}

impl Drop for AddressPermit {
    fn drop(&mut self) {
        let mut counts = self.counts.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(n) = counts.get_mut(&self.key) {
            *n -= 1;
            if *n == 0 {
                counts.remove(&self.key);
            }
        }
    }
}

/// What a connection holds for as long as it lives: its place under the global cap and under the per-address cap.
struct Held {
    _place: OwnedSemaphorePermit,
    _address: Option<AddressPermit>,
}

/// The tasks that took an upgraded connection over. A plain `JoinSet` behind a lock, because the handler that
/// spawns into it runs inside a connection task while `serve_until` is the one that waits for it and aborts it;
/// a bare counter would say how many are left but give no way to abort them.
#[derive(Clone, Default)]
pub struct Upgrades(Arc<Mutex<JoinSet<()>>>);

impl Upgrades {
    /// Takes the whole set out, for the shutdown to wait on. Called only after every connection task is gone, so
    /// no handler is left that could add to it.
    pub fn take(&self) -> JoinSet<()> {
        std::mem::take(&mut *self.0.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

/// Completes when the server begins to shut down. A task that has taken over an upgraded connection waits on it
/// next to its own work, and ends (after a close frame, say) so that it is gone within the grace period.
pub struct Shutdown(watch::Receiver<()>);

impl Shutdown {
    pub async fn wait(&mut self) {
        // `changed` fails once the sender is gone, which is the signal
        while self.0.changed().await.is_ok() {}
    }
}

/// The part of a connection that outlives its HTTP side when a request upgrades it (a WebSocket).
///
/// Every request carries one in its extensions. A handler that answers `101 Switching Protocols` takes it
/// (`req.extensions_mut().remove::<UpgradeSlot>()`) and hands the work to [`UpgradeSlot::spawn`]. From then on
/// the connection is no longer served by `hyper`, so what `serve_until` would otherwise have done for it, the slot
/// does: the places under both caps stay held until the task ends, the task is given the shutdown signal, and at
/// shutdown it is waited for up to the grace period and then aborted. The write timeout still applies to the
/// upgraded IO (it wraps the socket); a read has no timeout, an idle policy is the task's to make.
///
/// The contract is the handler's: one that answers 101 and takes the connection over *without* going through
/// [`UpgradeSlot::spawn`] escapes both caps, the shutdown signal and the wait for the grace period. Nothing in the
/// types forces the use of the slot, so a code review of a new upgrade route has to.
#[derive(Clone)]
pub struct UpgradeSlot {
    held: Arc<Held>,
    upgrades: Upgrades,
    drain: watch::Receiver<()>,
}

impl UpgradeSlot {
    /// Runs `task` for the rest of the connection's life. It is given the shutdown signal; if it does not end in
    /// the grace period it is aborted, which drops it and with it everything it owns.
    pub fn spawn<F, Fut>(self, task: F)
    where
        F: FnOnce(Shutdown) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let Self {
            held,
            upgrades,
            drain,
        } = self;
        let task = task(Shutdown(drain));
        let mut set = upgrades.0.lock().unwrap_or_else(PoisonError::into_inner);
        // finished ones are collected as new ones come, so the set stays near the number of open ones
        while let Some(done) = set.try_join_next() {
            if let Err(e) = done {
                if e.is_panic() {
                    tracing::error!("an upgraded connection's task panicked: {e}");
                }
            }
        }
        set.spawn(async move {
            let _held = held;
            task.await;
        });
    }
}

/// What a connection task is given besides its socket.
pub struct Connection {
    held: Arc<Held>,
    upgrades: Upgrades,
    drain: watch::Receiver<()>,
}

impl Connection {
    pub fn new(
        place: OwnedSemaphorePermit,
        address: Option<AddressPermit>,
        upgrades: Upgrades,
        drain: watch::Receiver<()>,
    ) -> Self {
        Self {
            held: Arc::new(Held {
                _place: place,
                _address: address,
            }),
            upgrades,
            drain,
        }
    }

    /// The slot a request of this connection carries.
    pub fn slot(&self) -> UpgradeSlot {
        UpgradeSlot {
            held: self.held.clone(),
            upgrades: self.upgrades.clone(),
            drain: self.drain.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn a_refusal_leaves_nothing_in_the_table_even_with_a_cap_of_zero() {
        let per_ip = PerAddress::new(0);
        assert!(per_ip.try_acquire("192.0.2.1".parse().unwrap()).is_none());
        assert_eq!(per_ip.clients(), 0, "a refused client is not remembered");
        let per_ip = PerAddress::new(1);
        let held = per_ip.try_acquire("192.0.2.1".parse().unwrap()).unwrap();
        assert!(per_ip.try_acquire("192.0.2.1".parse().unwrap()).is_none());
        assert_eq!(per_ip.clients(), 1);
        drop(held);
        assert_eq!(per_ip.clients(), 0);
    }

    #[tokio::test]
    async fn aborting_an_upgraded_task_gives_both_places_back() {
        let room = Arc::new(tokio::sync::Semaphore::new(1));
        let per_ip = PerAddress::new(1);
        let (_tx, rx) = watch::channel(());
        let upgrades = Upgrades::default();
        let conn = Connection::new(
            room.clone().try_acquire_owned().unwrap(),
            per_ip.try_acquire("192.0.2.1".parse().unwrap()),
            upgrades.clone(),
            rx,
        );
        let slot = conn.slot();
        // the HTTP side of the connection ends; the task keeps the places
        drop(conn);
        slot.spawn(|_| std::future::pending());
        assert_eq!(room.available_permits(), 0);
        assert_eq!(per_ip.clients(), 1);
        let mut set = upgrades.take();
        set.abort_all();
        while set.join_next().await.is_some() {}
        assert_eq!(room.available_permits(), 1);
        assert_eq!(per_ip.clients(), 0);
    }

    #[test]
    fn an_address_may_hold_its_share_and_no_more_and_gives_places_back() {
        let p = PerAddress::new(2);
        let ip: IpAddr = "192.0.2.1".parse().unwrap();
        let other: IpAddr = "192.0.2.2".parse().unwrap();
        let a = p.try_acquire(ip).unwrap();
        let _b = p.try_acquire(ip).unwrap();
        assert!(p.try_acquire(ip).is_none());
        assert!(
            p.try_acquire(other).is_some(),
            "another address has its own share"
        );
        drop(a);
        assert!(p.try_acquire(ip).is_some());
    }

    #[test]
    fn the_table_forgets_an_address_whose_connections_are_all_gone() {
        let p = PerAddress::new(1);
        let permit = p.try_acquire("192.0.2.1".parse().unwrap()).unwrap();
        assert_eq!(p.clients(), 1);
        drop(permit);
        assert_eq!(p.clients(), 0);
    }

    #[test]
    fn an_ipv6_network_is_one_client_and_a_mapped_ipv4_address_is_the_ipv4_client() {
        let p = PerAddress::new(1);
        let _held = p.try_acquire("2001:db8::1".parse().unwrap()).unwrap();
        assert!(
            p.try_acquire("2001:db8::ffff".parse().unwrap()).is_none(),
            "same /64"
        );
        assert!(
            p.try_acquire("2001:db8:0:1::1".parse().unwrap()).is_some(),
            "next /64"
        );
        let _v4 = p.try_acquire("192.0.2.9".parse().unwrap()).unwrap();
        assert!(p.try_acquire("::ffff:192.0.2.9".parse().unwrap()).is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn a_write_that_makes_no_progress_fails_after_the_limit() {
        let (a, _b) = tokio::io::duplex(8);
        let mut w = WriteTimeout::new(a, Duration::from_secs(5));
        // 8 bytes fit, the rest waits for a reader that never comes
        let started = Instant::now();
        let err = w.write_all(&[0u8; 64]).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert_eq!(started.elapsed(), Duration::from_secs(5));
    }

    #[tokio::test(start_paused = true)]
    async fn a_write_whose_peer_keeps_taking_bytes_is_never_cut_off() {
        let (a, mut b) = tokio::io::duplex(8);
        let mut w = WriteTimeout::new(a, Duration::from_secs(5));
        let reader = tokio::spawn(async move {
            let mut got = 0;
            let mut buf = [0u8; 8];
            while got < 200 {
                // slower than the limit in total, faster than it per step
                tokio::time::sleep(Duration::from_secs(2)).await;
                got += b.read(&mut buf).await.unwrap();
            }
            got
        });
        w.write_all(&[1u8; 200]).await.unwrap();
        assert_eq!(reader.await.unwrap(), 200);
    }

    #[tokio::test(start_paused = true)]
    async fn an_idle_connection_is_not_a_stalled_write() {
        let (a, mut b) = tokio::io::duplex(64);
        let mut w = WriteTimeout::new(a, Duration::from_secs(5));
        w.write_all(b"one").await.unwrap();
        tokio::time::sleep(Duration::from_secs(60)).await;
        w.write_all(b"two").await.unwrap();
        let mut buf = [0u8; 6];
        b.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"onetwo");
    }
}
