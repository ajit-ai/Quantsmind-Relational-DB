//! R4 — explicit transaction lifecycle + SQL transaction control.
//!
//! Covers BEGIN/COMMIT/ROLLBACK over the engine: lifecycle edges (begin while
//! active, commit/rollback with no txn, repeated commit/rollback), own-write
//! visibility inside a transaction, deferred index/columnar/page materialization
//! (a ROLLBACK must never leak phantom index state), and durability of committed
//! vs rolled-back transactions across reopen.

use qmind_sql::{Engine, SqlValue};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn fresh_dir(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "qmind-r4-txn-{tag}-pid{}-{}",
        std::process::id(),
        n
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn cols(r: &qmind_sql::ExecResult) -> Vec<SqlValue> {
    r.rows[0].clone()
}

#[test]
fn begin_insert_commit_is_durable_and_visible() {
    let mut eng = Engine::new(Vec::new());
    eng.execute("CREATE TABLE t (id INTEGER, v TEXT)").unwrap();

    eng.execute("BEGIN").unwrap();
    eng.execute("INSERT INTO t VALUES (1, 'a')").unwrap();
    eng.execute("INSERT INTO t VALUES (2, 'b')").unwrap();
    // Writes inside the txn are invisible to a fresh snapshot (execute_read
    // uses its own snapshot — never the transaction's).
    let r = eng.execute("SELECT COUNT(*) FROM t").unwrap();
    assert_eq!(
        cols(&r)[0],
        SqlValue::Int(2),
        "own writes visible inside txn"
    );
    eng.execute("COMMIT").unwrap();

    let r = eng.execute("SELECT COUNT(*) FROM t").unwrap();
    assert_eq!(cols(&r)[0], SqlValue::Int(2), "committed rows are visible");
    let r = eng.execute("SELECT v FROM t WHERE id = 1").unwrap();
    assert_eq!(r.rows[0], vec![SqlValue::Text("a".into())]);
}

#[test]
fn transaction_reads_see_own_uncommitted_writes() {
    let mut eng = Engine::new(Vec::new());
    eng.execute("CREATE TABLE t (id INTEGER, v TEXT)").unwrap();
    eng.execute("BEGIN").unwrap();
    eng.execute("INSERT INTO t VALUES (7, 'own')").unwrap();
    // Full scan must include the buffered row (read-your-own-writes).
    let r = eng.execute("SELECT id, v FROM t").unwrap();
    assert_eq!(r.rows.len(), 1);
    assert_eq!(
        r.rows[0],
        vec![SqlValue::Int(7), SqlValue::Text("own".into())]
    );
    let r = eng.execute("SELECT COUNT(*) FROM t").unwrap();
    assert_eq!(cols(&r)[0], SqlValue::Int(1));
    eng.execute("ROLLBACK").unwrap();
    let r = eng.execute("SELECT COUNT(*) FROM t").unwrap();
    assert_eq!(
        cols(&r)[0],
        SqlValue::Int(0),
        "rolled back writes invisible"
    );
}

#[test]
fn rollback_discards_buffered_rows_and_index_entries() {
    let mut eng = Engine::new(Vec::new());
    eng.execute("CREATE TABLE t (id INTEGER, v TEXT)").unwrap();
    eng.execute("CREATE INDEX iv ON t (v)").unwrap();

    eng.execute("BEGIN").unwrap();
    eng.execute("INSERT INTO t VALUES (1, 'keep')").unwrap();
    eng.execute("INSERT INTO t VALUES (2, 'discard')").unwrap();
    eng.execute("ROLLBACK").unwrap();

    let r = eng.execute("SELECT COUNT(*) FROM t").unwrap();
    assert_eq!(cols(&r)[0], SqlValue::Int(0));
    // The index must not contain the rolled-back `discard` row either.
    let r = eng.execute("SELECT id FROM t WHERE v = 'discard'").unwrap();
    assert_eq!(r.rows.len(), 0, "no phantom index entry after rollback");

    // Continue in autocommit: no resurrection, no corruption, and new rows
    // still index correctly.
    eng.execute("INSERT INTO t VALUES (3, 'later')").unwrap();
    let r = eng.execute("SELECT id FROM t WHERE v = 'later'").unwrap();
    assert_eq!(r.rows[0], vec![SqlValue::Int(3)]);
}

#[test]
fn lifecycle_edges_are_rejected_cleanly() {
    let mut eng = Engine::new(Vec::new());
    eng.execute("CREATE TABLE t (id INTEGER)").unwrap();

    // COMMIT/ROLLBACK with no transaction in progress.
    let err = eng.execute("COMMIT").unwrap_err();
    assert!(err.contains("no transaction"), "{err}");
    let err = eng.execute("ROLLBACK").unwrap_err();
    assert!(err.contains("no transaction"), "{err}");

    // BEGIN while already active.
    eng.execute("BEGIN").unwrap();
    let err = eng.execute("BEGIN").unwrap_err();
    assert!(err.contains("already in progress"), "{err}");

    // DDL is rejected inside an explicit transaction.
    let err = eng.execute("CREATE TABLE x (id INTEGER)").unwrap_err();
    assert!(err.contains("DDL"), "{err}");
    let err = eng.execute("CREATE INDEX ix ON t (id)").unwrap_err();
    assert!(err.contains("DDL"), "{err}");

    // COMMIT then repeated COMMIT is an error.
    eng.execute("COMMIT").unwrap();
    let err = eng.execute("COMMIT").unwrap_err();
    assert!(err.contains("no transaction"), "{err}");

    // ROLLBACK then repeated ROLLBACK is an error.
    eng.execute("BEGIN").unwrap();
    eng.execute("ROLLBACK").unwrap();
    let err = eng.execute("ROLLBACK").unwrap_err();
    assert!(err.contains("no transaction"), "{err}");
}

#[test]
fn begin_transaction_work_commit_variants_parse() {
    let mut eng = Engine::new(Vec::new());
    eng.execute("CREATE TABLE t (id INTEGER)").unwrap();

    // Standard spellings supported: BEGIN[ TRANSACTION|WORK], and the same
    // optional suffix on COMMIT/ROLLBACK.
    eng.execute("BEGIN TRANSACTION").unwrap();
    eng.execute("INSERT INTO t VALUES (1)").unwrap();
    eng.execute("COMMIT WORK").unwrap();

    eng.execute("BEGIN WORK").unwrap();
    eng.execute("INSERT INTO t VALUES (2)").unwrap();
    eng.execute("ROLLBACK TRANSACTION").unwrap();
    assert!(eng.execute("COMMIT").is_err(), "no txn after ROLLBACK");

    eng.execute("BEGIN").unwrap();
    eng.execute("INSERT INTO t VALUES (3)").unwrap();
    eng.execute("COMMIT WORK").unwrap();

    let r = eng.execute("SELECT id FROM t").unwrap();
    assert_eq!(r.rows, vec![vec![SqlValue::Int(1)], vec![SqlValue::Int(3)]]);
}

#[test]
fn autocommit_implicit_and_explicit_transactions_interleave() {
    let mut eng = Engine::new(Vec::new());
    eng.execute("CREATE TABLE t (id INTEGER)").unwrap();

    eng.execute("BEGIN").unwrap();
    eng.execute("INSERT INTO t VALUES (1)").unwrap();
    eng.execute("COMMIT").unwrap();

    // Autocommit statement after an explicit txn.
    eng.execute("INSERT INTO t VALUES (2)").unwrap();

    eng.execute("BEGIN").unwrap();
    eng.execute("INSERT INTO t VALUES (3)").unwrap();
    eng.execute("ROLLBACK").unwrap();
    eng.execute("INSERT INTO t VALUES (4)").unwrap();

    let r = eng.execute("SELECT id FROM t").unwrap();
    assert_eq!(r.rows.len(), 3);
    let ids: Vec<i64> = r
        .rows
        .iter()
        .map(|r| match r[0] {
            SqlValue::Int(n) => n,
            _ => panic!("expected INT"),
        })
        .collect();
    assert_eq!(ids, vec![1, 2, 4]);
}

#[test]
fn commit_survives_reopen_rollback_and_inflight_do_not() {
    let dir = fresh_dir("persist");
    {
        let mut eng = Engine::create_db(&dir).unwrap();
        eng.execute("CREATE TABLE t (id INTEGER, v TEXT)").unwrap();
        eng.execute("BEGIN").unwrap();
        eng.execute("INSERT INTO t VALUES (1, 'committed')")
            .unwrap();
        eng.execute("INSERT INTO t VALUES (2, 'rolled')").unwrap();
        eng.execute("ROLLBACK").unwrap();
        eng.execute("BEGIN").unwrap();
        eng.execute("INSERT INTO t VALUES (1, 'committed')")
            .unwrap();
        eng.execute("COMMIT").unwrap();
        eng.close().unwrap();
    }

    // Reopen: only the committed row is present.
    let mut eng = Engine::open_db(&dir).unwrap();
    let r = eng.execute("SELECT COUNT(*) FROM t").unwrap();
    assert_eq!(cols(&r)[0], SqlValue::Int(1));
    let r = eng.execute("SELECT v FROM t WHERE id = 1").unwrap();
    assert_eq!(r.rows[0], vec![SqlValue::Text("committed".into())]);

    // An in-flight transaction (BEGIN, no COMMIT) must not leak after a crash:
    // simulate with drop (no clean close) and reopen.
    eng.execute("BEGIN").unwrap();
    eng.execute("INSERT INTO t VALUES (99, 'inflight')")
        .unwrap();
    drop(eng); // no close(), no COMMIT — crash-equivalent for WAL content

    let mut eng = Engine::open_db(&dir).unwrap();
    let r = eng.execute("SELECT COUNT(*) FROM t").unwrap();
    assert_eq!(cols(&r)[0], SqlValue::Int(1), "in-flight txn must not leak");
    eng.close().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

// --- R4-MVCC: snapshot-isolation visibility through the SQL layer -----------

#[test]
fn index_lookup_inside_explicit_txn_sees_own_uncommitted_writes() {
    let mut eng = Engine::new(Vec::new());
    eng.execute("CREATE TABLE t (id INTEGER, v TEXT)").unwrap();
    eng.execute("CREATE INDEX iv ON t (v)").unwrap();
    eng.execute("INSERT INTO t VALUES (0, 'committed')")
        .unwrap();

    eng.execute("BEGIN").unwrap();
    // Uncommitted row whose indexed column has no index entry yet (deferred
    // materialization). A plain scan sees it; an index-assisted equality
    // lookup must see it too (own-write visibility through the index path).
    eng.execute("INSERT INTO t VALUES (1, 'own-uncommitted')")
        .unwrap();
    let r = eng
        .execute("SELECT id FROM t WHERE v = 'own-uncommitted'")
        .unwrap();
    assert_eq!(
        r.rows,
        vec![vec![SqlValue::Int(1)]],
        "own uncommitted row must be reachable through an indexed equality lookup"
    );
    eng.execute("ROLLBACK").unwrap();
}

#[test]
fn rolled_back_rid_gaps_do_not_misalign_create_index_backfill() {
    let mut eng = Engine::new(Vec::new());
    eng.execute("CREATE TABLE t (id INTEGER, v TEXT)").unwrap();
    eng.execute("INSERT INTO t VALUES (1, 'a')").unwrap();
    // Rid 1 is allocated and then discarded by a rollback, opening a gap.
    eng.execute("BEGIN").unwrap();
    eng.execute("INSERT INTO t VALUES (999, 'discarded')")
        .unwrap();
    eng.execute("ROLLBACK").unwrap();
    eng.execute("INSERT INTO t VALUES (2, 'b')").unwrap();
    eng.execute("INSERT INTO t VALUES (3, 'c')").unwrap();

    eng.execute("CREATE INDEX iv ON t (v)").unwrap();
    let r = eng.execute("SELECT id FROM t WHERE v = 'b'").unwrap();
    assert_eq!(r.rows, vec![vec![SqlValue::Int(2)]]);
    let r = eng.execute("SELECT id FROM t WHERE v = 'c'").unwrap();
    assert_eq!(r.rows, vec![vec![SqlValue::Int(3)]]);
    let r = eng.execute("SELECT COUNT(*) FROM t").unwrap();
    assert_eq!(cols(&r)[0], SqlValue::Int(3));
}

#[test]
fn explicit_txn_select_uses_row_store_not_columnar_snapshot() {
    let dir = fresh_dir("columnar");
    let mut eng = Engine::new(Vec::new()).with_columnar(dir.clone());
    eng.execute("CREATE TABLE p (id INTEGER NOT NULL, v TEXT)")
        .unwrap();
    eng.execute("INSERT INTO p VALUES (1, 'pre')").unwrap();
    eng.flush_to_columnar().unwrap();
    assert!(eng.has_columnar_data("p"));

    // Inside an explicit transaction the row store (MVCC) must be read so the
    // transaction's own buffered write is visible alongside committed rows.
    eng.execute("BEGIN").unwrap();
    eng.execute("INSERT INTO p VALUES (2, 'uncommitted')")
        .unwrap();
    let r = eng.execute("SELECT id, v FROM p").unwrap();
    assert_eq!(
        r.rows.len(),
        2,
        "explicit txn must see committed + own buffered rows, got {:?}",
        r.rows
    );
    let ids: Vec<i64> = r
        .rows
        .iter()
        .map(|r| match r[0] {
            SqlValue::Int(n) => n,
            _ => panic!("expected INT"),
        })
        .collect();
    assert_eq!(ids, vec![1, 2]);
    eng.execute("ROLLBACK").unwrap();

    // Autocommit SELECT still runs on the columnar fast path and sees only
    // the committed row.
    eng.flush_to_columnar().unwrap();
    let r = eng.execute("SELECT v FROM p").unwrap();
    assert_eq!(
        r.rows,
        vec![vec![SqlValue::Text("pre".into())]],
        "autocommit columnar read sees only committed rows"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn aggregate_group_by_and_join_see_own_uncommitted_rows() {
    let mut eng = Engine::new(Vec::new());

    // Tables + committed baseline established before BEGIN.
    eng.execute("CREATE TABLE t (id INTEGER, v TEXT)").unwrap();
    eng.execute("INSERT INTO t VALUES (1, 'a')").unwrap();
    eng.execute("CREATE TABLE orders (region TEXT, qty INTEGER)")
        .unwrap();
    eng.execute("INSERT INTO orders VALUES ('east', 10)")
        .unwrap();
    eng.execute("INSERT INTO orders VALUES ('west', 7)")
        .unwrap();
    eng.execute("CREATE TABLE a (id INTEGER, label TEXT)")
        .unwrap();
    eng.execute("CREATE TABLE b (cid INTEGER, w TEXT)").unwrap();
    eng.execute("INSERT INTO a VALUES (1, 'a1')").unwrap();
    eng.execute("INSERT INTO b VALUES (1, 'b1'), (2, 'b2')")
        .unwrap();

    eng.execute("BEGIN").unwrap();
    eng.execute("INSERT INTO t VALUES (2, 'b')").unwrap();
    // Aggregation fast path (COUNT) over committed + own buffered rows.
    let r = eng.execute("SELECT COUNT(*) FROM t").unwrap();
    assert_eq!(
        cols(&r)[0],
        SqlValue::Int(2),
        "COUNT includes own buffered row"
    );

    // GROUP BY: committed baseline plus a transaction-local order.
    eng.execute("INSERT INTO orders VALUES ('west', 3)")
        .unwrap();
    let r = eng
        .execute("SELECT region, COUNT(*) FROM orders GROUP BY region ORDER BY region")
        .unwrap();
    assert_eq!(
        r.rows,
        vec![
            vec![SqlValue::Text("east".into()), SqlValue::Int(1)],
            vec![SqlValue::Text("west".into()), SqlValue::Int(2)],
        ],
        "GROUP BY sees committed + own buffered rows"
    );

    // A JOIN where one side carries the transaction's own buffered row.
    eng.execute("INSERT INTO a VALUES (2, 'a2')").unwrap();
    let r = eng
        .execute("SELECT label, w FROM a JOIN b ON id = cid ORDER BY label")
        .unwrap();
    assert_eq!(
        r.rows,
        vec![
            vec![SqlValue::Text("a1".into()), SqlValue::Text("b1".into())],
            vec![SqlValue::Text("a2".into()), SqlValue::Text("b2".into())],
        ],
        "JOIN sees own buffered left-side row matched against committed right side"
    );

    // The join side scans must remain stable within the snapshot.
    eng.execute("ROLLBACK").unwrap();
    let r = eng.execute("SELECT COUNT(*) FROM t").unwrap();
    assert_eq!(cols(&r)[0], SqlValue::Int(1));
}

#[test]
fn mixed_committed_own_and_rolled_back_rows_in_one_visible_set() {
    let mut eng = Engine::new(Vec::new());
    eng.execute("CREATE TABLE t (id INTEGER, v TEXT)").unwrap();
    eng.execute("INSERT INTO t VALUES (1, 'pre')").unwrap();

    // A rolled-back transaction opens a rid gap without leaving state.
    eng.execute("BEGIN").unwrap();
    eng.execute("INSERT INTO t VALUES (2, 'discarded')")
        .unwrap();
    eng.execute("ROLLBACK").unwrap();

    // Current transaction borrows the snapshot established at BEGIN:
    // pre-existing committed row + its own writes.
    eng.execute("BEGIN").unwrap();
    eng.execute("INSERT INTO t VALUES (3, 'own1')").unwrap();
    let r = eng.execute("SELECT id FROM t ORDER BY id").unwrap();
    assert_eq!(
        r.rows,
        vec![vec![SqlValue::Int(1)], vec![SqlValue::Int(3)]],
        "visible set = committed-before-snapshot + own writes only"
    );
    eng.execute("INSERT INTO t VALUES (4, 'own2')").unwrap();
    let r = eng.execute("SELECT COUNT(*) FROM t").unwrap();
    assert_eq!(cols(&r)[0], SqlValue::Int(3));
    eng.execute("ROLLBACK").unwrap();

    let r = eng.execute("SELECT COUNT(*) FROM t").unwrap();
    assert_eq!(
        cols(&r)[0],
        SqlValue::Int(1),
        "rollback removes all own rows"
    );
}

#[test]
fn reopen_preserves_visibility_and_index_alignment_after_rollback_gaps() {
    let dir = fresh_dir("reopen-index");
    {
        let mut eng = Engine::create_db(&dir).unwrap();
        eng.execute("CREATE TABLE t (id INTEGER, v TEXT)").unwrap();
        eng.execute("INSERT INTO t VALUES (1, 'a')").unwrap();
        // Rollback leaves a rid gap.
        eng.execute("BEGIN").unwrap();
        eng.execute("INSERT INTO t VALUES (999, 'discarded')")
            .unwrap();
        eng.execute("ROLLBACK").unwrap();
        // Explicit transaction commit lands after the gap.
        eng.execute("BEGIN").unwrap();
        eng.execute("INSERT INTO t VALUES (2, 'b')").unwrap();
        eng.execute("COMMIT").unwrap();
        eng.close().unwrap();
    }

    // Reopen, then create the index on the gap-riddled committed set.
    let mut eng = Engine::open_db(&dir).unwrap();
    let r = eng.execute("SELECT COUNT(*) FROM t").unwrap();
    assert_eq!(cols(&r)[0], SqlValue::Int(2));

    eng.execute("CREATE INDEX iv ON t (v)").unwrap();
    let r = eng.execute("SELECT id FROM t WHERE v = 'b'").unwrap();
    assert_eq!(r.rows, vec![vec![SqlValue::Int(2)]]);
    let r = eng.execute("SELECT id FROM t WHERE v = 'a'").unwrap();
    assert_eq!(r.rows, vec![vec![SqlValue::Int(1)]]);

    // New transaction on the reopened engine sees the committed world.
    eng.execute("BEGIN").unwrap();
    let r = eng.execute("SELECT COUNT(*) FROM t").unwrap();
    assert_eq!(cols(&r)[0], SqlValue::Int(2));
    eng.execute("COMMIT").unwrap();
    eng.close().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn read_only_transaction_has_stable_repeated_views() {
    let mut eng = Engine::new(Vec::new());
    eng.execute("CREATE TABLE t (id INTEGER)").unwrap();
    for i in 0..5 {
        eng.execute(&format!("INSERT INTO t VALUES ({i})")).unwrap();
    }

    // No writes at all: SELECTs inside the transaction must agree with each
    // other and with a post-COMMIT statement (nothing changed meanwhile).
    eng.execute("BEGIN").unwrap();
    let r1 = eng.execute("SELECT COUNT(*) FROM t").unwrap();
    let r2 = eng.execute("SELECT COUNT(*) FROM t").unwrap();
    assert_eq!(r1.rows, r2.rows);
    assert_eq!(cols(&r1)[0], SqlValue::Int(5));
    eng.execute("COMMIT").unwrap();

    let r = eng.execute("SELECT COUNT(*) FROM t").unwrap();
    assert_eq!(cols(&r)[0], SqlValue::Int(5));
}

#[test]
fn ownership_released_after_commit_rollback_and_failure() {
    let mut eng = Engine::new(Vec::new());
    eng.execute("CREATE TABLE t (id INTEGER)").unwrap();
    assert!(!eng.in_transaction(), "no transaction before BEGIN");

    // COMMIT releases writer ownership.
    eng.execute("BEGIN").unwrap();
    assert!(eng.in_transaction());
    eng.execute("INSERT INTO t VALUES (1)").unwrap();
    eng.execute("COMMIT").unwrap();
    assert!(!eng.in_transaction());

    // ROLLBACK releases writer ownership.
    eng.execute("BEGIN").unwrap();
    assert!(eng.in_transaction());
    eng.execute("INSERT INTO t VALUES (2)").unwrap();
    eng.execute("ROLLBACK").unwrap();
    assert!(!eng.in_transaction());

    // A failed statement does not silently end the transaction (the documented
    // SQL semantic), but ROLLBACK still releases ownership afterwards.
    eng.execute("BEGIN").unwrap();
    eng.execute("INSERT INTO t VALUES (3)").unwrap();
    let err = eng.execute("INSERT INTO missing VALUES (1)").unwrap_err();
    assert!(!err.is_empty());
    assert!(eng.in_transaction(), "statement failure keeps the txn open");
    eng.execute("ROLLBACK").unwrap();
    assert!(!eng.in_transaction());

    // Only the first row ever committed.
    let r = eng.execute("SELECT COUNT(*) FROM t").unwrap();
    assert_eq!(cols(&r)[0], SqlValue::Int(1));

    // The engine is fully usable afterwards: ownership is never permanently stuck.
    eng.execute("BEGIN").unwrap();
    assert!(eng.in_transaction());
    eng.execute("INSERT INTO t VALUES (4)").unwrap();
    eng.execute("COMMIT").unwrap();
    assert!(!eng.in_transaction());
    let r = eng.execute("SELECT COUNT(*) FROM t").unwrap();
    assert_eq!(cols(&r)[0], SqlValue::Int(2));
}

#[test]
fn failed_statement_inside_txn_leaves_no_partial_state() {
    let mut eng = Engine::new(Vec::new());
    eng.execute("CREATE TABLE t (id INTEGER, v TEXT)").unwrap();

    eng.execute("BEGIN").unwrap();
    eng.execute("INSERT INTO t VALUES (1, 'kept')").unwrap();

    // Wrong arity: the statement fails without buffering anything.
    let err = eng.execute("INSERT INTO t VALUES (2)").unwrap_err();
    assert!(err.contains("has 2 columns"), "unexpected error: {err}");
    assert!(eng.in_transaction(), "statement failure keeps the txn open");

    // The successful earlier statement is still visible inside the txn.
    let r = eng.execute("SELECT COUNT(*) FROM t").unwrap();
    assert_eq!(cols(&r)[0], SqlValue::Int(1));

    // ROLLBACK discards everything, including the earlier successful write.
    eng.execute("ROLLBACK").unwrap();
    assert!(!eng.in_transaction());
    let r = eng.execute("SELECT COUNT(*) FROM t").unwrap();
    assert_eq!(cols(&r)[0], SqlValue::Int(0));

    // A subsequent write lands and the engine is healthy.
    eng.execute("INSERT INTO t VALUES (2, 'fresh')").unwrap();
    let r = eng.execute("SELECT COUNT(*) FROM t").unwrap();
    assert_eq!(cols(&r)[0], SqlValue::Int(1));
    let r = eng.execute("SELECT v FROM t WHERE id = 2").unwrap();
    assert_eq!(r.rows[0], vec![SqlValue::Text("fresh".into())]);
}
