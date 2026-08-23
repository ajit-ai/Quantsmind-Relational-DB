//! M3 SQL engine: parse → plan-lite → execute over the MVCC kernel.
//!
//! Supported surface (grows per ROADMAP.md):
//! - CREATE TABLE t (col TYPE [NOT NULL], ...)
//! - INSERT INTO t VALUES (..), (..)
//! - SELECT cols | * FROM t [WHERE cond] [LIMIT n]
//!   predicates: col op literal chained with AND; ops = != < <= > >=

use crate::codec::{decode_row, encode_row, row_key, ColumnDef, ColumnType, SqlValue};
use qmind_kernel::{MvccStore, WalWriter};
use sqlparser::ast::{
    BinaryOperator, Expr, JoinConstraint, ObjectName, Statement, Value as SqlParserValue,
};
use sqlparser::parser::Parser;
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
        let stmts = Parser::parse_sql(&sqlparser::dialect::GenericDialect {}, sql)
            .map_err(|e| format!("syntax error: {e}"))?;
        if stmts.len() != 1 {
            return Err(format!(
                "expected exactly one statement, got {}",
                stmts.len()
            ));
        }
        match &stmts[0] {
            Statement::CreateTable(ct) => self.create_table(ct),
            Statement::Insert(ins) => self.insert(ins),
            Statement::Query(q) => self.select(q),
            other => Err(format!("unsupported statement: {other:?}")),
        }
    }

    fn create_table(&mut self, ct: &sqlparser::ast::CreateTable) -> Result<ExecResult, String> {
        let name = object_name(&ct.name)?;
        if self.tables.contains_key(&name) {
            if ct.if_not_exists {
                return Ok(ExecResult::empty());
            }
            return Err(format!("table `{name}` already exists"));
        }
        let mut cols = Vec::new();
        for col in &ct.columns {
            let ty = match &col.data_type {
                sqlparser::ast::DataType::Integer(_)
                | sqlparser::ast::DataType::BigInt(_)
                | sqlparser::ast::DataType::Int(_) => ColumnType::Int,
                sqlparser::ast::DataType::Text
                | sqlparser::ast::DataType::Varchar(_)
                | sqlparser::ast::DataType::String(_) => ColumnType::Text,
                other => {
                    return Err(format!(
                        "unsupported type {other:?} for column {}",
                        col.name
                    ))
                }
            };
            let not_null = col
                .options
                .iter()
                .any(|o| matches!(o.option, sqlparser::ast::ColumnOption::NotNull));
            cols.push(ColumnDef {
                name: col.name.value.clone(),
                ty,
                nullable: !not_null,
            });
        }
        self.tables.insert(name.clone(), cols);
        self.next_row_id.entry(name).or_insert(0);
        Ok(ExecResult::empty())
    }

    fn insert(&mut self, ins: &sqlparser::ast::Insert) -> Result<ExecResult, String> {
        let table = object_name(&ins.table_name)?;
        let schema = self
            .tables
            .get(&table)
            .cloned()
            .ok_or_else(|| format!("no table `{table}`"))?;

        let rows_data = match ins.source.as_ref().map(|b| b.body.as_ref()) {
            Some(sqlparser::ast::SetExpr::Values(v)) => &v.rows,
            _ => return Err("only VALUES inserts supported".into()),
        };

        let (txn, _snap) = self.db.begin();
        let start_id = *self.next_row_id.entry(table.clone()).or_insert(0);
        let mut count = 0u64;
        for row_expr in rows_data {
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
            self.db.set(txn, &row_key(&table, rid), encode_row(&row));
            count += 1;
        }
        *self.next_row_id.get_mut(&table).unwrap() += count;

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

    fn select(&mut self, q: &sqlparser::ast::Query) -> Result<ExecResult, String> {
        let body = match q.body.as_ref() {
            sqlparser::ast::SetExpr::Select(sel) => sel,
            _ => return Err("only simple SELECT supported".into()),
        };

        // M4c: two-table INNER JOIN ... ON equi-condition → hash join.
        if !body.from.is_empty() && !body.from[0].joins.is_empty() {
            return self.select_join(body, &q.limit);
        }

        let table = match body.from.first() {
            Some(f) => match &f.relation {
                sqlparser::ast::TableFactor::Table { name, .. } => object_name(name)?,
                other => return Err(format!("unsupported FROM clause {other:?}")),
            },
            None => return Err("SELECT requires FROM".into()),
        };
        let schema = self
            .tables
            .get(&table)
            .cloned()
            .ok_or_else(|| format!("no table `{table}`"))?;

        // GROUP BY path: aggregates + grouping columns bucketed per key.
        if let sqlparser::ast::GroupByExpr::Expressions(exprs, _) = &body.group_by {
            if !exprs.is_empty() {
                return self.select_group_by(body, exprs, &schema, &table);
            }
        }

        // Aggregate fast path: SELECT COUNT(*)|COUNT(c)|SUM|AVG|MIN|MAX(c)
        // [WHERE cond].
        if let Some(aggs) = try_parse_aggregates(&body.projection, &schema)? {
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
                match &body.selection {
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
        let (proj, out_cols) = if body.projection.len() == 1
            && matches!(body.projection[0], sqlparser::ast::SelectItem::Wildcard(_))
        {
            (
                Proj::All,
                schema.iter().map(|c| c.name.clone()).collect::<Vec<_>>(),
            )
        } else {
            let mut idx = Vec::new();
            let mut names = Vec::new();
            for item in &body.projection {
                let sqlparser::ast::SelectItem::UnnamedExpr(Expr::Identifier(id)) = item else {
                    return Err("only plain column projections supported".into());
                };
                let pos = schema
                    .iter()
                    .position(|c| c.name == id.value)
                    .ok_or_else(|| format!("unknown column {}", id.value))?;
                idx.push(pos);
                names.push(id.value.clone());
            }
            (Proj::Cols(idx), names)
        };

        // Scan → filter under a single MVCC snapshot.
        let (_, snap) = self.db.begin();
        let max_rows = limit_literal(&q.limit)?;
        let mut rows = Vec::new();
        'scan: for rid in 0..*self.next_row_id.get(&table).unwrap_or(&0) {
            if max_rows > 0 && rows.len() >= max_rows {
                break 'scan;
            }
            let key = row_key(&table, rid);
            let Some(raw) = self.db.get_raw(&key, &snap) else {
                continue;
            };
            let Some(full) = decode_row(&raw, &schema) else {
                continue;
            };
            if let Some(pred) = &body.selection {
                if !eval_predicate(pred, &schema, &full)? {
                    continue;
                }
            }
            rows.push(match &proj {
                Proj::All => full,
                Proj::Cols(idx) => idx.iter().map(|&i| full[i].clone()).collect(),
            });
        }

        Ok(ExecResult {
            columns: out_cols,
            rows,
            rows_affected: 0,
        })
    }

    /// M4c: hash join for left INNER JOIN right ON lcol = rcol.
    /// Builds a hash table over the right side, probes with left rows,
    /// then applies WHERE / projection / LIMIT on the concatenated rows.
    fn select_join(
        &mut self,
        body: &sqlparser::ast::Select,
        limit: &Option<Expr>,
    ) -> Result<ExecResult, String> {
        let left_rel = &body.from[0].relation;
        let (lt, rt) = match (&left_rel, body.from[0].joins.first().map(|j| &j.relation)) {
            (
                sqlparser::ast::TableFactor::Table { name: ln, .. },
                Some(sqlparser::ast::TableFactor::Table { name: rn, .. }),
            ) => (object_name(ln)?, object_name(rn)?),
            _ => return Err("JOIN supports plain tables only".into()),
        };
        if lt == rt {
            return Err("self-joins unsupported".into());
        }
        let lschema = self
            .tables
            .get(&lt)
            .cloned()
            .ok_or_else(|| format!("no table `{lt}`"))?;
        let rschema = self
            .tables
            .get(&rt)
            .cloned()
            .ok_or_else(|| format!("no table `{rt}`"))?;

        // Join predicate must be exactly col = col.
        let on = match &body.from[0].joins[0].join_operator {
            sqlparser::ast::JoinOperator::Inner(JoinConstraint::On(e)) => Ok(e.clone()),
            sqlparser::ast::JoinOperator::Inner(other) => {
                Err(format!("unsupported JOIN constraint {other:?}"))
            }
            other => Err(format!("only INNER JOIN supported, got {other:?}")),
        }?;
        let Expr::BinaryOp {
            left,
            op: BinaryOperator::Eq,
            right,
        } = on
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
            find_col(&id.value).ok_or_else(|| format!("unknown column {}", id.value))
        };
        let (lside, rside) = {
            let a = resolve_side(&left)?;
            let b = resolve_side(&right)?;
            match (a.0, b.0) {
                (true, false) => ((a.1 as u8, 0u8), (b.1 as u8, 1u8)),
                (false, true) => ((b.1 as u8, 0u8), (a.1 as u8, 1u8)),
                _ => return Err("JOIN ON must span both tables".into()),
            }
        };
        let (li, ri) = (lside.0 as usize, rside.0 as usize);

        // Build phase: hash right rows by join key.
        let (_, snap) = self.db.begin();
        let mut hash: std::collections::HashMap<SqlValue, Vec<Vec<SqlValue>>> =
            std::collections::HashMap::new();
        for rid in 0..*self.next_row_id.get(&rt).unwrap_or(&0) {
            let key = row_key(&rt, rid);
            let Some(raw) = self.db.get_raw(&key, &snap) else {
                continue;
            };
            let Some(row) = decode_row(&raw, &rschema) else {
                continue;
            };
            if row[ri] == SqlValue::Null {
                continue;
            }
            hash.entry(row[ri].clone()).or_default().push(row);
        }

        // Probe phase.
        let mut joined: Vec<Vec<SqlValue>> = Vec::new();
        for rid in 0..*self.next_row_id.get(&lt).unwrap_or(&0) {
            let key = row_key(&lt, rid);
            let Some(raw) = self.db.get_raw(&key, &snap) else {
                continue;
            };
            let Some(lrow) = decode_row(&raw, &lschema) else {
                continue;
            };
            if lrow[li] == SqlValue::Null {
                continue;
            }
            if let Some(matches) = hash.get(&lrow[li]) {
                for rrow in matches {
                    joined.push(lrow.iter().chain(rrow.iter()).cloned().collect());
                }
            }
        }

        // WHERE on combined row.
        if let Some(pred) = &body.selection {
            joined.retain(|r| eval_predicate(pred, &combined, r).unwrap_or(false));
        }

        // Projection.
        enum Proj2 {
            All,
            Cols(Vec<usize>),
        }
        let (proj, out_cols) = if body.projection.len() == 1
            && matches!(body.projection[0], sqlparser::ast::SelectItem::Wildcard(_))
        {
            (
                Proj2::All,
                combined.iter().map(|c| c.name.clone()).collect::<Vec<_>>(),
            )
        } else {
            let mut idx = Vec::new();
            let mut names = Vec::new();
            for item in &body.projection {
                let sqlparser::ast::SelectItem::UnnamedExpr(Expr::Identifier(id)) = item else {
                    return Err("only plain column projections supported on joins".into());
                };
                let (is_l, p) =
                    find_col(&id.value).ok_or_else(|| format!("unknown column {}", id.value))?;
                idx.push(if is_l { p } else { lschema.len() + p });
                names.push(id.value.clone());
            }
            (Proj2::Cols(idx), names)
        };

        let max_rows = limit_literal(limit)?;
        let mut out_rows = Vec::new();
        for full in joined {
            if max_rows > 0 && out_rows.len() >= max_rows {
                break;
            }
            out_rows.push(match &proj {
                Proj2::All => full,
                Proj2::Cols(idx) => idx.iter().map(|&i| full[i].clone()).collect(),
            });
        }

        Ok(ExecResult {
            columns: out_cols,
            rows: out_rows,
            rows_affected: 0,
        })
    }
    /// GROUP BY execution: bucket filtered rows by grouping-key tuple, then
    /// evaluate each aggregate per bucket. Output is ordered by group key
    /// (BTreeMap) — deterministic without an explicit ORDER BY.
    fn select_group_by(
        &mut self,
        body: &sqlparser::ast::Select,
        exprs: &[Expr],
        schema: &[ColumnDef],
        table: &str,
    ) -> Result<ExecResult, String> {
        use sqlparser::ast::{FunctionArg, FunctionArgExpr};

        let mut key_idx = Vec::with_capacity(exprs.len());
        for e in exprs {
            let Expr::Identifier(id) = e else {
                return Err("GROUP BY supports plain columns only".into());
            };
            let pos = schema
                .iter()
                .position(|c| c.name == id.value)
                .ok_or_else(|| format!("unknown column {}", id.value))?;
            key_idx.push(pos);
        }

        enum Item {
            Key(usize),
            Agg(Aggregate),
        }
        let mut items = Vec::new();
        let mut out_cols = Vec::new();
        for item in &body.projection {
            let sqlparser::ast::SelectItem::UnnamedExpr(e) = item else {
                return Err("GROUP BY projection must be plain expressions".into());
            };
            match e {
                Expr::Function(f) => {
                    let fname = f.name.to_string().to_uppercase();
                    let func = match fname.as_str() {
                        "COUNT" => AggFn::Count,
                        "SUM" => AggFn::Sum,
                        "AVG" => AggFn::Avg,
                        "MIN" => AggFn::Min,
                        "MAX" => AggFn::Max,
                        other => return Err(format!("unsupported function {other}")),
                    };
                    let arg0: Option<FunctionArgExpr> = match &f.args {
                        sqlparser::ast::FunctionArguments::None => None,
                        sqlparser::ast::FunctionArguments::List(l) => {
                            l.args.first().map(|a| match a {
                                FunctionArg::Unnamed(n) => n.clone(),
                                FunctionArg::Named { arg, .. } => arg.clone(),
                            })
                        }
                        sqlparser::ast::FunctionArguments::Subquery(_) => {
                            return Err(format!("{fname}(subquery) unsupported"));
                        }
                    };
                    let col = match arg0 {
                        Some(FunctionArgExpr::Wildcard) => None,
                        Some(FunctionArgExpr::Expr(Expr::Identifier(id))) => Some(
                            schema
                                .iter()
                                .position(|c| c.name == id.value)
                                .ok_or_else(|| format!("unknown column {}", id.value))?,
                        ),
                        _ => return Err(format!("{fname} requires a single argument")),
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
                Expr::Identifier(id) => {
                    let pos = schema
                        .iter()
                        .position(|c| c.name == id.value)
                        .ok_or_else(|| format!("unknown column {}", id.value))?;
                    if !key_idx.contains(&pos) {
                        return Err(format!(
                            "column {} must appear in GROUP BY or be aggregated",
                            id.value
                        ));
                    }
                    out_cols.push(id.value.clone());
                    items.push(Item::Key(pos));
                }
                other => return Err(format!("unsupported GROUP BY projection {other:?}")),
            }
        }

        let (_, snap) = self.db.begin();
        let mut buckets: std::collections::BTreeMap<Vec<SqlValue>, Vec<Vec<SqlValue>>> =
            std::collections::BTreeMap::new();
        for rid in 0..*self.next_row_id.get(table).unwrap_or(&0) {
            let key = row_key(table, rid);
            let Some(raw) = self.db.get_raw(&key, &snap) else {
                continue;
            };
            let Some(full) = decode_row(&raw, schema) else {
                continue;
            };
            if let Some(pred) = &body.selection {
                if !eval_predicate(pred, schema, &full)? {
                    continue;
                }
            }
            let gkey: Vec<SqlValue> = key_idx.iter().map(|&i| full[i].clone()).collect();
            buckets.entry(gkey).or_default().push(full);
        }

        let mut rows = Vec::with_capacity(buckets.len());
        for (_gk, bucket) in buckets {
            let mut out = Vec::with_capacity(items.len());
            for item in &items {
                match item {
                    Item::Key(i) => out.push(bucket[0][*i].clone()),
                    Item::Agg(a) => out.push(a.evaluate(&bucket)?),
                }
            }
            rows.push(out);
        }

        Ok(ExecResult {
            columns: out_cols,
            rows,
            rows_affected: 0,
        })
    }
}

fn eval_predicate(pred: &Expr, schema: &[ColumnDef], row: &[SqlValue]) -> Result<bool, String> {
    match pred {
        Expr::BinaryOp { left, op, right } if *op == BinaryOperator::And => {
            Ok(eval_predicate(left, schema, row)? && eval_predicate(right, schema, row)?)
        }
        Expr::BinaryOp { left, op, right } => {
            let lhs = col_value(left, schema, row)?;
            let rhs = literal_value(right)?;
            Ok(match op {
                BinaryOperator::Eq => *lhs == rhs,
                BinaryOperator::NotEq => *lhs != rhs,
                BinaryOperator::Lt => compare(lhs, &rhs) == std::cmp::Ordering::Less,
                BinaryOperator::LtEq => compare(lhs, &rhs) != std::cmp::Ordering::Greater,
                BinaryOperator::Gt => compare(lhs, &rhs) == std::cmp::Ordering::Greater,
                BinaryOperator::GtEq => compare(lhs, &rhs) != std::cmp::Ordering::Less,
                other => return Err(format!("unsupported operator {other:?}")),
            })
        }
        Expr::Nested(e) => eval_predicate(e, schema, row),
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
        .position(|c| c.name == id.value)
        .ok_or_else(|| format!("unknown column {}", id.value))?;
    row.get(pos).ok_or("column index out of range".to_string())
}

fn literal_value(expr: &Expr) -> Result<SqlValue, String> {
    match expr {
        Expr::Value(SqlParserValue::Number(n, _)) => n
            .parse::<i64>()
            .map(SqlValue::Int)
            .map_err(|_| format!("{n} is not an INTEGER")),
        Expr::Value(SqlParserValue::SingleQuotedString(s)) => Ok(SqlValue::Text(s.clone())),
        Expr::Value(SqlParserValue::Null) => Ok(SqlValue::Null),
        other => Err(format!("unsupported literal {other:?}")),
    }
}

fn limit_literal(limit: &Option<Expr>) -> Result<usize, String> {
    match limit {
        None => Ok(0), // 0 = unlimited
        Some(e) => match literal_value(e)? {
            SqlValue::Int(n) if n >= 0 => Ok(n as usize),
            _ => Err("LIMIT must be a non-negative integer".into()),
        },
    }
}

fn object_name(n: &ObjectName) -> Result<String, String> {
    let parts: Vec<_> = n.0.iter().map(|p| p.value.clone()).collect();
    if parts.len() != 1 {
        return Err(format!("qualified names unsupported: {}", parts.join(".")));
    }
    Ok(parts.into_iter().next().expect("non-empty"))
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

/// One aggregate projection; col is a pre-bound schema index
/// (None = COUNT(*)).
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
    projection: &[sqlparser::ast::SelectItem],
    schema: &[ColumnDef],
) -> Result<Option<Vec<Aggregate>>, String> {
    use sqlparser::ast::{FunctionArg, FunctionArgExpr};
    let mut out = Vec::with_capacity(projection.len());
    for item in projection {
        let sqlparser::ast::SelectItem::UnnamedExpr(e) = item else {
            return Ok(None);
        };
        let Expr::Function(f) = e else {
            return Ok(None);
        };
        let fname = f.name.to_string().to_uppercase();
        let func = match fname.as_str() {
            "COUNT" => AggFn::Count,
            "SUM" => AggFn::Sum,
            "AVG" => AggFn::Avg,
            "MIN" => AggFn::Min,
            "MAX" => AggFn::Max,
            _ => continue,
        };
        let arg0: Option<FunctionArgExpr> = match &f.args {
            sqlparser::ast::FunctionArguments::None => None,
            sqlparser::ast::FunctionArguments::Subquery(_) => {
                return Err(format!("{fname}(subquery) unsupported"));
            }
            sqlparser::ast::FunctionArguments::List(l) => l.args.first().map(|a| match a {
                FunctionArg::Unnamed(n) => n.clone(),
                FunctionArg::Named { arg, .. } => arg.clone(),
            }),
        };
        let col = match arg0 {
            Some(FunctionArgExpr::Wildcard) => None,
            Some(FunctionArgExpr::Expr(Expr::Identifier(id))) => {
                let pos = schema
                    .iter()
                    .position(|c| c.name == id.value)
                    .ok_or_else(|| format!("unknown column {}", id.value))?;
                Some(pos)
            }
            _ => return Err(format!("{fname} requires a single argument")),
        };
        let label = format!("{fname}({})", if col.is_none() { "*" } else { "c" });
        out.push(Aggregate { label, func, col });
    }
    Ok(Some(out))
}
