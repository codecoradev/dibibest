//! Scheduled backups of the SQLite database (issue #95).
//!
//! A backup is a consistent copy made by the store (`VACUUM INTO`), never a copy of the live `.db` file, which
//! would miss what is still in the `-wal` file. It is written under a name that is not a backup yet
//! (`<name>.partial`), made durable, and only then renamed, so a copy that was cut short by a crash or a full disk
//! is never mistaken for a complete one and is removed by the next run.
//!
//! The scheduler runs inside the server process: a second process cannot be started on the same data directory
//! (the lock), and one in-process task needs no cron, no second container and no access to the live files.
//!
//! The name of a backup is its UTC time to the second, `dibibest-20261124T082320Z.db`. The schedule and the
//! retention read the time from the name, not from the file's modification time, so a copied or restored
//! directory keeps its meaning.

use crate::config::Config;
use dibibest_store::{SqliteStore, StoreError};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::watch;
use tokio::task::JoinHandle;

const PREFIX: &str = "dibibest-";
const SUFFIX: &str = "Z.db";
const PARTIAL: &str = ".partial";
/// How soon a failed backup is tried again at the latest (a short interval is used as it is).
const RETRY_AT_MOST: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    #[error("{what}: {source}")]
    Io {
        what: String,
        #[source]
        source: std::io::Error,
    },
    #[error("store: {0}")]
    Store(#[from] StoreError),
}

fn io(what: impl Into<String>) -> impl FnOnce(std::io::Error) -> BackupError {
    let what = what.into();
    move |source| BackupError::Io { what, source }
}

#[derive(Debug, Clone)]
pub struct BackupConfig {
    pub dir: PathBuf,
    /// How often a backup is made.
    pub interval: Duration,
    /// How many backups are kept (the newest).
    pub keep: usize,
    /// How soon a failed attempt is made again.
    pub retry: Duration,
}

impl BackupConfig {
    /// `None` when no backup directory is configured.
    pub fn from_config(config: &Config) -> Option<Self> {
        let dir = config.backup_dir.clone()?;
        Some(Self {
            dir,
            interval: config.backup_interval,
            keep: config.backup_keep,
            retry: config.backup_interval.min(RETRY_AT_MOST),
        })
    }
}

// ---- names ------------------------------------------------------------------------------------------------

/// Days since 1970-01-01 of a civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The civil date of a day count since 1970-01-01.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// `dibibest-20261124T082320Z.db` for a Unix time in seconds.
pub fn file_name(unix_secs: i64) -> String {
    let (days, rest) = (unix_secs.div_euclid(86_400), unix_secs.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(days);
    let (h, mi, s) = (rest / 3_600, rest % 3_600 / 60, rest % 60);
    format!("{PREFIX}{y:04}{m:02}{d:02}T{h:02}{mi:02}{s:02}{SUFFIX}")
}

/// The Unix time in a backup's file name, `None` for any other name (a partial file, a date that does not exist,
/// a file somebody else put there).
pub fn parse_file_name(name: &str) -> Option<i64> {
    let stamp = name.strip_prefix(PREFIX)?.strip_suffix(SUFFIX)?;
    let bytes = stamp.as_bytes();
    if bytes.len() != 15 || bytes[8] != b'T' {
        return None;
    }
    if !bytes
        .iter()
        .enumerate()
        .all(|(i, b)| i == 8 || b.is_ascii_digit())
    {
        return None;
    }
    let n = |range: std::ops::Range<usize>| stamp[range].parse::<i64>().ok();
    let (y, m, d) = (n(0..4)?, n(4..6)?, n(6..8)?);
    let (h, mi, s) = (n(9..11)?, n(11..13)?, n(13..15)?);
    let t = days_from_civil(y, m, d) * 86_400 + h * 3_600 + mi * 60 + s;
    // The round trip is the whole check: a month 13, a day 31 of April, an hour 25 or a February 30 all come back
    // from `file_name` as a different date, so they are refused here.
    (file_name(t) == name).then_some(t)
}

fn backups(dir: &Path) -> Vec<(i64, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            // a directory (or anything else) that merely has such a name is not a backup
            if !e.file_type().is_ok_and(|t| t.is_file()) {
                return None;
            }
            let t = parse_file_name(&e.file_name().to_string_lossy())?;
            Some((t, e.path()))
        })
        .collect()
}

/// The time of the newest complete backup in `dir`.
pub fn newest_backup(dir: &Path) -> Option<i64> {
    backups(dir).into_iter().map(|(t, _)| t).max()
}

/// Deletes all but the newest `keep` backups, and never `protect` (the one that has just been made). Files that are
/// not backups are never touched. If the clock was stepped back, the backup just made can be older by name than
/// others; it is kept anyway, and the directory holds one more until the clock catches up.
pub fn prune(dir: &Path, keep: usize, protect: Option<&Path>) -> std::io::Result<()> {
    let mut all = backups(dir);
    all.sort_by_key(|(t, _)| std::cmp::Reverse(*t));
    let mut first_error = None;
    for (_, path) in all.into_iter().skip(keep) {
        if Some(path.as_path()) == protect {
            continue;
        }
        // one file that cannot be removed must not keep the others from being removed
        if let Err(e) = std::fs::remove_file(&path) {
            first_error.get_or_insert(e);
        }
    }
    first_error.map_or(Ok(()), Err)
}

fn remove_partials(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.filter_map(Result::ok) {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with(PREFIX) && name.ends_with(&format!("{SUFFIX}{PARTIAL}")) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Closes `dir` to everybody but its owner, and every backup in it too (`0700` and `0600`). A backup holds every
/// project in the database, so it is not for other users of the host; with the directory closed first, a file made
/// inside it is not readable by them even in the moment before its own mode is set. If the mode cannot be changed
/// (the directory belongs to somebody else) that is logged and the backups go on, because a backup that is readable
/// is still better than none.
#[cfg(unix)]
fn tighten(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let set = |path: &Path, mode: u32| {
        if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)) {
            tracing::warn!(
                "backup: cannot restrict {} to its owner: {e}",
                path.display()
            );
        }
    };
    set(dir, 0o700);
    for (_, path) in backups(dir) {
        set(&path, 0o600);
    }
}

#[cfg(not(unix))]
fn tighten(_dir: &Path) {}

/// Makes sure backups can be written to `dir` (creates it, writes and removes a small file).
pub fn check_dir(dir: &Path) -> Result<(), BackupError> {
    std::fs::create_dir_all(dir).map_err(io(format!("cannot create {}", dir.display())))?;
    tighten(dir);
    let probe = dir.join(".dibibest-write-test");
    std::fs::write(&probe, b"ok").map_err(io(format!("cannot write to {}", dir.display())))?;
    let _ = std::fs::remove_file(probe);
    Ok(())
}

// ---- one backup -------------------------------------------------------------------------------------------

/// Makes one backup named for `now_unix`, then keeps the newest `keep`. The returned path is complete and durable.
pub fn run_once(
    store: &SqliteStore,
    dir: &Path,
    now_unix: i64,
    keep: usize,
) -> Result<PathBuf, BackupError> {
    std::fs::create_dir_all(dir).map_err(io(format!("cannot create {}", dir.display())))?;
    // closed before anything is written into it
    tighten(dir);
    remove_partials(dir);
    let name = file_name(now_unix);
    let final_path = dir.join(&name);
    if final_path.exists() {
        return Err(BackupError::Io {
            what: format!(
                "{} exists; a backup does not overwrite",
                final_path.display()
            ),
            source: std::io::ErrorKind::AlreadyExists.into(),
        });
    }
    let partial = dir.join(format!("{name}{PARTIAL}"));
    store.backup_to(&partial)?;
    // durable before it gets its real name, and the name durable too
    std::fs::File::open(&partial)
        .and_then(|f| f.sync_all())
        .map_err(io(format!("cannot sync {}", partial.display())))?;
    std::fs::rename(&partial, &final_path)
        .map_err(io(format!("cannot rename {}", partial.display())))?;
    let _ = std::fs::File::open(dir).and_then(|d| d.sync_all());
    tighten(dir);
    if let Err(e) = prune(dir, keep, Some(&final_path)) {
        // the backup itself is fine; old ones are left for the next run
        tracing::warn!(
            "backup: could not remove old backups in {}: {e}",
            dir.display()
        );
    }
    Ok(final_path)
}

// ---- the scheduler ----------------------------------------------------------------------------------------

/// A Unix time from before 2020 means the clock was never set (or reads as 0): a backup named for it would be
/// filed under 1970 and sorted as the oldest.
pub fn plausible_clock(unix_secs: i64) -> bool {
    unix_secs >= 1_577_836_800
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// How long to wait before the first backup: none if there is no backup or the newest is older than the interval,
/// otherwise what is left of the interval, so that restarting the server does not make a backup every time.
fn first_wait(cfg: &BackupConfig) -> Duration {
    let Some(newest) = newest_backup(&cfg.dir) else {
        return Duration::ZERO;
    };
    let age = Duration::from_secs(u64::try_from(now_unix() - newest).unwrap_or(0));
    cfg.interval.saturating_sub(age).min(cfg.interval)
}

/// A running scheduler. [`Backup::stop`] ends it, after the backup it is in the middle of.
pub struct Backup {
    stop: Arc<watch::Sender<bool>>,
    task: JoinHandle<()>,
}

impl Backup {
    /// What tells the scheduler to stop. The server holds a copy so it can say so the moment a shutdown begins,
    /// while it is still waiting for requests, instead of after.
    pub fn stop_signal(&self) -> Arc<watch::Sender<bool>> {
        self.stop.clone()
    }

    /// Waits for the scheduler to end, which includes the backup it is in the middle of.
    pub async fn join(self) {
        let _ = self.task.await;
    }

    /// Tells the scheduler to stop and waits for it.
    pub async fn stop(self) {
        let _ = self.stop.send(true);
        self.join().await;
    }
}

/// Starts the scheduler: a backup now if there is none newer than the interval, then one every interval. A failed
/// backup is logged and tried again after `retry`.
pub fn spawn(store: Arc<SqliteStore>, cfg: BackupConfig) -> Backup {
    let (stop, mut stopped) = watch::channel(false);
    let stop = Arc::new(stop);
    let task = tokio::spawn(async move {
        let mut wait = first_wait(&cfg);
        loop {
            tokio::select! {
                // a stop that is already waiting is looked at first (a guard, not something a test can pin: a sleep is
                // never ready on its first poll, so the stop would win here anyway)
                biased;
                // a stop, or the sender gone
                _ = stopped.changed() => return,
                () = tokio::time::sleep(wait) => {}
            }
            let now = now_unix();
            if !plausible_clock(now) {
                tracing::error!("backup skipped: the clock reads {now}, which is not a real time; trying again soon");
                wait = cfg.retry;
                continue;
            }
            let (s, c) = (store.clone(), cfg.clone());
            let result =
                tokio::task::spawn_blocking(move || run_once(&s, &c.dir, now, c.keep)).await;
            wait = match result {
                Ok(Ok(path)) => {
                    tracing::info!("backup written: {}", path.display());
                    cfg.interval
                }
                Ok(Err(e)) => {
                    tracing::error!("backup failed, trying again soon: {e}");
                    cfg.retry
                }
                Err(e) => {
                    tracing::error!("backup task failed, trying again soon: {e}");
                    cfg.retry
                }
            };
        }
    });
    Backup { stop, task }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_calendar_functions_agree_with_each_other_over_a_long_stretch() {
        let mut day = -800_000; // far before 1970
        while day < 800_000 {
            let (y, m, d) = civil_from_days(day);
            assert_eq!(days_from_civil(y, m, d), day, "{y}-{m}-{d}");
            day += 37;
        }
    }

    #[test]
    fn retry_is_never_longer_than_the_interval_or_a_quarter_of_an_hour() {
        let with = |hours_in_secs: u64, dir: Option<&str>| Config {
            backup_dir: dir.map(Into::into),
            backup_interval: Duration::from_secs(hours_in_secs),
            ..Config::default()
        };
        let day = BackupConfig::from_config(&with(24 * 3_600, Some("/x"))).unwrap();
        assert_eq!(day.retry, RETRY_AT_MOST);
        assert_eq!(day.keep, Config::default().backup_keep);
        assert_eq!(day.interval, Duration::from_secs(24 * 3_600));
        assert_eq!(day.dir, PathBuf::from("/x"));
        let minute = BackupConfig::from_config(&with(60, Some("/x"))).unwrap();
        assert_eq!(minute.retry, Duration::from_secs(60));
        assert!(BackupConfig::from_config(&with(60, None)).is_none());
    }
}
