use super::DatabaseInfo;
use anyhow::{Context, Result};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Result of replay-file scanning for a specific folder/type combination.
/// Covers both `.npz` and `.s2r` outputs — any file in either format counts
/// as a valid replay for the purposes of `TickData` flag management.
#[derive(Debug, Clone)]
pub struct NpzScanResult {
    /// Set of (demo_name, collection_num) tuples found in filesystem (NPZ or S2R)
    pub existing_files: HashSet<(String, u32)>,
    /// Total count of unique replay files found (de-duplicated across formats)
    pub file_count: usize,
}

/// Scan a directory for demo files (.dem, .gz, .zst)
pub fn scan_directory_for_demos(path: &Path, _recursive: bool) -> Result<Vec<PathBuf>> {
    let mut demo_files = Vec::new();

    if !path.exists() {
        return Ok(demo_files);
    }

    // Non-recursive scan
    let entries = match std::fs::read_dir(path) {
        Ok(iter) => iter,
        Err(e) => {
            return Err(anyhow::anyhow!(
                "Failed to read directory {}: {}",
                path.display(),
                e
            ))
        }
    };

    for entry in entries {
        // Silently skip entries we can't access
        if let Ok(entry) = entry {
            let path = entry.path();

            // is_file() might fail if metadata cannot be read, so we check existence first or just use it
            if path.is_file() {
                if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                    let ext_lower = ext.to_lowercase();
                    if ext_lower == "dem" || ext_lower == "gz" || ext_lower == "zst" {
                        demo_files.push(path);
                    }
                }
            }
        }
    }
    Ok(demo_files)
}

/// Scan directory for DuckDB files matching {TYPE}_*.duckdb pattern
pub fn scan_databases(master_dir: &Path) -> Result<Vec<DatabaseInfo>> {
    if !master_dir.exists() {
        // Directory doesn't exist yet - return empty list (first-time setup case)
        return Ok(Vec::new());
    }

    let mut databases = Vec::new();

    for entry in std::fs::read_dir(master_dir).context(format!(
        "Failed to read directory: {}",
        master_dir.display()
    ))? {
        let entry = entry?;
        let path = entry.path();

        if path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.eq_ignore_ascii_case("duckdb"))
            .unwrap_or(false)
        {
            if let Some(info) = DatabaseInfo::from_path(&path) {
                databases.push(info);
            }
        }
    }

    databases.sort_by(|a, b| {
        a.collection_type
            .cmp(&b.collection_type)
            .then_with(|| a.folder.cmp(&b.folder))
    });

    Ok(databases)
}

/// Scan replay files (`.npz` **and** `.s2r`) in a TickByTick directory for a
/// specific folder/type combination.
///
/// Returns the union of all `(demo_name, collection_num)` pairs found for
/// either format, so that the stats-cleanup logic in `stats.rs` does not
/// incorrectly reset `TickData = 0` for collections whose output is an S2R
/// file rather than an NPZ file.
///
/// Filename format (shared by both extensions): `{TYPE}_{DEMONAME}_{COLLECTIONNUM}.{ext}`
pub fn scan_npz_files(parser_output: &Path, folder: &str, collection_type: &str) -> NpzScanResult {
    let tick_dir = parser_output
        .join("TickByTick")
        .join(folder)
        .join(collection_type);

    let mut existing_files = HashSet::new();
    let mut file_count = 0;

    if !tick_dir.exists() {
        return NpzScanResult {
            existing_files,
            file_count,
        };
    }

    if let Ok(entries) = std::fs::read_dir(&tick_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let ext_match = path
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s.eq_ignore_ascii_case("npz") || s.eq_ignore_ascii_case("s2r"))
                .unwrap_or(false);

            if ext_match {
                if let Some(filename) = path.file_stem().and_then(|s| s.to_str()) {
                    // Parse: {TYPE}_{DEMONAME}_{COLLECTIONNUM}
                    let parts: Vec<&str> = filename.split('_').collect();
                    if parts.len() >= 3 {
                        // Last part is collection num, everything between first and last is demo name
                        if let Ok(col_num) = parts[parts.len() - 1].parse::<u32>() {
                            let demo_name = parts[1..parts.len() - 1].join("_");
                            // insert returns false for duplicates (same key found in both
                            // .npz and .s2r) — only count each unique pair once.
                            if existing_files.insert((demo_name, col_num)) {
                                file_count += 1;
                            }
                        }
                    }
                }
            }
        }
    }

    NpzScanResult {
        existing_files,
        file_count,
    }
}
