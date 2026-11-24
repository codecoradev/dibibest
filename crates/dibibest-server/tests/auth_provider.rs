mod common;
use common::idp::{Answer, Idp};
use dibibest_server::auth::http::{client, ReqwestJwks, MAX_BODY};
use dibibest_server::auth::jwt::JwksSource;
use dibibest_server::auth::provider::{Provider, ProviderError};
use serde_json::json;
use std::time::Duration;

fn provider(idp: &Idp) -> Provider {
    Provider::new(&idp.issuer(), client(Duration::from_secs(5)))
}

#[tokio::test]
async fn discovery_gives_the_endpoints_and_is_kept_for_next_time() {
    let idp = Idp::start().await;
    let p = provider(&idp);
    let d = p.discovery().await.unwrap();
    assert_eq!(
        d.authorization_endpoint,
        format!("{}/oauth2/authorize", idp.issuer())
    );
    assert_eq!(d.token_endpoint, format!("{}/oauth2/token", idp.issuer()));
    assert_eq!(
        d.userinfo_endpoint,
        format!("{}/oauth2/userinfo", idp.issuer())
    );
    assert_eq!(
        d.jwks_uri.as_deref(),
        Some(format!("{}/jwks", idp.issuer()).as_str())
    );
    p.discovery().await.unwrap();
    assert_eq!(
        idp.calls_to("/.well-known/openid-configuration").len(),
        1,
        "the second call is served from memory"
    );
}

#[tokio::test]
async fn discovery_is_fetched_again_once_it_is_stale() {
    let idp = Idp::start().await;
    let p = Provider::with_ttl(
        &idp.issuer(),
        client(Duration::from_secs(5)),
        Duration::from_millis(50),
    );
    p.discovery().await.unwrap();
    tokio::time::sleep(Duration::from_millis(120)).await;
    p.discovery().await.unwrap();
    assert_eq!(idp.calls_to("/.well-known/openid-configuration").len(), 2);
}

#[tokio::test]
async fn an_endpoint_the_server_must_not_talk_to_makes_the_whole_document_unusable() {
    for (field, bad) in [
        ("token_endpoint", "http://evil.example/token"),
        ("userinfo_endpoint", "http://169.254.169.254/latest"),
        (
            "authorization_endpoint",
            "https://u:p@idp.example/authorize",
        ),
        ("token_endpoint", "ftp://idp.example/token"),
        ("token_endpoint", "not a url"),
        ("jwks_uri", "http://evil.example/jwks"),
    ] {
        let idp = Idp::start().await;
        let base = idp.issuer();
        let mut doc = json!({
            "authorization_endpoint": format!("{base}/oauth2/authorize"),
            "token_endpoint": format!("{base}/oauth2/token"),
            "userinfo_endpoint": format!("{base}/oauth2/userinfo"),
            "jwks_uri": format!("{base}/jwks"),
        });
        doc[field] = json!(bad);
        idp.set("discovery", Answer::ok(doc));
        let err = provider(&idp).discovery().await.unwrap_err();
        assert!(
            matches!(err, ProviderError::Invalid(_)),
            "{field}={bad}: {err:?}"
        );
    }
}

#[tokio::test]
async fn a_discovery_document_without_an_endpoint_is_unusable() {
    for missing in [
        "authorization_endpoint",
        "token_endpoint",
        "userinfo_endpoint",
    ] {
        let idp = Idp::start().await;
        let base = idp.issuer();
        let mut doc = json!({
            "authorization_endpoint": format!("{base}/oauth2/authorize"),
            "token_endpoint": format!("{base}/oauth2/token"),
            "userinfo_endpoint": format!("{base}/oauth2/userinfo"),
        });
        doc.as_object_mut().unwrap().remove(missing);
        idp.set("discovery", Answer::ok(doc));
        assert!(
            matches!(
                provider(&idp).discovery().await,
                Err(ProviderError::Invalid(_))
            ),
            "{missing}"
        );
    }
}

#[tokio::test]
async fn a_provider_that_answers_badly_or_not_at_all_is_an_error_not_a_hang() {
    // not JSON
    let idp = Idp::start().await;
    idp.set(
        "discovery",
        Answer {
            raw: Some("<html>proxy error</html>".into()),
            ..Answer::ok(json!({}))
        },
    );
    assert!(matches!(
        provider(&idp).discovery().await,
        Err(ProviderError::Invalid(_))
    ));
    // an error status
    let idp = Idp::start().await;
    idp.set(
        "discovery",
        Answer {
            status: 500,
            ..Answer::ok(json!({}))
        },
    );
    assert!(matches!(
        provider(&idp).discovery().await,
        Err(ProviderError::Unreachable(_))
    ));
    // nothing listening
    let dead = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = dead.local_addr().unwrap();
    drop(dead);
    let p = Provider::new(
        &format!("http://{addr}/api/auth"),
        client(Duration::from_secs(2)),
    );
    assert!(matches!(
        p.discovery().await,
        Err(ProviderError::Unreachable(_))
    ));
    // too slow
    let idp = Idp::start().await;
    idp.set(
        "discovery",
        Answer {
            delay: Duration::from_secs(3),
            ..Answer::ok(json!({}))
        },
    );
    let p = Provider::new(&idp.issuer(), client(Duration::from_millis(300)));
    let started = std::time::Instant::now();
    assert!(matches!(
        p.discovery().await,
        Err(ProviderError::Unreachable(_))
    ));
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the timeout did not apply"
    );
}

#[tokio::test]
async fn a_redirect_from_the_provider_is_not_followed() {
    let idp = Idp::start().await;
    // The redirect points at a URL that would answer with a perfectly good discovery document, so following it
    // would succeed. It must not even be asked.
    let good = idp.state.discovery.lock().unwrap().clone();
    idp.set("jwks", good);
    idp.set(
        "discovery",
        Answer {
            status: 302,
            location: Some(format!("{}/jwks", idp.issuer())),
            ..Answer::ok(json!({}))
        },
    );
    assert!(provider(&idp).discovery().await.is_err());
    assert!(
        idp.calls_to("/jwks").is_empty(),
        "the redirect target was requested"
    );
}

#[tokio::test]
async fn an_answer_that_is_too_big_is_refused() {
    let idp = Idp::start().await;
    idp.set(
        "discovery",
        Answer {
            raw: Some("x".repeat(MAX_BODY + 1)),
            ..Answer::ok(json!({}))
        },
    );
    assert!(provider(&idp).discovery().await.is_err());
    idp.set(
        "jwks",
        Answer {
            raw: Some("x".repeat(MAX_BODY + 1)),
            ..Answer::ok(json!({}))
        },
    );
    let jwks = ReqwestJwks::new(client(Duration::from_secs(5)));
    assert!(jwks.fetch(&format!("{}/jwks", idp.issuer())).await.is_err());
}

#[tokio::test]
async fn an_answer_with_no_length_is_cut_off_at_the_limit() {
    // 10 times the limit, sent in chunks with no Content-Length: the limit has to be enforced while reading
    let addr = common::idp::chunked_flood(MAX_BODY * 10).await;
    let p = Provider::new(
        &format!("http://{addr}/api/auth"),
        client(Duration::from_secs(5)),
    );
    match p.discovery().await {
        Err(ProviderError::Unreachable(why)) => assert!(why.contains("too large"), "{why}"),
        other => panic!("expected a too-large answer to be refused, got {other:?}"),
    }
    let jwks = ReqwestJwks::new(client(Duration::from_secs(5)));
    let why = jwks
        .fetch(&format!("http://{addr}/jwks"))
        .await
        .unwrap_err();
    assert!(why.contains("too large"), "{why}");
}

#[tokio::test]
async fn the_key_source_refuses_an_address_the_server_must_not_use_without_connecting() {
    let jwks = ReqwestJwks::new(client(Duration::from_secs(5)));
    for bad in [
        "http://keys.example/jwks",
        "https://u:p@keys.example/jwks",
        "file:///etc/passwd",
        "not a url",
    ] {
        let why = jwks.fetch(bad).await.unwrap_err();
        assert!(
            why.contains("may use") || why.contains("relative URL") || why.contains("invalid"),
            "{bad}: {why}"
        );
    }
    let why = jwks.fetch("http://keys.example/jwks").await.unwrap_err();
    assert!(
        why.contains("may use"),
        "refused by the policy, not by a failed lookup: {why}"
    );
}

#[tokio::test]
async fn the_key_source_returns_the_body_and_fails_on_an_error_status() {
    let idp = Idp::start().await;
    idp.set("jwks", Answer::ok(json!({"keys": []})));
    let jwks = ReqwestJwks::new(client(Duration::from_secs(5)));
    let url = format!("{}/jwks", idp.issuer());
    assert_eq!(jwks.fetch(&url).await.unwrap(), r#"{"keys":[]}"#);
    idp.set(
        "jwks",
        Answer {
            status: 503,
            ..Answer::ok(json!({}))
        },
    );
    assert!(jwks.fetch(&url).await.is_err());
    assert!(
        jwks.fetch(&format!("{}/nowhere", idp.issuer()))
            .await
            .is_err(),
        "a 404"
    );
}

#[tokio::test]
async fn the_app_has_a_provider_only_when_sign_in_is_configured() {
    use dibibest_server::app::AppState;
    use dibibest_server::config::{Config, Oidc, TokenAuth};
    use dibibest_store::SqliteStore;
    use std::sync::Arc;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteStore::open(dir.path().join("s.db")).unwrap());
    let off = AppState::new(store.clone(), &Config::default());
    assert!(off.auth.provider.is_none() && off.auth.oidc.is_none());
    let on = AppState::new(
        store,
        &Config {
            public_url: Some("https://dibibest.codecora.dev".into()),
            oidc: Some(Oidc {
                issuer: "https://identity.example/api/auth".into(),
                client_id: "id".into(),
                client_secret: "secret".into(),
                session_secret: "0123456789abcdef0123456789abcdef".into(),
                token_auth: TokenAuth::Basic,
            }),
            ..Config::default()
        },
    );
    assert!(on.auth.provider.is_some());
    assert_eq!(
        on.auth.public_origin.as_deref(),
        Some("https://dibibest.codecora.dev")
    );
}
