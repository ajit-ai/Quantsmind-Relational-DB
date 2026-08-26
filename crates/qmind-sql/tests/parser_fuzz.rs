//! M7 property-based tests: random SQL → parse → never panics.
//!
//! Generates arbitrary SQL strings and verifies the parser always returns
//! a Result (Ok or Err), never panics. This catches unreachable!, index
//! OOB, and other crash bugs in the tokenizer/parser.

#[cfg(test)]
mod fuzz {
    use qmind_sql::parser::Parser;
    use std::panic;

    /// Generate a random SQL string from the alphabet of tokens we support.
    fn gen_sql(seed: u64) -> String {
        let mut rng = seed;
        let mut sql = String::new();
        let len = (rng % 200) + 1;
        rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);

        let keywords = [
            "SELECT", "*", "FROM", "WHERE", "AND", "INSERT", "INTO", "VALUES",
            "CREATE", "TABLE", "INTEGER", "TEXT", "NOT", "NULL", "LIMIT", "JOIN",
            "INNER", "ON", "GROUP", "BY", "SHOW", "TABLES", "IF", "EXISTS",
            "COUNT", "SUM", "AVG", "MIN", "MAX",
        ];

        for _ in 0..len {
            if !sql.is_empty() {
                sql.push(' ');
            }
            let choice = rng % 30;
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);

            match choice {
                0..=24 => sql.push_str(keywords[choice as usize]),
                25 => sql.push_str(&format!("col{}", rng % 50)),
                26 => sql.push_str(&format!("'str{}'", rng % 100)),
                27 => sql.push_str(&format!("{}", rng % 1000)),
                28 => {
                    sql.push('(');
                    sql.push(')');
                }
                29 => {
                    sql.push(',');
                }
                _ => {}
            }
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
        }
        sql
    }

    /// Fuzz: random SQL strings must never cause a panic.
    #[test]
    fn parser_never_panics_on_random_input() {
        for seed in 0..5000u64 {
            let sql = gen_sql(seed);
            let result = panic::catch_unwind(|| Parser::parse(&sql));
            assert!(
                result.is_ok(),
                "parser panicked on seed {seed}: {sql:?}\n{:?}",
                result.unwrap_err()
            );
        }
    }

    /// Fuzz: complete SQL statements must parse successfully.
    #[test]
    fn well_formed_statements_parse_ok() {
        let valid = [
            "SELECT * FROM t",
            "SELECT a, b FROM t WHERE a = 1",
            "SELECT a FROM t WHERE a = 1 AND b = 'x' LIMIT 5",
            "INSERT INTO t VALUES (1, 'hello')",
            "INSERT INTO t VALUES (1, 'a'), (2, 'b'), (3, 'c')",
            "CREATE TABLE t (id INTEGER NOT NULL, name TEXT)",
            "CREATE TABLE IF NOT EXISTS t (x INT)",
            "SHOW TABLES",
            "SELECT COUNT(*) FROM t",
            "SELECT tag, COUNT(*), SUM(v) FROM s WHERE v <= 4 GROUP BY tag",
            "SELECT name FROM c INNER JOIN o ON id = cid WHERE amount > 200",
            "SELECT MIN(tag) FROM s",
            "SELECT AVG(x), MAX(y) FROM t WHERE z != 0",
        ];
        for sql in &valid {
            let result = Parser::parse(sql);
            assert!(result.is_ok(), "failed to parse {sql:?}: {result:?}");
        }
    }

    /// Fuzz: tokenizer must never panic on any byte sequence.
    #[test]
    fn tokenizer_never_panics() {
        for seed in 0..3000u64 {
            let mut rng = seed;
            let mut input = Vec::new();
            let len = (rng % 100) + 1;
            for _ in 0..len {
                rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
                input.push((rng % 128) as u8);
            }
            let s = String::from_utf8_lossy(&input);
            let result = panic::catch_unwind(|| qmind_sql::parser::tokenize(&s));
            assert!(
                result.is_ok(),
                "tokenizer panicked on seed {seed}: {s:?}"
            );
        }
    }

    /// Fuzz: parser errors are always descriptive strings (never empty).
    #[test]
    fn parser_errors_are_descriptive() {
        let bad = [
            "",
            "   ",
            "SELEKT x",
            "CREATE TABLE",
            "INSERT INTO",
            "SELECT * FROM",
            "SELECT FROM WHERE",
            "12345",
            "SELECT * FROM t WHERE AND",
            "SELECT * FROM t LIMIT -1",
        ];
        for sql in &bad {
            let result = Parser::parse(sql);
            if let Err(e) = result {
                assert!(
                    !e.is_empty(),
                    "empty error message for {sql:?}"
                );
            }
        }
    }
}
