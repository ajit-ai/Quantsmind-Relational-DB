//! R4-MULTIWRITER — multiple sessions, each with its own explicit write
//! transaction, over one SQL engine.
//!
//! The engine exposes one active explicit transaction per `SessionId`
//! (`execute_session`). Statement execution is serialized by `&mut self`, but
//! transactions are fully independent: each holds its own kernel txn id,
//! snapshot, strict-2PL row locks and buffered rows. Any subset may roll back
//! or commit concurrently; a session's snapshot never observes another
//! session's uncommitted writes, nor any foreign commit newer than its BEGIN
//! (snapshot isolation holds across writers).
//!
//! Row keys are physical, append-only and uniquely allocated by the engine's
//! shared per-table counter, so two sessions' INSERTs always target disjoint
//! physical rows: multi-writer INSERTs cannot collide on a row key. The
//! no-wait `Busy` and first-committer-wins conflict surfaces are instead
//! proven at the kernel (see `concurrency_semantics.rs` row-local race test);
//! here we prove the multi-writer isolation and durability contract the SQL
//! layer must uphold. No sleeps, no timing: all interleavings are sequential
//! statements on the two sessions.

use qmind_sql::{Engine, SqlValue};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn fresh_dir(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir =
        std::env::temp_dir().join(format!("qmind-r4-mw-{tag}-pid{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn cols(r: &qmind_sql::ExecResult) -> Vec<SqlValue> {
    r.rows[0].clone()
}

#[test]
fn two_sessions_run_independent_write_transactions_and_commit() {
    let mut eng = Engine::new(Vec::new());
    eng.execute("CREATE TABLE t (id INTEGER, v TEXT)").unwrap();

    // Session 1 inserts two rows; session 2 begins concurrently and inserts
    // two more. Each sees only its own uncommitted writes.
    eng.execute_session(1, "BEGIN").unwrap();
    eng.execute_session(1, "INSERT INTO t VALUES (1, 'a'), (2, 'b')")
        .unwrap();
    eng.execute_session(2, "BEGIN").unwrap();
    eng.execute_session(2, "INSERT INTO t VALUES (10, 'x'), (11, 'y')")
        .unwrap();

    let r = eng
        .execute_session(1, "SELECT id FROM t ORDER BY id")
        .unwrap();
    assert_eq!(
        r.rows,
        vec![vec![SqlValue::Int(1)], vec![SqlValue::Int(2)]],
        "session 1 sees only its own rows"
    );
    let r = eng
        .execute_session(2, "SELECT id FROM t ORDER BY id")
        .unwrap();
    assert_eq!(
        r.rows,
        vec![vec![SqlValue::Int(10)], vec![SqlValue::Int(11)]],
        "session 2 sees only its own rows"
    );

    // Session 2 commits first. Session 1's pinned snapshot must not observe
    // the foreign commit mid-transaction.
    eng.execute_session(2, "COMMIT").unwrap();
    let r = eng
        .execute_session(1, "SELECT id FROM t ORDER BY id")
        .unwrap();
    assert_eq!(
        r.rows,
        vec![vec![SqlValue::Int(1)], vec![SqlValue::Int(2)]],
        "session 1 snapshot is stable across session 2's commit"
    );

    // Session 1 commits: both transactions' rows are durable.
    eng.execute_session(1, "COMMIT").unwrap();
    let r = eng.execute("SELECT COUNT(*) FROM t").unwrap();
    assert_eq!(cols(&r)[0], SqlValue::Int(4));
    let r = eng.execute("SELECT id FROM t ORDER BY id").unwrap();
    assert_eq!(
        r.rows,
        vec![
            vec![SqlValue::Int(1)],
            vec![SqlValue::Int(2)],
            vec![SqlValue::Int(10)],
            vec![SqlValue::Int(11)],
        ]
    );
}

#[test]
fn snapshot_isolation_holds_with_multiple_writers() {
    let mut eng = Engine::new(Vec::new());
    eng.execute("CREATE TABLE t (id INTEGER, v TEXT)").unwrap();
    eng.execute("INSERT INTO t VALUES (1, 'a'), (2, 'b'), (3, 'c')")
        .unwrap();

    // Session 1 snapshots the committed baseline {1,2,3}.
    eng.execute_session(1, "BEGIN").unwrap();
    let before = eng.execute_session(1, "SELECT COUNT(*) FROM t").unwrap();
    assert_eq!(cols(&before)[0], SqlValue::Int(3));

    // Session 2 (fresh snapshot) inserts and commits while session 1 is open.
    eng.execute_session(2, "BEGIN").unwrap();
    eng.execute_session(2, "INSERT INTO t VALUES (99, 'late')")
        .unwrap();
    eng.execute_session(2, "COMMIT").unwrap();

    // Session 1's second read agrees with its first: foreign commit invisible.
    let after = eng.execute_session(1, "SELECT COUNT(*) FROM t").unwrap();
    assert_eq!(
        before.rows, after.rows,
        "open snapshot must ignore foreign commits under multi-writer SI"
    );

    // After COMMIT, session 1 sees the whole committed world including the
    // writer that committed concurrently.
    eng.execute_session(1, "COMMIT").unwrap();
    let r = eng.execute("SELECT COUNT(*) FROM t").unwrap();
    assert_eq!(cols(&r)[0], SqlValue::Int(4));
}

#[test]
fn failed_statement_in_one_session_leaves_the_other_unaffected() {
    let mut eng = Engine::new(Vec::new());
    eng.execute("CREATE TABLE t (id INTEGER, v TEXT)").unwrap();

    eng.execute_session(1, "BEGIN").unwrap();
    eng.execute_session(1, "INSERT INTO t VALUES (1, 'kept')")
        .unwrap();
    let err = eng
        .execute_session(1, "INSERT INTO t VALUES (2)")
        .unwrap_err();
    assert!(err.contains("has 2 columns"), "unexpected error: {err}");

    // Session 2 starts, writes and commits independently while session 1's
    // transaction is still open after its failing statement.
    eng.execute_session(2, "BEGIN").unwrap();
    eng.execute_session(2, "INSERT INTO t VALUES (3, 'own')")
        .unwrap();
    eng.execute_session(2, "COMMIT").unwrap();

    // Session 1's earlier write is still visible to it; session 2's commit is
    // not (SI).
    let r = eng.execute_session(1, "SELECT COUNT(*) FROM t").unwrap();
    assert_eq!(cols(&r)[0], SqlValue::Int(1));

    eng.execute_session(1, "ROLLBACK").unwrap();
    let r = eng.execute("SELECT COUNT(*) FROM t").unwrap();
    assert_eq!(
        cols(&r)[0],
        SqlValue::Int(1),
        "only session 2's committed row survives"
    );
    let r = eng.execute("SELECT id, v FROM t").unwrap();
    assert_eq!(
        r.rows,
        vec![vec![SqlValue::Int(3), SqlValue::Text("own".into())]]
    );
}

#[test]
fn concurrent_indexed_writers_both_materialize_indexes_at_commit() {
    let mut eng = Engine::new(Vec::new());
    eng.execute("CREATE TABLE t (id INTEGER, v TEXT)").unwrap();
    eng.execute("CREATE INDEX iv ON t (v)").unwrap();

    eng.execute_session(1, "BEGIN").unwrap();
    eng.execute_session(1, "INSERT INTO t VALUES (1, 'a')")
        .unwrap();
    eng.execute_session(2, "BEGIN").unwrap();
    eng.execute_session(2, "INSERT INTO t VALUES (2, 'b')")
        .unwrap();

    // Inside its own transaction session 1 reaches its own row through the
    // index fallback path (deferred index materialization forces a txn-aware
    // scan; own-write visibility preserved).
    let r = eng
        .execute_session(1, "SELECT id FROM t WHERE v = 'a'")
        .unwrap();
    assert_eq!(r.rows, vec![vec![SqlValue::Int(1)]]);

    eng.execute_session(2, "COMMIT").unwrap();
    eng.execute_session(1, "COMMIT").unwrap();

    // Both sessions' committed rows are reachable through the materialized
    // index after their commits.
    let r = eng.execute("SELECT id FROM t WHERE v = 'a'").unwrap();
    assert_eq!(r.rows, vec![vec![SqlValue::Int(1)]]);
    let r = eng.execute("SELECT id FROM t WHERE v = 'b'").unwrap();
    assert_eq!(r.rows, vec![vec![SqlValue::Int(2)]]);
    let r = eng.execute("SELECT COUNT(*) FROM t").unwrap();
    assert_eq!(cols(&r)[0], SqlValue::Int(2));
}

#[test]
fn interleaved_multirow_inserts_allocate_disjoint_rows_in_one_table() {
    let mut eng = Engine::new(Vec::new());
    eng.execute("CREATE TABLE t (id INTEGER, v INTEGER)")
        .unwrap();

    // Both sessions interleave multi-row INSERTs into the SAME table while
    // both transactions are open. The shared row-id counter hands out disjoint
    // physical ranges to each statement, so neither transaction blocks the
    // other and both commit — FCW has nothing to reject because no physical
    // key overlaps.
    eng.execute_session(1, "BEGIN").unwrap();
    eng.execute_session(1, "INSERT INTO t VALUES (0, 0), (1, 1)")
        .unwrap();
    eng.execute_session(2, "BEGIN").unwrap();
    eng.execute_session(2, "INSERT INTO t VALUES (100, 100), (101, 101)")
        .unwrap();
    eng.execute_session(1, "INSERT INTO t VALUES (2, 2), (3, 3)")
        .unwrap();
    eng.execute_session(2, "INSERT INTO t VALUES (102, 102), (103, 103)")
        .unwrap();

    eng.execute_session(1, "COMMIT").unwrap();
    eng.execute_session(2, "COMMIT").unwrap();

    // Every physical row is present exactly once: no loss, no duplication.
    let r = eng.execute("SELECT id, v FROM t ORDER BY id").unwrap();
    let mut expected = Vec::new();
    for i in 0..4u64 {
        expected.push(vec![SqlValue::Int(i as i64), SqlValue::Int(i as i64)]);
    }
    for i in 100..104u64 {
        expected.push(vec![SqlValue::Int(i as i64), SqlValue::Int(i as i64)]);
    }
    assert_eq!(
        r.rows, expected,
        "all interleaved rows committed exactly once"
    );
    let r = eng.execute("SELECT COUNT(*) FROM t").unwrap();
    assert_eq!(cols(&r)[0], SqlValue::Int(8));
}

#[test]
fn autocommit_writes_are_not_blocked_by_an_open_explicit_transaction() {
    let mut eng = Engine::new(Vec::new());
    eng.execute("CREATE TABLE t (id INTEGER)").unwrap();

    // Session 1 holds an open transaction; session 2's autocommit writes
    // proceed immediately (they target new disjoint rows).
    eng.execute_session(1, "BEGIN").unwrap();
    eng.execute_session(1, "INSERT INTO t VALUES (1)").unwrap();
    eng.execute_session(2, "INSERT INTO t VALUES (2)").unwrap();
    eng.execute_session(2, "INSERT INTO t VALUES (3)").unwrap();

    let r = eng.execute_session(1, "SELECT COUNT(*) FROM t").unwrap();
    assert_eq!(
        cols(&r)[0],
        SqlValue::Int(1),
        "session 1's snapshot predates session 2's autocommit commits"
    );

    eng.execute_session(1, "COMMIT").unwrap();
    let r = eng.execute("SELECT COUNT(*) FROM t").unwrap();
    assert_eq!(cols(&r)[0], SqlValue::Int(3));
}

#[test]
fn a_second_session_may_begin_while_the_first_transaction_is_open() {
    let mut eng = Engine::new(Vec::new());
    eng.execute("CREATE TABLE t (id INTEGER)").unwrap();

    eng.execute_session(1, "BEGIN").unwrap();
    // Same session: re-BEGIN stays rejected.
    let err = eng.execute_session(1, "BEGIN").unwrap_err();
    assert!(err.contains("already in progress"), "{err}");
    // Different session: concurrent BEGIN is allowed (the single-writer
    // constraint is gone).
    eng.execute_session(2, "BEGIN").unwrap();

    eng.execute_session(2, "INSERT INTO t VALUES (2)").unwrap();
    eng.execute_session(2, "COMMIT").unwrap();
    eng.execute_session(1, "INSERT INTO t VALUES (1)").unwrap();
    eng.execute_session(1, "COMMIT").unwrap();

    let r = eng.execute("SELECT id FROM t ORDER BY id").unwrap();
    assert_eq!(r.rows, vec![vec![SqlValue::Int(1)], vec![SqlValue::Int(2)]]);
}

#[test]
fn ddl_boundary_is_per_session() {
    let mut eng = Engine::new(Vec::new());
    eng.execute("CREATE TABLE t (id INTEGER)").unwrap();

    eng.execute_session(1, "BEGIN").unwrap();
    // DDL inside session 1's own transaction is rejected.
    let err = eng
        .execute_session(1, "CREATE TABLE x (id INTEGER)")
        .unwrap_err();
    assert!(err.contains("DDL"), "{err}");

    // Session 2 is not in a transaction, so its DDL is allowed even though
    // session 1 holds an open transaction elsewhere.
    eng.execute_session(2, "CREATE TABLE other (id INTEGER)")
        .unwrap();
    eng.execute_session(2, "INSERT INTO other VALUES (7)")
        .unwrap();
    // CREATE INDEX backfill on the shared table reads committed rows only;
    // session 1's uncommitted rows are invisible to it.
    eng.execute_session(2, "CREATE INDEX ix ON t (id)").unwrap();
    eng.execute_session(2, "COMMIT").unwrap_err(); // no txn on session 2
    let r = eng
        .execute_session(2, "SELECT COUNT(*) FROM other")
        .unwrap();
    assert_eq!(cols(&r)[0], SqlValue::Int(1));

    eng.execute_session(1, "COMMIT").unwrap();
    let r = eng
        .execute_session(2, "SELECT COUNT(*) FROM other")
        .unwrap();
    assert_eq!(cols(&r)[0], SqlValue::Int(1));
}

#[test]
fn rollback_of_one_session_preserves_the_other_committed_rows() {
    let mut eng = Engine::new(Vec::new());
    eng.execute("CREATE TABLE t (id INTEGER, v TEXT)").unwrap();

    eng.execute_session(1, "BEGIN").unwrap();
    eng.execute_session(1, "INSERT INTO t VALUES (1, 'doomed')")
        .unwrap();
    eng.execute_session(2, "BEGIN").unwrap();
    eng.execute_session(2, "INSERT INTO t VALUES (2, 'kept')")
        .unwrap();
    eng.execute_session(2, "COMMIT").unwrap();

    eng.execute_session(1, "ROLLBACK").unwrap();
    let r = eng.execute("SELECT COUNT(*) FROM t").unwrap();
    assert_eq!(cols(&r)[0], SqlValue::Int(1));
    let r = eng.execute("SELECT id, v FROM t").unwrap();
    assert_eq!(
        r.rows,
        vec![vec![SqlValue::Int(2), SqlValue::Text("kept".into())]]
    );
}

#[test]
fn multiwriter_commits_are_durable_across_reopen() {
    let dir = fresh_dir("reopen");
    {
        let mut eng = Engine::create_db(&dir).unwrap();
        eng.execute("CREATE TABLE t (id INTEGER, v TEXT)").unwrap();
        eng.execute_session(1, "BEGIN").unwrap();
        eng.execute_session(1, "INSERT INTO t VALUES (1, 's1'), (2, 's1')")
            .unwrap();
        eng.execute_session(2, "BEGIN").unwrap();
        eng.execute_session(2, "INSERT INTO t VALUES (10, 's2')")
            .unwrap();
        // Interleave: session 1 commits before session 2; both WAL groups are
        // durable and ordered.
        eng.execute_session(1, "COMMIT").unwrap();
        eng.execute_session(2, "INSERT INTO t VALUES (11, 's2')")
            .unwrap();
        eng.execute_session(2, "COMMIT").unwrap();
        eng.close().unwrap();
    }

    let mut eng = Engine::open_db(&dir).unwrap();
    let r = eng.execute("SELECT id, v FROM t ORDER BY id").unwrap();
    assert_eq!(
        r.rows,
        vec![
            vec![SqlValue::Int(1), SqlValue::Text("s1".into())],
            vec![SqlValue::Int(2), SqlValue::Text("s1".into())],
            vec![SqlValue::Int(10), SqlValue::Text("s2".into())],
            vec![SqlValue::Int(11), SqlValue::Text("s2".into())],
        ],
        "both sessions' committed rows survive reopen"
    );
    // A session that was mid-transaction at close (crash-equivalent) leaves no
    // trace: open, insert, close without COMMIT, reopen clean.
    eng.execute_session(1, "BEGIN").unwrap();
    eng.execute_session(1, "INSERT INTO t VALUES (99, 'inflight')")
        .unwrap();
    let r = eng.execute_session(2, "BEGIN").unwrap();
    assert!(
        r.columns.is_empty(),
        "session 2 may begin during session 1's txn"
    );
    drop(eng);

    let mut eng = Engine::open_db(&dir).unwrap();
    let r = eng.execute("SELECT COUNT(*) FROM t").unwrap();
    assert_eq!(
        cols(&r)[0],
        SqlValue::Int(4),
        "in-flight transaction at crash must not leak"
    );
    eng.close().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
