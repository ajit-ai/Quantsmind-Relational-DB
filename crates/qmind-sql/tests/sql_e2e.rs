//! M3 end-to-end: SQL text → parse → MVCC commit → WAL → SELECT results.

use qmind_sql::{Engine, SqlValue};

#[test]
fn create_insert_select_roundtrip() {
    let mut eng = Engine::new(Vec::new());
    eng.execute("CREATE TABLE users (id INTEGER NOT NULL, name TEXT, city TEXT)")
        .unwrap();

    for (id, name, city) in [
        (1, "Ada", "London"),
        (2, "Grace", "New York"),
        (3, "Edsger", "Austin"),
    ] {
        eng.execute(&format!(
            "INSERT INTO users VALUES ({id}, '{name}', '{city}')"
        ))
        .unwrap();
    }

    let r = eng.execute("SELECT * FROM users").unwrap();
    assert_eq!(r.columns, vec!["id", "name", "city"]);
    assert_eq!(r.rows.len(), 3);
    assert_eq!(r.rows[0][1], SqlValue::Text("Ada".into()));
    assert_eq!(r.rows[2][0], SqlValue::Int(3));
}

#[test]
fn where_filters_and_projection() {
    let mut eng = Engine::new(Vec::new());
    eng.execute("CREATE TABLE t (a INTEGER, b TEXT)").unwrap();
    for i in 0..20 {
        eng.execute(&format!("INSERT INTO t VALUES ({i}, 'row{i}')"))
            .unwrap();
    }

    let r = eng
        .execute("SELECT b FROM t WHERE a >= 15 AND a < 18")
        .unwrap();
    assert_eq!(r.columns, vec!["b"]);
    assert_eq!(
        r.rows,
        vec![
            vec![SqlValue::Text("row15".into())],
            vec![SqlValue::Text("row16".into())],
            vec![SqlValue::Text("row17".into())],
        ]
    );

    let r = eng.execute("SELECT a FROM t WHERE b = 'row7'").unwrap();
    assert_eq!(r.rows, vec![vec![SqlValue::Int(7)]]);
}

#[test]
fn limit_and_not_null_enforcement() {
    let mut eng = Engine::new(Vec::new());
    eng.execute("CREATE TABLE n (k INTEGER NOT NULL)").unwrap();
    assert!(eng.execute("INSERT INTO n VALUES (NULL)").is_err());
    eng.execute("INSERT INTO n VALUES (1), (2), (3), (4)")
        .unwrap();
    let r = eng.execute("SELECT * FROM n LIMIT 2").unwrap();
    assert_eq!(r.rows.len(), 2);
}

#[test]
fn type_mismatch_rejected() {
    let mut eng = Engine::new(Vec::new());
    eng.execute("CREATE TABLE m (x INTEGER)").unwrap();
    assert!(eng.execute("INSERT INTO m VALUES ('oops')").is_err());
    assert!(eng.execute("INSERT INTO m VALUES (5)").is_ok());
}

#[test]
fn wal_log_contains_every_committed_row() {
    use qmind_kernel::{WalReader, WalRecord};
    use std::io::Cursor;

    let mut log = Vec::new();
    {
        let mut eng = Engine::new(&mut log);
        eng.execute("CREATE TABLE w (v INTEGER)").unwrap();
        eng.execute("INSERT INTO w VALUES (10), (20)").unwrap();
    }
    let replay = WalReader::replay(Cursor::new(log)).unwrap();
    let puts: Vec<_> = replay
        .records
        .iter()
        .filter_map(|(_, r)| match r {
            WalRecord::Put { key, value, .. } => Some((key.clone(), value.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(puts.len(), 2);
}

#[test]
fn errors_are_clean_strings() {
    let mut eng = Engine::new(Vec::new());
    assert!(eng
        .execute("SELEKT nonsense")
        .unwrap_err()
        .contains("syntax error"));
    assert!(eng
        .execute("SELECT * FROM missing")
        .unwrap_err()
        .contains("no table"));
}
