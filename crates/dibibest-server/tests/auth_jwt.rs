use dibibest_server::auth::codec::b64url;
use dibibest_server::auth::jwt::{Claims, JwksSource, TokenError, Verifier, VerifyOptions};
use ring::rand::SystemRandom;
use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_FIXED_SIGNING};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const ISS: &str = "https://idp.test/api/auth";
const AUD: &str = "https://app.test/mcp";
const JWKS: &str = "https://idp.test/api/auth/jwks";
const NOW: i64 = 1_795_000_000;

/// A throwaway ES256 key made for the test, so no private key is kept anywhere.
struct Keys {
    pair: EcdsaKeyPair,
    rng: SystemRandom,
}

impl Keys {
    fn new() -> Self {
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng)
            .unwrap();
        Self { pair, rng }
    }

    fn jwk(&self, kid: &str, alg: Option<&str>) -> Value {
        let point = self.pair.public_key().as_ref(); // 0x04, x (32 bytes), y (32 bytes)
        let mut k = json!({"kty": "EC", "crv": "P-256", "kid": kid, "use": "sig",
            "x": b64url(&point[1..33]), "y": b64url(&point[33..65])});
        if let Some(a) = alg {
            k["alg"] = json!(a);
        }
        k
    }

    fn token(&self, header: Value, claims: Value) -> String {
        let head = b64url(header.to_string().as_bytes());
        let body = b64url(claims.to_string().as_bytes());
        let sig = self
            .pair
            .sign(&self.rng, format!("{head}.{body}").as_bytes())
            .unwrap();
        format!("{head}.{body}.{}", b64url(sig.as_ref()))
    }
}

/// `over` replaces keys of `base`; a null value removes the key.
fn merge(mut base: Value, over: Value) -> Value {
    for (k, v) in over.as_object().cloned().unwrap_or_default() {
        if v.is_null() {
            base.as_object_mut().unwrap().remove(&k);
        } else {
            base[k] = v;
        }
    }
    base
}

fn header(over: Value) -> Value {
    merge(json!({"alg": "ES256", "typ": "at+jwt", "kid": "k1"}), over)
}

fn claims(over: Value) -> Value {
    merge(
        json!({"iss": ISS, "sub": "u1", "aud": [AUD, format!("{ISS}/oauth2/userinfo")], "exp": NOW + 300, "iat": NOW}),
        over,
    )
}

/// A JWKS endpoint that counts how often it is asked.
#[derive(Default)]
struct Canned {
    body: Mutex<String>,
    hits: AtomicUsize,
    down: AtomicBool,
}

impl Canned {
    fn serve(&self, keys: Vec<Value>) {
        *self.body.lock().unwrap() = json!({ "keys": keys }).to_string();
    }
    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

/// The trait comes from the library, so it cannot be implemented for `Arc<Canned>` here: a local newtype.
struct Source(Arc<Canned>);

impl JwksSource for Source {
    async fn fetch(&self, _uri: &str) -> Result<String, String> {
        self.0.hits.fetch_add(1, Ordering::SeqCst);
        if self.0.down.load(Ordering::SeqCst) {
            return Err("connection refused".into());
        }
        Ok(self.0.body.lock().unwrap().clone())
    }
}

fn setup(keys: &Keys) -> (Verifier<Source>, Arc<Canned>) {
    let source = Arc::new(Canned::default());
    source.serve(vec![keys.jwk("k1", Some("ES256"))]);
    (Verifier::new(Source(source.clone())), source)
}

async fn check(v: &Verifier<Source>, token: &str) -> Result<Claims, TokenError> {
    v.verify(
        token,
        &VerifyOptions {
            issuer: ISS,
            audience: AUD,
            jwks_uri: JWKS,
            now: NOW,
        },
    )
    .await
}

async fn refused(v: &Verifier<Source>, token: &str) -> String {
    match check(v, token).await {
        Err(TokenError::Invalid(why)) => why,
        other => panic!("expected an invalid token, got {other:?}"),
    }
}

#[tokio::test]
async fn a_good_token_gives_its_claims() {
    let keys = Keys::new();
    let (v, _) = setup(&keys);
    let c = check(&v, &keys.token(header(json!({})), claims(json!({}))))
        .await
        .unwrap();
    assert_eq!(c.sub, "u1");
    assert_eq!(c.iss, ISS);
    assert!(c.audiences().contains(&AUD.to_string()));
}

#[tokio::test]
async fn a_single_string_audience_is_accepted() {
    let keys = Keys::new();
    let (v, _) = setup(&keys);
    assert!(check(
        &v,
        &keys.token(header(json!({})), claims(json!({"aud": AUD})))
    )
    .await
    .is_ok());
}

#[tokio::test]
async fn every_way_a_token_can_be_wrong_is_refused() {
    let keys = Keys::new();
    let (v, _) = setup(&keys);
    let cases: Vec<(&str, Value, Value)> = vec![
        (
            "wrong issuer",
            json!({}),
            json!({"iss": "https://evil.example"}),
        ),
        (
            "wrong audience",
            json!({}),
            json!({"aud": "https://other.example/mcp"}),
        ),
        ("no audience", json!({}), json!({"aud": null})),
        ("expired", json!({}), json!({"exp": NOW - 3600})),
        ("no exp", json!({}), json!({"exp": null})),
        ("exp is not a number", json!({}), json!({"exp": "soon"})),
        ("not yet valid", json!({}), json!({"nbf": NOW + 3600})),
        (
            "issued in the future",
            json!({}),
            json!({"iat": NOW + 3600}),
        ),
        (
            "sender-constrained (DPoP)",
            json!({}),
            json!({"cnf": {"jkt": "abc"}}),
        ),
        ("empty subject", json!({}), json!({"sub": ""})),
        ("no subject", json!({}), json!({"sub": null})),
        ("unknown kid", json!({"kid": "nope"}), json!({})),
        ("alg none", json!({"alg": "none"}), json!({})),
        ("alg HS256", json!({"alg": "HS256"}), json!({})),
        (
            "an alg that is not allowed",
            json!({"alg": "RS512"}),
            json!({}),
        ),
        ("a crit header", json!({"crit": ["exp"]}), json!({})),
    ];
    for (name, h, c) in cases {
        refused(&v, &keys.token(header(h), claims(c))).await;
        eprintln!("refused: {name}");
    }
}

#[tokio::test]
async fn a_key_that_does_not_list_an_alg_does_not_let_alg_none_through() {
    let keys = Keys::new();
    let source = Arc::new(Canned::default());
    source.serve(vec![keys.jwk("k1", None)]);
    let v = Verifier::new(Source(source));
    assert!(
        check(&v, &keys.token(header(json!({})), claims(json!({}))))
            .await
            .is_ok(),
        "an honest token still works"
    );
    for alg in ["none", "HS256", "RS512"] {
        refused(
            &v,
            &keys.token(header(json!({"alg": alg})), claims(json!({}))),
        )
        .await;
    }
}

#[tokio::test]
async fn a_key_listed_for_another_alg_is_not_used() {
    let keys = Keys::new();
    let source = Arc::new(Canned::default());
    source.serve(vec![keys.jwk("k1", Some("RS256"))]);
    let v = Verifier::new(Source(source));
    refused(&v, &keys.token(header(json!({})), claims(json!({})))).await;
}

#[tokio::test]
async fn a_signature_from_another_key_or_a_changed_payload_is_refused() {
    let keys = Keys::new();
    let (v, _) = setup(&keys);
    let attacker = Keys::new();
    refused(&v, &attacker.token(header(json!({})), claims(json!({})))).await;
    let good = keys.token(header(json!({})), claims(json!({})));
    let parts: Vec<&str> = good.split('.').collect();
    let forged = b64url(claims(json!({"sub": "admin"})).to_string().as_bytes());
    refused(&v, &format!("{}.{forged}.{}", parts[0], parts[2])).await;
}

#[tokio::test]
async fn text_that_is_not_a_jwt_is_refused() {
    let keys = Keys::new();
    let (v, _) = setup(&keys);
    for bad in [
        "opaque-token-value",
        "a.b",
        "a.b.c.d",
        "",
        "..",
        "!!!.???.***",
    ] {
        refused(&v, bad).await;
    }
}

#[tokio::test]
async fn the_clock_has_a_leeway_of_one_minute() {
    let keys = Keys::new();
    let (v, _) = setup(&keys);
    let at = |over: Value| keys.token(header(json!({})), claims(over));
    assert!(check(&v, &at(json!({"exp": NOW - 30}))).await.is_ok());
    refused(&v, &at(json!({"exp": NOW - 61}))).await;
    assert!(check(&v, &at(json!({"nbf": NOW + 30}))).await.is_ok());
    refused(&v, &at(json!({"nbf": NOW + 61}))).await;
}

#[tokio::test(start_paused = true)]
async fn keys_are_fetched_once_and_kept_for_an_hour() {
    let keys = Keys::new();
    let (v, source) = setup(&keys);
    let t = keys.token(header(json!({})), claims(json!({})));
    check(&v, &t).await.unwrap();
    check(&v, &t).await.unwrap();
    assert_eq!(source.hits(), 1);
    tokio::time::advance(Duration::from_secs(3599)).await;
    check(&v, &t).await.unwrap();
    assert_eq!(source.hits(), 1);
    tokio::time::advance(Duration::from_secs(2)).await;
    check(&v, &t).await.unwrap();
    assert_eq!(source.hits(), 2, "after an hour the keys are fetched again");
}

#[tokio::test(start_paused = true)]
async fn an_unknown_key_id_refetches_the_keys_but_at_most_once_a_minute() {
    let keys = Keys::new();
    let (v, source) = setup(&keys);
    check(&v, &keys.token(header(json!({})), claims(json!({}))))
        .await
        .unwrap();
    assert_eq!(source.hits(), 1);

    // the provider rotates: a second key appears
    source.serve(vec![
        keys.jwk("k1", Some("ES256")),
        keys.jwk("k2", Some("ES256")),
    ]);
    let rotated = keys.token(header(json!({"kid": "k2"})), claims(json!({})));
    refused(&v, &rotated).await; // the cache is only seconds old: it is not asked again yet
    assert_eq!(source.hits(), 1);
    tokio::time::advance(Duration::from_secs(61)).await;
    check(&v, &rotated).await.unwrap();
    assert_eq!(
        source.hits(),
        2,
        "an unknown kid after a minute fetches the keys again"
    );

    // a flood of tokens with an unknown kid cannot make the server hammer the provider
    for _ in 0..20 {
        refused(
            &v,
            &keys.token(header(json!({"kid": "ghost"})), claims(json!({}))),
        )
        .await;
    }
    assert_eq!(source.hits(), 2);
}

#[tokio::test]
async fn an_unreachable_or_broken_key_endpoint_is_unavailable_not_invalid() {
    let keys = Keys::new();
    let t = keys.token(header(json!({})), claims(json!({})));

    let (v, source) = setup(&keys);
    source.down.store(true, Ordering::SeqCst);
    assert!(matches!(
        check(&v, &t).await,
        Err(TokenError::Unavailable(_))
    ));

    for body in ["not json", "{\"nokeys\":1}", "[]"] {
        let source = Arc::new(Canned::default());
        *source.body.lock().unwrap() = body.to_string();
        let v = Verifier::new(Source(source));
        assert!(
            matches!(check(&v, &t).await, Err(TokenError::Unavailable(_))),
            "{body}"
        );
    }
}

#[tokio::test]
async fn rsa_tokens_verify_rs256_and_ps256() {
    let data: Value = serde_json::from_str(include_str!("data/jwt-vectors.json")).unwrap();
    let source = Arc::new(Canned::default());
    source.serve(data["jwks"]["keys"].as_array().unwrap().clone());
    let v = Verifier::new(Source(source));
    let at = |name: &str| data["tokens"][name].as_str().unwrap().to_string();
    let opts = VerifyOptions {
        issuer: data["issuer"].as_str().unwrap(),
        audience: data["audience"].as_str().unwrap(),
        jwks_uri: JWKS,
        now: data["now"].as_i64().unwrap(),
    };
    assert_eq!(v.verify(&at("rs256"), &opts).await.unwrap().sub, "u1");
    assert_eq!(v.verify(&at("ps256"), &opts).await.unwrap().sub, "u1");
    for bad in ["rs256_tampered_payload", "rs256_other_key"] {
        assert!(
            matches!(v.verify(&at(bad), &opts).await, Err(TokenError::Invalid(_))),
            "{bad}"
        );
    }
}

#[tokio::test]
async fn a_key_of_the_wrong_type_or_curve_is_not_used() {
    let keys = Keys::new();
    let token = keys.token(header(json!({})), claims(json!({})));
    let mut wrong_curve = keys.jwk("k1", None);
    wrong_curve["crv"] = json!("P-384");
    let mut wrong_type = keys.jwk("k1", None);
    wrong_type["kty"] = json!("RSA");
    for key in [wrong_curve, wrong_type] {
        let source = Arc::new(Canned::default());
        source.serve(vec![key]);
        refused(&Verifier::new(Source(source)), &token).await;
    }
}
