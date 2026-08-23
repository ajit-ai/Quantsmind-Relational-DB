use qmind_embed::Database;

#[test]
fn json_contract_roundtrip() {
    let mut db = Database::new(Vec::new());
    db.execute("CREATE TABLE u (id INTEGER NOT NULL, name TEXT)");
    let out = db.execute("INSERT INTO u VALUES (1, 'Ada'), (2, NULL)");
    assert!(out.contains("\"rowsAffected\":2"), "{out}");

    let out = db.execute("SELECT id, name FROM u ORDER-LATER");
    // unsupported syntax -> structured error, not panic
    if !out.contains("\"ok\":true") {
        assert!(out.contains("\"error\""));
    }

    let out = db.execute("SELECT * FROM u");
    assert!(out.contains("\"columns\":[\"id\",\"name\"]"), "{out}");
    assert!(out.contains("Ada"));
    assert!(out.contains("null"), "{out}");
}

#[test]
fn errors_are_structured_not_panics() {
    let mut db = Database::new(Vec::new());
    let out = db.execute("SELECT * FROM ghost");
    assert_eq!(out.matches("\"ok\":false").count(), 1);
    assert!(out.contains("no table"));
}
