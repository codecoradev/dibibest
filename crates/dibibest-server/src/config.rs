//! Configuration, from environment variables only. Nothing is read from the repository at run time.
//!
//! A blank variable counts as unset (a compose file often passes an empty value). A bad value is an error that
//! names the variable; it is never replaced by the default.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Config {
    /// `DIBIBEST_ADDR`. Loopback by default: exposing the server is a choice.
    pub addr: SocketAddr,
    /// `DIBIBEST_DATA_DIR`: the database and the lock file live here. Use a local disk or a named volume.
    pub data_dir: PathBuf,
    /// `DIBIBEST_WEB_DIR`: the built web app (the output of `vite build`).
    pub web_dir: PathBuf,
    /// `DIBIBEST_TRUST_PROXY`: read the client address from the last `X-Forwarded-For` entry, the one the
    /// reverse proxy appended. Off by default, because a client can send that header itself.
    pub trust_proxy: bool,
    /// `DIBIBEST_RATE_PER_MINUTE`: requests per client per minute (also the burst size).
    pub rate_per_minute: u32,
    /// `DIBIBEST_MAX_BODY_BYTES`: largest request body.
    pub max_body_bytes: usize,
    /// `DIBIBEST_SHUTDOWN_GRACE_SECS`: how long a shutdown waits for requests in flight.
    pub shutdown_grace: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            addr: SocketAddr::from(([127, 0, 0, 1], 8080)),
            data_dir: PathBuf::from("./data"),
            web_dir: PathBuf::from("./web"),
            trust_proxy: false,
            rate_per_minute: 600,
            max_body_bytes: 1024 * 1024,
            shutdown_grace: Duration::from_secs(20),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{var}: {reason}")]
pub struct ConfigError {
    pub var: &'static str,
    pub reason: String,
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Reads settings through `get`, so a test does not have to touch the process environment.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let d = Self::default();
        let value = |var: &'static str| get(var).filter(|v| !v.trim().is_empty());
        let parsed = |var: &'static str| -> Result<Option<_>, ConfigError> {
            value(var)
                .map(|v| {
                    v.trim()
                        .parse()
                        .map_err(|e: <SocketAddr as FromStr>::Err| ConfigError {
                            var,
                            reason: e.to_string(),
                        })
                })
                .transpose()
        };
        let number = |var: &'static str| -> Result<Option<u64>, ConfigError> {
            value(var)
                .map(|v| {
                    v.trim().parse::<u64>().map_err(|e| ConfigError {
                        var,
                        reason: format!("{e} (a whole number is expected)"),
                    })
                })
                .transpose()
        };
        let rate = number("DIBIBEST_RATE_PER_MINUTE")?;
        if rate == Some(0) {
            return Err(ConfigError {
                var: "DIBIBEST_RATE_PER_MINUTE",
                reason: "must be at least 1".into(),
            });
        }
        let trust_proxy = match value("DIBIBEST_TRUST_PROXY") {
            None => d.trust_proxy,
            Some(v) => match v.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" => true,
                "0" | "false" | "no" => false,
                other => {
                    return Err(ConfigError {
                        var: "DIBIBEST_TRUST_PROXY",
                        reason: format!("`{other}` is not true or false"),
                    });
                }
            },
        };
        let to_u32 = |var: &'static str, n: u64| {
            u32::try_from(n).map_err(|_| ConfigError {
                var,
                reason: "too large".into(),
            })
        };
        let to_usize = |var: &'static str, n: u64| {
            usize::try_from(n).map_err(|_| ConfigError {
                var,
                reason: "too large".into(),
            })
        };
        Ok(Self {
            addr: parsed("DIBIBEST_ADDR")?.unwrap_or(d.addr),
            data_dir: value("DIBIBEST_DATA_DIR").map_or(d.data_dir, PathBuf::from),
            web_dir: value("DIBIBEST_WEB_DIR").map_or(d.web_dir, PathBuf::from),
            trust_proxy,
            rate_per_minute: match rate {
                Some(n) => to_u32("DIBIBEST_RATE_PER_MINUTE", n)?,
                None => d.rate_per_minute,
            },
            max_body_bytes: match number("DIBIBEST_MAX_BODY_BYTES")? {
                Some(n) => to_usize("DIBIBEST_MAX_BODY_BYTES", n)?,
                None => d.max_body_bytes,
            },
            shutdown_grace: number("DIBIBEST_SHUTDOWN_GRACE_SECS")?
                .map_or(d.shutdown_grace, Duration::from_secs),
        })
    }
}
