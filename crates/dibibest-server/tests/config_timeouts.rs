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
