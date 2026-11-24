use dibibest_store::{
    Appended, OpInput, Options, Result, Snapshot, SqliteStore, Store, StoreError, StoredOp,
};
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

fn op(key: &str, actor: &str, payload: &str) -> OpInput {
    OpInput {
        key: key.into(),
        actor: actor.into(),
        payload: payload.into(),
        schema_version: 1,
    }
}

/// A store whose directory lives as long as it does, so the contract macro can use it as a plain `Store`.
struct Tmp {
    store: SqliteStore,
    _dir: TempDir,
}

impl Store for Tmp {
    fn create_project(&self, id: &str, v: u32) -> Result<()> {
        self.store.create_project(id, v)
    }
    fn append(&self, p: &str, o: OpInput) -> Result<Appended> {
        self.store.append(p, o)
    }
    fn ops_after(&self, p: &str, a: u64, l: usize) -> Result<Vec<StoredOp>> {
        self.store.ops_after(p, a, l)
    }
    fn latest_seq(&self, p: &str) -> Result<u64> {
        self.store.latest_seq(p)
    }
    fn save_snapshot(&self, p: &str, s: u64, st: &str, v: u32) -> Result<()> {
        self.store.save_snapshot(p, s, st, v)
    }
    fn latest_snapshot(&self, p: &str) -> Result<Option<Snapshot>> {
        self.store.latest_snapshot(p)
    }
}

fn make() -> Tmp {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(dir.path().join("s.db")).unwrap();
    Tmp { store, _dir: dir }
}

dibibest_store::store_contract!(make);

fn open_at(dir: &Path) -> SqliteStore {
    SqliteStore::open(dir.join("s.db")).unwrap()
}

#[test]
fn wal_is_on_and_the_writer_is_configured_for_durability() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_at(dir.path());
    let p = store.pragmas().unwrap();
    assert_eq!(p.journal_mode, "wal");
    assert_eq!(p.synchronous, 2, "FULL");
    assert_eq!(p.foreign_keys, 1);
    // The file header says WAL too (bytes 18 and 19 are 2), so a reader opening it later gets WAL.
    let mut head = [0u8; 20];
    std::fs::File::open(dir.path().join("s.db"))
        .unwrap()
        .read_exact(&mut head)
        .unwrap();
    assert_eq!((head[18], head[19]), (2, 2));
}

#[test]
fn open_refuses_a_database_where_wal_cannot_be_turned_on() {
    // An in-memory database answers `memory` to the journal mode request.
    let err = SqliteStore::open(":memory:").err().expect("must refuse");
    assert!(
        matches!(&err, StoreError::Io(m) if m.contains("wal")),
        "{err:?}"
    );
}

#[test]
fn many_producers_get_no_busy_and_every_acked_op_survives_a_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let acked: Mutex<Vec<(String, u64, String)>> = Mutex::new(Vec::new());
    {
        let store = open_at(dir.path());
        for p in 0..4 {
            store.create_project(&format!("p{p}"), 1).unwrap();
        }
        thread::scope(|sc| {
            for t in 0..24 {
                let (store, acked) = (&store, &acked);
                sc.spawn(move || {
                    for i in 0..100 {
                        let project = format!("p{}", (t + i) % 4);
                        let key = format!("t{t}-{i}");
                        let got = store
                            .append(&project, op(&key, "u", &format!("payload {key}")))
                            .expect("no busy, no error");
                        acked.lock().unwrap().push((project, got.seq, key));
                    }
                });
            }
        });
    }
    let store = open_at(dir.path());
    let mut total = 0;
    for p in 0..4 {
        let ops = store.ops_after(&format!("p{p}"), 0, 100_000).unwrap();
        let seqs: Vec<u64> = ops.iter().map(|o| o.seq).collect();
        assert_eq!(seqs, (1..=ops.len() as u64).collect::<Vec<_>>(), "gap-free");
        total += ops.len();
    }
    assert_eq!(total, 2400);
    for (project, seq, key) in acked.into_inner().unwrap() {
        let ops = store.ops_after(&project, seq - 1, 1).unwrap();
        assert_eq!(ops[0].key, key);
    }
}

#[test]
fn jobs_waiting_in_the_queue_are_written_in_one_transaction() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_at(dir.path());
    store.create_project("a", 1).unwrap();
    let before = store.stats();
    let pause = store.pause_writer();
    thread::scope(|sc| {
        for i in 0..50 {
            let store = &store;
            sc.spawn(move || {
                store.append("a", op(&format!("k{i}"), "u", "x")).unwrap();
            });
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while store.stats().queued < 50 {
            assert!(Instant::now() < deadline, "jobs never queued");
            thread::sleep(Duration::from_millis(5));
        }
        drop(pause);
    });
    let after = store.stats();
    assert_eq!(after.jobs - before.jobs, 50);
    assert!(
        after.commits - before.commits <= 2,
        "50 queued jobs took {} commits",
        after.commits - before.commits
    );
    assert_eq!(store.latest_seq("a").unwrap(), 50);
}

#[test]
fn a_full_queue_answers_busy_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let opts = Options {
        queue_capacity: 2,
        ..Options::default()
    };
    let store = SqliteStore::open_with(dir.path().join("s.db"), opts).unwrap();
    store.create_project("a", 1).unwrap();
    let pause = store.pause_writer();
    thread::scope(|sc| {
        for i in 0..2 {
            let store = &store;
            sc.spawn(move || {
                store.append("a", op(&format!("k{i}"), "u", "x")).unwrap();
            });
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while store.stats().queued < 2 {
            assert!(Instant::now() < deadline, "jobs never queued");
            thread::sleep(Duration::from_millis(5));
        }
        let refused = store.append("a", op("late", "u", "x"));
        assert_eq!(refused, Err(StoreError::Busy));
        drop(pause);
    });
    assert_eq!(store.latest_seq("a").unwrap(), 2);
    let again = store.append("a", op("late", "u", "x")).unwrap();
    assert!(
        !again.duplicate,
        "the refused op must not have been written"
    );
    assert_eq!(again.seq, 3);
}

#[test]
fn a_full_disk_goes_read_only_and_reads_keep_working() {
    let dir = tempfile::tempdir().unwrap();
    let opts = Options {
        max_page_count: Some(64),
        ..Options::default()
    };
    let store = SqliteStore::open_with(dir.path().join("s.db"), opts).unwrap();
    store.create_project("a", 1).unwrap();
    let big = "x".repeat(50_000);
    let mut written = 0;
    let err = loop {
        match store.append("a", op(&format!("k{written}"), "u", &big)) {
            Ok(_) => written += 1,
            Err(e) => break e,
        }
        assert!(written < 1000, "the cap never bit");
    };
    assert_eq!(err, StoreError::Full);
    assert!(store.read_only());
    assert_eq!(store.ops_after("a", 0, 10_000).unwrap().len(), written);
    assert_eq!(store.latest_seq("a").unwrap(), written as u64);
}

#[test]
fn a_refused_job_does_not_fail_the_jobs_written_with_it() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_at(dir.path());
    store.create_project("a", 1).unwrap();
    store.append("a", op("dup", "alice", "x")).unwrap();
    let pause = store.pause_writer();
    let (good, bad) = thread::scope(|sc| {
        let s = &store;
        let bad = sc.spawn(move || s.append("a", op("dup", "bob", "x")));
        let good = sc.spawn(move || s.append("a", op("fresh", "bob", "y")));
        let deadline = Instant::now() + Duration::from_secs(10);
        while store.stats().queued < 2 {
            assert!(Instant::now() < deadline, "jobs never queued");
            thread::sleep(Duration::from_millis(5));
        }
        drop(pause);
        (good.join().unwrap(), bad.join().unwrap())
    });
    assert!(matches!(bad, Err(StoreError::KeyConflict(_))));
    assert_eq!(good.unwrap().seq, 2);
}

#[test]
fn an_op_cannot_be_changed_after_it_was_written() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_at(dir.path());
    store.create_project("a", 1).unwrap();
    store.append("a", op("k", "u", "x")).unwrap();
    let raw = rusqlite::Connection::open(dir.path().join("s.db")).unwrap();
    let err = raw.execute("UPDATE ops SET payload = 'y'", []).unwrap_err();
    assert!(err.to_string().contains("append-only"), "{err}");
}

#[test]
fn a_database_from_a_newer_version_is_refused_and_an_older_one_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    {
        let store = open_at(dir.path());
        store.create_project("a", 1).unwrap();
        store.append("a", op("k", "u", "x")).unwrap();
    }
    {
        let store = open_at(dir.path());
        assert_eq!(store.latest_seq("a").unwrap(), 1, "a reopen keeps the data");
    }
    let raw = rusqlite::Connection::open(dir.path().join("s.db")).unwrap();
    raw.pragma_update(None, "user_version", 999).unwrap();
    drop(raw);
    let err = SqliteStore::open(dir.path().join("s.db")).err().unwrap();
    assert!(
        matches!(&err, StoreError::Io(m) if m.contains("newer")),
        "{err:?}"
    );
}

#[test]
fn a_backup_restores_to_the_same_log_and_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_at(dir.path());
    store.create_project("a", 1).unwrap();
    for i in 1..=10 {
        store
            .append("a", op(&format!("k{i}"), "u", &format!("p{i}")))
            .unwrap();
    }
    store.save_snapshot("a", 6, "state-6", 1).unwrap();
    let backup = dir.path().join("backup.db");
    store.backup_to(&backup).unwrap();
    // The live store keeps going after the backup.
    store.append("a", op("k11", "u", "p11")).unwrap();

    let restored_dir = tempfile::tempdir().unwrap();
    std::fs::copy(&backup, restored_dir.path().join("s.db")).unwrap();
    let restored = open_at(restored_dir.path());
    assert_eq!(restored.latest_seq("a").unwrap(), 10);
    assert_eq!(
        restored.ops_after("a", 0, 100).unwrap(),
        store.ops_after("a", 0, 10).unwrap()
    );
    assert_eq!(
        restored.latest_snapshot("a").unwrap().unwrap().state,
        "state-6"
    );
}

#[test]
fn a_backup_never_overwrites_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_at(dir.path());
    let target = dir.path().join("existing.db");
    std::fs::write(&target, b"keep me").unwrap();
    assert!(store.backup_to(&target).is_err());
    assert_eq!(std::fs::read(&target).unwrap(), b"keep me");
}

/// The child half of the kill test: appends as fast as it can and prints each acknowledged number.
#[test]
#[ignore = "run by a_process_killed_mid_write_loses_no_acknowledged_op"]
fn crash_child() {
    let dir = std::env::var("DIBIBEST_CRASH_DIR").expect("run by the parent test");
    let store = open_at(Path::new(&dir));
    store.create_project("a", 1).unwrap();
    for i in 0..u64::MAX {
        let got = store
            .append("a", op(&format!("k{i}"), "u", &format!("payload {i}")))
            .unwrap();
        println!("ack {}", got.seq);
    }
}

#[test]
fn a_process_killed_mid_write_loses_no_acknowledged_op() {
    let dir = tempfile::tempdir().unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "crash_child", "--nocapture"])
        .env("DIBIBEST_CRASH_DIR", dir.path())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut last_acked = 0u64;
    let lines = BufReader::new(child.stdout.take().unwrap()).lines();
    for line in lines {
        let line = line.unwrap();
        if let Some(n) = line.strip_prefix("ack ") {
            last_acked = n.parse().unwrap();
            if last_acked >= 300 {
                break;
            }
        }
    }
    child.kill().unwrap(); // SIGKILL: no destructor, no flush
    child.wait().unwrap();
    assert!(last_acked >= 300, "the child never got going");

    let store = open_at(dir.path());
    let latest = store.latest_seq("a").unwrap();
    assert!(
        latest >= last_acked,
        "acknowledged {last_acked}, found {latest}"
    );
    let ops = store.ops_after("a", 0, 1_000_000).unwrap();
    assert_eq!(ops.len() as u64, latest);
    for (i, o) in ops.iter().enumerate() {
        assert_eq!(o.seq, i as u64 + 1);
        assert_eq!(o.payload, format!("payload {i}"), "no torn row");
    }
}

#[test]
fn a_job_that_fails_halfway_leaves_no_trace_and_does_not_burn_a_number() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_at(dir.path());
    store.create_project("a", 1).unwrap();
    store.append("a", op("k1", "u", "one")).unwrap();

    // Make the second op fail after its row is already inserted: the update of the counter is refused.
    let raw = rusqlite::Connection::open(dir.path().join("s.db")).unwrap();
    raw.execute_batch(
        "CREATE TRIGGER boom BEFORE UPDATE ON projects WHEN NEW.seq = 2
         BEGIN SELECT RAISE(ABORT, 'boom'); END;",
    )
    .unwrap();
    let err = store.append("a", op("k2", "u", "two")).unwrap_err();
    assert!(
        matches!(&err, StoreError::Io(m) if m.contains("boom")),
        "{err:?}"
    );
    raw.execute_batch("DROP TRIGGER boom;").unwrap();

    let again = store.append("a", op("k3", "u", "three")).unwrap();
    assert_eq!(again.seq, 2, "the failed op must not have taken number 2");
    let keys: Vec<String> = store
        .ops_after("a", 0, 10)
        .unwrap()
        .into_iter()
        .map(|o| o.key)
        .collect();
    assert_eq!(keys, ["k1", "k3"]);
}

#[test]
fn a_status_probe_does_not_clear_the_read_only_flag() {
    let dir = tempfile::tempdir().unwrap();
    let opts = Options {
        max_page_count: Some(64),
        ..Options::default()
    };
    let store = SqliteStore::open_with(dir.path().join("s.db"), opts).unwrap();
    store.create_project("a", 1).unwrap();
    let big = "x".repeat(50_000);
    for i in 0..1000 {
        if store.append("a", op(&format!("k{i}"), "u", &big)) == Err(StoreError::Full) {
            break;
        }
    }
    assert!(store.read_only(), "the disk is full");
    // A health check asks the writer a question; that is not a write and must not say the disk has room again.
    store.pragmas().unwrap();
    assert!(store.read_only(), "a probe cleared the flag");
}
