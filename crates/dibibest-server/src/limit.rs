//! A token bucket per client address, in memory.
//!
//! Each client has a bucket of `per_minute` tokens that refills at `per_minute` a minute; a request takes one.
//! When there is none the client gets 429 and the time to wait. The table is bounded, so a flood of different
//! addresses cannot grow it without limit.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

/// The client an address belongs to (the key of a rate bucket, and of the count of open connections). A person with an IPv6 network owns a whole /64 (2^64 addresses) and could
/// otherwise take a fresh bucket by changing address on every request, so the network is the client. An
/// IPv4-mapped address is the same client as the IPv4 address.
pub fn network_key(ip: IpAddr) -> IpAddr {
    match ip.to_canonical() {
        IpAddr::V6(v6) => {
            let mut octets = [0u8; 16];
            octets[..8].copy_from_slice(&v6.octets()[..8]);
            IpAddr::V6(Ipv6Addr::from(octets))
        }
        v4 => v4,
    }
}

/// Most clients tracked at once.
const MAX_TRACKED: usize = 10_000;

struct Bucket {
    tokens: f64,
    last: Instant,
}

pub struct RateLimiter {
    capacity: f64,
    per_second: f64,
    buckets: Mutex<HashMap<IpAddr, Bucket>>,
}

impl RateLimiter {
    pub fn new(per_minute: u32) -> Self {
        let capacity = f64::from(per_minute.max(1));
        Self {
            capacity,
            per_second: capacity / 60.0,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Takes a token for `ip`, or says how long until there is one.
    pub fn check_at(&self, ip: IpAddr, now: Instant) -> Result<(), Duration> {
        let ip = network_key(ip);
        let mut buckets = self.buckets.lock().unwrap_or_else(PoisonError::into_inner);
        if buckets.len() >= MAX_TRACKED && !buckets.contains_key(&ip) {
            self.make_room(&mut buckets, now);
        }
        let b = buckets.entry(ip).or_insert(Bucket {
            tokens: self.capacity,
            last: now,
        });
        let elapsed = now.saturating_duration_since(b.last).as_secs_f64();
        b.tokens = (b.tokens + elapsed * self.per_second).min(self.capacity);
        b.last = b.last.max(now);
        if b.tokens >= 1.0 {
            b.tokens -= 1.0;
            Ok(())
        } else {
            Err(Duration::from_secs_f64((1.0 - b.tokens) / self.per_second))
        }
    }

    /// How many clients are tracked.
    pub fn tracked(&self) -> usize {
        self.buckets
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// A bucket that has refilled to the top looks like a new client, so it can go. If that is not enough,
    /// the half that was seen longest ago goes too.
    fn make_room(&self, buckets: &mut HashMap<IpAddr, Bucket>, now: Instant) {
        buckets.retain(|_, b| {
            let elapsed = now.saturating_duration_since(b.last).as_secs_f64();
            b.tokens + elapsed * self.per_second < self.capacity
        });
        if buckets.len() >= MAX_TRACKED {
            let mut seen: Vec<Instant> = buckets.values().map(|b| b.last).collect();
            seen.sort_unstable();
            let cut = seen[seen.len() / 2];
            buckets.retain(|_, b| b.last > cut);
        }
    }
}
