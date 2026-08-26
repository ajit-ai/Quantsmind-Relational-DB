//! M3 SQL engine: parse → plan-lite → execute over the MVCC kernel.
//!
//! Uses the handwritten parser (E5) for SQL surface:
//! - CREATE TABLE t (col TYPE [NOT NULL], ...) [IF NOT EXISTS]
//! - INSERT INTO t VALUES (..), (..)
//! - SELECT cols | * FROM t [INNER JOIN t ON col = col] [WHERE cond]
//!   [GROUP BY col] [LIMIT n]

use crate::codec::{decode_row, encode_row, row_key, ColumnDef, ColumnType, SqlValue};
use crate::executor::Row;
use crate::executor::{Filter, HashAggregate, HashJoin, Limit, Operator, Project, VecScan};
use crate::parser::{self, BinOp, DataType, Expr, SelectItem, Statement, TableRef};
use qmind_kernel::{MvccStore, WalWriter};
use std::collections::HashMap;
use std::io::Write;

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
}

impl<W: Write> Engine<W> {
    pub fn new(wal_sink: W) -> Self {
        Self {
            db: MvccStore::new(),
            wal: WalWriter::new(wal_sink),
            tables: HashMap::new(),
            next_row_id: HashMap::new(),
        }
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
            Statement::Select(sel) => self.select(sel),
            Statement::ShowTables => {
                let mut names: Vec<String> = self.tables.keys().cloned().collect();
                names.sort();
                Ok(ExecResult {
                    columns: vec!["table".into()],
                    rows: names.into_iter().map(|n| vec![SqlValue::Text(n)]).collect(),
                    rows_affected: 0,
                })
            }
        }
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
        self.tables.insert(name.to_string(), cols);
        self.next_row_id.entry(name.to_string()).or_insert(0);
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

    fn select(&mut self, sel: &parser::Select) -> Result<ExecResult, String> {
        // JOIN path.
        if matches!(&sel.from, TableRef::Join { .. }) {
            return self.select_join(sel);
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

        // GROUP BY path.
        if !sel.group_by.is_empty() {
            return self.select_group_by(sel, &schema, &table);
        }

        // Aggregate fast path (no GROUP BY).
        if let Some(aggs) = try_parse_aggregates(&sel.projection, &schema)? {
            let (_, snap) = self.db.begin();
            let mut rows = Vec::new();
            for rid in 0..*self.next_row_id.get(&table).unwrap_or(&0) {
                let key = row_key(&table, rid);
                let Some(raw) = self.db.get_raw(&key, &snap) else {
                    continue;
                };
                let Some(full) = decode_row(&raw, &schema) else {
                    continue;
                };
                match &sel.selection {
                    Some(pred) if !eval_predicate(pred, &schema, &full)? => continue,
                    _ => rows.push(full),
                }
            }
            let out = aggs
                .iter()
                .map(|a| a.evaluate(&rows))
                .collect::<Result<Vec<_>, String>>()?;
            return Ok(ExecResult {
                columns: aggs.into_iter().map(|a| a.label).collect(),
                rows: vec![out],
                rows_affected: 0,
            });
        }

        // Projection plan.
        enum Proj {
            All,
            Cols(Vec<usize>),
        }
        let (proj, out_cols) =
            if sel.projection.len() == 1 && matches!(sel.projection[0], SelectItem::Star) {
                (
                    Proj::All,
                    schema.iter().map(|c| c.name.clone()).collect::<Vec<_>>(),
                )
            } else {
                let mut idx = Vec::new();
                let mut names = Vec::new();
                for item in &sel.projection {
                    let SelectItem::Expr(Expr::Identifier(id)) = item else {
                        return Err("only plain column projections supported".into());
                    };
                    let pos = schema
                        .iter()
                        .position(|c| c.name == id.as_str())
                        .ok_or_else(|| format!("unknown column {id}"))?;
                    idx.push(pos);
                    names.push(id.clone());
                }
                (Proj::Cols(idx), names)
            };

        // E4b: materialize the MVCC scan, then run the Volcano pipeline.
        let (_, snap) = self.db.begin();
        let max_rows = sel.limit.unwrap_or(0);
        let mut raw_rows: Vec<Row> = Vec::new();
        for rid in 0..*self.next_row_id.get(&table).unwrap_or(&0) {
            let key = row_key(&table, rid);
            if let Some(raw) = self.db.get_raw(&key, &snap) {
                if let Some(full) = decode_row(&raw, &schema) {
                    raw_rows.push(full);
                }
            }
        }

        let mut op: Box<dyn Operator> = Box::new(VecScan::new(raw_rows));
        if let Some(pred) = &sel.selection {
            let s = schema.clone();
            let p: Expr = pred.clone();
            op = Box::new(Filter::new(op, move |r: &Row| eval_predicate(&p, &s, r)));
        }
        op = match &proj {
            Proj::All => op,
            Proj::Cols(idx) => Box::new(Project::new(op, idx.clone())),
        };
        if max_rows > 0 {
            op = Box::new(Limit::new(op, max_rows));
        }

        let mut rows = Vec::new();
        while let Some(r) = op.next()? {
            rows.push(r);
        }

        Ok(ExecResult {
            columns: out_cols,
            rows,
            rows_affected: 0,
        })
    }

    fn select_join(&mut self, sel: &parser::Select) -> Result<ExecResult, String> {
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

        // E4b-2: materialize both sides, then run Volcano pipeline.
        let (_, snap) = self.db.begin();
        let max_rows = sel.limit.unwrap_or(0);

        let mut left_rows: Vec<Row> = Vec::new();
        for rid in 0..*self.next_row_id.get(&left).unwrap_or(&0) {
            let key = row_key(&left, rid);
            if let Some(raw) = self.db.get_raw(&key, &snap) {
                if let Some(row) = decode_row(&raw, &lschema) {
                    left_rows.push(row);
                }
            }
        }
        let mut right_rows: Vec<Row> = Vec::new();
        for rid in 0..*self.next_row_id.get(&right).unwrap_or(&0) {
            let key = row_key(&right, rid);
            if let Some(raw) = self.db.get_raw(&key, &snap) {
                if let Some(row) = decode_row(&raw, &rschema) {
                    right_rows.push(row);
                }
            }
        }

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
            op = Box::new(Filter::new(op, move |r: &Row| eval_predicate(&p, &c, r)));
        }

        // Projection.
        enum Proj2 {
            All,
            Cols(Vec<usize>),
        }
        let (proj, out_cols) =
            if sel.projection.len() == 1 && matches!(sel.projection[0], SelectItem::Star) {
                (
                    Proj2::All,
                    combined.iter().map(|c| c.name.clone()).collect::<Vec<_>>(),
                )
            } else {
                let mut idx = Vec::new();
                let mut names = Vec::new();
                for item in &sel.projection {
                    let SelectItem::Expr(Expr::Identifier(id)) = item else {
                        return Err("only plain column projections supported on joins".into());
                    };
                    let (is_l, p) = find_col(id).ok_or_else(|| format!("unknown column {id}"))?;
                    idx.push(if is_l { p } else { lschema.len() + p });
                    names.push(id.clone());
                }
                (Proj2::Cols(idx), names)
            };

        op = match &proj {
            Proj2::All => op,
            Proj2::Cols(idx) => Box::new(Project::new(op, idx.clone())),
        };

        if max_rows > 0 {
            op = Box::new(Limit::new(op, max_rows));
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
        &mut self,
        sel: &parser::Select,
        schema: &[ColumnDef],
        table: &str,
    ) -> Result<ExecResult, String> {
        let mut key_idx = Vec::with_capacity(sel.group_by.len());
        for e in &sel.group_by {
            let Expr::Identifier(id) = e else {
                return Err("GROUP BY supports plain columns only".into());
            };
            let pos = schema
                .iter()
                .position(|c| c.name == id.as_str())
                .ok_or_else(|| format!("unknown column {id}"))?;
            key_idx.push(pos);
        }

        enum Item {
            Key(usize),
            Agg(Aggregate),
        }
        let mut items = Vec::new();
        let mut out_cols = Vec::new();
        for item in &sel.projection {
            match item {
                SelectItem::Expr(Expr::Function { name, args }) => {
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
                            Some(
                                schema
                                    .iter()
                                    .position(|c| c.name == col_name.as_str())
                                    .ok_or_else(|| format!("unknown column {col_name}"))?,
                            )
                        }
                    };
                    out_cols.push(format!(
                        "{fname}({})",
                        if col.is_none() { "*" } else { "c" }
                    ));
                    items.push(Item::Agg(Aggregate {
                        label: String::new(),
                        func,
                        col,
                    }));
                }
                SelectItem::Expr(Expr::Identifier(id)) => {
                    let pos = schema
                        .iter()
                        .position(|c| c.name == id.as_str())
                        .ok_or_else(|| format!("unknown column {id}"))?;
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

        let (_, snap) = self.db.begin();

        // E4b-2: materialize filtered rows, then run Volcano pipeline.
        let mut raw_rows: Vec<Row> = Vec::new();
        for rid in 0..*self.next_row_id.get(table).unwrap_or(&0) {
            let key = row_key(table, rid);
            if let Some(raw) = self.db.get_raw(&key, &snap) {
                if let Some(full) = decode_row(&raw, schema) {
                    if let Some(pred) = &sel.selection {
                        if !eval_predicate(pred, schema, &full)? {
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

        let mut rows = Vec::with_capacity(32);
        while let Some(r) = op.next()? {
            rows.push(r);
        }

        Ok(ExecResult {
            columns: out_cols,
            rows,
            rows_affected: 0,
        })
    }
}

// == Expression evaluation ====================================================

fn eval_predicate(pred: &Expr, schema: &[ColumnDef], row: &[SqlValue]) -> Result<bool, String> {
    match pred {
        Expr::BinaryOp { left, op, right } if *op == BinOp::And => {
            Ok(eval_predicate(left, schema, row)? && eval_predicate(right, schema, row)?)
        }
        Expr::BinaryOp { left, op, right } => {
            let lhs = col_value(left, schema, row)?;
            let rhs = literal_value(right)?;
            Ok(match op {
                BinOp::Eq => *lhs == rhs,
                BinOp::NotEq => *lhs != rhs,
                BinOp::Lt => compare(lhs, &rhs) == std::cmp::Ordering::Less,
                BinOp::LtEq => compare(lhs, &rhs) != std::cmp::Ordering::Greater,
                BinOp::Gt => compare(lhs, &rhs) == std::cmp::Ordering::Greater,
                BinOp::GtEq => compare(lhs, &rhs) != std::cmp::Ordering::Less,
                BinOp::And => unreachable!(),
            })
        }
        other => Err(format!("unsupported predicate {other:?}")),
    }
}

fn compare(a: &SqlValue, b: &SqlValue) -> std::cmp::Ordering {
    match (a, b) {
        (SqlValue::Int(x), SqlValue::Int(y)) => x.cmp(y),
        (SqlValue::Text(x), SqlValue::Text(y)) => x.cmp(y),
        _ => std::cmp::Ordering::Equal,
    }
}

fn col_value<'a>(
    expr: &'a Expr,
    schema: &[ColumnDef],
    row: &'a [SqlValue],
) -> Result<&'a SqlValue, String> {
    let Expr::Identifier(id) = expr else {
        return Err("left side of predicate must be a column".into());
    };
    let pos = schema
        .iter()
        .position(|c| c.name == id.as_str())
        .ok_or_else(|| format!("unknown column {id}"))?;
    row.get(pos).ok_or("column index out of range".to_string())
}

fn literal_value(expr: &Expr) -> Result<SqlValue, String> {
    match expr {
        Expr::Literal(v) => Ok(v.clone()),
        other => Err(format!("unsupported literal {other:?}")),
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
        let SelectItem::Expr(Expr::Function { name, args }) = item else {
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
                let pos = schema
                    .iter()
                    .position(|c| c.name == col_name.as_str())
                    .ok_or_else(|| format!("unknown column {col_name}"))?;
                Some(pos)
            }
        };
        let label = format!("{fname}({})", if col.is_none() { "*" } else { "c" });
        out.push(Aggregate { label, func, col });
    }
    Ok(Some(out))
}
