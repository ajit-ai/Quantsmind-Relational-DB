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
use qmind_kernel::{BTree, MvccStore, Snapshot, WalWriter};
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

    /// Parse + execute a single statement.
    pub fn execute(&mut self, sql: &str) -> Result<ExecResult, String> {
        let stmts = parser::Parser::parse(sql)?;
        if stmts.len() != 1 {
            return Err(format!(
                "expected exactly one statement, got {}",
                stmts.len()
            ));
        }
        match &stmts[0] {
            Statement::CreateTable {
                name,
                columns,
                if_not_exists,
            } => self.create_table(name, columns, *if_not_exists),
            Statement::Insert { table, rows } => self.insert(table, rows),
            Statement::Select(sel) => {
                let snap = self.db.snapshot();
                self.select(sel, &snap)
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
            Statement::Select(sel) => self.select(sel, &snap),
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
        self.tables.insert(name.to_string(), cols.clone());
        self.next_row_id.entry(name.to_string()).or_insert(0);
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

        // Backfill from existing rows.
        let mut tree = BTree::new();
        let snap = self.db.snapshot();
        let rows = self.scan_table_rows(table, &schema, &snap);
        for (rid, row) in rows.iter().enumerate() {
            let v = &row[col_pos(&schema, &column)?];
            if *v != SqlValue::Null {
                tree.insert(&index_key_encode(v)?, rid as u64);
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
        if self.indexes.remove(name).is_none() {
            return Err(format!("no index `{name}`"));
        }
        self.index_trees.remove(name);
        Ok(ExecResult::empty())
    }

    fn insert(&mut self, table: &str, rows: &[Vec<Expr>]) -> Result<ExecResult, String> {
        let schema = self
            .tables
            .get(table)
            .cloned()
            .ok_or_else(|| format!("no table `{table}`"))?;

        let (txn, _snap) = self.db.begin();
        let start_id = *self.next_row_id.entry(table.to_string()).or_insert(0);
        let mut count = 0u64;
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
            self.db.set(txn, &row_key(table, rid), encode_row(&row));
            // Maintain secondary indexes for the table.
            let mut index_errors: Vec<String> = Vec::new();
            for (idx_name, def) in self.indexes.iter().filter(|(_, def)| def.table == table) {
                let col_idx = col_pos(&schema, &def.column)?;
                let v = &row[col_idx];
                if *v != SqlValue::Null {
                    match self.index_trees.get_mut(idx_name) {
                        Some(tree) => {
                            tree.insert(&index_key_encode(v).map_err(|e| e.to_string())?, rid)
                        }
                        None => index_errors.push(format!("index `{idx_name}` tree missing")),
                    }
                }
            }
            if let Some(e) = index_errors.first() {
                return Err(e.clone());
            }
            // M9: Capture row for columnar delta buffer.
            if let Some(ref mut applier) = self.delta_applier {
                let col_values: Vec<ColValue> = row.iter().map(sql_value_to_col_value).collect();
                applier.append_row_to(table, col_values);
            }
            count += 1;
        }
        *self.next_row_id.get_mut(table).unwrap() += count;

        let logged = self
            .db
            .commit::<()>(txn, |recs| {
                for r in recs {
                    self.wal.append(r);
                }
                self.wal.commit_group().map(|_| ()).map_err(|_| ())
            })
            .map_err(|e| format!("wal failure: {e:?}"))?;
        logged.map_err(|c| format!("conflict on {:?}", c.key))?;

        Ok(ExecResult {
            columns: vec![],
            rows: vec![],
            rows_affected: count,
        })
    }

    fn select(&self, sel: &parser::Select, snap: &Snapshot) -> Result<ExecResult, String> {
        // JOIN path.
        if matches!(&sel.from, TableRef::Join { .. }) {
            return self.select_join(sel, snap);
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
        if self.has_columnar_data(&table) {
            let rows = self.read_columnar(&table, &schema)?;
            return self.run_select(sel, &schema, rows);
        }

        // GROUP BY path.
        if !sel.group_by.is_empty() {
            return self.select_group_by(sel, &schema, &table, snap);
        }

        // Aggregate fast path (no GROUP BY).
        if let Some(aggs) = try_parse_aggregates(&sel.projection, &schema)? {
            return self.select_aggregates(sel, &schema, &table, aggs, snap);
        }

        // General pipeline over the MVCC row store.
        // P4c: index-assisted point lookup (top-level `col = literal`
        // conjunct backed by a secondary index).
        if let Some((rows, residual)) = self.index_lookup(&table, &schema, &sel.selection, snap)? {
            let mut sel2 = sel.clone();
            sel2.selection = residual;
            return self.run_select(&sel2, &schema, rows);
        }
        let rows = self.scan_table_rows(&table, &schema, snap);
        self.run_select(sel, &schema, rows)
    }

    /// Materialized MVCC snapshot scan of an entire table.
    fn scan_table_rows(&self, table: &str, schema: &[ColumnDef], snap: &Snapshot) -> Vec<Row> {
        let mut rows = Vec::new();
        for rid in 0..*self.next_row_id.get(table).unwrap_or(&0) {
            let key = row_key(table, rid);
            if let Some(raw) = self.db.get_raw(&key, snap) {
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
    ) -> Result<Option<IndexLookup>, String> {
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
                if let Some(raw) = self.db.get_raw(&row_key(table, rid), snap) {
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
    ) -> Result<ExecResult, String> {
        let rows = self.scan_table_rows(table, schema, snap);
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

    fn select_join(&self, sel: &parser::Select, snap: &Snapshot) -> Result<ExecResult, String> {
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

        let left_rows: Vec<Row> = self.scan_table_rows(&left, &lschema, snap);
        let right_rows: Vec<Row> = self.scan_table_rows(&right, &rschema, snap);

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
            if let Some(raw) = self.db.get_raw(&key, snap) {
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

/// Convert columnar ColValue back to engine SqlValue.
fn col_value_to_sql_value(cv: &ColValue) -> SqlValue {
    match cv {
        ColValue::Null => SqlValue::Null,
        ColValue::Int(n) => SqlValue::Int(*n),
        ColValue::Text(s) => SqlValue::Text(s.clone()),
    }
}
