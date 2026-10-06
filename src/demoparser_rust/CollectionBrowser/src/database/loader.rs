use super::DatabaseInfo;
use crate::models::{CollectionEntry, TickFilterState};
use anyhow::{Context, Result};
use duckdb::{params, Connection};
use std::collections::HashSet;

/// Load collections from specific databases
pub fn load_collections(
    databases: &[DatabaseInfo],
    tick_filter: TickFilterState,
) -> Result<Vec<CollectionEntry>> {
    let mut all_collections = Vec::new();

    for db_info in databases {
        match load_from_database(db_info, tick_filter) {
            Ok(mut collections) => {
                all_collections.append(&mut collections);
            }
            Err(e) => {
                eprintln!("Warning: Failed to load {}: {}", db_info.path.display(), e);
            }
        }
    }

    Ok(all_collections)
}

/// Load collections from a single database file
fn load_from_database(
    db_info: &DatabaseInfo,
    tick_filter: TickFilterState,
) -> Result<Vec<CollectionEntry>> {
    let conn = Connection::open(&db_info.path).context(format!(
        "Failed to open database: {}",
        db_info.path.display()
    ))?;

    // Check what columns actually exist
    let mut columns = HashSet::new();
    let schema_query = "PRAGMA table_info(kill_collections)";
    if let Ok(mut schema_stmt) = conn.prepare(schema_query) {
        if let Ok(schema_rows) = schema_stmt.query_map([], |row| {
            // Column name is index 1
            row.get::<_, String>(1)
        }) {
            for col_result in schema_rows {
                if let Ok(col_name) = col_result {
                    columns.insert(col_name);
                }
            }
        }
    }

    // Dynamic query construction based on available columns
    // Handle potential missing columns in older DBs
    let util_col = if columns.contains("util_thrown") {
        "util_thrown"
    } else {
        "''"
    };
    let game_ver_col = if columns.contains("game_version") {
        "game_version"
    } else {
        "0"
    };
    let grenade_col = if columns.contains("GrenadeTraj") {
        "GrenadeTraj"
    } else {
        "0"
    };
    let move_kills_col = if columns.contains("movement_between_kills") {
        "movement_between_kills"
    } else {
        "''"
    };
    let created_col = if columns.contains("created_at") {
        "created_at"
    } else {
        "''"
    };
    let rel_path_col = if columns.contains("demo_relative_path") {
        "demo_relative_path"
    } else {
        "''"
    };
    let tick_data_col = if columns.contains("TickData") {
        "TickData"
    } else {
        "-1"
    };

    // New columns
    let hits_col = if columns.contains("hits") {
        "hits"
    } else {
        "0"
    };
    let misses_col = if columns.contains("misses") {
        "misses"
    } else {
        "0"
    };
    let hit_rate_col = if columns.contains("hit_rate") {
        "hit_rate"
    } else {
        "0.0"
    };
    let weapons_fmt_col = if columns.contains("weapons_formatted") {
        "weapons_formatted"
    } else {
        "''"
    };
    let drive_col = if columns.contains("demo_source_drive") {
        "demo_source_drive"
    } else {
        "''"
    };
    let col_total_col = if columns.contains("col_total") {
        "col_total"
    } else {
        "0"
    };

    let mut where_clause = String::new();
    match tick_filter {
        TickFilterState::All => {} // No filter
        TickFilterState::TickOnly => {
            if columns.contains("TickData") {
                where_clause = "WHERE TickData = 1".to_string();
            } else {
                // Column missing, so no rows can match TickData=1
                return Ok(Vec::new());
            }
        }
        TickFilterState::NoTick => {
            if columns.contains("TickData") {
                // Show everything that is NOT 1 (so 0, or NULL if any, though usually 0)
                where_clause = "WHERE TickData != 1".to_string();
            } else {
                // Column missing, effectively means TickData is not available for any row.
                // So we return all rows (matches "No Tick").
            }
        }
    }

    let query = format!(
        "SELECT
            steam_id, demo_name, round, type, collection_num, tick_duration,
            map_name, killer_index, killer_team, start_kill_tick, end_kill_tick,
            killer_name, killer_radius, victims_radius, killer_move_distance,
            victim_team, round_start_tick, round_end_tick, round_freeze_end,
            weapons, weapons_id, kill_ticks, victims_index, victims_names, {},
            tag, {},
            {}, {}, {}, {},
            {}, {}, {}, {}, {}, {}, {}
        FROM kill_collections
        {}
        ORDER BY collection_num",
        game_ver_col,
        util_col,
        grenade_col,
        move_kills_col,
        created_col,
        rel_path_col,
        hits_col,
        misses_col,
        hit_rate_col,
        weapons_fmt_col,
        tick_data_col,
        drive_col,
        col_total_col,
        where_clause
    );

    let mut stmt = conn.prepare(&query)?;

    let rows = stmt.query_map([], |row| {
        Ok(CollectionEntry {
            steam_id: row.get(0)?,
            demo_name: row.get(1)?,
            round: row.get(2)?,
            collection_type: row.get(3)?,
            collection_num: row.get(4)?,
            tick_duration: row.get(5)?,
            map_name: row.get(6)?,
            killer_index: row.get(7)?,
            killer_team: row.get(8)?,
            start_kill_tick: row.get(9)?,
            end_kill_tick: row.get(10)?,
            killer_name: row.get(11)?,
            killer_radius: row.get(12)?,
            victims_radius: row.get(13)?,
            killer_move_distance: row.get(14)?,
            victim_team: row.get(15)?,
            round_start_tick: row.get(16)?,
            round_end_tick: row.get(17)?,
            round_freeze_end: row.get(18)?,
            weapons: row.get(19)?,
            weapons_id: row.get(20)?,
            kill_ticks: row.get(21)?,
            victims_index: row.get(22)?,
            victims_names: row.get::<_, String>(23).unwrap_or_default(),
            game_version: row.get(24)?,
            tag: row.get::<_, String>(25).unwrap_or_default(),
            util_thrown: row
                .get::<_, String>(26)
                .unwrap_or_default()
                .replace('[', "")
                .replace(']', "")
                .replace(';', " - "),
            grenade_traj: row.get::<_, i64>(27).unwrap_or(0) as i32,
            movement_between_kills: row.get::<_, String>(28).unwrap_or_default(),
            created_at: row.get::<_, String>(29).unwrap_or_default(),
            demo_relative_path: row.get::<_, String>(30).unwrap_or_default(),

            hits: row.get::<_, i32>(31).unwrap_or(0),
            misses: row.get::<_, i32>(32).unwrap_or(0),
            hit_rate: row.get::<_, f64>(33).unwrap_or(0.0),
            weapons_formatted: row
                .get::<_, String>(34)
                .unwrap_or_default()
                .replace('[', "")
                .replace(']', ""),
            tick_data: row.get::<_, i32>(35).unwrap_or(-1),

            // col_total loaded from initial query - read as i32, convert to Option (None if 0 or negative)
            // Note: drive_col is at index 36, col_total is at index 37
            col_total: {
                let val = row.get::<_, i32>(37).unwrap_or(0);
                if val > 0 {
                    Some(val)
                } else {
                    None
                }
            },
            killer_pos_x: None,
            killer_pos_y: None,
            killer_pos_z: None,
            killer_view_pitch: None,
            killer_view_yaw: None,
            ticks_between_kills: None,
            victim_distance: None,
            kill_weapon_ids: None,
            victim_pos_x: None,
            victim_pos_y: None,
            victim_pos_z: None,

            selected: false,
            details_loaded: false,
            source_db: db_info.path.to_string_lossy().to_string(),
            // Cached display strings - will be populated after initialization
            duration_display: String::new(),
            killer_radius_display: String::new(),
            victims_radius_display: String::new(),
            move_distance_display: String::new(),
            hit_rate_display: String::new(),
            weapons_display: String::new(),
        })
    })?;

    let mut collections: Vec<CollectionEntry> = Vec::new();
    for row_result in rows {
        match row_result {
            Ok(mut entry) => {
                entry.cache_display_strings();
                collections.push(entry);
            }
            Err(e) => {
                eprintln!("Error parsing row from {}: {}", db_info.path.display(), e);
            }
        }
    }

    Ok(collections)
}

/// Load detailed fields for a specific collection entry
pub fn load_details(mut entry: CollectionEntry) -> Result<CollectionEntry> {
    let conn = Connection::open(&entry.source_db)
        .context(format!("Failed to open database: {}", entry.source_db))?;

    // Check available columns
    let mut columns = HashSet::new();
    let schema_query = "PRAGMA table_info(kill_collections)";
    if let Ok(mut schema_stmt) = conn.prepare(schema_query) {
        if let Ok(schema_rows) = schema_stmt.query_map([], |row| row.get::<_, String>(1)) {
            for col_result in schema_rows {
                if let Ok(col_name) = col_result {
                    columns.insert(col_name);
                }
            }
        }
    }

    // Construct query for available fields
    // Note: col_total is already loaded in initial query, no need to reload it here
    let k_pos_x_col = if columns.contains("killer_pos_x") {
        "killer_pos_x"
    } else {
        "NULL"
    };
    let k_pos_y_col = if columns.contains("killer_pos_y") {
        "killer_pos_y"
    } else {
        "NULL"
    };
    let k_pos_z_col = if columns.contains("killer_pos_z") {
        "killer_pos_z"
    } else {
        "NULL"
    };
    let k_pitch_col = if columns.contains("killer_view_pitch") {
        "killer_view_pitch"
    } else {
        "NULL"
    };
    let k_yaw_col = if columns.contains("killer_view_yaw") {
        "killer_view_yaw"
    } else {
        "NULL"
    };
    let ticks_between_col = if columns.contains("ticks_between_kills") {
        "ticks_between_kills"
    } else {
        "NULL"
    };
    let victim_dist_col = if columns.contains("victim_distance") {
        "victim_distance"
    } else {
        "NULL"
    };
    let kill_weapon_ids_col = if columns.contains("kill_weapon_ids") {
        "kill_weapon_ids"
    } else {
        "NULL"
    };
    let v_pos_x_col = if columns.contains("victim_pos_x") {
        "victim_pos_x"
    } else {
        "NULL"
    };
    let v_pos_y_col = if columns.contains("victim_pos_y") {
        "victim_pos_y"
    } else {
        "NULL"
    };
    let v_pos_z_col = if columns.contains("victim_pos_z") {
        "victim_pos_z"
    } else {
        "NULL"
    };

    let query = format!(
        "SELECT {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}
            FROM kill_collections
            WHERE steam_id = ? AND demo_name = ? AND round = ?",
        k_pos_x_col,
        k_pos_y_col,
        k_pos_z_col,
        k_pitch_col,
        k_yaw_col,
        ticks_between_col,
        victim_dist_col,
        kill_weapon_ids_col,
        v_pos_x_col,
        v_pos_y_col,
        v_pos_z_col
    );

    let mut stmt = conn.prepare(&query)?;

    let result = stmt.query_row(
        params![&entry.steam_id, &entry.demo_name, entry.round],
        |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, Option<String>>(9)?,
                row.get::<_, Option<String>>(10)?,
            ))
        },
    )?;

    // col_total is already loaded from initial query, no need to reload
    entry.killer_pos_x = result.0;
    entry.killer_pos_y = result.1;
    entry.killer_pos_z = result.2;
    entry.killer_view_pitch = result.3;
    entry.killer_view_yaw = result.4;
    entry.ticks_between_kills = result.5;
    entry.victim_distance = result.6;
    entry.kill_weapon_ids = result.7;
    entry.victim_pos_x = result.8;
    entry.victim_pos_y = result.9;
    entry.victim_pos_z = result.10;
    entry.details_loaded = true;

    Ok(entry)
}
