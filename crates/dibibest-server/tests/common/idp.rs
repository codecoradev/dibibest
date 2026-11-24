//! A fake identity provider on the loopback address: discovery, keys, token and userinfo endpoints. It records
//! every request and its answers can be changed per test.
#![allow(dead_code)]
use axum::body::{to_bytes, Body};
use axum::extract::State;
use axum::http::{Request, Response, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Debug, Default)]
pub struct Call {
    pub method: String,
    pub path: String,
    pub headers: HashMap<String, String>,
    pub body: String,
}

/// What an endpoint answers. `raw` replaces the JSON body, to send something that is not JSON.
#[derive(Clone, Debug)]
pub struct Answer {
    pub status: u16,
    pub json: Option<Value>,
    pub raw: Option<String>,
    pub location: Option<String>,
    pub delay: Duration,
}

impl Answer {
    pub fn ok(v: Value) -> Self {
        Self {
            status: 200,
            json: Some(v),
            raw: None,
            location: None,
            delay: Duration::ZERO,
        }
    }
}

pub struct IdpState {
    pub calls: Mutex<Vec<Call>>,
    pub discovery: Mutex<Answer>,
    pub jwks: Mutex<Answer>,
    pub token: Mutex<Answer>,
    pub userinfo: Mutex<Answer>,
}

pub struct Idp {
    pub addr: SocketAddr,
    pub state: Arc<IdpState>,
}

impl Idp {
    pub async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}/api/auth");
        let state = Arc::new(IdpState {
            calls: Mutex::new(Vec::new()),
            discovery: Mutex::new(Answer::ok(json!({
                "issuer": base,
                "authorization_endpoint": format!("{base}/oauth2/authorize"),
                "token_endpoint": format!("{base}/oauth2/token"),
                "userinfo_endpoint": format!("{base}/oauth2/userinfo"),
                "jwks_uri": format!("{base}/jwks"),
            }))),
            jwks: Mutex::new(Answer::ok(json!({ "keys": [] }))),
            token: Mutex::new(Answer::ok(
                json!({ "access_token": "at-1", "token_type": "Bearer" }),
            )),
            userinfo: Mutex::new(Answer::ok(
                json!({ "sub": "u1", "email": "Ana@Example.com", "email_verified": true }),
            )),
        });
        let app = Router::new().fallback(handle).with_state(state.clone());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self { addr, state }
    }

    pub fn issuer(&self) -> String {
        format!("http://{}/api/auth", self.addr)
    }

    pub fn calls_to(&self, path_suffix: &str) -> Vec<Call> {
        self.state
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.path.ends_with(path_suffix))
            .cloned()
            .collect()
    }

    pub fn set(&self, which: &str, answer: Answer) {
        let slot = match which {
            "discovery" => &self.state.discovery,
            "jwks" => &self.state.jwks,
            "token" => &self.state.token,
            "userinfo" => &self.state.userinfo,
            other => panic!("no endpoint {other}"),
        };
        *slot.lock().unwrap() = answer;
    }
}

async fn handle(State(s): State<Arc<IdpState>>, req: Request<Body>) -> Response<Body> {
    let (parts, body) = req.into_parts();
    let body = String::from_utf8_lossy(&to_bytes(body, usize::MAX).await.unwrap()).into_owned();
    let path = parts.uri.path().to_string();
    s.calls.lock().unwrap().push(Call {
        method: parts.method.to_string(),
        path: path.clone(),
        headers: parts
            .headers
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
            .collect(),
        body,
    });
    let answer = match path.as_str() {
        "/api/auth/.well-known/openid-configuration" => s.discovery.lock().unwrap().clone(),
        "/api/auth/jwks" => s.jwks.lock().unwrap().clone(),
        "/api/auth/oauth2/token" => s.token.lock().unwrap().clone(),
        "/api/auth/oauth2/userinfo" => s.userinfo.lock().unwrap().clone(),
        _ => {
            return Response::builder()
                .status(404)
                .body(Body::from("not found"))
                .unwrap()
        }
    };
    if !answer.delay.is_zero() {
        tokio::time::sleep(answer.delay).await;
    }
    let mut res = Response::builder().status(StatusCode::from_u16(answer.status).unwrap());
    if let Some(l) = &answer.location {
        res = res.header("location", l);
    }
    let text = answer.raw.clone().unwrap_or_else(|| {
        answer
            .json
            .clone()
            .map(|v| v.to_string())
            .unwrap_or_default()
    });
    res.header("content-type", "application/json")
        .body(Body::from(text))
        .unwrap()
}

/// A server that answers with `total` bytes in chunks and no Content-Length, as an endless or hostile provider
/// could. Returns its address.
pub async fn chunked_flood(total: usize) -> SocketAddr {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut request = [0u8; 4096];
                let _ = sock.read(&mut request).await;
                let head = b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ntransfer-encoding: chunked\r\n\r\n";
                if sock.write_all(head).await.is_err() {
                    return;
                }
                let chunk = vec![b'x'; 16 * 1024];
                let mut sent = 0;
                while sent < total {
                    let size = format!("{:x}\r\n", chunk.len());
                    let wrote = sock.write_all(size.as_bytes()).await.is_ok()
                        && sock.write_all(&chunk).await.is_ok()
                        && sock.write_all(b"\r\n").await.is_ok();
                    if !wrote {
                        return; // the client hung up: it stopped reading at its limit
                    }
                    sent += chunk.len();
                }
                let _ = sock.write_all(b"0\r\n\r\n").await;
            });
        }
    });
    addr
}
