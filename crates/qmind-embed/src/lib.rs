//! # qmind-embed
//!
//! Host-facing JSON API over the SQL engine. This is the contract the
//! desktop GUI (Tauri commands) and any FFI/embedding consumer calls.
//! One `Database` owns its engine + WAL sink; all methods return plain
//! JSON strings to stay FFI-friendly.

use qmind_sql::Engine;
use std::io::Write;

pub struct Database<W: Write> {
    engine: Engine<W>,
}

impl<W: Write> Database<W> {
    pub fn new(wal_sink: W) -> Self {
        Self {
            engine: Engine::new(wal_sink),
        }
    }

    /// Execute one SQL statement.
    ///
    /// Success payload:
    /// `{"ok":true,"columns":[..],"rows":[[cell,..],..],"rowsAffected":n}`
    /// cells serialize as strings; NULL becomes JSON null.
    /// Failure payload: `{"ok":false,"error":"..."}`
    pub fn execute(&mut self, sql: &str) -> String {
        match self.engine.execute(sql) {
            Ok(res) => {
                let rows: Vec<Vec<serde_json::Value>> = res
                    .rows
                    .iter()
                    .map(|r| r.iter().map(cell_json).collect())
                    .collect();
                serde_json::json!({
                    "ok": true,
                    "columns": res.columns,
                    "rows": rows,
                    "rowsAffected": res.rows_affected,
                })
                .to_string()
            }
            Err(e) => serde_json::json!({ "ok": false, "error": e }).to_string(),
        }
    }

    pub fn engine_version() -> &'static str {
        qmind_sql::ENGINE.version
    }
}

fn cell_json(v: &qmind_sql::SqlValue) -> serde_json::Value {
    use qmind_sql::SqlValue::*;
    match v {
        Null => serde_json::Value::Null,
        Int(i) => serde_json::json!(i),
        Text(t) => serde_json::json!(t),
    }
}
