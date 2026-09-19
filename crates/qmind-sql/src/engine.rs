//! M3 SQL engine: parse → plan-lite → execute over the MVCC kernel.
//!
//! Uses the handwritten parser (E5) for SQL surface:
//! - CREATE TABLE t (col TYPE [NOT NULL], ...) [IF NOT EXISTS]
//! - INSERT INTO t VALUES (..), (..)
//! - SELECT cols | * FROM t [INNER JOIN t ON col = col] [WHERE cond]
//!   [GROUP BY col] [LIMIT n]
//!
//! M9: Columnar HTAP integration — reads from columnar segments when available.

use crate::codec::{decode_row, encode_row, row_key, ColumnDef, ColumnType, SqlValue};
use crate::executor::Row;
use crate::executor::{
    Filter, HashAggregate, HashJoin, Limit, Operator, Project, Scan, Sort, VecScan,
};
use crate::parser::{self, BinOp, DataType, Expr, SelectItem, Statement, TableRef, UnaryOp};
use qmind_kernel::column_delta::{ColumnDataType, ColumnInfo, DeltaApplier, TableSchema};
use qmind_kernel::column_reader::ColumnarReader;
use qmind_kernel::columnar::ColValue;
use qmind_kernel::wal::{CatalogColumn, ColumnKind, TxnId};
use qmind_kernel::{BTree, Error as KError, MvccStore, Snapshot, WalRecord, WalWriter};
use qmind_kernel::{BufferPool, FilePageStore, StorageManager};
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;

#[derive(Debug)]
pub struct ExecResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<SqlValue>>,
    pub rows_affected: u64,
}

impl ExecResult {
    fn empty() -> Self {
        Self {
            columns: vec![],
            rows: vec![],
            rows_affected: 0,
        }
    }
}

/// Embedded SQL engine over the transactional kernel. `W` is the WAL sink
/// (`Vec<u8>` for tests/embedded use; `File` for durable deployments).
pub struct Engine<W: Write> {
    db: MvccStore,
    wal: WalWriter<W>,
    tables: HashMap<String, Vec<ColumnDef>>,
    next_row_id: HashMap<String, u64>,
    /// Secondary indexes: index name → definition.
    indexes: HashMap<String, IndexDef>,
    /// In-memory index trees (single-column, keyed by order-preserving encoding).
    index_trees: HashMap<String, BTree>,
    /// Optional columnar segment directory for HTAP OLAP reads.
    columnar_dir: Option<PathBuf>,
    /// Delta buffer accumulates rows for async columnar flush.
    delta_applier: Option<DeltaApplier>,
    /// Row count threshold before auto-flushing to columnar segments.
    columnar_flush_threshold: usize,
    /// R3: optional persistent table storage (file-backed engines only).
    /// Populated by `create_db` / `open_db`; `None` for in-memory `Engine`.
    storage: Option<StorageManager<FilePageStore>>,
    /// R4-MULTIWRITER: the explicit transactions currently open on this
    /// engine, keyed by session. Multiple sessions may hold concurrent explicit
    /// transactions: each carries its own kernel txn id, snapshot, row locks
    /// (strict 2PL) and buffered rows. Statement execution is still serialized
    /// by `&mut self` (and the server's engine write guard), but transactions
    /// themselves live independently — any subset may roll back or commit
    /// (first-committer-wins on actual key overlap, which the append-only
    /// row-id allocation keeps disjoint). Session 0 is the embedded/default
    /// session used by [`execute`](Self::execute). Concurrent readers stay
    /// lock-free via `execute_read` snapshots.
    active: HashMap<SessionId, ActiveTxn>,
}

/// Identity of a database session. The embedded API ([`Engine::execute`]) uses
/// session 0; the wire server assigns each connection a unique id so every
/// connection can hold its own explicit transaction concurrently.
pub type SessionId = u64;

/// One buffered insert row inside an explicit transaction. Secondary index
/// entries, columnar deltas, and persistent page rows are all materialized
/// only at COMMIT so a ROLLBACK never leaks phantom index/columnar state.
#[derive(Debug)]
struct BufferedRow {
    table: String,
    rid: u64,
    encoded: Vec<u8>,
    values: Vec<SqlValue>,
}

/// R4 — explicit transaction state on the engine (thin lifecycle shim over the
/// kernel's `MvccStore` pending-transaction representation; deliberately not a
/// duplicate transaction state machine).
#[derive(Debug)]
struct ActiveTxn {
    /// Kernel txn id, derives WAL identity and MVCC version ordering.
    txn: TxnId,
    /// Read horizon captured at BEGIN; all reads in the txn observe it.
    snap: Snapshot,
    /// Rows inserted since BEGIN, materialized to indexes/columnar/pages at
    /// COMMIT (write-ahead: WAL is synced before any of this).
    buffered: Vec<BufferedRow>,
}

/// Secondary index definition. Indexes are single-column, non-NULL.
#[derive(Debug, Clone)]
pub struct IndexDef {
    pub name: String,
    pub table: String,
    pub column: String,
}

/// Result of an index-assisted lookup: matched rows plus the predicate with
/// the served conjunct removed (`None` when the conjunct was the whole WHERE).
type IndexLookup = (Vec<Row>, Option<Expr>);

impl<W: Write> Engine<W> {
    pub fn new(wal_sink: W) -> Self {
        Self {
            db: MvccStore::new(),
            wal: WalWriter::new(wal_sink),
            tables: HashMap::new(),
            next_row_id: HashMap::new(),
            indexes: HashMap::new(),
            index_trees: HashMap::new(),
            columnar_dir: None,
            delta_applier: None,
            columnar_flush_threshold: 10_000,
            storage: None,
            active: HashMap::new(),
        }
    }

    /// Enable columnar HTAP: set the directory for columnar segment files.
    /// Rows inserted after this call will be captured for async columnar flush.
    pub fn with_columnar(mut self, dir: PathBuf) -> Self {
        std::fs::create_dir_all(&dir).ok();
        let mut applier = DeltaApplier::new(dir.clone(), self.columnar_flush_threshold);
        // Register schemas for existing tables.
        for (name, cols) in &self.tables {
            let schema = column_def_to_schema(name, cols);
            applier.register_table(schema);
        }
        self.columnar_dir = Some(dir);
        self.delta_applier = Some(applier);
        self
    }

    /// Set the row count threshold before auto-flushing to columnar.
    pub fn set_columnar_flush_threshold(&mut self, threshold: usize) {
        self.columnar_flush_threshold = threshold;
    }

    /// Check if columnar data exists for a table.
    pub fn has_columnar_data(&self, table: &str) -> bool {
        if let Some(ref dir) = self.columnar_dir {
            let table_dir = dir.join(table);
            if let Ok(entries) = std::fs::read_dir(&table_dir) {
                return entries.filter_map(|e| e.ok()).any(|e| {
                    let name = e.file_name();
                    let name = name.to_string_lossy();
                    name.starts_with("col_") && name.ends_with(".seg")
                });
            }
        }
        false
    }

    /// Flush pending delta rows to columnar segments.
    /// Returns table_name → rows_flushed.
    pub fn flush_to_columnar(&mut self) -> Result<HashMap<String, usize>, String> {
        if let Some(ref mut applier) = self.delta_applier {
            applier.flush_all().map_err(|e| e.to_string())
        } else {
            Ok(HashMap::new())
        }
    }

    /// Read rows from columnar segments for a table.
    fn read_columnar(&self, table: &str, schema: &[ColumnDef]) -> Result<Vec<Row>, String> {
        let dir = self
            .columnar_dir
            .as_ref()
            .ok_or("columnar not enabled")?
            .join(table);

        let table_schema = column_def_to_schema(table, schema);
        let reader = ColumnarReader::open(&dir, table_schema).map_err(|e| e.to_string())?;

        let col_values = reader.read_all_rows().map_err(|e| e.to_string())?;

        // Convert ColValue → SqlValue for each row.
        Ok(col_values
            .into_iter()
            .map(|row| {
                row.into_iter()
                    .map(|cv| col_value_to_sql_value(&cv))
                    .collect()
            })
            .collect())
    }

    /// Parse + execute a single statement on the embedded default session
    /// (session 0). See [`execute_session`](Self::execute_session) for the
    /// session-aware form used by the wire server.
    ///
    /// R4: transaction-control statements (`BEGIN`, `COMMIT`, `ROLLBACK`)
    /// drive the engine's explicit transaction. With an explicit transaction
    /// open, DML is buffered and only durable at `COMMIT`; DDL is rejected
    /// (it is autocommit by design and cannot be rolled back). Without one,
    /// every statement remains autocommit.
    pub fn execute(&mut self, sql: &str) -> Result<ExecResult, String> {
        self.execute_session(0, sql)
    }

    /// Parse + execute a single statement on behalf of `session` (one active
    /// explicit transaction per session; multiple sessions may be active
    /// concurrently). Statement execution is serialized by `&mut self`, but the
    /// session's transaction — snapshot, pending writes and strict-2PL row
    /// locks — is independent of every other session's, so two sessions can
    /// write, roll back and commit concurrently without interfering.
    pub fn execute_session(&mut self, session: SessionId, sql: &str) -> Result<ExecResult, String> {
        self.execute_session_params(session, sql, &[])
    }

    /// Extended-protocol execute: parse `sql` with `$n` placeholders resolved
    /// to the `params` bound by the preceding Bind message (and passed
    /// 1-based), then run the self-contained statement through the exact same
    /// dispatch used by the simple-Query path. An empty slice is the
    /// simple-protocol fast path and keeps behavior identical to
    /// [`execute_session`](Self::execute_session).
    pub fn execute_session_params(
        &mut self,
        session: SessionId,
        sql: &str,
        params: &[SqlValue],
    ) -> Result<ExecResult, String> {
        let stmts = if params.is_empty() {
            parser::Parser::parse(sql)?
        } else {
            parser::Parser::parse_with_params(sql, params)?
        };
        if stmts.len() != 1 {
            return Err(format!(
                "expected exactly one statement, got {}",
                stmts.len()
            ));
        }
        match &stmts[0] {
            Statement::Begin => self.txn_begin(session),
            Statement::Commit => self.txn_commit(session),
            Statement::Rollback => self.txn_rollback(session),
            Statement::CreateTable { .. }
            | Statement::CreateIndex { .. }
            | Statement::DropIndex { .. }
                if self.active.contains_key(&session) =>
            {
                Err("DDL is not supported inside an explicit transaction; \
                     commit or roll back first"
                    .into())
            }
            Statement::CreateTable {
                name,
                columns,
                if_not_exists,
            } => self.create_table(name, columns, *if_not_exists),
            Statement::Insert { table, rows } => self.insert(session, table, rows),
            Statement::Update {
                table,
                assignments,
                selection,
            } => self.update(session, table, assignments, selection.as_ref()),
            Statement::Delete {
                table,
                selection,
            } => self.delete(session, table, selection.as_ref()),
            Statement::Select(sel) => {
                let (snap, txn) = match self.active.get(&session) {
                    Some(a) => (a.snap, Some(a.txn)),
                    None => (self.db.snapshot(), None),
                };
                self.select(sel, &snap, txn)
            }
            Statement::CreateIndex {
                name,
                table,
                columns,
            } => self.create_index(name, table, columns),
            Statement::DropIndex { name } => self.drop_index(name),
            Statement::ShowTables => self.show_tables(),
        }
    }

    /// True when `session` holds an explicit `BEGIN` transaction (the wire
    /// server uses this to route a session's statements through the write path
    /// so they observe the transaction's own writes).
    pub fn session_in_transaction(&self, session: SessionId) -> bool {
        self.active.contains_key(&session)
    }

    /// True when the embedded default session (0) holds an explicit
    /// `BEGIN` transaction.
    pub fn in_transaction(&self) -> bool {
        self.active.contains_key(&0)
    }

    // ── R4 transaction control ───────────────────────────────────────────────

    /// BEGIN — allocate a kernel transaction and snapshot for `session`. No
    /// WAL record is emitted yet: this engine uses deferred commit logging, so
    /// an explicit transaction only reaches the WAL atomically at COMMIT.
    fn txn_begin(&mut self, session: SessionId) -> Result<ExecResult, String> {
        if self.active.contains_key(&session) {
            return Err("a transaction is already in progress".into());
        }
        let (txn, snap) = self.db.begin();
        self.active.insert(
            session,
            ActiveTxn {
                txn,
                snap,
                buffered: Vec::new(),
            },
        );
        Ok(ExecResult::empty())
    }

    /// COMMIT — atomically persist `[Begin, Puts..., Commit]` to the WAL in a
    /// single group (one sync = the durability point), publish the versions to
    /// MVCC, then materialize indexes/columnar/page-store rows in write-ahead
    /// order. On a write-write conflict the loser is aborted per
    /// first-committer-wins.
    fn txn_commit(&mut self, session: SessionId) -> Result<ExecResult, String> {
        let Some(act) = self.active.remove(&session) else {
            return Err("no transaction in progress".into());
        };
        let count = self.finish_commit(act.txn, act.buffered)?;
        Ok(ExecResult {
            columns: vec![],
            rows: vec![],
            rows_affected: count,
        })
    }

    /// ROLLBACK — discard the kernel transaction's buffered writes (the MVCC
    /// pending state) and any engine-side materialization buffers. Since
    /// nothing was applied to indexes/columnar/pages during the transaction,
    /// abort is a pure in-memory discard; an `Abort` record is written for an
    /// auditable transaction boundary.
    fn txn_rollback(&mut self, session: SessionId) -> Result<ExecResult, String> {
        let Some(act) = self.active.remove(&session) else {
            return Err("no transaction in progress".into());
        };
        self.db.abort(act.txn);
        self.wal.append(&WalRecord::Abort { txn: act.txn });
        let _ = self.wal.commit_group();
        Ok(ExecResult::empty())
    }

    /// Shared commit tail: WAL-group the transaction, publish MVCC versions,
    /// then materialize buffered rows into secondary indexes, columnar deltas
    /// and persistent pages — always after the WAL durability point.
    /// Tombstone value written for a DELETE under the row key. It is a single
    /// `0xFF` byte: every real row payload starts with a column tag ∈ {0,1,2}
    /// (see [`encode_row`]), so no valid encoding ever begins with `0xFF` and
    /// `decode_row` rejects it unconditionally. Scans (table + index + columnar
    /// reconciliation) filter on `decode_row`'s `Option`, so the row vanishes
    /// from every read path while the MVCC version chain still records the
    /// tombstone — and the WAL `Put` carrying these bytes replays the DELETE
    /// exactly, with no new WAL variant required.
    const DELETED_ROW: [u8; 1] = [0xFF];

    pub fn update(
        &mut self,
        session: SessionId,
        table: &str,
        assignments: &[parser::Assignment],
        selection: Option<&parser::Expr>,
    ) -> Result<ExecResult, String> {
        let schema = self
            .tables
            .get(table)
            .cloned()
            .ok_or_else(|| format!("no table `{table}`"))?;
        if assignments.is_empty() {
            return Err("UPDATE requires at least one SET assignment".into());
        }
        let rid_max = *self.next_row_id.get(table).unwrap_or(&0);
        let col_idx: Vec<usize> = assignments
            .iter()
            .map(|a| col_pos(&schema, &a.column))
            .collect::<Result<_, String>>()?;

        let mut buffered: Vec<BufferedRow> = Vec::new();
        for rid in 0..rid_max {
            let Some(raw) = self.db.read(None, &row_key(table, rid), &self.db.snapshot()) else {
                continue; // never committed, or already deleted (tombstone)
            };
            let Some(mut row) = decode_row(&raw, &schema) else {
                continue; // must be a tombstone — invisible to UPDATE
            };
            if let Some(sel) = selection {
                if !is_true(&eval_expr(sel, &schema, &row)?)? {
                    continue;
                }
            }
            for (a, pos) in assignments.iter().zip(&col_idx) {
                row[*pos] = literal_value(&a.value)?;
            }
            let encoded = encode_row(&row);
            buffered.push(BufferedRow {
                table: table.to_string(),
                rid,
                encoded: encoded.clone(),
                values: row,
            });
        }
        let count = buffered.len() as u64;
        self.apply_mutations(session, buffered)?;
        Ok(ExecResult {
            columns: vec![],
            rows: vec![],
            rows_affected: count,
        })
    }

    pub fn delete(
        &mut self,
        session: SessionId,
        table: &str,
        selection: Option<&parser::Expr>,
    ) -> Result<ExecResult, String> {
        let schema = self
            .tables
            .get(table)
            .cloned()
            .ok_or_else(|| format!("no table `{table}`"))?;
        let rid_max = *self.next_row_id.get(table).unwrap_or(&0);
        let mut buffered: Vec<BufferedRow> = Vec::new();
        for rid in 0..rid_max {
            let Some(raw) = self.db.read(None, &row_key(table, rid), &self.db.snapshot()) else {
                continue;
            };
            let Some(row) = decode_row(&raw, &schema) else {
                continue;
            };
            if let Some(sel) = selection {
                if !is_true(&eval_expr(sel, &schema, &row)?)? {
                    continue;
                }
            }
            buffered.push(BufferedRow {
                table: table.to_string(),
                rid,
                encoded: Self::DELETED_ROW.to_vec(),
                values: row,
            });
        }
        let count = buffered.len() as u64;
        self.apply_mutations(session, buffered)?;
        Ok(ExecResult {
            columns: vec![],
            rows: vec![],
            rows_affected: count,
        })
    }

    /// Shared autocommit/explicit mutation tail — exactly the INSERT write
    /// discipline, reused for UPDATE/DELETE so every DML statement materializes
    /// durable state through one path.
    ///
    /// 1. Inside an explicit transaction: reserve an exclusive write lock on
    ///    every touched row key, buffer the new bytes in the session's pending
    ///    kernel txn (read-your-own-writes, no partial statement state on
    ///    failure), and return — MVCC visibility, WAL, indexes and columnar
    ///    deltas all materialize at COMMIT via `finish_commit`.
    /// 2. Autocommit: BEGIN a kernel txn, `set` the buffered rows (conflict or
    ///    lock failure leaves no trace), then `commit` with the WAL group. WAL
    ///    records are emitted before any new version publishes (write-ahead),
    ///    and the group is fsynced once at the durability point.
    fn apply_mutations(
        &mut self,
        session: SessionId,
        buffered: Vec<BufferedRow>,
    ) -> Result<(), String> {
        if buffered.is_empty() {
            return Ok(());
        }
        if let Some(act) = self.active.get_mut(&session) {
            for b in &buffered {
                self.db
                    .lock_write(act.txn, &row_key(&b.table, b.rid))
                    .map_err(|e| {
                        format!(
                            "statement failed: row locked by another transaction ({e:?})"
                        )
                    })?;
            }
            for b in &buffered {
                self.db
                    .set(act.txn, &row_key(&b.table, b.rid), b.encoded.clone())
                    .expect("row key is already write-locked by this transaction");
            }
            act.buffered.extend(buffered);
            return Ok(());
        }

        let (txn, _snap) = self.db.begin();
        for b in &buffered {
            if let Err(e) = self.db.set(txn, &row_key(&b.table, b.rid), b.encoded.clone()) {
                self.db.abort(txn);
                return Err(format!(
                    "statement failed: row locked by another transaction ({e:?})"
                ));
            }
        }
        let logged = self
            .db
            .commit(txn, |recs| {
                for rec in recs {
                    self.wal.append(rec);
                }
                self.wal
                    .commit_group()
                    .map(|_| ())
                    .map_err(|e| format!("wal failure: {e:?}"))
            })
            .map_err(|e| format!("wal failure: {e}"))?;
        match logged {
            Ok(()) => self.finish_commit(txn, buffered).map(|_| ()),
            Err(_) => Err("concurrent update/delete conflict".into()),
        }
    }

    fn finish_commit(&mut self, txn: TxnId, buffered: Vec<BufferedRow>) -> Result<u64, String> {
        let logged = self
            .db
            .commit::<String>(txn, |recs| {
                for r in recs {
                    self.wal.append(r);
                }
                self.wal
                    .commit_group()
                    .map(|_| ())
                    .map_err(|e| format!("wal failure: {e:?}"))
            })
            .map_err(|e| format!("wal failure: {e}"))?;
        if let Err(c) = logged {
            // First-committer-wins: this writer lost a concurrent conflict
            // on one of its keys. The kernel re-admitted the pending state,
            // so abort it cleanly — no rows, indexes or pages leak.
            self.db.abort(txn);
            return Err(format!(
                "transaction aborted: write-write conflict on key {:?} \
                 (first-committer-wins)",
                c.key
            ));
        }
        // WAL is synced (write-ahead) — now apply the buffered rows to
        // indexes, columnar deltas and the persistent page store.
        for b in &buffered {
            let schema = self
                .tables
                .get(&b.table)
                .cloned()
                .ok_or_else(|| format!("no table `{}`", b.table))?;
            for (idx_name, def) in self.indexes.iter().filter(|(_, d)| d.table == b.table) {
                let col_idx = col_pos(&schema, &def.column)?;
                let v = &b.values[col_idx];
                if *v != SqlValue::Null {
                    match self.index_trees.get_mut(idx_name) {
                        Some(tree) => {
                            tree.insert(&index_key_encode(v).map_err(|e| e.to_string())?, b.rid)
                        }
                        None => return Err(format!("index `{idx_name}` tree missing")),
                    }
                }
            }
            if let Some(ref mut applier) = self.delta_applier {
                let col_values: Vec<ColValue> =
                    b.values.iter().map(sql_value_to_col_value).collect();
                applier.append_row_to(&b.table, col_values);
            }
            if let Some(ref mut sm) = self.storage {
                sm.insert_row(&b.table, &b.encoded)
                    .map_err(|e| e.to_string())?;
            }
        }
        Ok(buffered.len() as u64)
    }

    /// Parse + execute a read-only statement (SELECT / SHOW TABLES).
    ///
    /// Unlike [`execute`](Self::execute), this takes `&self` and captures one
    /// snapshot for the whole statement, so the read observes a single
    /// point-in-time even across a multi-table JOIN. Concurrent readers can
    /// run in parallel under a shared read guard; the scan itself is lock-free
    /// against the writer's commit once the snapshot is captured.
    pub fn execute_read(&self, sql: &str) -> Result<ExecResult, String> {
        let stmts = parser::Parser::parse(sql)?;
        if stmts.len() != 1 {
            return Err(format!(
                "expected exactly one statement, got {}",
                stmts.len()
            ));
        }
        let snap = self.db.snapshot();
        match &stmts[0] {
            Statement::Select(sel) => self.select(sel, &snap, None),
            Statement::ShowTables => self.show_tables(),
            _ => Err("statement requires a write connection (use execute)".into()),
        }
    }

    fn show_tables(&self) -> Result<ExecResult, String> {
        let mut names: Vec<String> = self.tables.keys().cloned().collect();
        names.sort();
        Ok(ExecResult {
            columns: vec!["table".into()],
            rows: names.into_iter().map(|n| vec![SqlValue::Text(n)]).collect(),
            rows_affected: 0,
        })
    }
    fn create_table(
        &mut self,
        name: &str,
        columns: &[parser::Column],
        if_not_exists: bool,
    ) -> Result<ExecResult, String> {
        if self.tables.contains_key(name) {
            if if_not_exists {
                return Ok(ExecResult::empty());
            }
            return Err(format!("table `{name}` already exists"));
        }
        let mut cols = Vec::new();
        for col in columns {
            let ty = match col.data_type {
                DataType::Integer => ColumnType::Int,
                DataType::Text => ColumnType::Text,
            };
            cols.push(ColumnDef {
                name: col.name.clone(),
                ty,
                nullable: !col.not_null,
            });
        }
        // R2.4/2.8: DDL is autocommit — the durable WAL record lands (and is
        // synced) BEFORE the in-memory catalog mutates, so the catalog can be
        // rebuilt from the log and a crash never leaks a half-made table.
        self.wal.append(&WalRecord::CreateTable {
            name: name.to_string(),
            columns: column_def_to_catalog(&cols),
        });
        if let Err(e) = self.wal.commit_group() {
            return Err(format!("wal failure committing CREATE TABLE: {e:?}"));
        }
        self.tables.insert(name.to_string(), cols.clone());
        self.next_row_id.entry(name.to_string()).or_insert(0);
        // R3: register the table in persistent storage (post-WAL-fsync).
        if let Some(ref mut sm) = self.storage {
            sm.create_table(name).map_err(|e| e.to_string())?;
        }
        // M9: Register table schema for columnar delta capture.
        if let Some(ref mut applier) = self.delta_applier {
            let schema = column_def_to_schema(name, &cols);
            applier.register_table(schema);
        }
        Ok(ExecResult::empty())
    }

    fn create_index(
        &mut self,
        name: &str,
        table: &str,
        columns: &[String],
    ) -> Result<ExecResult, String> {
        if self.indexes.contains_key(name) {
            return Err(format!("index `{name}` already exists"));
        }
        if columns.len() != 1 {
            return Err("only single-column indexes are supported".into());
        }
        let column = columns[0].clone();
        let schema = self
            .tables
            .get(table)
            .cloned()
            .ok_or_else(|| format!("no table `{table}`"))?;
        col_pos(&schema, &column)?;

        // Durable autocommit DDL before in-memory registration.
        self.wal.append(&WalRecord::CreateIndex {
            name: name.to_string(),
            table: table.to_string(),
            column: column.clone(),
        });
        if let Err(e) = self.wal.commit_group() {
            return Err(format!("wal failure committing CREATE INDEX: {e:?}"));
        }

        // Backfill from existing rows.
        let mut tree = BTree::new();
        let snap = self.db.snapshot();
        // R4-MVCC: iterate actual row ids rather than enumerating present
        // rows — rolled-back transactions leave rid gaps, and a compressed
        // enumeration would map lookups onto the wrong row keys.
        for rid in 0..*self.next_row_id.get(table).unwrap_or(&0) {
            let key = row_key(table, rid);
            if let Some(raw) = self.db.get_raw(&key, &snap) {
                if let Some(row) = decode_row(&raw, &schema) {
                    let v = &row[col_pos(&schema, &column)?];
                    if *v != SqlValue::Null {
                        tree.insert(&index_key_encode(v)?, rid);
                    }
                }
            }
        }

        self.indexes.insert(
            name.to_string(),
            IndexDef {
                name: name.to_string(),
                table: table.to_string(),
                column,
            },
        );
        self.index_trees.insert(name.to_string(), tree);
        Ok(ExecResult::empty())
    }

    fn drop_index(&mut self, name: &str) -> Result<ExecResult, String> {
        if !self.indexes.contains_key(name) {
            return Err(format!("no index `{name}`"));
        }
        // Durable autocommit DDL before in-memory deregistration.
        self.wal.append(&WalRecord::DropIndex {
            name: name.to_string(),
        });
        if let Err(e) = self.wal.commit_group() {
            return Err(format!("wal failure committing DROP INDEX: {e:?}"));
        }
        self.indexes.remove(name);
        self.index_trees.remove(name);
        Ok(ExecResult::empty())
    }

    fn insert(
        &mut self,
        session: SessionId,
        table: &str,
        rows: &[Vec<Expr>],
    ) -> Result<ExecResult, String> {
        let schema = self
            .tables
            .get(table)
            .cloned()
            .ok_or_else(|| format!("no table `{table}`"))?;

        let start_id = *self.next_row_id.entry(table.to_string()).or_insert(0);
        let mut count = 0u64;
        // R4: collect evaluated + encoded rows; secondary indexes, columnar
        // deltas and persistent pages are all materialized at COMMIT (via
        // `finish_commit`), never during statement execution.
        let mut buffered: Vec<BufferedRow> = Vec::with_capacity(rows.len());
        for row_expr in rows {
            if row_expr.len() != schema.len() {
                return Err(format!(
                    "table `{table}` has {} columns, got {}",
                    schema.len(),
                    row_expr.len()
                ));
            }
            let mut row = Vec::with_capacity(schema.len());
            for (expr, def) in row_expr.iter().zip(&schema) {
                let v = literal_value(expr)?;
                if v == SqlValue::Null {
                    if !def.nullable {
                        return Err(format!("column {} is NOT NULL", def.name));
                    }
                } else if !def.ty.check(&v) {
                    return Err(format!("column {} expects {}", def.name, def.ty.name()));
                }
                row.push(v);
            }
            let rid = start_id + count;
            let row_bytes = encode_row(&row);
            buffered.push(BufferedRow {
                table: table.to_string(),
                rid,
                encoded: row_bytes.clone(),
                values: row,
            });
            count += 1;
        }
        *self.next_row_id.get_mut(table).unwrap() += count;

        // Explicit transaction: buffer rows in the kernel's pending set (own-
        // write visibility) and hold them for COMMIT. The row keys are reserved
        // by `next_row_id`, so a later statement in the same transaction never
        // reuses them; a ROLLBACK simply leaves gaps in the id sequence.
        //
        // Strict 2PL: every row key is write-locked for the whole transaction.
        // All keys are reserved up front (statement-atomic at the lock level)
        // — a conflict fails the statement before any pending write lands, so
        // a failed statement leaves no partial row state. The subsequent
        // writes are re-entrant on the reserved locks.
        if let Some(act) = self.active.get_mut(&session) {
            for b in &buffered {
                self.db
                    .lock_write(act.txn, &row_key(table, b.rid))
                    .map_err(|e| {
                        format!("statement failed: row locked by another transaction ({e:?})")
                    })?;
            }
            for b in &buffered {
                self.db
                    .set(act.txn, &row_key(table, b.rid), b.encoded.clone())
                    .expect("row key is already write-locked by this transaction");
            }
            act.buffered.extend(buffered);
            return Ok(ExecResult {
                columns: vec![],
                rows: vec![],
                rows_affected: count,
            });
        }

        // Autocommit: one implicit transaction per statement. A lock conflict
        // aborts the implicit transaction outright — pending writes and locks
        // are discarded, so the failed statement leaves no trace.
        let (txn, _snap) = self.db.begin();
        for b in &buffered {
            if let Err(e) = self.db.set(txn, &row_key(table, b.rid), b.encoded.clone()) {
                self.db.abort(txn);
                return Err(format!(
                    "statement failed: row locked by another transaction ({e:?})"
                ));
            }
        }
        match self.finish_commit(txn, buffered) {
            Ok(committed) => {
                if committed != count {
                    return Err("internal error: commit count mismatch".into());
                }
                Ok(ExecResult {
                    columns: vec![],
                    rows: vec![],
                    rows_affected: count,
                })
            }
            Err(e) => {
                // Do not leak the implicit transaction on statement failure.
                self.db.abort(txn);
                Err(e)
            }
        }
    }

    fn select(
        &self,
        sel: &parser::Select,
        snap: &Snapshot,
        txn: Option<TxnId>,
    ) -> Result<ExecResult, String> {
        // JOIN path.
        if matches!(&sel.from, TableRef::Join { .. }) {
            return self.select_join(sel, snap, txn);
        }

        let table = match &sel.from {
            TableRef::Table(t) => t.clone(),
            _ => unreachable!(),
        };
        let schema = self
            .tables
            .get(&table)
            .cloned()
            .ok_or_else(|| format!("no table `{table}`"))?;

        // M9: Columnar OLAP path — read from columnar segments if available.
        // R4-MVCC: inside an explicit transaction the txn-aware MVCC row store
        // must be read instead, so the transaction's own buffered writes stay
        // visible alongside committed rows (columnar segments only ever hold
        // committed, flushed data).
        if txn.is_none() && self.has_columnar_data(&table) {
            let rows = self.read_columnar(&table, &schema)?;
            return self.run_select(sel, &schema, rows);
        }

        // GROUP BY path.
        if !sel.group_by.is_empty() {
            return self.select_group_by(sel, &schema, &table, snap, txn);
        }

        // Aggregate fast path (no GROUP BY).
        if let Some(aggs) = try_parse_aggregates(&sel.projection, &schema)? {
            return self.select_aggregates(sel, &schema, &table, aggs, snap, txn);
        }

        // General pipeline over the MVCC row store.
        // P4c: index-assisted point lookup (top-level `col = literal`
        // conjunct backed by a secondary index).
        if let Some((rows, residual)) =
            self.index_lookup(&table, &schema, &sel.selection, snap, txn)?
        {
            let mut sel2 = sel.clone();
            sel2.selection = residual;
            return self.run_select(&sel2, &schema, rows);
        }
        let rows = self.scan_table_rows(&table, &schema, snap, txn);
        self.run_select(sel, &schema, rows)
    }

    /// Materialized MVCC snapshot scan of an entire table. Inside an explicit
    /// transaction the read is txn-aware: the transaction observes its own
    /// buffered writes alongside committed versions, never another
    /// transaction's uncommitted state.
    fn scan_table_rows(
        &self,
        table: &str,
        schema: &[ColumnDef],
        snap: &Snapshot,
        txn: Option<TxnId>,
    ) -> Vec<Row> {
        let mut rows = Vec::new();
        for rid in 0..*self.next_row_id.get(table).unwrap_or(&0) {
            let key = row_key(table, rid);
            if let Some(raw) = self.db.read(txn, &key, snap) {
                if let Some(full) = decode_row(&raw, schema) {
                    rows.push(full);
                }
            }
        }
        rows
    }

    /// Planner: try to serve a `col = literal` conjunct from an index.
    ///
    /// Finds the first top-level AND conjunct of the selection that is an
    /// equality between an indexed column and a non-NULL literal, looks up the
    /// index, and returns the matched rows plus the predicate with that
    /// conjunct removed (`None` when it was the whole WHERE clause). Returns
    /// `None` overall when no usable conjunct exists.
    fn index_lookup(
        &self,
        table: &str,
        schema: &[ColumnDef],
        selection: &Option<Expr>,
        snap: &Snapshot,
        txn: Option<TxnId>,
    ) -> Result<Option<IndexLookup>, String> {
        // R4-MVCC: secondary index trees are materialized only at COMMIT
        // (deferred materialization). Inside an explicit transaction they
        // cannot reference the transaction's own buffered (uncommitted) rows,
        // so index reads fall back to the txn-aware table scan, which merges
        // own writes with the committed set under the transaction snapshot.
        // Autocommit/fresh-snapshot reads keep the index fast path.
        if txn.is_some() {
            return Ok(None);
        }
        let Some(pred) = selection else {
            return Ok(None);
        };
        for term in conjuncts(pred) {
            let Expr::BinaryOp {
                left,
                op: BinOp::Eq,
                right,
            } = &term
            else {
                continue;
            };
            let Expr::Identifier(col) = left.as_ref() else {
                continue;
            };
            let Some(idx_name) = self
                .indexes
                .values()
                .find(|d| d.table == table && &d.column == col)
                .map(|d| d.name.clone())
            else {
                continue;
            };
            // Only non-NULL literal equality can use the index (NULLs are
            // not indexed, and `col = NULL` never matches under 3VL anyway).
            let Ok(lit) = literal_value(right) else {
                continue;
            };
            if lit == SqlValue::Null {
                continue;
            }
            let key = index_key_encode(&lit)?;
            let rids = self
                .index_trees
                .get(&idx_name)
                .ok_or_else(|| format!("index `{idx_name}` missing tree"))?
                .get_all(&key);

            let mut rows = Vec::new();
            for rid in rids {
                if let Some(raw) = self.db.read(txn, &row_key(table, rid), snap) {
                    if let Some(full) = decode_row(&raw, schema) {
                        rows.push(full);
                    }
                }
            }
            let residual = remove_conjunct(pred, &term);
            return Ok(Some((rows, residual)));
        }
        Ok(None)
    }

    /// Shared OLTP/OLAP pipeline: scan → filter → ORDER BY → project → limit.
    fn run_select(
        &self,
        sel: &parser::Select,
        schema: &[ColumnDef],
        rows: Vec<Row>,
    ) -> Result<ExecResult, String> {
        let mut op: Box<dyn Operator> = Box::new(VecScan::new(rows));

        if let Some(pred) = &sel.selection {
            let s = schema.to_vec();
            let p: Expr = pred.clone();
            op = Box::new(Filter::new(op, move |r: &Row| {
                is_true(&eval_expr(&p, &s, r)?)
            }));
        }

        if !sel.order_by.is_empty() {
            let s = schema.to_vec();
            let keys: Vec<Expr> = sel.order_by.iter().map(|o| o.expr.clone()).collect();
            let desc: Vec<bool> = sel.order_by.iter().map(|o| !o.asc).collect();
            op = Box::new(Sort::new(op, desc, move |r: &Row| {
                keys.iter()
                    .map(|e| eval_expr(e, &s, r))
                    .collect::<Result<Vec<_>, _>>()
            }));
        }

        let (exprs, out_cols) = projection_specs(sel, schema)?;
        let s = schema.to_vec();
        op = Box::new(Scan::new(move || {
            let Some(r) = op.next()? else {
                return Ok(None);
            };
            let mut out = Vec::with_capacity(exprs.len());
            for e in &exprs {
                out.push(eval_expr(e, &s, &r)?);
            }
            Ok(Some(out))
        }));

        if let Some(limit) = sel.limit {
            op = Box::new(Limit::new(op, limit));
        }

        let mut out_rows = Vec::new();
        while let Some(r) = op.next()? {
            out_rows.push(r);
        }

        Ok(ExecResult {
            columns: out_cols,
            rows: out_rows,
            rows_affected: 0,
        })
    }

    /// Ungrouped aggregate (`SELECT COUNT(*), SUM(v) ... FROM t [WHERE ..]`).
    fn select_aggregates(
        &self,
        sel: &parser::Select,
        schema: &[ColumnDef],
        table: &str,
        aggs: Vec<Aggregate>,
        snap: &Snapshot,
        txn: Option<TxnId>,
    ) -> Result<ExecResult, String> {
        let rows = self.scan_table_rows(table, schema, snap, txn);
        let mut filtered = Vec::new();
        for row in rows {
            match &sel.selection {
                Some(pred) if !is_true(&eval_expr(pred, schema, &row)?)? => continue,
                _ => filtered.push(row),
            }
        }
        let out = aggs
            .iter()
            .map(|a| a.evaluate(&filtered))
            .collect::<Result<Vec<_>, String>>()?;
        Ok(ExecResult {
            columns: aggs.into_iter().map(|a| a.label).collect(),
            rows: vec![out],
            rows_affected: 0,
        })
    }

    fn select_join(
        &self,
        sel: &parser::Select,
        snap: &Snapshot,
        txn: Option<TxnId>,
    ) -> Result<ExecResult, String> {
        let (left, right, on_expr) = match &sel.from {
            TableRef::Join { left, right, on } => match left.as_ref() {
                TableRef::Table(lt) => (lt.clone(), right.clone(), on.clone()),
                _ => return Err("JOIN left side must be a table".into()),
            },
            _ => unreachable!(),
        };
        if left == right {
            return Err("self-joins unsupported".into());
        }
        let lschema = self
            .tables
            .get(&left)
            .cloned()
            .ok_or_else(|| format!("no table `{left}`"))?;
        let rschema = self
            .tables
            .get(&right)
            .cloned()
            .ok_or_else(|| format!("no table `{right}`"))?;

        // JOIN ON must be exactly col = col.
        let Expr::BinaryOp {
            left: on_l,
            op: BinOp::Eq,
            right: on_r,
        } = on_expr
        else {
            return Err("JOIN ON must be an equality".into());
        };

        let combined: Vec<ColumnDef> = lschema.iter().chain(rschema.iter()).cloned().collect();
        let find_col = |name: &str| -> Option<(bool, usize)> {
            let mut hit = None;
            if let Some(p) = lschema.iter().position(|c| c.name == name) {
                hit = Some((true, p));
            }
            if let Some(p) = rschema.iter().position(|c| c.name == name) {
                if hit.is_some() {
                    return None; // ambiguous
                }
                hit = Some((false, p));
            }
            hit
        };
        let resolve_side = |e: &Expr| -> Result<(bool, usize), String> {
            let Expr::Identifier(id) = e else {
                return Err("JOIN ON sides must be columns".into());
            };
            find_col(id).ok_or_else(|| format!("unknown column {id}"))
        };
        let (lside, rside) = {
            let a = resolve_side(&on_l)?;
            let b = resolve_side(&on_r)?;
            match (a.0, b.0) {
                (true, false) => ((a.1 as u8, 0u8), (b.1 as u8, 1u8)),
                (false, true) => ((b.1 as u8, 0u8), (a.1 as u8, 1u8)),
                _ => return Err("JOIN ON must span both tables".into()),
            }
        };
        let (li, ri) = (lside.0 as usize, rside.0 as usize);

        let left_rows: Vec<Row> = self.scan_table_rows(&left, &lschema, snap, txn);
        let right_rows: Vec<Row> = self.scan_table_rows(&right, &rschema, snap, txn);

        let mut op: Box<dyn Operator> = Box::new(HashJoin::new(
            Box::new(VecScan::new(left_rows)),
            Box::new(VecScan::new(right_rows)),
            li,
            ri,
            lschema.len(),
        )?);

        if let Some(pred) = &sel.selection {
            let c = combined.clone();
            let p: Expr = pred.clone();
            op = Box::new(Filter::new(op, move |r: &Row| {
                is_true(&eval_expr(&p, &c, r)?)
            }));
        }

        if !sel.order_by.is_empty() {
            let c = combined.clone();
            let keys: Vec<Expr> = sel.order_by.iter().map(|o| o.expr.clone()).collect();
            let desc: Vec<bool> = sel.order_by.iter().map(|o| !o.asc).collect();
            op = Box::new(Sort::new(op, desc, move |r: &Row| {
                keys.iter()
                    .map(|e| eval_expr(e, &c, r))
                    .collect::<Result<Vec<_>, _>>()
            }));
        }

        let (exprs, out_cols) = projection_specs(sel, &combined)?;
        let c = combined.clone();
        op = Box::new(Scan::new(move || {
            let Some(r) = op.next()? else {
                return Ok(None);
            };
            let mut out = Vec::with_capacity(exprs.len());
            for e in &exprs {
                out.push(eval_expr(e, &c, &r)?);
            }
            Ok(Some(out))
        }));

        if let Some(limit) = sel.limit {
            op = Box::new(Limit::new(op, limit));
        }

        let mut out_rows = Vec::new();
        while let Some(r) = op.next()? {
            out_rows.push(r);
        }

        Ok(ExecResult {
            columns: out_cols,
            rows: out_rows,
            rows_affected: 0,
        })
    }

    fn select_group_by(
        &self,
        sel: &parser::Select,
        schema: &[ColumnDef],
        table: &str,
        snap: &Snapshot,
        txn: Option<TxnId>,
    ) -> Result<ExecResult, String> {
        let mut key_idx = Vec::with_capacity(sel.group_by.len());
        for e in &sel.group_by {
            let Expr::Identifier(id) = e else {
                return Err("GROUP BY supports plain columns only".into());
            };
            key_idx.push(col_pos(schema, id)?);
        }

        enum Item {
            Key(usize),
            Agg(Aggregate),
        }
        let mut items = Vec::new();
        let mut out_cols = Vec::new();
        for item in &sel.projection {
            let SelectItem::Expr(expr) = item else {
                return Err("unsupported GROUP BY projection".into());
            };
            match expr {
                Expr::Function { name, args } => {
                    let fname = name.to_uppercase();
                    let func = match fname.as_str() {
                        "COUNT" => AggFn::Count,
                        "SUM" => AggFn::Sum,
                        "AVG" => AggFn::Avg,
                        "MIN" => AggFn::Min,
                        "MAX" => AggFn::Max,
                        other => return Err(format!("unsupported function {other}")),
                    };
                    let col = if args.is_empty() {
                        None
                    } else {
                        let Expr::Identifier(col_name) = &args[0] else {
                            return Err(format!("{fname} requires a column or *"));
                        };
                        if col_name == "*" {
                            None
                        } else {
                            Some(col_pos(schema, col_name)?)
                        }
                    };
                    out_cols.push(format_expr(expr));
                    items.push(Item::Agg(Aggregate {
                        label: String::new(),
                        func,
                        col,
                    }));
                }
                Expr::Identifier(id) => {
                    let pos = col_pos(schema, id)?;
                    if !key_idx.contains(&pos) {
                        return Err(format!(
                            "column {id} must appear in GROUP BY or be aggregated",
                        ));
                    }
                    out_cols.push(id.clone());
                    items.push(Item::Key(pos));
                }
                other => return Err(format!("unsupported GROUP BY projection {other:?}")),
            }
        }

        let mut raw_rows: Vec<Row> = Vec::new();
        for rid in 0..*self.next_row_id.get(table).unwrap_or(&0) {
            let key = row_key(table, rid);
            if let Some(raw) = self.db.read(txn, &key, snap) {
                if let Some(full) = decode_row(&raw, schema) {
                    if let Some(pred) = &sel.selection {
                        if !is_true(&eval_expr(pred, schema, &full)?)? {
                            continue;
                        }
                    }
                    raw_rows.push(full);
                }
            }
        }

        let mut aggs: Vec<(crate::executor::AggFn, Option<usize>)> = Vec::new();
        for item in &items {
            if let Item::Agg(a) = item {
                let f = match a.func {
                    AggFn::Count => crate::executor::AggFn::Count,
                    AggFn::Sum => crate::executor::AggFn::Sum,
                    AggFn::Avg => crate::executor::AggFn::Avg,
                    AggFn::Min => crate::executor::AggFn::Min,
                    AggFn::Max => crate::executor::AggFn::Max,
                };
                aggs.push((f, a.col));
            }
        }

        let nkeys = key_idx.len();
        let mut op: Box<dyn Operator> = Box::new(HashAggregate::new(
            Box::new(VecScan::new(raw_rows)),
            key_idx.clone(),
            aggs,
        ));

        let mut agg_counter = 0usize;
        let mut reorder: Vec<usize> = Vec::with_capacity(items.len());
        for item in &items {
            match item {
                Item::Key(pos) => {
                    reorder.push(key_idx.iter().position(|&i| i == *pos).unwrap());
                }
                Item::Agg(_) => {
                    reorder.push(nkeys + agg_counter);
                    agg_counter += 1;
                }
            }
        }
        op = Box::new(Project::new(op, reorder));

        // ORDER BY over the post-aggregation output.
        if !sel.order_by.is_empty() {
            let mut positions = Vec::new();
            let mut desc = Vec::new();
            for ob in &sel.order_by {
                let pos = grouped_order_index(&ob.expr, &out_cols)?;
                positions.push(pos);
                desc.push(!ob.asc);
            }
            let pos2 = positions.clone();
            op = Box::new(Sort::new(op, desc, move |r: &Row| {
                Ok(pos2.iter().map(|&p| r[p].clone()).collect())
            }));
        }

        if let Some(limit) = sel.limit {
            op = Box::new(Limit::new(op, limit));
        }

        let mut out_rows = Vec::with_capacity(32);
        while let Some(r) = op.next()? {
            out_rows.push(r);
        }

        Ok(ExecResult {
            columns: out_cols,
            rows: out_rows,
            rows_affected: 0,
        })
    }

    // ── R3 streaming batch query ─────────────────────────────────────────

    /// Execute a SELECT through persistent storage using the batch pipeline.
    ///
    /// Streams results to `sink` in bounded batches (`BATCH_ROWS` rows at a
    /// time).  For queries that require full materialization (ORDER BY,
    /// GROUP BY, JOIN) the rows are first collected into memory from the
    /// storage scan, then processed — memory usage is still bounded by the
    /// underlying table size (same as the existing Volcano path), but the
    /// scan itself never exceeds one page per batch step.
    ///
    /// **Usage:**
    /// ```ignore
    /// engine.stream_query("SELECT id, name FROM users WHERE id > 100", |batch| {
    ///     // process batch
    ///     Ok(())
    /// })?;
    /// ```
    ///
    /// Falls back to [`execute`](Self::execute) for DDL/DML and for queries
    /// that require the MvccStore snapshot path.
    pub fn stream_query(
        &mut self,
        sql: &str,
        mut sink: impl FnMut(crate::batch::Batch) -> Result<(), String>,
    ) -> Result<ExecResult, String> {
        use crate::batch::{Batch, SelectionVector, DEFAULT_BATCH_SIZE};

        let stmts = parser::Parser::parse(sql)?;
        if stmts.len() != 1 {
            return Err(format!(
                "expected exactly one statement, got {}",
                stmts.len()
            ));
        }
        let Statement::Select(sel) = &stmts[0] else {
            return Err("stream_query only supports SELECT statements".into());
        };

        if matches!(&sel.from, TableRef::Join { .. }) {
            // ── INNER equi-join over persistent storage (R3-EXEC-2) ──
            use crate::batch_ops::{BatchHashJoin, BatchVecScan};

            let (left_t, right_t, on) = match &sel.from {
                TableRef::Join { left, right, on } => match left.as_ref() {
                    TableRef::Table(lt) => (lt.clone(), right.clone(), on.clone()),
                    _ => return Err("stream_query: JOIN left side must be a table".into()),
                },
                _ => unreachable!(),
            };
            if left_t == right_t {
                return Err("self-joins unsupported".into());
            }
            let lschema = self
                .tables
                .get(&left_t)
                .cloned()
                .ok_or_else(|| format!("no table `{left_t}`"))?;
            let rschema = self
                .tables
                .get(&right_t)
                .cloned()
                .ok_or_else(|| format!("no table `{right_t}`"))?;

            let Expr::BinaryOp {
                left: on_l,
                op: BinOp::Eq,
                right: on_r,
            } = &on
            else {
                return Err("JOIN ON must be an equality".into());
            };
            let Expr::Identifier(lname) = on_l.as_ref() else {
                return Err("JOIN ON sides must be columns".into());
            };
            let Expr::Identifier(rname) = on_r.as_ref() else {
                return Err("JOIN ON sides must be columns".into());
            };
            let find_side = |name: &str| -> Option<(u8, usize)> {
                let mut hit = None;
                if let Some(p) = lschema.iter().position(|c| c.name == name) {
                    hit = Some((0u8, p));
                }
                if let Some(p) = rschema.iter().position(|c| c.name == name) {
                    if hit.is_some() {
                        return None;
                    }
                    hit = Some((1u8, p));
                }
                hit
            };
            let (at, ai) = find_side(lname).ok_or_else(|| format!("unknown column {lname}"))?;
            let (bt, bi) = find_side(rname).ok_or_else(|| format!("unknown column {rname}"))?;
            if at == bt {
                return Err("JOIN ON must span both tables".into());
            }
            // Probe side is the FROM-left table; resolve its key index.
            let (lkey, rkey) = if at == 0 { (ai, bi) } else { (bi, ai) };

            let combined: Vec<ColumnDef> = lschema.iter().chain(rschema.iter()).cloned().collect();
            let (exprs, out_cols) = projection_specs(sel, &combined)?;

            let sm = self
                .storage
                .as_mut()
                .ok_or("storage not enabled (use create_db or open_db)")?;

            // Both inputs are materialized from persistent storage and then
            // run through the batch hash-join operator (build = right side).
            let mut read_rows = |tbl: &str, sch: &[ColumnDef]| -> Result<Vec<Row>, String> {
                let mut rows = Vec::new();
                let mut it = sm.scan_rows(tbl).map_err(|e| e.to_string())?;
                while let Some(raw) = it.next_row().map_err(|e| e.to_string())? {
                    if let Some(r) = decode_row(&raw, sch) {
                        rows.push(r);
                    }
                }
                Ok(rows)
            };
            let left_rows = read_rows(&left_t, &lschema)?;
            let right_rows = read_rows(&right_t, &rschema)?;

            let mut join: Box<dyn crate::batch_ops::BatchOperator> = Box::new(BatchHashJoin::new(
                Box::new(BatchVecScan::new(left_rows)),
                Box::new(BatchVecScan::new(right_rows)),
                lkey,
                rkey,
                lschema.len(),
            )?);

            // Pull joined rows (combined layout) → WHERE → ORDER BY → project → LIMIT.
            let mut rows: Vec<Row> = Vec::new();
            loop {
                let batch = join.next_batch(DEFAULT_BATCH_SIZE)?;
                let Some(batch) = batch else {
                    break;
                };
                for i in 0..batch.num_rows() {
                    let row = batch.row(i);
                    let keep = match &sel.selection {
                        Some(pred) => is_true(&eval_expr(pred, &combined, &row)?)?,
                        None => true,
                    };
                    if keep {
                        rows.push(row);
                    }
                }
            }

            if !sel.order_by.is_empty() {
                let c = combined.clone();
                let keys: Vec<Expr> = sel.order_by.iter().map(|o| o.expr.clone()).collect();
                let desc: Vec<bool> = sel.order_by.iter().map(|o| !o.asc).collect();
                rows.sort_by(|a, b| {
                    let mut ord = std::cmp::Ordering::Equal;
                    for (i, e) in keys.iter().enumerate() {
                        let ka = eval_expr(e, &c, a).unwrap_or(SqlValue::Null);
                        let kb = eval_expr(e, &c, b).unwrap_or(SqlValue::Null);
                        let mut o = crate::codec::total_cmp(&ka, &kb);
                        if desc.get(i).copied().unwrap_or(false) {
                            o = o.reverse();
                        }
                        if o != std::cmp::Ordering::Equal {
                            ord = o;
                            break;
                        }
                    }
                    ord
                });
            }

            if let Some(n) = sel.limit {
                rows.truncate(n);
            }

            let c = combined.clone();
            let mut buf: Vec<Row> = Vec::with_capacity(DEFAULT_BATCH_SIZE);
            let mut total: u64 = 0;
            for row in rows {
                let mut out = Vec::with_capacity(exprs.len());
                for e in &exprs {
                    out.push(eval_expr(e, &c, &row)?);
                }
                buf.push(out);
                if buf.len() >= DEFAULT_BATCH_SIZE {
                    total += buf.len() as u64;
                    sink(crate::batch::rows_to_batch(std::mem::replace(
                        &mut buf,
                        Vec::with_capacity(DEFAULT_BATCH_SIZE),
                    )))?;
                }
            }
            if !buf.is_empty() {
                total += buf.len() as u64;
                sink(crate::batch::rows_to_batch(buf))?;
            }

            return Ok(ExecResult {
                columns: out_cols,
                rows: vec![],
                rows_affected: total,
            });
        }

        let table = match &sel.from {
            TableRef::Table(t) => t.clone(),
            _ => unreachable!(),
        };
        let schema = self
            .tables
            .get(&table)
            .cloned()
            .ok_or_else(|| format!("no table `{table}`"))?;
        let (exprs, out_cols) = projection_specs(sel, &schema)?;

        let has_agg_or_group = !sel.group_by.is_empty() || exprs.iter().any(contains_aggregate);

        if has_agg_or_group {
            use crate::batch_ops::BatchAggregate as BatchAgg;
            use crate::codec::total_cmp;

            let mut key_idx = Vec::with_capacity(sel.group_by.len());
            for e in &sel.group_by {
                let Expr::Identifier(id) = e else {
                    return Err("GROUP BY supports plain columns only".into());
                };
                key_idx.push(col_pos(&schema, id)?);
            }

            enum GItem {
                Key(usize),
                Agg,
            }
            let mut items: Vec<GItem> = Vec::new();
            let mut agg_desc: Vec<(crate::executor::AggFn, Option<usize>)> = Vec::new();

            for expr in &exprs {
                match expr {
                    Expr::Function { name, args } => {
                        let fname = name.to_uppercase();
                        let func = match fname.as_str() {
                            "COUNT" => crate::executor::AggFn::Count,
                            "SUM" => crate::executor::AggFn::Sum,
                            "AVG" => crate::executor::AggFn::Avg,
                            "MIN" => crate::executor::AggFn::Min,
                            "MAX" => crate::executor::AggFn::Max,
                            other => return Err(format!("unsupported function {other}")),
                        };
                        let col = if args.is_empty() {
                            None
                        } else {
                            let Expr::Identifier(col_name) = &args[0] else {
                                return Err(format!("{fname} requires a column or *"));
                            };
                            if col_name == "*" {
                                None
                            } else {
                                Some(col_pos(&schema, col_name)?)
                            }
                        };
                        agg_desc.push((func, col));
                        items.push(GItem::Agg);
                    }
                    Expr::Identifier(id) => {
                        let pos = col_pos(&schema, id)?;
                        if !key_idx.contains(&pos) {
                            return Err(format!(
                                "column {id} must appear in GROUP BY or be aggregated",
                            ));
                        }
                        items.push(GItem::Key(pos));
                    }
                    other => return Err(format!("unsupported GROUP BY projection {other:?}")),
                }
            }

            let nkeys = key_idx.len();
            let mut reorder: Vec<usize> = Vec::with_capacity(items.len());
            let mut agg_counter = 0usize;
            for item in &items {
                match item {
                    GItem::Key(pos) => {
                        reorder.push(key_idx.iter().position(|&i| i == *pos).unwrap());
                    }
                    GItem::Agg => {
                        reorder.push(nkeys + agg_counter);
                        agg_counter += 1;
                    }
                }
            }

            let sm = self
                .storage
                .as_mut()
                .ok_or("storage not enabled (use create_db or open_db)")?;
            let mut iter = sm.scan_rows(&table).map_err(|e| e.to_string())?;
            let mut scan_rows: Vec<Row> = Vec::new();
            while let Some(raw) = iter.next_row().map_err(|e| e.to_string())? {
                if let Some(row) = decode_row(&raw, &schema) {
                    let keep = match &sel.selection {
                        Some(pred) => is_true(&eval_expr(pred, &schema, &row)?)?,
                        None => true,
                    };
                    if keep {
                        scan_rows.push(row);
                    }
                }
            }
            drop(iter);

            let is_grouped = !key_idx.is_empty();
            let agg_desc_empty = agg_desc.clone();
            let input: Box<dyn crate::batch_ops::BatchOperator> =
                Box::new(crate::batch_ops::BatchVecScan::new(scan_rows));

            let mut op: Box<dyn crate::batch_ops::BatchOperator> =
                Box::new(BatchAgg::new(input, key_idx, agg_desc));

            let mut all_rows: Vec<Row> = Vec::new();
            loop {
                let batch = op.next_batch(DEFAULT_BATCH_SIZE)?;
                let Some(batch) = batch else {
                    break;
                };
                for i in 0..batch.num_rows() {
                    let row = batch.row(i);
                    let mut out = Vec::with_capacity(reorder.len());
                    for &p in &reorder {
                        out.push(row[p].clone());
                    }
                    all_rows.push(out);
                }
            }

            // Ungrouped aggregate over an empty input still produces one row
            // (SQL semantics, matching the Volcano `select_aggregates` path):
            // COUNT(*) / COUNT(col) are 0; value aggregates are NULL.
            if all_rows.is_empty() && !is_grouped {
                let mut row: Row = Vec::with_capacity(agg_desc_empty.len());
                for (f, _col) in &agg_desc_empty {
                    row.push(match f {
                        crate::executor::AggFn::Count => SqlValue::Int(0),
                        _ => SqlValue::Null,
                    });
                }
                all_rows.push(row);
            }

            if !sel.order_by.is_empty() {
                let mut positions = Vec::new();
                let mut desc = Vec::new();
                for ob in &sel.order_by {
                    let pos = grouped_order_index(&ob.expr, &out_cols)?;
                    positions.push(pos);
                    desc.push(!ob.asc);
                }
                all_rows.sort_by(|a, b| {
                    let mut ord = std::cmp::Ordering::Equal;
                    for (i, &pos) in positions.iter().enumerate() {
                        let mut o = total_cmp(&a[pos], &b[pos]);
                        if desc[i] {
                            o = o.reverse();
                        }
                        if o != std::cmp::Ordering::Equal {
                            ord = o;
                            break;
                        }
                    }
                    ord
                });
            }

            if let Some(n) = sel.limit {
                all_rows.truncate(n);
            }

            let mut buf: Vec<Row> = Vec::with_capacity(DEFAULT_BATCH_SIZE);
            let mut total_rows: u64 = 0;
            for row in all_rows {
                buf.push(row);
                if buf.len() >= DEFAULT_BATCH_SIZE {
                    total_rows += buf.len() as u64;
                    sink(crate::batch::rows_to_batch(std::mem::replace(
                        &mut buf,
                        Vec::with_capacity(DEFAULT_BATCH_SIZE),
                    )))?;
                }
            }
            if !buf.is_empty() {
                total_rows += buf.len() as u64;
                sink(crate::batch::rows_to_batch(buf))?;
            }

            return Ok(ExecResult {
                columns: out_cols,
                rows: vec![],
                rows_affected: total_rows,
            });
        }

        // ── streaming path (no ORDER BY): read pages → apply WHERE → project ──
        if sel.order_by.is_empty() {
            let sm = self
                .storage
                .as_mut()
                .ok_or("storage not enabled (use create_db or open_db)")?;
            let mut iter = sm.scan_rows(&table).map_err(|e| e.to_string())?;
            let batch_size = DEFAULT_BATCH_SIZE;
            let mut total_rows: u64 = 0;
            let mut limit_rem = sel.limit;
            let s = schema.clone();

            loop {
                let take = limit_rem.unwrap_or(batch_size).min(batch_size);
                if take == 0 {
                    break;
                }
                // Pull raw rows and decode into a batch.
                let mut batch = Batch::with_capacity(schema.len(), take);
                for _ in 0..take {
                    match iter.next_row() {
                        Ok(Some(raw)) => {
                            if let Some(row) = decode_row(&raw, &schema) {
                                batch.push_row(&row);
                            }
                        }
                        Ok(None) => break,
                        Err(e) => return Err(e.to_string()),
                    }
                }
                if batch.is_empty() {
                    break;
                }

                // WHERE: inline selection vector (avoids materializing
                // rejected rows).
                if let Some(ref pred) = sel.selection {
                    let mut sv = SelectionVector::new();
                    for i in 0..batch.num_rows() {
                        let row = batch.row(i);
                        if is_true(&eval_expr(pred, &schema, &row)?)? {
                            sv.push(i);
                        }
                    }
                    if sv.is_empty() {
                        continue;
                    }
                    batch = batch.apply_selection(sv.as_slice());
                }

                let filtered_count = batch.num_rows();

                // Projection (expression-based, row-level).
                let mut proj_batch = Batch::with_capacity(exprs.len(), filtered_count);
                for i in 0..filtered_count {
                    let row = batch.row(i);
                    let mut out = Vec::with_capacity(exprs.len());
                    for e in &exprs {
                        out.push(eval_expr(e, &s, &row)?);
                    }
                    proj_batch.push_row(&out);
                }

                // Limit truncation.
                if let Some(ref mut n) = limit_rem {
                    let take = (*n).min(proj_batch.num_rows());
                    if take < proj_batch.num_rows() {
                        let truncated: Vec<Row> = (0..take).map(|i| proj_batch.row(i)).collect();
                        proj_batch = crate::batch::rows_to_batch(truncated);
                    }
                    *n = n.saturating_sub(proj_batch.num_rows());
                }

                total_rows += proj_batch.num_rows() as u64;
                sink(proj_batch)?;
            }

            return Ok(ExecResult {
                columns: out_cols,
                rows: vec![],
                rows_affected: total_rows,
            });
        }

        // ── materialized path (ORDER BY present): collect → sort → emit ──
        let sm = self
            .storage
            .as_mut()
            .ok_or("storage not enabled (use create_db or open_db)")?;
        let mut iter = sm.scan_rows(&table).map_err(|e| e.to_string())?;
        let batch_size = DEFAULT_BATCH_SIZE;
        let mut all_rows: Vec<Row> = Vec::new();

        loop {
            match iter.next_row() {
                Ok(Some(raw)) => {
                    if let Some(row) = decode_row(&raw, &schema) {
                        // WHERE inline.
                        let keep = match &sel.selection {
                            Some(pred) => is_true(&eval_expr(pred, &schema, &row)?)?,
                            None => true,
                        };
                        if keep {
                            all_rows.push(row);
                        }
                    }
                }
                Ok(None) => break,
                Err(e) => return Err(e.to_string()),
            }
        }

        // Sort (stable, nulls-last, PostgreSQL default).
        if !sel.order_by.is_empty() {
            let s = schema.clone();
            let keys: Vec<Expr> = sel.order_by.iter().map(|o| o.expr.clone()).collect();
            let desc: Vec<bool> = sel.order_by.iter().map(|o| !o.asc).collect();
            all_rows.sort_by(|a, b| {
                let mut ord = std::cmp::Ordering::Equal;
                for (i, e) in keys.iter().enumerate() {
                    let ka = eval_expr(e, &s, a).unwrap_or(SqlValue::Null);
                    let kb = eval_expr(e, &s, b).unwrap_or(SqlValue::Null);
                    let mut o = crate::codec::total_cmp(&ka, &kb);
                    if desc.get(i).copied().unwrap_or(false) {
                        o = o.reverse();
                    }
                    if o != std::cmp::Ordering::Equal {
                        ord = o;
                        break;
                    }
                }
                ord
            });
        }

        // Limit.
        if let Some(n) = sel.limit {
            all_rows.truncate(n);
        }

        // Project and emit.
        let total = all_rows.len() as u64;
        let s = schema.clone();
        let mut buf = Vec::with_capacity(batch_size);
        for row in all_rows {
            let mut out = Vec::with_capacity(exprs.len());
            for e in &exprs {
                out.push(eval_expr(e, &s, &row)?);
            }
            buf.push(out);
            if buf.len() >= batch_size {
                sink(crate::batch::rows_to_batch(std::mem::replace(
                    &mut buf,
                    Vec::with_capacity(batch_size),
                )))?;
            }
        }
        if !buf.is_empty() {
            sink(crate::batch::rows_to_batch(buf))?;
        }

        Ok(ExecResult {
            columns: out_cols,
            rows: vec![],
            rows_affected: total,
        })
    }
}

/// WAL durability hook for file-backed databases: push committed groups
/// across the OS durability boundary via `sync_data` (R2.3).
fn sync_file(f: &mut std::fs::File) -> std::io::Result<()> {
    f.sync_data()
}

impl Engine<std::fs::File> {
    /// Create a fresh database at `path`: versioned metadata, canonical
    /// directory layout, and an empty WAL. Fails if a database already
    /// exists there (a pre-existing `db.meta` is authoritative).
    pub fn create_db(path: impl AsRef<std::path::Path>) -> Result<Self, KError> {
        let root = path.as_ref();
        if crate::dbdir::meta_path(root).exists() {
            return Err(KError::Other(format!(
                "database already exists at {}",
                root.display()
            )));
        }
        crate::dbdir::create_layout(root)?;
        crate::dbdir::write_meta(root)?;
        let wal_file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .append(true)
            .open(crate::dbdir::wal_path(root))?;
        let storage = StorageManager::new(BufferPool::new(
            FilePageStore::open(root.join(crate::dbdir::TABLES_DIR))?,
            256,
        ));
        // Fresh database: no tables yet.
        Ok(Self {
            db: MvccStore::new(),
            wal: WalWriter::with_syncer(wal_file, sync_file),
            tables: HashMap::new(),
            next_row_id: HashMap::new(),
            indexes: HashMap::new(),
            index_trees: HashMap::new(),
            columnar_dir: None,
            delta_applier: None,
            columnar_flush_threshold: 10_000,
            storage: Some(storage),
            active: HashMap::new(),
        })
    }

    /// Open a database at `path` and run startup recovery (R2.5):
    ///
    /// 1. validate `db.meta` versions,
    /// 2. replay the WAL clean prefix,
    /// 3. truncate a torn tail (crash during a commit group),
    /// 4. apply the durable catalog (DDL records),
    /// 5. redo committed transactions into the MVCC store,
    /// 6. recompute row-id counters and rebuild secondary indexes,
    /// 7. resume logging past the recovered prefix.
    ///
    /// Committed data is present after open; in-flight/aborted writes are
    /// gone. Internal corruption (bad CRC, invalid frame lengths) fails
    /// loudly rather than silently inventing state (R2.18).
    pub fn open_db(path: impl AsRef<std::path::Path>) -> Result<Self, KError> {
        let root = path.as_ref();
        let _meta = crate::dbdir::read_meta(root)?;

        let mut wal_file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(crate::dbdir::wal_path(root))?;

        let replay = qmind_kernel::WalReader::replay(&mut wal_file)?;
        let recovered_count = replay.records.len();
        if replay.torn_tail {
            // Crash mid-commit-group: drop the partial frame, keep the prefix.
            wal_file.set_len(replay.end_offset as u64)?;
            wal_file.sync_data()?;
        }

        // ── Catalog: apply DDL records in log order (autocommit semantics). ──
        let mut tables: HashMap<String, Vec<ColumnDef>> = HashMap::new();
        let mut indexes: HashMap<String, IndexDef> = HashMap::new();
        for (_, rec) in &replay.records {
            match rec {
                WalRecord::CreateTable { name, columns } => {
                    tables.insert(name.clone(), catalog_to_column_def(columns));
                }
                WalRecord::CreateIndex {
                    name,
                    table,
                    column,
                } => {
                    indexes.insert(
                        name.clone(),
                        IndexDef {
                            name: name.clone(),
                            table: table.clone(),
                            column: column.clone(),
                        },
                    );
                }
                WalRecord::DropIndex { name } => {
                    indexes.remove(name);
                }
                _ => {} // data records are handled by the kernel redo below
            }
        }

        // ── Data: deterministic committed-state redo (R2.5/2.15). ──
        let mut db = MvccStore::new();
        db.redo_from_records(&replay.records);

        // ── Row-id counters: max committed rid + 1 per table. ──
        let snap = db.snapshot();
        let mut next_row_id = HashMap::new();
        for name in tables.keys() {
            let prefix = format!("{name}\u{1}");
            let mut max_rid: i64 = -1;
            for (key, _) in db.scan_prefix(prefix.as_bytes(), &snap) {
                let rest = &key[prefix.len()..];
                if let Ok(rest) = std::str::from_utf8(rest) {
                    if let Ok(rid) = rest.parse::<u64>() {
                        max_rid = max_rid.max(rid as i64);
                    }
                }
            }
            next_row_id.insert(name.clone(), (max_rid + 1) as u64);
        }

        // ── Indexes (R2.10, option B): rebuild from committed rows. ──
        let mut index_trees = HashMap::new();
        for (idx_name, def) in &indexes {
            let schema = tables
                .get(&def.table)
                .ok_or_else(|| KError::CatalogCorrupt {
                    table: def.table.clone(),
                    reason: format!("index `{idx_name}` references a missing table"),
                })?;
            let col_idx = col_pos(schema, &def.column).map_err(|e| KError::CatalogCorrupt {
                table: def.table.clone(),
                reason: format!("index `{idx_name}`: {e}"),
            })?;
            let mut tree = BTree::new();
            let n = *next_row_id.get(&def.table).unwrap_or(&0);
            for rid in 0..n {
                if let Some(raw) = db.get_raw(&row_key(&def.table, rid), &snap) {
                    if let Some(row) = decode_row(&raw, schema) {
                        let v = &row[col_idx];
                        if *v != SqlValue::Null {
                            let key = index_key_encode(v).map_err(|e| KError::CatalogCorrupt {
                                table: def.table.clone(),
                                reason: e,
                            })?;
                            tree.insert(&key, rid);
                        }
                    }
                }
            }
            index_trees.insert(idx_name.clone(), tree);
        }

        // ── Resume logging past the recovered prefix (already durable). ──
        use std::io::Seek;
        wal_file.seek(std::io::SeekFrom::Start(wal_file.metadata()?.len()))?;
        let mut wal = WalWriter::with_syncer(wal_file, sync_file);
        wal.resume(recovered_count as u64 + 1);

        // ── R3 storage: rebuild persistent tables from the recovered state. ──
        // The WAL remains the recovery authority (R2); the page store is
        // derived so a page scan is never missing historical rows.
        // Wipe stale pages from a prior session (the WAL is the source of truth).
        let tables_dir = root.join(crate::dbdir::TABLES_DIR);
        if tables_dir.exists() {
            std::fs::remove_dir_all(&tables_dir)?;
        }
        std::fs::create_dir_all(&tables_dir)?;
        let mut storage =
            StorageManager::new(BufferPool::new(FilePageStore::open(tables_dir)?, 256));
        // Rebuild in deterministic (sorted) name order so reconstructed table
        // ids and page allocation are stable across reopen/recovery cycles.
        let mut table_names: Vec<&String> = tables.keys().collect();
        table_names.sort();
        for table_name in table_names {
            storage
                .create_table(table_name)
                .map_err(|e| KError::Other(format!("storage rebuild: {e}")))?;
            let n = *next_row_id.get(table_name).unwrap_or(&0);
            for rid in 0..n {
                if let Some(raw) = db.get_raw(&row_key(table_name, rid), &snap) {
                    storage
                        .insert_row(table_name, &raw)
                        .map_err(|e| KError::Other(format!("storage rebuild: {e}")))?;
                }
            }
        }

        Ok(Self {
            db,
            wal,
            tables,
            next_row_id,
            indexes,
            index_trees,
            columnar_dir: None,
            delta_applier: None,
            columnar_flush_threshold: 10_000,
            storage: Some(storage),
            active: HashMap::new(),
        })
    }

    /// Clean shutdown (R2.12 / R3.8): flush and sync any pending WAL group,
    /// then flush dirty storage pages to disk, and release the log file.
    /// Recovery never depends on this method being called — crash safety
    /// comes from the WAL (write-ahead: the WAL is synced before any page
    /// flush, so page data never leads the log).
    pub fn close(mut self) -> Result<(), KError> {
        self.wal.commit_group()?;
        if let Some(ref mut sm) = self.storage {
            sm.flush()
                .map_err(|e| KError::Other(format!("storage flush: {e}")))?;
        }
        Ok(())
    }
}

// == Expression evaluation ====================================================
//
// Three-valued logic (SQL): a predicate is TRUE / FALSE / UNKNOWN (NULL).
// `is_true` treats UNKNOWN as false for WHERE; `and3`/`or3` follow SQL truth
// tables. Comparisons involving NULL or mismatched types are UNKNOWN.

pub fn col_pos(schema: &[ColumnDef], name: &str) -> Result<usize, String> {
    schema
        .iter()
        .position(|c| c.name == name)
        .ok_or_else(|| format!("unknown column {name}"))
}

/// Predicate evaluation for WHERE/JOIN ON: UNKNOWN is treated as false.
pub fn is_true(v: &SqlValue) -> Result<bool, String> {
    Ok(to_logic(v)?.unwrap_or(false))
}

/// SQL boolean coercion: NULL → UNKNOWN (None), INT 0 → false, else true.
pub fn to_logic(v: &SqlValue) -> Result<Option<bool>, String> {
    match v {
        SqlValue::Null => Ok(None),
        SqlValue::Int(n) => Ok(Some(*n != 0)),
        SqlValue::Text(_) => Err(format!("boolean predicate expected INTEGER, got {v:?}")),
    }
}

fn logic_to_int(b: Option<bool>) -> SqlValue {
    match b {
        Some(true) => SqlValue::Int(1),
        Some(false) => SqlValue::Int(0),
        None => SqlValue::Null,
    }
}

fn and3(a: Option<bool>, b: Option<bool>) -> Option<bool> {
    match (a, b) {
        (Some(false), _) | (_, Some(false)) => Some(false),
        (Some(true), Some(true)) => Some(true),
        _ => None,
    }
}

fn or3(a: Option<bool>, b: Option<bool>) -> Option<bool> {
    match (a, b) {
        (Some(true), _) | (_, Some(true)) => Some(true),
        (Some(false), Some(false)) => Some(false),
        _ => None,
    }
}

pub fn eval_expr(expr: &Expr, schema: &[ColumnDef], row: &[SqlValue]) -> Result<SqlValue, String> {
    match expr {
        Expr::Identifier(id) => {
            let pos = col_pos(schema, id)?;
            row.get(pos)
                .cloned()
                .ok_or_else(|| format!("row missing column {id}"))
        }
        Expr::Literal(v) => Ok(v.clone()),
        Expr::Param(n) => Err(format!(
            "unbound parameter ${n}: the extended protocol Bind message must \
             supply a value before Execute (Phase A s1)"
        )),
        Expr::Unary {
            op: UnaryOp::Neg,
            expr,
        } => match eval_expr(expr, schema, row)? {
            SqlValue::Int(n) => n
                .checked_neg()
                .map(SqlValue::Int)
                .ok_or("integer overflow in unary -".into()),
            SqlValue::Null => Ok(SqlValue::Null),
            other => Err(format!("unary - requires INTEGER, got {other:?}")),
        },
        Expr::Unary {
            op: UnaryOp::Not,
            expr,
        } => {
            let v = eval_expr(expr, schema, row)?;
            Ok(match to_logic(&v)? {
                Some(b) => SqlValue::Int(if b { 0 } else { 1 }),
                None => SqlValue::Null,
            })
        }
        Expr::Between { expr, lo, hi } => {
            use std::cmp::Ordering;
            let v = eval_expr(expr, schema, row)?;
            let lo = eval_expr(lo, schema, row)?;
            let hi = eval_expr(hi, schema, row)?;
            Ok(
                match (
                    crate::codec::try_cmp(&v, &lo),
                    crate::codec::try_cmp(&v, &hi),
                ) {
                    (Some(a), Some(b)) => {
                        SqlValue::Int(if a != Ordering::Less && b != Ordering::Greater {
                            1
                        } else {
                            0
                        })
                    }
                    _ => SqlValue::Null,
                },
            )
        }
        Expr::InList { expr, list } => {
            let v = eval_expr(expr, schema, row)?;
            let mut saw_null = false;
            for item in list {
                let iv = eval_expr(item, schema, row)?;
                if iv == SqlValue::Null {
                    saw_null = true;
                } else if iv == v {
                    return Ok(SqlValue::Int(1));
                }
            }
            Ok(if saw_null {
                SqlValue::Null
            } else {
                SqlValue::Int(0)
            })
        }
        Expr::Function { name, args } => eval_scalar(name, args, schema, row),
        Expr::BinaryOp { left, op, right } => eval_binary(left, *op, right, schema, row),
    }
}

/// Scalar (non-aggregate) function evaluation.
fn eval_scalar(
    name: &str,
    args: &[Expr],
    schema: &[ColumnDef],
    row: &[SqlValue],
) -> Result<SqlValue, String> {
    let fname = name.to_uppercase();
    if args.len() != 1 {
        return Err(format!("{name} expects exactly one argument"));
    }
    let v = eval_expr(&args[0], schema, row)?;
    match fname.as_str() {
        "UPPER" => match v {
            SqlValue::Text(s) => Ok(SqlValue::Text(s.to_uppercase())),
            SqlValue::Null => Ok(SqlValue::Null),
            other => Err(format!("UPPER expects TEXT, got {other:?}")),
        },
        "LOWER" => match v {
            SqlValue::Text(s) => Ok(SqlValue::Text(s.to_lowercase())),
            SqlValue::Null => Ok(SqlValue::Null),
            other => Err(format!("LOWER expects TEXT, got {other:?}")),
        },
        "LENGTH" => match v {
            SqlValue::Text(s) => Ok(SqlValue::Int(s.chars().count() as i64)),
            SqlValue::Null => Ok(SqlValue::Null),
            other => Err(format!("LENGTH expects TEXT, got {other:?}")),
        },
        other => Err(format!("unsupported function {other}")),
    }
}

fn eval_binary(
    left: &Expr,
    op: BinOp,
    right: &Expr,
    schema: &[ColumnDef],
    row: &[SqlValue],
) -> Result<SqlValue, String> {
    use std::cmp::Ordering;
    match op {
        BinOp::And | BinOp::Or => {
            let a = to_logic(&eval_expr(left, schema, row)?)?;
            let b = to_logic(&eval_expr(right, schema, row)?)?;
            let r = if op == BinOp::And {
                and3(a, b)
            } else {
                or3(a, b)
            };
            Ok(logic_to_int(r))
        }
        BinOp::Like => {
            let v = eval_expr(left, schema, row)?;
            let p = eval_expr(right, schema, row)?;
            Ok(match (v, p) {
                (SqlValue::Text(s), SqlValue::Text(pat)) => {
                    SqlValue::Int(if like_matches(&s, &pat) { 1 } else { 0 })
                }
                (SqlValue::Null, _) | (_, SqlValue::Null) => SqlValue::Null,
                _ => return Err("LIKE requires TEXT operands".into()),
            })
        }
        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
            let a = eval_expr(left, schema, row)?;
            let b = eval_expr(right, schema, row)?;
            match (a, b) {
                (SqlValue::Null, _) | (_, SqlValue::Null) => Ok(SqlValue::Null),
                (SqlValue::Int(x), SqlValue::Int(y)) => {
                    let n = match op {
                        BinOp::Add => x.checked_add(y),
                        BinOp::Sub => x.checked_sub(y),
                        BinOp::Mul => x.checked_mul(y),
                        BinOp::Div => {
                            if y == 0 {
                                return Err("division by zero".into());
                            }
                            x.checked_div(y)
                        }
                        BinOp::Mod => {
                            if y == 0 {
                                return Err("division by zero".into());
                            }
                            x.checked_rem(y)
                        }
                        _ => unreachable!(),
                    };
                    n.map(SqlValue::Int)
                        .ok_or("integer overflow in arithmetic".into())
                }
                _ => Err("arithmetic requires INTEGER operands".into()),
            }
        }
        BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => {
            let a = eval_expr(left, schema, row)?;
            let b = eval_expr(right, schema, row)?;
            let cmp = crate::codec::try_cmp(&a, &b);
            let truth = match op {
                BinOp::Eq => cmp.map(|o| o == Ordering::Equal),
                BinOp::NotEq => cmp.map(|o| o != Ordering::Equal),
                BinOp::Lt => cmp.map(|o| o == Ordering::Less),
                BinOp::LtEq => cmp.map(|o| o != Ordering::Greater),
                BinOp::Gt => cmp.map(|o| o == Ordering::Greater),
                BinOp::GtEq => cmp.map(|o| o != Ordering::Less),
                _ => unreachable!(),
            };
            Ok(match truth {
                Some(true) => SqlValue::Int(1),
                Some(false) => SqlValue::Int(0),
                None => SqlValue::Null,
            })
        }
    }
}

/// SQL LIKE matcher with `%` (any run) and `_` (single char), case-sensitive.
fn like_matches(s: &str, pattern: &str) -> bool {
    fn helper(s: &[char], p: &[char]) -> bool {
        match (s, p) {
            (_, []) => s.is_empty(),
            (s, ['_', rest @ ..]) => !s.is_empty() && helper(&s[1..], rest),
            (s, ['%', rest @ ..]) => helper(s, rest) || (!s.is_empty() && helper(&s[1..], p)),
            ([c, s_rest @ ..], [pc, p_rest @ ..]) => c == pc && helper(s_rest, p_rest),
            _ => false,
        }
    }
    let s: Vec<char> = s.chars().collect();
    let p: Vec<char> = pattern.chars().collect();
    helper(&s, &p)
}

/// Render an expression back to SQL for result column labels.
pub fn format_expr(e: &Expr) -> String {
    match e {
        Expr::Identifier(id) => id.clone(),
        Expr::Literal(SqlValue::Int(n)) => n.to_string(),
        Expr::Literal(SqlValue::Text(s)) => format!("'{s}'"),
        Expr::Literal(SqlValue::Null) => "NULL".into(),
        Expr::Unary {
            op: UnaryOp::Not,
            expr,
        } => format!("(NOT {})", format_expr(expr)),
        Expr::Unary {
            op: UnaryOp::Neg,
            expr,
        } => format!("(-{})", format_expr(expr)),
        Expr::BinaryOp { left, op, right } => {
            let sym = match op {
                BinOp::Eq => "=",
                BinOp::NotEq => "!=",
                BinOp::Lt => "<",
                BinOp::LtEq => "<=",
                BinOp::Gt => ">",
                BinOp::GtEq => ">=",
                BinOp::And => "AND",
                BinOp::Or => "OR",
                BinOp::Like => "LIKE",
                BinOp::Add => "+",
                BinOp::Sub => "-",
                BinOp::Mul => "*",
                BinOp::Div => "/",
                BinOp::Mod => "%",
            };
            format!("({} {sym} {})", format_expr(left), format_expr(right))
        }
        Expr::Between { expr, lo, hi } => format!(
            "({} BETWEEN {} AND {})",
            format_expr(expr),
            format_expr(lo),
            format_expr(hi)
        ),
        Expr::InList { expr, list } => {
            let items: Vec<String> = list.iter().map(format_expr).collect();
            format!("({} IN ({}))", format_expr(expr), items.join(", "))
        }
        Expr::Function { name, args } => {
            let fname = name.to_uppercase();
            let rendered: Vec<String> = args.iter().map(format_expr).collect();
            format!("{fname}({})", rendered.join(", "))
        }
        Expr::Param(n) => format!("${n}"),
    }
}

/// Resolve a SELECT projection list into per-row expressions + column labels.
fn projection_specs(
    sel: &parser::Select,
    schema: &[ColumnDef],
) -> Result<(Vec<Expr>, Vec<String>), String> {
    if sel.projection.len() == 1 && matches!(sel.projection[0], SelectItem::Star) {
        return Ok((
            schema
                .iter()
                .map(|c| Expr::Identifier(c.name.clone()))
                .collect(),
            schema.iter().map(|c| c.name.clone()).collect(),
        ));
    }
    let mut exprs = Vec::new();
    let mut cols = Vec::new();
    for item in &sel.projection {
        let SelectItem::Expr(e) = item else {
            return Err("unsupported projection".into());
        };
        exprs.push(e.clone());
        cols.push(format_expr(e));
    }
    Ok((exprs, cols))
}

fn contains_aggregate(e: &Expr) -> bool {
    match e {
        Expr::Function { name, args } => {
            let up = name.to_uppercase();
            matches!(up.as_str(), "COUNT" | "SUM" | "AVG" | "MIN" | "MAX")
                || args.iter().any(contains_aggregate)
        }
        Expr::BinaryOp { left, right, .. } => contains_aggregate(left) || contains_aggregate(right),
        _ => false,
    }
}

/// Resolve a GROUP BY ORDER BY expression against the post-aggregation
/// output columns (group keys by name; aggregates by rendered label).
fn grouped_order_index(e: &Expr, out_cols: &[String]) -> Result<usize, String> {
    match e {
        Expr::Identifier(id) => out_cols
            .iter()
            .position(|c| c == id)
            .ok_or_else(|| format!("unknown ORDER BY column {id}")),
        Expr::Function { .. } => {
            let label = format_expr(e);
            out_cols
                .iter()
                .position(|c| *c == label)
                .ok_or_else(|| format!("unknown ORDER BY expression {label}"))
        }
        other => Err(format!("unsupported ORDER BY expression {other:?}")),
    }
}

fn literal_value(expr: &Expr) -> Result<SqlValue, String> {
    match expr {
        Expr::Literal(v) => Ok(v.clone()),
        Expr::Param(_) => Err("unbound parameter: Bind did not supply a value".into()),
        other => Err(format!("unsupported literal {other:?}")),
    }
}

/// Split a predicate into its top-level AND conjuncts.
fn conjuncts(expr: &Expr) -> Vec<Expr> {
    match expr {
        Expr::BinaryOp {
            left,
            op: BinOp::And,
            right,
        } => {
            let mut out = conjuncts(left);
            out.extend(conjuncts(right));
            out
        }
        other => vec![other.clone()],
    }
}

/// Rebuild `pred` with the top-level conjunct `term` removed. Returns `None`
/// only when `term` was the entire predicate.
fn remove_conjunct(pred: &Expr, term: &Expr) -> Option<Expr> {
    match pred {
        Expr::BinaryOp {
            left,
            op: BinOp::And,
            right,
        } => match (remove_conjunct(left, term), remove_conjunct(right, term)) {
            (Some(l), Some(r)) => Some(Expr::BinaryOp {
                left: Box::new(l),
                op: BinOp::And,
                right: Box::new(r),
            }),
            (Some(l), None) => Some(l),
            (None, Some(r)) => Some(r),
            (None, None) => None,
        },
        other => {
            if other == term {
                None
            } else {
                Some(other.clone())
            }
        }
    }
}

/// Order-preserving key for a single index column: tag + sign-flipped
/// big-endian INT bytes, or length-prefixed TEXT. Lexicographic key order
/// matches value order, enabling equality and range scans. NULLs are not
/// indexed (rows with NULL in the indexed column are invisible to the index).
fn index_key_encode(v: &SqlValue) -> Result<Vec<u8>, String> {
    match v {
        SqlValue::Null => Err("NULL values are not indexed".into()),
        SqlValue::Int(n) => {
            let mut key = vec![0u8];
            key.extend_from_slice(&(*n as u64 ^ (1u64 << 63)).to_be_bytes());
            Ok(key)
        }
        SqlValue::Text(s) => {
            let mut key = vec![1u8];
            key.extend_from_slice(&(s.len() as u32).to_be_bytes());
            key.extend_from_slice(s.as_bytes());
            Ok(key)
        }
    }
}

// == M4 aggregates ==========================================================

#[derive(Debug, Clone, Copy, PartialEq)]
enum AggFn {
    Count,
    Sum,
    Avg,
    Min,
    Max,
}

struct Aggregate {
    label: String,
    func: AggFn,
    col: Option<usize>,
}

impl Aggregate {
    fn evaluate(&self, rows: &[Vec<SqlValue>]) -> Result<SqlValue, String> {
        if self.func == AggFn::Count && self.col.is_none() {
            return Ok(SqlValue::Int(rows.len() as i64));
        }
        let idx = self.col.expect("COUNT(*) handled above");
        let mut ints: Vec<i64> = Vec::new();
        let mut texts: Vec<String> = Vec::new();
        for r in rows {
            match &r[idx] {
                SqlValue::Null => continue,
                SqlValue::Int(v) => ints.push(*v),
                SqlValue::Text(v) => texts.push(v.clone()),
            }
        }
        Ok(match self.func {
            AggFn::Count => SqlValue::Int((ints.len() + texts.len()) as i64),
            AggFn::Sum => {
                if ints.is_empty() && texts.is_empty() {
                    SqlValue::Null
                } else if !ints.is_empty() {
                    SqlValue::Int(ints.iter().sum())
                } else {
                    return Err("SUM requires INTEGER column".into());
                }
            }
            AggFn::Avg => {
                if ints.is_empty() {
                    return Ok(SqlValue::Null);
                }
                let sum: i64 = ints.iter().sum();
                SqlValue::Int(sum / ints.len() as i64)
            }
            AggFn::Min | AggFn::Max => {
                let want_min = self.func == AggFn::Min;
                if ints.is_empty() && texts.is_empty() {
                    return Ok(SqlValue::Null);
                }
                if !ints.is_empty() && !texts.is_empty() {
                    return Err("mixed types in MIN/MAX".into());
                }
                if !ints.is_empty() {
                    let best = if want_min {
                        ints.iter().min().expect("non-empty")
                    } else {
                        ints.iter().max().expect("non-empty")
                    };
                    SqlValue::Int(*best)
                } else {
                    let mut best = &texts[0];
                    for v in &texts[1..] {
                        if (want_min && v < best) || (!want_min && v > best) {
                            best = v;
                        }
                    }
                    SqlValue::Text(best.clone())
                }
            }
        })
    }
}

fn try_parse_aggregates(
    projection: &[SelectItem],
    schema: &[ColumnDef],
) -> Result<Option<Vec<Aggregate>>, String> {
    let mut out = Vec::with_capacity(projection.len());
    for item in projection {
        let SelectItem::Expr(expr) = item else {
            return Ok(None);
        };
        let Expr::Function { name, args } = expr else {
            return Ok(None);
        };
        let fname = name.to_uppercase();
        let func = match fname.as_str() {
            "COUNT" => AggFn::Count,
            "SUM" => AggFn::Sum,
            "AVG" => AggFn::Avg,
            "MIN" => AggFn::Min,
            "MAX" => AggFn::Max,
            _ => continue,
        };
        let col = if args.is_empty() {
            None
        } else {
            let Expr::Identifier(col_name) = &args[0] else {
                return Err(format!("{fname} requires a column or *"));
            };
            if col_name == "*" {
                None
            } else {
                Some(col_pos(schema, col_name)?)
            }
        };
        let label = format_expr(expr);
        out.push(Aggregate { label, func, col });
    }
    if out.is_empty() {
        // Projection of scalar functions only — not an aggregate query.
        return Ok(None);
    }
    Ok(Some(out))
}

// ── Columnar integration helpers (M9) ─────────────────────────────────────

/// Convert engine SqlValue to columnar ColValue.
fn sql_value_to_col_value(sv: &SqlValue) -> ColValue {
    match sv {
        SqlValue::Null => ColValue::Null,
        SqlValue::Int(n) => ColValue::Int(*n),
        SqlValue::Text(s) => ColValue::Text(s.clone()),
    }
}

/// Convert engine ColumnDef slice to column_delta TableSchema.
fn column_def_to_schema(table_name: &str, cols: &[ColumnDef]) -> TableSchema {
    TableSchema {
        table_name: table_name.to_string(),
        columns: cols
            .iter()
            .map(|c| ColumnInfo {
                name: c.name.clone(),
                col_type: match c.ty {
                    ColumnType::Int => ColumnDataType::Int,
                    ColumnType::Text => ColumnDataType::Text,
                },
            })
            .collect(),
    }
}

/// Engine ColumnDefs → WAL CatalogColumn list (R2.4 durable catalog).
fn column_def_to_catalog(cols: &[ColumnDef]) -> Vec<CatalogColumn> {
    cols.iter()
        .map(|c| CatalogColumn {
            name: c.name.clone(),
            kind: match c.ty {
                ColumnType::Int => ColumnKind::Int,
                ColumnType::Text => ColumnKind::Text,
            },
            nullable: c.nullable,
        })
        .collect()
}

/// WAL CatalogColumn list → engine ColumnDefs (recovery catalog apply).
fn catalog_to_column_def(cols: &[CatalogColumn]) -> Vec<ColumnDef> {
    cols.iter()
        .map(|c| ColumnDef {
            name: c.name.clone(),
            ty: match c.kind {
                ColumnKind::Int => ColumnType::Int,
                ColumnKind::Text => ColumnType::Text,
            },
            nullable: c.nullable,
        })
        .collect()
}

/// Convert columnar ColValue back to engine SqlValue.
fn col_value_to_sql_value(cv: &ColValue) -> SqlValue {
    match cv {
        ColValue::Null => SqlValue::Null,
        ColValue::Int(n) => SqlValue::Int(*n),
        ColValue::Text(s) => SqlValue::Text(s.clone()),
    }
}
