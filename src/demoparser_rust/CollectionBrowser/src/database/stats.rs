use super::scanner;
use anyhow::Result;
use duckdb::{params, Connection};
use std::collections::HashSet;
use std::path::Path;

/// Reset TickData flag for entries where NPZ file is missing
/// (entries with TickData=1 but no corresponding NPZ file)
/// Returns number of entries reset
fn reset_missing_tickdata_entries(
    conn: &Connection,
    _collection_type: &str,
    npz_scan: &scanner::NpzScanResult,
) -> Result<usize> {
    // Query all entries with TickData=1
    let mut stmt =
        conn.prepare("SELECT demo_name, collection_num FROM kill_collections WHERE TickData = 1")?;

    let db_entries: Vec<(String, u32)> = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i32>(1)? as u32))
        })?
        .flatten()
        .collect();

    // Find entries where NPZ is missing (TickData=1 but no NPZ file)
    let mut to_reset = Vec::new();
    for (demo_name, col_num) in &db_entries {
        if !npz_scan
            .existing_files
            .contains(&(demo_name.clone(), *col_num))
        {
            to_reset.push((demo_name.clone(), *col_num));
        }
    }

    if to_reset.is_empty() {
        return Ok(0);
    }

    // Reset TickData and GrenadeTraj to 0 for entries with missing NPZ files
    conn.execute("BEGIN TRANSACTION", [])?;

    let mut update_stmt = conn.prepare(
        "UPDATE kill_collections SET TickData = 0, GrenadeTraj = 0 WHERE demo_name = ? AND collection_num = ?"
    )?;

    for (demo_name, col_num) in &to_reset {
        update_stmt.execute(params![demo_name, *col_num as i32])?;
    }

    conn.execute("COMMIT", [])?;

    Ok(to_reset.len())
}

/// Enable TickData flag for entries where NPZ file is found but TickData is not 1
/// (entries with TickData=0/NULL but valid NPZ file found)
/// Returns number of entries updated
fn enable_found_tickdata_entries(
    conn: &Connection,
    _collection_type: &str,
    npz_scan: &scanner::NpzScanResult,
) -> Result<usize> {
    // Query all entries where TickData is NOT 1 (or NULL)
    let mut stmt = conn.prepare(
        "SELECT demo_name, collection_num FROM kill_collections WHERE TickData != 1 OR TickData IS NULL"
    )?;

    let db_entries: Vec<(String, u32)> = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i32>(1)? as u32))
        })?
        .flatten()
        .collect();

    // Find entries where NPZ exists (in scan result) but DB says disabled
    let mut to_enable = Vec::new();
    for (demo_name, col_num) in &db_entries {
        if npz_scan
            .existing_files
            .contains(&(demo_name.clone(), *col_num))
        {
            to_enable.push((demo_name.clone(), *col_num));
        }
    }

    if to_enable.is_empty() {
        return Ok(0);
    }

    // Update TickData to 1
    conn.execute("BEGIN TRANSACTION", [])?;

    let mut update_stmt = conn.prepare(
        "UPDATE kill_collections SET TickData = 1 WHERE demo_name = ? AND collection_num = ?",
    )?;

    for (demo_name, col_num) in &to_enable {
        update_stmt.execute(params![demo_name, *col_num as i32])?;
    }

    conn.execute("COMMIT", [])?;

    Ok(to_enable.len())
}

/// Count all directory statistics in a single synchronous call
/// This prevents race conditions by ensuring all database accesses are sequential
/// Also performs automatic cleanup of orphaned DuckDB entries when NPZ count mismatch is detected
/// Returns a Vec of (folder_name, processed_demos_count, tick_data_enabled, total_collections)
pub fn count_all_directory_stats(
    master_dir: &Path,
    folders: &[String],
    parser_output: &Path,
) -> Result<Vec<(String, usize, usize, usize)>> {
    let mut results = Vec::new();

    if !master_dir.exists() {
        // Return zeros for all folders
        for folder in folders {
            results.push((folder.clone(), 0, 0, 0));
        }
        return Ok(results);
    }

    // Collect all database files once
    let db_files = scanner::scan_databases(master_dir)?;

    // Process each folder sequentially
    for folder in folders {
        let mut unique_demos = HashSet::new();
        let mut tick_data_enabled = 0usize;
        let mut total_collections = 0usize;
        let mut total_deleted = 0usize;

        println!("Counting stats for folder: {}", folder);

        // Check each database file
        for db_info in &db_files {
            // Check if the filename matches the folder name exactly (case-insensitive)
            if db_info.folder.eq_ignore_ascii_case(folder) {
                println!("  Processing: {}", db_info.path.display());

                if let Ok(conn) = Connection::open(&db_info.path) {
                    // Count unique demos
                    if let Ok(mut stmt) =
                        conn.prepare("SELECT DISTINCT demo_name FROM kill_collections")
                    {
                        if let Ok(rows) = stmt.query_map([], |row| row.get::<_, String>(0)) {
                            for demo_result in rows.flatten() {
                                unique_demos.insert(demo_result);
                            }
                        }
                    }

                    // Check if TickData column exists
                    let tick_data_exists: bool = conn.query_row(
                        "SELECT count(*) FROM pragma_table_info('kill_collections') WHERE name='TickData'",
                        [],
                        |row| row.get(0)
                    ).unwrap_or(0) > 0;

                    if tick_data_exists {
                        // Count where TickData = 1
                        let mut db_tick_count = conn
                            .query_row::<i32, _, _>(
                                "SELECT COUNT(*) FROM kill_collections WHERE TickData = 1",
                                [],
                                |row| row.get(0),
                            )
                            .unwrap_or(0) as usize;

                        // Scan NPZ files for this folder/type combination
                        let npz_scan = scanner::scan_npz_files(
                            parser_output,
                            folder,
                            &db_info.collection_type,
                        );

                        println!(
                            "    DuckDB TickData=1: {}, NPZ files: {}",
                            db_tick_count, npz_scan.file_count
                        );

                        // Trigger optimization: Check if files avail (Always check to handle swap cases where counts match but files differ)
                        if npz_scan.file_count > 0 {
                            match enable_found_tickdata_entries(
                                &conn,
                                &db_info.collection_type,
                                &npz_scan,
                            ) {
                                Ok(enable_count) => {
                                    if enable_count > 0 {
                                        println!(
                                            "    → Enabled TickData=1 for {} entries in {}",
                                            enable_count, db_info.collection_type
                                        );
                                    }
                                }
                                Err(e) => {
                                    eprintln!("    → Error enabling TickData flags: {}", e);
                                }
                            }
                        }

                        // Update db_tick_count from DB to be sure
                        db_tick_count = conn
                            .query_row::<i32, _, _>(
                                "SELECT COUNT(*) FROM kill_collections WHERE TickData = 1",
                                [],
                                |row| row.get(0),
                            )
                            .unwrap_or(0) as usize;

                        // If mismatch detected (DB says we have more than files on disk), reset TickData flags for missing NPZ files
                        if db_tick_count > npz_scan.file_count {
                            println!("    → Mismatch detected! Resetting TickData for missing NPZ files...");
                            match reset_missing_tickdata_entries(
                                &conn,
                                &db_info.collection_type,
                                &npz_scan,
                            ) {
                                Ok(reset_count) => {
                                    if reset_count > 0 {
                                        println!(
                                            "    → Reset TickData to 0 for {} entries in {}",
                                            reset_count, db_info.collection_type
                                        );
                                        total_deleted += reset_count;
                                    }
                                }
                                Err(e) => {
                                    eprintln!("    → Error resetting TickData flags: {}", e);
                                }
                            }

                            // Recount after reset
                            if let Ok(new_count) = conn.query_row::<i32, _, _>(
                                "SELECT COUNT(*) FROM kill_collections WHERE TickData = 1",
                                [],
                                |row| row.get(0),
                            ) {
                                tick_data_enabled += new_count as usize;
                                println!("    → Updated TickData=1 count: {}", new_count);
                            }
                        } else {
                            tick_data_enabled += db_tick_count;
                        }
                    }

                    // Count total collections
                    if let Ok(total) = conn.query_row::<i32, _, _>(
                        "SELECT COUNT(*) FROM kill_collections",
                        [],
                        |row| row.get(0),
                    ) {
                        total_collections += total as usize;
                    }
                }
            }
        }

        if total_deleted > 0 {
            println!(
                "  ✓ Reset TickData for {} entries (missing NPZ files) for {}",
                total_deleted, folder
            );
        }

        println!(
            "  Result for {}: demos={}, tickdata={}/{}",
            folder,
            unique_demos.len(),
            tick_data_enabled,
            total_collections
        );

        results.push((
            folder.clone(),
            unique_demos.len(),
            tick_data_enabled,
            total_collections,
        ));
    }

    Ok(results)
}
