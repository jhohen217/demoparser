//! DuckDB writer module for the KillCollectionMaster crate
//!
//! This module contains functionality for writing kill collections to DuckDB database files
//! using truly incremental updates - only inserting NEW collections.

pub mod demo_sources;
pub mod formatting;
pub mod metadata;
pub mod schema;
pub mod tick_assets;

#[cfg(test)]
mod performance_tests;

use duckdb::{params, Connection, Result as DuckResult};
use interface::models::collection::KillCollection;
use interface::models::demo_source::DemoSource;
use std::path::Path;

/// DuckDB writer for writing kill collections to database files
pub struct DuckDBWriter {
    /// DuckDB file path
    db_path: String,
    /// Collection type (ACE, TRIPLE, etc.)
    collection_type: String,
    /// Folder name
    folder: String,
}

#[cfg(test)]
mod replay_update_tests {
    use super::*;

    #[test]
    fn reparsing_preserves_tags_in_both_insert_paths() {
        for staging in [true, false] {
            let conn = Connection::open_in_memory().unwrap();
            schema::initialize_tables(&conn).unwrap();
            let writer = DuckDBWriter::new("unused", "TRIPLE", "month");
            let insert = |col: &KillCollection| {
                if staging {
                    writer
                        .staging_table_insert_collections(
                            &conn,
                            std::slice::from_ref(col),
                            "",
                            "now",
                        )
                        .unwrap();
                } else {
                    writer
                        .bulk_insert_collections(&conn, std::slice::from_ref(col), "", "now")
                        .unwrap();
                }
            };
            let mut col = KillCollection {
                killer_steamid: "player".into(),
                demo_name: "match".into(),
                round: 14,
                collection_type: "TRIPLE".into(),
                collection_num: 7,
                tag: "bookmark".into(),
                start_kill_tick: 50,
                ..Default::default()
            };
            insert(&col);
            col.tag.clear();
            col.start_kill_tick = 100;
            insert(&col);
            let actual: (String, i32) = conn
                .query_row(
                    "SELECT tag,start_kill_tick FROM kill_collections",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert_eq!(actual, ("bookmark".into(), 100));
        }
    }
}

impl DuckDBWriter {
    /// Create a new DuckDB writer
    pub fn new(db_path: &str, collection_type: &str, folder: &str) -> Self {
        DuckDBWriter {
            db_path: db_path.to_string(),
            collection_type: collection_type.to_string(),
            folder: folder.to_string(),
        }
    }

    /// Recalculate all metadata tables from the kill_collections data
    pub fn recalculate_metadata(&self) -> Result<(), Box<dyn std::error::Error>> {
        metadata::recalculate_metadata(&self.db_path, &self.collection_type, &self.folder)
    }

    /// Append new collections to the DuckDB file - ONLY inserts new data
    pub fn append_collections(
        &self,
        new_collections: &[KillCollection],
        skip_metadata_update: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.append_collections_with_sources(new_collections, &[], skip_metadata_update)
    }

    /// Append collections and their original source provenance in the same transaction.
    pub fn append_collections_with_sources(
        &self,
        new_collections: &[KillCollection],
        demo_sources: &[DemoSource],
        skip_metadata_update: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.append_collections_with_assets(
            new_collections,
            demo_sources,
            &[],
            skip_metadata_update,
        )
    }

    /// Commit collection rows, original provenance and every replay format on one connection.
    pub fn append_collections_with_assets(
        &self,
        new_collections: &[KillCollection],
        demo_sources: &[DemoSource],
        assets: &[interface::models::tick_asset::TickAsset],
        skip_metadata_update: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.append_with_ingestion(
            new_collections,
            demo_sources,
            assets,
            skip_metadata_update,
            Self::append_collection_rows,
            tick_assets::record_tick_assets_in_transaction,
        )
    }

    // Injectable ingestion only for native same-build benchmarks; public writes always use
    // the optimized implementations above, with identical transaction ownership.
    fn append_with_ingestion(
        &self,
        new_collections: &[KillCollection],
        demo_sources: &[DemoSource],
        assets: &[interface::models::tick_asset::TickAsset],
        skip_metadata_update: bool,
        fill: fn(&Connection, &[KillCollection], &str, &str) -> DuckResult<()>,
        record_assets: fn(
            &Connection,
            &[interface::models::tick_asset::TickAsset],
        ) -> DuckResult<()>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if new_collections.is_empty() {
            return Ok(());
        }

        println!(
            "DEBUG: append_collections called with {} collections, skip_metadata={}",
            new_collections.len(),
            skip_metadata_update
        );

        // Create parent directory if it doesn't exist
        if let Some(parent) = Path::new(&self.db_path).parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent)?;
            }
        }

        // Open connection directly to the database file
        let conn = Connection::open(&self.db_path)?;
        println!("DEBUG: Database connection opened: {}", self.db_path);

        // Initialize tables if they don't exist
        schema::initialize_tables(&conn)?;
        println!("DEBUG: Tables initialized");

        // Handle Base Path Logic
        let mut current_base_path: Option<String> = None;
        // Try to fetch existing base path
        let existing_base_path: DuckResult<String> = conn.query_row(
            "SELECT demo_base_path FROM manifest_info WHERE type = ? AND folder = ?",
            params![&self.collection_type, &self.folder],
            |row| row.get(0),
        );

        if let Ok(path) = existing_base_path {
            if !path.is_empty() {
                current_base_path = Some(path);
            }
        }

        // If no base path, determine from first collection
        if current_base_path.is_none() && !new_collections.is_empty() {
            let first_path = Path::new(&new_collections[0].demo_path);
            if let Some(parent) = first_path.parent() {
                let base = parent.to_string_lossy().replace('\\', "/") + "/"; // Normalize to forward slash with trailing slash
                current_base_path = Some(base);

                // Update manifest immediately
                conn.execute(
                    "INSERT OR IGNORE INTO manifest_info (type, folder, demo_base_path) VALUES (?, ?, ?)",
                    params![&self.collection_type, &self.folder, &current_base_path],
                )?;
                conn.execute(
                    "UPDATE manifest_info SET demo_base_path = ? WHERE type = ? AND folder = ?",
                    params![&current_base_path, &self.collection_type, &self.folder],
                )?;
            }
        }

        let base_path_str = current_base_path.unwrap_or_default();

        // Get timestamp for created_at field
        let now = chrono::Utc::now().to_rfc3339();

        // Begin transaction
        conn.execute("BEGIN TRANSACTION", [])?;
        println!("DEBUG: Transaction BEGIN");

        let transaction_result: Result<(), Box<dyn std::error::Error>> = (|| {
            // Use staging table for maximum performance (Phase 3 optimization)
            self.staging_insert_with(&conn, new_collections, &base_path_str, &now, fill)?;
            println!("DEBUG: Staging table insert completed");
            demo_sources::upsert_demo_sources(&conn, demo_sources)?;
            conn.execute("CREATE TABLE IF NOT EXISTS collection_discovery (demo_name VARCHAR PRIMARY KEY, version BIGINT NOT NULL)", [])?;
            let names: std::collections::HashSet<_> = new_collections.iter().map(|c| &c.demo_name).collect();
            for name in names {
                conn.execute("INSERT OR REPLACE INTO collection_discovery VALUES (?, ?)",
                    params![name, interface::core::tick_processor::COLLECTION_DISCOVERY_VERSION])?;
            }
            record_assets(&conn, assets)?;

            // Only recalculate metadata if requested (expensive operation)
            if !skip_metadata_update {
                println!("DEBUG: Updating metadata...");
                metadata::update_manifest_info(&conn, &self.collection_type, &self.folder)?;
                metadata::update_map_totals(&conn, &self.collection_type)?;
                metadata::update_weapon_totals(&conn, &self.collection_type)?;
                println!("DEBUG: Metadata updated");
            } else {
                println!("DEBUG: Skipping metadata update");
            }
            Ok(())
        })();

        if let Err(error) = transaction_result {
            let _ = conn.execute("ROLLBACK", []);
            return Err(error);
        }
        if let Err(error) = conn.execute("COMMIT", []) {
            let _ = conn.execute("ROLLBACK", []);
            return Err(Box::new(error));
        }
        println!("DEBUG: Transaction COMMITTED successfully");

        // Verify data was written
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM kill_collections", [], |row| {
                row.get(0)
            })
            .unwrap_or(0);
        println!(
            "DEBUG: Total rows in kill_collections after commit: {}",
            count
        );

        if !skip_metadata_update {
            println!(
                "Appended {} new collections to DuckDB: {}",
                new_collections.len(),
                self.db_path
            );
        }

        Ok(())
    }

    #[cfg(test)]
    /// Test entry point for native staging plus the production conflict merge.
    fn staging_table_insert_collections(
        &self,
        conn: &Connection,
        collections: &[KillCollection],
        base_path_str: &str,
        now: &str,
    ) -> DuckResult<()> {
        self.staging_insert_with(
            conn,
            collections,
            base_path_str,
            now,
            Self::append_collection_rows,
        )
    }

    fn staging_insert_with(
        &self,
        conn: &Connection,
        collections: &[KillCollection],
        base_path_str: &str,
        now: &str,
        fill: fn(&Connection, &[KillCollection], &str, &str) -> DuckResult<()>,
    ) -> DuckResult<()> {
        println!("DEBUG: Creating staging table...");

        // Create temporary staging table (same schema as main table, no constraints)
        if let Err(e) = conn.execute(
            "CREATE TEMP TABLE staging_collections AS
             SELECT * FROM kill_collections WHERE 1=0",
            [],
        ) {
            eprintln!("ERROR: Failed to create staging table: {}", e);
            println!("DEBUG: Falling back to bulk insert method");
            // Fallback to bulk insert if staging table fails
            return self.bulk_insert_collections(conn, collections, base_path_str, now);
        }

        println!("DEBUG: Staging table created successfully");

        fill(conn, collections, base_path_str, now)?;

        // Now merge staging into main table with single operation
        conn.execute(
            "INSERT INTO kill_collections
             SELECT * FROM staging_collections
             ON CONFLICT (steam_id, demo_name, round) DO UPDATE SET
                type = excluded.type,
                collection_num = excluded.collection_num,
                col_total = excluded.col_total,
                tick_duration = excluded.tick_duration,
                map_name = excluded.map_name,
                game_version = excluded.game_version,
                tag = CASE WHEN excluded.tag IS NULL OR excluded.tag = '' THEN kill_collections.tag ELSE excluded.tag END,
                created_at = excluded.created_at,
                demo_relative_path = excluded.demo_relative_path,
                demo_source_drive = excluded.demo_source_drive,
                killer_index = excluded.killer_index,
                killer_team = excluded.killer_team,
                start_kill_tick = excluded.start_kill_tick,
                end_kill_tick = excluded.end_kill_tick,
                killer_name = excluded.killer_name,
                killer_radius = excluded.killer_radius,
                victims_radius = excluded.victims_radius,
                killer_move_distance = excluded.killer_move_distance,
                victim_team = excluded.victim_team,
                round_start_tick = excluded.round_start_tick,
                round_end_tick = excluded.round_end_tick,
                round_freeze_end = excluded.round_freeze_end,
                weapons = excluded.weapons,
                weapons_id = excluded.weapons_id,
                kill_ticks = excluded.kill_ticks,
                victims_index = excluded.victims_index,
                TickData = CASE WHEN (kill_collections.TickData = 1 AND excluded.TickData = 0) THEN kill_collections.TickData ELSE excluded.TickData END,
                GrenadeTraj = CASE WHEN (kill_collections.TickData = 1 AND excluded.TickData = 0) THEN kill_collections.GrenadeTraj ELSE excluded.GrenadeTraj END,
                util_thrown = CASE WHEN (kill_collections.TickData = 1 AND excluded.TickData = 0) THEN kill_collections.util_thrown ELSE excluded.util_thrown END,
                hits = CASE WHEN (kill_collections.TickData = 1 AND excluded.TickData = 0) THEN kill_collections.hits ELSE excluded.hits END,
                misses = CASE WHEN (kill_collections.TickData = 1 AND excluded.TickData = 0) THEN kill_collections.misses ELSE excluded.misses END,
                hit_rate = CASE WHEN (kill_collections.TickData = 1 AND excluded.TickData = 0) THEN kill_collections.hit_rate ELSE excluded.hit_rate END,
                weapons_formatted = CASE WHEN (kill_collections.TickData = 1 AND excluded.TickData = 0) THEN kill_collections.weapons_formatted ELSE excluded.weapons_formatted END,
                killer_pos_x = CASE WHEN (kill_collections.TickData = 1 AND excluded.TickData = 0) THEN kill_collections.killer_pos_x ELSE excluded.killer_pos_x END,
                killer_pos_y = CASE WHEN (kill_collections.TickData = 1 AND excluded.TickData = 0) THEN kill_collections.killer_pos_y ELSE excluded.killer_pos_y END,
                killer_pos_z = CASE WHEN (kill_collections.TickData = 1 AND excluded.TickData = 0) THEN kill_collections.killer_pos_z ELSE excluded.killer_pos_z END,
                killer_view_pitch = CASE WHEN (kill_collections.TickData = 1 AND excluded.TickData = 0) THEN kill_collections.killer_view_pitch ELSE excluded.killer_view_pitch END,
                killer_view_yaw = CASE WHEN (kill_collections.TickData = 1 AND excluded.TickData = 0) THEN kill_collections.killer_view_yaw ELSE excluded.killer_view_yaw END,
                victim_pos_x = CASE WHEN (kill_collections.TickData = 1 AND excluded.TickData = 0) THEN kill_collections.victim_pos_x ELSE excluded.victim_pos_x END,
                victim_pos_y = CASE WHEN (kill_collections.TickData = 1 AND excluded.TickData = 0) THEN kill_collections.victim_pos_y ELSE excluded.victim_pos_y END,
                victim_pos_z = CASE WHEN (kill_collections.TickData = 1 AND excluded.TickData = 0) THEN kill_collections.victim_pos_z ELSE excluded.victim_pos_z END,
                victim_distance = CASE WHEN (kill_collections.TickData = 1 AND excluded.TickData = 0) THEN kill_collections.victim_distance ELSE excluded.victim_distance END,
                ticks_between_kills = CASE WHEN (kill_collections.TickData = 1 AND excluded.TickData = 0) THEN kill_collections.ticks_between_kills ELSE excluded.ticks_between_kills END,
                movement_between_kills = CASE WHEN (kill_collections.TickData = 1 AND excluded.TickData = 0) THEN kill_collections.movement_between_kills ELSE excluded.movement_between_kills END,
                kill_weapon_ids = CASE WHEN (kill_collections.TickData = 1 AND excluded.TickData = 0) THEN kill_collections.kill_weapon_ids ELSE excluded.kill_weapon_ids END",
            [],
        )?;

        // Drop staging table (temp tables auto-drop at end of transaction, but explicit is better)
        conn.execute("DROP TABLE staging_collections", [])?;

        Ok(())
    }

    /// Feed typed values directly to DuckDB; flush errors must reach the transaction owner.
    fn append_collection_rows(
        conn: &Connection,
        collections: &[KillCollection],
        base_path_str: &str,
        now: &str,
    ) -> DuckResult<()> {
        let mut appender = conn.appender("staging_collections")?;
        for collection in collections {
            let (drive_letter, relative_path, arrays) =
                formatting::prepare_collection_data(collection, base_path_str);
            appender.append_row(params![
                collection.killer_steamid,
                collection.demo_name,
                collection.round as i64,
                collection.collection_type,
                collection.collection_num as i64,
                collection.col_total as i64,
                collection.tick_duration as i64,
                collection.map_name,
                collection.game_version as i64,
                collection.parsed as i64,
                collection.grenade_traj as i64,
                collection.tag,
                now,
                relative_path,
                drive_letter,
                collection.killer_index as i64,
                collection.killer_team,
                collection.killer_name,
                collection.killer_radius,
                collection.killer_move_distance,
                collection.start_kill_tick as i64,
                collection.end_kill_tick as i64,
                collection.victim_team,
                collection.victims_radius,
                collection.round_start_tick as i64,
                collection.round_end_tick as i64,
                collection.round_freeze_end as i64,
                arrays.weapons_str,
                arrays.weapons_id_str,
                arrays.kill_ticks_str,
                arrays.victims_index_str,
                arrays.victim_names,
                collection.util_thrown,
                collection.hits as i64,
                collection.misses as i64,
                collection.hit_rate,
                arrays.weapons_formatted,
                arrays.killer_pos_x,
                arrays.killer_pos_y,
                arrays.killer_pos_z,
                arrays.killer_pitch,
                arrays.killer_yaw,
                arrays.victim_pos_x,
                arrays.victim_pos_y,
                arrays.victim_pos_z,
                arrays.victim_dist,
                arrays.ticks_between,
                arrays.move_between,
                arrays.kill_weapon_ids,
            ])?;
        }
        appender.flush()?;
        drop(appender);
        Ok(())
    }

    // Original ingestion kept only for equivalence and native writer performance tests.
    #[cfg(test)]
    fn values_collection_rows(
        conn: &Connection,
        collections: &[KillCollection],
        base_path_str: &str,
        now: &str,
    ) -> DuckResult<()> {
        // Bulk insert all data into staging table (no conflict checking)
        const CHUNK_SIZE: usize = 500; // Larger chunks for staging table

        for chunk in collections.chunks(CHUNK_SIZE) {
            let mut values_clauses = Vec::new();
            let mut all_params: Vec<Box<dyn duckdb::ToSql>> = Vec::new();

            for collection in chunk {
                let (drive_letter, relative_path, arrays) =
                    formatting::prepare_collection_data(collection, base_path_str);

                values_clauses.push(format!(
                    "({placeholders})",
                    placeholders = (0..49).map(|_| "?").collect::<Vec<_>>().join(", ")
                ));

                // Add all parameters in exact table column order (49 total)
                all_params.push(Box::new(collection.killer_steamid.clone())); // 1. steam_id
                all_params.push(Box::new(collection.demo_name.clone())); // 2. demo_name
                all_params.push(Box::new(collection.round as i64)); // 3. round
                all_params.push(Box::new(collection.collection_type.clone())); // 4. type
                all_params.push(Box::new(collection.collection_num as i64)); // 5. collection_num
                all_params.push(Box::new(collection.col_total as i64)); // 6. col_total
                all_params.push(Box::new(collection.tick_duration as i64)); // 7. tick_duration
                all_params.push(Box::new(collection.map_name.clone())); // 8. map_name
                all_params.push(Box::new(collection.game_version as i64)); // 9. game_version
                all_params.push(Box::new(collection.parsed as i64)); // 10. TickData
                all_params.push(Box::new(collection.grenade_traj as i64)); // 11. GrenadeTraj
                all_params.push(Box::new(collection.tag.clone())); // 12. tag
                all_params.push(Box::new(now.to_string())); // 13. created_at
                all_params.push(Box::new(relative_path)); // 14. demo_relative_path
                all_params.push(Box::new(drive_letter)); // 15. demo_source_drive
                all_params.push(Box::new(collection.killer_index as i64)); // 16. killer_index
                all_params.push(Box::new(collection.killer_team.clone())); // 17. killer_team
                all_params.push(Box::new(collection.killer_name.clone())); // 18. killer_name
                all_params.push(Box::new(collection.killer_radius)); // 19. killer_radius
                all_params.push(Box::new(collection.killer_move_distance)); // 20. killer_move_distance
                all_params.push(Box::new(collection.start_kill_tick as i64)); // 21. start_kill_tick
                all_params.push(Box::new(collection.end_kill_tick as i64)); // 22. end_kill_tick
                all_params.push(Box::new(collection.victim_team.clone())); // 23. victim_team
                all_params.push(Box::new(collection.victims_radius)); // 24. victims_radius
                all_params.push(Box::new(collection.round_start_tick as i64)); // 25. round_start_tick
                all_params.push(Box::new(collection.round_end_tick as i64)); // 26. round_end_tick
                all_params.push(Box::new(collection.round_freeze_end as i64)); // 27. round_freeze_end
                all_params.push(Box::new(arrays.weapons_str)); // 28. weapons
                all_params.push(Box::new(arrays.weapons_id_str)); // 29. weapons_id
                all_params.push(Box::new(arrays.kill_ticks_str)); // 30. kill_ticks
                all_params.push(Box::new(arrays.victims_index_str)); // 31. victims_index
                all_params.push(Box::new(arrays.victim_names)); // 32. victims_names
                all_params.push(Box::new(collection.util_thrown.clone())); // 33. util_thrown
                all_params.push(Box::new(collection.hits as i64)); // 34. hits
                all_params.push(Box::new(collection.misses as i64)); // 35. misses
                all_params.push(Box::new(collection.hit_rate)); // 36. hit_rate
                all_params.push(Box::new(arrays.weapons_formatted)); // 37. weapons_formatted
                all_params.push(Box::new(arrays.killer_pos_x)); // 38. killer_pos_x
                all_params.push(Box::new(arrays.killer_pos_y)); // 39. killer_pos_y
                all_params.push(Box::new(arrays.killer_pos_z)); // 40. killer_pos_z
                all_params.push(Box::new(arrays.killer_pitch)); // 41. killer_view_pitch
                all_params.push(Box::new(arrays.killer_yaw)); // 42. killer_view_yaw
                all_params.push(Box::new(arrays.victim_pos_x)); // 43. victim_pos_x
                all_params.push(Box::new(arrays.victim_pos_y)); // 44. victim_pos_y
                all_params.push(Box::new(arrays.victim_pos_z)); // 45. victim_pos_z
                all_params.push(Box::new(arrays.victim_dist)); // 46. victim_distance
                all_params.push(Box::new(arrays.ticks_between)); // 47. ticks_between_kills
                all_params.push(Box::new(arrays.move_between)); // 48. movement_between_kills
                all_params.push(Box::new(arrays.kill_weapon_ids)); // 49. kill_weapon_ids
            }

            // Simple INSERT into staging (no conflict handling needed)
            let sql = format!(
                "INSERT INTO staging_collections VALUES {}",
                values_clauses.join(", ")
            );

            let params_refs: Vec<&dyn duckdb::ToSql> = all_params
                .iter()
                .map(|p| p.as_ref() as &dyn duckdb::ToSql)
                .collect();
            conn.execute(&sql, params_refs.as_slice())?;
        }

        Ok(())
    }

    /// Phase 2: Bulk insert collections using multi-row VALUES for performance
    #[allow(dead_code)]
    fn bulk_insert_collections(
        &self,
        conn: &Connection,
        collections: &[KillCollection],
        base_path_str: &str,
        now: &str,
    ) -> DuckResult<()> {
        // Process in chunks to avoid query size limits
        const CHUNK_SIZE: usize = 100;

        for chunk in collections.chunks(CHUNK_SIZE) {
            // Build multi-row INSERT statement
            let mut values_clauses = Vec::new();
            let mut all_params: Vec<Box<dyn duckdb::ToSql>> = Vec::new();

            for collection in chunk {
                // Pre-compute all string values (array formatting)
                let (drive_letter, relative_path, arrays) =
                    formatting::prepare_collection_data(collection, base_path_str);

                // Build single value clause with placeholders
                values_clauses.push(format!(
                    "({placeholders})",
                    placeholders = (0..49).map(|_| "?").collect::<Vec<_>>().join(", ")
                ));

                // Add all parameters in order
                all_params.push(Box::new(collection.killer_steamid.clone()));
                all_params.push(Box::new(collection.demo_name.clone()));
                all_params.push(Box::new(collection.round as i64));
                all_params.push(Box::new(collection.collection_type.clone()));
                all_params.push(Box::new(collection.collection_num as i64));
                all_params.push(Box::new(collection.col_total as i64));
                all_params.push(Box::new(collection.tick_duration as i64));
                all_params.push(Box::new(collection.map_name.clone()));
                all_params.push(Box::new(collection.game_version as i64));
                all_params.push(Box::new(collection.parsed as i64));
                all_params.push(Box::new(collection.grenade_traj as i64)); // GrenadeTraj
                all_params.push(Box::new(collection.tag.clone()));
                all_params.push(Box::new(now.to_string()));
                all_params.push(Box::new(relative_path));
                all_params.push(Box::new(drive_letter));

                all_params.push(Box::new(collection.killer_index as i64));
                all_params.push(Box::new(collection.killer_team.clone()));
                all_params.push(Box::new(collection.start_kill_tick as i64));
                all_params.push(Box::new(collection.end_kill_tick as i64));
                all_params.push(Box::new(collection.killer_name.clone()));
                all_params.push(Box::new(collection.killer_radius));
                all_params.push(Box::new(collection.victims_radius));
                all_params.push(Box::new(collection.killer_move_distance));
                all_params.push(Box::new(collection.victim_team.clone()));

                all_params.push(Box::new(collection.round_start_tick as i64));
                all_params.push(Box::new(collection.round_end_tick as i64));
                all_params.push(Box::new(collection.round_freeze_end as i64));

                all_params.push(Box::new(arrays.weapons_str));
                all_params.push(Box::new(arrays.weapons_id_str));
                all_params.push(Box::new(arrays.kill_ticks_str));
                all_params.push(Box::new(arrays.victims_index_str));
                all_params.push(Box::new(arrays.victim_names));
                all_params.push(Box::new(collection.util_thrown.clone()));

                all_params.push(Box::new(collection.hits as i64));
                all_params.push(Box::new(collection.misses as i64));
                all_params.push(Box::new(collection.hit_rate));
                all_params.push(Box::new(arrays.weapons_formatted));

                // Position arrays
                all_params.push(Box::new(arrays.killer_pos_x));
                all_params.push(Box::new(arrays.killer_pos_y));
                all_params.push(Box::new(arrays.killer_pos_z));
                all_params.push(Box::new(arrays.killer_pitch));
                all_params.push(Box::new(arrays.killer_yaw));
                all_params.push(Box::new(arrays.victim_pos_x));
                all_params.push(Box::new(arrays.victim_pos_y));
                all_params.push(Box::new(arrays.victim_pos_z));
                all_params.push(Box::new(arrays.victim_dist));
                all_params.push(Box::new(arrays.ticks_between));
                all_params.push(Box::new(arrays.move_between));
                all_params.push(Box::new(arrays.kill_weapon_ids));
            }

            // Build complete INSERT statement
            let sql = format!(
                "INSERT INTO kill_collections (
                    steam_id, demo_name, round, type, collection_num, col_total, tick_duration, map_name,
                    game_version, TickData, GrenadeTraj, tag, created_at, demo_relative_path, demo_source_drive,
                    killer_index, killer_team, start_kill_tick, end_kill_tick, killer_name,
                    killer_radius, victims_radius, killer_move_distance, victim_team,
                    round_start_tick, round_end_tick, round_freeze_end,
                    weapons, weapons_id, kill_ticks, victims_index, victims_names, util_thrown,
                    hits, misses, hit_rate, weapons_formatted,
                    killer_pos_x, killer_pos_y, killer_pos_z, killer_view_pitch, killer_view_yaw,
                    victim_pos_x, victim_pos_y, victim_pos_z, victim_distance,
                    ticks_between_kills, movement_between_kills, kill_weapon_ids
                ) VALUES {}
                ON CONFLICT (steam_id, demo_name, round) DO UPDATE SET
                    type = excluded.type,
                    collection_num = excluded.collection_num,
                    col_total = excluded.col_total,
                    tick_duration = excluded.tick_duration,
                    map_name = excluded.map_name,
                    game_version = excluded.game_version,
                    tag = CASE WHEN excluded.tag IS NULL OR excluded.tag = '' THEN kill_collections.tag ELSE excluded.tag END,
                    created_at = excluded.created_at,
                    demo_relative_path = excluded.demo_relative_path,
                    demo_source_drive = excluded.demo_source_drive,
                    killer_index = excluded.killer_index,
                    killer_team = excluded.killer_team,
                    start_kill_tick = excluded.start_kill_tick,
                    end_kill_tick = excluded.end_kill_tick,
                    killer_name = excluded.killer_name,
                    killer_radius = excluded.killer_radius,
                    victims_radius = excluded.victims_radius,
                    killer_move_distance = excluded.killer_move_distance,
                    victim_team = excluded.victim_team,
                    round_start_tick = excluded.round_start_tick,
                    round_end_tick = excluded.round_end_tick,
                    round_freeze_end = excluded.round_freeze_end,
                    weapons = excluded.weapons,
                    weapons_id = excluded.weapons_id,
                    kill_ticks = excluded.kill_ticks,
                    victims_index = excluded.victims_index,
                    TickData = CASE WHEN (kill_collections.TickData = 1 AND excluded.TickData = 0) THEN kill_collections.TickData ELSE excluded.TickData END,
                    GrenadeTraj = CASE WHEN (kill_collections.TickData = 1 AND excluded.TickData = 0) THEN kill_collections.GrenadeTraj ELSE excluded.GrenadeTraj END,
                    util_thrown = CASE WHEN (kill_collections.TickData = 1 AND excluded.TickData = 0) THEN kill_collections.util_thrown ELSE excluded.util_thrown END,
                    hits = CASE WHEN (kill_collections.TickData = 1 AND excluded.TickData = 0) THEN kill_collections.hits ELSE excluded.hits END,
                    misses = CASE WHEN (kill_collections.TickData = 1 AND excluded.TickData = 0) THEN kill_collections.misses ELSE excluded.misses END,
                    hit_rate = CASE WHEN (kill_collections.TickData = 1 AND excluded.TickData = 0) THEN kill_collections.hit_rate ELSE excluded.hit_rate END,
                    weapons_formatted = CASE WHEN (kill_collections.TickData = 1 AND excluded.TickData = 0) THEN kill_collections.weapons_formatted ELSE excluded.weapons_formatted END,
                    killer_pos_x = CASE WHEN (kill_collections.TickData = 1 AND excluded.TickData = 0) THEN kill_collections.killer_pos_x ELSE excluded.killer_pos_x END,
                    killer_pos_y = CASE WHEN (kill_collections.TickData = 1 AND excluded.TickData = 0) THEN kill_collections.killer_pos_y ELSE excluded.killer_pos_y END,
                    killer_pos_z = CASE WHEN (kill_collections.TickData = 1 AND excluded.TickData = 0) THEN kill_collections.killer_pos_z ELSE excluded.killer_pos_z END,
                    killer_view_pitch = CASE WHEN (kill_collections.TickData = 1 AND excluded.TickData = 0) THEN kill_collections.killer_view_pitch ELSE excluded.killer_view_pitch END,
                    killer_view_yaw = CASE WHEN (kill_collections.TickData = 1 AND excluded.TickData = 0) THEN kill_collections.killer_view_yaw ELSE excluded.killer_view_yaw END,
                    victim_pos_x = CASE WHEN (kill_collections.TickData = 1 AND excluded.TickData = 0) THEN kill_collections.victim_pos_x ELSE excluded.victim_pos_x END,
                    victim_pos_y = CASE WHEN (kill_collections.TickData = 1 AND excluded.TickData = 0) THEN kill_collections.victim_pos_y ELSE excluded.victim_pos_y END,
                    victim_pos_z = CASE WHEN (kill_collections.TickData = 1 AND excluded.TickData = 0) THEN kill_collections.victim_pos_z ELSE excluded.victim_pos_z END,
                    victim_distance = CASE WHEN (kill_collections.TickData = 1 AND excluded.TickData = 0) THEN kill_collections.victim_distance ELSE excluded.victim_distance END,
                    ticks_between_kills = CASE WHEN (kill_collections.TickData = 1 AND excluded.TickData = 0) THEN kill_collections.ticks_between_kills ELSE excluded.ticks_between_kills END,
                    movement_between_kills = CASE WHEN (kill_collections.TickData = 1 AND excluded.TickData = 0) THEN kill_collections.movement_between_kills ELSE excluded.movement_between_kills END,
                    kill_weapon_ids = CASE WHEN (kill_collections.TickData = 1 AND excluded.TickData = 0) THEN kill_collections.kill_weapon_ids ELSE excluded.kill_weapon_ids END",
                values_clauses.join(", ")
            );

            // Execute with all parameters
            let params_refs: Vec<&dyn duckdb::ToSql> = all_params
                .iter()
                .map(|p| p.as_ref() as &dyn duckdb::ToSql)
                .collect();
            conn.execute(&sql, params_refs.as_slice())?;
        }

        Ok(())
    }

    /// Check if a demo exists and return its status
    /// Returns: (exists, col_total, actual_count)
    pub fn check_demo_status(
        &self,
        demo_name: &str,
    ) -> Result<(bool, i32, i32), Box<dyn std::error::Error>> {
        if !Path::new(&self.db_path).exists() {
            return Ok((false, 0, 0));
        }

        let conn = Connection::open(&self.db_path)?;

        // Check if table exists first
        let table_exists: bool = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='kill_collections'",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0)
            > 0;

        if !table_exists {
            return Ok((false, 0, 0));
        }

        // Check if col_total column exists
        let col_total_exists: bool = conn
            .query_row(
                "SELECT count(*) FROM pragma_table_info('kill_collections') WHERE name='col_total'",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0)
            > 0;

        let query = if col_total_exists {
            "SELECT COUNT(*), MAX(col_total) FROM kill_collections WHERE demo_name = ?"
        } else {
            "SELECT COUNT(*), 0 FROM kill_collections WHERE demo_name = ?"
        };

        let (count, col_total): (i32, Option<i32>) =
            conn.query_row(query, params![demo_name], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?;

        Ok((count > 0, col_total.unwrap_or(0), count))
    }

    /// Check if a specific collection number exists for a demo
    pub fn has_collection(
        &self,
        demo_name: &str,
        col_num: i32,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        if !Path::new(&self.db_path).exists() {
            return Ok(false);
        }

        let conn = Connection::open(&self.db_path)?;

        // Check if table exists
        let table_exists: bool = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='kill_collections'",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0)
            > 0;

        if !table_exists {
            return Ok(false);
        }

        let count: i32 = conn.query_row(
            "SELECT COUNT(*) FROM kill_collections WHERE demo_name = ? AND collection_num = ?",
            params![demo_name, col_num],
            |row| row.get(0),
        )?;

        Ok(count > 0)
    }
}
