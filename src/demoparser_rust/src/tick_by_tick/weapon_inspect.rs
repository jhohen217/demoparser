//! Weapon inspect sequence detection
//!
//! This module provides functionality to detect weapon inspection sequences
//! from tick-by-tick player data, similar to the Python implementation.

use super::data_types::TickRecord;
use serde::Serialize;

/// Represents a weapon inspect sequence start
#[derive(Debug, Clone, Serialize)]
pub struct WeaponInspectSequence {
    pub tick: i32,
    pub weapon: String,
    pub weapon_id: String,
}

/// Detects weapon inspect sequence start points for a specific player
///
/// This function analyzes tick records to find transitions from not inspecting (0)
/// to inspecting (1), marking the start of weapon inspection sequences.
///
/// Only inspect sequences that last at least 24 ticks without a weapon change
/// or FIRE event are included.
///
/// # Arguments
/// * `tick_records` - Vector of tick records for the player, sorted by tick
///
/// # Returns
/// * Vector of WeaponInspectSequence representing each inspect sequence start
pub fn detect_weapon_inspect_sequences(tick_records: &[TickRecord]) -> Vec<WeaponInspectSequence> {
    let mut inspect_sequences = Vec::new();

    if tick_records.is_empty() {
        return inspect_sequences;
    }

    let mut was_inspecting_prev_tick = false;

    for (i, record) in tick_records.iter().enumerate() {
        let is_inspecting_this_tick = record.inspecting == 1;

        // Detect transition from not inspecting (0) to inspecting (1)
        if is_inspecting_this_tick && !was_inspecting_prev_tick {
            // Validate the inspect sequence by checking the next 24 ticks
            let is_valid = validate_inspect_sequence(tick_records, i);

            if is_valid {
                // Start of a valid inspection sequence
                let inspect_start = WeaponInspectSequence {
                    tick: record.tick,
                    weapon: record.weapon.clone(),
                    weapon_id: record.weapon_id.clone(),
                };

                inspect_sequences.push(inspect_start);
            }
        }

        // Update state for the next iteration
        was_inspecting_prev_tick = is_inspecting_this_tick;
    }

    inspect_sequences
}

/// Validates an inspect sequence by checking the next 24 ticks
///
/// An inspect sequence is valid if:
/// - The weapon_id remains the same for at least 24 ticks
/// - No FIRE event (fire == 1) occurs in the next 24 ticks
///
/// # Arguments
/// * `tick_records` - All tick records for the player
/// * `start_index` - Index of the inspect start in tick_records
///
/// # Returns
/// * true if the inspect sequence is valid, false otherwise
fn validate_inspect_sequence(tick_records: &[TickRecord], start_index: usize) -> bool {
    // Need at least 24 more records to validate
    if start_index + 24 > tick_records.len() {
        return false;
    }

    let start_weapon_id = &tick_records[start_index].weapon_id;

    // Check the next 24 ticks (including the current one)
    for i in start_index..start_index + 24 {
        let record = &tick_records[i];

        // Check if weapon changed
        if record.weapon_id != *start_weapon_id {
            return false;
        }

        // Check if FIRE event occurred
        if record.fire == 1 {
            return false;
        }
    }

    true
}

/// Convert weapon inspect sequences to a format suitable for CSV metadata
///
/// This function formats the inspect sequences as a semicolon-separated string
/// containing tick numbers, similar to other sequence data in the CSV.
///
/// # Arguments
/// * `sequences` - Vector of weapon inspect sequences
///
/*
/// # Returns
/// * String formatted as "tick1;tick2;tick3" or empty string if no sequences
pub fn format_inspect_sequences_for_csv(sequences: &[WeaponInspectSequence]) -> String {
    if sequences.is_empty() {
        String::new()
    } else {
        sequences
            .iter()
            .map(|seq| seq.tick.to_string())
            .collect::<Vec<_>>()
            .join(";")
    }
}

/// Convert weapon inspect sequences to a detailed format for debugging
///
/// This function creates a detailed string representation of inspect sequences
/// for logging and debugging purposes.
///
/// # Arguments
/// * `sequences` - Vector of weapon inspect sequences
///
/// # Returns
/// * String with detailed sequence information
pub fn format_inspect_sequences_detailed(sequences: &[WeaponInspectSequence]) -> String {
    if sequences.is_empty() {
        "No weapon inspect sequences detected".to_string()
    } else {
        sequences
            .iter()
            .map(|seq| format!("Tick {}: {} (ID: {})", seq.tick, seq.weapon, seq.weapon_id))
            .collect::<Vec<_>>()
            .join(", ")
    }
}
*/

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_record(
        tick: i32,
        inspecting: u32,
        weapon: &str,
        weapon_id: &str,
        fire: u32,
    ) -> TickRecord {
        TickRecord {
            tick,
            inspecting,
            weapon: weapon.to_string(),
            weapon_id: weapon_id.to_string(),
            fire,
            ..Default::default()
        }
    }

    #[test]
    fn test_valid_inspect_sequence_24_ticks() {
        let mut records = vec![
            create_test_record(100, 0, "AK47", "7", 0),
            create_test_record(101, 1, "AK47", "7", 0), // Start of inspect
        ];
        // Add 23 more ticks of inspecting with same weapon, no fire
        for i in 102..125 {
            records.push(create_test_record(i, 1, "AK47", "7", 0));
        }

        let sequences = detect_weapon_inspect_sequences(&records);

        assert_eq!(sequences.len(), 1);
        assert_eq!(sequences[0].tick, 101);
        assert_eq!(sequences[0].weapon, "AK47");
        assert_eq!(sequences[0].weapon_id, "7");
    }

    #[test]
    fn test_invalid_inspect_weapon_change() {
        let mut records = vec![
            create_test_record(100, 0, "AK47", "7", 0),
            create_test_record(101, 1, "AK47", "7", 0), // Start of inspect
        ];
        // Add 10 more ticks, then weapon changes
        for i in 102..112 {
            records.push(create_test_record(i, 1, "AK47", "7", 0));
        }
        // Weapon change at tick 112
        records.push(create_test_record(112, 1, "knife", "42", 0));
        // Add more ticks to reach 24
        for i in 113..125 {
            records.push(create_test_record(i, 0, "knife", "42", 0));
        }

        let sequences = detect_weapon_inspect_sequences(&records);

        // Should be 0 because weapon changed within 24 ticks
        assert_eq!(sequences.len(), 0);
    }

    #[test]
    fn test_invalid_inspect_fire_event() {
        let mut records = vec![
            create_test_record(100, 0, "AK47", "7", 0),
            create_test_record(101, 1, "AK47", "7", 0), // Start of inspect
        ];
        // Add 10 more ticks
        for i in 102..112 {
            records.push(create_test_record(i, 1, "AK47", "7", 0));
        }
        // Fire event at tick 112
        records.push(create_test_record(112, 1, "AK47", "7", 1));
        // Add more ticks to reach 24
        for i in 113..125 {
            records.push(create_test_record(i, 0, "AK47", "7", 0));
        }

        let sequences = detect_weapon_inspect_sequences(&records);

        // Should be 0 because FIRE occurred within 24 ticks
        assert_eq!(sequences.len(), 0);
    }

    #[test]
    fn test_invalid_inspect_not_enough_ticks() {
        let records = vec![
            create_test_record(100, 0, "AK47", "7", 0),
            create_test_record(101, 1, "AK47", "7", 0), // Start of inspect
            create_test_record(102, 1, "AK47", "7", 0),
            create_test_record(103, 0, "AK47", "7", 0),
        ];

        let sequences = detect_weapon_inspect_sequences(&records);

        // Should be 0 because not enough ticks to validate
        assert_eq!(sequences.len(), 0);
    }

    #[test]
    fn test_no_inspect_sequences() {
        let records = vec![
            create_test_record(100, 0, "AK47", "7", 0),
            create_test_record(101, 0, "AK47", "7", 0),
            create_test_record(102, 0, "AK47", "7", 0),
        ];

        let sequences = detect_weapon_inspect_sequences(&records);
        assert_eq!(sequences.len(), 0);
    }

    #[test]
    fn test_multiple_valid_inspect_sequences() {
        let mut records = vec![
            create_test_record(100, 0, "AK47", "7", 0),
            create_test_record(101, 1, "AK47", "7", 0), // First inspect start
        ];
        // Add 23 more ticks for first inspect
        for i in 102..125 {
            records.push(create_test_record(i, 1, "AK47", "7", 0));
        }
        // Gap
        for i in 125..130 {
            records.push(create_test_record(i, 0, "AK47", "7", 0));
        }
        // Second inspect starts
        records.push(create_test_record(130, 1, "M4A4", "16", 0));
        // Add 23 more ticks for second inspect
        for i in 131..154 {
            records.push(create_test_record(i, 1, "M4A4", "16", 0));
        }

        let sequences = detect_weapon_inspect_sequences(&records);

        assert_eq!(sequences.len(), 2);
        assert_eq!(sequences[0].tick, 101);
        assert_eq!(sequences[0].weapon, "AK47");
        assert_eq!(sequences[1].tick, 130);
        assert_eq!(sequences[1].weapon, "M4A4");
    }
}
