//! `DIBIBEST_BACKUP_DIR`, `DIBIBEST_BACKUP_INTERVAL_HOURS` and `DIBIBEST_BACKUP_KEEP` (DIB-7, issue #95).

use dibibest_server::config::{Config, ConfigError};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

fn lookup(pairs: &[(&str, &str)]) -> Result<Config, ConfigError> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    Config::from_lookup(|k| map.get(k).cloned())
}

#[test]
fn backups_are_off_unless_a_directory_is_given() {
    let c = lookup(&[]).unwrap();
    assert_eq!(c.backup_dir, None);
    assert_eq!(c.backup_interval, Duration::from_secs(24 * 3600));
    assert_eq!(c.backup_keep, 7);
}

#[test]
fn the_three_settings_are_read_from_their_own_variables() {
    let c = lookup(&[
        ("DIBIBEST_BACKUP_DIR", "/backup"),
        ("DIBIBEST_BACKUP_INTERVAL_HOURS", "6"),
        ("DIBIBEST_BACKUP_KEEP", "30"),
    ])
    .unwrap();
    assert_eq!(c.backup_dir, Some(PathBuf::from("/backup")));
    assert_eq!(c.backup_interval, Duration::from_secs(6 * 3600));
    assert_eq!(c.backup_keep, 30);
    let only_dir = lookup(&[("DIBIBEST_BACKUP_DIR", "/backup")]).unwrap();
    assert_eq!(only_dir.backup_interval, Duration::from_secs(24 * 3600));
    assert_eq!(only_dir.backup_keep, 7);
}

#[test]
fn a_blank_value_counts_as_unset() {
    let c = lookup(&[
        ("DIBIBEST_BACKUP_DIR", "  "),
        ("DIBIBEST_BACKUP_INTERVAL_HOURS", ""),
        ("DIBIBEST_BACKUP_KEEP", " "),
    ])
    .unwrap();
    assert_eq!(c.backup_dir, None);
    assert_eq!(c.backup_keep, 7);
}

#[test]
fn an_interval_or_a_keep_without_a_directory_is_an_error_not_silently_ignored() {
    for var in ["DIBIBEST_BACKUP_INTERVAL_HOURS", "DIBIBEST_BACKUP_KEEP"] {
        let err = lookup(&[(var, "3")]).unwrap_err().to_string();
        assert!(err.contains("DIBIBEST_BACKUP_DIR"), "{var}: {err}");
    }
}

#[test]
fn the_ranges_are_one_hour_to_a_year_and_one_to_a_thousand_files() {
    let dir = ("DIBIBEST_BACKUP_DIR", "/backup");
    for (var, bad) in [
        ("DIBIBEST_BACKUP_INTERVAL_HOURS", "0"),
        ("DIBIBEST_BACKUP_INTERVAL_HOURS", "8761"),
        ("DIBIBEST_BACKUP_INTERVAL_HOURS", "often"),
        ("DIBIBEST_BACKUP_INTERVAL_HOURS", "-1"),
        ("DIBIBEST_BACKUP_KEEP", "0"),
        ("DIBIBEST_BACKUP_KEEP", "1001"),
        ("DIBIBEST_BACKUP_KEEP", "many"),
    ] {
        let err = lookup(&[dir, (var, bad)]).unwrap_err().to_string();
        assert!(err.contains(var), "{var}={bad}: {err}");
    }
    assert!(lookup(&[dir, ("DIBIBEST_BACKUP_INTERVAL_HOURS", "1")]).is_ok());
    assert!(lookup(&[dir, ("DIBIBEST_BACKUP_INTERVAL_HOURS", "8760")]).is_ok());
    assert!(lookup(&[dir, ("DIBIBEST_BACKUP_KEEP", "1")]).is_ok());
    assert!(lookup(&[dir, ("DIBIBEST_BACKUP_KEEP", "1000")]).is_ok());
}
