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

#[test]
fn aggregates_count_sum_avg_min_max_with_filter() {
    let mut eng = Engine::new(Vec::new());
    eng.execute("CREATE TABLE s (v INTEGER, tag TEXT)").unwrap();
    for i in 1..=10 {
        eng.execute(&format!("INSERT INTO s VALUES ({i}, 'g{}')", i % 2))
            .unwrap();
    }

    let r = eng.execute("SELECT COUNT(*) FROM s").unwrap();
    assert_eq!(r.rows[0][0], SqlValue::Int(10));

    let r = eng
        .execute("SELECT SUM(v), AVG(v), MIN(v), MAX(v) FROM s WHERE v <= 4")
        .unwrap();
    assert_eq!(r.rows[0][0], SqlValue::Int(10));
    assert_eq!(r.rows[0][1], SqlValue::Int(2));
    assert_eq!(r.rows[0][2], SqlValue::Int(1));
    assert_eq!(r.rows[0][3], SqlValue::Int(4));

    let r = eng.execute("SELECT MIN(tag) FROM s").unwrap();
    assert_eq!(r.rows[0][0], SqlValue::Text("g0".into()));

    let r = eng.execute("SELECT COUNT(v) FROM s WHERE v > 100").unwrap();
    assert_eq!(r.rows[0][0], SqlValue::Int(0));
}

#[test]
fn group_by_with_filter_and_multiple_aggs() {
    let mut eng = Engine::new(Vec::new());
    eng.execute("CREATE TABLE s (v INTEGER, tag TEXT)").unwrap();
    for i in 1..=8 {
        eng.execute(&format!("INSERT INTO s VALUES ({i}, 'g{}')", i % 2))
            .unwrap();
    }

    let r = eng
        .execute("SELECT tag, COUNT(*), SUM(v) FROM s GROUP BY tag")
        .unwrap();
    assert_eq!(r.columns.len(), 3);
    // BTreeMap order: g0 first, then g1
    assert_eq!(r.rows[0][0], SqlValue::Text("g0".into()));
    assert_eq!(r.rows[0][1], SqlValue::Int(4)); // v in {2,4,6,8}
    assert_eq!(r.rows[0][2], SqlValue::Int(20));
    assert_eq!(r.rows[1][0], SqlValue::Text("g1".into()));
    assert_eq!(r.rows[1][1], SqlValue::Int(4)); // v in {1,3,5,7}
    assert_eq!(r.rows[1][2], SqlValue::Int(16));

    let r = eng
        .execute("SELECT tag, MIN(v), MAX(v) FROM s WHERE v <= 4 GROUP BY tag")
        .unwrap();
    assert_eq!(r.rows[0][1], SqlValue::Int(2));
    assert_eq!(r.rows[0][2], SqlValue::Int(4));
    assert_eq!(r.rows[1][1], SqlValue::Int(1));
    assert_eq!(r.rows[1][2], SqlValue::Int(3));

    // non-grouped bare column rejected
    assert!(eng.execute("SELECT v FROM s GROUP BY tag").is_err());
}

#[test]
fn inner_join_hash_matches_and_filters() {
    let mut eng = Engine::new(Vec::new());
    eng.execute("CREATE TABLE customers (id INTEGER, name TEXT)")
        .unwrap();
    eng.execute("CREATE TABLE orders (cid INTEGER, amount INTEGER)")
        .unwrap();
    eng.execute("INSERT INTO customers VALUES (1, 'Ada'), (2, 'Grace'), (3, 'Lonely')")
        .unwrap();
    eng.execute("INSERT INTO orders VALUES (1, 100), (1, 250), (2, 75), (9, 999)")
        .unwrap();

    let r = eng
        .execute("SELECT name, amount FROM customers INNER JOIN orders ON id = cid")
        .unwrap();
    assert_eq!(r.columns, vec!["name", "amount"]);
    assert_eq!(r.rows.len(), 3); // customer 3 and order cid=9 have no match
    let total: i64 = r
        .rows
        .iter()
        .map(|row| match &row[1] {
            SqlValue::Int(v) => *v,
            _ => 0,
        })
        .sum();
    assert_eq!(total, 425);

    let r = eng
        .execute("SELECT name FROM customers INNER JOIN orders ON id = cid WHERE amount > 200")
        .unwrap();
    assert_eq!(r.rows, vec![vec![SqlValue::Text("Ada".into())]]);

    // TODO(M4d): qualified names (t.col) + ambiguity detection
}

#[test]
fn show_tables_lists_created_tables() {
    let mut eng = Engine::new(Vec::new());
    eng.execute("CREATE TABLE beta (x INTEGER)").unwrap();
    eng.execute("CREATE TABLE alpha (y TEXT)").unwrap();
    let r = eng.execute("SHOW TABLES").unwrap();
    assert_eq!(r.columns, vec!["table"]);
    assert_eq!(
        r.rows,
        vec![
            vec![SqlValue::Text("alpha".into())],
            vec![SqlValue::Text("beta".into())]
        ]
    );
}
