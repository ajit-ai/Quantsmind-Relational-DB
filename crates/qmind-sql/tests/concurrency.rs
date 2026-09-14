//! P5 SQL-level concurrency: `Arc<RwLock<Engine>>` with snapshot reads.
//!
//! Mirrors the kernel `read_stress` harness one layer up: a single engine is
//! shared behind an `RwLock`, a writer thread runs multi-row INSERT commits,
//! and reader threads call `Engine::execute_read` (which locks only long
//! enough to capture a snapshot). Invariants:
//!
//! 1. No torn reads â€” every `COUNT(*)` observed is a fully-committed batch
//!    boundary (batch_size Ã— an integer).
//! 2. `execute_read` rejects write statements (read-only guard).
//! 3. Snapshot consistency: `COUNT(*) == MAX(id)+1` for the single-writer
//!    contiguous-id workload.
//! 4. Concurrent writers never lose committed rows.

use qmind_sql::{Engine, SqlValue};
use std::sync::{Arc, RwLock};

type Shared = Arc<RwLock<Engine<Vec<u8>>>>;

fn setup() -> Shared {
    let e = Arc::new(RwLock::new(Engine::new(Vec::new())));
    {
        let mut g = e.write().unwrap();
        g.execute("CREATE TABLE t (id INTEGER, grp INTEGER, val INTEGER)")
            .unwrap();
    }
    e
}

fn count_rows(e: &Shared) -> u64 {
    let g = e.read().unwrap();
    let res = g.execute_read("SELECT COUNT(*) FROM t").unwrap();
    match &res.rows[0][0] {
        SqlValue::Int(n) => *n as u64,
        other => panic!("unexpected COUNT type: {other:?}"),
    }
}

fn count_and_max_id(e: &Shared) -> (u64, u64) {
    let g = e.read().unwrap();
    let res = g.execute_read("SELECT COUNT(*), MAX(id) FROM t").unwrap();
    match (&res.rows[0][0], &res.rows[0][1]) {
        (SqlValue::Int(n), SqlValue::Int(m)) => (*n as u64, *m as u64),
        other => panic!("unexpected aggregate types: {other:?}"),
    }
}

#[test]
fn batch_commits_are_never_torn_for_concurrent_readers() {
    const BATCH: u64 = 25;
    const BATCHES: u64 = 20;
    let e = setup();

    // Writer: BATCHES Ã— BATCH-row single-txn commits on contiguous ids.
    let writer = {
        let e = Arc::clone(&e);
        std::thread::spawn(move || {
            let mut g = e.write().unwrap();
            for b in 0..BATCHES {
                let vals: Vec<String> = (0..BATCH)
                    .map(|i| format!("({}, {}, {})", b * BATCH + i, i, b))
                    .collect();
                g.execute(&format!("INSERT INTO t VALUES {};", vals.join(",")))
                    .unwrap();
            }
            drop(g);
        })
    };

    let mut readers = Vec::new();
    for _ in 0..4 {
        let e = Arc::clone(&e);
        readers.push(std::thread::spawn(move || {
            for _ in 0..100 {
                let n = count_rows(&e);
                assert_eq!(n % BATCH, 0, "reader observed a torn commit: count={n}");
            }
        }));
    }
    for r in readers {
        r.join().unwrap();
    }
    writer.join().unwrap();
    assert_eq!(count_rows(&e), BATCH * BATCHES);
}

#[test]
fn execute_read_rejects_write_statements() {
    let e = setup();
    let g = e.read().unwrap();
    assert!(g.execute_read("INSERT INTO t VALUES (1, 1, 1)").is_err());
    assert!(g.execute_read("CREATE TABLE x (id INTEGER)").is_err());
    assert!(g.execute_read("CREATE INDEX ix ON t (id)").is_err());
    assert!(g.execute_read("SELECT * FROM t").is_ok());
    assert!(g.execute_read("SHOW TABLES").is_ok());
}

#[test]
fn every_snapshot_is_internally_consistent() {
    const BATCH: u64 = 20;
    const BATCHES: u64 = 30;
    let e = setup();
    // Prime with a contiguous block so COUNT == MAX(id)+1 holds from the start.
    {
        let mut g = e.write().unwrap();
        let vals: Vec<String> = (0..BATCH).map(|i| format!("({i}, 0, 0)")).collect();
        g.execute(&format!("INSERT INTO t VALUES {};", vals.join(",")))
            .unwrap();
    }

    let writer = {
        let e = Arc::clone(&e);
        std::thread::spawn(move || {
            let mut g = e.write().unwrap();
            for b in 0..BATCHES {
                let vals: Vec<String> = (0..BATCH)
                    .map(|i| format!("({}, 1, {})", BATCH + b * BATCH + i, b))
                    .collect();
                g.execute(&format!("INSERT INTO t VALUES {};", vals.join(",")))
                    .unwrap();
            }
        })
    };

    let mut readers = Vec::new();
    for _ in 0..3 {
        let e = Arc::clone(&e);
        readers.push(std::thread::spawn(move || {
            for _ in 0..120 {
                let (count, max_id) = count_and_max_id(&e);
                assert_eq!(
                    count,
                    max_id + 1,
                    "snapshot inconsistent: count={count} max_id={max_id}"
                );
            }
        }));
    }
    for r in readers {
        r.join().unwrap();
    }
    writer.join().unwrap();
    let (count, max_id) = count_and_max_id(&e);
    assert_eq!(count, max_id + 1);
    assert_eq!(count, BATCH * (BATCHES + 1));
}

#[test]
fn concurrent_writers_never_lose_committed_rows() {
    const PER_WRITER: u64 = 200;
    let e = setup();
    let mut writers = Vec::new();
    for w in 0..4u64 {
        let e = Arc::clone(&e);
        writers.push(std::thread::spawn(move || {
            let mut g = e.write().unwrap();
            for i in 0..PER_WRITER {
                g.execute(&format!(
                    "INSERT INTO t VALUES ({}, {}, {})",
                    w * PER_WRITER + i,
                    w,
                    i
                ))
                .unwrap();
            }
        }));
    }
    for w in writers {
        w.join().unwrap();
    }
    assert_eq!(count_rows(&e), 4 * PER_WRITER, "lost committed rows");
}
