//! Reading and writing `tick_assets` rows.

use duckdb::{params, Connection, Result as DuckResult, Row};
use interface::models::tick_asset::{AssetStatus, TickAsset};
use std::collections::HashMap;

/// Identity of an asset row: the table's primary key.
type AssetKey = (String, i64, String);

/// The stored content of an asset row, excluding `written_at`.
///
/// Every payload column is compared, not just the path/size/checksum trio that identifies the
/// file. Skipping on a subset would leave a row whose file is unchanged but whose recorded
/// authority or lifetime counts are stale, which is a worse failure than an extra write.
#[derive(Debug, PartialEq)]
struct AssetPayload {
    collection_type: String,
    folder: String,
    format_version: i64,
    path: String,
    size_bytes: i64,
    checksum: String,
    status: String,
    grenade_traj: i64,
    authority_bytes: i64,
    agent_life_count: i64,
    weapon_lifetime_count: i64,
    inventory_delta_count: i64,
    world_weapon_delta_count: i64,
    checkpoint_tick: Option<i64>,
    logical_start_tick: Option<i64>,
    logical_end_tick: Option<i64>,
    source_path: Option<String>,
    source_bytes: Option<i64>,
}

impl AssetPayload {
    fn of(asset: &TickAsset) -> Self {
        Self {
            collection_type: asset.collection_type.clone(),
            folder: asset.folder.clone(),
            format_version: asset.format_version as i64,
            path: asset.path.clone(),
            size_bytes: asset.size_bytes,
            checksum: asset.checksum.clone(),
            status: asset.status.as_str().to_string(),
            grenade_traj: asset.grenade_traj as i64,
            authority_bytes: asset.authority_bytes,
            agent_life_count: asset.agent_life_count as i64,
            weapon_lifetime_count: asset.weapon_lifetime_count as i64,
            inventory_delta_count: asset.inventory_delta_count as i64,
            world_weapon_delta_count: asset.world_weapon_delta_count as i64,
            checkpoint_tick: asset.checkpoint_tick.map(i64::from),
            logical_start_tick: asset.logical_start_tick.map(i64::from),
            logical_end_tick: asset.logical_end_tick.map(i64::from),
            source_path: asset.source_path.clone(),
            source_bytes: asset.source_bytes,
        }
    }
}

fn key_of(asset: &TickAsset) -> AssetKey {
    (
        interface::utils::parser_utils::canonical_demo_name(&asset.demo_name),
        asset.collection_num as i64,
        asset.format.as_str().to_string(),
    )
}

/// Upsert asset rows. Re-running a collection replaces its rows rather than duplicating
/// them, so the table always reflects the most recent write attempt per format.
///
/// Rows whose stored content already matches are left untouched. `written_at` therefore means
/// "when this asset last changed" rather than "when an import last ran", which is what makes
/// it usable for finding the assets a run actually produced.
///
/// The writes share one transaction and one prepared statement. Previously each asset was its
/// own autocommitted statement, so a group of assets paid a commit per row.
pub fn record_tick_assets(conn: &Connection, assets: &[TickAsset]) -> DuckResult<()> {
    if assets.is_empty() {
        return Ok(());
    }

    let stored = load_payloads(conn, assets)?;
    let changed: Vec<&TickAsset> = assets
        .iter()
        .filter(|asset| stored.get(&key_of(asset)) != Some(&AssetPayload::of(asset)))
        .collect();

    if changed.is_empty() {
        return Ok(());
    }

    conn.execute("BEGIN TRANSACTION", [])?;
    match upsert_all(conn, &changed) {
        Ok(()) => {
            conn.execute("COMMIT", [])?;
            Ok(())
        }
        Err(e) => {
            // A partially applied asset group would claim files that may not exist. Report the
            // failure with nothing written rather than with some rows in place.
            let _ = conn.execute("ROLLBACK", []);
            Err(e)
        }
    }
}

/// Upsert changed asset rows inside a transaction owned by the caller. This is used when source
/// provenance and asset registration must share one commit.
pub fn record_tick_assets_in_transaction(
    conn: &Connection,
    assets: &[TickAsset],
) -> DuckResult<()> {
    if assets.is_empty() {
        return Ok(());
    }
    let stored = load_payloads(conn, assets)?;
    let changed = assets
        .iter()
        .filter(|asset| stored.get(&key_of(asset)) != Some(&AssetPayload::of(asset)))
        .collect::<Vec<_>>();
    upsert_all(conn, &changed)
}

/// Bulk upsert for catalogue repair, whose caller has already selected only missing/stale rows.
/// Staging avoids paying one primary-key conflict operation per asset, which becomes hours of
/// CPU time on folder-scale legacy repairs.
pub fn repair_tick_assets_in_transaction(
    conn: &Connection,
    assets: &[TickAsset],
) -> DuckResult<()> {
    upsert_all(conn, &assets.iter().collect::<Vec<_>>())
}

/// Append a batch once, then resolve its primary-key conflicts in a single SQL operation.
fn bulk_upsert(conn: &Connection, assets: &[&TickAsset]) -> DuckResult<()> {
    conn.execute(
        "CREATE TEMP TABLE repair_tick_assets AS SELECT
            demo_name, collection_num, collection_type, folder, format, format_version,
            path, size_bytes, checksum, status, grenade_traj, authority_bytes,
            agent_life_count, weapon_lifetime_count, inventory_delta_count,
            world_weapon_delta_count, checkpoint_tick, logical_start_tick,
            logical_end_tick, source_path, source_bytes
         FROM tick_assets WHERE 1=0",
        [],
    )?;
    {
        let mut appender = conn.appender("repair_tick_assets")?;
        for asset in assets {
            let demo_name = interface::utils::parser_utils::canonical_demo_name(&asset.demo_name);
            appender.append_row(params![
                &demo_name,
                asset.collection_num as i64,
                &asset.collection_type,
                &asset.folder,
                asset.format.as_str(),
                asset.format_version as i64,
                &asset.path,
                asset.size_bytes,
                &asset.checksum,
                asset.status.as_str(),
                asset.grenade_traj as i64,
                asset.authority_bytes,
                asset.agent_life_count as i64,
                asset.weapon_lifetime_count as i64,
                asset.inventory_delta_count as i64,
                asset.world_weapon_delta_count as i64,
                asset.checkpoint_tick.map(i64::from),
                asset.logical_start_tick.map(i64::from),
                asset.logical_end_tick.map(i64::from),
                &asset.source_path,
                asset.source_bytes,
            ])?;
        }
        appender.flush()?;
    }
    conn.execute(
        "INSERT INTO tick_assets (
            demo_name, collection_num, collection_type, folder, format, format_version,
            path, size_bytes, checksum, status, grenade_traj, authority_bytes,
            agent_life_count, weapon_lifetime_count, inventory_delta_count,
            world_weapon_delta_count, checkpoint_tick, logical_start_tick,
            logical_end_tick, source_path, source_bytes, written_at)
         SELECT *, CURRENT_TIMESTAMP FROM repair_tick_assets
         ON CONFLICT (demo_name, collection_num, format) DO UPDATE SET
            collection_type = excluded.collection_type,
            folder = excluded.folder,
            format_version = excluded.format_version,
            path = excluded.path,
            size_bytes = excluded.size_bytes,
            checksum = excluded.checksum,
            status = excluded.status,
            grenade_traj = excluded.grenade_traj,
            authority_bytes = excluded.authority_bytes,
            agent_life_count = excluded.agent_life_count,
            weapon_lifetime_count = excluded.weapon_lifetime_count,
            inventory_delta_count = excluded.inventory_delta_count,
            world_weapon_delta_count = excluded.world_weapon_delta_count,
            checkpoint_tick = excluded.checkpoint_tick,
            logical_start_tick = excluded.logical_start_tick,
            logical_end_tick = excluded.logical_end_tick,
            source_path = excluded.source_path,
            source_bytes = excluded.source_bytes,
            written_at = excluded.written_at",
        [],
    )?;
    conn.execute("DROP TABLE repair_tick_assets", [])?;
    Ok(())
}

fn upsert_all(conn: &Connection, assets: &[&TickAsset]) -> DuckResult<()> {
    if assets.is_empty() {
        return Ok(());
    }
    // Duplicate keys must retain sequential last-write semantics, including normalized names.
    // Small batches avoid staging overhead. Benchmarked against the retained prepared path.
    if assets.len() >= 8 {
        let mut keys = std::collections::HashSet::with_capacity(assets.len());
        if assets.iter().all(|asset| keys.insert(key_of(asset))) {
            return bulk_upsert(conn, assets);
        }
    }
    row_upsert(conn, assets)
}

fn row_upsert(conn: &Connection, assets: &[&TickAsset]) -> DuckResult<()> {
    let mut stmt = conn.prepare(
        "INSERT INTO tick_assets (
            demo_name, collection_num, collection_type, folder,
            format, format_version, path, size_bytes, checksum,
            status, grenade_traj, authority_bytes, agent_life_count,
            weapon_lifetime_count, inventory_delta_count, world_weapon_delta_count,
            checkpoint_tick, logical_start_tick, logical_end_tick, source_path, source_bytes,
            written_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP)
        ON CONFLICT (demo_name, collection_num, format) DO UPDATE SET
            collection_type = excluded.collection_type,
            folder          = excluded.folder,
            format_version  = excluded.format_version,
            path            = excluded.path,
            size_bytes      = excluded.size_bytes,
            checksum        = excluded.checksum,
            status          = excluded.status,
            grenade_traj    = excluded.grenade_traj,
            authority_bytes = excluded.authority_bytes,
            agent_life_count = excluded.agent_life_count,
            weapon_lifetime_count = excluded.weapon_lifetime_count,
            inventory_delta_count = excluded.inventory_delta_count,
            world_weapon_delta_count = excluded.world_weapon_delta_count,
            checkpoint_tick = excluded.checkpoint_tick,
            logical_start_tick = excluded.logical_start_tick,
            logical_end_tick = excluded.logical_end_tick,
            source_path = excluded.source_path,
            source_bytes = excluded.source_bytes,
            written_at      = excluded.written_at",
    )?;

    for asset in assets {
        let demo_name = interface::utils::parser_utils::canonical_demo_name(&asset.demo_name);
        stmt.execute(params![
            &demo_name,
            asset.collection_num as i64,
            &asset.collection_type,
            &asset.folder,
            asset.format.as_str(),
            asset.format_version as i64,
            &asset.path,
            asset.size_bytes,
            &asset.checksum,
            asset.status.as_str(),
            asset.grenade_traj as i64,
            asset.authority_bytes,
            asset.agent_life_count as i64,
            asset.weapon_lifetime_count as i64,
            asset.inventory_delta_count as i64,
            asset.world_weapon_delta_count as i64,
            asset.checkpoint_tick.map(i64::from),
            asset.logical_start_tick.map(i64::from),
            asset.logical_end_tick.map(i64::from),
            &asset.source_path,
            asset.source_bytes,
        ])?;
    }

    Ok(())
}

/// The rows already stored for the demos this batch touches.
///
/// A composite primary key does not guarantee prefix index scans. Bound each lookup
/// to this batch's demos without rescanning the table separately for every demo.
fn load_payloads(
    conn: &Connection,
    assets: &[TickAsset],
) -> DuckResult<HashMap<AssetKey, AssetPayload>> {
    let mut demos: Vec<String> = assets
        .iter()
        .map(|asset| interface::utils::parser_utils::canonical_demo_name(&asset.demo_name))
        .collect();
    demos.sort_unstable();
    demos.dedup();

    let mut stored = HashMap::new();
    for chunk in demos.chunks(256) {
        let placeholders = vec!["?"; chunk.len()].join(",");
        let mut stmt = conn.prepare(&format!(
            "SELECT collection_num, format, collection_type, folder, format_version, path,
                size_bytes, checksum, status, grenade_traj, authority_bytes,
                agent_life_count, weapon_lifetime_count, inventory_delta_count,
                world_weapon_delta_count, checkpoint_tick, logical_start_tick,
                logical_end_tick, source_path, source_bytes, demo_name
         FROM tick_assets WHERE demo_name IN ({placeholders})",
        ))?;
        let rows = stmt.query_map(duckdb::params_from_iter(chunk.iter()), |row| {
            Ok((
                text(row, 20)?,
                int(row, 0)?,
                text(row, 1)?,
                AssetPayload {
                    collection_type: text(row, 2)?,
                    folder: text(row, 3)?,
                    format_version: int(row, 4)?,
                    path: text(row, 5)?,
                    size_bytes: int(row, 6)?,
                    checksum: text(row, 7)?,
                    status: text(row, 8)?,
                    grenade_traj: int(row, 9)?,
                    authority_bytes: int(row, 10)?,
                    agent_life_count: int(row, 11)?,
                    weapon_lifetime_count: int(row, 12)?,
                    inventory_delta_count: int(row, 13)?,
                    world_weapon_delta_count: int(row, 14)?,
                    checkpoint_tick: optional_int(row, 15)?,
                    logical_start_tick: optional_int(row, 16)?,
                    logical_end_tick: optional_int(row, 17)?,
                    source_path: optional_text(row, 18)?,
                    source_bytes: optional_int(row, 19)?,
                },
            ))
        })?;

        for row in rows {
            let (demo, collection_num, format, payload) = row?;
            stored.insert((demo, collection_num, format), payload);
        }
    }

    Ok(stored)
}

#[cfg(test)]
pub(super) fn legacy_record_in_transaction(
    conn: &Connection,
    assets: &[TickAsset],
) -> DuckResult<()> {
    let stored = legacy_load_payloads(conn, assets)?;
    let changed = assets
        .iter()
        .filter(|asset| stored.get(&key_of(asset)) != Some(&AssetPayload::of(asset)))
        .collect::<Vec<_>>();
    row_upsert(conn, &changed)
}

#[cfg(test)]
/// The rows already stored for the demos this batch touches.
///
/// One prepared lookup per distinct demo rather than a scan of the whole table: the primary key
/// leads with `demo_name`, and a batch touches few demos relative to everything accumulated.
fn legacy_load_payloads(
    conn: &Connection,
    assets: &[TickAsset],
) -> DuckResult<HashMap<AssetKey, AssetPayload>> {
    let mut demos: Vec<String> = assets
        .iter()
        .map(|asset| interface::utils::parser_utils::canonical_demo_name(&asset.demo_name))
        .collect();
    demos.sort_unstable();
    demos.dedup();

    let mut stmt = conn.prepare(
        "SELECT collection_num, format, collection_type, folder, format_version, path,
                size_bytes, checksum, status, grenade_traj, authority_bytes,
                agent_life_count, weapon_lifetime_count, inventory_delta_count,
                world_weapon_delta_count, checkpoint_tick, logical_start_tick,
                logical_end_tick, source_path, source_bytes
         FROM tick_assets WHERE demo_name = ?",
    )?;

    let mut stored = HashMap::new();
    for demo in demos {
        let rows = stmt.query_map(params![&demo], |row| {
            Ok((
                int(row, 0)?,
                text(row, 1)?,
                AssetPayload {
                    collection_type: text(row, 2)?,
                    folder: text(row, 3)?,
                    format_version: int(row, 4)?,
                    path: text(row, 5)?,
                    size_bytes: int(row, 6)?,
                    checksum: text(row, 7)?,
                    status: text(row, 8)?,
                    grenade_traj: int(row, 9)?,
                    authority_bytes: int(row, 10)?,
                    agent_life_count: int(row, 11)?,
                    weapon_lifetime_count: int(row, 12)?,
                    inventory_delta_count: int(row, 13)?,
                    world_weapon_delta_count: int(row, 14)?,
                    checkpoint_tick: optional_int(row, 15)?,
                    logical_start_tick: optional_int(row, 16)?,
                    logical_end_tick: optional_int(row, 17)?,
                    source_path: optional_text(row, 18)?,
                    source_bytes: optional_int(row, 19)?,
                },
            ))
        })?;

        for row in rows {
            let (collection_num, format, payload) = row?;
            stored.insert((demo.to_string(), collection_num, format), payload);
        }
    }

    Ok(stored)
}

/// Every payload column but the key is nullable, and rows written before a column existed hold
/// NULL. Reading those as a default makes them compare unequal to any real payload, so such a
/// row is rewritten rather than mistaken for up to date.
fn text(row: &Row, index: usize) -> DuckResult<String> {
    Ok(row.get::<_, Option<String>>(index)?.unwrap_or_default())
}

fn int(row: &Row, index: usize) -> DuckResult<i64> {
    Ok(row.get::<_, Option<i64>>(index)?.unwrap_or_default())
}

fn optional_text(row: &Row, index: usize) -> DuckResult<Option<String>> {
    row.get(index)
}

fn optional_int(row: &Row, index: usize) -> DuckResult<Option<i64>> {
    row.get(index)
}

/// Count assets by status for a format, for reporting and for reconciling the database
/// against what is actually on disk.
pub fn count_by_status(conn: &Connection, format: &str, status: AssetStatus) -> DuckResult<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM tick_assets WHERE format = ? AND status = ?",
        params![format, status.as_str()],
        |row| row.get(0),
    )
}

/// Assets whose recorded file is missing or no longer matches the recorded size.
///
/// This is the query the pipeline could not previously answer: which collections claim to
/// have tick data that is not actually on disk.
pub fn find_missing_or_changed(conn: &Connection) -> DuckResult<Vec<(String, i32, String)>> {
    let mut stmt = conn.prepare(
        "SELECT demo_name, collection_num, format, path, size_bytes
         FROM tick_assets WHERE status = 'complete'",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)? as i32,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, i64>(4)?,
        ))
    })?;

    let mut stale = Vec::new();
    for row in rows {
        let (demo, num, format, path, size) = row?;
        let on_disk = std::fs::metadata(&path).ok().map(|m| m.len() as i64);
        if on_disk != Some(size) {
            stale.push((demo, num, format));
        }
    }
    Ok(stale)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::duckdb::schema::initialize_tables;
    use interface::models::tick_asset::AssetFormat;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        initialize_tables(&conn).unwrap();
        conn
    }

    fn asset(format: AssetFormat, num: i32, path: &str, size: i64) -> TickAsset {
        TickAsset {
            demo_name: "demo1".to_string(),
            collection_num: num,
            collection_type: "ACE".to_string(),
            folder: "test".to_string(),
            format,
            format_version: 5,
            path: path.to_string(),
            size_bytes: size,
            checksum: "0123456789abcdef".to_string(),
            status: AssetStatus::Complete,
            grenade_traj: 1,
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
        }
    }

    #[test]
    fn both_formats_for_one_collection_coexist() {
        let conn = db();
        record_tick_assets(
            &conn,
            &[
                asset(AssetFormat::Npz, 1, "a.npz", 100),
                asset(AssetFormat::S2r, 1, "a.s2r", 50),
            ],
        )
        .unwrap();

        let total: i64 = conn
            .query_row("SELECT COUNT(*) FROM tick_assets", [], |r| r.get(0))
            .unwrap();
        assert_eq!(total, 2, "NPZ and S2R are separate assets");
        assert_eq!(
            count_by_status(&conn, "NPZ", AssetStatus::Complete).unwrap(),
            1
        );
        assert_eq!(
            count_by_status(&conn, "S2R", AssetStatus::Complete).unwrap(),
            1
        );
    }

    fn batch(count: i32) -> Vec<TickAsset> {
        (0..count)
            .map(|i| {
                let mut a = asset(AssetFormat::S2r, i, &format!("replay-{i}.s2r"), 123456);
                a.demo_name = format!("match-{i}.dem.zst");
                a.format_version = 16;
                a.authority_bytes = 300;
                a.agent_life_count = 10;
                a.weapon_lifetime_count = 50;
                a.inventory_delta_count = 23;
                a.world_weapon_delta_count = 32;
                a.checkpoint_tick = Some(10);
                a.logical_start_tick = Some(20);
                a.logical_end_tick = Some(1000);
                a.source_path = Some("D:/demos/match.dem.gz".into());
                a.source_bytes = Some(999999);
                a
            })
            .collect()
    }

    #[test]
    fn bulk_assets_match_row_path_and_leave_unchanged_timestamps() {
        let conn = db();
        let assets = batch(300);
        let refs = assets.iter().collect::<Vec<_>>();
        conn.execute("BEGIN", []).unwrap();
        row_upsert(&conn, &refs).unwrap();
        conn.execute(
            "CREATE TEMP TABLE expected_assets AS SELECT * FROM tick_assets",
            [],
        )
        .unwrap();
        conn.execute("DELETE FROM tick_assets", []).unwrap();
        bulk_upsert(&conn, &refs).unwrap();
        let diff: i64 = conn.query_row("SELECT count(*) FROM ((SELECT * FROM expected_assets EXCEPT ALL SELECT * FROM tick_assets) UNION ALL (SELECT * FROM tick_assets EXCEPT ALL SELECT * FROM expected_assets))", [], |r| r.get(0)).unwrap();
        assert_eq!(diff, 0); // Includes timestamp (one transaction) and all nullable payloads.
        conn.execute("COMMIT", []).unwrap();
        conn.execute(
            "UPDATE tick_assets SET written_at=TIMESTAMP '2000-01-01'",
            [],
        )
        .unwrap();
        record_tick_assets(&conn, &assets).unwrap();
        let unchanged: i64 = conn
            .query_row(
                "SELECT count(*) FROM tick_assets WHERE written_at=TIMESTAMP '2000-01-01'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(unchanged, 300);
        assert_eq!(
            load_payloads(&conn, &assets).unwrap(),
            legacy_load_payloads(&conn, &assets).unwrap()
        );
        let mut changed = assets.clone();
        for a in &mut changed {
            a.authority_bytes += 1;
            a.source_path = None;
        }
        record_tick_assets(&conn, &changed).unwrap();
        let updated: i64 = conn.query_row("SELECT count(*) FROM tick_assets WHERE authority_bytes=301 AND source_path IS NULL AND written_at<>TIMESTAMP '2000-01-01'", [], |r| r.get(0)).unwrap();
        assert_eq!(updated, 300);
    }

    #[test]
    fn duplicate_normalized_asset_keys_keep_sequential_semantics() {
        let conn = db();
        let mut assets = batch(8);
        let mut last = assets[0].clone();
        last.demo_name = "match-0.dem.gz".into();
        last.path = "last.s2r".into();
        assets.push(last);
        record_tick_assets(&conn, &assets).unwrap();
        let path: String = conn
            .query_row(
                "SELECT path FROM tick_assets WHERE collection_num=0",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(path, "last.s2r");
        assert_eq!(
            conn.query_row("SELECT count(*) FROM tick_assets", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            8
        );
    }

    #[test]
    fn bulk_assets_roll_back_when_a_row_violates_constraints() {
        let conn = db();
        // Trigger a late merge failure after native staging has accepted every row.
        conn.execute(
            "ALTER TABLE tick_assets ALTER COLUMN source_path SET NOT NULL",
            [],
        )
        .unwrap();
        let mut assets = batch(8);
        assets[7].source_path = None;
        assert!(record_tick_assets(&conn, &assets).is_err());
        assert_eq!(
            conn.query_row("SELECT count(*) FROM tick_assets", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM duckdb_tables() WHERE table_name='repair_tick_assets'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assets[7].source_path = Some("D:/recovered.dem".into());
        record_tick_assets(&conn, &assets).unwrap();
        assert_eq!(
            conn.query_row("SELECT count(*) FROM tick_assets", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            8
        );
    }

    #[test]
    #[ignore = "bounded native asset benchmark; run alone with --nocapture"]
    fn native_asset_ingestion_benchmark() {
        let conn = db();
        let all = batch(1000);
        let mut results = Vec::new();
        for count in [1, 8, 32, 200, 1000] {
            let assets = &all[..count];
            let refs = assets.iter().collect::<Vec<_>>();
            for bulk in [false, true] {
                let mut times = Vec::new();
                for _ in 0..5 {
                    conn.execute("DELETE FROM tick_assets", []).unwrap();
                    let start = std::time::Instant::now();
                    conn.execute("BEGIN", []).unwrap();
                    if bulk {
                        bulk_upsert(&conn, &refs).unwrap();
                    } else {
                        row_upsert(&conn, &refs).unwrap();
                    }
                    conn.execute("COMMIT", []).unwrap();
                    times.push(start.elapsed().as_secs_f64() * 1000.0);
                }
                times.sort_by(f64::total_cmp);
                let result = serde_json::json!({"rows":count,"bulk":bulk,"median_ms":times[2],"trials_ms":times});
                println!("DUCKDB_ASSET_BENCH {result}");
                results.push(result);
            }
        }
        for count in [1, 8, 32, 200, 1000] {
            for bulk in [false, true] {
                let mut times = Vec::new();
                for _ in 0..5 {
                    let start = std::time::Instant::now();
                    let stored = if bulk {
                        load_payloads(&conn, &all[..count])
                    } else {
                        legacy_load_payloads(&conn, &all[..count])
                    }
                    .unwrap();
                    assert_eq!(stored.len(), count);
                    times.push(start.elapsed().as_secs_f64() * 1000.0);
                }
                times.sort_by(f64::total_cmp);
                let result = serde_json::json!({"lookup_rows":count,"bulk":bulk,"median_ms":times[2],"trials_ms":times});
                println!("DUCKDB_LOOKUP_BENCH {result}");
                results.push(result);
            }
        }
        if let Some(root) = std::env::var_os("S2DVR_DUCKDB_BENCH_DIR") {
            std::fs::write(
                std::path::PathBuf::from(root).join("asset-results.json"),
                serde_json::to_vec_pretty(&results).unwrap(),
            )
            .unwrap();
        }
    }

    #[test]
    fn collection_provenance_and_assets_rollback_together() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("ACE_test.duckdb");
        let conn = Connection::open(&path).unwrap();
        crate::duckdb::schema::initialize_tables(&conn).unwrap();
        // Force a failure in the asset step, after collection/provenance inserts.
        conn.execute("ALTER TABLE tick_assets DROP COLUMN checksum", [])
            .unwrap();
        drop(conn);
        let writer = crate::duckdb::DuckDBWriter::new(path.to_str().unwrap(), "ACE", "test");
        let collection = interface::models::collection::KillCollection {
            demo_name: "demo1".into(),
            collection_num: 1,
            col_total: 1,
            collection_type: "ACE".into(),
            folder: "test".into(),
            ..Default::default()
        };
        let source = interface::models::demo_source::DemoSource {
            demo_name: "demo1".into(),
            source_path: "demo1.dem".into(),
            size_bytes: 100,
            modified_ns: 1,
        };
        assert!(writer
            .append_collections_with_assets(
                &[collection],
                &[source],
                &[asset(AssetFormat::S2r, 1, "a.s2r", 50)],
                true
            )
            .is_err());
        let conn = Connection::open(path).unwrap();
        for table in ["kill_collections", "demo_sources"] {
            let count: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(
                count, 0,
                "{table} must roll back when asset registration fails"
            );
        }
    }

    #[test]
    fn rewriting_a_collection_replaces_rather_than_duplicates() {
        let conn = db();
        let mut first = asset(AssetFormat::S2r, 1, "a.s2r", 50);
        first.demo_name = "demo1.dem.gz".into();
        record_tick_assets(&conn, &[first]).unwrap();

        let mut updated = asset(AssetFormat::S2r, 1, "a.s2r", 999);
        updated.demo_name = "demo1.dem.zst".into();
        updated.checksum = "ffffffffffffffff".to_string();
        record_tick_assets(&conn, &[updated]).unwrap();

        let (count, size): (i64, i64) = conn
            .query_row(
                "SELECT COUNT(*), MAX(size_bytes) FROM tick_assets",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(size, 999, "the newer write must win");
    }

    /// Two players can each produce a collection in the same round; keying on
    /// collection_num keeps them distinct.
    #[test]
    fn collections_from_the_same_round_do_not_collide() {
        let conn = db();
        record_tick_assets(
            &conn,
            &[
                asset(AssetFormat::S2r, 1, "a.s2r", 10),
                asset(AssetFormat::S2r, 2, "b.s2r", 20),
            ],
        )
        .unwrap();

        let total: i64 = conn
            .query_row("SELECT COUNT(*) FROM tick_assets", [], |r| r.get(0))
            .unwrap();
        assert_eq!(total, 2);
    }

    #[test]
    fn a_failed_asset_is_not_counted_complete() {
        let conn = db();
        let mut failed = asset(AssetFormat::Npz, 1, "a.npz", 0);
        failed.status = AssetStatus::Failed;
        record_tick_assets(&conn, &[failed]).unwrap();

        assert_eq!(
            count_by_status(&conn, "NPZ", AssetStatus::Complete).unwrap(),
            0
        );
        assert_eq!(
            count_by_status(&conn, "NPZ", AssetStatus::Failed).unwrap(),
            1
        );
    }

    /// `written_at` is stamped from a sentinel so the assertion does not depend on clock
    /// resolution between two writes in the same test.
    fn stamp(conn: &Connection, sentinel: &str) {
        conn.execute(
            "UPDATE tick_assets SET written_at = ?::TIMESTAMP",
            params![sentinel],
        )
        .unwrap();
    }

    fn written_at(conn: &Connection) -> String {
        conn.query_row(
            "SELECT COALESCE(CAST(written_at AS TEXT), '') FROM tick_assets",
            [],
            |row| row.get(0),
        )
        .unwrap()
    }

    #[test]
    fn an_unchanged_asset_is_not_rewritten() {
        let conn = db();
        record_tick_assets(&conn, &[asset(AssetFormat::S2r, 1, "a.s2r", 50)]).unwrap();
        stamp(&conn, "2001-01-01 00:00:00");

        record_tick_assets(&conn, &[asset(AssetFormat::S2r, 1, "a.s2r", 50)]).unwrap();

        assert!(
            written_at(&conn).starts_with("2001-01-01"),
            "an identical asset must leave written_at alone, or the column cannot say which \
             assets an import actually touched"
        );
    }

    #[test]
    fn a_changed_count_alone_still_rewrites() {
        let conn = db();
        record_tick_assets(&conn, &[asset(AssetFormat::S2r, 1, "a.s2r", 50)]).unwrap();
        stamp(&conn, "2001-01-01 00:00:00");

        // The file is byte-identical; only a recorded count moved. Suppressing this would
        // leave the row describing the asset incorrectly.
        let mut recounted = asset(AssetFormat::S2r, 1, "a.s2r", 50);
        recounted.agent_life_count = 7;
        record_tick_assets(&conn, &[recounted]).unwrap();

        assert!(!written_at(&conn).starts_with("2001-01-01"));
        let stored: i64 = conn
            .query_row("SELECT agent_life_count FROM tick_assets", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stored, 7);
    }

    #[test]
    fn changed_dem_timeline_metadata_rewrites_the_full_payload() {
        let conn = db();
        let mut original = asset(AssetFormat::Dem, 1, "match_r4.dem", 50);
        original.checkpoint_tick = Some(100);
        original.logical_start_tick = Some(120);
        original.logical_end_tick = Some(180);
        original.source_path = Some("match.dem.gz".into());
        original.source_bytes = Some(500);
        record_tick_assets(&conn, &[original.clone()]).unwrap();
        stamp(&conn, "2001-01-01 00:00:00");

        original.logical_end_tick = Some(181);
        record_tick_assets(&conn, &[original]).unwrap();

        assert!(!written_at(&conn).starts_with("2001-01-01"));
        let stored: i64 = conn
            .query_row("SELECT logical_end_tick FROM tick_assets", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(stored, 181);
    }

    #[test]
    fn repair_bulk_merge_handles_multiple_chunks_and_conflicts() {
        let conn = db();
        let mut existing = asset(AssetFormat::Dem, 7, "old.dem", 10);
        existing.demo_name = "match7".into();
        record_tick_assets(&conn, &[existing]).unwrap();

        let mut repairs = (0..450)
            .map(|number| {
                let mut value = asset(
                    AssetFormat::Dem,
                    number,
                    &format!("match{number}_r4.dem"),
                    100 + i64::from(number),
                );
                value.demo_name = format!("match{number}");
                value.checkpoint_tick = Some(100);
                value.logical_start_tick = Some(120);
                value.logical_end_tick = Some(180);
                value.source_path = Some(format!("match{number}.dem.gz"));
                value.source_bytes = Some(500);
                value
            })
            .collect::<Vec<_>>();
        repairs[7].path = "replacement.dem".into();
        repairs[7].size_bytes = 999;

        conn.execute("BEGIN TRANSACTION", []).unwrap();
        repair_tick_assets_in_transaction(&conn, &repairs).unwrap();
        conn.execute("COMMIT", []).unwrap();

        let (count, replacement_size, described): (i64, i64, i64) = conn
            .query_row(
                "SELECT COUNT(*),
                        MAX(CASE WHEN demo_name = 'match7' THEN size_bytes END),
                        COUNT(*) FILTER (WHERE checkpoint_tick = 100 AND source_bytes = 500)
                 FROM tick_assets",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(count, 450);
        assert_eq!(replacement_size, 999);
        assert_eq!(described, 450);
    }

    /// Two demos in one batch must not have their rows confused for one another, which is the
    /// failure mode of loading stored payloads keyed on anything less than the full key.
    #[test]
    fn assets_are_matched_per_demo() {
        let conn = db();
        let mut other = asset(AssetFormat::S2r, 1, "b.s2r", 50);
        other.demo_name = "demo2".to_string();
        record_tick_assets(&conn, &[asset(AssetFormat::S2r, 1, "a.s2r", 50), other]).unwrap();

        let paths: i64 = conn
            .query_row(
                "SELECT COUNT(DISTINCT path) FROM tick_assets WHERE collection_num = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(paths, 2, "same collection_num in two demos are two assets");
    }

    #[test]
    fn missing_and_resized_files_are_detected() {
        let dir = tempfile::tempdir().unwrap();
        let present = dir.path().join("present.s2r");
        std::fs::write(&present, vec![0u8; 64]).unwrap();
        let resized = dir.path().join("resized.s2r");
        std::fs::write(&resized, vec![0u8; 8]).unwrap();

        let conn = db();
        record_tick_assets(
            &conn,
            &[
                asset(AssetFormat::S2r, 1, present.to_str().unwrap(), 64),
                asset(AssetFormat::S2r, 2, resized.to_str().unwrap(), 64),
                asset(
                    AssetFormat::S2r,
                    3,
                    dir.path().join("gone.s2r").to_str().unwrap(),
                    64,
                ),
            ],
        )
        .unwrap();

        let stale = find_missing_or_changed(&conn).unwrap();
        let nums: Vec<i32> = stale.iter().map(|(_, n, _)| *n).collect();
        assert!(!nums.contains(&1), "an intact file is not stale");
        assert!(nums.contains(&2), "a truncated file must be flagged");
        assert!(nums.contains(&3), "a missing file must be flagged");
    }
}
