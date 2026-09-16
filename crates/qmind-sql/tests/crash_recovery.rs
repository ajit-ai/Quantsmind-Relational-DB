//! R2 crash-recovery integration tests (R2.14 / R2.25). Each "crash" is a real
//! subprocess that dies via `std::process::exit` -- destructors never run -- so
//! recovery is proven against on-disk state alone.

use std::path::Path;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn fixture() -> PathBuf {
    // Cargo sets this at compile time when building integration tests for a
    // package that owns the [[bin]] target; the name is used exactly as-is in
    // the manifest ("qmind-persistence-fixture", hyphens preserved).
    PathBuf::from(env!("CARGO_BIN_EXE_qmind-persistence-fixture"))
}

fn fresh_dir(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "qmind-r2-crash-{tag}-pid{}-{}",
        std::process::id(),
        n
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn run(args: &[&str], want: i32) -> Output {
    let out = Command::new(fixture())
        .args(args)
        .output()
        .expect("spawn fixture");
    let code = out.status.code();
    assert_eq!(
        code,
        Some(want),
        "fixture {args:?} exited {code:?}, wanted {want};\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn verify_count(dir: &Path, table: &str) -> i64 {
    let out = run(&["verify-count", dir.to_str().unwrap(), table], 0);
    let text = String::from_utf8_lossy(&out.stdout);
    let text = text.trim();
    text.parse()
        .unwrap_or_else(|_| panic!("verify-count returned non-numeric stdout: {text:?}"))
}

fn verify_lookup(dir: &Path, table: &str, column: &str, value: &str) -> i64 {
    let out = run(
        &["verify-lookup", dir.to_str().unwrap(), table, column, value],
        0,
    );
    let text = String::from_utf8_lossy(&out.stdout);
    let text = text.trim();
    text.parse()
        .unwrap_or_else(|_| panic!("verify-lookup stdout: {text:?}"))
}

fn init(dir: &Path) {
    run(&["init", dir.to_str().unwrap()], 0);
}

fn commit_rows(dir: &Path, table: &str, n: i64) {
    // Crash exit 3: process dies inside the write loop, no close().
    run(
        &["commit-rows", dir.to_str().unwrap(), table, &n.to_string()],
        3,
    );
}

fn insert_uncommitted(dir: &Path, table: &str, n: i64) {
    run(
        &[
            "insert-uncommitted",
            dir.to_str().unwrap(),
            table,
            &n.to_string(),
        ],
        3,
    );
}

#[test]
fn committed_rows_survive_crash_without_close() {
    let dir = fresh_dir("committed");
    init(&dir);
    commit_rows(&dir, "customers", 3);
    assert_eq!(verify_count(&dir, "customers"), 3);
    assert_eq!(verify_lookup(&dir, "customers", "name", "user_1"), 1);
}

#[test]
fn uncommitted_inflight_writes_are_rolled_back() {
    let dir = fresh_dir("inflight");
    init(&dir);
    // Physically appended + fsynced to the WAL, but the transaction never
    // commits: recovery must discard it.
    insert_uncommitted(&dir, "customers", 4);
    assert_eq!(verify_count(&dir, "customers"), 0);
    assert_eq!(verify_lookup(&dir, "customers", "name", "ghost_0"), 0);
}

#[test]
fn committed_and_uncommitted_coexist_in_one_log() {
    let dir = fresh_dir("mixed");
    init(&dir);
    commit_rows(&dir, "customers", 2);
    insert_uncommitted(&dir, "orders", 5);
    assert_eq!(verify_count(&dir, "customers"), 2);
    assert_eq!(verify_count(&dir, "orders"), 0);
}

#[test]
fn multiple_tables_survive_crash() {
    let dir = fresh_dir("tables");
    init(&dir);
    commit_rows(&dir, "customers", 2);
    commit_rows(&dir, "orders", 3);
    assert_eq!(verify_count(&dir, "customers"), 2);
    assert_eq!(verify_count(&dir, "orders"), 3);
}

#[test]
fn index_durable_ddl_rebuilt_after_crash() {
    let dir = fresh_dir("index");
    init(&dir);
    commit_rows(&dir, "customers", 5);
    // CREATE INDEX autocommits a DDL record, then the process dies.
    run(&["create-index", dir.to_str().unwrap()], 3);
    // Index must be durable (catalog) and rebuilt from rows on open.
    assert_eq!(verify_lookup(&dir, "customers", "name", "user_1"), 1);
    assert_eq!(verify_lookup(&dir, "customers", "name", "user_4"), 1);
    assert_eq!(verify_count(&dir, "customers"), 5);
    // And the rebuilt index still tracks new rows after further commits +
    // restarts (commit_rows restarts names at user_0, so it is now duplicated).
    commit_rows(&dir, "customers", 2);
    assert_eq!(verify_lookup(&dir, "customers", "name", "user_0"), 2);
}

#[test]
fn repeated_crash_restart_cycles_accumulate() {
    let dir = fresh_dir("cycles");
    init(&dir);
    for i in 1..=3 {
        commit_rows(&dir, "customers", 1);
        assert_eq!(verify_count(&dir, "customers"), i);
    }
}

#[test]
fn r2_25_acceptance_crash_recovery_scenario() {
    // Acceptance regression for the R2 crash/recovery contract: every phase a
    // fresh database process, including hard kills mid-write and an appended
    // (durable but uncommitted) transaction, must end in exactly the
    // committed-visible state.
    let dir = fresh_dir("acceptance");
    init(&dir);

    // Phase 1: committed inserts, killed mid-loop.
    commit_rows(&dir, "customers", 2);
    assert_eq!(verify_count(&dir, "customers"), 2);

    // Phase 2: durable index DDL, killed before clean shutdown.
    run(&["create-index", dir.to_str().unwrap()], 3);
    assert_eq!(verify_lookup(&dir, "customers", "name", "user_1"), 1);

    // Phase 3: a second table gets durable-but-uncommitted writes; recovery
    // must drop them while keeping every earlier commit.
    insert_uncommitted(&dir, "orders", 5);
    assert_eq!(verify_count(&dir, "customers"), 2);
    assert_eq!(verify_count(&dir, "orders"), 0);

    // Phase 4: further crash/restart cycles keep accumulating committed rows,
    // and the rebuilt index tracks the post-crash writes (each cycle reuses
    // name user_0, so it accumulates).
    for i in 3..=5 {
        commit_rows(&dir, "customers", 1);
        assert_eq!(verify_count(&dir, "customers"), i);
    }
    assert_eq!(verify_lookup(&dir, "customers", "name", "user_1"), 1);
    assert_eq!(verify_lookup(&dir, "customers", "name", "user_0"), 4);
}

#[test]
fn page_store_rebuilt_matches_wal_state_after_crash() {
    // R3-STORAGE regression: the persistent page store is rebuilt from the
    // WAL on open, but `verify-count`/`verify-lookup` only read the MVCC side.
    // This test drives post-crash reads through `stream_query` (the page
    // store) and cross-checks them against the WAL-recovered MVCC rows, so a
    // divergence in the reconstructed page storage fails loudly.
    let dir = fresh_dir("pages");
    init(&dir);
    // Both tables committed, each process crashes hard mid-write.
    commit_rows(&dir, "customers", 40);
    commit_rows(&dir, "orders", 25);
    // Recovery rebuilds BOTH tables' page storage from the WAL; verify-stream
    // compares stream_query vs execute row-for-row inside a fresh process.
    run(&["verify-stream", dir.to_str().unwrap()], 0);
    assert_eq!(verify_count(&dir, "customers"), 40);
    assert_eq!(verify_count(&dir, "orders"), 25);

    // Further crash/restart cycles keep page storage in lockstep with the log.
    commit_rows(&dir, "customers", 7);
    run(&["verify-stream", dir.to_str().unwrap()], 0);
    commit_rows(&dir, "orders", 3);
    run(&["verify-stream", dir.to_str().unwrap()], 0);
    assert_eq!(verify_count(&dir, "customers"), 47);
    assert_eq!(verify_count(&dir, "orders"), 28);
}

#[test]
fn uncommitted_rows_never_leak_into_rebuilt_pages() {
    // The WAL drops in-flight transactions on recovery; the rebuilt page
    // store must not contain those ghost rows either (both read paths agree).
    let dir = fresh_dir("ghost-pages");
    init(&dir);
    commit_rows(&dir, "customers", 2);
    insert_uncommitted(&dir, "orders", 5);
    // Orders table: 0 committed rows in both MVCC and page storage.
    run(&["verify-stream", dir.to_str().unwrap()], 0);
    assert_eq!(verify_count(&dir, "customers"), 2);
    assert_eq!(verify_count(&dir, "orders"), 0);
}
