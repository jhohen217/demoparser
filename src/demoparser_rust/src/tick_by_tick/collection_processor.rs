use super::cli::CliArgs;
use super::config_manager::AppConfig;
use super::data_processing::{ProcessingConfig, TickDataProcessor};
use super::demo_loader::{load_and_validate_demo, resolve_demo_path};
use super::kill_collection_parser::{Collection, KillCollectionData};
use super::parser_config::{
    create_event_authority_parser_settings, create_multi_player_parser_settings,
    initialize_huffman_table,
};
use dashmap::DashMap;
use std::sync::{Arc, Mutex};
// use super::weapon_fire_detector; // DISABLED - functionality moved to backup
use super::data_types::TickRecord;
use crate::collection_buffer::CollectionBuffer;
use anyhow::{anyhow, Result};
use interface::models::tick_asset::{AssetFormat, AssetStatus, TickAsset};

fn round_replay_window(start: u32, freeze_end: u32, score_tick: u32, next_start: Option<u32>, skip_buy_time: bool) -> (u32, u32) {
    let first = if skip_buy_time { freeze_end.max(start) } else { start };
    let last = next_start.map(|tick| tick.saturating_sub(1))
        .unwrap_or_else(|| score_tick.saturating_add(demo_writer::DEFAULT_TAIL_TICKS as u32));
    (first, last)
}

#[derive(Default)]
struct S2rAuthoritySummary {
    bytes: i64,
    agent_lives: i32,
    weapon_lifetimes: i32,
    inventory_deltas: i32,
    world_weapon_deltas: i32,
}

/// Read only the fixed header and S2EX directory. Detailed authority remains in the S2R file;
/// DuckDB gets compact health/discovery counters rather than a second copy of replay state.
fn read_s2r_authority_summary(path: &std::path::Path) -> std::io::Result<S2rAuthoritySummary> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = std::fs::File::open(path)?;
    let file_length = file.metadata()?.len();
    let mut header = [0u8; 64];
    if file.read_exact(&mut header).is_err() {
        return Ok(S2rAuthoritySummary::default());
    }
    let extension_offset = u32::from_le_bytes(header[54..58].try_into().unwrap()) as u64;
    let extension_length = u32::from_le_bytes(header[58..62].try_into().unwrap()) as u64;
    if extension_offset == 0
        || extension_length < 8
        || extension_offset
            .checked_add(extension_length)
            .is_none_or(|end| end > file_length)
    {
        return Ok(S2rAuthoritySummary::default());
    }

    file.seek(SeekFrom::Start(extension_offset))?;
    let mut extension_header = [0u8; 8];
    file.read_exact(&mut extension_header)?;
    if &extension_header[0..4] != b"S2EX" {
        return Ok(S2rAuthoritySummary::default());
    }
    let section_count = u16::from_le_bytes(extension_header[6..8].try_into().unwrap()) as usize;
    let directory_length = section_count.checked_mul(16).unwrap_or(usize::MAX);
    if 8usize
        .checked_add(directory_length)
        .is_none_or(|length| length as u64 > extension_length)
    {
        return Ok(S2rAuthoritySummary::default());
    }
    let mut directory = vec![0u8; directory_length];
    file.read_exact(&mut directory)?;

    let mut summary = S2rAuthoritySummary {
        bytes: i64::try_from(extension_length).unwrap_or(i64::MAX),
        ..Default::default()
    };
    for index in 0..section_count {
        let entry = index * 16;
        let tag = u16::from_le_bytes(directory[entry..entry + 2].try_into().unwrap());
        let count = i32::try_from(u32::from_le_bytes(
            directory[entry + 12..entry + 16].try_into().unwrap(),
        ))
        .unwrap_or(i32::MAX);
        match tag {
            1 => summary.agent_lives = count,
            2 => summary.weapon_lifetimes = count,
            3 => summary.inventory_deltas = count,
            4 => summary.world_weapon_deltas = count,
            _ => {}
        }
    }
    Ok(summary)
}
use parser::parse_demo::{DemoOutput, Parser};
use parser::second_pass::audio::AudioEvent;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

/// Represents a pending DuckDB update
#[derive(Debug, Clone)]
struct DuckDBUpdate {
    demo_name: String,
    collection_num: u32,
    util_thrown: String,
    traj_mode: u8,
    hits: u32,
    misses: u32,
    hit_rate: f32,
    weapons_formatted: String,
}

struct PendingCollection<'a> {
    collection: &'a Collection,
    s2r_path: PathBuf,
    range: (u32, u32),
}

/// The grenade/event lane is parsed once for a collection request; each S2R
/// output retains history through its inclusive end. Starts/parameters before
/// the visible window can be necessary to reconstruct an active decoy or loop.
fn audio_events_for_replay_range(
    events: &[AudioEvent],
    _tick_start: u32,
    tick_end: u32,
) -> Vec<AudioEvent> {
    let tick_end = tick_end as i32;
    events
        .iter()
        .filter(|event| event.tick <= tick_end)
        .cloned()
        .collect()
}

fn weapon_snapshots_for_window(
    source_snapshots: &[parser::second_pass::parser_settings::WeaponEntitySnapshot],
    checkpoint_snapshots: &[parser::second_pass::parser_settings::WeaponEntitySnapshot],
    tick_start: i32,
    tick_end: i32,
) -> Vec<parser::second_pass::parser_settings::WeaponEntitySnapshot> {
    use parser::second_pass::parser_settings::WeaponEntitySnapshot;

    let baseline_tick = checkpoint_snapshots
        .iter()
        .filter(|row| row.tick >= tick_start && row.tick <= tick_end && row.present)
        .map(|row| row.tick)
        .min();

    // Multiple packet updates can precede the checkpoint's tick callback. Collapse those
    // observations to the final state for each Source entity lifetime, which is the state a
    // replay beginning at tick_start must see.
    let mut baseline_by_entity: BTreeMap<(i32, u32), WeaponEntitySnapshot> = BTreeMap::new();
    if let Some(baseline_tick) = baseline_tick {
        for row in checkpoint_snapshots
            .iter()
            .filter(|row| row.tick == baseline_tick && row.present)
        {
            baseline_by_entity.insert((row.entity_id, row.entity_serial), row.clone());
        }
    }

    let mut selected = Vec::with_capacity(
        baseline_by_entity.len()
            + source_snapshots
                .iter()
                .filter(|row| row.tick >= tick_start && row.tick <= tick_end)
                .count(),
    );
    for (index, mut row) in baseline_by_entity.into_values().enumerate() {
        row.tick = tick_start;
        // Checkpoint state is the window's seed and must sort before same-tick source updates.
        row.source_order = index as u64;
        selected.push(row);
    }
    selected.extend(
        source_snapshots
            .iter()
            .filter(|row| row.tick >= tick_start && row.tick <= tick_end)
            .cloned(),
    );
    selected
}

pub struct CollectionProcessor {
    config: AppConfig,
    cli_args: CliArgs,
    master_file_lock: Arc<DashMap<PathBuf, Arc<Mutex<()>>>>,
    progress: Option<Arc<dyn Fn(crate::ProgressEvent) + Send + Sync>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_replay_keeps_the_entire_post_win_interval_and_excludes_next_buy_tick() {
        assert_eq!(round_replay_window(100, 200, 400, Some(1200), true), (200, 1199));
        assert_eq!(round_replay_window(100, 200, 400, Some(1200), false), (100, 1199));
        assert_eq!(round_replay_window(100, 0, 400, Some(500), true), (100, 499));
        assert_eq!(round_replay_window(100, 200, 400, None, true), (200, 847));
    }
    use parser::second_pass::audio::{AudioEventOrder, AudioEventPayload};
    use parser::second_pass::parser_settings::WeaponEntitySnapshot;

    fn event(tick: i32) -> AudioEvent {
        AudioEvent {
            tick,
            order: AudioEventOrder {
                demo_frame_offset: tick as u64,
                network_message_index: 0,
            },
            payload: AudioEventPayload::SvcStopSound { guid: None },
        }
    }

    #[test]
    fn audio_filter_keeps_prewindow_history_and_inclusive_end() {
        let selected = audio_events_for_replay_range(
            &[event(99), event(100), event(101), event(102)],
            100,
            101,
        );
        assert_eq!(
            selected.iter().map(|event| event.tick).collect::<Vec<_>>(),
            [99, 100, 101]
        );
    }

    fn weapon(tick: i32, source_order: u64, entity_id: i32, serial: u32) -> WeaponEntitySnapshot {
        WeaponEntitySnapshot {
            tick,
            source_order,
            entity_id,
            entity_serial: serial,
            class_name: "weapon_ak47".to_string(),
            item_id_high: None,
            item_id_low: None,
            item_definition_index: Some(7),
            paint_kit_id: None,
            paint_seed: None,
            wear: None,
            econ_attributes: Vec::new(),
            stickers: Vec::new(),
            current_owner_handle: None,
            owner_steamid: None,
            inventory_slot: None,
            econ_inventory_position: None,
            active: None,
            clip_ammo: Some(30),
            reserve_ammo: Some(90),
            position: None,
            rotation: None,
            pvs_state: None,
            current_owner_id: None,
            previous_owner_handle: None,
            original_owner_xuid_low: None,
            original_owner_xuid_high: None,
            dropped_at_time: None,
            last_shake_time: None,
            present: true,
        }
    }

    #[test]
    fn weapon_window_adds_one_checkpoint_seed_per_entity_lifetime() {
        let source = [weapon(110, 500, 10, 1), weapon(121, 600, 11, 2)];
        let mut first = weapon(105, 90, 10, 1);
        first.clip_ammo = Some(29);
        let mut final_state = weapon(105, 100, 10, 1);
        final_state.clip_ammo = Some(28);
        let outside = weapon(99, 80, 12, 1);

        let selected =
            weapon_snapshots_for_window(&source, &[outside, first, final_state.clone()], 100, 120);

        assert_eq!(selected.len(), 2);
        assert_eq!(selected[0].tick, 100);
        assert_eq!(selected[0].entity_id, 10);
        assert_eq!(selected[0].clip_ammo, final_state.clip_ammo);
        assert_eq!(selected[0].source_order, 0);
        assert_eq!(selected[1].tick, 110);
    }

    #[test]
    fn weapon_window_keeps_source_create_update_delete_order_inclusively() {
        let mut deleted = weapon(120, 700, 10, 1);
        deleted.present = false;
        let source = [
            weapon(99, 1, 9, 1),
            weapon(100, 2, 10, 1),
            deleted,
            weapon(121, 3, 11, 1),
        ];

        let selected = weapon_snapshots_for_window(&source, &[], 100, 120);

        assert_eq!(
            selected.iter().map(|row| row.tick).collect::<Vec<_>>(),
            [100, 120]
        );
        assert!(selected.last().is_some_and(|row| !row.present));
    }
}

impl CollectionProcessor {
    pub fn new(
        config: AppConfig,
        cli_args: CliArgs,
        master_file_lock: Arc<DashMap<PathBuf, Arc<Mutex<()>>>>,
    ) -> Self {
        Self {
            config,
            cli_args,
            master_file_lock,
            progress: None,
        }
    }

    pub fn with_progress(mut self, progress: Option<Arc<dyn Fn(crate::ProgressEvent) + Send + Sync>>) -> Self {
        self.progress = progress;
        self
    }

    fn report_replay(&self, data: &KillCollectionData, phase: &'static str, completed: usize, total: usize) {
        if let Some(callback) = &self.progress {
            callback(crate::ProgressEvent::ReplayProgress {
                filename: std::path::Path::new(&data.demo_info.demo_path)
                    .file_name().unwrap_or_default().to_string_lossy().into_owned(),
                phase, completed, total,
            });
        }
    }

    /// Process all collections from kill collection data (backward compatibility)
    /// Uses the legacy DuckDB update approach
    pub fn process_collections(
        &self,
        collection_data: &KillCollectionData,
    ) -> Result<Vec<ProcessResult>> {
        self.process_collections_with_buffer(collection_data, None)
    }

    /// Process all collections from kill collection data with optional buffer
    /// If buffer is provided, updates are applied to RAM buffer instead of DuckDB
    pub fn process_collections_with_buffer(
        &self,
        collection_data: &KillCollectionData,
        buffer: Option<&CollectionBuffer>,
    ) -> Result<Vec<ProcessResult>> {
        let mut results = Vec::new();
        let mut duckdb_updates: HashMap<PathBuf, Vec<DuckDBUpdate>> = HashMap::new();

        // Resolve selected catalog rows. The outer catalog scan skips current demos;
        // rows entering this stage still need their individual statistics and asset references,
        // including when another collection already generated the shared round file.
        let mut pending = Vec::new();
        for collection in &collection_data.collections {
            if !self.should_process_collection(collection) {
                continue;
            }
            if self
                .cli_args
                .collection_nums
                .as_ref()
                .is_some_and(|numbers| !numbers.contains(&collection.collection_num))
            {
                continue;
            }

            let s2r_path = self.generate_s2r_output_path(collection, collection_data)?;
            let range = self.calculate_tick_range(collection, collection_data)?;
            pending.push(PendingCollection {
                collection,
                s2r_path,
                range,
            });
        }

        if pending.is_empty() {
            return Ok(results);
        }

        let started = std::time::Instant::now();

        // When ram_unzip is enabled, pass None for both resolve and load to trigger in-memory decompression
        let unzip_param = if self.config.ram_unzip {
            None
        } else {
            self.config.unzip_dir.as_deref()
        };

        // Resolve the actual demo path (check unzip_dir for existing decompressed file first, if disk mode)
        let demo_path = resolve_demo_path(&collection_data.demo_info.demo_path, unzip_param)?;

        // Load and decompress once for every collection in this source.
        let demo = load_and_validate_demo(&demo_path, unzip_param)
            .map_err(|e| anyhow!("Failed to load demo: {}", e))?;

        // Group identical round windows so multiple collections in one round share the same
        // parser output. Then choose between sparse checkpoint-backed windows and one union
        // parse based on the actual retained byte estimate.
        let mut groups: BTreeMap<(u32, u32), Vec<PendingCollection<'_>>> = BTreeMap::new();
        for work in pending {
            groups.entry(work.range).or_default().push(work);
        }
        let write_count = groups.values().map(Vec::len).sum::<usize>();
        let mut completed_replays = 0;
        self.report_replay(collection_data, "reading", 0, write_count);
        let window_builder = demo_writer::ParserWindowBuilder::new(demo.data())?;
        let estimated_window_bytes = groups.keys().try_fold(0u64, |total, &(start, end)| {
            window_builder
                .estimate_window_bytes(start as i32, end as i32)
                .and_then(|bytes| {
                    total
                        .checked_add(bytes)
                        .ok_or_else(|| anyhow!("parser window byte estimate overflow"))
                })
        })?;
        let use_windows = estimated_window_bytes < window_builder.source_bytes();
        let tick_start = groups.keys().map(|range| range.0).min().unwrap();
        let tick_end = groups.keys().map(|range| range.1).max().unwrap();

        // Ordered event authority cannot be reconstructed faithfully from a checkpoint alone:
        // smoke voxel vectors and source-order keys depend on messages before the window. Parse
        // that lane once from the original source, then share it with every player window.
        let huf = initialize_huffman_table();
        let event_settings =
            create_event_authority_parser_settings(tick_start as i32, tick_end as i32, huf);
        let mut event_parser = Parser::new(
            event_settings,
            parser::parse_demo::ParsingMode::ForceSingleThreaded,
        );
        let mut event_output = event_parser
            .parse_demo(demo.data())
            .map_err(|e| anyhow!("Failed to parse shared event authority: {}", e))?;

        crate::round_replay::normalize_embedded_ticks(&mut event_output);

        if use_windows {
            println!(
                "  Authority plan: {} checkpoint windows ({:.1}% of source)",
                groups.len(),
                estimated_window_bytes as f64 / window_builder.source_bytes() as f64 * 100.0
            );
            for (&range, work) in &groups {
                let bytes = window_builder.build_window(range.0 as i32, range.1 as i32)?;
                let window_results = self.process_authority_window(
                    &bytes,
                    range,
                    work,
                    collection_data,
                    &event_output,
                    &mut duckdb_updates,
                    buffer,
                    &mut completed_replays,
                    write_count,
                )?;
                results.extend(window_results);
            }
        } else {
            println!(
                "  Authority plan: shared source pass (windows would retain {:.1}%)",
                estimated_window_bytes as f64 / window_builder.source_bytes() as f64 * 100.0
            );
            let all_work: Vec<PendingCollection<'_>> = groups.into_values().flatten().collect();
            let window_results = self.process_authority_window(
                demo.data(),
                (tick_start, tick_end),
                &all_work,
                collection_data,
                &event_output,
                &mut duckdb_updates,
                buffer,
                &mut completed_replays,
                write_count,
            )?;
            results.extend(window_results);
        }

        // Print single summary per demo
        if write_count > 0 {
            let demo_name = std::path::Path::new(&collection_data.demo_info.demo_path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown");
            println!(
                "  ✓ Prepared replay references for {} collections in {} ({:.2}s)",
                write_count,
                demo_name.trim_end_matches(".dem"),
                started.elapsed().as_secs_f64()
            );
        }

        // Apply all batched DuckDB updates only if no buffer is provided
        if buffer.is_none() {
            self.apply_batched_duckdb_updates(duckdb_updates)?;
        }

        Ok(results)
    }

    fn process_authority_window(
        &self,
        demo_bytes: &[u8],
        parse_range: (u32, u32),
        work: &[PendingCollection<'_>],
        collection_data: &KillCollectionData,
        event_output: &DemoOutput,
        duckdb_updates: &mut HashMap<PathBuf, Vec<DuckDBUpdate>>,
        buffer: Option<&CollectionBuffer>,
        completed_replays: &mut usize,
        total_replays: usize,
    ) -> Result<Vec<ProcessResult>> {
        let huf = initialize_huffman_table();
        let tracked_steamids = self.get_tracked_steamids(work[0].collection, collection_data, true);

        let player_settings = create_multi_player_parser_settings(
            parse_range.0 as i32,
            parse_range.1 as i32,
            huf,
            false,
        );
        let mut player_parser =
            Parser::new(player_settings, parser::parse_demo::ParsingMode::Normal)
                .with_animation_recipes(false);
        let player_output = player_parser
            .parse_demo(demo_bytes)
            .map_err(|e| anyhow!("Failed to parse shared player authority: {}", e))?;

        // The source-ordered event lane carries every authoritative create/update/delete,
        // but its one full inventory seed belongs to the earliest requested match tick.  A
        // later checkpoint window therefore needs one compact seed of its own so a weapon
        // that stayed unchanged between rounds is still present at this replay's first tick.
        // Keep only that first checkpoint observation, then use the original-source stream
        // for all subsequent changes.  This is O(weapons per window), not O(weapons * ticks).
        let weapon_snapshots = weapon_snapshots_for_window(
            &event_output.weapon_entity_snapshots,
            &player_output.weapon_entity_snapshots,
            parse_range.0 as i32,
            parse_range.1 as i32,
        );

        let processing_config = ProcessingConfig::new(tracked_steamids[0])
            .with_debug(false)
            .with_data_offset(0);
        let mut processor = TickDataProcessor::new(processing_config);
        let shared_records = processor
            .process_demo_output_multi_player(&player_output, &tracked_steamids)
            .map_err(|e| anyhow!("Failed to process shared player authority: {}", e))?;

        // Several killers can produce collections in the same round. Slice and clone that
        // round's player records once, then share the immutable map across every S2R writer for
        // the range instead of deep-cloning every TickRecord per collection.
        let records_by_range: BTreeMap<(u32, u32), HashMap<u64, Vec<TickRecord>>> = work
            .iter()
            .map(|item| item.range)
            .filter(|range| *range != parse_range)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .map(|range| {
                let selected = shared_records
                    .iter()
                    .map(|(&steamid, records)| {
                        let start = records.partition_point(|record| record.tick < range.0 as i32);
                        let end = records.partition_point(|record| record.tick <= range.1 as i32);
                        (steamid, records[start..end].to_vec())
                    })
                    .collect();
                (range, selected)
            })
            .collect();

        let mut results = Vec::with_capacity(work.len());
        let mut written = std::collections::HashSet::new();
        for item in work {
            let grouped_records = if item.range == parse_range {
                &shared_records
            } else {
                records_by_range
                    .get(&item.range)
                    .expect("every pending replay range was sliced")
            };
            results.push(
                self.process_single_collection(
                    item.collection,
                    collection_data,
                    grouped_records,
                    &player_output,
                    event_output,
                    &weapon_snapshots,
                    item.s2r_path.clone(),
                    item.range,
                    written.insert(item.s2r_path.clone()),
                    duckdb_updates,
                    buffer,
                )
                .map_err(|error| {
                    anyhow!(
                        "Failed to process collection {}: {:#}",
                        item.collection.collection_num,
                        error
                    )
                })?,
            );
            *completed_replays += 1;
            self.report_replay(collection_data, "writing", *completed_replays, total_replays);
        }
        Ok(results)
    }

    /// Process a single collection
    fn process_single_collection(
        &self,
        collection: &Collection,
        collection_data: &KillCollectionData,
        grouped_records: &HashMap<u64, Vec<TickRecord>>,
        player_output: &DemoOutput,
        event_output: &DemoOutput,
        weapon_snapshots: &[parser::second_pass::parser_settings::WeaponEntitySnapshot],
        s2r_path: PathBuf,
        range: (u32, u32),
        first_write: bool,
        duckdb_updates: &mut HashMap<PathBuf, Vec<DuckDBUpdate>>,
        buffer: Option<&CollectionBuffer>,
    ) -> Result<ProcessResult> {
        let (tick_start, tick_end) = range;
        // Player records deliberately omit repeated dead ticks. Keep the replay clock
        // running through the actual parsed interval so lingering effects/audio survive.
        let last_parsed_tick = player_output.df.get(&parser::first_pass::prop_controller::TICK_ID)
            .and_then(|column| match &column.data {
                Some(parser::second_pass::variants::VarVec::I32(ticks)) => ticks.iter().flatten().copied().max(),
                _ => None,
            }).unwrap_or(tick_end as i32);
        let playback_range = (tick_start as i32, (tick_end as i32).min(last_parsed_tick));
        let mut tracked_steamids: Vec<u64> = grouped_records.keys().copied().collect();
        tracked_steamids.sort_unstable();

        let grenade_source = event_output;
        anyhow::ensure!(grenade_source.utility.captured,
            "Refusing replay generation without utility authority capture");

        // The event/projectile pass is the single authoritative raw-audio
        // source. Its parser records are globally source-ordered; keep only
        // this collection's inclusive replay window before serializing S2R.
        let audio_events =
            audio_events_for_replay_range(&grenade_source.audio_events, tick_start, tick_end);

        let weapon_fire_events = super::grenade_processor::process_weapon_fire_events(
            grenade_source,
            collection.steam_id,
            tick_start,
            tick_end,
        )
        .unwrap_or_else(|e| {
            eprintln!("Warning: Failed to process weapon fire events: {}", e);
            Vec::new()
        });

        let utility_thrown = super::grenade_processor::process_utility_thrown(
            grenade_source,
            collection.steam_id,
            tick_start,
            tick_end,
        )
        .unwrap_or_else(|e| {
            eprintln!("Warning: Failed to process utility thrown: {}", e);
            Vec::new()
        });

        let grenade_trajectories = if self.config.pad_ticks != 0 && self.config.grenade_trajectory_mode > 0 {
            let filter_steamids = if self.config.grenade_trajectory_mode == 1 {
                vec![collection.steam_id]
            } else {
                tracked_steamids.clone()
            };
            super::grenade_processor::process_grenade_trajectories(
                grenade_source,
                &filter_steamids,
                tick_start,
                tick_end,
            )
            .unwrap_or_else(|e| {
                eprintln!("Warning: Failed to process grenade trajectories: {}", e);
                Vec::new()
            })
        } else {
            Vec::new()
        };

        let grenade_stats =
            super::grenade_processor::calculate_grenade_stats(&weapon_fire_events, &utility_thrown);

        // Detect weapon fire sequences for the killer
        let killer_records = grouped_records
            .get(&collection.steam_id)
            .map(|records| records.as_slice())
            .unwrap_or(&[]);

        // DISABLED: Weapon fire detection - will be redone later
        // let weapon_fire_sequences = weapon_fire_detector::detect_fire_sequences(
        //     killer_records,
        //     collection.steam_id,
        //     collection,
        // );

        // Detect weapon switch ticks for the killer
        let weapon_switch_ticks = detect_weapon_switches(killer_records);

        // Format util_thrown for master CSV
        let util_thrown = format!("[{}]", grenade_stats.util_thrown.join(";"));

        // CSV writing logic removed entirely as per request

        // The S2R output goes through a temp file and an atomic rename, so an interrupted run
        // can never leave a truncated file that the skip check above would later accept.
        // Each write is described into `assets` so the database records what is actually on
        // disk, per format, rather than a single opaque TickData flag.
        let mut assets: Vec<TickAsset> = Vec::new();

        let shared_round = self.config.pad_ticks == 0;
        let needs_write = first_write && (self.config.overwrite
            || !super::s2r_output::is_current_s2r_for(&s2r_path, self.config.skip_buy_time, self.config.pad_ticks)
            || !super::s2r_output::matches_source(&s2r_path, std::path::Path::new(&collection_data.demo_info.demo_path))
            || !super::s2r_output::has_tick_range(&s2r_path, (playback_range.0 as u32, playback_range.1 as u32)));
        if needs_write {
            let mut round_fire = Vec::new();
            let mut round_util = Vec::new();
            if shared_round {
                for sid in &tracked_steamids {
                    round_fire.extend(super::grenade_processor::process_weapon_fire_events(grenade_source, *sid, tick_start, tick_end)?);
                    round_util.extend(super::grenade_processor::process_utility_thrown(grenade_source, *sid, tick_start, tick_end)?);
                }
                round_fire.sort_by_key(|event| (event.tick, event.attacker_steamid));
                round_util.sort_by_key(|event| (event.tick_throw, event.entity_id));
            }
            let round_trajectories = if shared_round {
                super::grenade_processor::process_grenade_trajectories(grenade_source, &tracked_steamids, tick_start, tick_end)?
            } else { Vec::new() };

            if let Err(e) = super::atomic_write::write_atomically(&s2r_path, |temp| {
                super::s2r_output::write_replay_s2r(
                    temp,
                    grouped_records,
                    collection,
                    collection_data,
                    if shared_round { &round_util } else { &utility_thrown },
                    if shared_round { &round_trajectories } else { &grenade_trajectories },
                    if shared_round { &round_fire } else { &weapon_fire_events },
                    &grenade_stats,
                    &weapon_switch_ticks,
                    self.config.pad_ticks,
                    &audio_events,
                    &grenade_source.smoke_voxels,
                    weapon_snapshots,
                    &grenade_source.game_events,
                    self.config.skip_buy_time,
                    Some(playback_range),
                    shared_round,
                    &grenade_source.infernos,
                    Some(&grenade_source.utility),
                    &grenade_source.world_entities,
                    &grenade_source.ag2_recipes,
                )
            }) {
                eprintln!(
                    "Warning: Failed to write S2R for collection {}: {}",
                    collection.collection_num, e
                );
                // Record the failure rather than leaving a silent gap: a missing row and a
                // failed row mean different things when reconciling the database to disk.
                assets.push(TickAsset {
                    demo_name: collection.demo_name.trim_end_matches(".dem").to_string(),
                    collection_num: collection.collection_num as i32,
                    collection_type: collection.collection_type.clone(),
                    folder: collection.folder.clone(),
                    format: AssetFormat::S2r,
                    format_version: super::s2r_output::S2R_VERSION as i32,
                    path: self.stored_s2r_path(&s2r_path, &collection.folder),
                    size_bytes: 0,
                    checksum: String::new(),
                    status: AssetStatus::Failed,
                    grenade_traj: if self.config.pad_ticks == 0 { 2 } else { self.config.grenade_trajectory_mode as i32 },
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
                if let Some(buffer) = buffer {
                    buffer.record_assets(assets);
                }
                return Err(anyhow!(
                    "Failed to write S2R collection {}: {}",
                    collection.collection_num,
                    e
                ));
            }
        }
        assets.push(self.describe_asset(collection, &s2r_path, AssetFormat::S2r,
            super::s2r_output::S2R_VERSION as i32));

        // Update buffer or queue DuckDB update based on whether buffer is provided
        if let Some(buffer) = buffer {
            // Update the collection in RAM buffer
            let weapons_formatted = if !grenade_stats.weapons_damaged.is_empty()
                && grenade_stats.weapons_damaged.len()
                    == grenade_stats.weapons_damaged_num_hits.len()
            {
                let formatted_parts: Vec<String> = grenade_stats
                    .weapons_damaged
                    .iter()
                    .zip(grenade_stats.weapons_damaged_num_hits.iter())
                    .map(|(w, h)| format!("{}({})", w, h))
                    .collect();
                format!("[{}]", formatted_parts.join(" - "))
            } else {
                String::new()
            };

            let updated = buffer.update_with_tickbytick(
                &collection.collection_type,
                &collection.folder,
                &collection.demo_name.trim_end_matches(".dem"),
                collection.collection_num,
                &util_thrown,
                if shared_round { 2 } else { self.config.grenade_trajectory_mode },
                grenade_stats.hits as u32,
                grenade_stats.misses as u32,
                grenade_stats.hit_rate,
                &weapons_formatted,
            );

            if !updated {
                eprintln!(
                    "Warning: Failed to update collection {} in buffer",
                    collection.collection_num
                );
            }

            buffer.record_assets(assets);
        } else {
            // Queue DuckDB update for batched processing (legacy path)
            self.queue_duckdb_update(collection, &util_thrown, &grenade_stats, duckdb_updates)?;
        }

        Ok(ProcessResult {
            collection_num: collection.collection_num,
            collection_type: collection.collection_type.clone(),
            output_path: s2r_path,
            records_count: grouped_records.values().map(|v| v.len()).sum(),
        })
    }

    /// Describe a freshly written output file for the `tick_assets` table.
    ///
    /// Size and checksum are read back from the file that actually landed, so the recorded
    /// row describes disk rather than intent. If the file cannot be read the asset is
    /// marked failed rather than recorded as complete with unknown contents.
    fn describe_asset(
        &self,
        collection: &Collection,
        path: &std::path::Path,
        format: AssetFormat,
        format_version: i32,
    ) -> TickAsset {
        let (size_bytes, checksum, status) =
            match interface::models::tick_asset::checksum_file(path) {
                Ok((size, sum)) => (size, sum, AssetStatus::Complete),
                Err(e) => {
                    eprintln!(
                        "Warning: wrote {} but could not read it back: {}",
                        path.display(),
                        e
                    );
                    (0, String::new(), AssetStatus::Failed)
                }
            };

        let authority = if format == AssetFormat::S2r && status == AssetStatus::Complete {
            read_s2r_authority_summary(path).unwrap_or_default()
        } else {
            S2rAuthoritySummary::default()
        };

        TickAsset {
            demo_name: collection.demo_name.trim_end_matches(".dem").to_string(),
            collection_num: collection.collection_num as i32,
            collection_type: collection.collection_type.clone(),
            folder: collection.folder.clone(),
            format,
            format_version,
            path: self.stored_s2r_path(path, &collection.folder),
            size_bytes,
            checksum,
            status,
            grenade_traj: if self.config.pad_ticks == 0 { 2 } else { self.config.grenade_trajectory_mode as i32 },
            authority_bytes: authority.bytes,
            agent_life_count: authority.agent_lives,
            weapon_lifetime_count: authority.weapon_lifetimes,
            inventory_delta_count: authority.inventory_deltas,
            world_weapon_delta_count: authority.world_weapon_deltas,
            checkpoint_tick: None,
            logical_start_tick: None,
            logical_end_tick: None,
            source_path: None,
            source_bytes: None,
        }
    }

    /// Determine if a collection should be processed based on type filters
    fn should_process_collection(&self, collection: &Collection) -> bool {
        self.config
            .should_process_collection_type(&collection.collection_type)
    }

    /// Calculate the tick range for processing based on padding configuration.
    ///
    /// Preserve post-win gameplay up to, but excluding, the next respawn/buy period.
    /// Only the explicit freeze interval is idle time; stationary live play is retained.
    fn calculate_tick_range(&self, collection: &Collection, data: &KillCollectionData) -> Result<(u32, u32)> {
        let next_start = data.rounds.iter().find(|round| round.round == collection.round)
            .and_then(|round| round.next_start_tick)
            .or_else(|| data.rounds.iter().map(|round| round.start_tick)
                .filter(|tick| *tick > collection.round_start_tick).min());
        let (round_start, round_last) = round_replay_window(collection.round_start_tick,
            collection.round_freeze_end, collection.round_end_tick, next_start, self.config.skip_buy_time);
        let round_last = round_last.min(data.demo_info.total_ticks);
        if self.config.pad_ticks == 0 {
            Ok((round_start, round_last))
        } else {
            // Integer padding mode - parse 1 less tick than before
            let padding = self.config.pad_ticks as u32;
            let tick_start = collection.start_kill_tick.saturating_sub(padding);
            // Modified: EndKillTick + padding - 3 (was -2, now -3 for 1 less tick)
            let tick_end = collection
                .end_kill_tick
                .saturating_add(padding)
                .saturating_sub(3);

            Ok((tick_start.max(round_start), tick_end.min(round_last)))
        }
    }

    /// Get the list of SteamIDs to track for this collection
    fn get_tracked_steamids(
        &self,
        collection: &Collection,
        collection_data: &KillCollectionData,
        require_all_players: bool,
    ) -> Vec<u64> {
        let mut steamids = Vec::new();

        // Always track the killer
        steamids.push(collection.steam_id);

        if self.config.track_all_players || require_all_players {
            // Track all players
            for player in &collection_data.players {
                if !steamids.contains(&player.steam_id) {
                    steamids.push(player.steam_id);
                }
            }
        } else {
            // Track only killer and victims
            if let Some(details) = collection_data
                .collection_details
                .get(&collection.collection_num)
            {
                for detail in details {
                    if !steamids.contains(&detail.victim_steamid) {
                        steamids.push(detail.victim_steamid);
                    }
                }
            }
        }

        steamids
    }

    /// Generate the sole replay payload path using the shortened naming convention.
    /// Format: {TYPE}_{DEMONAME}_{COLLECTIONNUM}.s2r
    fn generate_s2r_output_path(
        &self,
        collection: &Collection,
        _collection_data: &KillCollectionData,
    ) -> Result<PathBuf> {
        let mut demo_name = collection.demo_name.as_str();
        for extension in [".zst", ".gz", ".dem"] {
            if let Some(stem) = demo_name.strip_suffix(extension) { demo_name = stem; }
        }

        // Use output directory from CLI or config, otherwise use current directory
        let base_output_dir = if let Some(dir) = &self.cli_args.output_dir {
            dir.clone()
        } else if let Some(dir) = &self.config.parser_output {
            dir.clone()
        } else {
            PathBuf::from(".")
        };

        if self.config.pad_ticks == 0 {
            return Ok(config::storage::round_replay_path(&base_output_dir, &collection.collection_type, &collection.folder, demo_name, collection.round));
        }
        Ok(config::storage::replay_path(&base_output_dir, &collection.collection_type,
            &collection.folder, demo_name, collection.round, collection.collection_num))
    }

    fn stored_s2r_path(&self, path: &std::path::Path, folder: &str) -> String {
        let root = self.cli_args.output_dir.as_deref().or(self.config.parser_output.as_deref())
            .unwrap_or_else(|| std::path::Path::new("."));
        config::storage::stored_replay_path(root, path, folder)
    }

    /// Queue a DuckDB update for batched processing
    fn queue_duckdb_update(
        &self,
        collection: &Collection,
        util_thrown: &str,
        grenade_stats: &super::grenade_processor::GrenadeStats,
        duckdb_updates: &mut HashMap<PathBuf, Vec<DuckDBUpdate>>,
    ) -> Result<()> {
        // Get the DuckDB file path
        let base_output_dir = if let Some(dir) = &self.cli_args.output_dir {
            dir.clone()
        } else if let Some(dir) = &self.config.parser_output {
            dir.clone()
        } else {
            PathBuf::from(".")
        };

        let duckdb_file_path = config::storage::database_path(&base_output_dir,
            &collection.collection_type, &collection.folder);

        // Only queue if the DuckDB file exists
        if !duckdb_file_path.exists() {
            return Ok(());
        }

        // Format weapons damaged hits
        // weapons_damaged: [ak47, glock]
        // weapons_damaged_num_hits: [8, 2]
        // Output: [ak47(8) - glock(2)]
        let mut weapons_formatted = String::new();
        if !grenade_stats.weapons_damaged.is_empty()
            && grenade_stats.weapons_damaged.len() == grenade_stats.weapons_damaged_num_hits.len()
        {
            let formatted_parts: Vec<String> = grenade_stats
                .weapons_damaged
                .iter()
                .zip(grenade_stats.weapons_damaged_num_hits.iter())
                .map(|(w, h)| format!("{}({})", w, h))
                .collect();
            weapons_formatted = format!("[{}]", formatted_parts.join(" - "));
        }

        // Create update record
        let update = DuckDBUpdate {
            demo_name: collection.demo_name.trim_end_matches(".dem").to_string(),
            collection_num: collection.collection_num,
            util_thrown: util_thrown.to_string(),
            traj_mode: if self.config.pad_ticks == 0 { 2 } else { self.config.grenade_trajectory_mode },
            hits: grenade_stats.hits as u32,
            misses: grenade_stats.misses as u32,
            hit_rate: grenade_stats.hit_rate,
            weapons_formatted,
        };

        // Add to batch for this DuckDB file
        duckdb_updates
            .entry(duckdb_file_path)
            .or_insert_with(Vec::new)
            .push(update);

        Ok(())
    }

    /// Apply all batched DuckDB updates efficiently with graceful error handling
    fn apply_batched_duckdb_updates(
        &self,
        duckdb_updates: HashMap<PathBuf, Vec<DuckDBUpdate>>,
    ) -> Result<()> {
        if duckdb_updates.is_empty() {
            return Ok(());
        }

        use duckdb::Connection;

        for (duckdb_file_path, updates) in duckdb_updates {
            if updates.is_empty() {
                continue;
            }

            // Lock the DuckDB file for thread-safe writing
            let entry = self
                .master_file_lock
                .entry(duckdb_file_path.clone())
                .or_insert_with(|| Arc::new(Mutex::new(())));
            let lock = Arc::clone(entry.value());
            let _guard = lock.lock().unwrap();

            // Open connection once for all updates to this file
            let conn = match Connection::open(&duckdb_file_path) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!(
                        "Warning: Failed to open DuckDB file {}: {}",
                        duckdb_file_path.display(),
                        e
                    );
                    continue;
                }
            };

            // Begin transaction for batch update
            if let Err(e) = conn.execute("BEGIN TRANSACTION", []) {
                eprintln!("Warning: Failed to begin transaction: {}", e);
                continue;
            }

            // Prepare update statement using correct column names (TickData and GrenadeTraj)
            let mut stmt = match conn.prepare(
                "UPDATE kill_collections
                 SET TickData = ?, GrenadeTraj = ?, util_thrown = ?,
                     hits = ?, misses = ?, hit_rate = ?, weapons_formatted = ?
                 WHERE demo_name = ? AND collection_num = ?",
            ) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("Warning: Failed to prepare DuckDB statement: {}", e);
                    let _ = conn.execute("ROLLBACK", []);
                    continue;
                }
            };

            let mut total_updated = 0;
            let mut failed_updates = 0;

            // Execute all updates
            for update in &updates {
                let result = stmt.execute(duckdb::params![
                    1i64,                    // TickData = 1 (Parsed)
                    update.traj_mode as i64, // GrenadeTraj
                    &update.util_thrown,
                    update.hits,
                    update.misses,
                    update.hit_rate,
                    &update.weapons_formatted,
                    &update.demo_name,
                    update.collection_num as i64,
                ]);

                match result {
                    Ok(rows) => {
                        if rows > 0 {
                            total_updated += 1;
                        } else {
                            failed_updates += 1;
                        }
                    }
                    Err(e) => {
                        eprintln!(
                            "Warning: Error updating collection {}: {}",
                            update.collection_num, e
                        );
                        failed_updates += 1;
                    }
                }
            }

            drop(stmt);

            // Commit transaction
            if let Err(e) = conn.execute("COMMIT", []) {
                eprintln!("Warning: Failed to commit transaction: {}", e);
                continue;
            }

            println!(
                "✓ DuckDB update: {} - {}/{} collections updated",
                duckdb_file_path.file_name().unwrap().to_string_lossy(),
                total_updated,
                updates.len()
            );

            if failed_updates > 0 {
                eprintln!(
                    "  Note: {} updates didn't match existing collections",
                    failed_updates
                );
            }
        }

        Ok(())
    }
}

#[allow(dead_code)]
#[derive(Debug)]
pub struct ProcessResult {
    pub collection_num: u32,
    pub collection_type: String,
    pub output_path: PathBuf,
    pub records_count: usize,
}

/// Detect ticks where the killer switched weapons.
/// Only includes switches where the previous weapon was held for at least 8 ticks.
pub fn detect_weapon_switches(records: &[TickRecord]) -> Vec<i32> {
    if records.is_empty() {
        return Vec::new();
    }

    let mut switch_ticks = Vec::new();
    let mut current_weapon = &records[0].weapon;
    let mut weapon_tick_count = 1;

    for record in records.iter().skip(1) {
        if record.weapon == *current_weapon {
            // Same weapon, increment count
            weapon_tick_count += 1;
        } else {
            // Weapon changed - check if previous weapon was held for at least 8 ticks
            if weapon_tick_count >= 8 {
                switch_ticks.push(record.tick);
            }

            // Start tracking new weapon
            current_weapon = &record.weapon;
            weapon_tick_count = 1;
        }
    }

    switch_ticks
}
