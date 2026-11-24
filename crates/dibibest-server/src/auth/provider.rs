//! OpenID Connect discovery for the identity provider.
//!
//! The endpoints come from the provider's own document, so each one is checked before it is used: the server
//! sends the client secret to the token endpoint and the visitor's browser to the authorization endpoint, and a
//! document that points either at plain HTTP or at an address with credentials in it is refused whole.

use super::http::{read_limited, MAX_BODY};
use crate::config::allowed_url;
use reqwest::Client;
use serde_json::Value;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::Instant;
use url::Url;

/// How long a discovery document is kept.
const DISCOVERY_TTL: Duration = Duration::from_secs(3600);
/// How long the provider's own metadata document is kept before it is fetched again.
const METADATA_TTL: Duration = Duration::from_secs(300);
/// After a refresh failed and the old copy was served instead, how long before the next attempt: a provider that
/// is down must not be asked again by every request.
const RETRY_AFTER_FAILURE: Duration = Duration::from_secs(30);
#[derive(Debug, Clone)]
pub struct Discovery {
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub userinfo_endpoint: String,
    pub jwks_uri: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderError {
    /// The provider could not be reached or answered with an error status. Worth trying again in a moment.
    Unreachable(String),
    /// It answered, but the document cannot be used.
    Invalid(String),
}

pub struct Provider {
    issuer: String,
    client: Client,
    ttl: Duration,
    metadata_ttl: Duration,
    retry_gap: Duration,
    cache: Mutex<Option<(Discovery, Instant)>>,
    metadata: Mutex<MetadataCache>,
}

/// The provider's metadata document, when it was fetched and when the next attempt may be made.
#[derive(Default)]
struct MetadataCache {
    doc: Option<Value>,
    fetched: Option<Instant>,
    /// Set after a failed attempt: no new attempt before this, so a provider that is down is not asked by every
    /// request, whether or not there is an old copy to serve meanwhile.
    retry_not_before: Option<Instant>,
}

impl Provider {
    pub fn new(issuer: &str, client: Client) -> Self {
        Self::with_ttls(issuer, client, DISCOVERY_TTL, METADATA_TTL)
    }

    /// One lifetime for both the discovery document and the provider's metadata (for tests).
    pub fn with_ttl(issuer: &str, client: Client, ttl: Duration) -> Self {
        Self::with_ttls(issuer, client, ttl, ttl)
    }

    pub fn with_ttls(issuer: &str, client: Client, ttl: Duration, metadata_ttl: Duration) -> Self {
        Self {
            issuer: issuer.trim_end_matches('/').to_string(),
            client,
            ttl,
            metadata_ttl,
            retry_gap: RETRY_AFTER_FAILURE,
            cache: Mutex::new(None),
            metadata: Mutex::new(MetadataCache::default()),
        }
    }

    /// How long to wait after a failed metadata refresh before trying again (for tests).
    pub fn with_retry_gap(mut self, gap: Duration) -> Self {
        self.retry_gap = gap;
        self
    }

    pub async fn discovery(&self) -> Result<Discovery, ProviderError> {
        // Held across the fetch on purpose: concurrent callers wait for one fetch instead of making one each.
        let mut cache = self.cache.lock().await;
        if let Some((d, at)) = cache.as_ref() {
            if at.elapsed() < self.ttl {
                return Ok(d.clone());
            }
        }
        let unreachable = |why: String| ProviderError::Unreachable(why);
        let response = self
            .client
            .get(format!("{}/.well-known/openid-configuration", self.issuer))
            .header("accept", "application/json")
            .send()
            .await
            .map_err(|e| unreachable(e.to_string()))?;
        if !response.status().is_success() {
            return Err(unreachable(format!(
                "discovery answered {}",
                response.status()
            )));
        }
        let body = read_limited(response, MAX_BODY)
            .await
            .map_err(unreachable)?;
        let doc: Value = serde_json::from_str(&body)
            .map_err(|_| ProviderError::Invalid("the discovery document is not JSON".into()))?;
        let found = parse(&doc)?;
        *cache = Some((found.clone(), Instant::now()));
        Ok(found)
    }
}

impl Provider {
    /// Whether `doc` describes this provider: it names this issuer (RFC 8414 section 3.3, a trailing slash aside)
    /// and every address it gives for an endpoint or key set is one the server itself would use. Documentation and
    /// policy links (`service_documentation`, `op_policy_uri`, ...) are not endpoints and are left alone.
    fn is_ours(&self, doc: &Value) -> bool {
        let issuer_matches = doc
            .get("issuer")
            .and_then(Value::as_str)
            .is_some_and(|i| i.trim_end_matches('/') == self.issuer);
        let Some(fields) = doc.as_object() else {
            return false;
        };
        issuer_matches
            && fields.iter().all(|(name, value)| match name.as_str() {
                // RFC 8705: the same endpoints under their mutual-TLS addresses
                "mtls_endpoint_aliases" => value
                    .as_object()
                    .is_some_and(|aliases| aliases.values().all(usable_address)),
                n if n.ends_with("_endpoint") || n == "jwks_uri" => usable_address(value),
                _ => true,
            })
    }

    /// The provider's own authorization server metadata (RFC 8414, or its OpenID configuration), unchanged, for
    /// the MCP origin to hand to clients. Kept for five minutes. When a refresh fails the last copy is served, since
    /// a stale document beats none, and the next attempt waits `retry_gap` whether or not there is a copy. `None`
    /// if there has never been one.
    pub async fn authorization_server_metadata(&self) -> Option<Value> {
        // Held across the fetch on purpose, as for discovery: concurrent callers wait for one attempt.
        let mut cache = self.metadata.lock().await;
        let fresh = cache
            .fetched
            .is_some_and(|at| at.elapsed() < self.metadata_ttl);
        let waiting = cache.retry_not_before.is_some_and(|t| Instant::now() < t);
        if fresh || waiting {
            return cache.doc.clone();
        }
        match self.fetch_metadata().await {
            Some(doc) => {
                cache.doc = Some(doc.clone());
                cache.fetched = Some(Instant::now());
                Some(doc)
            }
            None => {
                tracing::warn!(
                    issuer = %self.issuer,
                    serving_old_copy = cache.doc.is_some(),
                    "the identity provider's metadata could not be refreshed"
                );
                cache.retry_not_before = Some(Instant::now() + self.retry_gap);
                cache.doc.clone()
            }
        }
    }

    /// The first of the two well-known documents that answers with something this server may pass on.
    async fn fetch_metadata(&self) -> Option<Value> {
        for name in ["oauth-authorization-server", "openid-configuration"] {
            let Ok(response) = self
                .client
                .get(format!("{}/.well-known/{name}", self.issuer))
                .header("accept", "application/json")
                .send()
                .await
            else {
                continue;
            };
            if !response.status().is_success() {
                continue;
            }
            let Ok(body) = read_limited(response, MAX_BODY).await else {
                continue;
            };
            let Ok(doc) = serde_json::from_str::<Value>(&body) else {
                continue;
            };
            if doc
                .get("authorization_endpoint")
                .is_some_and(Value::is_string)
                && self.is_ours(&doc)
            {
                return Some(doc);
            }
        }
        None
    }
}

fn usable_address(value: &Value) -> bool {
    value
        .as_str()
        .and_then(|u| Url::parse(u).ok())
        .is_some_and(|u| allowed_url(&u))
}

fn parse(doc: &Value) -> Result<Discovery, ProviderError> {
    let field = |name: &str| -> Result<Option<String>, ProviderError> {
        let Some(v) = doc.get(name).and_then(Value::as_str) else {
            return Ok(None);
        };
        let ok = Url::parse(v).is_ok_and(|u| allowed_url(&u));
        if ok {
            Ok(Some(v.to_string()))
        } else {
            Err(ProviderError::Invalid(format!(
                "{name} is not an address this server may use"
            )))
        }
    };
    let required = |name: &str| {
        field(name)?
            .ok_or_else(|| ProviderError::Invalid(format!("the discovery document has no {name}")))
    };
    Ok(Discovery {
        authorization_endpoint: required("authorization_endpoint")?,
        token_endpoint: required("token_endpoint")?,
        userinfo_endpoint: required("userinfo_endpoint")?,
        jwks_uri: field("jwks_uri")?,
    })
}
