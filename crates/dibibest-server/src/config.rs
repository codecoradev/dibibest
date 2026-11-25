//! Configuration, from environment variables only. Nothing is read from the repository at run time.
//!
//! A blank variable counts as unset (a compose file often passes an empty value). A bad value is an error that
//! names the variable; it is never replaced by the default.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;
use url::{Host, Url};

/// How the client proves itself at the token endpoint (`DIBIBEST_OIDC_TOKEN_AUTH`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenAuth {
    /// HTTP Basic with the form-encoded id and secret, RFC 6749 section 2.3.1. The default.
    Basic,
    /// `client_id` and `client_secret` in the request body.
    Post,
}

/// Sign-in through an OpenID Connect provider (Gatehouse). Present only when every setting is.
#[derive(Clone)]
pub struct Oidc {
    /// `DIBIBEST_OIDC_ISSUER`, without a trailing slash.
    pub issuer: String,
    /// `DIBIBEST_OIDC_CLIENT_ID`.
    pub client_id: String,
    /// `DIBIBEST_OIDC_CLIENT_SECRET`.
    pub client_secret: String,
    /// `DIBIBEST_SESSION_SECRET`: signs the login handshake and the session cookie. At least 32 characters.
    pub session_secret: String,
    /// `DIBIBEST_OIDC_TOKEN_AUTH`.
    pub token_auth: TokenAuth,
}

// The secrets are left out of the debug output: a config that is logged must not leak them.
impl std::fmt::Debug for Oidc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Oidc")
            .field("issuer", &self.issuer)
            .field("client_id", &self.client_id)
            .field("client_secret", &"[redacted]")
            .field("session_secret", &"[redacted]")
            .field("token_auth", &self.token_auth)
            .finish()
    }
}

/// Whether the server may talk to `url`, or hand a person's browser to it: HTTPS, or plain HTTP to this machine
/// (for development and tests), and no credentials in the address. A provider that points the token endpoint at
/// `http://elsewhere` must not make the server send the client secret there.
pub fn allowed_url(url: &Url) -> bool {
    let loopback = match url.host() {
        Some(Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        None => return false,
    };
    (url.scheme() == "https" || (url.scheme() == "http" && loopback))
        && url.username().is_empty()
        && url.password().is_none()
}

#[derive(Debug, Clone)]
pub struct Config {
    /// `DIBIBEST_ADDR`. Loopback by default: exposing the server is a choice.
    pub addr: SocketAddr,
    /// `DIBIBEST_DATA_DIR`: the database and the lock file live here. Use a local disk or a named volume.
    pub data_dir: PathBuf,
    /// `DIBIBEST_WEB_DIR`: the built web app (the output of `vite build`).
    pub web_dir: PathBuf,
    /// `DIBIBEST_TRUSTED_PROXIES`: how many reverse proxies stand in front of the server (0 to 16). With N, the
    /// client is the Nth entry from the end of `X-Forwarded-For`: the proxies appended those entries, everything
    /// before them was written by the client. 0, the default, ignores the header, because a client can send it.
    pub trusted_proxies: usize,
    /// `DIBIBEST_RATE_PER_MINUTE`: requests per client per minute (also the burst size).
    pub rate_per_minute: u32,
    /// `DIBIBEST_MAX_BODY_BYTES`: largest request body.
    pub max_body_bytes: usize,
    /// `DIBIBEST_SHUTDOWN_GRACE_SECS`: how long a shutdown waits for requests in flight.
    pub shutdown_grace: Duration,
    /// `DIBIBEST_HEADER_TIMEOUT_SECS`: how long a connection has to deliver the headers of a request.
    pub header_timeout: Duration,
    /// `DIBIBEST_REQUEST_TIMEOUT_SECS`: how long a request may take, body and handler together.
    pub request_timeout: Duration,
    /// `DIBIBEST_BACKUP_DIR`: where scheduled backups of the database are written. Unset, there are none. Keep it
    /// off the data volume (a host directory mounted into the container, or another disk).
    pub backup_dir: Option<PathBuf>,
    /// `DIBIBEST_BACKUP_INTERVAL_HOURS`: how often (1 to 8760).
    pub backup_interval: Duration,
    /// `DIBIBEST_BACKUP_KEEP`: how many backups are kept, the newest (1 to 1000).
    pub backup_keep: usize,
    /// `DIBIBEST_MAX_CONNECTIONS`: how many connections are served at once. Past it the server stops accepting,
    /// and further connections wait in the queue of the operating system until one closes.
    pub max_connections: usize,
    /// `DIBIBEST_WRITE_TIMEOUT_SECS`: how long a write to a client may make no progress at all (1 to 86400). A client
    /// that never reads its response is cut off after it.
    pub write_timeout: Duration,
    /// `DIBIBEST_MAX_CONNECTIONS_PER_IP`: how many connections one address (an IPv6 network counts as one) may hold
    /// at once; further ones are closed at once. 0, the default, is no limit. Refused together with
    /// `DIBIBEST_TRUSTED_PROXIES`, because behind a proxy every connection comes from the proxy.
    pub max_connections_per_ip: usize,
    /// `DIBIBEST_PUBLIC_URL`: the origin people use to reach the server, such as `https://dibibest.codecora.dev`.
    /// The redirect address registered with the provider, the `Secure` flag of cookies and the same-origin check
    /// all come from it, never from the `Host` header, which a client can write. Needed when sign-in is on.
    pub public_url: Option<String>,
    /// `DIBIBEST_OIDC_ISSUER` on its own: the identity provider the MCP endpoint trusts, without sign-in for
    /// people. Without trailing slash. [`Config::issuer`] also answers it when sign-in is on.
    pub issuer: Option<String>,
    /// Sign-in, or `None` when it is not configured.
    pub oidc: Option<Oidc>,
}

impl Config {
    /// The identity provider, from the issuer setting or from sign-in.
    pub fn issuer(&self) -> Option<&str> {
        self.issuer
            .as_deref()
            .or(self.oidc.as_ref().map(|o| o.issuer.as_str()))
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            addr: SocketAddr::from(([127, 0, 0, 1], 8080)),
            data_dir: PathBuf::from("./data"),
            web_dir: PathBuf::from("./web"),
            trusted_proxies: 0,
            rate_per_minute: 600,
            max_body_bytes: 1024 * 1024,
            shutdown_grace: Duration::from_secs(20),
            // Longer than the 90 s a reverse proxy such as Traefik keeps an idle connection for: the limit also ends
            // an idle kept-alive connection, and one closed under a proxy that still means to reuse it fails the next
            // request sent on it.
            header_timeout: Duration::from_secs(120),
            request_timeout: Duration::from_secs(30),
            max_connections: 1024,
            write_timeout: Duration::from_secs(60),
            max_connections_per_ip: 0,
            backup_dir: None,
            backup_interval: Duration::from_secs(24 * 3_600),
            backup_keep: 7,
            public_url: None,
            issuer: None,
            oidc: None,
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
        // A value that is only a placeholder (`...`, `xxx`, `changeme`, `todo`) counts as not set, so a copied
        // example file does not switch sign-in on with a fake secret.
        let real = |var: &'static str| {
            value(var)
                .map(|v| v.trim().to_string())
                .filter(|v| !is_placeholder(v))
        };
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
        let trusted_proxies = match number("DIBIBEST_TRUSTED_PROXIES")? {
            None => d.trusted_proxies,
            Some(n) if n <= 16 => n as usize,
            Some(_) => {
                return Err(ConfigError {
                    var: "DIBIBEST_TRUSTED_PROXIES",
                    reason: "more than 16 proxies in front is not believable".into(),
                });
            }
        };
        let public_url = real("DIBIBEST_PUBLIC_URL")
            .map(|v| parse_public_url(&v))
            .transpose()?;
        let (issuer, oidc) = parse_oidc(&real)?;
        if (issuer.is_some() || oidc.is_some()) && public_url.is_none() {
            return Err(ConfigError {
                var: "DIBIBEST_PUBLIC_URL",
                reason: "needed when an identity provider is configured: the redirect address, the cookies and the MCP resource URL depend on it".into(),
            });
        }
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
        // A limit in seconds: 1 to a day. 0 would switch the limit off or close every connection at once, and a
        // huge number is a limit nobody will meet; neither is a thing to guess the meaning of.
        let seconds = |var: &'static str, default: Duration| -> Result<Duration, ConfigError> {
            match number(var)? {
                None => Ok(default),
                Some(n @ 1..=86_400) => Ok(Duration::from_secs(n)),
                Some(_) => Err(ConfigError {
                    var,
                    reason: "must be between 1 and 86400 seconds".into(),
                }),
            }
        };
        let in_range =
            |var: &'static str, min: u64, max: u64| -> Result<Option<u64>, ConfigError> {
                match number(var)? {
                    Some(n) if !(min..=max).contains(&n) => Err(ConfigError {
                        var,
                        reason: format!("must be between {min} and {max}"),
                    }),
                    other => Ok(other),
                }
            };
        let max_connections_per_ip = in_range("DIBIBEST_MAX_CONNECTIONS_PER_IP", 0, 1_000_000)?
            .map(|n| to_usize("DIBIBEST_MAX_CONNECTIONS_PER_IP", n))
            .transpose()?
            .unwrap_or(d.max_connections_per_ip);
        if max_connections_per_ip > 0 && trusted_proxies > 0 {
            return Err(ConfigError {
                var: "DIBIBEST_MAX_CONNECTIONS_PER_IP",
                reason: "cannot be used with DIBIBEST_TRUSTED_PROXIES above 0: behind a reverse proxy every connection comes from the proxy's own address, so the cap would limit the proxy and with it every client; set it to 0 and let the proxy limit connections per client".into(),
            });
        }
        let backup_dir = value("DIBIBEST_BACKUP_DIR").map(|v| PathBuf::from(v.trim()));
        let backup_hours = in_range("DIBIBEST_BACKUP_INTERVAL_HOURS", 1, 8_760)?;
        let backup_keep = in_range("DIBIBEST_BACKUP_KEEP", 1, 1_000)?
            .map(|n| to_usize("DIBIBEST_BACKUP_KEEP", n))
            .transpose()?;
        if backup_dir.is_none() && (backup_hours.is_some() || backup_keep.is_some()) {
            return Err(ConfigError {
                var: "DIBIBEST_BACKUP_DIR",
                reason: "needed when DIBIBEST_BACKUP_INTERVAL_HOURS or DIBIBEST_BACKUP_KEEP is set: without a directory there are no backups".into(),
            });
        }
        Ok(Self {
            addr: parsed("DIBIBEST_ADDR")?.unwrap_or(d.addr),
            data_dir: value("DIBIBEST_DATA_DIR").map_or(d.data_dir, PathBuf::from),
            web_dir: value("DIBIBEST_WEB_DIR").map_or(d.web_dir, PathBuf::from),
            trusted_proxies,
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
            header_timeout: seconds("DIBIBEST_HEADER_TIMEOUT_SECS", d.header_timeout)?,
            request_timeout: seconds("DIBIBEST_REQUEST_TIMEOUT_SECS", d.request_timeout)?,
            backup_dir,
            backup_interval: match backup_hours {
                None => d.backup_interval,
                Some(h) => Duration::from_secs(h * 3_600),
            },
            backup_keep: backup_keep.unwrap_or(d.backup_keep),
            max_connections: match number("DIBIBEST_MAX_CONNECTIONS")? {
                None => d.max_connections,
                Some(n @ 1..=1_000_000) => to_usize("DIBIBEST_MAX_CONNECTIONS", n)?,
                Some(_) => {
                    return Err(ConfigError {
                        var: "DIBIBEST_MAX_CONNECTIONS",
                        reason: "must be between 1 and 1000000".into(),
                    })
                }
            },
            write_timeout: seconds("DIBIBEST_WRITE_TIMEOUT_SECS", d.write_timeout)?,
            max_connections_per_ip,
            public_url,
            issuer,
            oidc,
        })
    }
}

fn is_placeholder(v: &str) -> bool {
    let lower = v.to_ascii_lowercase();
    lower == "..." || lower == "changeme" || lower == "todo" || lower.chars().all(|c| c == 'x')
}

/// `https://host[:port]` with nothing after it, as an origin without a trailing slash.
fn parse_public_url(v: &str) -> Result<String, ConfigError> {
    const VAR: &str = "DIBIBEST_PUBLIC_URL";
    let fail = |reason: &str| ConfigError {
        var: VAR,
        reason: reason.into(),
    };
    let url = Url::parse(v).map_err(|e| fail(&format!("not an address: {e}")))?;
    if !allowed_url(&url) {
        return Err(fail(
            "must be https (http is allowed only for localhost) and carry no credentials",
        ));
    }
    if (url.path() != "/" && !url.path().is_empty())
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(fail("must be an origin, such as https://dibibest.codecora.dev, with no path, query or fragment"));
    }
    Ok(url.origin().ascii_serialization())
}

/// The identity provider and sign-in. The issuer alone is valid: it is all the MCP endpoint needs. The three
/// settings that make sign-in for people (client id, client secret, session secret) come together with it or not at
/// all: a half is a mistake worth stopping for, not a reason to quietly leave sign-in off.
fn parse_oidc(
    real: &dyn Fn(&'static str) -> Option<String>,
) -> Result<(Option<String>, Option<Oidc>), ConfigError> {
    const ISSUER: &str = "DIBIBEST_OIDC_ISSUER";
    const SESSION: &str = "DIBIBEST_SESSION_SECRET";
    let issuer = real(ISSUER)
        .map(|raw| {
            let bad = |reason: &str| ConfigError { var: ISSUER, reason: reason.into() };
            let url = Url::parse(&raw).map_err(|e| bad(&format!("not an address: {e}")))?;
            if !allowed_url(&url) || url.query().is_some() || url.fragment().is_some() {
                return Err(bad("must be an https address (http is allowed only for localhost) with no credentials, query or fragment"));
            }
            Ok(raw.trim_end_matches('/').to_string())
        })
        .transpose()?;
    let signin = [
        "DIBIBEST_OIDC_CLIENT_ID",
        "DIBIBEST_OIDC_CLIENT_SECRET",
        SESSION,
    ];
    let values: Vec<Option<String>> = signin.iter().map(|v| real(v)).collect();
    if values.iter().all(Option::is_none) {
        return Ok((issuer, None));
    }
    // some sign-in setting is present, so all of them and the issuer are needed
    if issuer.is_none() {
        return Err(ConfigError {
            var: ISSUER,
            reason: "sign-in is configured only in part: the issuer is missing".into(),
        });
    }
    for (var, v) in signin.iter().zip(&values) {
        if v.is_none() {
            return Err(ConfigError {
                var,
                reason: "sign-in is configured only in part: set the client id, the client secret and the session secret together (the issuer alone is enough for the MCP endpoint)".into(),
            });
        }
    }
    let [client_id, client_secret, session_secret] =
        <[String; 3]>::try_from(values.into_iter().flatten().collect::<Vec<_>>())
            .expect("three values were checked");
    if session_secret.len() < 32 {
        return Err(ConfigError {
            var: SESSION,
            reason: "must be at least 32 characters, for example the output of `openssl rand -base64 32`".into(),
        });
    }
    let token_auth = match real("DIBIBEST_OIDC_TOKEN_AUTH").as_deref() {
        None | Some("basic") => TokenAuth::Basic,
        Some("post") => TokenAuth::Post,
        Some(other) => {
            return Err(ConfigError {
                var: "DIBIBEST_OIDC_TOKEN_AUTH",
                reason: format!("`{other}` is not basic or post"),
            });
        }
    };
    let issuer = issuer.expect("checked above");
    Ok((
        Some(issuer.clone()),
        Some(Oidc {
            issuer,
            client_id,
            client_secret,
            session_secret,
            token_auth,
        }),
    ))
}
