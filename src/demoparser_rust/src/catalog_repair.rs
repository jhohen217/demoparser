//! Backfill catalogue schema, surviving source provenance, and verified legacy DEM assets.
//!
//! This lane never parses an original demo and never rewrites `kill_collections`. Existing clips
//! are independently verified by DemoWriter; legacy JSON reports are intentionally ignored.

use anyhow::{anyhow, bail, Context, Result};
use config::AppConfig;
use interface::models::demo_source::DemoSource;
use interface::models::tick_asset::{AssetFormat, AssetStatus, TickAsset};
use kill_collection_master::master_writer::MasterWriter;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

#[derive(Debug, Default, Clone, Serialize)]
pub struct RepairSummary {
    pub databases: usize,
    pub migrations_needed: usize,
    pub catalog_demos: usize,
    pub provenance_rows: usize,
    pub dem_rows_already_current: usize,
    pub dem_rows_repairable: usize,
    pub dem_rows_written: usize,
    pub dem_rows_missing_clip: usize,
    pub dem_rows_deferred: usize,
    pub unique_clips: usize,
    pub verified_clips: usize,
    pub validation_failures: usize,
    pub metadata_sources_inspected: usize,
    pub dem_metadata_rows_written: usize,
}

impl RepairSummary {
    fn add(&mut self, other: &Self) {
        self.databases += other.databases;
        self.migrations_needed += other.migrations_needed;
        self.catalog_demos += other.catalog_demos;
        self.provenance_rows += other.provenance_rows;
        self.dem_rows_already_current += other.dem_rows_already_current;
        self.dem_rows_repairable += other.dem_rows_repairable;
        self.dem_rows_written += other.dem_rows_written;
        self.dem_rows_missing_clip += other.dem_rows_missing_clip;
        self.dem_rows_deferred += other.dem_rows_deferred;
        self.unique_clips += other.unique_clips;
        self.verified_clips += other.verified_clips;
        self.validation_failures += other.validation_failures;
        self.metadata_sources_inspected += other.metadata_sources_inspected;
        self.dem_metadata_rows_written += other.dem_metadata_rows_written;
    }
}

#[derive(Debug)]
struct CatalogRow {
    demo_name: String,
    collection_num: i32,
    round: i32,
    grenade_traj: i32,
}

#[derive(Debug)]
struct MissingMetadataRow {
    database: PathBuf,
    demo_name: String,
    collection_num: i32,
    round: i32,
    clip_path: PathBuf,
    clip_bytes: i64,
    source_path: PathBuf,
    source_bytes: i64,
}

#[derive(Default)]
struct FolderIndex {
    sources: HashMap<String, PathBuf>,
    clips: HashMap<(String, i32), PathBuf>,
}

fn demo_identity(name: &str) -> Option<String> {
    let name = Path::new(name).file_name()?.to_string_lossy();
    let mut value = name.to_ascii_lowercase();
    for suffix in [".zst", ".gz"] {
        if value.ends_with(suffix) {
            value.truncate(value.len() - suffix.len());
            break;
        }
    }
    if value.ends_with(".dem") {
        value.truncate(value.len() - 4);
    }
    (!value.is_empty()).then_some(value)
}

fn clip_identity_and_round(name: &str) -> Option<(String, i32)> {
    let lower = name.to_ascii_lowercase();
    let stem = lower.strip_suffix(".dem")?;
    let (identity, round) = stem.rsplit_once("_r")?;
    if identity.is_empty() || round.is_empty() || !round.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let round = round.parse().ok()?;
    (round > 0).then(|| (identity.to_string(), round))
}

fn is_archive(path: &Path) -> bool {
    let lower = path.to_string_lossy().to_ascii_lowercase();
    lower.ends_with(".dem.gz") || lower.ends_with(".dem.zst")
}

fn build_folder_index(folder: &Path) -> Result<FolderIndex> {
    let mut source_candidates: HashMap<String, Vec<PathBuf>> = HashMap::new();
    let mut clip_candidates: HashMap<(String, i32), Vec<PathBuf>> = HashMap::new();
    for entry in
        fs::read_dir(folder).with_context(|| format!("could not enumerate {}", folder.display()))?
    {
        let path = entry?.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if let Some(key) = clip_identity_and_round(name) {
            clip_candidates.entry(key).or_default().push(path);
            continue;
        }
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".dem") || lower.ends_with(".dem.gz") || lower.ends_with(".dem.zst") {
            if let Some(identity) = demo_identity(name) {
                source_candidates.entry(identity).or_default().push(path);
            }
        }
    }

    let mut index = FolderIndex::default();
    for (identity, paths) in source_candidates {
        let archives = paths
            .iter()
            .filter(|path| is_archive(path))
            .collect::<Vec<_>>();
        let chosen = match archives.as_slice() {
            [archive] => Some((*archive).clone()),
            [] if paths.len() == 1 => Some(paths[0].clone()),
            [] => None,
            _ => None,
        };
        if let Some(path) = chosen {
            index.sources.insert(identity, path);
        } else {
            eprintln!(
                "Warning: ambiguous source identity {}: {}",
                identity,
                paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
    for (key, paths) in clip_candidates {
        if paths.len() == 1 {
            index.clips.insert(key, paths[0].clone());
        } else {
            eprintln!(
                "Warning: ambiguous clip identity {:?}: {}",
                key,
                paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
    Ok(index)
}

fn database_identity(path: &Path) -> Option<(String, String)> {
    config::storage::database_identity(path)
}

fn table_exists(connection: &duckdb::Connection, table: &str) -> Result<bool> {
    Ok(connection.query_row(
        "SELECT COUNT(*) > 0 FROM information_schema.tables WHERE table_name = ?",
        duckdb::params![table],
        |row| row.get(0),
    )?)
}

fn source_metadata(demo_name: &str, path: &Path) -> Result<DemoSource> {
    let canonical = fs::canonicalize(path)
        .with_context(|| format!("could not canonicalize source {}", path.display()))?;
    let metadata = fs::metadata(&canonical)
        .with_context(|| format!("could not stat source {}", canonical.display()))?;
    let modified_ns = metadata
        .modified()?
        .duration_since(UNIX_EPOCH)
        .map_err(|error| anyhow!("source timestamp predates Unix epoch: {}", error))?
        .as_nanos()
        .try_into()
        .map_err(|_| anyhow!("source timestamp exceeds i64"))?;
    Ok(DemoSource {
        demo_name: demo_name.to_string(),
        source_path: canonical.to_string_lossy().to_string(),
        size_bytes: metadata
            .len()
            .try_into()
            .map_err(|_| anyhow!("source is too large"))?,
        modified_ns,
    })
}

pub fn repair_folder(
    output_root: &Path,
    source_folder: &Path,
    dry_run: bool,
    parse_check: bool,
    max_clips: Option<usize>,
) -> Result<RepairSummary> {
    let folder_name = config::storage::source_directory(output_root, source_folder);
    let index = build_folder_index(source_folder)?;
    let mut databases = config::storage::databases(output_root).into_iter()
        .filter(|path| {
            database_identity(path)
                .is_some_and(|(_, folder)| folder.eq_ignore_ascii_case(&folder_name))
        })
        .collect::<Vec<_>>();
    databases.sort();
    if databases.is_empty() {
        // A newly mounted folder has nothing to repair yet. Treat that as a successful no-op so
        // hosts can always run repair -> incremental import without racing a separate preflight
        // against the catalog directory.
        return Ok(RepairSummary::default());
    }

    let mut summary = RepairSummary::default();
    let mut inspected: HashMap<
        PathBuf,
        std::result::Result<demo_writer::VerifiedClipInspection, String>,
    > = HashMap::new();
    let mut unique_clips = HashSet::new();

    for database in databases {
        summary.databases += 1;
        let (collection_type, database_folder) = database_identity(&database).unwrap();
        let connection = duckdb::Connection::open(&database)?;
        let has_sources = table_exists(&connection, "demo_sources")?;
        if !has_sources {
            summary.migrations_needed += 1;
        }
        let mut rows = Vec::new();
        let mut demos = HashSet::new();
        let mut statement = connection.prepare(
            "SELECT DISTINCT demo_name, collection_num, round, COALESCE(GrenadeTraj, 0)
             FROM kill_collections ORDER BY demo_name, collection_num",
        )?;
        let mapped = statement.query_map([], |row| {
            Ok(CatalogRow {
                demo_name: row.get(0)?,
                collection_num: row.get::<_, i64>(1)? as i32,
                round: row.get::<_, i64>(2)? as i32,
                grenade_traj: row.get::<_, Option<i64>>(3)?.unwrap_or(0) as i32,
            })
        })?;
        for row in mapped {
            let row = row?;
            demos.insert(row.demo_name.clone());
            rows.push(row);
        }
        drop(statement);
        summary.catalog_demos += demos.len();

        let mut current_dem = HashMap::new();
        if table_exists(&connection, "tick_assets")? {
            let mut statement = connection.prepare(
                "SELECT demo_name, collection_num, format_version, path, size_bytes, status
                 FROM tick_assets WHERE format = 'DEM'",
            )?;
            let mapped = statement.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)? as i32,
                    row.get::<_, Option<i64>>(2)?.unwrap_or(0) as i32,
                    row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    row.get::<_, Option<i64>>(4)?.unwrap_or(0),
                    row.get::<_, Option<String>>(5)?.unwrap_or_default(),
                ))
            })?;
            for row in mapped {
                let (demo, collection, version, path, size, status) = row?;
                current_dem.insert(
                    (demo.to_ascii_lowercase(), collection),
                    (version, path, size, status),
                );
            }
        }
        drop(connection);

        let mut sources = Vec::new();
        for demo_name in demos {
            let Some(identity) = demo_identity(&demo_name) else {
                continue;
            };
            if let Some(path) = index.sources.get(&identity) {
                sources.push(source_metadata(&demo_name, path)?);
            }
        }
        summary.provenance_rows += sources.len();

        let mut assets = Vec::new();
        for row in rows {
            let mut inspected_format_version = 1;
            if let Some((version, path, size, status)) =
                current_dem.get(&(row.demo_name.to_ascii_lowercase(), row.collection_num))
            {
                inspected_format_version = (*version).max(1);
                let path = config::storage::resolve_asset(output_root, &database, path);
                if status == "complete"
                    && fs::metadata(&path)
                        .ok()
                        .map(|metadata| metadata.len() as i64)
                        == Some(*size)
                {
                    summary.dem_rows_already_current += 1;
                    continue;
                }
            }
            let Some(identity) = demo_identity(&row.demo_name) else {
                summary.dem_rows_missing_clip += 1;
                continue;
            };
            let Some(clip) = index.clips.get(&(identity, row.round)) else {
                summary.dem_rows_missing_clip += 1;
                continue;
            };
            summary.dem_rows_repairable += 1;
            unique_clips.insert(clip.clone());
            if dry_run {
                continue;
            }
            if !inspected.contains_key(clip)
                && max_clips.is_some_and(|limit| inspected.len() >= limit)
            {
                summary.dem_rows_deferred += 1;
                continue;
            }
            let inspection = inspected.entry(clip.clone()).or_insert_with(|| {
                demo_writer::inspect_verified_clip(clip, parse_check)
                    .map_err(|error| error.to_string())
            });
            let inspection = match inspection {
                Ok(inspection) => inspection,
                Err(error) => {
                    summary.validation_failures += 1;
                    eprintln!(
                        "Warning: clip validation failed for {}: {}",
                        clip.display(),
                        error
                    );
                    continue;
                }
            };
            assets.push(TickAsset {
                demo_name: row.demo_name,
                collection_num: row.collection_num,
                collection_type: collection_type.clone(),
                folder: database_folder.clone(),
                format: AssetFormat::Dem,
                // Structural inspection cannot prove which logical tail policy produced an
                // existing clip. Preserve a known version, and conservatively classify an
                // unregistered legacy clip as v1; only DemoWriter may mint the current v2
                // contract after writing the full S2R-compatible tail.
                format_version: inspected_format_version,
                path: fs::canonicalize(&inspection.path)?
                    .to_string_lossy()
                    .to_string(),
                size_bytes: inspection.output_bytes as i64,
                checksum: inspection.checksum.clone(),
                status: AssetStatus::Complete,
                grenade_traj: row.grenade_traj,
                authority_bytes: 0,
                agent_life_count: 0,
                weapon_lifetime_count: 0,
                inventory_delta_count: 0,
                world_weapon_delta_count: 0,
                checkpoint_tick: None,
                logical_start_tick: None,
                logical_end_tick: None,
                source_path: None,
                source_bytes: None,
            });
            summary.dem_rows_written += 1;
        }

        if !dry_run {
            let master_path = database.clone();
            let writer = MasterWriter::new(
                &master_path.to_string_lossy(),
                &collection_type,
                &database_folder,
            )?;
            writer.repair_catalog_assets(&sources, &assets, None)?;
        }
    }
    summary.unique_clips = unique_clips.len();
    summary.verified_clips = inspected.values().filter(|result| result.is_ok()).count();
    Ok(summary)
}

/// Resolve source-timeline metadata for complete DEM rows written before schema v6.
///
/// Every source and round is inspected before the first database write. Existing clips are not
/// rewritten, collection rows are not reparsed, and a partially populated identity card is
/// treated as a conflict rather than guessed over.
fn backfill_dem_metadata(
    output_root: &Path,
    source_folder: &Path,
    dry_run: bool,
) -> Result<RepairSummary> {
    let folder_name = config::storage::source_directory(output_root, source_folder);
    let mut databases = config::storage::databases(output_root).into_iter()
        .filter(|path| {
            database_identity(path)
                .is_some_and(|(_, folder)| folder.eq_ignore_ascii_case(&folder_name))
        })
        .collect::<Vec<_>>();
    databases.sort();

    let mut summary = RepairSummary::default();
    let mut rows = Vec::new();
    for database in &databases {
        let connection = duckdb::Connection::open(database)?;
        kill_collection_master::duckdb::schema::initialize_tables(&connection)
            .with_context(|| format!("could not migrate {}", database.display()))?;
        summary.databases += 1;
        let mut statement = connection.prepare(
            "SELECT a.demo_name, a.collection_num, k.round, a.path, a.size_bytes,
                    s.source_path, s.size_bytes,
                    a.checkpoint_tick, a.logical_start_tick, a.logical_end_tick,
                    a.source_path, a.source_bytes
             FROM tick_assets a
             INNER JOIN kill_collections k
               ON k.demo_name = a.demo_name AND k.collection_num = a.collection_num
             INNER JOIN demo_sources s ON s.demo_name = a.demo_name
             WHERE a.format = 'DEM' AND a.status = 'complete' AND a.format_version < 5
               AND (a.checkpoint_tick IS NULL OR a.logical_start_tick IS NULL
                    OR a.logical_end_tick IS NULL OR a.source_path IS NULL
                    OR a.source_bytes IS NULL)
             ORDER BY a.demo_name, a.collection_num",
        )?;
        let mapped = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)? as i32,
                row.get::<_, i64>(2)? as i32,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, Option<i64>>(7)?,
                row.get::<_, Option<i64>>(8)?,
                row.get::<_, Option<i64>>(9)?,
                row.get::<_, Option<String>>(10)?,
                row.get::<_, Option<i64>>(11)?,
            ))
        })?;
        for mapped_row in mapped {
            let (
                demo_name,
                collection_num,
                round,
                clip_path,
                clip_bytes,
                source_path,
                source_bytes,
                checkpoint_tick,
                logical_start_tick,
                logical_end_tick,
                asset_source_path,
                asset_source_bytes,
            ) = mapped_row?;
            let present = [
                checkpoint_tick.is_some(),
                logical_start_tick.is_some(),
                logical_end_tick.is_some(),
                asset_source_path.is_some(),
                asset_source_bytes.is_some(),
            ];
            if present.iter().any(|value| *value) {
                bail!(
                    "refusing partial DEM metadata in {}: {} collection {}",
                    database.display(),
                    demo_name,
                    collection_num
                );
            }
            let clip_path = config::storage::resolve_asset(output_root, database, &clip_path);
            let actual_clip_bytes = fs::metadata(&clip_path)
                .with_context(|| format!("could not stat clip {}", clip_path.display()))?
                .len();
            if i64::try_from(actual_clip_bytes).ok() != Some(clip_bytes) {
                bail!(
                    "clip size changed for {}: catalog {} bytes, disk {} bytes",
                    clip_path.display(),
                    clip_bytes,
                    actual_clip_bytes
                );
            }
            let source_path = fs::canonicalize(config::storage::resolve_asset(output_root, database, &source_path))
                .with_context(|| format!("could not resolve source {}", source_path))?;
            let actual_source_bytes = fs::metadata(&source_path)?.len();
            if i64::try_from(actual_source_bytes).ok() != Some(source_bytes) {
                bail!(
                    "source size changed for {}: catalog {} bytes, disk {} bytes",
                    source_path.display(),
                    source_bytes,
                    actual_source_bytes
                );
            }
            rows.push(MissingMetadataRow {
                database: database.clone(),
                demo_name,
                collection_num,
                round,
                clip_path,
                clip_bytes,
                source_path,
                source_bytes,
            });
        }
    }
    if rows.is_empty() {
        return Ok(summary);
    }

    let mut sources: BTreeMap<String, (PathBuf, i64, BTreeSet<i32>)> = BTreeMap::new();
    for row in &rows {
        let key = row.source_path.to_string_lossy().to_ascii_lowercase();
        let entry = sources
            .entry(key)
            .or_insert_with(|| (row.source_path.clone(), row.source_bytes, BTreeSet::new()));
        if entry.1 != row.source_bytes {
            bail!("conflicting source sizes for {}", row.source_path.display());
        }
        entry.2.insert(row.round);
    }

    let mut resolved = HashMap::new();
    for (source_key, (source_path, source_bytes, rounds)) in &sources {
        let rounds = rounds.iter().copied().collect::<Vec<_>>();
        let temp = tempfile::Builder::new()
            .prefix("demoparser-metadata-backfill-")
            .tempdir()
            .context("could not create metadata backfill temporary directory")?;
        let lower = source_path.to_string_lossy().to_ascii_lowercase();
        let materialized = if lower.ends_with(".dem.gz") || lower.ends_with(".dem.zst") {
            let temp_path = temp.path().to_path_buf();
            demoparser::decompression::decompress_file(source_path, Some(&temp_path))
                .with_context(|| format!("could not decompress {}", source_path.display()))?
        } else if lower.ends_with(".dem") {
            source_path.clone()
        } else {
            bail!("unsupported source transport {}", source_path.display());
        };
        let mut clips = rows.iter()
            .filter(|row| row.source_path.to_string_lossy().to_ascii_lowercase() == *source_key)
            .map(|row| (row.round, row.clip_path.clone())).collect::<Vec<_>>();
        clips.sort();
        clips.dedup();
        let metadata = demo_writer::inspect_existing_round_clips(&materialized, &clips)
            .with_context(|| {
                format!(
                    "could not resolve round metadata from {}",
                    source_path.display()
                )
            })?;
        if metadata.len() != rounds.len() {
            bail!(
                "resolved {} of {} rounds from {}",
                metadata.len(),
                rounds.len(),
                source_path.display()
            );
        }
        for item in metadata {
            resolved.insert(
                (source_key.clone(), item.round),
                (
                    item.checkpoint_tick,
                    item.logical_start_tick,
                    item.logical_end_tick,
                    source_path.to_string_lossy().to_string(),
                    *source_bytes,
                ),
            );
        }
        summary.metadata_sources_inspected += 1;
    }

    if dry_run {
        return Ok(summary);
    }
    let mut by_database: BTreeMap<PathBuf, Vec<&MissingMetadataRow>> = BTreeMap::new();
    for row in &rows {
        by_database
            .entry(row.database.clone())
            .or_default()
            .push(row);
    }
    for (database, database_rows) in by_database {
        let connection = duckdb::Connection::open(&database)?;
        connection.execute("BEGIN TRANSACTION", [])?;
        let result = (|| -> Result<usize> {
            let mut updated = 0;
            for row in database_rows {
                let source_key = row.source_path.to_string_lossy().to_ascii_lowercase();
                let (checkpoint, logical_start, logical_end, source_path, source_bytes) =
                    resolved.get(&(source_key, row.round)).with_context(|| {
                        format!(
                            "round {} metadata missing for {}",
                            row.round,
                            row.source_path.display()
                        )
                    })?;
                let changed = connection.execute(
                    "UPDATE tick_assets
                     SET checkpoint_tick = ?, logical_start_tick = ?, logical_end_tick = ?,
                         source_path = ?, source_bytes = ?, written_at = CURRENT_TIMESTAMP
                     WHERE demo_name = ? AND collection_num = ? AND format = 'DEM'
                       AND checkpoint_tick IS NULL AND logical_start_tick IS NULL
                       AND logical_end_tick IS NULL AND source_path IS NULL
                       AND source_bytes IS NULL",
                    duckdb::params![
                        i64::from(*checkpoint),
                        i64::from(*logical_start),
                        i64::from(*logical_end),
                        source_path,
                        *source_bytes,
                        &row.demo_name,
                        i64::from(row.collection_num),
                    ],
                )?;
                if changed != 1 {
                    bail!(
                        "metadata update matched {} rows in {}: {} collection {}",
                        changed,
                        database.display(),
                        row.demo_name,
                        row.collection_num
                    );
                }
                let verified: i64 = connection.query_row(
                    "SELECT COUNT(*) FROM tick_assets
                     WHERE demo_name = ? AND collection_num = ? AND format = 'DEM'
                       AND checkpoint_tick = ? AND logical_start_tick = ?
                       AND logical_end_tick = ? AND source_path = ? AND source_bytes = ?
                       AND path = ? AND size_bytes = ?",
                    duckdb::params![
                        &row.demo_name,
                        i64::from(row.collection_num),
                        i64::from(*checkpoint),
                        i64::from(*logical_start),
                        i64::from(*logical_end),
                        source_path,
                        *source_bytes,
                        row.clip_path.to_string_lossy().as_ref(),
                        row.clip_bytes,
                    ],
                    |result| result.get(0),
                )?;
                if verified != 1 {
                    bail!(
                        "metadata update did not verify in {}: {} collection {}",
                        database.display(),
                        row.demo_name,
                        row.collection_num
                    );
                }
                updated += 1;
            }
            Ok(updated)
        })();
        match result {
            Ok(updated) => {
                connection.execute("COMMIT", [])?;
                summary.dem_metadata_rows_written += updated;
            }
            Err(error) => {
                let _ = connection.execute("ROLLBACK", []);
                return Err(error);
            }
        }
    }
    Ok(summary)
}

fn migrate_folder_catalog(output_root: &Path, source_folder: &Path) -> Result<RepairSummary> {
    let folder_name = config::storage::source_directory(output_root, source_folder);
    let mut databases = config::storage::databases(output_root).into_iter()
        .filter(|path| {
            database_identity(path)
                .is_some_and(|(_, folder)| folder.eq_ignore_ascii_case(&folder_name))
        })
        .collect::<Vec<_>>();
    databases.sort();

    let mut summary = RepairSummary::default();
    for database in databases {
        let connection = duckdb::Connection::open(&database)?;
        let before = if table_exists(&connection, "schema_migrations")? {
            connection.query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
                [],
                |row| row.get::<_, i64>(0),
            )?
        } else {
            0
        };
        if before < kill_collection_master::duckdb::schema::SCHEMA_VERSION {
            summary.migrations_needed += 1;
        }
        kill_collection_master::duckdb::schema::initialize_tables(&connection)
            .with_context(|| format!("could not migrate {}", database.display()))?;
        summary.databases += 1;
    }
    Ok(summary)
}

pub fn run_from(args: &[String]) -> Result<()> {
    let mut dry_run = false;
    let mut parse_check = true;
    let mut migrate_only = false;
    let mut backfill_metadata_only = false;
    let mut output_root = None;
    let mut max_clips = None;
    let mut folders = Vec::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--dry-run" => dry_run = true,
            "--no-parse-check" => parse_check = false,
            "--migrate-only" => migrate_only = true,
            "--backfill-dem-metadata" => backfill_metadata_only = true,
            "--output-root" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| anyhow!("--output-root requires a path"))?;
                output_root = Some(PathBuf::from(value));
            }
            "--max-clips" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| anyhow!("--max-clips requires a positive integer"))?;
                let parsed = value
                    .parse::<usize>()
                    .with_context(|| format!("invalid --max-clips value {}", value))?;
                if parsed == 0 {
                    bail!("--max-clips must be positive");
                }
                max_clips = Some(parsed);
            }
            value if value.starts_with("--") => bail!("unknown repair-catalog option {}", value),
            value => folders.push(PathBuf::from(value)),
        }
        index += 1;
    }
    if folders.is_empty() {
        bail!(
            "usage: Demoparser repair-catalog <source-folder> [...] [--output-root <path>] [--max-clips <n>] [--dry-run] [--no-parse-check] [--migrate-only | --backfill-dem-metadata]"
        );
    }
    if migrate_only && backfill_metadata_only {
        bail!("--migrate-only and --backfill-dem-metadata are mutually exclusive");
    }
    if backfill_metadata_only && max_clips.is_some() {
        bail!("--max-clips does not apply to --backfill-dem-metadata");
    }
    let config = AppConfig::load()?;
    let output_root = output_root.unwrap_or(config.paths.parser_output);
    let mut total = RepairSummary::default();
    for folder in folders {
        let summary = if migrate_only {
            migrate_folder_catalog(&output_root, &folder)?
        } else if backfill_metadata_only {
            backfill_dem_metadata(&output_root, &folder, dry_run)?
        } else {
            repair_folder(&output_root, &folder, dry_run, parse_check, max_clips)?
        };
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "folder": folder,
                "dry_run": dry_run,
                "migrate_only": migrate_only,
                "backfill_metadata_only": backfill_metadata_only,
                "summary": summary,
            }))?
        );
        total.add(&summary);
    }
    println!(
        "{}",
        serde_json::to_string(&serde_json::json!({"total": total}))?
    );
    if total.validation_failures > 0 {
        bail!("{} clip rows failed validation", total.validation_failures);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_only_terminal_round_suffix() {
        assert_eq!(
            clip_identity_and_round("match_r2_r17.dem"),
            Some(("match_r2".into(), 17))
        );
        assert_eq!(clip_identity_and_round("match_r0.dem"), None);
        assert_eq!(clip_identity_and_round("match_rX.dem"), None);
    }

    #[test]
    fn identity_removes_only_demo_and_archive_suffixes() {
        assert_eq!(demo_identity("match.one.dem.gz"), Some("match.one".into()));
        assert_eq!(demo_identity("match.one.dem"), Some("match.one".into()));
    }

    #[test]
    fn a_new_folder_without_catalog_partitions_is_a_successful_no_op() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("new-mount");
        let output = root.path().join("parsed");
        fs::create_dir_all(&source).unwrap();

        let summary = repair_folder(&output, &source, false, true, None).unwrap();

        assert_eq!(summary.databases, 0);
        assert_eq!(summary.catalog_demos, 0);
    }

    #[test]
    fn a_folder_without_its_own_partition_is_a_successful_no_op() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("new-mount");
        let catalog = root.path().join("parsed").join("KillCollectionMaster");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&catalog).unwrap();
        fs::write(catalog.join("triple-another-mount.duckdb"), b"placeholder").unwrap();

        let summary = repair_folder(
            root.path().join("parsed").as_path(),
            &source,
            false,
            true,
            None,
        )
        .unwrap();

        assert_eq!(summary.databases, 0);
        assert_eq!(summary.catalog_demos, 0);
    }
}
