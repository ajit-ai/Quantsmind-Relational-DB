//! Crash-test fixture binary (R2.13 / R2.14 / R2.25).
//!
//! Integration tests spawn this process to exercise the durable database
//! lifecycle across real `open`/`create`/`exit` boundaries. `die` uses
//! `std::process::exit`, which skips destructors — exactly the abrupt-teardown
//! that a crash produces, and the strongest proof that recovery never depends
//! on `close()` being called.
//!
//! Subcommands:
//! - `init <dir>`                 create an empty database with customers/orders
//! - `commit-rows <dir> <t> <n>`  open, insert `n` committed rows into `t`, crash
//! - `create-index <dir>`         open, CREATE INDEX on customers(name), crash
//! - `insert-uncommitted <dir> <t> <n>`  append an in-flight txn (no Commit
//!   record) to the WAL tail, then crash
//! - `verify-count <dir> <t>`     open, print `SELECT COUNT(*)` for `t`, exit 0
//! - `verify-lookup <dir> <t> <col> <val>` open, print range/equality count, exit 0
//! - `verify-stream <dir>`        open, cross-check EVERY table's rebuilt page
//!   store (stream_query) against the WAL-recovered MVCC rows (execute); exit
//!   0 only if both agree for all tables (R3 storage rebuild regression)

use std::path::Path;
use std::path::PathBuf;

use qmind_kernel::{WalRecord, WalWriter};
use qmind_sql::codec::{encode_row, row_key, SqlValue};
use qmind_sql::Engine;

fn die(code: i32) -> ! {
    // Deliberately bypass destructors: this is the "process died" boundary.
    std::process::exit(code)
}

fn insert_sql(table: &str, i: u64) -> String {
    match table {
        "customers" => format!("INSERT INTO customers VALUES ({}, 'user_{}')", i + 1, i),
        "orders" => format!(
            "INSERT INTO orders VALUES ({}, {}, {})",
            i + 1,
            (i % 5) + 1,
            i * 100
        ),
        other => panic!("fixture: unknown table {other}"),
    }
}

/// Append `count` rows belonging to a transaction that never commits.
fn append_uncommitted(dir: &Path, table: &str, count: u64) {
    let wal_path = dir.join("wal").join("wal.log");
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .create(true)
        .truncate(false)
        .append(true)
        .open(&wal_path)
        .expect("open wal for uncommitted writes");
    let txn: u64 = 100_000 + count;
    let mut w = WalWriter::new(&mut f);
    w.append(&WalRecord::Begin { txn });
    for i in 0..count {
        let key = row_key(table, 5_000 + i);
        let value = match table {
            "customers" => encode_row(&[
                SqlValue::Int((i * 7 + 1) as i64),
                SqlValue::Text(format!("ghost_{i}")),
            ]),
            "orders" => encode_row(&[
                SqlValue::Int((i * 7 + 1) as i64),
                SqlValue::Int((i % 5 + 1) as i64),
                SqlValue::Int((i * 777) as i64),
            ]),
            other => panic!("fixture: unknown table {other}"),
        };
        w.append(&WalRecord::Put { txn, key, value });
    }
    // No Commit record: the transaction was in flight when the process died.
    w.commit_group().expect("flush uncommitted group");
    drop(w);
    f.sync_data().expect("sync uncommitted group");
    drop(f);
}

fn open_verify(dir: &PathBuf) -> Engine<std::fs::File> {
    Engine::<std::fs::File>::open_db(dir).expect("open_db in verify")
}

fn print_count(db: &mut Engine<std::fs::File>, sql: &str) {
    let r = db.execute(sql).expect("query");
    let n = match &r.rows[0][0] {
        SqlValue::Int(n) => *n,
        other => panic!("fixture: expected integer count, got {other:?}"),
    };
    println!("{n}");
}

fn main() {
    let mut args = std::env::args();
    let cmd = args.nth(1).unwrap_or_default();
    let dir = PathBuf::from(args.next().unwrap_or_default());

    match cmd.as_str() {
        "init" => {
            let mut db = Engine::<std::fs::File>::create_db(&dir).expect("create_db");
            db.execute("CREATE TABLE customers (id INTEGER NOT NULL, name TEXT)")
                .expect("create customers");
            db.execute(
                "CREATE TABLE orders (id INTEGER NOT NULL, cid INTEGER NOT NULL, amount INTEGER)",
            )
            .expect("create orders");
            db.close().expect("close");
            die(0);
        }
        "commit-rows" => {
            let table = args.next().unwrap_or_default();
            let count: u64 = args.next().map(|s| s.parse().expect("count")).unwrap_or(0);
            let mut db = Engine::<std::fs::File>::open_db(&dir).expect("open_db");
            for i in 0..count {
                db.execute(&insert_sql(&table, i)).expect("insert");
            }
            // Crash here: committed groups were fsynced per statement; the
            // process dies without close().
            die(3);
        }
        "create-index" => {
            let mut db = Engine::<std::fs::File>::open_db(&dir).expect("open_db");
            db.execute("CREATE INDEX idx_customers_name ON customers (name)")
                .expect("create index");
            die(3);
        }
        "insert-uncommitted" => {
            let table = args.next().unwrap_or_default();
            let count: u64 = args.next().map(|s| s.parse().expect("count")).unwrap_or(0);
            append_uncommitted(&dir, &table, count);
            die(3);
        }
        "verify-count" => {
            let table = args.next().unwrap_or_default();
            let mut db = open_verify(&dir);
            print_count(&mut db, &format!("SELECT COUNT(*) FROM {table}"));
            db.close().expect("close");
            die(0);
        }
        "verify-lookup" => {
            let table = args.next().unwrap_or_default();
            let column = args.next().unwrap_or_default();
            let value = args.next().unwrap_or_default();
            let mut db = open_verify(&dir);
            print_count(
                &mut db,
                &format!("SELECT COUNT(*) FROM {table} WHERE {column} = '{value}'"),
            );
            db.close().expect("close");
            die(0);
        }
        "verify-stream" => {
            let mut db = open_verify(&dir);
            // R3-STORAGE regression: after crash recovery the page store is
            // rebuilt from the WAL-recovered MVCC state. Compare every table's
            // stream_query (page scan) against execute (MVCC) row-for-row;
            // a mismatch means the rebuilt page storage diverged from the log.
            let show: Vec<String> = db
                .execute("SHOW TABLES")
                .expect("show tables")
                .rows
                .into_iter()
                .map(|r| match &r[0] {
                    SqlValue::Text(t) => t.clone(),
                    other => panic!("fixture: unexpected SHOW TABLES value {other:?}"),
                })
                .collect();
            for table in show {
                let sql = format!("SELECT * FROM {table}");
                let via_mvcc = db.execute(&sql).expect("execute");
                let mut via_pages: Vec<Vec<SqlValue>> = Vec::new();
                db.stream_query(&sql, |b| {
                    for i in 0..b.num_rows() {
                        via_pages.push(b.row(i));
                    }
                    Ok(())
                })
                .expect("stream_query");
                if via_mvcc.rows != via_pages {
                    eprintln!(
                        "fixture: verify-stream: table `{table}` mismatch:\n  mvcc = {via_mvcc:?}\n  pages = {via_pages:?}"
                    );
                    die(1);
                }
            }
            db.close().expect("close");
            die(0);
        }
        other => {
            eprintln!("fixture: unknown subcommand {other}");
            die(64);
        }
    }
}
