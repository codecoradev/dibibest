use axum::http::{HeaderMap, HeaderValue};
use dibibest_server::auth::codec::{b64url, b64url_decode};
use dibibest_server::auth::session::{
    pkce_challenge, random_token, safe_return_to, same_origin, sign, verify,
};
use serde::{Deserialize, Serialize};

const SECRET: &str = "xxxxxxxxxxxxxxxxsession-secret-long-enough";

#[derive(Serialize, Deserialize, PartialEq, Debug)]
struct Session {
    email: String,
    exp: i64,
}

fn session(exp: i64) -> Session {
    Session {
        email: "a@b.c".into(),
        exp,
    }
}

#[test]
fn base64url_round_trips_and_refuses_anything_else() {
    for bytes in [
        &b""[..],
        b"a",
        b"ab",
        b"abc",
        b"\xff\xfe\xfd\x00\x01",
        &[0u8; 33][..],
    ] {
        assert_eq!(b64url_decode(&b64url(bytes)).as_deref(), Some(bytes));
    }
    assert_eq!(b64url(b"\xfb\xff"), "-_8", "the URL alphabet, no padding");
    for bad in ["a=", "a+b", "a/b", "a b", "a\n", "é", "a"] {
        assert_eq!(b64url_decode(bad), None, "{bad:?}");
    }
}

#[test]
fn a_signed_value_round_trips_until_it_expires() {
    let token = sign(SECRET, &session(1000));
    assert_eq!(verify::<Session>(SECRET, &token, 999), Some(session(1000)));
    assert_eq!(
        verify::<Session>(SECRET, &token, 1000),
        None,
        "exp is exclusive"
    );
    assert_eq!(verify::<Session>(SECRET, &token, 5000), None);
}

#[test]
fn a_signed_value_with_a_changed_payload_or_another_secret_is_refused() {
    let token = sign(SECRET, &session(1000));
    let (body, mac) = token.split_once('.').unwrap();
    let forged = b64url(br#"{"email":"evil@x","exp":1000}"#);
    assert_eq!(
        verify::<Session>(SECRET, &format!("{forged}.{mac}"), 0),
        None
    );
    assert_eq!(
        verify::<Session>("another-secret-of-sufficient-length!!", &token, 0),
        None
    );
    assert!(verify::<Session>(SECRET, &format!("{body}.{mac}"), 0).is_some());
}

#[test]
fn malformed_signed_values_are_refused() {
    let token = sign(SECRET, &session(1000));
    for bad in [
        "garbage",
        "",
        ".",
        "a.",
        ".b",
        &format!("{token}.extra"),
        "a.b.c",
    ] {
        assert_eq!(verify::<Session>(SECRET, bad, 0), None, "{bad:?}");
    }
    // a valid signature around a body that is not the expected shape
    let body = b64url(br#"{"nope":1}"#);
    let sig = token.split_once('.').unwrap().1;
    assert_eq!(verify::<Session>(SECRET, &format!("{body}.{sig}"), 0), None);
}

#[test]
fn a_value_without_a_numeric_exp_never_verifies_even_when_correctly_signed() {
    #[derive(Serialize)]
    struct NoExp {
        email: &'static str,
    }
    let token = sign(SECRET, &NoExp { email: "a@b.c" });
    assert_eq!(verify::<Session>(SECRET, &token, 0), None);
}

#[test]
fn random_tokens_are_long_enough_url_safe_and_different() {
    let a = random_token(32);
    let b = random_token(32);
    assert_ne!(a, b);
    assert_eq!(b64url_decode(&a).map(|v| v.len()), Some(32));
    assert_eq!(b64url_decode(&random_token(48)).map(|v| v.len()), Some(48));
}

#[test]
fn pkce_challenge_matches_the_rfc_7636_example() {
    assert_eq!(
        pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
}

#[test]
fn return_to_only_accepts_same_site_paths() {
    let app = "/app".to_string();
    assert_eq!(safe_return_to(Some("/app")), "/app");
    for bad in [
        "//evil.test/x",
        "https://evil.test",
        "/\\evil.test",
        "relative",
        "",
    ] {
        assert_eq!(safe_return_to(Some(bad)), app, "{bad:?}");
    }
    assert_eq!(safe_return_to(None), app);
    // browsers strip tab, CR and LF and read a backslash as a slash in a Location: all of these must fall back
    for bad in [
        "/\t/evil.test",
        "/\n/evil.test",
        "/\r/evil.test",
        "/\\/evil.test",
        "/x\\..\\..//evil.test",
        "/\u{0}/evil",
        "/\u{7f}/x",
    ] {
        assert_eq!(safe_return_to(Some(bad)), app, "{bad:?}");
    }
    // what /auth/login receives for ?return_to=/%09/evil.test, once the query is decoded
    assert_eq!(safe_return_to(Some("/\t/evil.test")), app);
    // ordinary paths survive, normalised
    assert_eq!(safe_return_to(Some("/app?x=1#y")), "/app?x=1#y");
    assert_eq!(safe_return_to(Some("/a/../app")), "/app");
    assert_eq!(safe_return_to(Some("/p/some-id")), "/p/some-id");
}

fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
    let mut h = HeaderMap::new();
    for (k, v) in pairs {
        h.insert(*k, HeaderValue::from_static(v));
    }
    h
}

#[test]
fn a_state_changing_request_must_come_from_the_same_origin() {
    let me = "https://dibibest.codecora.dev";
    assert!(same_origin(
        &headers(&[("origin", "https://dibibest.codecora.dev")]),
        me
    ));
    assert!(!same_origin(
        &headers(&[("origin", "https://evil.test")]),
        me
    ));
    assert!(
        !same_origin(&headers(&[("origin", "http://dibibest.codecora.dev")]), me),
        "scheme counts"
    );
    assert!(!same_origin(&headers(&[("origin", "null")]), me));
    // no Origin: fetch metadata decides
    assert!(same_origin(
        &headers(&[("sec-fetch-site", "same-origin")]),
        me
    ));
    assert!(same_origin(
        &headers(&[("sec-fetch-site", "same-site")]),
        me
    ));
    assert!(!same_origin(
        &headers(&[("sec-fetch-site", "cross-site")]),
        me
    ));
    assert!(!same_origin(&headers(&[("sec-fetch-site", "none")]), me));
    assert!(!same_origin(&headers(&[]), me), "nothing to go on: refuse");
    // an Origin that disagrees wins over fetch metadata that agrees
    assert!(!same_origin(
        &headers(&[
            ("origin", "https://evil.test"),
            ("sec-fetch-site", "same-origin")
        ]),
        me
    ));
}

#[test]
fn the_expiry_is_enforced_for_any_payload_type_not_only_ones_that_declare_it() {
    #[derive(Serialize)]
    struct NoExp {
        email: &'static str,
    }
    // A caller that reads the payload as loose JSON must not get a value that never expires.
    let token = sign(SECRET, &NoExp { email: "a@b.c" });
    assert_eq!(verify::<serde_json::Value>(SECRET, &token, 0), None);
    let expiring = sign(SECRET, &session(10));
    assert!(verify::<serde_json::Value>(SECRET, &expiring, 9).is_some());
    assert_eq!(verify::<serde_json::Value>(SECRET, &expiring, 10), None);
}
