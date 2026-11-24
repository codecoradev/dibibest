//! The HTTP client the server uses to talk to the identity provider, and the key source built on it.
//!
//! The provider is another party's server, so the client does not trust it: it never follows a redirect, every
//! request has a timeout, and an answer is read only up to [`MAX_BODY`] bytes, so a huge or endless one cannot
//! use up memory.

use super::jwt::JwksSource;
use crate::config::allowed_url;
use reqwest::redirect::Policy;
use reqwest::{Client, Response};
use std::time::Duration;

/// The largest answer read from the provider: discovery, keys, token and profile are all far smaller.
pub const MAX_BODY: usize = 256 * 1024;

pub fn client(timeout: Duration) -> Client {
    Client::builder()
        .timeout(timeout)
        .connect_timeout(timeout.min(Duration::from_secs(5)))
        .redirect(Policy::none())
        .user_agent(concat!("dibibest/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("an HTTP client can be built")
}

/// The body as text, or an error if it is larger than `max` bytes or not UTF-8. It is read in chunks and
/// stopped at the limit, instead of being held whole first.
pub async fn read_limited(mut response: Response, max: usize) -> Result<String, String> {
    if response.content_length().is_some_and(|n| n > max as u64) {
        return Err("the answer is too large".into());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        if body.len() + chunk.len() > max {
            return Err("the answer is too large".into());
        }
        body.extend_from_slice(&chunk);
    }
    String::from_utf8(body).map_err(|_| "the answer is not text".into())
}

/// Fetches the provider's keys over HTTP.
pub struct ReqwestJwks {
    client: Client,
}

impl ReqwestJwks {
    pub fn new(client: Client) -> Self {
        Self { client }
    }
}

impl JwksSource for ReqwestJwks {
    async fn fetch(&self, uri: &str) -> Result<String, String> {
        let url = url::Url::parse(uri).map_err(|e| e.to_string())?;
        if !allowed_url(&url) {
            return Err("the key address is not one this server may use".into());
        }
        let response = self
            .client
            .get(url)
            .header("accept", "application/json")
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !response.status().is_success() {
            return Err(format!("status {}", response.status()));
        }
        read_limited(response, MAX_BODY).await
    }
}
