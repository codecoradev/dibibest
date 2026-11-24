use dibibest_server::config::Config;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

fn lookup(pairs: &[(&str, &str)]) -> Result<Config, dibibest_server::config::ConfigError> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    Config::from_lookup(|k| map.get(k).cloned())
}

#[test]
fn nothing_set_gives_safe_defaults() {
    let c = lookup(&[]).unwrap();
    assert_eq!(
        c.addr.to_string(),
        "127.0.0.1:8080",
        "loopback unless told otherwise"
    );
    assert_eq!(c.data_dir, PathBuf::from("./data"));
    assert_eq!(c.web_dir, PathBuf::from("./web"));
    assert_eq!(
        c.trusted_proxies, 0,
        "forwarded headers are not trusted by default"
    );
    assert_eq!(c.rate_per_minute, 600);
    assert_eq!(c.max_body_bytes, 1024 * 1024);
}

#[test]
fn every_setting_is_read_from_its_variable() {
    let c = lookup(&[
        ("DIBIBEST_ADDR", "0.0.0.0:9000"),
        ("DIBIBEST_DATA_DIR", "/data"),
        ("DIBIBEST_WEB_DIR", "/app/web"),
        ("DIBIBEST_TRUSTED_PROXIES", "2"),
        ("DIBIBEST_RATE_PER_MINUTE", "120"),
        ("DIBIBEST_MAX_BODY_BYTES", "4096"),
        ("DIBIBEST_SHUTDOWN_GRACE_SECS", "7"),
    ])
    .unwrap();
    assert_eq!(c.addr.to_string(), "0.0.0.0:9000");
    assert_eq!(c.data_dir, PathBuf::from("/data"));
    assert_eq!(c.web_dir, PathBuf::from("/app/web"));
    assert_eq!(c.trusted_proxies, 2);
    assert_eq!(c.rate_per_minute, 120);
    assert_eq!(c.max_body_bytes, 4096);
    assert_eq!(c.shutdown_grace, Duration::from_secs(7));
}

#[test]
fn a_bad_value_is_an_error_that_names_the_variable() {
    for (var, bad) in [
        ("DIBIBEST_ADDR", "not an address"),
        ("DIBIBEST_TRUSTED_PROXIES", "maybe"),
        ("DIBIBEST_TRUSTED_PROXIES", "-1"),
        ("DIBIBEST_TRUSTED_PROXIES", "99"),
        ("DIBIBEST_RATE_PER_MINUTE", "-1"),
        ("DIBIBEST_RATE_PER_MINUTE", "0"),
        ("DIBIBEST_MAX_BODY_BYTES", "lots"),
        ("DIBIBEST_SHUTDOWN_GRACE_SECS", "x"),
    ] {
        let err = lookup(&[(var, bad)]).unwrap_err().to_string();
        assert!(err.contains(var), "{var}={bad} gave: {err}");
    }
}

// ── sign-in (issue #93) ────────────────────────────────────────────────────────────────────────────────

use dibibest_server::config::TokenAuth;

const SECRET: &str = "0123456789abcdef0123456789abcdef-session";

fn signin(extra: &[(&str, &str)]) -> Result<Config, dibibest_server::config::ConfigError> {
    let mut pairs = vec![
        ("DIBIBEST_PUBLIC_URL", "https://dibibest.codecora.dev"),
        ("DIBIBEST_OIDC_ISSUER", "https://identity.example/api/auth/"),
        ("DIBIBEST_OIDC_CLIENT_ID", "dibibest-web"),
        ("DIBIBEST_OIDC_CLIENT_SECRET", "s3cret"),
        ("DIBIBEST_SESSION_SECRET", SECRET),
    ];
    for (k, v) in extra {
        pairs.retain(|(name, _)| name != k);
        pairs.push((k, v));
    }
    lookup(&pairs)
}

#[test]
fn sign_in_is_off_when_nothing_about_it_is_set() {
    let c = lookup(&[]).unwrap();
    assert!(c.oidc.is_none());
    assert!(c.public_url.is_none());
}

#[test]
fn sign_in_is_on_when_everything_is_set() {
    let c = signin(&[]).unwrap();
    let o = c.oidc.expect("sign-in is configured");
    assert_eq!(
        o.issuer, "https://identity.example/api/auth",
        "a trailing slash is dropped"
    );
    assert_eq!(o.client_id, "dibibest-web");
    assert_eq!(o.client_secret, "s3cret");
    assert_eq!(o.session_secret, SECRET);
    assert_eq!(
        o.token_auth,
        TokenAuth::Basic,
        "RFC 6749 basic is the default"
    );
    assert_eq!(
        c.public_url.as_deref(),
        Some("https://dibibest.codecora.dev")
    );
}

#[test]
fn a_half_configured_sign_in_is_an_error_that_names_what_is_missing() {
    for missing in [
        "DIBIBEST_OIDC_ISSUER",
        "DIBIBEST_OIDC_CLIENT_ID",
        "DIBIBEST_OIDC_CLIENT_SECRET",
        "DIBIBEST_SESSION_SECRET",
    ] {
        let err = signin(&[(missing, "")]).unwrap_err().to_string();
        assert!(err.contains(missing), "{missing}: {err}");
    }
}

#[test]
fn placeholders_count_as_not_set() {
    for placeholder in ["...", "xxxx", "changeme", "TODO", "  "] {
        let all = [
            ("DIBIBEST_OIDC_ISSUER", placeholder),
            ("DIBIBEST_OIDC_CLIENT_ID", placeholder),
            ("DIBIBEST_OIDC_CLIENT_SECRET", placeholder),
            ("DIBIBEST_SESSION_SECRET", placeholder),
        ];
        assert!(lookup(&all).unwrap().oidc.is_none(), "{placeholder:?}");
    }
}

#[test]
fn a_session_secret_that_is_too_short_is_refused() {
    let err = signin(&[("DIBIBEST_SESSION_SECRET", "too-short")])
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("DIBIBEST_SESSION_SECRET") && err.contains("32"),
        "{err}"
    );
}

#[test]
fn the_provider_must_be_https_except_on_this_machine() {
    for bad in [
        "http://identity.example/api/auth",
        "ftp://identity.example",
        "identity.example",
        "https://",
        "https://u:p@identity.example",
    ] {
        let err = signin(&[("DIBIBEST_OIDC_ISSUER", bad)])
            .unwrap_err()
            .to_string();
        assert!(err.contains("DIBIBEST_OIDC_ISSUER"), "{bad}: {err}");
    }
    for ok in [
        "http://127.0.0.1:9000/api/auth",
        "http://localhost:9000",
        "http://[::1]:9000/x",
        "https://identity.example",
    ] {
        assert!(signin(&[("DIBIBEST_OIDC_ISSUER", ok)]).is_ok(), "{ok}");
    }
}

#[test]
fn the_public_address_is_an_origin_and_is_needed_for_sign_in() {
    let err = signin(&[("DIBIBEST_PUBLIC_URL", "")])
        .unwrap_err()
        .to_string();
    assert!(err.contains("DIBIBEST_PUBLIC_URL"), "{err}");
    for bad in [
        "https://dibibest.codecora.dev/app",
        "https://dibibest.codecora.dev/?x=1",
        "https://dibibest.codecora.dev/#f",
        "http://dibibest.codecora.dev",
        "dibibest.codecora.dev",
        "https://u:p@dibibest.codecora.dev",
    ] {
        let err = signin(&[("DIBIBEST_PUBLIC_URL", bad)])
            .unwrap_err()
            .to_string();
        assert!(err.contains("DIBIBEST_PUBLIC_URL"), "{bad}: {err}");
    }
    let c = signin(&[("DIBIBEST_PUBLIC_URL", "https://dibibest.codecora.dev/")]).unwrap();
    assert_eq!(
        c.public_url.as_deref(),
        Some("https://dibibest.codecora.dev"),
        "a trailing slash is dropped"
    );
    let c = signin(&[("DIBIBEST_PUBLIC_URL", "http://localhost:8080")]).unwrap();
    assert_eq!(c.public_url.as_deref(), Some("http://localhost:8080"));
    // the public address alone, with sign-in off, is fine: the MCP endpoint needs it later
    assert_eq!(
        lookup(&[("DIBIBEST_PUBLIC_URL", "https://dibibest.codecora.dev")])
            .unwrap()
            .public_url
            .as_deref(),
        Some("https://dibibest.codecora.dev")
    );
}

#[test]
fn the_token_endpoint_login_style_is_basic_or_post() {
    assert_eq!(
        signin(&[("DIBIBEST_OIDC_TOKEN_AUTH", "post")])
            .unwrap()
            .oidc
            .unwrap()
            .token_auth,
        TokenAuth::Post
    );
    assert_eq!(
        signin(&[("DIBIBEST_OIDC_TOKEN_AUTH", "basic")])
            .unwrap()
            .oidc
            .unwrap()
            .token_auth,
        TokenAuth::Basic
    );
    let err = signin(&[("DIBIBEST_OIDC_TOKEN_AUTH", "digest")])
        .unwrap_err()
        .to_string();
    assert!(err.contains("DIBIBEST_OIDC_TOKEN_AUTH"), "{err}");
}

#[test]
fn a_secret_never_shows_up_in_the_debug_output_of_the_config() {
    let text = format!("{:?}", signin(&[]).unwrap());
    assert!(!text.contains("s3cret") && !text.contains(SECRET), "{text}");
}
