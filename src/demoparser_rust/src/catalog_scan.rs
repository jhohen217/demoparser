//! One read-only catalog scan per folder/partition, instead of reopening up to twelve
//! databases per source demo. Connections close before the writer phase begins.
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Default)]
struct Status {
    total: i64,
    last: i64,
    assets_current: bool,
}

pub fn completed_sources(entries: &[PathBuf], config: &config::AppConfig) -> HashSet<PathBuf> {
    let mut folders: HashMap<String, Vec<&PathBuf>> = HashMap::new();
    for path in entries {
        folders
            .entry(config::storage::source_folder(&config.paths.parser_output, path))
            .or_default()
            .push(path);
    }
    let mut completed = HashSet::new();
    for (folder, paths) in folders {
        let wanted: HashSet<String> = paths
            .iter()
            .flat_map(|path| crate::demo_database_names(path))
            .collect();
        let source_files: HashMap<String, &Path> = paths.iter().flat_map(|path|
            crate::demo_database_names(path).into_iter().map(move |name| (name, path.as_path()))).collect();
        let mut statuses: HashMap<String, Status> = HashMap::new();
        let mut invalid_assets = HashSet::new();
        let mut readable = true;
        for kind in ["ACE", "QUAD", "TRIPLE", "MULTI", "DOUBLE", "SINGLE"] {
            let database = config::storage::database_path(&config.paths.parser_output, kind, &folder);
            if !database.exists() {
                continue;
            }
            if scan_partition(
                &database,
                kind,
                &wanted,
                &source_files,
                config,
                &mut statuses,
                &mut invalid_assets,
            )
            .is_err()
            {
                // Unknown/legacy schemas and unreadable partitions require a fresh import.
                readable = false;
                break;
            }
        }
        if !readable {
            continue;
        }
        for path in paths {
            if crate::demo_database_names(path).iter().any(|name| {
                statuses.get(name).is_some_and(|state| {
                    (state.total == 0 || state.last == state.total)
                        && state.assets_current
                        && !invalid_assets.contains(name)
                })
            }) {
                completed.insert(path.clone());
            }
        }
    }
    completed
}

fn scan_partition(
    database: &Path,
    kind: &str,
    wanted: &HashSet<String>,
    source_files: &HashMap<String, &Path>,
    config: &config::AppConfig,
    statuses: &mut HashMap<String, Status>,
    invalid: &mut HashSet<String>,
) -> duckdb::Result<()> {
    let flags = duckdb::Config::default().access_mode(duckdb::AccessMode::ReadOnly)?;
    let conn = duckdb::Connection::open_with_flags(database, flags)?;
    let discovery_versions: HashMap<String, i64> = conn.prepare(
        "SELECT demo_name, version FROM collection_discovery").and_then(|mut query| {
            query.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<duckdb::Result<HashMap<_, _>>>()
        }).unwrap_or_default();
    let mut statement = conn.prepare(
        "SELECT demo_name, COALESCE(MAX(col_total), 0), MAX(collection_num)
         FROM kill_collections GROUP BY demo_name",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })?;
    let mut partition_names = HashSet::new();
    for row in rows {
        let (name, total, last) = row?;
        if !wanted.contains(&name) {
            continue;
        }
        partition_names.insert(name.clone());
        // Existing assets cannot prove that an older discovery pass found singles
        // and separate-tick doubles. Refresh those catalogs once with the new rules.
        let discovery_version = discovery_versions.get(&name).copied().unwrap_or(0);
        if discovery_version < interface::core::tick_processor::COLLECTION_DISCOVERY_VERSION {
            invalid.insert(name.clone());
        }
        let entry = statuses.entry(name).or_insert_with(|| Status {
            assets_current: true,
            ..Default::default()
        });
        entry.total = entry.total.max(total);
        entry.last = entry.last.max(last);
    }

    // Folder matching is sampled; cheap per-file metadata still catches changed
    // inputs under a previously registered path without hashing whole demos.
    if config::storage::folder_layout(&config.paths.parser_output) {
        let mut provenance = conn.prepare("SELECT demo_name, source_path, size_bytes, modified_ns FROM demo_sources")?;
        let rows = provenance.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?, row.get::<_, i64>(3)?)))?;
        let mut known = HashSet::new();
        for row in rows {
            let (name, _old_path, size, modified) = row?;
            let Some(source) = source_files.get(&name) else { continue; };
            known.insert(name.clone());
            let valid = std::fs::metadata(source).ok().is_some_and(|meta| {
                meta.len() as i64 == size && (meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_nanos() as i64) == Some(modified))
            });
            if !valid { invalid.insert(name); }
        }
        invalid.extend(partition_names.difference(&known).cloned());
    }

    let replay = config.parser.process_tick_data
        && match kind {
            "ACE" => config.parser.aces,
            "QUAD" => config.parser.quads,
            "TRIPLE" => config.parser.triples,
            "MULTI" => config.parser.multi,
            "DOUBLE" => config.parser.doubles,
            _ => config.parser.singles,
        };
    let trim = config.parser.trim_collection_rounds && config.is_trim_collection_type_enabled(kind);
    for (format, enabled, version) in [
        (
            "S2R",
            replay,
            crate::tick_by_tick::s2r_output::S2R_VERSION as i64,
        ),
        (
            "DEM",
            trim,
            crate::round_replay::LOCAL_DEM_VERSION as i64,
        ),
    ] {
        if !enabled {
            continue;
        }
        let mut statement = conn.prepare(
            "SELECT c.demo_name, a.path, a.size_bytes, a.status, a.format_version,
                    a.logical_start_tick, c.round_start_tick, c.round_freeze_end
             FROM kill_collections c LEFT JOIN tick_assets a
             ON c.demo_name = a.demo_name AND c.collection_num = a.collection_num AND a.format = ?
             WHERE c.type = ?",
        )?;
        let rows = statement.query_map(duckdb::params![format, kind], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, Option<i64>>(5)?,
                row.get::<_, Option<i64>>(6)?,
                row.get::<_, Option<i64>>(7)?,
            ))
        })?;
        // Several collections may share one round clip. Stat/read each physical file once.
        let mut checked = HashMap::new();
        for row in rows {
            let (name, path, bytes, status, stored_version, logical_start, round_start, freeze_end) = row?;
            if !wanted.contains(&name) || invalid.contains(&name) {
                continue;
            }
            let current = match (path, bytes, status, stored_version) {
                (Some(path), Some(bytes), Some(status), Some(stored_version))
                    if status == "complete" && stored_version == version =>
                {
                    *checked.entry((path.clone(), bytes)).or_insert_with(|| {
                        let path = config::storage::resolve_asset(&config.paths.parser_output, database, &path);
                        std::fs::metadata(&path)
                            .ok()
                            .is_some_and(|m| m.len() as i64 == bytes)
                            && if format == "S2R" {
                                crate::tick_by_tick::s2r_output::is_current_s2r_for(&path, config.parser.skip_buy_time, config.parser.pad_ticks)
                            } else {
                                demo_header_current(&path)
                            }
                    })
                }
                _ => false,
            };
            let expected_start = if config.parser.skip_buy_time { freeze_end.zip(round_start).map(|(f, s)| f.max(s)) } else { round_start };
            if !current || (format == "DEM" && (expected_start.is_none() || logical_start != expected_start)) {
                invalid.insert(name);
            }
        }
    }
    Ok(())
}

fn demo_header_current(path: &Path) -> bool {
    use std::io::Read;
    let mut magic = [0; 8];
    std::fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut magic))
        .is_ok()
        && &magic == b"PBDEMS2\0"
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn folder_cache_does_not_skip_a_changed_source() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = config::AppConfig::load_from_file(&Path::new(env!("CARGO_MANIFEST_DIR")).join("config.ini")).unwrap();
        config.paths.parser_output = dir.path().join("ANOMALOUS");
        config.parser.process_tick_data = false;
        config.parser.trim_collection_rounds = false;
        let source = dir.path().join("inputs/match.dem");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, b"original").unwrap();
        let folder = config::storage::source_folder(&config.paths.parser_output, &source);
        let db = config::storage::database_path(&config.paths.parser_output, "ACE", &folder);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let conn = duckdb::Connection::open(&db).unwrap();
        kill_collection_master::duckdb::schema::initialize_tables(&conn).unwrap();
        conn.execute("INSERT INTO kill_collections (steam_id, demo_name, round, collection_num, col_total, type) VALUES ('1', 'match', 1, 1, 1, 'ACE')", []).unwrap();
        let modified = std::fs::metadata(&source).unwrap().modified().unwrap().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as i64;
        conn.execute("INSERT INTO demo_sources (demo_name, source_path, size_bytes, modified_ns) VALUES ('match', ?, 8, ?)", duckdb::params![source.to_string_lossy(), modified]).unwrap();
        conn.execute("CREATE TABLE collection_discovery (demo_name VARCHAR PRIMARY KEY, version BIGINT)", []).unwrap();
        conn.execute("INSERT INTO collection_discovery VALUES ('match', ?)", duckdb::params![interface::core::tick_processor::COLLECTION_DISCOVERY_VERSION]).unwrap();
        drop(conn);
        assert_eq!(completed_sources(&[source.clone()], &config).len(), 1);
        std::fs::write(&source, b"changed source").unwrap();
        assert!(completed_sources(&[source], &config).is_empty());
    }

    #[test]
    fn incremental_scan_checks_trim_assets_and_incomplete_catalogs() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = config::AppConfig::load_from_file(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("config.ini"),
        )
        .unwrap();
        config.paths.parser_output = dir.path().join("catalog");
        config.parser.process_tick_data = false;
        config.parser.trim_collection_rounds = false;
        config.parser.overwrite = false;
        let source = dir.path().join("inputs/match.dem");
        let partial = dir.path().join("inputs/partial.dem");
        let folder =
            crate::demo_processor::extract_folder_name_from_path(&source.to_string_lossy());
        let masters = config.paths.parser_output.join("KillCollectionMaster");
        std::fs::create_dir_all(&masters).unwrap();
        let db = masters.join(format!("ACE_{folder}.duckdb"));
        let conn = duckdb::Connection::open(&db).unwrap();
        kill_collection_master::duckdb::schema::initialize_tables(&conn).unwrap();
        conn.execute("INSERT INTO kill_collections (steam_id, demo_name, round, collection_num, col_total, type) VALUES ('1', 'match', 1, 1, 1, 'ACE'), ('2', 'partial', 1, 1, 2, 'ACE')", []).unwrap();
        drop(conn);
        let entries = vec![source.clone(), partial];
        assert!(completed_sources(&entries, &config).is_empty(), "old discovery must be refreshed");
        let conn = duckdb::Connection::open(&db).unwrap();
        conn.execute("CREATE TABLE collection_discovery (demo_name VARCHAR PRIMARY KEY, version BIGINT)", []).unwrap();
        conn.execute("INSERT INTO collection_discovery VALUES ('match', ?), ('partial', ?)", duckdb::params![interface::core::tick_processor::COLLECTION_DISCOVERY_VERSION, interface::core::tick_processor::COLLECTION_DISCOVERY_VERSION]).unwrap();
        drop(conn);
        assert_eq!(
            completed_sources(&entries, &config),
            HashSet::from([source.clone()])
        );
        config.parser.trim_collection_rounds = true;
        config.parser.trim_aces = true;
        assert!(completed_sources(&entries, &config).is_empty());
        let clip = dir.path().join("clip.dem");
        std::fs::write(&clip, b"PBDEMS2\0fixture").unwrap();
        let conn = duckdb::Connection::open(&db).unwrap();
        conn.execute("INSERT INTO tick_assets (demo_name, collection_num, format, path, size_bytes, status, format_version) VALUES ('match', 1, 'DEM', ?, 15, 'complete', ?)",
            duckdb::params![clip.to_string_lossy(), crate::round_replay::LOCAL_DEM_VERSION]).unwrap();
        conn.execute("UPDATE kill_collections SET round_start_tick = 100, round_freeze_end = 200 WHERE demo_name = 'match'", []).unwrap();
        conn.execute("UPDATE tick_assets SET logical_start_tick = 200 WHERE demo_name = 'match'", []).unwrap();
        drop(conn);
        assert_eq!(
            completed_sources(&entries, &config),
            HashSet::from([source.clone()])
        );
        config.parser.skip_buy_time = false;
        assert!(completed_sources(&entries, &config).is_empty());
        config.parser.skip_buy_time = true;
        std::fs::write(&clip, b"corrupt").unwrap();
        assert!(completed_sources(&entries, &config).is_empty());
        config.parser.overwrite = true;
        let mut skipped = 0;
        assert_eq!(
            crate::filter_processed_demos(entries.clone(), &config, &mut skipped).unwrap(),
            entries
        );
        assert_eq!(skipped, 0);
    }
}
