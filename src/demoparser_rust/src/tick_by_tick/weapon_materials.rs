//! S2EX tag 11, schema 1: network game clock and per-lifetime weapon shake state.
//! Header: clock_count:u32, state_count:u32; clock rows (tick:i32,time:f32),
//! state rows (tick:i32,lifetime:u32,last_shake:f32). NaN means unknown state.
//! The directory count is the sum of both row counts. No frame ABI changes.
use super::data_types::TickRecord;
use anyhow::{bail, Result};
use parser::second_pass::parser_settings::WeaponEntitySnapshot;
use std::collections::BTreeMap;

pub fn encode<'a>(
    players: impl Iterator<Item = &'a TickRecord>,
    weapons: impl Iterator<Item = (u32, &'a WeaponEntitySnapshot)>,
    min_tick: i32,
    max_tick: i32,
) -> Result<(Vec<u8>, u32)> {
    let mut clocks = BTreeMap::new();
    for row in players.filter(|row| row.tick >= min_tick && row.tick <= max_tick) {
        if let Some(time) = row.state.game_time.filter(|value| value.is_finite()) {
            if let Some(previous) = clocks.insert(row.tick, time) {
                if previous != time {
                    bail!("Conflicting network game clocks at demo tick {}", row.tick);
                }
            }
        }
    }
    let mut states = BTreeMap::new();
    for (id, row) in weapons.filter(|(_, row)| row.tick >= min_tick && row.tick <= max_tick) {
        let value = row.last_shake_time.filter(|v| v.is_finite());
        // Lifetimes arrive in source order, so the last update at a tick is authoritative.
        states.insert((id, row.tick), if row.present { value } else { None });
    }
    let mut changes = Vec::new();
    let mut previous = None;
    for ((id, tick), value) in states {
        if previous != Some((id, value)) {
            changes.push((tick, id, value));
            previous = Some((id, value));
        }
    }
    changes.sort_by_key(|&(tick, id, _)| (tick, id));
    let clock_count = u32::try_from(clocks.len())?;
    let state_count = u32::try_from(changes.len())?;
    let mut output = Vec::new();
    output.extend_from_slice(&clock_count.to_le_bytes());
    output.extend_from_slice(&state_count.to_le_bytes());
    for (tick, time) in clocks {
        output.extend_from_slice(&tick.to_le_bytes());
        output.extend_from_slice(&time.to_le_bytes());
    }
    for (tick, id, value) in changes {
        output.extend_from_slice(&tick.to_le_bytes());
        output.extend_from_slice(&id.to_le_bytes());
        output.extend_from_slice(&value.unwrap_or(f32::NAN).to_le_bytes());
    }
    Ok((output, clock_count.checked_add(state_count).ok_or_else(|| anyhow::anyhow!("Material row count overflow"))?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clocks_preserve_game_origin_pause_and_absence() {
        let mut rows = vec![TickRecord::default(); 5];
        for (index, row) in rows.iter_mut().enumerate() { row.tick = 10 + index as i32; }
        rows[0].state.game_time = Some(600.0);
        rows[1].state.game_time = Some(600.0); // paused game clock
        rows[3].state.game_time = Some(600.03125);
        rows[4].state.game_time = Some(f32::INFINITY);
        let (bytes, count) = encode(rows.iter(), std::iter::empty(), 10, 14).unwrap();
        assert_eq!(count, 3);
        assert_eq!(bytes.len(), 8 + 3 * 8);
        assert_eq!(f32::from_le_bytes(bytes[12..16].try_into().unwrap()), 600.0);
        assert_eq!(f32::from_le_bytes(bytes[20..24].try_into().unwrap()), 600.0);
        assert_eq!(i32::from_le_bytes(bytes[24..28].try_into().unwrap()), 13);
    }

    #[test]
    fn conflicting_player_clocks_are_not_silently_merged() {
        let mut a = TickRecord::default(); a.tick = 1; a.state.game_time = Some(500.0);
        let mut b = a.clone(); b.state.game_time = Some(501.0);
        assert!(encode([&a, &b].into_iter(), std::iter::empty(), 1, 1).is_err());
    }
}
