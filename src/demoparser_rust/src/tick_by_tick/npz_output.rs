//! NPZ output writer for CS2 demo tick-by-tick data
//!
//! This module writes demo data in NumPy NPZ format for efficient import
//! into Blender for animation and visualization. The format uses columnar
//! arrays with schema versioning and embedded metadata.

use super::data_types::TickRecord;
use super::grenade_processor::{GrenadeStats, GrenadeTrajectory, UtilityThrown, WeaponFireEvent};
use super::kill_collection_parser::{Collection, KillCollectionData};
use super::weapon_inspect::WeaponInspectSequence;
use super::weapon_mapper::create_weapon_name_to_id_map;
use anyhow::{anyhow, Result};
use ndarray::{Array1, Array2, Array3};
use ndarray_npy::NpzWriter;
use serde::Serialize;
use std::collections::HashMap;
use std::fs::File;
use std::path::Path;

/// Major revision of the NPZ schema (the leading component of `SchemaInfo::version`).
/// Recorded per asset in `tick_assets` so a stale output is detectable in SQL.
pub const NPZ_FORMAT_VERSION: u16 = 1;

/// Schema version information for NPZ format
#[derive(Debug, Serialize)]
pub struct SchemaInfo {
    pub name: &'static str,
    pub version: &'static str,
    pub tick_axis: &'static str,
    pub player_axis: &'static str,
    pub index_sentinel: u8,
    pub flags_bits: HashMap<&'static str, u8>,
}

impl Default for SchemaInfo {
    fn default() -> Self {
        let mut bits = HashMap::new();
        bits.insert("in_reload", 0);
        bits.insert("scoped", 1);
        bits.insert("inspecting", 2);
        bits.insert("airborne", 3);
        bits.insert("walking", 4);
        bits.insert("defusing", 5);
        bits.insert("fw", 6);
        bits.insert("lf", 7);
        bits.insert("rt", 8);
        bits.insert("bk", 9);
        bits.insert("fire", 10);
        Self {
            name: "cs2_npz",
            version: "1.0.0",
            tick_axis: "T",
            player_axis: "P",
            index_sentinel: 255,
            flags_bits: bits,
        }
    }
}

/// Unit information for data arrays
#[derive(Debug, Serialize)]
pub struct UnitsInfo {
    pub pos: &'static str,
    pub angles: &'static str,
    pub vel: &'static str,
    pub tickrate: Option<u16>,
}

impl Default for UnitsInfo {
    fn default() -> Self {
        Self {
            pos: "hammer_units",
            angles: "degrees",
            vel: "units_per_sec",
            tickrate: Some(64),
        }
    }
}

/// Player metadata for NPZ
#[derive(Debug, Serialize)]
pub struct PlayerMeta {
    pub steamid: String,
    pub name: String,
    pub index: usize,
    pub team: String,
}

/// Blender binding information
#[derive(Debug, Serialize)]
pub struct BindingInfo {
    pub target: String,
    pub name: String,
    pub path: String,
    pub index: i32,
    pub source: String,
    pub slice: HashMap<String, usize>,
}

/// Complete metadata structure
#[derive(Debug, Serialize)]
pub struct NpzMetadata {
    pub schema: SchemaInfo,
    pub units: UnitsInfo,
    pub players: Vec<PlayerMeta>,
    pub kill_collection: CollectionInfo,
    pub bindings: Vec<BindingInfo>,
}

/// Kill collection information for metadata
#[derive(Debug, Serialize)]
pub struct CollectionInfo {
    pub collection_type: String,
    pub collection_num: u32,
    pub tick_duration: u32,
    pub map_name: String,
    pub game_version: String,
    pub killer_index: u32,
    pub killer_team: String,
    pub start_tick: u32,
    pub end_tick: u32,
    pub killer_name: String,
    pub killer_steamid: u64,
    pub demo_name: String,
    pub folder: String,
    pub killer_radius: f64,
    pub victims_radius: f64,
    pub killer_move_distance: f64,
    pub victim_team: String,
    pub round_start_tick: u32,
    pub round_end_tick: u32,
    pub round_freeze_end: u32,
    pub round: u32,
    pub weapons: String,
    pub weapons_id: String,
    pub kill_ticks: String,
    pub victims_index: String,
    pub weapon_switch_ticks: Vec<i32>,
    pub padding: i32,
    pub ticks_tracked: u32,
    pub game_start_offset: f64,
    pub parsed: u32,
    pub hits: u32,
    pub misses: u32,
    pub hit_rate: f32,
    pub util_thrown: Vec<String>,
    pub util_thrown_ticks: Vec<i32>,
    pub util_land_ticks: Vec<i32>,
    pub weapons_damaged: Vec<String>,
    pub weapons_damaged_num_hits: Vec<u32>,
}

/// Dense array data structure
struct DenseArrays {
    frames: Array1<i32>,
    pos: Array3<f32>,
    angles: Array3<f32>,
    vel: Array3<f32>,
    ammo: Array2<i16>,
    weapon_id: Array2<u16>,
    flags: Array2<u16>,
    mouse_vel: Array2<f32>,
}

/// Weapon fire CSR arrays
struct WeaponFireArrays {
    wf_tick: Array1<i32>,
    wf_impact_tick: Array1<i32>,
    wf_attacker: Array1<u8>,
    wf_weapon_id: Array1<u16>,
    wf_offsets: Array1<i32>,
    wf_victim_idx: Array1<u8>,
    wf_damage: Array1<u16>,
    wf_kill: Array1<u8>,
}

/// Utility thrown arrays
struct UtilityArrays {
    ut_throw_tick: Array1<i32>,
    ut_land_tick: Array1<i32>,
    ut_thrower: Array1<u8>,
    ut_type: Array1<u8>,
    ut_id: Array1<u32>,
    ut_pos_land: Array2<f32>,
}

/// Grenade trajectory CSR arrays
struct TrajectoryArrays {
    gt_offsets: Array1<i32>,
    gt_thrower: Array1<u8>,
    gt_type: Array1<u8>,
    gt_id: Array1<u32>,
    gt_tick: Array1<i32>,
    gt_pos: Array2<f32>,
}

/// Pack tick record flags into a u16 bitfield
/// Bit layout:
/// 0: in_reload
/// 1: scoped
/// 2: inspecting
/// 3: airborne
/// 4: walking
/// 5: defusing
/// 6: fw (forward)
/// 7: lf (left)
/// 8: rt (right)
/// 9: bk (back)
/// 10: fire
fn pack_flags(record: &TickRecord) -> u16 {
    let mut flags: u16 = 0;
    if record.in_reload > 0 {
        flags |= 1 << 0;
    }
    if record.scoped > 0 {
        flags |= 1 << 1;
    }
    if record.inspecting > 0 {
        flags |= 1 << 2;
    }
    if record.airborne > 0 {
        flags |= 1 << 3;
    }
    if record.walking > 0 {
        flags |= 1 << 4;
    }
    if record.defusing > 0 {
        flags |= 1 << 5;
    }
    if record.fw > 0 {
        flags |= 1 << 6;
    }
    if record.lf > 0 {
        flags |= 1 << 7;
    }
    if record.rt > 0 {
        flags |= 1 << 8;
    }
    if record.bk > 0 {
        flags |= 1 << 9;
    }
    if record.fire > 0 {
        flags |= 1 << 10;
    }
    flags
}

/// Build dense arrays from grouped tick records
fn build_dense_arrays(
    grouped_records: &HashMap<u64, Vec<TickRecord>>,
    steamid_to_idx: &HashMap<u64, usize>,
    min_tick: i32,
    max_tick: i32,
) -> DenseArrays {
    let n_ticks = (max_tick - min_tick + 1) as usize;
    let n_players = grouped_records.len();

    // Pre-allocate arrays with sentinels
    let mut frames = Array1::<i32>::zeros(n_ticks);
    let mut pos = Array3::<f32>::from_elem((n_ticks, n_players, 3), f32::NAN);
    let mut angles = Array3::<f32>::from_elem((n_ticks, n_players, 2), f32::NAN);
    let mut vel = Array3::<f32>::from_elem((n_ticks, n_players, 4), f32::NAN);
    let mut ammo = Array2::<i16>::from_elem((n_ticks, n_players), -1);
    let mut weapon_id = Array2::<u16>::from_elem((n_ticks, n_players), 0xFFFF);
    let mut flags = Array2::<u16>::zeros((n_ticks, n_players));
    let mut mouse_vel = Array2::<f32>::from_elem((n_ticks, n_players), f32::NAN);

    // Canonically fill frames array (monotonic, independent of player data)
    for (i, tick) in (min_tick..=max_tick).enumerate() {
        frames[i] = tick;
    }

    // Fill arrays from grouped records
    for (&steamid, records) in grouped_records.iter() {
        let player_idx = *steamid_to_idx.get(&steamid).unwrap();

        for record in records {
            let tick_idx = (record.tick - min_tick) as usize;

            pos[[tick_idx, player_idx, 0]] = record.pos_x;
            pos[[tick_idx, player_idx, 1]] = record.pos_y;
            pos[[tick_idx, player_idx, 2]] = record.pos_z;

            angles[[tick_idx, player_idx, 0]] = record.view_pitch;
            angles[[tick_idx, player_idx, 1]] = record.view_yaw;

            vel[[tick_idx, player_idx, 0]] = record.velocity;
            vel[[tick_idx, player_idx, 1]] = record.velocity_x;
            vel[[tick_idx, player_idx, 2]] = record.velocity_y;
            vel[[tick_idx, player_idx, 3]] = record.velocity_z;

            ammo[[tick_idx, player_idx]] = record.ammo as i16;

            // Parse weapon_id with fallback chain
            let wid = record
                .weapon_id
                .parse::<u16>()
                .ok()
                .or_else(|| Some(map_weapon_name_to_id(&record.weapon)))
                .unwrap_or(0xFFFF);
            weapon_id[[tick_idx, player_idx]] = wid;

            flags[[tick_idx, player_idx]] = pack_flags(record);
            mouse_vel[[tick_idx, player_idx]] = record.mouse_velocity;
        }
    }

    DenseArrays {
        frames,
        pos,
        angles,
        vel,
        ammo,
        weapon_id,
        flags,
        mouse_vel,
    }
}

/// Build weapon fire CSR arrays
fn build_weapon_fire_arrays(
    weapon_fire_events: &[WeaponFireEvent],
    steamid_to_idx: &HashMap<u64, usize>,
    killer_steamid: u64,
) -> WeaponFireArrays {
    let n_events = weapon_fire_events.len();
    let killer_idx = *steamid_to_idx.get(&killer_steamid).unwrap_or(&0);

    let mut wf_tick = Vec::with_capacity(n_events);
    let mut wf_impact_tick = Vec::with_capacity(n_events);
    let mut wf_attacker = Vec::with_capacity(n_events);
    let mut wf_weapon_id = Vec::with_capacity(n_events);
    let mut wf_offsets = Vec::with_capacity(n_events + 1);
    let mut wf_victim_idx = Vec::new();
    let mut wf_damage = Vec::new();
    let mut wf_kill = Vec::new();

    wf_offsets.push(0);

    for event in weapon_fire_events {
        wf_tick.push(event.tick);
        wf_impact_tick.push(event.impact_tick.unwrap_or(-1));
        wf_attacker.push(killer_idx as u8);

        // Parse weapon name to ID (simplified - could use weapon_mapper)
        let weapon_id = map_weapon_name_to_id(&event.weapon);
        wf_weapon_id.push(weapon_id);

        // Add victims for this event
        for i in 0..event.victim_steamids.len() {
            let victim_idx = steamid_to_idx
                .get(&event.victim_steamids[i])
                .copied()
                .unwrap_or(255); // 255 as sentinel for unknown player
            wf_victim_idx.push(victim_idx as u8);
            wf_damage.push(event.damages[i] as u16);
            wf_kill.push(if event.kill_flags[i] { 1 } else { 0 });
        }

        wf_offsets.push(wf_victim_idx.len() as i32);
    }

    WeaponFireArrays {
        wf_tick: Array1::from(wf_tick),
        wf_impact_tick: Array1::from(wf_impact_tick),
        wf_attacker: Array1::from(wf_attacker),
        wf_weapon_id: Array1::from(wf_weapon_id),
        wf_offsets: Array1::from(wf_offsets),
        wf_victim_idx: Array1::from(wf_victim_idx),
        wf_damage: Array1::from(wf_damage),
        wf_kill: Array1::from(wf_kill),
    }
}

/// Build utility thrown arrays
fn build_utility_arrays(
    utility_thrown: &[UtilityThrown],
    steamid_to_idx: &HashMap<u64, usize>,
    _killer_steamid: u64,
) -> UtilityArrays {
    let n_util = utility_thrown.len();

    let mut ut_throw_tick = Vec::with_capacity(n_util);
    let mut ut_land_tick = Vec::with_capacity(n_util);
    let mut ut_thrower = Vec::with_capacity(n_util);
    let mut ut_type = Vec::with_capacity(n_util);
    let mut ut_id: Vec<u32> = Vec::with_capacity(n_util);
    let mut ut_pos_land = Vec::with_capacity(n_util);

    for util in utility_thrown {
        ut_throw_tick.push(util.tick_throw);
        ut_land_tick.push(util.tick_land);

        // Use actual thrower's steamid for each utility
        let thrower_idx = steamid_to_idx
            .get(&util.thrower_steamid)
            .copied()
            .unwrap_or(255);
        ut_thrower.push(thrower_idx as u8);

        ut_type.push(map_projectile_name_to_type(&util.weapon));
        ut_id.push(util.entity_id as u32);
        ut_pos_land.push(vec![util.util_pos_x, util.util_pos_y, util.util_pos_z]);
    }

    // Convert to 2D array
    let ut_pos_land_2d = if ut_pos_land.is_empty() {
        Array2::<f32>::zeros((0, 3))
    } else {
        Array2::from_shape_vec((n_util, 3), ut_pos_land.into_iter().flatten().collect()).unwrap()
    };

    UtilityArrays {
        ut_throw_tick: Array1::from(ut_throw_tick),
        ut_land_tick: Array1::from(ut_land_tick),
        ut_thrower: Array1::from(ut_thrower),
        ut_type: Array1::from(ut_type),
        ut_id: Array1::from(ut_id),
        ut_pos_land: ut_pos_land_2d,
    }
}

/// Build grenade trajectory CSR arrays
fn build_trajectory_arrays(
    grenade_trajectories: &[GrenadeTrajectory],
    steamid_to_idx: &HashMap<u64, usize>,
) -> TrajectoryArrays {
    let n_grenades = grenade_trajectories.len();

    let mut gt_offsets = Vec::with_capacity(n_grenades + 1);
    let mut gt_thrower = Vec::with_capacity(n_grenades);
    let mut gt_type = Vec::with_capacity(n_grenades);
    let mut gt_id: Vec<u32> = Vec::with_capacity(n_grenades);
    let mut gt_tick = Vec::new();
    let mut gt_pos = Vec::new();

    gt_offsets.push(0);

    for trajectory in grenade_trajectories {
        let thrower_idx = steamid_to_idx
            .get(&trajectory.steamid)
            .copied()
            .unwrap_or(255);

        gt_thrower.push(thrower_idx as u8);
        gt_type.push(map_projectile_name_to_type(&trajectory.grenade_type));
        gt_id.push(trajectory.entity_id as u32);

        // Add trajectory points
        for point in &trajectory.trajectory_points {
            gt_tick.push(point.tick);
            gt_pos.push(vec![point.pos_x, point.pos_y, point.pos_z]);
        }

        gt_offsets.push(gt_tick.len() as i32);
    }

    // Convert to 2D array
    let gt_pos_2d = if gt_pos.is_empty() {
        Array2::<f32>::zeros((0, 3))
    } else {
        let n_points = gt_pos.len();
        Array2::from_shape_vec((n_points, 3), gt_pos.into_iter().flatten().collect()).unwrap()
    };

    TrajectoryArrays {
        gt_offsets: Array1::from(gt_offsets),
        gt_thrower: Array1::from(gt_thrower),
        gt_type: Array1::from(gt_type),
        gt_id: Array1::from(gt_id),
        gt_tick: Array1::from(gt_tick),
        gt_pos: gt_pos_2d,
    }
}

/// Weapon name to ID mapping using shared weapon_mapper
fn map_weapon_name_to_id(weapon_name: &str) -> u16 {
    let weapon_map = create_weapon_name_to_id_map();
    let normalized = weapon_name.to_lowercase().replace("weapon_", "");
    weapon_map.get(&normalized).copied().unwrap_or(0) as u16
}

/// Map projectile name to type ID (normalized lowercase matching)
fn map_projectile_name_to_type(projectile_name: &str) -> u8 {
    let s = projectile_name.to_ascii_lowercase();
    match s.as_str() {
        x if x.contains("flash") => 1,
        x if x.contains("hegren") || x.contains("he") || x == "he" => 2,
        x if x.contains("smoke") => 3,
        x if x.contains("molotov") || x.contains("incgren") => 4,
        x if x.contains("decoy") => 5,
        _ => 0,
    }
}

/// Check CSR offset integrity
fn check_offsets(name: &str, offsets: &Array1<i32>, flat_len: usize) -> Result<()> {
    if offsets.len() == 0 || offsets[0] != 0 {
        return Err(anyhow!("{}: offsets must start at 0", name));
    }
    for i in 0..offsets.len() - 1 {
        if offsets[i + 1] < offsets[i] {
            return Err(anyhow!("{}: offsets decreasing at index={}", name, i));
        }
    }
    let last = *offsets.last().unwrap() as usize;
    if last != flat_len {
        return Err(anyhow!(
            "{}: last offset {} != flat_len {}",
            name,
            last,
            flat_len
        ));
    }
    Ok(())
}

/// Assert array shape matches expected
fn assert_shape(name: &str, got: (usize, usize, usize), exp: (usize, usize, usize)) -> Result<()> {
    if got != exp {
        return Err(anyhow!("{} shape {:?} != expected {:?}", name, got, exp));
    }
    Ok(())
}

/// Validate array shapes and CSR offsets
fn validate_npz_data(
    dense: &DenseArrays,
    weapon_fire: &WeaponFireArrays,
    trajectories: &TrajectoryArrays,
) -> Result<()> {
    let t = dense.frames.len();
    let p = dense.pos.shape()[1];

    // Validate dense array shapes (T, P, *)
    assert_shape("pos", dense.pos.dim(), (t, p, 3))?;
    assert_shape("angles", dense.angles.dim(), (t, p, 2))?;
    assert_shape("vel", dense.vel.dim(), (t, p, 4))?;

    if dense.ammo.dim() != (t, p) {
        return Err(anyhow!(
            "ammo shape {:?} != expected ({}, {})",
            dense.ammo.dim(),
            t,
            p
        ));
    }
    if dense.weapon_id.dim() != (t, p) {
        return Err(anyhow!(
            "weapon_id shape {:?} != expected ({}, {})",
            dense.weapon_id.dim(),
            t,
            p
        ));
    }
    if dense.flags.dim() != (t, p) {
        return Err(anyhow!(
            "flags shape {:?} != expected ({}, {})",
            dense.flags.dim(),
            t,
            p
        ));
    }
    if dense.mouse_vel.dim() != (t, p) {
        return Err(anyhow!(
            "mouse_vel shape {:?} != expected ({}, {})",
            dense.mouse_vel.dim(),
            t,
            p
        ));
    }

    // Validate weapon fire CSR integrity
    check_offsets(
        "wf",
        &weapon_fire.wf_offsets,
        weapon_fire.wf_victim_idx.len(),
    )?;

    // Validate weapon fire victim indices
    for (i, &idx) in weapon_fire.wf_victim_idx.iter().enumerate() {
        if (idx as usize) >= p && idx != 255 {
            return Err(anyhow!(
                "wf_victim_idx[{}] = {} out of bounds (p={})",
                i,
                idx,
                p
            ));
        }
    }

    // Validate trajectory CSR integrity
    if trajectories.gt_offsets.len() > 0 {
        check_offsets("gt", &trajectories.gt_offsets, trajectories.gt_tick.len())?;
    }

    Ok(())
}

/// Build metadata structure
fn build_metadata(
    collection: &Collection,
    collection_data: &KillCollectionData,
    grouped_records: &HashMap<u64, Vec<TickRecord>>,
    steamid_to_idx: &HashMap<u64, usize>,
    weapon_switch_ticks: &[i32],
    pad_ticks: i32,
    grenade_stats: &GrenadeStats,
) -> NpzMetadata {
    // Build player metadata
    let mut players_meta = Vec::new();
    for player in &collection_data.players {
        if let Some(&idx) = steamid_to_idx.get(&player.steam_id) {
            // Get team from grouped records if available
            let team = grouped_records
                .get(&player.steam_id)
                .and_then(|records| records.first())
                .map(|record| record.team.clone())
                .unwrap_or_else(|| "Unknown".to_string());

            players_meta.push(PlayerMeta {
                steamid: player.steam_id.to_string(),
                name: player.player_name.clone(),
                index: idx,
                team,
            });
        }
    }

    // Sort by index
    players_meta.sort_by_key(|p| p.index);

    // Build collection info
    let padding_value = if pad_ticks == 0 { -1 } else { pad_ticks };
    let collection_info = CollectionInfo {
        collection_type: collection.collection_type.clone(),
        collection_num: collection.collection_num,
        tick_duration: collection.tick_duration,
        map_name: collection.map_name.clone(),
        game_version: collection_data.demo_info.game_version.clone(),
        killer_index: collection.killer_index,
        killer_team: collection.killer_team.clone(),
        start_tick: collection.start_kill_tick,
        end_tick: collection.end_kill_tick,
        killer_name: collection.killer_name.clone(),
        killer_steamid: collection.steam_id,
        demo_name: collection.demo_name.clone(),
        folder: collection.folder.clone(),
        killer_radius: collection.killer_radius,
        victims_radius: collection.victims_radius,
        killer_move_distance: collection.killer_move_distance,
        victim_team: collection.victim_team.clone(),
        round_start_tick: collection.round_start_tick,
        round_end_tick: collection.round_end_tick,
        round_freeze_end: collection.round_freeze_end,
        round: collection.round,
        weapons: collection.weapons.clone(),
        weapons_id: collection.weapons_id.clone(),
        kill_ticks: collection.kill_ticks.clone(),
        victims_index: collection.victims_index.clone(),
        weapon_switch_ticks: weapon_switch_ticks.to_vec(),
        padding: padding_value,
        ticks_tracked: collection.tick_duration,
        game_start_offset: collection_data.demo_info.game_start_offset,
        parsed: 1, // Always 1 - NPZ is only written after successful parsing
        hits: grenade_stats.hits,
        misses: grenade_stats.misses,
        hit_rate: grenade_stats.hit_rate,
        util_thrown: grenade_stats.util_thrown.clone(),
        util_thrown_ticks: grenade_stats.util_thrown_ticks.clone(),
        util_land_ticks: grenade_stats.util_land_ticks.clone(),
        weapons_damaged: grenade_stats.weapons_damaged.clone(),
        weapons_damaged_num_hits: grenade_stats.weapons_damaged_num_hits.clone(),
    };

    // Build sample bindings (can be extended)
    let bindings = vec![BindingInfo {
        target: "OBJECT".to_string(),
        name: "Player_0".to_string(),
        path: "location".to_string(),
        index: 0,
        source: "pos".to_string(),
        slice: [("player".to_string(), 0), ("col".to_string(), 0)]
            .iter()
            .cloned()
            .collect(),
    }];

    NpzMetadata {
        schema: SchemaInfo::default(),
        units: UnitsInfo::default(),
        players: players_meta,
        kill_collection: collection_info,
        bindings,
    }
}

/// Main function to write collection data to NPZ format
pub fn write_collection_npz(
    output_path: &Path,
    grouped_records: &HashMap<u64, Vec<TickRecord>>,
    collection: &Collection,
    collection_data: &KillCollectionData,
    weapon_fire_events: &[WeaponFireEvent],
    utility_thrown: &[UtilityThrown],
    grenade_trajectories: &[GrenadeTrajectory],
    _grenade_stats: &GrenadeStats,
    _weapon_inspect_sequences: &[WeaponInspectSequence],
    weapon_switch_ticks: &[i32],
    pad_ticks: i32,
) -> Result<()> {
    // Build SteamID to index mapping
    let mut steamid_to_idx: HashMap<u64, usize> = HashMap::new();
    let mut sorted_steamids: Vec<u64> = grouped_records.keys().copied().collect();
    sorted_steamids.sort();
    for (idx, &steamid) in sorted_steamids.iter().enumerate() {
        steamid_to_idx.insert(steamid, idx);
    }

    // Find tick range
    let mut min_tick = i32::MAX;
    let mut max_tick = i32::MIN;
    for records in grouped_records.values() {
        for record in records {
            min_tick = min_tick.min(record.tick);
            max_tick = max_tick.max(record.tick);
        }
    }

    if min_tick == i32::MAX || max_tick == i32::MIN {
        return Err(anyhow!("No tick data found"));
    }

    // Build arrays
    let dense = build_dense_arrays(grouped_records, &steamid_to_idx, min_tick, max_tick);
    let weapon_fire =
        build_weapon_fire_arrays(weapon_fire_events, &steamid_to_idx, collection.steam_id);
    let utility = build_utility_arrays(utility_thrown, &steamid_to_idx, collection.steam_id);
    let trajectories = build_trajectory_arrays(grenade_trajectories, &steamid_to_idx);

    // Validate data
    validate_npz_data(&dense, &weapon_fire, &trajectories)?;

    // Calculate grenade stats
    let grenade_stats =
        super::grenade_processor::calculate_grenade_stats(weapon_fire_events, utility_thrown);

    // Build metadata
    let metadata = build_metadata(
        collection,
        collection_data,
        grouped_records,
        &steamid_to_idx,
        weapon_switch_ticks,
        pad_ticks,
        &grenade_stats,
    );

    // Serialize metadata to JSON bytes
    let meta_json = serde_json::to_vec(&metadata)?;
    let meta_array = Array1::from(meta_json);

    // Ensure the full directory structure exists
    if let Some(parent_dir) = output_path.parent() {
        std::fs::create_dir_all(parent_dir)?;
    }

    // Create NPZ file
    let file = File::create(output_path)?;
    let mut npz = NpzWriter::new(file);

    // Write dense arrays
    npz.add_array("frames", &dense.frames)?;
    npz.add_array("pos", &dense.pos)?;
    npz.add_array("angles", &dense.angles)?;
    npz.add_array("vel", &dense.vel)?;
    npz.add_array("ammo", &dense.ammo)?;
    npz.add_array("weapon_id", &dense.weapon_id)?;
    npz.add_array("flags", &dense.flags)?;
    npz.add_array("mouse_vel", &dense.mouse_vel)?;

    // Write weapon fire CSR
    npz.add_array("wf_tick", &weapon_fire.wf_tick)?;
    npz.add_array("wf_impact_tick", &weapon_fire.wf_impact_tick)?;
    npz.add_array("wf_attacker", &weapon_fire.wf_attacker)?;
    npz.add_array("wf_weapon_id", &weapon_fire.wf_weapon_id)?;
    npz.add_array("wf_offsets", &weapon_fire.wf_offsets)?;
    npz.add_array("wf_victim_idx", &weapon_fire.wf_victim_idx)?;
    npz.add_array("wf_damage", &weapon_fire.wf_damage)?;
    npz.add_array("wf_kill", &weapon_fire.wf_kill)?;

    // Write utility arrays
    npz.add_array("ut_throw_tick", &utility.ut_throw_tick)?;
    npz.add_array("ut_land_tick", &utility.ut_land_tick)?;
    npz.add_array("ut_thrower", &utility.ut_thrower)?;
    npz.add_array("ut_type", &utility.ut_type)?;
    npz.add_array("ut_id", &utility.ut_id)?;
    npz.add_array("ut_pos_land", &utility.ut_pos_land)?;

    // Write grenade trajectory CSR
    npz.add_array("gt_offsets", &trajectories.gt_offsets)?;
    npz.add_array("gt_thrower", &trajectories.gt_thrower)?;
    npz.add_array("gt_type", &trajectories.gt_type)?;
    npz.add_array("gt_id", &trajectories.gt_id)?;
    npz.add_array("gt_tick", &trajectories.gt_tick)?;
    npz.add_array("gt_pos", &trajectories.gt_pos)?;

    // Write metadata
    npz.add_array("meta", &meta_array)?;

    // Finish writing
    npz.finish()?;

    // NPZ write completed successfully (verbose logging removed for cleaner output)

    Ok(())
}
