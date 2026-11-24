//! Access-token verification against the identity provider's JWKS.
//!
//! Gatehouse issues RFC 9068 JWT access tokens when the client names a resource (RFC 8707). They are verified
//! here, locally, so no call to the provider is made per request. A token is accepted only if it carries an
//! allowed algorithm (RS256, PS256 or ES256), a signature from a key the provider lists, the expected issuer and
//! audience, a valid time window, a subject, and no `cnf` (a DPoP-bound token needs a proof this server does not
//! check, so it is refused as a bearer token).
//!
//! The allow-list of algorithms is the server's own: a provider whose keys do not list an `alg` must not let a
//! token choose `none` or `HS256`.

use super::codec::b64url_decode;
use ring::signature::{self, RsaPublicKeyComponents, UnparsedPublicKey};
use serde::Deserialize;
use serde_json::Value;
use std::future::Future;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::Instant;

/// Clock skew tolerated on `exp`, `nbf` and `iat`.
const LEEWAY_S: i64 = 60;
/// How long fetched keys are kept.
const JWKS_TTL: Duration = Duration::from_secs(3600);
/// An unknown `kid` fetches the keys again (the provider rotated), but not more often than this.
const JWKS_REFETCH: Duration = Duration::from_secs(60);

/// Where the keys come from. The server uses HTTP; tests use a canned answer.
pub trait JwksSource: Send + Sync {
    fn fetch(&self, uri: &str) -> impl Future<Output = Result<String, String>> + Send;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenError {
    /// The token is not acceptable. The caller answers 401 `invalid_token`.
    Invalid(String),
    /// The keys could not be had right now, so nothing can be said about the token. The caller answers 503.
    Unavailable(String),
}

impl std::fmt::Display for TokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(why) | Self::Unavailable(why) => f.write_str(why),
        }
    }
}

pub struct VerifyOptions<'a> {
    pub issuer: &'a str,
    /// The resource the token must be meant for (the MCP URL).
    pub audience: &'a str,
    pub jwks_uri: &'a str,
    /// Unix seconds. A parameter so a test can fix the clock.
    pub now: i64,
}

#[derive(Debug, Clone)]
pub struct Claims {
    pub iss: String,
    pub sub: String,
    pub aud: Vec<String>,
    pub exp: i64,
    pub scope: Option<String>,
    pub client_id: Option<String>,
    pub azp: Option<String>,
}

impl Claims {
    pub fn audiences(&self) -> &[String] {
        &self.aud
    }
}

#[derive(Deserialize, Clone)]
struct Jwk {
    kty: Option<String>,
    kid: Option<String>,
    alg: Option<String>,
    #[serde(rename = "use")]
    use_: Option<String>,
    n: Option<String>,
    e: Option<String>,
    crv: Option<String>,
    x: Option<String>,
    y: Option<String>,
}

#[derive(Deserialize)]
struct JwkSet {
    keys: Vec<Jwk>,
}

struct Cached {
    uri: String,
    keys: Vec<Jwk>,
    at: Instant,
}

pub struct Verifier<S> {
    source: S,
    cache: Mutex<Option<Cached>>,
}

fn invalid(why: impl Into<String>) -> TokenError {
    TokenError::Invalid(why.into())
}

/// A JWT segment as JSON.
fn segment(part: &str) -> Result<Value, TokenError> {
    b64url_decode(part)
        .and_then(|raw| serde_json::from_slice(&raw).ok())
        .ok_or_else(|| invalid("malformed token"))
}

impl<S: JwksSource> Verifier<S> {
    pub fn new(source: S) -> Self {
        Self {
            source,
            cache: Mutex::new(None),
        }
    }

    /// The provider's keys. Without `refresh` they are served for an hour; with it (an unknown `kid`) they are
    /// served for a minute, so a stream of tokens with made-up key ids cannot make this fetch on every request.
    async fn keys(&self, uri: &str, refresh: bool) -> Result<Vec<Jwk>, TokenError> {
        let ttl = if refresh { JWKS_REFETCH } else { JWKS_TTL };
        // The lock is held across the fetch on purpose: concurrent requests wait for one fetch instead of
        // making one each.
        let mut cache = self.cache.lock().await;
        if let Some(c) = cache.as_ref() {
            if c.uri == uri && c.at.elapsed() < ttl {
                return Ok(c.keys.clone());
            }
        }
        let body =
            self.source.fetch(uri).await.map_err(|e| {
                TokenError::Unavailable(format!("the keys could not be fetched: {e}"))
            })?;
        let set: JwkSet = serde_json::from_str(&body)
            .map_err(|_| TokenError::Unavailable("the key set is not valid".into()))?;
        *cache = Some(Cached {
            uri: uri.to_string(),
            keys: set.keys.clone(),
            at: Instant::now(),
        });
        Ok(set.keys)
    }

    /// The claims when `token` is acceptable for `opts`.
    pub async fn verify(
        &self,
        token: &str,
        opts: &VerifyOptions<'_>,
    ) -> Result<Claims, TokenError> {
        let parts: Vec<&str> = token.split('.').collect();
        let [head, payload, sig] = parts[..] else {
            return Err(invalid("not a JWT"));
        };
        let header = segment(head)?;
        let alg_name = header
            .get("alg")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let alg = Alg::from_name(alg_name)
            .ok_or_else(|| invalid(format!("unsupported alg {alg_name}")))?;
        if header.get("crit").is_some() {
            return Err(invalid("unsupported crit header"));
        }
        let kid = header.get("kid").and_then(Value::as_str);
        let sig = b64url_decode(sig).ok_or_else(|| invalid("malformed signature"))?;

        let pick = |keys: &[Jwk]| -> Vec<Jwk> {
            keys.iter()
                .filter(|k| {
                    kid.is_none_or(|kid| k.kid.as_deref() == Some(kid))
                        && k.alg.as_deref().is_none_or(|a| a == alg_name)
                        && k.use_.as_deref().is_none_or(|u| u == "sig")
                })
                .cloned()
                .collect()
        };
        let mut candidates = pick(&self.keys(opts.jwks_uri, false).await?);
        if candidates.is_empty() {
            candidates = pick(&self.keys(opts.jwks_uri, true).await?);
        }
        if candidates.is_empty() {
            return Err(invalid("no matching key"));
        }
        let signed = format!("{head}.{payload}");
        if !candidates
            .iter()
            .any(|k| alg.verify(k, signed.as_bytes(), &sig))
        {
            return Err(invalid("bad signature"));
        }
        check_claims(&segment(payload)?, opts)
    }
}

fn check_claims(c: &Value, opts: &VerifyOptions<'_>) -> Result<Claims, TokenError> {
    let number = |name: &str| c.get(name).and_then(Value::as_f64).map(|f| f as i64);
    let text = |name: &str| c.get(name).and_then(Value::as_str).map(str::to_string);

    let iss = text("iss").unwrap_or_default();
    if iss != opts.issuer {
        return Err(invalid("wrong issuer"));
    }
    let aud: Vec<String> = match c.get("aud") {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    };
    if !aud.iter().any(|a| a == opts.audience) {
        return Err(invalid("wrong audience"));
    }
    let Some(exp) = number("exp").filter(|exp| exp + LEEWAY_S > opts.now) else {
        return Err(invalid("expired"));
    };
    if number("nbf").is_some_and(|nbf| nbf - LEEWAY_S > opts.now) {
        return Err(invalid("not yet valid"));
    }
    if number("iat").is_some_and(|iat| iat - LEEWAY_S > opts.now) {
        return Err(invalid("issued in the future"));
    }
    let Some(sub) = text("sub").filter(|s| !s.is_empty()) else {
        return Err(invalid("no subject"));
    };
    // DPoP-bound tokens (RFC 9449) need a proof this server does not check: refused as bearer tokens.
    if c.get("cnf").is_some() {
        return Err(invalid("sender-constrained token"));
    }
    Ok(Claims {
        iss,
        sub,
        aud,
        exp,
        scope: text("scope"),
        client_id: text("client_id"),
        azp: text("azp"),
    })
}

#[derive(Clone, Copy)]
enum Alg {
    Rs256,
    Ps256,
    Es256,
}

impl Alg {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "RS256" => Some(Self::Rs256),
            "PS256" => Some(Self::Ps256),
            "ES256" => Some(Self::Es256),
            _ => None,
        }
    }

    /// Whether `sig` is a valid signature of `msg` by `key`. A key of the wrong type is simply not a match.
    fn verify(self, key: &Jwk, msg: &[u8], sig: &[u8]) -> bool {
        let part = |v: &Option<String>| v.as_deref().and_then(b64url_decode);
        match self {
            Self::Rs256 | Self::Ps256 => {
                if key.kty.as_deref() != Some("RSA") {
                    return false;
                }
                let (Some(n), Some(e)) = (part(&key.n), part(&key.e)) else {
                    return false;
                };
                let params: &signature::RsaParameters = match self {
                    Self::Rs256 => &signature::RSA_PKCS1_2048_8192_SHA256,
                    _ => &signature::RSA_PSS_2048_8192_SHA256,
                };
                RsaPublicKeyComponents { n, e }
                    .verify(params, msg, sig)
                    .is_ok()
            }
            Self::Es256 => {
                if key.kty.as_deref() != Some("EC") || key.crv.as_deref() != Some("P-256") {
                    return false;
                }
                let (Some(x), Some(y)) = (part(&key.x), part(&key.y)) else {
                    return false;
                };
                if x.len() > 32 || y.len() > 32 {
                    return false;
                }
                // uncompressed point: 0x04, then x and y as 32-byte big-endian numbers
                let mut point = vec![4u8];
                point.extend(std::iter::repeat_n(0, 32 - x.len()));
                point.extend(&x);
                point.extend(std::iter::repeat_n(0, 32 - y.len()));
                point.extend(&y);
                UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_FIXED, point)
                    .verify(msg, sig)
                    .is_ok()
            }
        }
    }
}
