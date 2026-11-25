//! Scheduled backups of the SQLite database (DIB-7, issue #95): file names, retention, a restore that matches the
//! log, the scheduler (restart-safe, retries, stops on request) and its place in `bind` and `Bound::run`.
//!
//! Timing is polled against a generous ceiling, never slept for a fixed time and then asserted.

use dibibest_server::backup::{
    file_name, newest_backup, parse_file_name, plausible_clock, prune, run_once, spawn,
    BackupConfig,
};
use dibibest_server::bind;
use dibibest_server::config::Config;
use dibibest_store::{OpInput, SqliteStore, Store};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::oneshot;

const CEILING: Duration = Duration::from_secs(10);

fn store_with_log(dir: &Path) -> Arc<SqliteStore> {
    let store = Arc::new(SqliteStore::open(dir.join("dibibest.db")).unwrap());
    store.create_project("p", 1).unwrap();
    for i in 1..=5 {
        store
            .append(
                "p",
                OpInput {
                    key: format!("k{i}"),
                    actor: "alice".into(),
                    payload: format!("{{\"n\":{i}}}"),
                    schema_version: 1,
                },
            )
            .unwrap();
    }
    store.save_snapshot("p", 3, "{\"upto\":3}", 1).unwrap();
    store
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn backups_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| parse_file_name(n).is_some())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

async fn until(what: &str, mut condition: impl FnMut() -> bool) {
    let started = Instant::now();
    while !condition() {
        assert!(started.elapsed() < CEILING, "still waiting for: {what}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn config(dir: &Path, interval: Duration, keep: usize) -> BackupConfig {
    BackupConfig {
        dir: dir.to_path_buf(),
        interval,
        keep,
        retry: Duration::from_millis(200),
    }
}

// ---- names ------------------------------------------------------------------------------------------------

#[test]
fn a_file_name_is_the_utc_time_to_the_second() {
    assert_eq!(file_name(0), "dibibest-19700101T000000Z.db");
    assert_eq!(file_name(951_782_400), "dibibest-20000229T000000Z.db"); // a leap day
    assert_eq!(file_name(1_795_508_600), "dibibest-20261124T082320Z.db");
    assert_eq!(file_name(4_102_444_799), "dibibest-20991231T235959Z.db");
}

#[test]
fn a_file_name_reads_back_and_only_our_own_names_do() {
    for t in [0, 951_782_400, 1_795_508_600, 4_102_444_799, 1_234_567_890] {
        assert_eq!(parse_file_name(&file_name(t)), Some(t), "{t}");
    }
    for foreign in [
        "dibibest.db",
        "dibibest.db-wal",
        "dibibest-20261124T082320Z.db.partial",
        "dibibest-20261124T082320.db",
        "dibibest-2026112T082320Z.db",
        "dibibest-20261324T082320Z.db", // month 13
        "dibibest-20260230T082320Z.db", // February 30
        "dibibest-20260431T082320Z.db", // April 31
        "dibibest-20260000T082320Z.db", // day 0
        "dibibest-20261124T252320Z.db", // hour 25
        "other-20261124T082320Z.db",
        "notes.txt",
        "",
    ] {
        assert_eq!(parse_file_name(foreign), None, "{foreign:?}");
    }
}

// ---- one backup -------------------------------------------------------------------------------------------

#[test]
fn a_backup_restores_to_the_same_log_and_snapshot() {
    let live = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let store = store_with_log(live.path());
    let path = run_once(&store, out.path(), 1_795_508_600, 7).unwrap();
    assert_eq!(path, out.path().join("dibibest-20261124T082320Z.db"));

    // "restore": open the copy as a database of its own and compare it with the live one
    let restored = SqliteStore::open(&path).unwrap();
    assert_eq!(
        restored.ops_after("p", 0, 100).unwrap(),
        store.ops_after("p", 0, 100).unwrap(),
        "the log of the copy matches the live log"
    );
    assert_eq!(restored.latest_seq("p").unwrap(), 5);
    assert_eq!(
        restored.latest_snapshot("p").unwrap(),
        store.latest_snapshot("p").unwrap()
    );
}

#[test]
fn a_backup_taken_while_the_writer_is_busy_is_still_whole() {
    let live = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let store = store_with_log(live.path());
    let writer = {
        let store = store.clone();
        std::thread::spawn(move || {
            for i in 6..200 {
                let _ = store.append(
                    "p",
                    OpInput {
                        key: format!("k{i}"),
                        actor: "bob".into(),
                        payload: "{}".into(),
                        schema_version: 1,
                    },
                );
            }
        })
    };
    let path = run_once(&store, out.path(), now(), 7).unwrap();
    writer.join().unwrap();
    let restored = SqliteStore::open(&path).unwrap();
    let ops = restored.ops_after("p", 0, 1000).unwrap();
    let seqs: Vec<u64> = ops.iter().map(|o| o.seq).collect();
    assert_eq!(seqs, (1..=seqs.len() as u64).collect::<Vec<_>>(), "no gap");
    assert!(ops.len() >= 5);
}

#[test]
fn the_directory_is_created_and_a_second_backup_in_the_same_second_is_refused_not_overwritten() {
    let live = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let nested = out.path().join("a/b");
    let store = store_with_log(live.path());
    let first = run_once(&store, &nested, 1_000_000, 7).unwrap();
    let before = std::fs::read(&first).unwrap();
    assert!(run_once(&store, &nested, 1_000_000, 7).is_err());
    assert_eq!(std::fs::read(&first).unwrap(), before, "left as it was");
    assert_eq!(backups_in(&nested).len(), 1);
}

#[test]
fn a_leftover_partial_file_is_removed_and_is_never_a_backup() {
    let live = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let store = store_with_log(live.path());
    let stale = out.path().join("dibibest-20261124T082320Z.db.partial");
    std::fs::write(&stale, b"half a file").unwrap();
    assert_eq!(
        newest_backup(out.path()),
        None,
        "a partial file is not a backup"
    );
    run_once(&store, out.path(), 1_795_508_700, 7).unwrap();
    assert!(!stale.exists(), "cleaned up by the next run");
    assert_eq!(backups_in(out.path()).len(), 1);
}

#[test]
fn no_partial_file_is_left_behind_by_a_backup_that_succeeds() {
    let live = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let store = store_with_log(live.path());
    run_once(&store, out.path(), 1_795_508_700, 7).unwrap();
    let leftovers: Vec<_> = std::fs::read_dir(out.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".partial"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

// ---- retention --------------------------------------------------------------------------------------------

#[test]
fn only_the_newest_few_are_kept_and_foreign_files_are_never_touched() {
    let live = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let store = store_with_log(live.path());
    for t in [1_000_000, 2_000_000, 3_000_000, 4_000_000, 5_000_000] {
        run_once(&store, out.path(), t, 3).unwrap();
    }
    // files that are not ours sit in the same directory
    for name in [
        "notes.txt",
        "dibibest.db",
        "dibibest-20000101T000000Z.db.bak",
    ] {
        std::fs::write(out.path().join(name), b"keep me").unwrap();
    }
    let kept = backups_in(out.path());
    assert_eq!(
        kept,
        [3_000_000, 4_000_000, 5_000_000].map(file_name),
        "the three newest"
    );
    assert!(out.path().join("notes.txt").exists());
    assert!(out.path().join("dibibest.db").exists());
    assert!(out.path().join("dibibest-20000101T000000Z.db.bak").exists());
    // pruning again changes nothing
    prune(out.path(), 3, None).unwrap();
    assert_eq!(backups_in(out.path()).len(), 3);
}

#[test]
fn a_keep_of_one_keeps_the_one_just_made() {
    let live = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let store = store_with_log(live.path());
    run_once(&store, out.path(), 1_000_000, 1).unwrap();
    let last = run_once(&store, out.path(), 2_000_000, 1).unwrap();
    assert_eq!(backups_in(out.path()), [file_name(2_000_000)]);
    assert!(last.exists());
}

// ---- the scheduler ----------------------------------------------------------------------------------------

#[tokio::test]
async fn with_no_backup_yet_the_first_one_is_made_at_once_and_the_next_after_the_interval() {
    let live = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let store = store_with_log(live.path());
    let backup = spawn(store, config(out.path(), Duration::from_millis(1_200), 5));
    until("the first backup", || backups_in(out.path()).len() == 1).await;
    until("the second backup", || backups_in(out.path()).len() == 2).await;
    backup.stop().await;
}

#[tokio::test]
async fn a_recent_backup_means_a_restart_does_not_make_another_at_once() {
    let live = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let store = store_with_log(live.path());
    // one second ago: a backup made at once would be named for now, a different file, and be counted
    let recent = out.path().join(file_name(now() - 1));
    std::fs::write(&recent, b"made a moment ago").unwrap();
    let backup = spawn(store, config(out.path(), Duration::from_secs(3_600), 5));
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(backups_in(out.path()).len(), 1, "no second backup yet");
    backup.stop().await;
}

#[tokio::test]
async fn an_old_backup_means_one_is_made_at_once() {
    let live = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let store = store_with_log(live.path());
    std::fs::write(out.path().join(file_name(now() - 7_200)), b"two hours old").unwrap();
    let backup = spawn(store, config(out.path(), Duration::from_secs(3_600), 5));
    until("a new backup beside the old one", || {
        backups_in(out.path()).len() == 2
    })
    .await;
    backup.stop().await;
}

#[tokio::test]
async fn the_scheduler_keeps_to_the_retention_it_was_given() {
    let live = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let store = store_with_log(live.path());
    let backup = spawn(store, config(out.path(), Duration::from_millis(1_100), 2));
    // wait for a third to have been made, which is when the first has to be gone
    let mut seen: Vec<String> = Vec::new();
    until("three backups made over time", || {
        for n in backups_in(out.path()) {
            if !seen.contains(&n) {
                seen.push(n);
            }
        }
        seen.len() >= 3
    })
    .await;
    // the file that has just appeared is pruned a moment after it is renamed, so wait for that, not for the clock
    until("only two are left", || backups_in(out.path()).len() <= 2).await;
    assert!(
        !backups_in(out.path()).contains(&seen[0]),
        "the oldest of the three is the one that went"
    );
    backup.stop().await;
}

#[tokio::test]
async fn a_failed_backup_is_tried_again_soon_and_a_stop_is_not_held_up_by_a_long_interval() {
    let live = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let store = store_with_log(live.path());
    // the directory cannot be made while a plain file stands where it should be
    let blocked: PathBuf = out.path().join("backups");
    std::fs::write(&blocked, b"a file, not a directory").unwrap();
    let backup = spawn(store, config(&blocked, Duration::from_secs(3_600), 5));
    tokio::time::sleep(Duration::from_millis(500)).await; // at least one failed try
    std::fs::remove_file(&blocked).unwrap();
    until("a backup after the obstacle is gone", || {
        backups_in(&blocked).len() == 1
    })
    .await;
    let started = Instant::now();
    backup.stop().await;
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "stopping waited for the hour"
    );
}

// ---- in the server ----------------------------------------------------------------------------------------

fn server_config(dir: &Path) -> Config {
    let web = dir.join("web");
    std::fs::create_dir_all(&web).unwrap();
    std::fs::write(web.join("index.html"), "<html>INDEX</html>").unwrap();
    Config {
        addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: dir.join("data"),
        web_dir: web,
        shutdown_grace: Duration::from_millis(500),
        ..Config::default()
    }
}

#[tokio::test]
async fn the_server_backs_up_while_it_runs_and_stops_the_backup_with_itself() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("backups");
    let mut c = server_config(dir.path());
    c.backup_dir = Some(out.clone());
    c.backup_interval = Duration::from_secs(3_600);
    c.backup_keep = 3;
    let bound = bind(c).await.unwrap();
    let (stop, stopped) = oneshot::channel::<()>();
    let task = tokio::spawn(bound.run(async move {
        let _ = stopped.await;
    }));
    until("the first backup of a new server", || {
        backups_in(&out).len() == 1
    })
    .await;
    stop.send(()).unwrap();
    tokio::time::timeout(CEILING, task)
        .await
        .expect("run returns after the stop")
        .unwrap()
        .unwrap();
    // the database it made can be opened by anyone: the writer has been flushed and the lock released
    let copy = out.join(&backups_in(&out)[0]);
    SqliteStore::open(&copy).expect("the backup is a database");
}

#[tokio::test]
async fn once_the_server_has_stopped_no_more_backups_are_made() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("backups");
    let mut c = server_config(dir.path());
    c.backup_dir = Some(out.clone());
    c.backup_interval = Duration::from_millis(1_100); // shorter than the setting allows, for this test only
    let bound = bind(c).await.unwrap();
    let (stop, stopped) = oneshot::channel::<()>();
    let task = tokio::spawn(bound.run(async move {
        let _ = stopped.await;
    }));
    until("the first backup", || !backups_in(&out).is_empty()).await;
    stop.send(()).unwrap();
    tokio::time::timeout(CEILING, task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let at_stop = backups_in(&out);
    tokio::time::sleep(Duration::from_millis(2_500)).await; // more than two intervals
    assert_eq!(
        backups_in(&out),
        at_stop,
        "a backup was made after the stop"
    );
}

#[tokio::test]
async fn a_backup_directory_that_cannot_be_used_stops_the_start_instead_of_never_backing_up() {
    let dir = tempfile::tempdir().unwrap();
    let blocker = dir.path().join("not-a-dir");
    std::fs::write(&blocker, b"x").unwrap();
    let mut c = server_config(dir.path());
    c.backup_dir = Some(blocker.join("backups"));
    let err = bind(c).await.err().expect("the start is refused");
    assert!(err.to_string().to_lowercase().contains("backup"), "{err}");
}

#[tokio::test]
async fn without_a_backup_directory_nothing_is_written_beside_the_database() {
    let dir = tempfile::tempdir().unwrap();
    let c = server_config(dir.path());
    let data = c.data_dir.clone();
    let bound = bind(c).await.unwrap();
    let (stop, stopped) = oneshot::channel::<()>();
    let task = tokio::spawn(bound.run(async move {
        let _ = stopped.await;
    }));
    tokio::time::sleep(Duration::from_millis(300)).await;
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
    assert!(backups_in(&data).is_empty());
}

#[test]
fn the_backup_just_made_is_never_the_one_retention_deletes_even_when_the_clock_went_back() {
    let live = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let store = store_with_log(live.path());
    // a backup that is named for a time AFTER the one the next backup gets: the clock was stepped back
    run_once(&store, out.path(), 5_000_000, 1).unwrap();
    let made = run_once(&store, out.path(), 3_000_000, 1).unwrap();
    assert!(
        made.exists(),
        "the backup just made must survive its own retention"
    );
    assert_eq!(
        backups_in(out.path()).len(),
        2,
        "both are kept rather than losing the new one"
    );
}

#[test]
fn a_directory_that_is_named_like_a_backup_is_not_one() {
    let live = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let store = store_with_log(live.path());
    let lookalike = out.path().join(file_name(9_000_000));
    std::fs::create_dir(&lookalike).unwrap();
    std::fs::write(lookalike.join("inside"), b"x").unwrap();
    assert_eq!(newest_backup(out.path()), None);
    run_once(&store, out.path(), 1_000_000, 1).unwrap();
    assert!(lookalike.join("inside").exists(), "left alone");
}

#[test]
fn a_clock_that_is_not_set_is_not_believed() {
    assert!(!plausible_clock(0));
    assert!(!plausible_clock(86_400));
    assert!(!plausible_clock(-5));
    assert!(plausible_clock(1_795_508_600));
}

// ---- permissions: a backup holds everybody's projects ------------------------------------------------------

#[cfg(unix)]
mod permissions {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn a_backup_is_readable_by_its_owner_only_and_so_is_the_directory_it_made() {
        let live = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let store = store_with_log(live.path());
        let dir = out.path().join("made-by-the-server");
        let path = run_once(&store, &dir, 1_000_000, 7).unwrap();
        assert_eq!(mode(&path), 0o600, "the backup file");
        assert_eq!(mode(&dir), 0o700, "the directory");
    }

    #[test]
    fn a_directory_that_was_open_to_everyone_is_closed_and_every_backup_in_it_is_owner_only() {
        let live = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let store = store_with_log(live.path());
        std::fs::set_permissions(out.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        // a backup an earlier version made, readable by others
        let old = out.path().join(file_name(500_000));
        std::fs::write(&old, b"older backup").unwrap();
        std::fs::set_permissions(&old, std::fs::Permissions::from_mode(0o644)).unwrap();
        run_once(&store, out.path(), 1_000_000, 7).unwrap();
        assert_eq!(mode(out.path()), 0o700);
        assert_eq!(mode(&old), 0o600, "an older backup is tightened too");
    }

    #[test]
    fn a_file_that_is_not_a_backup_keeps_its_own_mode() {
        let live = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let store = store_with_log(live.path());
        let other = out.path().join("notes.txt");
        std::fs::write(&other, b"not a backup").unwrap();
        std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o644)).unwrap();
        run_once(&store, out.path(), 1_000_000, 7).unwrap();
        assert_eq!(mode(&other), 0o644, "only backups are restricted");
    }

    #[test]
    fn the_write_test_leaves_nothing_behind_and_the_checked_directory_is_closed() {
        let out = tempfile::tempdir().unwrap();
        let dir = out.path().join("checked");
        dibibest_server::backup::check_dir(&dir).unwrap();
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    }
}
