//! The contract of the [`Store`](crate::Store) seam as test cases that every adapter runs.
//!
//! ```ignore
//! fn make() -> MemoryStore { MemoryStore::new() }
//! dibibest_store::store_contract!(make);
//! ```

use crate::{OpInput, Store, StoreError};

pub fn op(key: &str, actor: &str, payload: &str) -> OpInput {
    OpInput {
        key: key.into(),
        actor: actor.into(),
        payload: payload.into(),
        schema_version: 1,
    }
}

pub fn seq_starts_at_one_and_counts_per_project<S: Store>(s: &S) {
    s.create_project("a", 1).unwrap();
    s.create_project("b", 1).unwrap();
    assert_eq!(s.append("a", op("k1", "u", "x")).unwrap().seq, 1);
    assert_eq!(s.append("a", op("k2", "u", "y")).unwrap().seq, 2);
    assert_eq!(s.append("b", op("k1", "u", "z")).unwrap().seq, 1);
    assert_eq!(s.latest_seq("a").unwrap(), 2);
    assert_eq!(s.latest_seq("b").unwrap(), 1);
}

pub fn an_empty_log_has_seq_zero<S: Store>(s: &S) {
    s.create_project("a", 1).unwrap();
    assert_eq!(s.latest_seq("a").unwrap(), 0);
    assert!(s.ops_after("a", 0, 10).unwrap().is_empty());
}

pub fn a_retried_key_returns_the_same_seq_and_writes_nothing<S: Store>(s: &S) {
    s.create_project("a", 1).unwrap();
    let first = s.append("a", op("k1", "u", "x")).unwrap();
    assert!(!first.duplicate);
    let again = s.append("a", op("k1", "u", "x")).unwrap();
    assert_eq!(again.seq, first.seq);
    assert!(again.duplicate);
    assert_eq!(s.latest_seq("a").unwrap(), 1);
    assert_eq!(s.ops_after("a", 0, 10).unwrap().len(), 1);
}

pub fn a_key_belongs_to_one_actor<S: Store>(s: &S) {
    s.create_project("a", 1).unwrap();
    s.append("a", op("k1", "alice", "x")).unwrap();
    let err = s.append("a", op("k1", "bob", "x")).unwrap_err();
    assert!(matches!(err, StoreError::KeyConflict(_)), "{err:?}");
    assert_eq!(s.latest_seq("a").unwrap(), 1);
}

pub fn a_key_reused_for_other_content_is_refused<S: Store>(s: &S) {
    s.create_project("a", 1).unwrap();
    s.append("a", op("k1", "u", "x")).unwrap();
    let err = s.append("a", op("k1", "u", "different")).unwrap_err();
    assert!(matches!(err, StoreError::KeyConflict(_)), "{err:?}");
    assert_eq!(s.latest_seq("a").unwrap(), 1);
    assert_eq!(s.ops_after("a", 0, 10).unwrap()[0].payload, "x");
}

pub fn the_same_key_in_two_projects_is_two_ops<S: Store>(s: &S) {
    s.create_project("a", 1).unwrap();
    s.create_project("b", 1).unwrap();
    assert!(!s.append("a", op("k", "u", "1")).unwrap().duplicate);
    assert!(!s.append("b", op("k", "u", "2")).unwrap().duplicate);
}

pub fn ops_after_returns_the_log_in_order_with_all_fields<S: Store>(s: &S) {
    s.create_project("a", 1).unwrap();
    for i in 1..=5 {
        s.append("a", op(&format!("k{i}"), "alice", &format!("p{i}")))
            .unwrap();
    }
    let all = s.ops_after("a", 0, 100).unwrap();
    assert_eq!(
        all.iter().map(|o| o.seq).collect::<Vec<_>>(),
        [1, 2, 3, 4, 5]
    );
    assert_eq!(all[2].key, "k3");
    assert_eq!(all[2].actor, "alice");
    assert_eq!(all[2].payload, "p3");
    assert_eq!(all[2].schema_version, 1);
    assert!(all[2].created_at_ms > 0);
    let tail = s.ops_after("a", 3, 100).unwrap();
    assert_eq!(tail.iter().map(|o| o.seq).collect::<Vec<_>>(), [4, 5]);
    assert!(s.ops_after("a", 5, 100).unwrap().is_empty());
}

pub fn a_small_limit_returns_the_first_page_and_paging_equals_one_go<S: Store>(s: &S) {
    s.create_project("a", 1).unwrap();
    for i in 1..=7 {
        s.append("a", op(&format!("k{i}"), "u", &format!("p{i}")))
            .unwrap();
    }
    let first = s.ops_after("a", 0, 3).unwrap();
    assert_eq!(first.iter().map(|o| o.seq).collect::<Vec<_>>(), [1, 2, 3]);
    let mut paged = Vec::new();
    let mut after = 0;
    loop {
        let page = s.ops_after("a", after, 3).unwrap();
        if page.is_empty() {
            break;
        }
        after = page.last().unwrap().seq;
        paged.extend(page);
    }
    assert_eq!(paged, s.ops_after("a", 0, 100).unwrap());
}

pub fn a_payload_comes_back_unchanged<S: Store>(s: &S) {
    s.create_project("a", 1).unwrap();
    let nasty = "{\"a\":\"line\\nbreak\",\"u\":\"ünï\u{1F600}\",\"q\":\"it's \\\"x\\\"\"}\n\ttab ' \" \\ ;--";
    s.append("a", op("k", "u", nasty)).unwrap();
    assert_eq!(s.ops_after("a", 0, 1).unwrap()[0].payload, nasty);
}

pub fn an_unknown_project_is_not_found<S: Store>(s: &S) {
    assert_eq!(
        s.append("nope", op("k", "u", "x")),
        Err(StoreError::NotFound)
    );
    assert_eq!(s.ops_after("nope", 0, 1), Err(StoreError::NotFound));
    assert_eq!(s.latest_seq("nope"), Err(StoreError::NotFound));
    assert_eq!(s.latest_snapshot("nope"), Err(StoreError::NotFound));
    assert_eq!(
        s.save_snapshot("nope", 0, "s", 1),
        Err(StoreError::NotFound)
    );
}

pub fn a_project_is_created_once<S: Store>(s: &S) {
    s.create_project("a", 1).unwrap();
    assert_eq!(s.create_project("a", 1), Err(StoreError::AlreadyExists));
}

pub fn there_is_no_snapshot_until_one_is_saved_and_the_newest_wins<S: Store>(s: &S) {
    s.create_project("a", 1).unwrap();
    assert_eq!(s.latest_snapshot("a").unwrap(), None);
    for i in 1..=4 {
        s.append("a", op(&format!("k{i}"), "u", "p")).unwrap();
    }
    s.save_snapshot("a", 2, "state-2", 1).unwrap();
    s.save_snapshot("a", 4, "state-4", 1).unwrap();
    let snap = s.latest_snapshot("a").unwrap().unwrap();
    assert_eq!((snap.seq, snap.state.as_str()), (4, "state-4"));
    assert_eq!(snap.schema_version, 1);
}

pub fn a_snapshot_ahead_of_the_log_or_not_newer_is_refused<S: Store>(s: &S) {
    s.create_project("a", 1).unwrap();
    s.append("a", op("k1", "u", "p")).unwrap();
    s.append("a", op("k2", "u", "p")).unwrap();
    assert!(matches!(
        s.save_snapshot("a", 3, "ahead", 1),
        Err(StoreError::SnapshotRefused(_))
    ));
    s.save_snapshot("a", 2, "two", 1).unwrap();
    assert!(matches!(
        s.save_snapshot("a", 2, "again", 1),
        Err(StoreError::SnapshotRefused(_))
    ));
    assert!(matches!(
        s.save_snapshot("a", 1, "older", 1),
        Err(StoreError::SnapshotRefused(_))
    ));
    assert_eq!(s.latest_snapshot("a").unwrap().unwrap().state, "two");
}

pub fn a_snapshot_never_changes_the_log<S: Store>(s: &S) {
    s.create_project("a", 1).unwrap();
    for i in 1..=3 {
        s.append("a", op(&format!("k{i}"), "u", &format!("p{i}")))
            .unwrap();
    }
    let before = s.ops_after("a", 0, 100).unwrap();
    s.save_snapshot("a", 3, "state", 1).unwrap();
    assert_eq!(s.ops_after("a", 0, 100).unwrap(), before);
    assert_eq!(s.latest_seq("a").unwrap(), 3);
}

pub fn a_snapshot_plus_the_log_after_it_restores_the_state<S: Store>(s: &S) {
    s.create_project("a", 1).unwrap();
    for i in 1..=5 {
        s.append("a", op(&format!("k{i}"), "u", &format!("p{i}")))
            .unwrap();
    }
    s.save_snapshot("a", 3, "{\"upto\":3}", 1).unwrap();
    let snap = s.latest_snapshot("a").unwrap().unwrap();
    assert_eq!(snap.state, "{\"upto\":3}");
    let rest = s.ops_after("a", snap.seq, 100).unwrap();
    assert_eq!(rest.iter().map(|o| o.seq).collect::<Vec<_>>(), [4, 5]);
}

/// Runs every case against a fresh store from `$make` (`fn() -> impl Store`): one `#[test]` per case.
#[macro_export]
macro_rules! store_contract {
    ($make:path) => {
        $crate::store_contract!(@each $make,
            seq_starts_at_one_and_counts_per_project,
            an_empty_log_has_seq_zero,
            a_retried_key_returns_the_same_seq_and_writes_nothing,
            a_key_belongs_to_one_actor,
            a_key_reused_for_other_content_is_refused,
            the_same_key_in_two_projects_is_two_ops,
            ops_after_returns_the_log_in_order_with_all_fields,
            a_small_limit_returns_the_first_page_and_paging_equals_one_go,
            a_payload_comes_back_unchanged,
            an_unknown_project_is_not_found,
            a_project_is_created_once,
            there_is_no_snapshot_until_one_is_saved_and_the_newest_wins,
            a_snapshot_ahead_of_the_log_or_not_newer_is_refused,
            a_snapshot_never_changes_the_log,
            a_snapshot_plus_the_log_after_it_restores_the_state
        );
    };
    (@each $make:path, $($case:ident),+ $(,)?) => {
        $(
            #[test]
            fn $case() {
                let store = $make();
                $crate::contract::$case(&store);
            }
        )+
    };
}
