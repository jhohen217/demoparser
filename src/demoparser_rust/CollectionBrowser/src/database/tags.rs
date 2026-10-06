use super::DatabaseInfo;
use crate::models::CollectionEntry;
use anyhow::{Context, Result};
use duckdb::{params, Connection};
use std::collections::{HashMap, HashSet};

/// Update tags for selected collections
pub fn update_tags(collections: &[&CollectionEntry]) -> Result<()> {
    // Group by source database
    let mut by_database: HashMap<String, Vec<&CollectionEntry>> = HashMap::new();
    for collection in collections {
        by_database
            .entry(collection.source_db.clone())
            .or_insert_with(Vec::new)
            .push(collection);
    }

    // Update each database
    for (db_path, entries) in by_database {
        let conn =
            Connection::open(&db_path).context(format!("Failed to open database: {}", db_path))?;

        conn.execute("BEGIN TRANSACTION", [])?;

        for entry in entries {
            conn.execute(
                "UPDATE kill_collections SET tag = ? WHERE steam_id = ? AND demo_name = ? AND round = ?",
                params![&entry.tag, &entry.steam_id, &entry.demo_name, entry.round],
            )?;
        }

        conn.execute("COMMIT", [])?;
    }

    Ok(())
}

/// Get all unique tags from databases
pub fn get_all_tags(databases: &[DatabaseInfo]) -> Result<Vec<String>> {
    let mut tags = HashSet::new();

    for db_info in databases {
        if let Ok(conn) = Connection::open(&db_info.path) {
            if let Ok(mut stmt) =
                conn.prepare("SELECT DISTINCT tag FROM kill_collections WHERE tag != ''")
            {
                if let Ok(rows) = stmt.query_map([], |row| row.get::<_, String>(0)) {
                    for tag_result in rows.flatten() {
                        tags.insert(tag_result);
                    }
                }
            }
        }
    }

    let mut tag_list: Vec<String> = tags.into_iter().collect();
    tag_list.sort();
    Ok(tag_list)
}
