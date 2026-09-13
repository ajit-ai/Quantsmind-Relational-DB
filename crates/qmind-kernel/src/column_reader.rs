//! Columnar reader — reads all segments for a table and produces rows.
//! Used by the OLAP scan path (M8c).

use crate::column_delta::{list_segments, TableSchema};
use crate::columnar::{open_column_segment, ColValue, ColumnSegment};
use crate::error::Result;
use std::path::Path;

/// Reads all columnar segments for a table and produces row-oriented output.
pub struct ColumnarReader {
    segments: Vec<ColumnSegment>,
    schema: TableSchema,
}

impl ColumnarReader {
    /// Open all segments for a table from its directory.
    pub fn open(table_dir: &Path, schema: TableSchema) -> Result<Self> {
        let seg_paths = list_segments(table_dir)?;
        let mut segments = Vec::with_capacity(seg_paths.len());
        for path in seg_paths {
            segments.push(open_column_segment(&path)?);
        }
        Ok(Self { segments, schema })
    }

    /// Total rows across all segments.
    pub fn total_rows(&self) -> usize {
        self.segments.iter().map(|s| s.num_rows() as usize).sum()
    }

    /// Number of columns.
    pub fn num_columns(&self) -> usize {
        self.schema.columns.len()
    }

    /// Column names.
    pub fn column_names(&self) -> Vec<String> {
        self.schema.columns.iter().map(|c| c.name.clone()).collect()
    }

    /// Read all rows from all segments, returning column-oriented `ColValue`s.
    pub fn read_all_rows(&self) -> Result<Vec<Vec<ColValue>>> {
        let mut all_rows = Vec::with_capacity(self.total_rows());
        for seg in &self.segments {
            let num_rows = seg.num_rows() as usize;
            let num_cols = seg.num_columns() as usize;
            if num_cols == 0 || num_rows == 0 {
                continue;
            }

            // Decode all columns from this segment.
            let mut columns: Vec<Vec<ColValue>> = Vec::with_capacity(num_cols);
            for col_idx in 0..num_cols {
                columns.push(seg.decode_column(col_idx)?);
            }

            // Transpose column-oriented to row-oriented.
            for row_idx in 0..num_rows {
                let row: Vec<ColValue> = columns.iter().map(|col| col[row_idx].clone()).collect();
                all_rows.push(row);
            }
        }
        Ok(all_rows)
    }

    /// Read rows with a simple predicate filter.
    pub fn read_filtered<F>(&self, pred: F) -> Result<Vec<Vec<ColValue>>>
    where
        F: Fn(&[ColValue]) -> bool,
    {
        let mut result = Vec::new();
        for row in self.read_all_rows()? {
            if pred(&row) {
                result.push(row);
            }
        }
        Ok(result)
    }

    /// Read rows with projection (column indices).
    pub fn read_projected(&self, col_indices: &[usize]) -> Result<Vec<Vec<ColValue>>> {
        let all = self.read_all_rows()?;
        Ok(all
            .into_iter()
            .map(|row| col_indices.iter().map(|&i| row[i].clone()).collect())
            .collect())
    }

    /// Read rows filtered and projected.
    pub fn read_filtered_projected<F>(
        &self,
        col_indices: &[usize],
        pred: F,
    ) -> Result<Vec<Vec<ColValue>>>
    where
        F: Fn(&[ColValue]) -> bool,
    {
        let mut result = Vec::new();
        for row in self.read_all_rows()? {
            if pred(&row) {
                result.push(col_indices.iter().map(|&i| row[i].clone()).collect());
            }
        }
        Ok(result)
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::column_delta::ColumnDataType;
    use crate::column_delta::ColumnInfo;
    use crate::columnar::ColumnSegmentBuilder;
    use std::fs;
    use std::path::PathBuf;

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("qmind_colreader_test_{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn schema() -> TableSchema {
        TableSchema {
            table_name: "t".into(),
            columns: vec![
                ColumnInfo {
                    name: "id".into(),
                    col_type: ColumnDataType::Int,
                },
                ColumnInfo {
                    name: "val".into(),
                    col_type: ColumnDataType::Text,
                },
            ],
        }
    }

    fn write_segment(dir: &Path, rows: &[(i64, &str)]) {
        let mut b = ColumnSegmentBuilder::new(2, rows.len() as u32);
        let ids: Vec<Option<i64>> = rows.iter().map(|(id, _)| Some(*id)).collect();
        let vals: Vec<Option<&str>> = rows.iter().map(|(_, v)| Some(*v)).collect();
        b.push_int_raw(&ids);
        b.push_text_raw(&vals);
        let seg_id = count_segs(dir);
        b.write_to(dir.join(format!("col_{seg_id:06}.seg")))
            .unwrap();
    }

    fn count_segs(dir: &Path) -> u64 {
        list_segments(dir).unwrap().len() as u64
    }

    #[test]
    fn read_single_segment() {
        let dir = test_dir("single");
        write_segment(&dir, &[(1, "a"), (2, "b"), (3, "c")]);

        let reader = ColumnarReader::open(&dir, schema()).unwrap();
        assert_eq!(reader.total_rows(), 3);
        assert_eq!(reader.num_columns(), 2);

        let rows = reader.read_all_rows().unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0][0], ColValue::Int(1));
        assert_eq!(rows[0][1], ColValue::Text("a".into()));
        assert_eq!(rows[2][0], ColValue::Int(3));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_multiple_segments_concatenated() {
        let dir = test_dir("multi");
        write_segment(&dir, &[(1, "x"), (2, "y")]);
        write_segment(&dir, &[(3, "z"), (4, "w"), (5, "v")]);

        let reader = ColumnarReader::open(&dir, schema()).unwrap();
        assert_eq!(reader.total_rows(), 5);

        let rows = reader.read_all_rows().unwrap();
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[3][0], ColValue::Int(4));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_filtered() {
        let dir = test_dir("filtered");
        write_segment(&dir, &[(1, "a"), (2, "b"), (3, "c"), (4, "d")]);

        let reader = ColumnarReader::open(&dir, schema()).unwrap();
        let rows = reader
            .read_filtered(|r| matches!(&r[0], ColValue::Int(n) if *n > 2))
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][0], ColValue::Int(3));
        assert_eq!(rows[1][0], ColValue::Int(4));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_projected() {
        let dir = test_dir("projected");
        write_segment(&dir, &[(1, "a"), (2, "b")]);

        let reader = ColumnarReader::open(&dir, schema()).unwrap();
        let rows = reader.read_projected(&[0]).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], vec![ColValue::Int(1)]);
        assert_eq!(rows[1], vec![ColValue::Int(2)]);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn column_names_match_schema() {
        let dir = test_dir("names");
        let reader = ColumnarReader::open(&dir, schema()).unwrap();
        assert_eq!(reader.column_names(), vec!["id", "val"]);
        let _ = fs::remove_dir_all(&dir);
    }
}
