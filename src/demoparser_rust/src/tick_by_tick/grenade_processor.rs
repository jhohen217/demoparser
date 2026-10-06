//! Grenade and weapon fire processing for CS2 demo parsing
//!
//! This module handles the processing of weapon fire events, utility thrown data,
//! and grenade trajectories from parsed demo data.

use super::data_extraction::{
    get_f32_value as get_f32, get_i32_value as get_i32, get_string_value as get_str,
};
use ahash::AHashMap;
use anyhow::Result;
use parser::first_pass::prop_controller::*;
use parser::parse_demo::DemoOutput;
use parser::second_pass::variants::{PropColumn, VarVec};

/// Represents a weapon fire event with hit detection
/// Supports multiple victims per event (e.g., grenade explosions)
#[derive(Debug, Clone)]
pub struct WeaponFireEvent {
    pub tick: i32,
    pub weapon: String,
    pub attacker_steamid: u64,
    pub hit_players: Vec<String>,
    pub victim_steamids: Vec<u64>,
    pub damages: Vec<i32>,
    pub kill_flags: Vec<bool>,
    pub impact_tick: Option<i32>,
}

/// Represents a utility grenade that was thrown
#[derive(Debug, Clone)]
pub struct UtilityThrown {
    pub tick_throw: i32,
    pub tick_land: i32,
    pub weapon: String,
    pub entity_id: i32,
    pub util_pos_x: f32,
    pub util_pos_y: f32,
    pub util_pos_z: f32,
    pub thrower_steamid: u64,
}

/// Represents a complete grenade trajectory
#[derive(Debug, Clone)]
pub struct GrenadeTrajectory {
    pub steamid: u64,
    pub grenade_type: String,
    pub entity_id: i32,
    pub trajectory_points: Vec<GrenadeTrajectoryPoint>,
}

/// A single point in a grenade's trajectory
#[derive(Debug, Clone)]
pub struct GrenadeTrajectoryPoint {
    pub tick: i32,
    pub pos_x: f32,
    pub pos_y: f32,
    pub pos_z: f32,
}

/// Compiled statistics from weapon fire and grenade usage
#[derive(Debug, Clone)]
pub struct GrenadeStats {
    pub hits: u32,
    pub misses: u32,
    pub hit_rate: f32,
    pub util_thrown: Vec<String>,
    pub util_thrown_ticks: Vec<i32>,
    pub util_land_ticks: Vec<i32>,
    pub weapons_damaged: Vec<String>,
    pub weapons_damaged_num_hits: Vec<u32>,
}

impl Default for GrenadeStats {
    fn default() -> Self {
        Self {
            hits: 0,
            misses: 0,
            hit_rate: 0.0,
            util_thrown: Vec::new(),
            util_thrown_ticks: Vec::new(),
            util_land_ticks: Vec::new(),
            weapons_damaged: Vec::new(),
            weapons_damaged_num_hits: Vec::new(),
        }
    }
}

/// Process weapon fire events with hit detection and kill correlation
/// Supports multiple victims per event (e.g., grenade explosions)
pub fn process_weapon_fire_events(
    output: &DemoOutput,
    target_steamid: u64,
    tick_start: u32,
    tick_end: u32,
) -> Result<Vec<WeaponFireEvent>> {
    let mut weapon_fire_events = Vec::new();

    // Collect player_hurt events indexed by tick for efficient lookup
    let mut player_hurt_by_tick: AHashMap<i32, Vec<(usize, String, u64, i32)>> =
        AHashMap::default();
    for event in &output.game_events {
        if event.name == "player_hurt" {
            let tick = event.tick;

            let attacker_steamid = event
                .fields
                .iter()
                .find(|f| f.name == "attacker_steamid")
                .and_then(|f| f.data.as_ref())
                .and_then(|d| {
                    if let parser::second_pass::variants::Variant::String(s) = d {
                        s.parse::<u64>().ok()
                    } else {
                        None
                    }
                })
                .unwrap_or(0);

            if attacker_steamid == target_steamid {
                let victim_name = event
                    .fields
                    .iter()
                    .find(|f| f.name == "user_name")
                    .and_then(|f| f.data.as_ref())
                    .and_then(|d| {
                        if let parser::second_pass::variants::Variant::String(s) = d {
                            Some(s.clone())
                        } else {
                            None
                        }
                    })
                    .unwrap_or_else(|| "Unknown".to_string());

                let victim_steamid = event
                    .fields
                    .iter()
                    .find(|f| f.name == "user_steamid")
                    .and_then(|f| f.data.as_ref())
                    .and_then(|d| {
                        if let parser::second_pass::variants::Variant::String(s) = d {
                            s.parse::<u64>().ok()
                        } else {
                            None
                        }
                    })
                    .unwrap_or(0);

                let damage = event
                    .fields
                    .iter()
                    .find(|f| f.name == "dmg_health")
                    .and_then(|f| f.data.as_ref())
                    .and_then(|d| match d {
                        parser::second_pass::variants::Variant::I32(i) => Some(*i),
                        parser::second_pass::variants::Variant::U32(u) => Some(*u as i32),
                        _ => None,
                    })
                    .unwrap_or(0);

                let entries = player_hurt_by_tick.entry(tick).or_insert_with(Vec::new);
                let index = entries.len();
                entries.push((index, victim_name, victim_steamid, damage));
            }
        }
    }

    // Collect player_death events indexed by tick for efficient lookup
    let mut player_death_by_tick: AHashMap<i32, Vec<u64>> = AHashMap::default();
    for event in &output.game_events {
        if event.name == "player_death" {
            let tick = event.tick;

            let attacker_steamid = event
                .fields
                .iter()
                .find(|f| f.name == "attacker_steamid")
                .and_then(|f| f.data.as_ref())
                .and_then(|d| {
                    if let parser::second_pass::variants::Variant::String(s) = d {
                        s.parse::<u64>().ok()
                    } else {
                        None
                    }
                })
                .unwrap_or(0);

            if attacker_steamid == target_steamid {
                let victim_steamid = event
                    .fields
                    .iter()
                    .find(|f| f.name == "user_steamid")
                    .and_then(|f| f.data.as_ref())
                    .and_then(|d| {
                        if let parser::second_pass::variants::Variant::String(s) = d {
                            s.parse::<u64>().ok()
                        } else {
                            None
                        }
                    })
                    .unwrap_or(0);

                player_death_by_tick
                    .entry(tick)
                    .or_insert_with(Vec::new)
                    .push(victim_steamid);
            }
        }
    }

    // Track used hurt events to prevent double-counting
    use std::collections::HashSet;
    let mut used_hurt_events: HashSet<(i32, usize)> = HashSet::new();

    // Process weapon_fire events
    for event in &output.game_events {
        if event.name != "weapon_fire" {
            continue;
        }

        let tick = event.tick;

        // Filter by tick range
        if tick < tick_start as i32 || tick > tick_end as i32 {
            continue;
        }

        // Extract weapon name
        let weapon = event
            .fields
            .iter()
            .find(|f| f.name == "weapon")
            .and_then(|f| f.data.as_ref())
            .and_then(|d| {
                if let parser::second_pass::variants::Variant::String(s) = d {
                    Some(s.clone())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| "unknown".to_string());

        // Extract steamid
        let user_steamid = event
            .fields
            .iter()
            .find(|f| f.name == "user_steamid")
            .and_then(|f| f.data.as_ref())
            .and_then(|d| {
                if let parser::second_pass::variants::Variant::String(s) = d {
                    s.parse::<u64>().ok()
                } else {
                    None
                }
            })
            .unwrap_or(0);

        // Only include events from the target player
        if user_steamid != target_steamid {
            continue;
        }

        // Determine search window based on weapon type
        // Grenades have delay between throw and explosion (up to ~200 ticks)
        // Bullets/other weapons have near-instant impact (2-3 ticks)
        let is_grenade = weapon.contains("grenade")
            || weapon.contains("molotov")
            || weapon.contains("incgrenade")
            || weapon.contains("flashbang")
            || weapon.contains("decoy")
            || weapon.contains("smokegrenade");
        let max_offset = if is_grenade { 200 } else { 3 };

        // Collect ALL victims within the tick window
        #[derive(Debug)]
        struct VictimInfo {
            hit_player: String,
            victim_steamid: u64,
            damage: i32,
            is_kill: bool,
            check_tick: i32,
            index: usize,
        }
        let mut victims: Vec<VictimInfo> = Vec::new();

        for offset in 0..max_offset {
            let check_tick = tick + offset;

            // Look for hurt events at this tick
            if let Some(hurt_events) = player_hurt_by_tick.get(&check_tick) {
                for &(index, ref victim_name, victim_sid, damage) in hurt_events {
                    // Check if this hurt event has already been used
                    if used_hurt_events.contains(&(check_tick, index)) {
                        continue;
                    }

                    // Check if this hurt resulted in a kill
                    let is_kill = player_death_by_tick
                        .get(&check_tick)
                        .map(|deaths| deaths.contains(&victim_sid))
                        .unwrap_or(false);

                    victims.push(VictimInfo {
                        hit_player: victim_name.clone(),
                        victim_steamid: victim_sid,
                        damage,
                        is_kill,
                        check_tick,
                        index,
                    });
                }
            }
        }

        // Create weapon fire event
        if !victims.is_empty() {
            // Sort victims: kills first, then by damage (highest first)
            victims.sort_by(|a, b| match (b.is_kill, a.is_kill) {
                (true, false) => std::cmp::Ordering::Greater,
                (false, true) => std::cmp::Ordering::Less,
                _ => b.damage.cmp(&a.damage),
            });

            // Mark all hurt events as used
            for victim in &victims {
                used_hurt_events.insert((victim.check_tick, victim.index));
            }

            // Impact tick is the actual tick where damage occurred
            let impact_tick = Some(victims[0].check_tick);

            weapon_fire_events.push(WeaponFireEvent {
                tick,
                weapon,
                attacker_steamid: user_steamid,
                hit_players: victims.iter().map(|v| v.hit_player.clone()).collect(),
                victim_steamids: victims.iter().map(|v| v.victim_steamid).collect(),
                damages: victims.iter().map(|v| v.damage).collect(),
                kill_flags: victims.iter().map(|v| v.is_kill).collect(),
                impact_tick,
            });
        } else {
            // No hits - create a MISS entry
            weapon_fire_events.push(WeaponFireEvent {
                tick,
                weapon,
                attacker_steamid: user_steamid,
                hit_players: Vec::new(),
                victim_steamids: Vec::new(),
                damages: Vec::new(),
                kill_flags: Vec::new(),
                impact_tick: None,
            });
        }
    }

    Ok(weapon_fire_events)
}

/// Process utility grenades thrown by a specific player
pub fn process_utility_thrown(
    output: &DemoOutput,
    target_steamid: u64,
    tick_start: u32,
    tick_end: u32,
) -> Result<Vec<UtilityThrown>> {
    let mut utilities = Vec::new();

    // Extract projectile data from the demo output
    let data_len = if let Some(tick_data) = output.df.get(&TICK_ID) {
        tick_data.len()
    } else {
        return Ok(utilities);
    };

    // Collect all projectile data points
    let mut projectile_data: Vec<(i32, i32, String, u64, f32, f32, f32)> = Vec::new();

    for i in 0..data_len {
        if let Some(grenade_type) = get_string_value(&output.df, &GRENADE_TYPE_ID, i) {
            if !grenade_type.contains("Projectile") {
                continue;
            }

            if let Some(entity_id) = get_i32_value(&output.df, &ENTITY_ID_ID, i) {
                if let Some(tick) = get_i32_value(&output.df, &TICK_ID, i) {
                    if tick >= tick_start as i32 && tick <= tick_end as i32 {
                        let steamid = get_u64_value(&output.df, &STEAMID_ID, i).unwrap_or(0);

                        // Filter by target steamid
                        if steamid != target_steamid {
                            continue;
                        }

                        let x = get_f32_value(&output.df, &GRENADE_X, i).unwrap_or(0.0);
                        let y = get_f32_value(&output.df, &GRENADE_Y, i).unwrap_or(0.0);
                        let z = get_f32_value(&output.df, &GRENADE_Z, i).unwrap_or(0.0);

                        projectile_data.push((
                            tick,
                            entity_id,
                            grenade_type.clone(),
                            steamid,
                            x,
                            y,
                            z,
                        ));
                    }
                }
            }
        }
    }

    // Group by entity_id to track individual grenades
    let mut projectiles_by_entity: AHashMap<i32, Vec<(i32, String, u64, f32, f32, f32)>> =
        AHashMap::default();
    for (tick, entity_id, grenade_type, steamid, x, y, z) in projectile_data {
        projectiles_by_entity
            .entry(entity_id)
            .or_insert_with(Vec::new)
            .push((tick, grenade_type, steamid, x, y, z));
    }

    // Process each grenade to determine throw and land ticks
    for (entity_id, trajectory) in projectiles_by_entity.iter() {
        if let (Some(first), Some(last)) = (trajectory.first(), trajectory.last()) {
            let tick_throw = first.0;
            let tick_land = last.0;
            let weapon = first.1.clone();
            let thrower_steamid = first.2;
            let util_pos_x = last.3;
            let util_pos_y = last.4;
            let util_pos_z = last.5;

            utilities.push(UtilityThrown {
                tick_throw,
                tick_land,
                weapon,
                entity_id: *entity_id,
                util_pos_x,
                util_pos_y,
                util_pos_z,
                thrower_steamid,
            });
        }
    }

    // Sort utilities by tick_throw in ascending order
    utilities.sort_by_key(|u| u.tick_throw);

    Ok(utilities)
}

/// Process grenade trajectories for specified players
pub fn process_grenade_trajectories(
    output: &DemoOutput,
    filter_steamids: &[u64],
    tick_start: u32,
    tick_end: u32,
) -> Result<Vec<GrenadeTrajectory>> {
    if output.utility.captured {
        return Ok(super::utility::trajectories(&output.utility, filter_steamids, tick_start, tick_end));
    }
    let mut trajectories = Vec::new();

    // Extract projectile data from the demo output
    let data_len = if let Some(tick_data) = output.df.get(&TICK_ID) {
        tick_data.len()
    } else {
        return Ok(trajectories);
    };

    // Collect all projectile data points
    let mut projectile_data: Vec<(i32, i32, String, u64, f32, f32, f32)> = Vec::new();

    for i in 0..data_len {
        if let Some(grenade_type) = get_string_value(&output.df, &GRENADE_TYPE_ID, i) {
            if !grenade_type.contains("Projectile") {
                continue;
            }

            if let Some(entity_id) = get_i32_value(&output.df, &ENTITY_ID_ID, i) {
                if let Some(tick) = get_i32_value(&output.df, &TICK_ID, i) {
                    if tick >= tick_start as i32 && tick <= tick_end as i32 {
                        let steamid = get_u64_value(&output.df, &STEAMID_ID, i).unwrap_or(0);

                        // Filter by steamids
                        if !filter_steamids.contains(&steamid) {
                            continue;
                        }

                        let x = get_f32_value(&output.df, &GRENADE_X, i).unwrap_or(0.0);
                        let y = get_f32_value(&output.df, &GRENADE_Y, i).unwrap_or(0.0);
                        let z = get_f32_value(&output.df, &GRENADE_Z, i).unwrap_or(0.0);

                        projectile_data.push((
                            tick,
                            entity_id,
                            grenade_type.clone(),
                            steamid,
                            x,
                            y,
                            z,
                        ));
                    }
                }
            }
        }
    }

    // Group by entity_id to track individual grenade trajectories
    let mut projectiles_by_entity: AHashMap<i32, Vec<(i32, String, u64, f32, f32, f32)>> =
        AHashMap::default();
    for (tick, entity_id, grenade_type, steamid, x, y, z) in projectile_data {
        projectiles_by_entity
            .entry(entity_id)
            .or_insert_with(Vec::new)
            .push((tick, grenade_type, steamid, x, y, z));
    }

    // Build trajectory structures with resting position pruning
    for (entity_id, trajectory_data) in projectiles_by_entity {
        if let Some(first) = trajectory_data.first() {
            let steamid = first.2;
            let grenade_type = first.1.clone();

            // Convert to trajectory points and prune resting positions
            let mut trajectory_points: Vec<GrenadeTrajectoryPoint> = trajectory_data
                .iter()
                .map(|(tick, _, _, x, y, z)| GrenadeTrajectoryPoint {
                    tick: *tick,
                    pos_x: *x,
                    pos_y: *y,
                    pos_z: *z,
                })
                .collect();
            trajectory_points.sort_by_key(|point| point.tick);

            // Prune duplicate resting positions (when grenade has settled)
            prune_resting_positions(&mut trajectory_points);

            // Only add trajectory if it has meaningful data
            if !trajectory_points.is_empty() {
                trajectories.push(GrenadeTrajectory {
                    steamid,
                    grenade_type,
                    entity_id,
                    trajectory_points,
                });
            }
        }
    }

    trajectories.sort_by(|left, right| {
        left.trajectory_points
            .first()
            .map(|point| point.tick)
            .cmp(&right.trajectory_points.first().map(|point| point.tick))
            .then_with(|| left.entity_id.cmp(&right.entity_id))
            .then_with(|| left.steamid.cmp(&right.steamid))
            .then_with(|| left.grenade_type.cmp(&right.grenade_type))
    });

    Ok(trajectories)
}

/// Calculate statistics from weapon fire events and utility usage
pub fn calculate_grenade_stats(
    weapon_fire_events: &[WeaponFireEvent],
    utility_thrown: &[UtilityThrown],
) -> GrenadeStats {
    let mut stats = GrenadeStats::default();

    // Calculate hits and misses
    for event in weapon_fire_events {
        if !event.hit_players.is_empty() {
            stats.hits += 1;
        } else {
            stats.misses += 1;
        }
    }

    // Calculate hit rate (as decimal fraction 0.0-1.0, not percentage)
    let total_shots = stats.hits + stats.misses;
    if total_shots > 0 {
        stats.hit_rate = stats.hits as f32 / total_shots as f32;
    }

    // Collect utility information with clean names
    for util in utility_thrown {
        let clean_util = clean_projectile_name(&util.weapon);
        stats.util_thrown.push(clean_util);
        stats.util_thrown_ticks.push(util.tick_throw);
        stats.util_land_ticks.push(util.tick_land);
    }

    // Calculate weapon damage statistics with clean names
    let mut weapon_hits: AHashMap<String, u32> = AHashMap::default();
    for event in weapon_fire_events {
        if !event.hit_players.is_empty() {
            let clean_weapon = clean_weapon_name(&event.weapon);
            *weapon_hits.entry(clean_weapon).or_insert(0) += 1;
        }
    }

    let mut weapon_hits: Vec<_> = weapon_hits.into_iter().collect();
    weapon_hits.sort_by(|left, right| left.0.cmp(&right.0));
    for (weapon, hits) in weapon_hits {
        stats.weapons_damaged.push(weapon);
        stats.weapons_damaged_num_hits.push(hits);
    }

    stats
}

/// Helper function to get string value from dataframe (uses existing data_extraction function)
fn get_string_value(df: &AHashMap<u32, PropColumn>, prop_id: &u32, index: usize) -> Option<String> {
    get_str(df, prop_id, index)
}

/// Helper function to get i32 value from dataframe (uses existing data_extraction function)
fn get_i32_value(df: &AHashMap<u32, PropColumn>, prop_id: &u32, index: usize) -> Option<i32> {
    get_i32(df, prop_id, index)
}

/// Helper function to get u64 value from dataframe
fn get_u64_value(df: &AHashMap<u32, PropColumn>, prop_id: &u32, index: usize) -> Option<u64> {
    if let Some(column) = df.get(prop_id) {
        if let Some(VarVec::U64(values)) = &column.data {
            if let Some(Some(value)) = values.get(index) {
                return Some(*value);
            }
        }
    }
    None
}

/// Helper function to get f32 value from dataframe (uses existing data_extraction function)
fn get_f32_value(df: &AHashMap<u32, PropColumn>, prop_id: &u32, index: usize) -> Option<f32> {
    get_f32(df, prop_id, index)
}

/// Clean weapon name by removing common prefixes
///
/// Strips "weapon_" prefix to get clean weapon names (e.g., "weapon_ak47" -> "ak47")
pub fn clean_weapon_name(weapon: &str) -> String {
    weapon.strip_prefix("weapon_").unwrap_or(weapon).to_string()
}

/// Clean projectile type name to canonical grenade name
///
/// Maps CS2 projectile class names to canonical weapon names from weapon_mapper.rs:
/// - CHEGrenadeProjectile -> hegrenade
/// - CFlashbangProjectile -> flashbang
/// - CSmokeGrenadeProjectile -> smokegrenade
/// - CMolotovProjectile -> molotov
/// - CIncendiaryGrenade -> incgrenade
/// - CDecoyProjectile -> decoy
pub fn clean_projectile_name(projectile_type: &str) -> String {
    match projectile_type {
        "CHEGrenadeProjectile" => "hegrenade".to_string(),
        "CFlashbangProjectile" => "flashbang".to_string(),
        "CSmokeGrenadeProjectile" => "smokegrenade".to_string(),
        "CMolotovProjectile" => "molotov".to_string(),
        "CIncendiaryGrenade" => "incgrenade".to_string(),
        "CDecoyProjectile" => "decoy".to_string(),
        _ => projectile_type.to_string(), // Fallback to original if unknown
    }
}

/// Prune duplicate resting positions from grenade trajectory
///
/// When a grenade comes to rest, it reports the same position for many ticks.
/// This function detects when positions stop changing and keeps only the first
/// resting position, removing all duplicate entries.
fn prune_resting_positions(trajectory: &mut Vec<GrenadeTrajectoryPoint>) {
    if trajectory.len() < 3 {
        return; // Need at least 3 points to detect resting
    }

    // Find where the grenade stops moving (position becomes constant)
    let mut last_different_idx = trajectory.len() - 1;

    for i in (1..trajectory.len()).rev() {
        let current = &trajectory[i];
        let previous = &trajectory[i - 1];

        // Check if position is identical (grenade at rest)
        let position_same = (current.pos_x - previous.pos_x).abs() < 0.01
            && (current.pos_y - previous.pos_y).abs() < 0.01
            && (current.pos_z - previous.pos_z).abs() < 0.01;

        if !position_same {
            // Found last point where grenade was still moving
            last_different_idx = i;
            break;
        }
    }

    // Keep trajectory up to and including the first resting point
    // (last_different_idx + 1 gives us one resting position for reference)
    if last_different_idx + 1 < trajectory.len() {
        trajectory.truncate(last_different_idx + 2);
    }
}
