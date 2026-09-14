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
        .contains("unsupported statement"));
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

// ── P4: expression engine — richer predicates ──────────────────────────────

#[test]
fn arithmetic_expression_predicates_and_projection() {
    let mut eng = Engine::new(Vec::new());
    eng.execute("CREATE TABLE t (a INTEGER, b INTEGER)")
        .unwrap();
    for i in 0..10i64 {
        eng.execute(&format!("INSERT INTO t VALUES ({i}, {})", i * 3))
            .unwrap();
    }

    // Column-to-column comparison.
    let r = eng.execute("SELECT a FROM t WHERE b = a * 3").unwrap();
    assert_eq!(r.rows.len(), 10);

    // Arithmetic + modulo in predicates and projections.
    let r = eng
        .execute("SELECT a + 1, a * 2 FROM t WHERE a % 2 = 0 ORDER BY a LIMIT 3")
        .unwrap();
    assert_eq!(r.columns, vec!["(a + 1)", "(a * 2)"]);
    assert_eq!(
        r.rows,
        vec![
            vec![SqlValue::Int(1), SqlValue::Int(0)],
            vec![SqlValue::Int(3), SqlValue::Int(4)],
            vec![SqlValue::Int(5), SqlValue::Int(8)],
        ]
    );

    // Division by zero is an error, not a silent result.
    assert!(eng.execute("SELECT a / 0 FROM t").is_err());
}

#[test]
fn or_not_and_null_three_valued_logic() {
    let mut eng = Engine::new(Vec::new());
    eng.execute("CREATE TABLE t (id INTEGER, name TEXT)")
        .unwrap();
    eng.execute("INSERT INTO t VALUES (1, 'Ada'), (2, 'Grace'), (3, NULL), (4, 'Linus')")
        .unwrap();

    // OR + parentheses.
    let r = eng
        .execute("SELECT id FROM t WHERE name = 'Ada' OR (id = 2 AND name = 'Grace')")
        .unwrap();
    assert_eq!(r.rows, vec![vec![SqlValue::Int(1)], vec![SqlValue::Int(2)]]);

    // NOT wraps the whole comparison.
    let r = eng.execute("SELECT id FROM t WHERE NOT id = 4").unwrap();
    assert_eq!(
        r.rows,
        vec![
            vec![SqlValue::Int(1)],
            vec![SqlValue::Int(2)],
            vec![SqlValue::Int(3)]
        ]
    );

    // NULL comparisons never match.
    assert_eq!(
        eng.execute("SELECT id FROM t WHERE name = NULL")
            .unwrap()
            .rows
            .len(),
        0
    );
    assert_eq!(
        eng.execute("SELECT id FROM t WHERE name != NULL")
            .unwrap()
            .rows
            .len(),
        0
    );
    // NULL only matches IS-agnostic * ? (no: NULL = NULL is unknown too).
    assert_eq!(
        eng.execute("SELECT id FROM t WHERE id = NULL")
            .unwrap()
            .rows
            .len(),
        0
    );

    // Scalar functions.
    let r = eng
        .execute("SELECT UPPER(name), LENGTH(name) FROM t WHERE id = 1")
        .unwrap();
    assert_eq!(
        r.rows,
        vec![vec![SqlValue::Text("ADA".into()), SqlValue::Int(3)]]
    );
}

#[test]
fn like_in_between_predicates() {
    let mut eng = Engine::new(Vec::new());
    eng.execute("CREATE TABLE t (name TEXT, score INTEGER)")
        .unwrap();
    let names = ["Alice", "Bob", "Carol", "alice", "Dan", "Dave"];
    for (i, n) in names.iter().enumerate() {
        eng.execute(&format!(
            "INSERT INTO t VALUES ('{n}', {})",
            (i as i64) * 10
        ))
        .unwrap();
    }

    let r = eng
        .execute("SELECT name FROM t WHERE name LIKE 'A%' ORDER BY name")
        .unwrap();
    assert_eq!(r.rows, vec![vec![SqlValue::Text("Alice".into())]]);
    // LIKE is case-sensitive.
    assert_eq!(
        eng.execute("SELECT name FROM t WHERE name LIKE 'a%'")
            .unwrap()
            .rows
            .len(),
        1
    );

    // IN list.
    let r = eng
        .execute("SELECT name FROM t WHERE name IN ('Bob', 'Dave') ORDER BY name")
        .unwrap();
    assert_eq!(
        r.rows,
        vec![
            vec![SqlValue::Text("Bob".into())],
            vec![SqlValue::Text("Dave".into())]
        ]
    );

    let r = eng.execute("SELECT name FROM t WHERE name NOT IN ('Bob', 'Dave') AND name IN ('Alice', 'Carol') ORDER BY name").unwrap();
    assert_eq!(
        r.rows,
        vec![
            vec![SqlValue::Text("Alice".into())],
            vec![SqlValue::Text("Carol".into())]
        ]
    );

    // BETWEEN is inclusive.
    let r = eng
        .execute("SELECT name FROM t WHERE score BETWEEN 20 AND 40")
        .unwrap();
    assert_eq!(
        r.rows,
        vec![
            vec![SqlValue::Text("Carol".into())],
            vec![SqlValue::Text("alice".into())],
            vec![SqlValue::Text("Dan".into())],
        ]
    );
}

// ── P4: ORDER BY / sort ────────────────────────────────────────────────────

#[test]
fn order_by_asc_desc_multiple_keys_and_null_last() {
    let mut eng = Engine::new(Vec::new());
    eng.execute("CREATE TABLE t (k INTEGER, v TEXT)").unwrap();
    eng.execute("INSERT INTO t VALUES (2, 'b'), (1, 'a'), (3, 'c'), (2, 'a'), (NULL, 'n')")
        .unwrap();

    let r = eng.execute("SELECT k FROM t ORDER BY k").unwrap();
    assert_eq!(
        r.rows,
        vec![
            vec![SqlValue::Int(1)],
            vec![SqlValue::Int(2)],
            vec![SqlValue::Int(2)],
            vec![SqlValue::Int(3)],
            vec![SqlValue::Null],
        ]
    );

    let r = eng.execute("SELECT k FROM t ORDER BY k DESC").unwrap();
    assert_eq!(r.rows[0], vec![SqlValue::Null]);
    assert_eq!(r.rows[1], vec![SqlValue::Int(3)]);

    // Multiple keys: k DESC, then v ASC within ties (stable).
    let r = eng
        .execute("SELECT k, v FROM t WHERE k < 100 ORDER BY k DESC, v")
        .unwrap();
    assert_eq!(
        r.rows,
        vec![
            vec![SqlValue::Int(3), SqlValue::Text("c".into())],
            vec![SqlValue::Int(2), SqlValue::Text("a".into())],
            vec![SqlValue::Int(2), SqlValue::Text("b".into())],
            vec![SqlValue::Int(1), SqlValue::Text("a".into())],
        ]
    );

    // ORDER BY over an expression.
    let r = eng.execute("SELECT k FROM t ORDER BY k * -1").unwrap();
    assert_eq!(r.rows[0], vec![SqlValue::Int(3)]);
}

#[test]
fn order_by_with_limit_and_on_joins_and_group_by() {
    let mut eng = Engine::new(Vec::new());
    eng.execute("CREATE TABLE a (id INTEGER, val TEXT)")
        .unwrap();
    eng.execute("CREATE TABLE b (cid INTEGER, amt INTEGER)")
        .unwrap();
    eng.execute("INSERT INTO a VALUES (1, 'one'), (2, 'two'), (3, 'three')")
        .unwrap();
    eng.execute("INSERT INTO b VALUES (1, 100), (2, 50), (3, 200)")
        .unwrap();

    let r = eng
        .execute("SELECT val FROM a INNER JOIN b ON id = cid ORDER BY amt")
        .unwrap();
    assert_eq!(
        r.rows,
        vec![
            vec![SqlValue::Text("two".into())],
            vec![SqlValue::Text("one".into())],
            vec![SqlValue::Text("three".into())],
        ]
    );

    let r = eng
        .execute("SELECT val FROM a ORDER BY val DESC LIMIT 2")
        .unwrap();
    assert_eq!(
        r.rows,
        vec![
            vec![SqlValue::Text("two".into())],
            vec![SqlValue::Text("three".into())]
        ]
    );

    let r = eng
        .execute("SELECT val, COUNT(*) FROM a GROUP BY val ORDER BY COUNT(*) DESC, val")
        .unwrap();
    assert_eq!(r.rows.len(), 3);
    assert_eq!(r.columns, vec!["val", "COUNT(*)"]);
}

#[test]
fn order_by_pulls_from_columnar_path() {
    let dir = std::env::temp_dir().join("qmind_e2e_order_by");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let mut eng = Engine::new(Vec::new()).with_columnar(dir.clone());
    eng.execute("CREATE TABLE t (id INTEGER NOT NULL, v TEXT)")
        .unwrap();
    for i in 0..5i64 {
        eng.execute(&format!("INSERT INTO t VALUES ({i}, 'v{}')", 4 - i))
            .unwrap();
    }
    eng.flush_to_columnar().unwrap();

    let r = eng.execute("SELECT id FROM t ORDER BY id DESC").unwrap();
    assert_eq!(
        r.rows,
        vec![
            vec![SqlValue::Int(4)],
            vec![SqlValue::Int(3)],
            vec![SqlValue::Int(2)],
            vec![SqlValue::Int(1)],
            vec![SqlValue::Int(0)],
        ]
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// ── M9: Columnar HTAP integration ──────────────────────────────────────────

#[test]
fn columnar_htap_insert_flush_select() {
    let dir = std::env::temp_dir().join("qmind_e2e_columnar");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let mut eng = Engine::new(Vec::new()).with_columnar(dir.clone());

    eng.execute("CREATE TABLE products (id INTEGER NOT NULL, name TEXT, price INTEGER)")
        .unwrap();

    // Insert 100 rows.
    for i in 0..100i64 {
        eng.execute(&format!(
            "INSERT INTO products VALUES ({i}, 'item_{i}', {})",
            i * 10
        ))
        .unwrap();
    }

    // Data is in delta buffer, not yet flushed.
    assert!(!eng.has_columnar_data("products"));

    // Flush to columnar.
    let counts = eng.flush_to_columnar().unwrap();
    assert_eq!(counts.get("products"), Some(&100));

    // Now columnar data exists.
    assert!(eng.has_columnar_data("products"));

    // SELECT * from columnar.
    let r = eng.execute("SELECT * FROM products").unwrap();
    assert_eq!(r.rows.len(), 100);
    assert_eq!(r.columns, vec!["id", "name", "price"]);

    // Verify first row.
    assert_eq!(r.rows[0][0], SqlValue::Int(0));
    assert_eq!(r.rows[0][1], SqlValue::Text("item_0".into()));
    assert_eq!(r.rows[0][2], SqlValue::Int(0));

    // Verify last row.
    assert_eq!(r.rows[99][0], SqlValue::Int(99));
    assert_eq!(r.rows[99][1], SqlValue::Text("item_99".into()));
    assert_eq!(r.rows[99][2], SqlValue::Int(990));

    // SELECT with WHERE.
    let r = eng
        .execute("SELECT name, price FROM products WHERE id = 42")
        .unwrap();
    assert_eq!(r.rows.len(), 1);
    assert_eq!(r.rows[0][0], SqlValue::Text("item_42".into()));
    assert_eq!(r.rows[0][1], SqlValue::Int(420));

    // SELECT with LIMIT.
    let r = eng.execute("SELECT * FROM products LIMIT 5").unwrap();
    assert_eq!(r.rows.len(), 5);

    // SELECT with projection.
    let r = eng.execute("SELECT price FROM products").unwrap();
    assert_eq!(r.columns, vec!["price"]);
    assert_eq!(r.rows.len(), 100);
    assert_eq!(r.rows[0], vec![SqlValue::Int(0)]);
    assert_eq!(r.rows[5], vec![SqlValue::Int(50)]);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn columnar_htap_no_columnar_falls_back_to_mvcc() {
    let mut eng = Engine::new(Vec::new());

    eng.execute("CREATE TABLE t (id INTEGER NOT NULL, val TEXT)")
        .unwrap();
    eng.execute("INSERT INTO t VALUES (1, 'hello')").unwrap();
    eng.execute("INSERT INTO t VALUES (2, 'world')").unwrap();

    // Without columnar enabled, SELECT reads from MVCC (row store).
    let r = eng.execute("SELECT * FROM t").unwrap();
    assert_eq!(r.rows.len(), 2);
    assert_eq!(r.rows[0][0], SqlValue::Int(1));
}

#[test]
fn columnar_htap_multiple_flushes_concatenate() {
    let dir = std::env::temp_dir().join("qmind_e2e_multi_flush");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let mut eng = Engine::new(Vec::new()).with_columnar(dir.clone());

    eng.execute("CREATE TABLE logs (ts INTEGER NOT NULL, msg TEXT)")
        .unwrap();

    // First batch: 3 rows.
    for i in 0..3i64 {
        eng.execute(&format!("INSERT INTO logs VALUES ({i}, 'log_{i}')"))
            .unwrap();
    }
    eng.flush_to_columnar().unwrap();

    // Second batch: 2 rows.
    for i in 3..5i64 {
        eng.execute(&format!("INSERT INTO logs VALUES ({i}, 'log_{i}')"))
            .unwrap();
    }
    eng.flush_to_columnar().unwrap();

    // Both segments concatenated.
    let r = eng.execute("SELECT * FROM logs").unwrap();
    assert_eq!(r.rows.len(), 5);

    let _ = std::fs::remove_dir_all(&dir);
}

// ── P4: Secondary indexes ─────────────────────────────────────────────────

#[test]
fn create_index_backfill_and_equality_lookup() {
    let mut eng = Engine::new(Vec::new());
    eng.execute("CREATE TABLE accounts (id INTEGER NOT NULL, city TEXT)")
        .unwrap();

    // Seed rows, then build the index (backfill over existing data).
    for (id, city) in [(1i64, "London"), (2, "Paris"), (3, "Tokyo"), (4, "London")] {
        eng.execute(&format!("INSERT INTO accounts VALUES ({id}, '{city}')"))
            .unwrap();
    }
    eng.execute("CREATE INDEX idx_city ON accounts (city)")
        .unwrap();

    // Point query on the indexed column.
    let r = eng
        .execute("SELECT id FROM accounts WHERE city = 'London'")
        .unwrap();
    let mut ids: Vec<_> = r.rows.into_iter().map(|row| row[0].clone()).collect();
    ids.sort();
    assert_eq!(ids, vec![SqlValue::Int(1), SqlValue::Int(4)]);

    // Indexed INTEGER equality.
    let r = eng.execute("SELECT id FROM accounts WHERE id = 3").unwrap();
    assert_eq!(r.rows, vec![vec![SqlValue::Int(3)]]);
}

#[test]
fn index_maintained_on_insert_and_null_skipped() {
    let mut eng = Engine::new(Vec::new());
    eng.execute("CREATE TABLE events (eid INTEGER NOT NULL, host TEXT)")
        .unwrap();
    eng.execute("INSERT INTO events VALUES (1, 'alpha')")
        .unwrap();
    eng.execute("CREATE INDEX idx_host ON events (host)")
        .unwrap();

    // New inserts must appear in index-driven lookups.
    eng.execute("INSERT INTO events VALUES (2, 'beta'), (3, 'alpha')")
        .unwrap();

    let r = eng
        .execute("SELECT eid FROM events WHERE host = 'alpha'")
        .unwrap();
    let mut ids: Vec<_> = r.rows.into_iter().map(|row| row[0].clone()).collect();
    ids.sort();
    assert_eq!(ids, vec![SqlValue::Int(1), SqlValue::Int(3)]);

    // NULL indexed values are skipped: the NULL row exists but never
    // participates in index lookups (and `host = NULL` is NULL, so no rows).
    eng.execute("INSERT INTO events VALUES (4, NULL)").unwrap();
    assert_eq!(eng.execute("SELECT * FROM events").unwrap().rows.len(), 4);
    let r = eng
        .execute("SELECT eid FROM events WHERE host = 'alpha'")
        .unwrap();
    assert_eq!(r.rows.len(), 2);
    assert!(eng
        .execute("SELECT eid FROM events WHERE host = NULL")
        .unwrap()
        .rows
        .is_empty());
}

#[test]
fn index_equality_plus_residual_predicate() {
    let mut eng = Engine::new(Vec::new());
    eng.execute("CREATE TABLE orders (oid INTEGER NOT NULL, cust TEXT)")
        .unwrap();
    for (oid, cust) in [(1i64, "ada"), (2, "bob"), (3, "ada"), (4, "ada")] {
        eng.execute(&format!("INSERT INTO orders VALUES ({oid}, '{cust}')"))
            .unwrap();
    }
    eng.execute("CREATE INDEX idx_cust ON orders (cust)")
        .unwrap();

    // Index serves `cust = 'ada'`; the remaining conjunct must still filter.
    let r = eng
        .execute("SELECT oid FROM orders WHERE cust = 'ada' AND oid > 1")
        .unwrap();
    let mut ids: Vec<_> = r.rows.into_iter().map(|row| row[0].clone()).collect();
    ids.sort();
    assert_eq!(ids, vec![SqlValue::Int(3), SqlValue::Int(4)]);

    // Range predicates don't use the index but must still match the same rows.
    let r = eng
        .execute("SELECT oid FROM orders WHERE cust >= 'ada' AND cust <= 'ada'")
        .unwrap();
    let mut ids: Vec<_> = r.rows.into_iter().map(|row| row[0].clone()).collect();
    ids.sort();
    assert_eq!(
        ids,
        vec![SqlValue::Int(1), SqlValue::Int(3), SqlValue::Int(4)]
    );
}

#[test]
fn create_index_errors_and_drop_index() {
    let mut eng = Engine::new(Vec::new());
    eng.execute("CREATE TABLE a (x INTEGER)").unwrap();
    eng.execute("CREATE TABLE b (y TEXT)").unwrap();

    assert!(eng
        .execute("CREATE INDEX i ON missing (x)")
        .unwrap_err()
        .contains("no table"));

    assert!(eng
        .execute("CREATE INDEX i ON a (x, y)")
        .unwrap_err()
        .contains("single-column"));

    eng.execute("CREATE INDEX i ON a (x)").unwrap();
    assert!(eng
        .execute("CREATE INDEX i ON a (x)")
        .unwrap_err()
        .contains("already exists"));

    // Drop removes the index (subsequent DROP errors).
    eng.execute("DROP INDEX i").unwrap();
    assert!(eng
        .execute("DROP INDEX i")
        .unwrap_err()
        .contains("no index"));

    // Old error-message stubs are gone: CREATE/DROP are real features now.
    assert!(eng.execute("CREATE INDEX j ON a (x)").is_ok());
    eng.execute("DROP INDEX j").unwrap();
}
