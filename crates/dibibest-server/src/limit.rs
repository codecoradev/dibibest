//! A token bucket per client address, in memory.
//!
//! Each client has a bucket of `per_minute` tokens that refills at `per_minute` a minute; a request takes one.
//! When there is none the client gets 429 and the time to wait. The table is bounded, so a flood of different
//! addresses cannot grow it without limit.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

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
