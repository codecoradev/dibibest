use dibibest_server::lock::{DataLock, LockError};

#[test]
fn a_second_process_on_the_same_data_directory_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let first = DataLock::acquire(dir.path()).unwrap();
    let second = DataLock::acquire(dir.path()).err().expect("must refuse");
    assert!(matches!(second, LockError::Held(_)), "{second:?}");
    assert!(second.to_string().contains("another"), "{second}");
    drop(first);
}

#[test]
fn the_lock_is_free_again_when_the_holder_is_gone() {
    let dir = tempfile::tempdir().unwrap();
    drop(DataLock::acquire(dir.path()).unwrap());
    DataLock::acquire(dir.path()).unwrap();
}

#[test]
fn two_different_directories_do_not_block_each_other() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let _la = DataLock::acquire(a.path()).unwrap();
    let _lb = DataLock::acquire(b.path()).unwrap();
}

#[test]
fn a_missing_data_directory_is_created() {
    let dir = tempfile::tempdir().unwrap();
    let nested = dir.path().join("a/b/data");
    let _l = DataLock::acquire(&nested).unwrap();
    assert!(nested.is_dir());
}
