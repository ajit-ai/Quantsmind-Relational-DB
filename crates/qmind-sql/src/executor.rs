//! E4 — Volcano iterator model.
//!
//! Every operator implements `next()` pulling one row at a time from its
//! child, enabling streaming with bounded memory (except hash build sides
//! and aggregation, which materialize). The engine's ad-hoc select/join/
//! group paths refactor onto these operators; existing SQL e2e tests are
//! the acceptance gate for that wiring (E4b).

use crate::codec::SqlValue;
use std::collections::HashMap;

pub type Row = Vec<SqlValue>;

pub trait Operator {
    /// Next output row, or None when exhausted.
    fn next(&mut self) -> Result<Option<Row>, String>;
}

/// Materialized scan (tests + small tables). Streaming storage scans wrap a
/// closure instead.
pub struct VecScan {
    rows: std::vec::IntoIter<Row>,
}

impl VecScan {
    pub fn new(rows: Vec<Row>) -> Self {
        Self {
            rows: rows.into_iter(),
        }
    }
}

impl Operator for VecScan {
    fn next(&mut self) -> Result<Option<Row>, String> {
        Ok(self.rows.next())
    }
}

/// Pull-based scan over any closure — the storage-facing shape.
pub struct Scan<F>
where
    F: FnMut() -> Result<Option<Row>, String>,
{
    f: F,
}

impl<F> Scan<F>
where
    F: FnMut() -> Result<Option<Row>, String>,
{
    pub fn new(f: F) -> Self {
        Self { f }
    }
}

impl<F> Operator for Scan<F>
where
    F: FnMut() -> Result<Option<Row>, String>,
{
    fn next(&mut self) -> Result<Option<Row>, String> {
        (self.f)()
    }
}

pub struct Filter<F>
where
    F: Fn(&Row) -> Result<bool, String>,
{
    input: Box<dyn Operator>,
    pred: F,
}

impl<F> Filter<F>
where
    F: Fn(&Row) -> Result<bool, String>,
{
    pub fn new(input: Box<dyn Operator>, pred: F) -> Self {
        Self { input, pred }
    }
}

impl<F> Operator for Filter<F>
where
    F: Fn(&Row) -> Result<bool, String>,
{
    fn next(&mut self) -> Result<Option<Row>, String> {
        loop {
            match self.input.next()? {
                None => return Ok(None),
                Some(r) => {
                    if (self.pred)(&r)? {
                        return Ok(Some(r));
                    }
                }
            }
        }
    }
}

pub struct Project {
    input: Box<dyn Operator>,
    idx: Vec<usize>,
}

impl Project {
    pub fn new(input: Box<dyn Operator>, idx: Vec<usize>) -> Self {
        Self { input, idx }
    }
}

impl Operator for Project {
    fn next(&mut self) -> Result<Option<Row>, String> {
        Ok(self
            .input
            .next()?
            .map(|r| self.idx.iter().map(|&i| r[i].clone()).collect()))
    }
}

pub struct Limit {
    input: Box<dyn Operator>,
    remaining: usize,
}

impl Limit {
    pub fn new(input: Box<dyn Operator>, n: usize) -> Self {
        Self {
            input,
            remaining: n,
        }
    }
}

impl Operator for Limit {
    fn next(&mut self) -> Result<Option<Row>, String> {
        if self.remaining == 0 {
            return Ok(None);
        }
        let r = self.input.next()?;
        if r.is_some() {
            self.remaining -= 1;
        }
        Ok(r)
    }
}

/// Build-side hash join: materializes the RIGHT child, probes with LEFT.
pub struct HashJoin {
    left: Box<dyn Operator>,
    hash: HashMap<SqlValue, Vec<Row>>,
    pending: Vec<Row>,
    lkey: usize,
    built: bool,
    lcols: usize,
}

impl HashJoin {
    pub fn new(
        left: Box<dyn Operator>,
        mut right: Box<dyn Operator>,
        lkey: usize,
        rkey: usize,
        lcols: usize,
    ) -> Result<Self, String> {
        let mut hash: HashMap<SqlValue, Vec<Row>> = HashMap::new();
        while let Some(r) = right.next()? {
            if r[rkey] == SqlValue::Null {
                continue;
            }
            hash.entry(r[rkey].clone()).or_default().push(r);
        }
        Ok(Self {
            left,
            hash,
            pending: Vec::new(),
            lkey,
            built: true,
            lcols,
        })
    }
}

impl Operator for HashJoin {
    fn next(&mut self) -> Result<Option<Row>, String> {
        debug_assert!(self.built);
        loop {
            if let Some(r) = self.pending.pop() {
                return Ok(Some(r));
            }
            let Some(lrow) = self.left.next()? else {
                return Ok(None);
            };
            if lrow[self.lkey] == SqlValue::Null {
                continue;
            }
            if let Some(matches) = self.hash.get(&lrow[self.lkey]) {
                for rrow in matches {
                    let mut out = lrow.clone();
                    out.extend(rrow.iter().cloned());
                    self.pending.push(out);
                }
                self.pending.reverse(); // preserve left-then-right order
                let _ = self.lcols;
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggFn {
    Count,
    Sum,
    Min,
    Max,
}

/// GROUP BY + aggregates. Materializes on first next(), emits one row per
/// group ordered by group key (BTreeMap).
pub struct HashAggregate {
    input: Box<dyn Operator>,
    group_idx: Vec<usize>,
    aggs: Vec<(AggFn, Option<usize>)>,
    groups: Option<std::collections::BTreeMap<Vec<SqlValue>, Vec<Row>>>,
    keys: std::vec::IntoIter<Vec<SqlValue>>,
}

impl HashAggregate {
    pub fn new(
        input: Box<dyn Operator>,
        group_idx: Vec<usize>,
        aggs: Vec<(AggFn, Option<usize>)>,
    ) -> Self {
        Self {
            input,
            group_idx,
            aggs,
            groups: None,
            keys: Vec::new().into_iter(),
        }
    }

    fn evaluate(&self, bucket: &[Row]) -> Result<Row, String> {
        let mut out = Vec::with_capacity(self.group_idx.len() + self.aggs.len());
        for (f, col) in &self.aggs {
            let vals: Vec<SqlValue> = match *col {
                None => vec![SqlValue::Int(bucket.len() as i64)],
                Some(i) => bucket
                    .iter()
                    .map(|r| r[i].clone())
                    .filter(|v| *v != SqlValue::Null)
                    .collect(),
            };
            out.push(match f {
                AggFn::Count => SqlValue::Int(if col.is_none() {
                    bucket.len() as i64
                } else {
                    vals.len() as i64
                }),
                AggFn::Sum => {
                    let mut acc = 0i64;
                    for v in &vals {
                        match v {
                            SqlValue::Int(i) => acc += i,
                            _ => return Err("SUM requires INTEGER".into()),
                        }
                    }
                    SqlValue::Int(acc)
                }
                AggFn::Min | AggFn::Max => {
                    let want_min = *f == AggFn::Min;
                    let mut best: Option<&SqlValue> = None;
                    for v in &vals {
                        best = Some(match best {
                            None => v,
                            Some(b) => {
                                let ord = match (b, v) {
                                    (SqlValue::Int(a), SqlValue::Int(x)) => a.cmp(x),
                                    (SqlValue::Text(a), SqlValue::Text(x)) => a.cmp(x),
                                    _ => std::cmp::Ordering::Equal,
                                };
                                if (want_min && ord == std::cmp::Ordering::Greater)
                                    || (!want_min && ord == std::cmp::Ordering::Less)
                                {
                                    v
                                } else {
                                    b
                                }
                            }
                        });
                    }
                    best.cloned().unwrap_or(SqlValue::Null)
                }
            });
        }
        Ok(out)
    }
}

impl Operator for HashAggregate {
    fn next(&mut self) -> Result<Option<Row>, String> {
        if self.groups.is_none() {
            let mut groups: std::collections::BTreeMap<Vec<SqlValue>, Vec<Row>> =
                std::collections::BTreeMap::new();
            while let Some(r) = self.input.next()? {
                let k: Vec<SqlValue> = self.group_idx.iter().map(|&i| r[i].clone()).collect();
                groups.entry(k).or_default().push(r);
            }
            self.keys = groups.keys().cloned().collect::<Vec<_>>().into_iter();
            self.groups = Some(groups);
        }
        let Some(k) = self.keys.next() else {
            return Ok(None);
        };
        let bucket = self.groups.as_ref().expect("materialized")[&k].clone();
        let mut out: Vec<SqlValue> = k;
        out.extend(self.evaluate(&bucket)?);
        Ok(Some(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<Row> {
        (0..20u64)
            .map(|i| vec![SqlValue::Int(i as i64), SqlValue::Text(format!("row{i}"))])
            .collect()
    }

    #[test]
    fn scan_filter_project_limit_pipeline() -> Result<(), String> {
        let mut op = Limit::new(
            Box::new(Project::new(
                Box::new(Filter::new(Box::new(VecScan::new(sample())), |r| {
                    Ok(r[0] >= SqlValue::Int(15))
                })),
                vec![1],
            )),
            2,
        );
        assert_eq!(op.next()?, Some(vec![SqlValue::Text("row15".into())]));
        assert_eq!(op.next()?, Some(vec![SqlValue::Text("row16".into())]));
        assert_eq!(op.next()?, None);
        Ok(())
    }

    #[test]
    fn closure_scan_streams_from_source() -> Result<(), String> {
        let mut n = 0;
        let mut op = Scan::new(move || {
            n += 1;
            Ok((n <= 3).then(|| vec![SqlValue::Int(n)]))
        });
        assert_eq!(op.next()?, Some(vec![SqlValue::Int(1)]));
        assert_eq!(op.next()?, Some(vec![SqlValue::Int(2)]));
        assert_eq!(op.next()?, Some(vec![SqlValue::Int(3)]));
        assert_eq!(op.next()?, None);
        Ok(())
    }

    #[test]
    fn hash_join_fanout_and_null_rejection() -> Result<(), String> {
        let left = vec![
            vec![SqlValue::Int(1), SqlValue::Text("a".into())],
            vec![SqlValue::Int(2), SqlValue::Text("b".into())],
            vec![SqlValue::Null, SqlValue::Text("n".into())],
        ];
        let right = vec![
            vec![SqlValue::Int(1), SqlValue::Int(100)],
            vec![SqlValue::Int(1), SqlValue::Int(200)],
            vec![SqlValue::Int(9), SqlValue::Int(900)],
        ];
        let mut op = HashJoin::new(
            Box::new(VecScan::new(left)),
            Box::new(VecScan::new(right)),
            0,
            0,
            2,
        )?;
        let mut got = Vec::new();
        while let Some(r) = op.next()? {
            got.push(r);
        }
        assert_eq!(got.len(), 2, "1 matches twice; 2, NULL unmatched");
        assert_eq!(got[0][1], SqlValue::Text("a".into()));
        assert_eq!(got[0][3], SqlValue::Int(100));
        Ok(())
    }

    #[test]
    fn hash_aggregate_group_by_with_count_sum() -> Result<(), String> {
        let rows: Vec<Row> = (1..=8u64)
            .map(|i| {
                vec![
                    SqlValue::Int(i as i64),
                    SqlValue::Text(format!("g{}", i % 2)),
                ]
            })
            .collect();
        let mut op = HashAggregate::new(
            Box::new(VecScan::new(rows)),
            vec![1],
            vec![(AggFn::Count, None), (AggFn::Sum, Some(0))],
        );
        let r1 = op.next()?.expect("g0");
        assert_eq!(r1[0], SqlValue::Text("g0".into()));
        assert_eq!(r1[1], SqlValue::Int(4));
        assert_eq!(r1[2], SqlValue::Int(20)); // 2+4+6+8
        let r2 = op.next()?.expect("g1");
        assert_eq!(r2[1], SqlValue::Int(4));
        assert_eq!(r2[2], SqlValue::Int(16)); // 1+3+5+7
        assert_eq!(op.next()?, None);
        Ok(())
    }
}
