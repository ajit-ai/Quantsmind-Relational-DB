//! # qmind-embed
//!
//! Host-facing JSON API over the SQL engine. This is the contract the
//! desktop GUI (Tauri commands) and any FFI/embedding consumer calls.
//! One `Database` owns its engine + WAL sink; all methods return plain
//! JSON strings to stay FFI-friendly.

use qmind_sql::Engine;
use std::io::Write;
use std::path::Path;

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

impl Database<std::fs::File> {
    /// Create a fresh durable database directory (R2.12): versioned metadata,
    /// canonical layout, empty WAL. Errors surface as strings for FFI.
    pub fn create(dir: impl AsRef<Path>) -> Result<Self, String> {
        let engine = Engine::<std::fs::File>::create_db(dir).map_err(|e| e.to_string())?;
        Ok(Self { engine })
    }

    /// Open an existing database and run startup recovery (R2.5): replay the
    /// WAL, apply the durable catalog, redo committed transactions, rebuild
    /// indexes. Committed data is present on return; in-flight writes are gone.
    pub fn open(dir: impl AsRef<Path>) -> Result<Self, String> {
        let engine = Engine::<std::fs::File>::open_db(dir).map_err(|e| e.to_string())?;
        Ok(Self { engine })
    }

    /// Clean shutdown: flush/sync pending WAL groups and release files.
    /// Crash recovery never depends on this being called.
    pub fn close(self) -> Result<(), String> {
        self.engine.close().map_err(|e| e.to_string())
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
