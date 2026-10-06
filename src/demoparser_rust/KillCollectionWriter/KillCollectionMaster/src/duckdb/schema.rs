use duckdb::{Connection, Result as DuckResult};
use std::collections::{HashMap, HashSet};

/// Schema revision this build expects. Bump when adding a migration, and extend
/// `initialize_tables` to apply it only when `current_schema_version` is lower.
///
/// v1 — additive columns previously applied by an unconditional ALTER loop
/// v2 — `tick_assets`: one row per (collection, output format)
/// v3 — compact S2R replay-authority summaries on tick_assets
/// v4 — `demo_sources`: original source provenance, independent of JSON sidecars
/// v5 — transport-independent canonical `demo_name` across all catalogue tables
/// v6 — self-describing DEM clip timeline and source provenance on `tick_assets`
pub const SCHEMA_VERSION: i64 = 6;

/// Configure DuckDB for optimal write performance
pub fn configure_performance(conn: &Connection) -> DuckResult<()> {
    // Note: PRAGMA synchronous is SQLite-specific and not supported by DuckDB
    // DuckDB handles transaction safety differently and doesn't need this setting

    // Increase memory limit for better performance
    conn.execute("PRAGMA memory_limit = '4GB'", [])?;

    // Increase checkpoint threshold to reduce I/O
    conn.execute("PRAGMA checkpoint_threshold = '1GB'", [])?;

    // Enable parallelism
    conn.execute("PRAGMA threads = 4", [])?;

    Ok(())
}

/// Initialize the database tables with proper constraints for upsert operations
pub fn initialize_tables(conn: &Connection) -> DuckResult<()> {
    // Configure performance settings
    configure_performance(conn)?;

    // Create manifest_info table with primary key and base path
    conn.execute(
        "CREATE TABLE IF NOT EXISTS manifest_info (
            type TEXT,
            folder TEXT,
            total_demos INTEGER,
            total_collections INTEGER,
            unique_steam_ids INTEGER,
            demo_base_path TEXT,
            last_updated TEXT,
            PRIMARY KEY (type, folder)
        )",
        [],
    )?;

    // Create map_totals table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS map_totals (
            map_name TEXT PRIMARY KEY,
            count INTEGER
        )",
        [],
    )?;

    // Create weapon_totals table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS weapon_totals (
            weapon TEXT PRIMARY KEY,
            count INTEGER,
            exclusive_count INTEGER
        )",
        [],
    )?;

    // Create kill_collections table with composite primary key for UP SERT
    // Added new columns for path optimization, grenade mode, and per-kill details
    conn.execute(
        "CREATE TABLE IF NOT EXISTS kill_collections (
            steam_id TEXT,
            demo_name TEXT,
            round INTEGER,
            type TEXT,
            collection_num INTEGER,
            col_total INTEGER,
            tick_duration INTEGER,
            map_name TEXT,
            game_version INTEGER,
            TickData INTEGER,
            GrenadeTraj INTEGER,
            tag TEXT,
            created_at TEXT,
            demo_relative_path TEXT,
            demo_source_drive TEXT,

            -- Killer Info
            killer_index INTEGER,
            killer_team TEXT,
            killer_name TEXT,
            killer_radius REAL,
            killer_move_distance REAL,
            start_kill_tick INTEGER,
            end_kill_tick INTEGER,

            -- Victim Info
            victim_team TEXT,
            victims_radius REAL,

            -- Round Info
            round_start_tick INTEGER,
            round_end_tick INTEGER,
            round_freeze_end INTEGER,

            -- Weapon & Kill summary
            weapons TEXT,
            weapons_id TEXT,
            kill_ticks TEXT,
            victims_index TEXT,
            victims_names TEXT,
            util_thrown TEXT,

            -- TickByTick Stats
            hits INTEGER,
            misses INTEGER,
            hit_rate REAL,
            weapons_formatted TEXT,

            -- Per-Kill Details (Arrays)
            killer_pos_x TEXT,
            killer_pos_y TEXT,
            killer_pos_z TEXT,
            killer_view_pitch TEXT,
            killer_view_yaw TEXT,
            victim_pos_x TEXT,
            victim_pos_y TEXT,
            victim_pos_z TEXT,
            victim_distance TEXT,
            ticks_between_kills TEXT,
            movement_between_kills TEXT,
            kill_weapon_ids TEXT,

            PRIMARY KEY (steam_id, demo_name, round)
        )",
        [],
    )?;

    // Track which schema changes have been applied so they are not retried per connection
    conn.execute(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY,
            description TEXT,
            applied_at TIMESTAMP
        )",
        [],
    )?;

    let applied = current_schema_version(conn)?;

    // v1 — additive columns previously handled by an unconditional ALTER loop
    if applied < 1 {
        ensure_columns_exist(conn)?;
        record_schema_version(conn, 1, "additive columns through v1")?;
    }

    // v2 — per-format tick asset records
    if applied < 2 {
        create_tick_assets_table(conn)?;
        record_schema_version(conn, 2, "tick_assets table")?;
    }

    if applied < 3 {
        ensure_tick_asset_authority_columns(conn)?;
        record_schema_version(conn, 3, "S2R authority summaries")?;
    }

    if applied < 4 {
        create_demo_sources_table(conn)?;
        record_schema_version(conn, 4, "demo_sources table")?;
    }

    if applied < 5 {
        migrate_canonical_demo_names(conn)?;
    }

    if applied < 6 {
        migrate_dem_timeline_columns(conn)?;
    }

    Ok(())
}

fn migration_error(message: impl Into<String>) -> duckdb::Error {
    duckdb::Error::ToSqlConversionFailure(Box::new(std::io::Error::other(message.into())))
}

fn canonical_demo_sql(column: &str) -> String {
    format!(
        "lower(CASE
            WHEN lower({column}) LIKE '%.dem.zst' THEN left({column}, length({column}) - 8)
            WHEN lower({column}) LIKE '%.dem.gz'  THEN left({column}, length({column}) - 7)
            WHEN lower({column}) LIKE '%.zst'     THEN left({column}, length({column}) - 4)
            WHEN lower({column}) LIKE '%.gz'      THEN left({column}, length({column}) - 3)
            WHEN lower({column}) LIKE '%.dem'     THEN left({column}, length({column}) - 4)
            ELSE {column}
         END)"
    )
}

/// Canonicalize the shared demo identity transactionally.
///
/// A collision is refused before any write. Provenance is the one exception only when every
/// physical fact is identical; those rows differ solely because an older writer included a
/// transport suffix, so keeping the lexicographically first spelling loses no provenance.
fn migrate_canonical_demo_names(conn: &Connection) -> DuckResult<()> {
    let mut collection_keys = HashSet::new();
    let mut statement = conn.prepare("SELECT steam_id, demo_name, round FROM kill_collections")?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })?;
    for row in rows {
        let (steam_id, demo_name, round) = row?;
        let key = (
            steam_id,
            interface::utils::parser_utils::canonical_demo_name(&demo_name),
            round,
        );
        if !collection_keys.insert(key.clone()) {
            return Err(migration_error(format!(
                "schema v5 demo identity collision in kill_collections: {:?}",
                key
            )));
        }
    }
    drop(statement);

    let mut asset_keys = HashSet::new();
    if existing_columns(conn, "tick_assets")?.contains("demo_name") {
        let mut statement =
            conn.prepare("SELECT demo_name, collection_num, format FROM tick_assets")?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (demo_name, collection_num, format) = row?;
            let key = (
                interface::utils::parser_utils::canonical_demo_name(&demo_name),
                collection_num,
                format,
            );
            if !asset_keys.insert(key.clone()) {
                return Err(migration_error(format!(
                    "schema v5 demo identity collision in tick_assets: {:?}",
                    key
                )));
            }
        }
    }

    let mut provenance: HashMap<String, (String, i64, i64, Vec<String>)> = HashMap::new();
    if existing_columns(conn, "demo_sources")?.contains("demo_name") {
        let mut statement = conn
            .prepare("SELECT demo_name, source_path, size_bytes, modified_ns FROM demo_sources")?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?;
        for row in rows {
            let (demo_name, source_path, size_bytes, modified_ns) = row?;
            let identity = interface::utils::parser_utils::canonical_demo_name(&demo_name);
            match provenance.get_mut(&identity) {
                None => {
                    provenance.insert(
                        identity,
                        (source_path, size_bytes, modified_ns, vec![demo_name]),
                    );
                }
                Some((stored_path, stored_size, stored_modified, names)) => {
                    if stored_path != &source_path
                        || *stored_size != size_bytes
                        || *stored_modified != modified_ns
                    {
                        return Err(migration_error(format!(
                            "schema v5 conflicting provenance for demo identity {}",
                            identity
                        )));
                    }
                    names.push(demo_name);
                }
            }
        }
    }

    conn.execute("BEGIN TRANSACTION", [])?;
    let result = (|| -> DuckResult<()> {
        // Remove redundant spellings before updating the primary key. The payload equality check
        // above proves that only `demo_name`/`written_at` differ.
        for (_identity, (_path, _size, _modified, names)) in &provenance {
            if names.len() <= 1 {
                continue;
            }
            let mut names = names.clone();
            names.sort();
            for redundant in names.iter().skip(1) {
                conn.execute(
                    "DELETE FROM demo_sources WHERE demo_name = ?",
                    duckdb::params![redundant],
                )?;
            }
        }

        conn.execute(
            &format!(
                "UPDATE kill_collections SET demo_name = {}",
                canonical_demo_sql("demo_name")
            ),
            [],
        )?;
        conn.execute(
            &format!(
                "UPDATE tick_assets SET demo_name = {}",
                canonical_demo_sql("demo_name")
            ),
            [],
        )?;
        conn.execute(
            &format!(
                "UPDATE demo_sources SET demo_name = {}",
                canonical_demo_sql("demo_name")
            ),
            [],
        )?;
        record_schema_version(conn, 5, "transport-independent canonical demo identity")?;
        Ok(())
    })();
    if let Err(error) = result {
        let _ = conn.execute("ROLLBACK", []);
        return Err(error);
    }
    if let Err(error) = conn.execute("COMMIT", []) {
        let _ = conn.execute("ROLLBACK", []);
        return Err(error);
    }
    Ok(())
}

pub fn create_demo_sources_table(conn: &Connection) -> DuckResult<()> {
    // `demo_name` is the canonical transport-independent identity shared by kill_collections and
    // tick_assets. The physical container and its suffix live in source_path instead.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS demo_sources (
            demo_name TEXT PRIMARY KEY,
            source_path TEXT NOT NULL,
            size_bytes BIGINT NOT NULL,
            modified_ns BIGINT NOT NULL,
            written_at TIMESTAMP
        )",
        [],
    )?;
    Ok(())
}

/// One row per (collection, output format).
///
/// The collection row's single `TickData` flag cannot say which formats exist, what
/// version they were written at, or whether the file on disk is still the one recorded.
/// Keyed on `collection_num` rather than `round`, because two players can each produce a
/// collection in the same round.
pub fn create_tick_assets_table(conn: &Connection) -> DuckResult<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS tick_assets (
            demo_name TEXT NOT NULL,
            collection_num INTEGER NOT NULL,
            collection_type TEXT,
            folder TEXT,
            format TEXT NOT NULL,
            format_version INTEGER,
            path TEXT,
            size_bytes BIGINT,
            checksum TEXT,
            status TEXT,
            grenade_traj INTEGER,
            authority_bytes BIGINT DEFAULT 0,
            agent_life_count INTEGER DEFAULT 0,
            weapon_lifetime_count INTEGER DEFAULT 0,
            inventory_delta_count INTEGER DEFAULT 0,
            world_weapon_delta_count INTEGER DEFAULT 0,
            checkpoint_tick INTEGER,
            logical_start_tick INTEGER,
            logical_end_tick INTEGER,
            source_path TEXT,
            source_bytes BIGINT,
            written_at TIMESTAMP,
            PRIMARY KEY (demo_name, collection_num, format)
        )",
        [],
    )?;
    Ok(())
}

fn migrate_dem_timeline_columns(conn: &Connection) -> DuckResult<()> {
    let existing = existing_columns(conn, "tick_assets")?;
    conn.execute("BEGIN TRANSACTION", [])?;
    let result = (|| -> DuckResult<()> {
        for (name, definition) in [
            ("checkpoint_tick", "INTEGER"),
            ("logical_start_tick", "INTEGER"),
            ("logical_end_tick", "INTEGER"),
            ("source_path", "TEXT"),
            ("source_bytes", "BIGINT"),
        ] {
            if !existing.contains(name) {
                conn.execute(
                    &format!("ALTER TABLE tick_assets ADD COLUMN {name} {definition}"),
                    [],
                )?;
            }
        }
        record_schema_version(conn, 6, "self-describing DEM clip timeline and source")?;
        Ok(())
    })();
    if let Err(error) = result {
        let _ = conn.execute("ROLLBACK", []);
        return Err(error);
    }
    if let Err(error) = conn.execute("COMMIT", []) {
        let _ = conn.execute("ROLLBACK", []);
        return Err(error);
    }
    Ok(())
}

fn ensure_tick_asset_authority_columns(conn: &Connection) -> DuckResult<()> {
    let existing = existing_columns(conn, "tick_assets")?;
    for (name, definition) in [
        ("authority_bytes", "BIGINT DEFAULT 0"),
        ("agent_life_count", "INTEGER DEFAULT 0"),
        ("weapon_lifetime_count", "INTEGER DEFAULT 0"),
        ("inventory_delta_count", "INTEGER DEFAULT 0"),
        ("world_weapon_delta_count", "INTEGER DEFAULT 0"),
    ] {
        if !existing.contains(name) {
            conn.execute(
                &format!("ALTER TABLE tick_assets ADD COLUMN {name} {definition}"),
                [],
            )?;
        }
    }
    Ok(())
}

/// Highest migration version recorded against this database, or 0 if none.
pub fn current_schema_version(conn: &Connection) -> DuckResult<i64> {
    conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )
}

fn record_schema_version(conn: &Connection, version: i64, description: &str) -> DuckResult<()> {
    conn.execute(
        "INSERT INTO schema_migrations (version, description, applied_at)
         VALUES (?, ?, CURRENT_TIMESTAMP)
         ON CONFLICT (version) DO NOTHING",
        duckdb::params![version, description],
    )?;
    Ok(())
}

/// Column names already present on `table`, read from the catalogue.
fn existing_columns(conn: &Connection, table: &str) -> DuckResult<HashSet<String>> {
    let mut stmt =
        conn.prepare("SELECT column_name FROM information_schema.columns WHERE table_name = ?")?;
    let rows = stmt.query_map(duckdb::params![table], |row| row.get::<_, String>(0))?;

    let mut names = HashSet::new();
    for row in rows {
        names.insert(row?);
    }
    Ok(names)
}

/// Add any columns this build expects that the database does not already have.
///
/// Previously this fired ~24 unconditional `ALTER TABLE ... ADD COLUMN` statements and
/// discarded every error with `let _ =`, on every single connection. That silently
/// swallowed real failures (a genuinely broken ALTER was indistinguishable from the
/// expected "column already exists"), and repeated the whole exercise forever because
/// nothing recorded that it had been done. Columns are now diffed against the catalogue,
/// only genuinely missing ones are added, errors propagate, and the result is recorded in
/// `schema_migrations` so subsequent connections skip it entirely.
pub fn ensure_columns_exist(conn: &Connection) -> DuckResult<()> {
    // List of new columns to check and add if missing
    let columns = vec![
        ("demo_relative_path", "TEXT"),
        ("GrenadeTraj", "INTEGER"),
        ("TickData", "INTEGER"),
        ("created_at", "TEXT"),
        ("killer_pos_x", "TEXT"),
        ("killer_pos_y", "TEXT"),
        ("killer_pos_z", "TEXT"),
        ("killer_view_pitch", "TEXT"),
        ("killer_view_yaw", "TEXT"),
        ("victim_pos_x", "TEXT"),
        ("victim_pos_y", "TEXT"),
        ("victim_pos_z", "TEXT"),
        ("victim_distance", "TEXT"),
        ("ticks_between_kills", "TEXT"),
        ("movement_between_kills", "TEXT"),
        ("kill_weapon_ids", "TEXT"),
        ("hits", "INTEGER"),
        ("misses", "INTEGER"),
        ("hit_rate", "REAL"),
        ("weapons_formatted", "TEXT"),
        ("col_total", "INTEGER"),
        ("demo_source_drive", "TEXT"),
        ("victims_names", "TEXT"),
    ];

    let present = existing_columns(conn, "kill_collections")?;
    for (col_name, col_type) in columns {
        if present.contains(col_name) {
            continue;
        }
        conn.execute(
            &format!(
                "ALTER TABLE kill_collections ADD COLUMN {} {}",
                col_name, col_type
            ),
            [],
        )?;
    }

    // Check manifest_info for demo_base_path
    if !existing_columns(conn, "manifest_info")?.contains("demo_base_path") {
        conn.execute(
            "ALTER TABLE manifest_info ADD COLUMN demo_base_path TEXT",
            [],
        )?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    #[test]
    fn initialising_a_new_database_records_the_schema_version() {
        let conn = fresh();
        initialize_tables(&conn).unwrap();

        assert_eq!(current_schema_version(&conn).unwrap(), SCHEMA_VERSION);
    }

    /// Every connection used to re-run ~24 ALTER statements. Running twice must be a no-op
    /// the second time, and must not error either way.
    #[test]
    fn initialising_twice_is_idempotent() {
        let conn = fresh();
        initialize_tables(&conn).unwrap();
        let after_first: i64 = conn
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))
            .unwrap();

        initialize_tables(&conn).unwrap();
        let after_second: i64 = conn
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))
            .unwrap();

        assert_eq!(current_schema_version(&conn).unwrap(), SCHEMA_VERSION);
        assert_eq!(
            after_first, after_second,
            "a second run must not record migrations again"
        );
        assert_eq!(
            after_first, SCHEMA_VERSION,
            "every migration up to SCHEMA_VERSION should be recorded once"
        );
    }

    #[test]
    fn tick_assets_is_created_and_recorded_as_v2() {
        let conn = fresh();
        initialize_tables(&conn).unwrap();

        assert!(existing_columns(&conn, "tick_assets")
            .unwrap()
            .contains("checksum"));
        let has_v2: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM schema_migrations WHERE version = 2",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(has_v2, 1);
    }

    #[test]
    fn v6_adds_dem_identity_card_columns_without_changing_asset_rows() {
        let conn = fresh();
        initialize_tables(&conn).unwrap();
        conn.execute(
            "INSERT INTO tick_assets (
                demo_name, collection_num, format, path, size_bytes, status
             ) VALUES ('match', 7, 'DEM', 'match_r4.dem', 123, 'complete')",
            [],
        )
        .unwrap();
        conn.execute("DELETE FROM schema_migrations WHERE version = 6", [])
            .unwrap();
        for column in [
            "checkpoint_tick",
            "logical_start_tick",
            "logical_end_tick",
            "source_path",
            "source_bytes",
        ] {
            conn.execute(&format!("ALTER TABLE tick_assets DROP COLUMN {column}"), [])
                .unwrap();
        }

        initialize_tables(&conn).unwrap();

        let columns = existing_columns(&conn, "tick_assets").unwrap();
        for column in [
            "checkpoint_tick",
            "logical_start_tick",
            "logical_end_tick",
            "source_path",
            "source_bytes",
        ] {
            assert!(columns.contains(column), "missing v6 column {column}");
        }
        let surviving: i64 = conn
            .query_row("SELECT COUNT(*) FROM tick_assets", [], |row| row.get(0))
            .unwrap();
        assert_eq!(surviving, 1);
        assert_eq!(current_schema_version(&conn).unwrap(), 6);
    }

    /// A database already at v1 must gain the later migrations without re-running v1.
    #[test]
    fn a_v1_database_is_upgraded_through_v3() {
        let conn = fresh();
        initialize_tables(&conn).unwrap();
        conn.execute("DELETE FROM schema_migrations WHERE version >= 2", [])
            .unwrap();
        conn.execute("DROP TABLE tick_assets", []).unwrap();
        assert_eq!(current_schema_version(&conn).unwrap(), 1);

        initialize_tables(&conn).unwrap();

        assert_eq!(current_schema_version(&conn).unwrap(), SCHEMA_VERSION);
        let columns = existing_columns(&conn, "tick_assets").unwrap();
        assert!(columns.contains("path"));
        assert!(columns.contains("inventory_delta_count"));
    }

    /// A database written by an older build lacks some columns; adoption must add exactly
    /// the missing ones and leave the existing data alone.
    #[test]
    fn a_legacy_database_gains_only_the_missing_columns() {
        let conn = fresh();
        conn.execute(
            "CREATE TABLE kill_collections (
                steam_id TEXT, demo_name TEXT, round INTEGER, type TEXT, collection_num INTEGER
            )",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO kill_collections VALUES ('7656', 'demo1', 5, 'ACE', 1)",
            [],
        )
        .unwrap();

        initialize_tables(&conn).unwrap();

        let cols = existing_columns(&conn, "kill_collections").unwrap();
        assert!(cols.contains("GrenadeTraj"), "missing column must be added");
        assert!(cols.contains("col_total"));
        assert!(
            cols.contains("steam_id"),
            "pre-existing column must survive"
        );

        let surviving: i64 = conn
            .query_row("SELECT COUNT(*) FROM kill_collections", [], |r| r.get(0))
            .unwrap();
        assert_eq!(surviving, 1, "existing rows must be preserved");
        assert_eq!(current_schema_version(&conn).unwrap(), SCHEMA_VERSION);
    }

    /// The old code could not distinguish "column already exists" from a genuinely broken
    /// statement, because it discarded every error. A real failure must now surface.
    #[test]
    fn a_broken_alter_now_surfaces_instead_of_being_swallowed() {
        let conn = fresh();
        // No kill_collections table at all, so the ALTERs cannot succeed.
        let result = ensure_columns_exist(&conn);

        assert!(
            result.is_err(),
            "a failing schema change must not be reported as success"
        );
    }

    #[test]
    fn existing_columns_reads_the_catalogue() {
        let conn = fresh();
        conn.execute("CREATE TABLE t (a INTEGER, b TEXT)", [])
            .unwrap();

        let cols = existing_columns(&conn, "t").unwrap();
        assert!(cols.contains("a") && cols.contains("b"));
        assert!(!cols.contains("c"));
    }

    #[test]
    fn v5_canonicalizes_all_three_tables_and_consolidates_identical_provenance() {
        let conn = fresh();
        initialize_tables(&conn).unwrap();
        conn.execute("DELETE FROM schema_migrations WHERE version >= 5", [])
            .unwrap();
        conn.execute(
            "INSERT INTO kill_collections (steam_id, demo_name, round, type, collection_num)
             VALUES ('1', 'Match.One.dem.gz', 4, 'ACE', 7)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tick_assets (demo_name, collection_num, format)
             VALUES ('Match.One.dem.gz', 7, 'DEM'), ('match.one', 7, 'S2R')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO demo_sources (demo_name, source_path, size_bytes, modified_ns)
             VALUES
             ('Match.One.dem.gz', 'D:/Match.One.dem.gz', 10, 20),
             ('match.one', 'D:/Match.One.dem.gz', 10, 20)",
            [],
        )
        .unwrap();

        initialize_tables(&conn).unwrap();

        let collection_name: String = conn
            .query_row("SELECT demo_name FROM kill_collections", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(collection_name, "match.one");
        let asset_names: Vec<String> = conn
            .prepare("SELECT demo_name FROM tick_assets ORDER BY format")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(asset_names, vec!["match.one", "match.one"]);
        let sources: (i64, String) = conn
            .query_row(
                "SELECT COUNT(*), MIN(demo_name) FROM demo_sources",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(sources, (1, "match.one".into()));
        assert_eq!(current_schema_version(&conn).unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn v5_refuses_asset_key_collisions_without_partial_changes() {
        let conn = fresh();
        initialize_tables(&conn).unwrap();
        conn.execute("DELETE FROM schema_migrations WHERE version >= 5", [])
            .unwrap();
        conn.execute(
            "INSERT INTO tick_assets (demo_name, collection_num, format)
             VALUES ('match.dem.gz', 1, 'S2R'), ('match.dem.zst', 1, 'S2R')",
            [],
        )
        .unwrap();

        assert!(initialize_tables(&conn).is_err());
        assert_eq!(current_schema_version(&conn).unwrap(), 4);
        let names: Vec<String> = conn
            .prepare("SELECT demo_name FROM tick_assets ORDER BY demo_name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(names, vec!["match.dem.gz", "match.dem.zst"]);
    }
}
