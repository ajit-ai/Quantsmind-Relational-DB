//! R2 in-process persistence tests (R2.13 / R2.25): exercise `create_db`,
//! `open_db` and `close` directly, plus WAL torn-tail and corruption handling
//! and format-version mismatches.

use qmind_kernel::wal::WalWriter;
use qmind_kernel::{WalReader, WalRecord};
use qmind_sql::{Engine, SqlValue};
use std::io::Write as IoWrite;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn fresh_dir(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("qmind-r2-{tag}-pid{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn open_wal(dir: &Path) -> PathBuf {
    dir.join("wal").join("wal.log")
}

fn rows(db: &mut Engine<std::fs::File>, sql: &str) -> Vec<Vec<SqlValue>> {
    db.execute(sql).unwrap().rows
}

fn count(db: &mut Engine<std::fs::File>, table: &str) -> i64 {
    match rows(db, &format!("SELECT COUNT(*) FROM {table}"))[0][0] {
        SqlValue::Int(n) => n,
        _ => panic!("count is not an int"),
    }
}

#[test]
fn create_insert_close_reopen_preserves_committed_rows() {
    let dir = fresh_dir("basic");
    {
        let mut db = Engine::<std::fs::File>::create_db(&dir).unwrap();
        db.execute("CREATE TABLE t (id INTEGER NOT NULL, v TEXT)")
            .unwrap();
        db.execute("INSERT INTO t VALUES (1, 'alpha'), (2, 'beta')")
            .unwrap();
        db.close().unwrap();
    }
    let mut db = Engine::<std::fs::File>::open_db(&dir).unwrap();
    assert_eq!(count(&mut db, "t"), 2);
    let all = rows(&mut db, "SELECT * FROM t ORDER BY id");
    assert_eq!(
        all,
        vec![
            vec![SqlValue::Int(1), SqlValue::Text("alpha".into())],
            vec![SqlValue::Int(2), SqlValue::Text("beta".into())],
        ]
    );
    db.close().unwrap();
}

#[test]
fn multi_table_data_survives_restarts() {
    let dir = fresh_dir("multi");
    {
        let mut db = Engine::<std::fs::File>::create_db(&dir).unwrap();
        db.execute("CREATE TABLE customers (id INTEGER NOT NULL, name TEXT)")
            .unwrap();
        db.execute(
            "CREATE TABLE orders (id INTEGER NOT NULL, cid INTEGER NOT NULL, amount INTEGER)",
        )
        .unwrap();
        db.execute("INSERT INTO customers VALUES (1, 'dawn')")
            .unwrap();
        db.execute("INSERT INTO orders VALUES (10, 1, 500)")
            .unwrap();
        db.close().unwrap();
    }
    {
        let mut db = Engine::<std::fs::File>::open_db(&dir).unwrap();
        assert_eq!(count(&mut db, "customers"), 1);
        assert_eq!(count(&mut db, "orders"), 1);
        db.execute("INSERT INTO orders VALUES (11, 1, 999)")
            .unwrap();
        db.close().unwrap();
    }
    let mut db = Engine::<std::fs::File>::open_db(&dir).unwrap();
    assert_eq!(count(&mut db, "customers"), 1);
    assert_eq!(count(&mut db, "orders"), 2);
    db.close().unwrap();
}

#[test]
fn repeated_restarts_accumulate_committed_data() {
    let dir = fresh_dir("repeated");
    let mut db = Engine::<std::fs::File>::create_db(&dir).unwrap();
    db.execute("CREATE TABLE t (id INTEGER NOT NULL, v TEXT)")
        .unwrap();
    db.close().unwrap();
    for i in 0..3u64 {
        let mut db = Engine::<std::fs::File>::open_db(&dir).unwrap();
        db.execute(&format!("INSERT INTO t VALUES ({}, 'r{i}')", i + 1))
            .unwrap();
        db.close().unwrap();
        let mut check = Engine::<std::fs::File>::open_db(&dir).unwrap();
        assert_eq!(count(&mut check, "t"), (i + 1) as i64);
        check.close().unwrap();
    }
}

#[test]
fn catalog_survives_restart_show_tables() {
    let dir = fresh_dir("catalog");
    {
        let mut db = Engine::<std::fs::File>::create_db(&dir).unwrap();
        db.execute("CREATE TABLE alpha (id INTEGER NOT NULL)")
            .unwrap();
        db.execute("CREATE TABLE beta (v TEXT)").unwrap();
        db.close().unwrap();
    }
    let mut db = Engine::<std::fs::File>::open_db(&dir).unwrap();
    let tables: Vec<String> = rows(&mut db, "SHOW TABLES")
        .into_iter()
        .map(|r| match &r[0] {
            SqlValue::Text(t) => t.clone(),
            _ => panic!("table name not text"),
        })
        .collect();
    assert_eq!(tables, vec!["alpha".to_string(), "beta".to_string()]);
    db.close().unwrap();
}

#[test]
fn index_survives_restart_and_serves_lookups() {
    let dir = fresh_dir("index");
    {
        let mut db = Engine::<std::fs::File>::create_db(&dir).unwrap();
        db.execute("CREATE TABLE people (id INTEGER NOT NULL, name TEXT)")
            .unwrap();
        db.execute("INSERT INTO people VALUES (1, 'amy'), (2, 'ben'), (3, 'amy')")
            .unwrap();
        db.execute("CREATE INDEX idx_people_name ON people (name)")
            .unwrap();
        db.close().unwrap();
    }
    // Index was backfilled from durable rows; reopens must reproduce it.
    let mut db = Engine::<std::fs::File>::open_db(&dir).unwrap();
    let hits = rows(&mut db, "SELECT * FROM people WHERE name = 'amy'");
    assert_eq!(hits.len(), 2, "index lookup after restart");
    db.execute("INSERT INTO people VALUES (4, 'amy'), (5, 'ced')")
        .unwrap();
    db.close().unwrap();

    let mut db = Engine::<std::fs::File>::open_db(&dir).unwrap();
    let after = rows(&mut db, "SELECT * FROM people WHERE name = 'amy'");
    assert_eq!(
        after.len(),
        3,
        "index stays consistent across more restarts"
    );
    assert_eq!(count(&mut db, "people"), 5);
    db.close().unwrap();
}

#[test]
fn open_fails_when_metadata_is_missing() {
    let dir = fresh_dir("nodb");
    std::fs::create_dir_all(&dir).unwrap();
    assert!(
        Engine::<std::fs::File>::open_db(&dir).is_err(),
        "opening a directory without db.meta must fail"
    );
}

#[test]
fn create_fails_when_database_already_exists() {
    let dir = fresh_dir("exists");
    Engine::<std::fs::File>::create_db(&dir).unwrap();
    assert!(
        Engine::<std::fs::File>::create_db(&dir).is_err(),
        "double create_db must fail on the existing meta"
    );
}

#[test]
fn empty_database_opens_cleanly() {
    let dir = fresh_dir("empty");
    let db = Engine::<std::fs::File>::create_db(&dir).unwrap();
    db.close().unwrap();
    let mut db = Engine::<std::fs::File>::open_db(&dir).unwrap();
    assert_eq!(rows(&mut db, "SHOW TABLES").len(), 0);
    db.close().unwrap();
}

#[test]
fn torn_wal_tail_is_truncated_and_committed_prefix_kept() {
    let dir = fresh_dir("torn");
    {
        let mut db = Engine::<std::fs::File>::create_db(&dir).unwrap();
        db.execute("CREATE TABLE t (id INTEGER NOT NULL, v TEXT)")
            .unwrap();
        db.execute("INSERT INTO t VALUES (1, 'kept'), (2, 'kept')")
            .unwrap();
        db.close().unwrap();
    }
    // Simulate a crash half-way through a commit-group frame: a valid frame
    // header with a payload that never arrived.
    let mut tmp = Vec::new();
    {
        let mut w = WalWriter::new(&mut tmp);
        w.append(&WalRecord::Begin { txn: 9999 });
        w.commit_group().unwrap();
    }
    let fragment = &tmp[..tmp.len() - 3]; // valid header + partial payload
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(open_wal(&dir))
        .unwrap();
    f.write_all(fragment).unwrap();
    f.sync_all().unwrap();
    drop(f);

    // Open must truncate the torn tail and keep the committed prefix.
    let mut db = Engine::<std::fs::File>::open_db(&dir).unwrap();
    assert_eq!(count(&mut db, "t"), 2);
    db.execute("INSERT INTO t VALUES (3, 'after')").unwrap();
    db.close().unwrap();

    // After truncation + append, everything still replays cleanly.
    let mut db = Engine::<std::fs::File>::open_db(&dir).unwrap();
    assert_eq!(count(&mut db, "t"), 3);
    db.close().unwrap();

    // And the WAL tail is clean (no residual fragment) on disk.
    let bytes = std::fs::read(open_wal(&dir)).unwrap();
    let replay = WalReader::replay(std::io::Cursor::new(bytes)).unwrap();
    assert!(
        !replay.torn_tail,
        "torn tail must have been truncated on open"
    );
}

#[test]
fn interior_frame_corruption_fails_open_loudly() {
    let dir = fresh_dir("corrupt");
    {
        let mut db = Engine::<std::fs::File>::create_db(&dir).unwrap();
        db.execute("CREATE TABLE t (id INTEGER NOT NULL, v TEXT)")
            .unwrap();
        db.execute("INSERT INTO t VALUES (1, 'alpha'), (2, 'beta')")
            .unwrap();
        db.close().unwrap();
    }
    // Flip the first payload byte of the first frame (the Begin tag): the
    // frame is fully present but its payload now decodes as garbage, which is
    // corruption, not a tear -- open must fail loudly instead of truncating.
    let mut bytes = std::fs::read(open_wal(&dir)).unwrap();
    bytes[8] ^= 0xFF;
    std::fs::write(open_wal(&dir), &bytes).unwrap();

    let err = match Engine::<std::fs::File>::open_db(&dir) {
        Ok(_) => panic!("corrupt WAL must fail open"),
        Err(e) => e,
    };
    let msg = err.to_string();
    assert!(
        msg.contains("corrupt WAL") || msg.contains("unknown tag"),
        "open must report WAL corruption loudly, got: {msg}"
    );
}

#[test]
fn unsupported_format_version_fails_open() {
    let dir = fresh_dir("version");
    {
        let mut db = Engine::<std::fs::File>::create_db(&dir).unwrap();
        db.execute("CREATE TABLE t (id INTEGER NOT NULL)").unwrap();
        db.close().unwrap();
    }
    // db.meta layout: magic(8) meta(4) db(4) format(4) wal(4) catalog(4).
    let path = dir.join("db.meta");
    let mut bytes = std::fs::read(&path).unwrap();
    let wal_version = u32::from_le_bytes(bytes[20..24].try_into().unwrap());
    assert_eq!(wal_version, 1);
    bytes[20..24].copy_from_slice(&2u32.to_le_bytes());
    std::fs::write(&path, &bytes).unwrap();

    let err = match Engine::<std::fs::File>::open_db(&dir) {
        Ok(_) => panic!("unsupported version must refuse to open"),
        Err(e) => e,
    };
    let msg = err.to_string();
    assert!(
        msg.contains("unsupported WAL format"),
        "version mismatch must refuse to open, got: {msg}"
    );
}

#[test]
fn foreign_meta_magic_fails_open() {
    let dir = fresh_dir("notqmind");
    std::fs::create_dir_all(dir.join("wal")).unwrap();
    std::fs::write(dir.join("db.meta"), b"NOTAQMINDDB0123456789abcdef").unwrap();
    assert!(
        Engine::<std::fs::File>::open_db(&dir).is_err(),
        "a file with foreign magic must not be opened"
    );
}
