//! Original demo provenance stored with each DuckDB catalogue partition.

use serde::{Deserialize, Serialize};

/// One source file per demo name in a type/folder database.
///
/// The surrounding DuckDB partition supplies collection type and source folder. Two distinct
/// files with the same leaf demo name in that same partition are therefore ambiguous; the newest
/// successful import intentionally replaces the earlier provenance row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DemoSource {
    pub demo_name: String,
    pub source_path: String,
    pub size_bytes: i64,
    pub modified_ns: i64,
}
