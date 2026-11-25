//! `DIBIBEST_HEADER_TIMEOUT_SECS` and `DIBIBEST_REQUEST_TIMEOUT_SECS` (issue #104).

use dibibest_server::config::{Config, ConfigError};
use std::collections::HashMap;
use std::time::Duration;

fn lookup(pairs: &[(&str, &str)]) -> Result<Config, ConfigError> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    Config::from_lookup(|k| map.get(k).cloned())
}

const VARS: [&str; 2] = [
    "DIBIBEST_HEADER_TIMEOUT_SECS",
    "DIBIBEST_REQUEST_TIMEOUT_SECS",
];

#[test]
fn the_defaults_are_ones_a_browser_or_an_mcp_client_never_meets() {
    let c = lookup(&[]).unwrap();
    assert_eq!(c.header_timeout, Duration::from_secs(120));
    // the same limit ends a kept-alive connection that sat idle, so it must outlast a proxy's idle pool (Traefik's
    // is 90 s); a shorter one closes connections the proxy still means to reuse, and the POST on one fails
    assert!(c.header_timeout > Duration::from_secs(90));
    assert_eq!(c.request_timeout, Duration::from_secs(30));
}

#[test]
fn each_timeout_is_read_from_its_own_variable() {
    let c = lookup(&[
        ("DIBIBEST_HEADER_TIMEOUT_SECS", "3"),
        ("DIBIBEST_REQUEST_TIMEOUT_SECS", "45"),
    ])
    .unwrap();
    assert_eq!(c.header_timeout, Duration::from_secs(3));
    assert_eq!(c.request_timeout, Duration::from_secs(45));
    let only_header = lookup(&[("DIBIBEST_HEADER_TIMEOUT_SECS", "3")]).unwrap();
    assert_eq!(only_header.request_timeout, Duration::from_secs(30));
    let only_request = lookup(&[("DIBIBEST_REQUEST_TIMEOUT_SECS", "45")]).unwrap();
    assert_eq!(only_request.header_timeout, Duration::from_secs(120));
}

#[test]
fn a_blank_value_counts_as_unset() {
    for var in VARS {
        for blank in ["", "   "] {
            let c = lookup(&[(var, blank)]).unwrap();
            assert_eq!(c.header_timeout, Duration::from_secs(120), "{var}");
            assert_eq!(c.request_timeout, Duration::from_secs(30), "{var}");
        }
    }
}

#[test]
fn a_bad_value_is_an_error_that_names_the_variable() {
    for var in VARS {
        for bad in ["soon", "-1", "1.5", "10s"] {
            let err = lookup(&[(var, bad)]).unwrap_err().to_string();
            assert!(err.contains(var), "{var}={bad} gave: {err}");
        }
    }
}

#[test]
fn zero_is_an_error_not_a_way_to_switch_the_limit_off() {
    for var in VARS {
        let err = lookup(&[(var, "0")]).unwrap_err().to_string();
        assert!(err.contains(var), "{var}=0 gave: {err}");
    }
}

#[test]
fn a_value_beyond_a_day_is_an_error_not_a_limit_nobody_will_meet() {
    for var in VARS {
        for big in ["86401", "18446744073709551615", "18446744073709551616"] {
            let err = lookup(&[(var, big)]).unwrap_err().to_string();
            assert!(err.contains(var), "{var}={big} gave: {err}");
        }
        assert!(lookup(&[(var, "86400")]).is_ok(), "a day is the largest");
    }
}

#[test]
fn the_cap_on_open_connections_has_a_default_and_a_range() {
    assert_eq!(lookup(&[]).unwrap().max_connections, 1024);
    let c = lookup(&[("DIBIBEST_MAX_CONNECTIONS", "50")]).unwrap();
    assert_eq!(c.max_connections, 50);
    assert_eq!(
        lookup(&[("DIBIBEST_MAX_CONNECTIONS", "  ")])
            .unwrap()
            .max_connections,
        1024,
        "a blank value counts as unset"
    );
    for bad in ["0", "1000001", "many", "-1"] {
        let err = lookup(&[("DIBIBEST_MAX_CONNECTIONS", bad)])
            .unwrap_err()
            .to_string();
        assert!(err.contains("DIBIBEST_MAX_CONNECTIONS"), "{bad}: {err}");
    }
    assert!(lookup(&[("DIBIBEST_MAX_CONNECTIONS", "1000000")]).is_ok());
}

// ── the write timeout and the per-address cap (issue #109) ──────────────────────────────────────────────

#[test]
fn the_write_timeout_has_a_default_a_range_and_treats_blank_as_unset() {
    const VAR: &str = "DIBIBEST_WRITE_TIMEOUT_SECS";
    assert_eq!(lookup(&[]).unwrap().write_timeout, Duration::from_secs(60));
    assert_eq!(
        lookup(&[(VAR, "5")]).unwrap().write_timeout,
        Duration::from_secs(5)
    );
    for blank in ["", "  "] {
        assert_eq!(
            lookup(&[(VAR, blank)]).unwrap().write_timeout,
            Duration::from_secs(60)
        );
    }
    for bad in ["soon", "-1", "1.5", "0", "86401", "18446744073709551616"] {
        let err = lookup(&[(VAR, bad)]).unwrap_err().to_string();
        assert!(err.contains(VAR), "{bad}: {err}");
    }
    assert!(lookup(&[(VAR, "86400")]).is_ok());
}

#[test]
fn the_cap_per_address_is_off_by_default_and_zero_means_off() {
    const VAR: &str = "DIBIBEST_MAX_CONNECTIONS_PER_IP";
    assert_eq!(lookup(&[]).unwrap().max_connections_per_ip, 0);
    assert_eq!(lookup(&[(VAR, "0")]).unwrap().max_connections_per_ip, 0);
    assert_eq!(lookup(&[(VAR, " ")]).unwrap().max_connections_per_ip, 0);
    assert_eq!(lookup(&[(VAR, "20")]).unwrap().max_connections_per_ip, 20);
    assert!(lookup(&[(VAR, "1000000")]).is_ok());
    for bad in ["1000001", "many", "-1", "1.5"] {
        let err = lookup(&[(VAR, bad)]).unwrap_err().to_string();
        assert!(err.contains(VAR), "{bad}: {err}");
    }
}

#[test]
fn the_cap_per_address_refuses_to_start_behind_a_trusted_proxy() {
    // Behind a proxy every connection comes from the proxy's own address, so the cap would limit the proxy.
    let err = lookup(&[
        ("DIBIBEST_MAX_CONNECTIONS_PER_IP", "20"),
        ("DIBIBEST_TRUSTED_PROXIES", "1"),
    ])
    .unwrap_err();
    assert_eq!(err.var, "DIBIBEST_MAX_CONNECTIONS_PER_IP");
    assert!(err.reason.contains("DIBIBEST_TRUSTED_PROXIES"), "{err}");
    // off, or no proxy: fine
    assert!(lookup(&[
        ("DIBIBEST_MAX_CONNECTIONS_PER_IP", "0"),
        ("DIBIBEST_TRUSTED_PROXIES", "1"),
    ])
    .is_ok());
    assert!(lookup(&[
        ("DIBIBEST_MAX_CONNECTIONS_PER_IP", "20"),
        ("DIBIBEST_TRUSTED_PROXIES", "0"),
    ])
    .is_ok());
}
