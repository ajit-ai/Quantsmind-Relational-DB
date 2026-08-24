//! # qmind-sql
//!
//! Parser → plan-lite → executor over the MVCC kernel.
//!
//! - M3: single-table DDL/DML + scan/filter/project, columnar batches (~2048 rows)
//! - M4: joins, aggregates, secondary indexes, OLTP/OLAP plan routing (D-001)

pub mod codec;
pub mod engine;
pub mod executor;

pub use codec::{ColumnDef, ColumnType, SqlValue};
pub use engine::{Engine, ExecResult};

/// Rows per execution batch. Tunable; 2048 balances SIMD utilization against
/// cache footprint. Frozen as a contract for M3 benchmarks.
pub const BATCH_ROWS: usize = 2048;

#[derive(Debug)]
pub struct EngineInfo {
    pub name: &'static str,
    pub version: &'static str,
}

pub const ENGINE: EngineInfo = EngineInfo {
    name: "quantsmind",
    version: env!("CARGO_PKG_VERSION"),
};
