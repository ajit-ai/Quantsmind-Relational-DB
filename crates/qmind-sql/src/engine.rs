//! M3 SQL engine: parse → plan-lite → execute over the MVCC kernel.
//!
//! Supported surface (grows per ROADMAP.md):
//! - CREATE TABLE t (col TYPE [NOT NULL], ...)
//! - INSERT INTO t VALUES (..), (..)
//! - SELECT cols | * FROM t [WHERE cond] [LIMIT n]
//!   predicates: col op literal chained with AND; ops = != < <= > >=

use crate::codec::{decode_row, encode_row, row_key, ColumnDef, ColumnType, SqlValue};
use qmind_kernel::{MvccStore, WalWriter};
use sqlparser::ast::{BinaryOperator, Expr, ObjectName, Statement, Value as SqlParserValue};
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
