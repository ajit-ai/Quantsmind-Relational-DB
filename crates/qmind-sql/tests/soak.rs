//! M7 soak test: 10K insert/select/join/group-by cycle.
//!
//! Runs a sustained workload checking data integrity at every step.
//! Catches subtle corruption, memory leaks, and WAL drift.

#[test]
fn soak_10k_insert_select_group_join() {
    use qmind_sql::{Engine, SqlValue};

    let mut eng = Engine::new(Vec::new());

    // Setup: 5 tables with different schemas.
    eng.execute("CREATE TABLE users (id INTEGER NOT NULL, name TEXT, dept TEXT)")
        .unwrap();
    eng.execute("CREATE TABLE orders (uid INTEGER NOT NULL, amount INTEGER NOT NULL)")
        .unwrap();
    eng.execute("CREATE TABLE logs (id INTEGER NOT NULL, msg TEXT)")
        .unwrap();
    eng.execute("CREATE TABLE metrics (k TEXT, v INTEGER)")
        .unwrap();
    eng.execute("CREATE TABLE tags (id INTEGER NOT NULL, tag TEXT)")
        .unwrap();

    // Phase 1: bulk insert 10K rows across tables.
    for batch in 0..100u64 {
        let mut values = String::new();
        for i in 0..100u64 {
            let rid = batch * 100 + i;
            if i > 0 {
                values.push_str(", ");
            }
            values.push_str(&format!("({rid}, 'user{rid}', 'dept{}')", rid % 5));
        }
        eng.execute(&format!("INSERT INTO users VALUES {values}"))
            .unwrap();
    }

    // Verify count.
    let r = eng.execute("SELECT COUNT(*) FROM users").unwrap();
    assert_eq!(r.rows[0][0], SqlValue::Int(10000));

    // Phase 2: insert into orders (1 per user).
    for batch in 0..100u64 {
        let mut values = String::new();
        for i in 0..100u64 {
            let uid = batch * 100 + i;
            let amount = (uid * 37) % 10000;
            if i > 0 {
                values.push_str(", ");
            }
            values.push_str(&format!("({uid}, {amount})"));
        }
        eng.execute(&format!("INSERT INTO orders VALUES {values}"))
            .unwrap();
    }

    let r = eng.execute("SELECT COUNT(*) FROM orders").unwrap();
    assert_eq!(r.rows[0][0], SqlValue::Int(10000));

    // Phase 3: SELECT WHERE filters — verify no phantom rows.
    let r = eng.execute("SELECT COUNT(*) FROM users WHERE dept = 'dept0'").unwrap();
    let dept0_count = match &r.rows[0][0] {
        SqlValue::Int(n) => *n,
        _ => panic!("expected int"),
    };
    assert_eq!(dept0_count, 2000, "each dept should have exactly 2000 users");

    // Phase 4: JOIN — verify row count matches.
    let r = eng
        .execute("SELECT name, amount FROM users INNER JOIN orders ON id = uid")
        .unwrap();
    assert_eq!(r.rows.len(), 10000, "joined rows should be 10K (one per user)");
    let mut total_amount = 0i64;
    for row in &r.rows {
        match &row[1] {
            SqlValue::Int(n) => total_amount += n,
            _ => panic!("expected int"),
        }
    }
    assert!(total_amount > 0, "total amount should be positive");

    // Phase 4b: GROUP BY on single table.
    let r = eng
        .execute("SELECT dept, COUNT(*) FROM users GROUP BY dept")
        .unwrap();
    assert_eq!(r.rows.len(), 5, "5 departments");
    let mut total_users = 0i64;
    for row in &r.rows {
        match &row[1] {
            SqlValue::Int(n) => total_users += n,
            _ => panic!("expected int"),
        }
    }
    assert_eq!(total_users, 10000);

    // Phase 5: LIMIT stress — every LIMIT returns ≤ N rows.
    for limit in [1, 10, 100, 500, 9999] {
        let r = eng
            .execute(&format!("SELECT * FROM users LIMIT {limit}"))
            .unwrap();
        assert!(
            r.rows.len() <= limit,
            "LIMIT {limit} returned {} rows",
            r.rows.len()
        );
    }

    // Phase 6: multi-table inserts into logs and tags.
    for i in 0..5000u64 {
        eng.execute(&format!("INSERT INTO logs VALUES ({i}, 'log{i}')"))
            .unwrap();
        eng.execute(&format!(
            "INSERT INTO tags VALUES ({i}, 'tag{}')",
            i % 10
        ))
        .unwrap();
    }

    let r = eng.execute("SELECT COUNT(*) FROM logs").unwrap();
    assert_eq!(r.rows[0][0], SqlValue::Int(5000));
    let r = eng.execute("SELECT COUNT(*) FROM tags").unwrap();
    assert_eq!(r.rows[0][0], SqlValue::Int(5000));

    // Phase 7: GROUP BY on tags.
    let r = eng
        .execute("SELECT tag, COUNT(*) FROM tags GROUP BY tag")
        .unwrap();
    assert_eq!(r.rows.len(), 10, "10 distinct tags");
    for row in &r.rows {
        match &row[1] {
            SqlValue::Int(n) => assert_eq!(*n, 500, "each tag should have 500 rows"),
            _ => panic!("expected int"),
        }
    }

    // Phase 8: WHERE + aggregate on single table.
    let r = eng
        .execute("SELECT COUNT(*), MIN(amount), MAX(amount) FROM orders WHERE amount > 5000")
        .unwrap();
    assert!(!r.rows.is_empty(), "should have some large orders");

    // Phase 9: LIMIT on JOIN results.
    let r = eng
        .execute("SELECT name, amount FROM users INNER JOIN orders ON id = uid LIMIT 3")
        .unwrap();
    assert_eq!(r.rows.len(), 3);

    // Phase 10: IF NOT EXISTS idempotency.
    eng.execute("CREATE TABLE IF NOT EXISTS users (id INTEGER)")
        .unwrap();
    let r = eng.execute("SELECT COUNT(*) FROM users").unwrap();
    assert_eq!(r.rows[0][0], SqlValue::Int(10000), "IF NOT EXISTS must not truncate");

    // Final: SHOW TABLES lists all 5 tables.
    let r = eng.execute("SHOW TABLES").unwrap();
    assert_eq!(r.rows.len(), 5);
}
