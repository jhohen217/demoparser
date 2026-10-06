use duckdb::{params, Connection, Result as DuckResult};
use std::collections::HashMap;

/// Recalculate all metadata tables from the kill_collections data
pub fn recalculate_metadata(
    db_path: &str,
    collection_type: &str,
    folder: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let conn = Connection::open(db_path)?;

    // CRITICAL FIX: Initialize tables if they don't exist
    // This prevents crashes when recalculating metadata on newly created DuckDB files
    super::schema::initialize_tables(&conn)?;

    conn.execute("BEGIN TRANSACTION", [])?;
    update_manifest_info(&conn, collection_type, folder)?;
    update_map_totals(&conn, collection_type)?;
    update_weapon_totals(&conn, collection_type)?;
    conn.execute("COMMIT", [])?;
    println!("Recalculated metadata for DuckDB: {}", db_path);
    Ok(())
}

/// Update manifest info by recalculating from the database
pub fn update_manifest_info(
    conn: &Connection,
    collection_type: &str,
    folder: &str,
) -> DuckResult<()> {
    let (total_collections, total_demos, unique_steam_ids): (i64, i64, i64) = conn.query_row(
        "SELECT COUNT(*), COUNT(DISTINCT demo_name), COUNT(DISTINCT steam_id)
         FROM kill_collections WHERE type = ?",
        params![collection_type],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;

    // Preserve existing demo_base_path
    let _ = conn.execute(
        "INSERT OR IGNORE INTO manifest_info (type, folder, demo_base_path) VALUES (?, ?, '')",
        params![collection_type, folder],
    );

    let now = chrono::Utc::now().to_rfc3339();

    conn.execute(
        "UPDATE manifest_info SET
            total_demos = ?,
            total_collections = ?,
            unique_steam_ids = ?,
            last_updated = ?
            WHERE type = ? AND folder = ?",
        params![
            total_demos,
            total_collections,
            unique_steam_ids,
            &now,
            collection_type,
            folder,
        ],
    )?;

    Ok(())
}

/// Update map totals by recalculating from the database
pub fn update_map_totals(conn: &Connection, collection_type: &str) -> DuckResult<()> {
    conn.execute("DELETE FROM map_totals", [])?;
    conn.execute(
        "INSERT INTO map_totals (map_name, count)
            SELECT map_name, COUNT(*) as count
            FROM kill_collections
            WHERE type = ?
            GROUP BY map_name",
        params![collection_type],
    )?;
    Ok(())
}

/// Update weapon totals by recalculating from the database
pub fn update_weapon_totals(conn: &Connection, collection_type: &str) -> DuckResult<()> {
    conn.execute("DELETE FROM weapon_totals", [])?;

    let mut stmt = conn.prepare("SELECT weapons, type FROM kill_collections WHERE type = ?")?;

    let mut weapon_counts: HashMap<String, usize> = HashMap::new();
    let mut weapon_exclusive_counts: HashMap<String, usize> = HashMap::new();

    let rows = stmt.query_map(params![collection_type], |row| {
        let weapons_str: String = row.get(0)?;
        let coll_type: String = row.get(1)?;
        Ok((weapons_str, coll_type))
    })?;

    for row in rows {
        let (weapons_str, _coll_type) = row?;
        if weapons_str.is_empty() {
            continue;
        }

        let weapons: Vec<&str> = weapons_str.split(';').collect();

        for weapon in &weapons {
            if !weapon.is_empty() {
                *weapon_counts.entry(weapon.to_string()).or_insert(0) += 1;
            }
        }

        if !weapons.is_empty() {
            let unique_weapons: std::collections::HashSet<&&str> = weapons.iter().collect();
            if unique_weapons.len() == 1 {
                let weapon = weapons[0].to_string();
                *weapon_exclusive_counts.entry(weapon).or_insert(0) += 1;
            }
        }
    }

    for (weapon, count) in weapon_counts {
        let exclusive_count = weapon_exclusive_counts.get(&weapon).copied().unwrap_or(0);
        conn.execute(
            "INSERT INTO weapon_totals (weapon, count, exclusive_count) VALUES (?, ?, ?)",
            params![weapon, count as i64, exclusive_count as i64],
        )?;
    }

    Ok(())
}
