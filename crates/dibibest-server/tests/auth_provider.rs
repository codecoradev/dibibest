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
        on.auth.issuer.as_deref(),
        Some("https://identity.example/api/auth")
    );
    assert_eq!(off.auth.issuer, None);
    // an issuer alone (the MCP endpoint, no sign-in for people) gives a provider but no sign-in
    let alone = AppState::new(
        Arc::new(SqliteStore::open(dir.path().join("alone.db")).unwrap()),
        &Config {
            public_url: Some("https://dibibest.codecora.dev".into()),
            issuer: Some("https://identity.example/api/auth".into()),
            ..Config::default()
        },
    );
    assert!(alone.auth.provider.is_some() && alone.auth.oidc.is_none());
    assert_eq!(
        alone.auth.issuer.as_deref(),
        Some("https://identity.example/api/auth")
    );
    assert_eq!(
        on.auth.public_origin.as_deref(),
        Some("https://dibibest.codecora.dev")
    );
}

// ── the provider's own metadata document (for the MCP origin, issue #94) ────────────────────────────────

#[tokio::test]
async fn the_providers_metadata_is_passed_on_unchanged_and_kept() {
    let idp = Idp::start().await;
    let mut doc = idp.state.discovery.lock().unwrap().json.clone().unwrap();
    doc["x_extra_field"] = json!({"kept": true});
    idp.set("discovery", Answer::ok(doc));
    let p = provider(&idp);
    for _ in 0..2 {
        let got = p.authorization_server_metadata().await.expect("a document");
        assert_eq!(
            got["x_extra_field"],
            json!({"kept": true}),
            "fields the server does not know are kept"
        );
        assert_eq!(got["issuer"], idp.issuer());
    }
    assert_eq!(
        idp.calls_to("/.well-known/oauth-authorization-server")
            .len(),
        1,
        "the RFC 8414 address is tried first"
    );
    assert_eq!(
        idp.calls_to("/.well-known/openid-configuration").len(),
        1,
        "the second answer came from memory"
    );
}

#[tokio::test]
async fn an_old_copy_of_the_metadata_is_refreshed_and_when_the_provider_is_down_it_is_still_served()
{
    let idp = Idp::start().await;
    let p = Provider::with_ttl(
        &idp.issuer(),
        client(Duration::from_secs(5)),
        Duration::from_millis(50),
    );
    assert!(p.authorization_server_metadata().await.is_some());

    // once the copy is old, a changed document is fetched and replaces it
    let mut changed = idp.state.discovery.lock().unwrap().json.clone().unwrap();
    changed["generation"] = json!(2);
    idp.set("discovery", Answer::ok(changed));
    tokio::time::sleep(Duration::from_millis(120)).await;
    assert_eq!(
        p.authorization_server_metadata().await.unwrap()["generation"],
        2,
        "an old copy is refreshed"
    );

    // a provider that is down: the last copy still beats nothing
    idp.set(
        "discovery",
        Answer {
            status: 500,
            ..Answer::ok(json!({}))
        },
    );
    tokio::time::sleep(Duration::from_millis(120)).await;
    let stale = p
        .authorization_server_metadata()
        .await
        .expect("the old copy is served while the provider is down");
    assert_eq!(stale["generation"], 2);
}

#[tokio::test]
async fn a_document_that_is_not_a_provider_description_is_not_accepted_and_nothing_to_serve_is_none(
) {
    // an error status is not a document, whatever the body looks like
    let idp = Idp::start().await;
    let good = idp.state.discovery.lock().unwrap().json.clone().unwrap();
    idp.set(
        "discovery",
        Answer {
            status: 503,
            ..Answer::ok(good)
        },
    );
    assert!(provider(&idp)
        .authorization_server_metadata()
        .await
        .is_none());
    for bad in [
        json!({"issuer": 5, "authorization_endpoint": "https://x.example/a"}),
        json!({"issuer": "https://x.example"}),
        json!({"authorization_endpoint": "https://x.example/a"}),
        json!([1, 2]),
    ] {
        let idp = Idp::start().await;
        idp.set("discovery", Answer::ok(bad.clone()));
        assert!(
            provider(&idp)
                .authorization_server_metadata()
                .await
                .is_none(),
            "{bad}"
        );
    }
    let idp = Idp::start().await;
    idp.set(
        "discovery",
        Answer {
            raw: Some("<html>proxy error</html>".into()),
            ..Answer::ok(json!({}))
        },
    );
    assert!(provider(&idp)
        .authorization_server_metadata()
        .await
        .is_none());
}

fn rfc8414_doc(idp: &Idp, extra: serde_json::Value) -> serde_json::Value {
    let base = idp.issuer();
    let mut doc = json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/oauth2/authorize"),
        "token_endpoint": format!("{base}/oauth2/token"),
        "jwks_uri": format!("{base}/jwks"),
    });
    for (k, v) in extra.as_object().cloned().unwrap_or_default() {
        doc[k] = v;
    }
    doc
}

#[tokio::test]
async fn the_rfc_8414_document_is_preferred_and_the_openid_one_is_the_fallback() {
    let idp = Idp::start().await;
    idp.set(
        "as_metadata",
        Answer::ok(rfc8414_doc(&idp, json!({"served_from": "rfc8414"}))),
    );
    let got = provider(&idp)
        .authorization_server_metadata()
        .await
        .unwrap();
    assert_eq!(got["served_from"], "rfc8414");
    assert!(
        idp.calls_to("/.well-known/openid-configuration").is_empty(),
        "the second address is not asked when the first answers"
    );

    // the first address failing falls through to the second
    let idp = Idp::start().await; // as_metadata answers 404 by default
    assert!(provider(&idp)
        .authorization_server_metadata()
        .await
        .is_some());
    assert_eq!(idp.calls_to("/.well-known/openid-configuration").len(), 1);
}

#[tokio::test]
async fn a_document_that_claims_another_issuer_is_not_served() {
    // RFC 8414 section 3.3: the issuer in the document must be the one it was fetched for
    let idp = Idp::start().await;
    idp.set(
        "as_metadata",
        Answer::ok(rfc8414_doc(
            &idp,
            json!({"issuer": "https://evil.example/api/auth"}),
        )),
    );
    idp.set(
        "discovery",
        Answer::ok(rfc8414_doc(
            &idp,
            json!({"issuer": "https://evil.example/api/auth"}),
        )),
    );
    assert!(
        provider(&idp)
            .authorization_server_metadata()
            .await
            .is_none(),
        "both documents name another issuer"
    );

    // one wrong, one right: the right one is the answer
    let idp = Idp::start().await;
    idp.set(
        "as_metadata",
        Answer::ok(rfc8414_doc(
            &idp,
            json!({"issuer": "https://evil.example/api/auth"}),
        )),
    );
    let got = provider(&idp)
        .authorization_server_metadata()
        .await
        .expect("the OpenID document is fine");
    assert_eq!(got["issuer"], idp.issuer());

    // a trailing slash on the issuer in the document is the same issuer
    let idp = Idp::start().await;
    let with_slash = format!("{}/", idp.issuer());
    idp.set(
        "discovery",
        Answer::ok(rfc8414_doc(&idp, json!({"issuer": with_slash}))),
    );
    assert!(provider(&idp)
        .authorization_server_metadata()
        .await
        .is_some());
}

#[tokio::test]
async fn a_document_that_names_an_address_the_server_would_not_use_is_not_served() {
    // an address that is not even text
    let idp = Idp::start().await;
    idp.set(
        "discovery",
        Answer::ok(rfc8414_doc(&idp, json!({"token_endpoint": 5}))),
    );
    assert!(
        provider(&idp)
            .authorization_server_metadata()
            .await
            .is_none(),
        "token_endpoint=5"
    );
    for (field, bad) in [
        ("authorization_endpoint", "http://evil.example/authorize"),
        ("token_endpoint", "https://user:pass@idp.example/token"),
        ("jwks_uri", "http://169.254.169.254/keys"),
        ("registration_endpoint", "ftp://idp.example/register"),
    ] {
        let idp = Idp::start().await;
        idp.set(
            "discovery",
            Answer::ok(rfc8414_doc(&idp, json!({field: bad}))),
        );
        assert!(
            provider(&idp)
                .authorization_server_metadata()
                .await
                .is_none(),
            "{field}={bad}"
        );
    }
}

fn down() -> Answer {
    Answer {
        status: 500,
        ..Answer::ok(json!({}))
    }
}

async fn calls_for_metadata(idp: &Idp) -> usize {
    idp.calls_to("/.well-known/openid-configuration").len()
        + idp
            .calls_to("/.well-known/oauth-authorization-server")
            .len()
}

#[tokio::test]
async fn while_the_provider_is_down_the_old_copy_is_served_without_asking_again_every_time() {
    let idp = Idp::start().await;
    let p = Provider::with_ttl(
        &idp.issuer(),
        client(Duration::from_secs(5)),
        Duration::from_millis(50),
    );
    assert!(p.authorization_server_metadata().await.is_some());
    idp.set("discovery", down());
    tokio::time::sleep(Duration::from_millis(120)).await;

    assert!(
        p.authorization_server_metadata().await.is_some(),
        "the first call after the outage tries and serves the old copy"
    );
    let asked = calls_for_metadata(&idp).await;
    for _ in 0..10 {
        assert!(p.authorization_server_metadata().await.is_some());
    }
    assert_eq!(
        calls_for_metadata(&idp).await,
        asked,
        "a failed refresh must not be retried by every request"
    );
}

#[tokio::test]
async fn a_provider_that_was_down_from_the_start_is_not_asked_again_by_every_request() {
    let idp = Idp::start().await;
    idp.set("discovery", down());
    let p = provider(&idp);
    assert!(p.authorization_server_metadata().await.is_none());
    let asked = calls_for_metadata(&idp).await;
    assert!(asked > 0);
    for _ in 0..10 {
        assert!(p.authorization_server_metadata().await.is_none());
    }
    assert_eq!(calls_for_metadata(&idp).await, asked);
}

#[tokio::test]
async fn once_the_wait_is_over_a_recovered_provider_fills_the_cache_and_replaces_an_old_copy() {
    // empty cache, provider down at first
    let idp = Idp::start().await;
    let good = idp.state.discovery.lock().unwrap().clone();
    idp.set("discovery", down());
    let p = Provider::with_ttl(
        &idp.issuer(),
        client(Duration::from_secs(5)),
        Duration::from_millis(50),
    )
    .with_retry_gap(Duration::from_millis(200));
    assert!(p.authorization_server_metadata().await.is_none());
    idp.set("discovery", good);
    assert!(
        p.authorization_server_metadata().await.is_none(),
        "still inside the wait"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    let first = p.authorization_server_metadata().await.expect("recovered");

    // an old copy is replaced by the new document once the wait is over
    idp.set("discovery", down());
    tokio::time::sleep(Duration::from_millis(120)).await;
    assert_eq!(p.authorization_server_metadata().await.unwrap(), first);
    idp.set(
        "discovery",
        Answer::ok(rfc8414_doc(&idp, json!({"generation": 2}))),
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        p.authorization_server_metadata().await.unwrap()["generation"],
        2
    );
}

#[tokio::test]
async fn the_production_lifetimes_keep_a_copy_for_minutes_and_wait_before_retrying() {
    // Provider::new: five minutes for a copy, thirty seconds between failed attempts
    let idp = Idp::start().await;
    let p = provider(&idp);
    assert!(p.authorization_server_metadata().await.is_some());
    idp.set("discovery", down());
    for _ in 0..5 {
        assert!(p.authorization_server_metadata().await.is_some());
    }
    assert_eq!(
        calls_for_metadata(&idp).await,
        2,
        "one fetch (the RFC 8414 address fails, the OpenID one answers) and nothing more inside the lifetime"
    );
}

#[tokio::test]
async fn every_endpoint_address_in_the_document_is_checked_and_links_are_not() {
    for field in [
        "introspection_endpoint",
        "end_session_endpoint",
        "device_authorization_endpoint",
        "pushed_authorization_request_endpoint",
    ] {
        let idp = Idp::start().await;
        idp.set(
            "discovery",
            Answer::ok(rfc8414_doc(&idp, json!({field: "http://evil.example/x"}))),
        );
        assert!(
            provider(&idp)
                .authorization_server_metadata()
                .await
                .is_none(),
            "{field}"
        );
    }
    // addresses inside mtls_endpoint_aliases too, and a malformed one
    for aliases in [
        json!({"token_endpoint": "http://evil.example/t"}),
        json!({"token_endpoint": 5}),
        json!("not an object"),
    ] {
        let idp = Idp::start().await;
        idp.set(
            "discovery",
            Answer::ok(rfc8414_doc(&idp, json!({"mtls_endpoint_aliases": aliases}))),
        );
        assert!(provider(&idp)
            .authorization_server_metadata()
            .await
            .is_none());
    }
    // links to documentation are not endpoints; a good alias is fine
    let idp = Idp::start().await;
    let ok_alias = format!("{}/mtls/token", idp.issuer());
    idp.set(
        "discovery",
        Answer::ok(rfc8414_doc(
            &idp,
            json!({
                "scopes_supported": ["openid"],
                "service_documentation": "http://docs.example/",
                "op_policy_uri": "http://docs.example/policy",
                "mtls_endpoint_aliases": {"token_endpoint": ok_alias},
            }),
        )),
    );
    assert!(provider(&idp)
        .authorization_server_metadata()
        .await
        .is_some());
}

#[tokio::test]
async fn a_null_address_is_the_same_as_none_but_a_wrong_kind_of_value_is_not() {
    let idp = Idp::start().await;
    idp.set(
        "discovery",
        Answer::ok(rfc8414_doc(&idp, json!({"registration_endpoint": null}))),
    );
    assert!(provider(&idp)
        .authorization_server_metadata()
        .await
        .is_some());
    let idp = Idp::start().await;
    idp.set(
        "discovery",
        Answer::ok(rfc8414_doc(&idp, json!({"registration_endpoint": 7}))),
    );
    assert!(provider(&idp)
        .authorization_server_metadata()
        .await
        .is_none());
}
