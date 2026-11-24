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
