use super::*;
use interface::models::kill::Kill;
use interface::models::tick_asset::{AssetFormat, AssetStatus, TickAsset};
use std::time::Instant;

#[test]
#[ignore = "private native growth fixture; coordinated lifecycle audit only"]
fn native_catalog_growth_fixture() {
    let path = std::path::PathBuf::from(
        std::env::var_os("S2DVR_DUCKDB_GROW_FILE").expect("explicit private catalog required"),
    ).canonicalize().unwrap();
    assert!(path.components().any(|part| part.as_os_str() == ".artifacts"));
    assert_eq!(path.extension().and_then(|x| x.to_str()), Some("duckdb"));
    assert!(!std::path::PathBuf::from(format!("{}.wal", path.display())).exists());
    let cycle: usize = std::env::var("S2DVR_DUCKDB_GROW_CYCLE").unwrap().parse().unwrap();
    assert!(cycle < 2);
    let tables = ["kill_collections", "demo_sources", "tick_assets"];
    let counts = |connection: &Connection| -> Vec<i64> {
        tables.iter().map(|table| connection.query_row(
            &format!("SELECT count(*) FROM {table}"), [], |row| row.get(0),
        ).unwrap()).collect()
    };
    let (before, engine) = {
        let connection = Connection::open(&path).unwrap();
        (counts(&connection), connection.query_row("SELECT version()", [], |row| row.get::<_, String>(0)).unwrap())
    };
    let mut rows = collections(200);
    for (i, row) in rows.iter_mut().enumerate() {
        row.demo_name = format!("native-growth-{cycle}-{i:05}");
        row.demo_path = format!("D:/synthetic-not-read/{}.dem", row.demo_name);
    }
    let sources: Vec<_> = rows.iter().map(|row| DemoSource {
        demo_name: row.demo_name.clone(), source_path: row.demo_path.clone(),
        size_bytes: 9000000, modified_ns: 42,
    }).collect();
    let assets: Vec<_> = rows.iter().map(|row| TickAsset {
        demo_name: row.demo_name.clone(), collection_num: row.collection_num,
        collection_type: "QUAD".into(), folder: "bench".into(),
        format: AssetFormat::S2r, format_version: 16,
        path: format!("{}.s2r", row.demo_name), size_bytes: 123456,
        checksum: "fedcba9876543210".into(), status: AssetStatus::Complete,
        grenade_traj: 2, authority_bytes: 4096, agent_life_count: 10,
        weapon_lifetime_count: 100, inventory_delta_count: 200, world_weapon_delta_count: 30,
        checkpoint_tick: Some(100), logical_start_tick: Some(200), logical_end_tick: Some(1000),
        source_path: Some(row.demo_path.clone()), source_bytes: Some(9000000),
    }).collect();
    let writer = DuckDBWriter::new(path.to_str().unwrap(), "QUAD", "bench");
    let watch = Instant::now();
    for batch in 0..20 {
        let range = batch * 10..(batch + 1) * 10;
        writer.append_collections_with_assets(&rows[range.clone()], &sources[range.clone()], &assets[range], true).unwrap();
    }
    writer.recalculate_metadata().unwrap();
    let elapsed_ms = watch.elapsed().as_secs_f64() * 1000.0;
    let connection = Connection::open(&path).unwrap();
    assert_eq!(counts(&connection), before.iter().map(|n| n + 200).collect::<Vec<_>>());
    let groups: i64 = connection.query_row("SELECT count(DISTINCT row_group_id) FROM pragma_storage_info('kill_collections')", [], |row| row.get(0)).unwrap();
    println!("NATIVE_GROWTH {}", serde_json::json!({"cycle":cycle,"engine":engine,"batches":20,"added_rows_per_table":200,"collection_row_groups":groups,"elapsed_ms":elapsed_ms}));
}

fn collections(count: usize) -> Vec<KillCollection> {
    (0..count)
        .map(|i| KillCollection {
            killer_steamid: format!("765611980000{:05}", i % 17),
            demo_name: format!("match-{i:05}"),
            demo_path: format!("D:/demos/match-{i:05}.dem"),
            collection_type: "QUAD".into(),
            folder: "bench".into(),
            collection_num: i as i32,
            col_total: 4,
            tick_duration: 4321,
            map_name: "de_mirage".into(),
            killer_index: 7,
            killer_team: "CT".into(),
            killer_name: "Náme '🦆'".into(),
            killer_radius: 13.25,
            killer_move_distance: 71.5,
            victims_radius: 52.75,
            start_kill_tick: 500,
            end_kill_tick: 820,
            victim_team: "T".into(),
            round: i as i32 % 24 + 1,
            round_start_tick: 100,
            round_end_tick: 1000,
            round_freeze_end: 200,
            weapons: vec!["ak47".into(), "awp".into()],
            weapons_id: vec!["7".into(), "9".into()],
            kill_ticks: vec![500, 820],
            victim_indices: vec![2, 3],
            game_version: 14000,
            tag: "keep me".into(),
            util_thrown: "[flashbang(2);molotov(1)]".into(),
            parsed: 1,
            grenade_traj: 2,
            hits: 7,
            misses: 3,
            hit_rate: 0.7,
            kills: vec![Kill {
                tick: 500,
                killer_name: "Náme".into(),
                killer_steamid: "player".into(),
                killer_team: "CT".into(),
                killer_pos_x: 123.25,
                killer_pos_y: -57.5,
                killer_pos_z: 17.75,
                killer_view_pitch: -13.5,
                killer_view_yaw: 179.25,
                victim_name: "victim '雪'".into(),
                victim_steamid: "victim".into(),
                victim_team: "T".into(),
                victim_pos_x: -12.5,
                victim_pos_y: 89.25,
                victim_pos_z: 3.5,
                weapon: "ak47".into(),
                weapon_id: "7".into(),
                distance_to_enemy: 133.25,
                headshot: true,
                penetrated: false,
                attacker_blind: false,
                thru_smoke: false,
                no_scope: false,
                attacker_airborne: None,
                victim_airborne: Some(false),
                penetration_count: Some(0),
                modifier_known_flags: 0,
                round: 1,
                round_start_tick: 100,
                round_end_tick: 1000,
                round_freeze_end: 200,
                ticks_since_last_kill: 123,
                distance_moved_since_last_kill: 55.25,
                killer_index: 7,
                victim_index: 2,
                killer_is_controlling_bot: false,
            }],
            ..Default::default()
        })
        .collect()
}

#[test]
fn appender_matches_values_for_all_columns_and_reparse_rules() {
    let conn = Connection::open_in_memory().unwrap();
    schema::initialize_tables(&conn).unwrap();
    let writer = DuckDBWriter::new("unused", "QUAD", "bench");
    let original = collections(503); // Crosses the old 500-row boundary.
    for appender in [false, true] {
        conn.execute("DELETE FROM kill_collections", []).unwrap();
        let fill = if appender {
            DuckDBWriter::append_collection_rows
        } else {
            DuckDBWriter::values_collection_rows
        };
        writer
            .staging_insert_with(&conn, &original, "D:/demos/", "fixed", fill)
            .unwrap();
        let mut reparse = original.clone();
        for row in &mut reparse {
            row.tag.clear();
            row.parsed = 0;
            row.grenade_traj = 0;
            row.hits = 0;
            row.util_thrown.clear();
            row.kills.clear();
            row.start_kill_tick += 1;
        }
        writer
            .staging_insert_with(&conn, &reparse, "D:/demos/", "updated", fill)
            .unwrap();
        if !appender {
            conn.execute(
                "CREATE TEMP TABLE expected AS SELECT * FROM kill_collections",
                [],
            )
            .unwrap();
        }
    }
    let difference: i64 = conn.query_row(
        "SELECT count(*) FROM ((SELECT * FROM expected EXCEPT ALL SELECT * FROM kill_collections)
         UNION ALL (SELECT * FROM kill_collections EXCEPT ALL SELECT * FROM expected))",
        [], |r| r.get(0)).unwrap();
    assert_eq!(difference, 0);
    let kept: i64 = conn.query_row("SELECT count(*) FROM kill_collections WHERE tag='keep me' AND TickData=1 AND GrenadeTraj=2 AND hits=7 AND start_kill_tick=501", [], |r| r.get(0)).unwrap();
    assert_eq!(kept, 503);
    metadata::update_manifest_info(&conn, "QUAD", "bench").unwrap();
    let totals: (i64, i64, i64) = conn.query_row("SELECT total_collections,total_demos,unique_steam_ids FROM manifest_info WHERE type='QUAD' AND folder='bench'", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
    assert_eq!(totals, (503, 503, 17));
}

#[test]
fn appender_staging_rolls_back_with_owning_transaction() {
    let conn = Connection::open_in_memory().unwrap();
    schema::initialize_tables(&conn).unwrap();
    let writer = DuckDBWriter::new("unused", "QUAD", "bench");
    conn.execute("BEGIN", []).unwrap();
    writer
        .staging_table_insert_collections(&conn, &collections(8), "D:/demos/", "fixed")
        .unwrap();
    conn.execute("ROLLBACK", []).unwrap();
    let count: i64 = conn
        .query_row("SELECT count(*) FROM kill_collections", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
#[ignore = "bounded native writer benchmark; run alone with --nocapture"]
fn native_collection_ingestion_benchmark() {
    let temporary = tempfile::tempdir().unwrap();
    let output = std::env::var_os("S2DVR_DUCKDB_BENCH_DIR").map(std::path::PathBuf::from);
    if let Some(path) = &output {
        std::fs::create_dir(path).unwrap();
    }
    let root = output.as_deref().unwrap_or(temporary.path());
    let engine: String = Connection::open_in_memory()
        .unwrap()
        .query_row("SELECT version()", [], |r| r.get(0))
        .unwrap();
    let mut results = Vec::new();
    for count in [1, 8, 32, 200, 1000] {
        let rows = collections(count);
        for appender in [false, true] {
            let mut timings = Vec::new();
            for trial in 0..5 {
                let path = root.join(format!("collections-{count}-{appender}-{trial}.duckdb"));
                let conn = Connection::open(&path).unwrap();
                schema::initialize_tables(&conn).unwrap();
                let writer = DuckDBWriter::new(path.to_str().unwrap(), "QUAD", "bench");
                let fill = if appender {
                    DuckDBWriter::append_collection_rows
                } else {
                    DuckDBWriter::values_collection_rows
                };
                let start = Instant::now();
                conn.execute("BEGIN", []).unwrap();
                writer
                    .staging_insert_with(&conn, &rows, "D:/demos/", "fixed", fill)
                    .unwrap();
                conn.execute("COMMIT", []).unwrap();
                timings.push(start.elapsed().as_secs_f64() * 1000.0);
                let actual: i64 = conn
                    .query_row("SELECT count(*) FROM kill_collections", [], |r| r.get(0))
                    .unwrap();
                assert_eq!(actual, count as i64);
                conn.execute("CHECKPOINT", []).unwrap();
            }
            timings.sort_by(f64::total_cmp);
            let result = serde_json::json!({"engine":engine,"rows":count,"appender":appender,"median_ms":timings[2],"trials_ms":timings});
            println!("DUCKDB_BENCH {result}");
            results.push(result);
        }
    }
    std::fs::write(
        root.join("native-results.json"),
        serde_json::to_vec_pretty(&results).unwrap(),
    )
    .unwrap();
}

#[test]
#[ignore = "bounded full catalog writer benchmark; run alone with --nocapture"]
fn native_catalog_pipeline_benchmark() {
    let root = std::path::PathBuf::from(
        std::env::var_os("S2DVR_DUCKDB_BENCH_DIR").expect("private benchmark directory required"),
    );
    let rows = collections(555);
    let sources = rows
        .iter()
        .map(|row| DemoSource {
            demo_name: row.demo_name.clone(),
            source_path: row.demo_path.clone(),
            size_bytes: 9000000,
            modified_ns: 42,
        })
        .collect::<Vec<_>>();
    let assets = rows
        .iter()
        .map(|row| TickAsset {
            demo_name: row.demo_name.clone(),
            collection_num: row.collection_num,
            collection_type: "QUAD".into(),
            folder: "bench".into(),
            format: AssetFormat::S2r,
            format_version: 16,
            path: format!("{}.s2r", row.demo_name),
            size_bytes: 123456,
            checksum: "fedcba9876543210".into(),
            status: AssetStatus::Complete,
            grenade_traj: 2,
            authority_bytes: 4096,
            agent_life_count: 10,
            weapon_lifetime_count: 100,
            inventory_delta_count: 200,
            world_weapon_delta_count: 30,
            checkpoint_tick: Some(100),
            logical_start_tick: Some(200),
            logical_end_tick: Some(1000),
            source_path: Some(row.demo_path.clone()),
            source_bytes: Some(9000000),
        })
        .collect::<Vec<_>>();
    let mut results = Vec::new();
    for optimized in [false, true] {
        for trial in 0..3 {
            let path = root.join(format!("pipeline-{optimized}-{trial}.duckdb"));
            assert!(!path.exists());
            let writer = DuckDBWriter::new(path.to_str().unwrap(), "QUAD", "bench");
            let fill = if optimized {
                DuckDBWriter::append_collection_rows
            } else {
                DuckDBWriter::values_collection_rows
            };
            let record = if optimized {
                tick_assets::record_tick_assets_in_transaction
            } else {
                tick_assets::legacy_record_in_transaction
            };
            let watch = Instant::now();
            for (i, chunk) in rows.chunks(10).enumerate() {
                let start = i * 10;
                let end = start + chunk.len();
                writer
                    .append_with_ingestion(
                        chunk,
                        &sources[start..end],
                        &assets[start..end],
                        true,
                        fill,
                        record,
                    )
                    .unwrap();
            }
            writer.recalculate_metadata().unwrap();
            let elapsed = watch.elapsed().as_secs_f64() * 1000.0;
            let conn = Connection::open(&path).unwrap();
            for table in ["kill_collections", "tick_assets", "demo_sources"] {
                assert_eq!(
                    conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                        .get::<_, i64>(0))
                        .unwrap(),
                    555
                );
            }
            let groups:i64=conn.query_row("SELECT count(DISTINCT row_group_id) FROM pragma_storage_info('kill_collections')", [], |r| r.get(0)).unwrap();
            let result = serde_json::json!({"optimized":optimized,"trial":trial,"rows":555,"batch_rows":10,"elapsed_ms":elapsed,"bytes":std::fs::metadata(&path).unwrap().len(),"collection_row_groups":groups});
            println!("DUCKDB_PIPELINE_BENCH {result}");
            results.push(result);
        }
    }
    std::fs::write(
        root.join("pipeline-results.json"),
        serde_json::to_vec_pretty(&results).unwrap(),
    )
    .unwrap();
}
