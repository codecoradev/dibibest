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
    cache: Mutex<Option<(Discovery, Instant)>>,
    metadata: Mutex<Option<(Value, Instant)>>,
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
            cache: Mutex::new(None),
            metadata: Mutex::new(None),
        }
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
    /// The provider's own authorization server metadata (RFC 8414, or its OpenID configuration), unchanged, for
    /// the MCP origin to hand to clients. Kept for five minutes; if the provider cannot be reached the last copy is
    /// served, because a stale document beats none. `None` if there has never been one.
    pub async fn authorization_server_metadata(&self) -> Option<Value> {
        let mut cache = self.metadata.lock().await;
        if let Some((doc, at)) = cache.as_ref() {
            if at.elapsed() < self.metadata_ttl {
                return Some(doc.clone());
            }
        }
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
            let named = |k: &str| doc.get(k).is_some_and(Value::is_string);
            if named("issuer") && named("authorization_endpoint") {
                *cache = Some((doc.clone(), Instant::now()));
                return Some(doc);
            }
        }
        cache.as_ref().map(|(doc, _)| doc.clone())
    }
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
