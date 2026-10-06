//! Weapon fire detection and sequence analysis
//!
//! This module detects weapon fire sequences (bursts/sprays) from tick data
//! and matches them with kills from the collection metadata.

use super::data_types::{TickRecord, WeaponFireSequence};
use super::kill_collection_parser::Collection;
use super::weapon_mapper::get_correct_weapon_id;

/// Weapon IDs that are considered grenades (single-shot logic)
const GRENADE_WEAPON_IDS: &[&str] = &["44", "43", "45", "46", "48", "47"];

/// Detect weapon fire sequences for a specific player
pub fn detect_fire_sequences(
    tick_records: &[TickRecord],
    killer_steamid: u64,
    collection: &Collection,
) -> Vec<WeaponFireSequence> {
    let mut sequences = Vec::new();

    // Parse kill ticks from collection
    let kill_ticks = parse_kill_ticks(&collection.kill_ticks);
    let victim_indices = parse_victim_indices(&collection.victims_index);
    let collection_weapons_id = parse_weapons_id(&collection.weapons_id);

    // Group consecutive fire states
    let mut in_fire_sequence = false;
    let mut sequence_start = 0;
    let mut prev_ammo = 0;
    let mut prev_weapon = String::new();
    let mut bullets_used = 0;

    for (i, record) in tick_records.iter().enumerate() {
        // Skip if not the killer
        if record.steamid != killer_steamid {
            continue;
        }

        let current_fire = record.fire == 1;
        let weapon_changed = record.weapon != prev_weapon;

        // Handle weapon change - end current sequence if active
        if weapon_changed && in_fire_sequence {
            let sequence = create_fire_sequence(
                tick_records[sequence_start].tick,
                if i > 0 { tick_records[i-1].tick } else { tick_records[sequence_start].tick },
                &prev_weapon,
                &tick_records[sequence_start].weapon_id,
                bullets_used,
                &kill_ticks,
                &victim_indices,
                &collection_weapons_id,
            );
            sequences.push(sequence);
            in_fire_sequence = false;
            bullets_used = 0;
        }

        // Start of fire sequence
        if current_fire && !in_fire_sequence {
            in_fire_sequence = true;
            sequence_start = i;
            bullets_used = 0;
        }

        // Count bullets used (ammo decrease)
        if in_fire_sequence && !weapon_changed && record.ammo < prev_ammo {
            bullets_used += prev_ammo - record.ammo;
        }

        // End of fire sequence
        if !current_fire && in_fire_sequence {
            let sequence = create_fire_sequence(
                tick_records[sequence_start].tick,
                record.tick,
                &record.weapon,
                &tick_records[sequence_start].weapon_id,
                if bullets_used == 0 { 1 } else { bullets_used }, // At least 1 bullet
                &kill_ticks,
                &victim_indices,
                &collection_weapons_id,
            );
            sequences.push(sequence);
            in_fire_sequence = false;
            bullets_used = 0;
        }

        prev_ammo = record.ammo;
        prev_weapon = record.weapon.clone();
    }

    // Handle sequence that ends at the last tick
    if in_fire_sequence && !tick_records.is_empty() {
        let last_record = &tick_records[tick_records.len() - 1];
        let sequence = create_fire_sequence(
            tick_records[sequence_start].tick,
            last_record.tick,
            &last_record.weapon,
            &tick_records[sequence_start].weapon_id,
            if bullets_used == 0 { 1 } else { bullets_used },
            &kill_ticks,
            &victim_indices,
            &collection_weapons_id,
        );
        sequences.push(sequence);
    }

    sequences
}

/// Create a weapon fire sequence and match it with kills
fn create_fire_sequence(
    start_tick: i32,
    end_tick: i32,
    weapon: &str,
    weapon_id: &str,
    bullets_used: u32,
    kill_ticks: &[i32],
    victim_indices: &[u32],
    collection_weapons_id: &[String],
) -> WeaponFireSequence {
    let corrected_weapon_id = get_correct_weapon_id(weapon, weapon_id);

    let mut sequence = WeaponFireSequence {
        start_tick,
        end_tick,
        weapon: weapon.to_string(),
        weapon_id: corrected_weapon_id,
        bullets_used,
        kill: 0,
        victims: Vec::new(),
    };

    // Match kills to this fire sequence
    match_kills_to_sequence(&mut sequence, kill_ticks, victim_indices, collection_weapons_id);

    sequence
}

/// Match kills to a fire sequence based on timing and weapon type
fn match_kills_to_sequence(
    sequence: &mut WeaponFireSequence,
    kill_ticks: &[i32],
    victim_indices: &[u32],
    collection_weapons_id: &[String],
) {
    let is_grenade = GRENADE_WEAPON_IDS.contains(&sequence.weapon_id.as_str());
    let is_single_shot = sequence.start_tick == sequence.end_tick;

    for (i, &kill_tick) in kill_ticks.iter().enumerate() {
        // Get corresponding weapon ID and victim index if available
        let kill_weapon_id = collection_weapons_id.get(i).map(|s| s.as_str()).unwrap_or("");
        let victim_index = victim_indices.get(i).copied().unwrap_or(0);

        // Check if weapon IDs match or fallback to weapon names
        let weapons_match = sequence.weapon_id == kill_weapon_id ||
                            sequence.weapon.to_lowercase() == collection_weapons_id.get(i).map(|s| s.to_lowercase()).unwrap_or_default();

        if weapons_match {
            let kill_matches = if is_grenade {
                // Grenade kills happen after the throw
                kill_tick > sequence.end_tick
            } else if is_single_shot {
                // Single shot kills are very close to the fire tick
                (kill_tick - sequence.start_tick).abs() <= 5
            } else {
                // Automatic weapon kills are within the spray duration
                kill_tick >= sequence.start_tick - 1 && kill_tick <= sequence.end_tick
            };

            if kill_matches {
                if !sequence.victims.contains(&victim_index) {
                    sequence.victims.push(victim_index);
                }
                sequence.kill = 1;
            }
        }
    }
}

/// Parse kill ticks from the collection string format: [tick1;tick2;tick3]
fn parse_kill_ticks(kill_ticks_str: &str) -> Vec<i32> {
    let trimmed = kill_ticks_str.trim();

    // Remove brackets if present
    let content = if trimmed.starts_with('[') && trimmed.ends_with(']') {
        &trimmed[1..trimmed.len()-1]
    } else {
        trimmed
    };

    if content.trim().is_empty() {
        return Vec::new();
    }

    content.split(';')
        .filter_map(|s| s.trim().parse::<i32>().ok())
        .collect()
}

/// Parse victim indices from the collection string format: [index1;index2;index3]
fn parse_victim_indices(victims_str: &str) -> Vec<u32> {
    let trimmed = victims_str.trim();

    // Remove brackets if present
    let content = if trimmed.starts_with('[') && trimmed.ends_with(']') {
        &trimmed[1..trimmed.len()-1]
    } else {
        trimmed
    };

    if content.trim().is_empty() {
        return Vec::new();
    }

    content.split(';')
        .filter_map(|s| s.trim().parse::<u32>().ok())
        .collect()
}

/// Parse weapon IDs from the collection string format: [id1;id2;id3]
fn parse_weapons_id(weapons_id_str: &str) -> Vec<String> {
    let trimmed = weapons_id_str.trim();

    // Remove brackets if present
    let content = if trimmed.starts_with('[') && trimmed.ends_with(']') {
        &trimmed[1..trimmed.len()-1]
    } else {
        trimmed
    };

    if content.trim().is_empty() {
        return Vec::new();
    }

    content.split(';')
        .map(|s| s.trim().to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_kill_ticks() {
        assert_eq!(parse_kill_ticks("[147591;148267;148965]"), vec![147591, 148267, 148965]);
        assert_eq!(parse_kill_ticks("[]"), Vec::<i32>::new());
        assert_eq!(parse_kill_ticks("147591;148267"), vec![147591, 148267]);
    }

    #[test]
    fn test_parse_victim_indices() {
        assert_eq!(parse_victim_indices("[13;12;7]"), vec![13, 12, 7]);
        assert_eq!(parse_victim_indices("[]"), Vec::<u32>::new());
    }

    #[test]
    fn test_parse_weapons_id() {
        assert_eq!(parse_weapons_id("[9;9;7]"), vec!["9", "9", "7"]);
        assert_eq!(parse_weapons_id("[]"), Vec::<String>::new());
    }
}
