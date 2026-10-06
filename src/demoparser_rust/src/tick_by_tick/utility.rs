//! Additive utility authority envelope. Source time values in fields stay raw.
use anyhow::Result;
use parser::second_pass::utility::{UtilityData, UtilityRecord};
use serde::Serialize;
use std::io::{Read, Write};

pub const MAX_UTILITY_JSON: usize = 256 * 1024 * 1024;

#[derive(Serialize)]
struct Envelope<'a> {
    captured: bool,
    server_tick_offset: Option<i32>,
    records: Vec<&'a UtilityRecord>,
}

/// Retained for reproducible v15/v16 performance and equivalence comparisons.
pub fn encode_legacy(data: Option<&UtilityData>, max_tick: i32) -> Result<(Vec<u8>, u32)> {
    // Preserve history before the visible window: a decoy, fire, flash or
    // sound may have started there. EOF never synthesizes an expiry.
    let records: Vec<_> = data.into_iter().flat_map(|d| &d.records)
        .filter(|r| r.tick <= max_tick).collect();
    let count = u32::try_from(records.len())?;
    let json = serde_json::to_vec(&Envelope {
        captured: data.is_some_and(|d| d.captured),
        server_tick_offset: data.and_then(|d| d.server_tick_offset), records,
    })?;
    anyhow::ensure!(json.len() <= MAX_UTILITY_JSON, "Utility authority exceeds reader limit");
    let mut payload = (json.len() as u32).to_le_bytes().to_vec();
    let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gzip.write_all(&json)?;
    payload.extend(gzip.finish()?);
    Ok((payload, count))
}

pub fn is_captured_legacy(payload: &[u8], declared: u32) -> bool {
    #[derive(serde::Deserialize)]
    struct Coverage { captured: bool, records: Vec<serde::de::IgnoredAny> }
    let Some(header) = payload.get(..4) else { return false; };
    let length = u32::from_le_bytes(header.try_into().unwrap()) as usize;
    if length > MAX_UTILITY_JSON { return false; }
    let mut json = Vec::new();
    if flate2::read::GzDecoder::new(&payload[4..]).take(length as u64 + 1).read_to_end(&mut json).is_err()
        || json.len() != length { return false; }
    serde_json::from_slice::<Coverage>(&json).is_ok_and(|v| v.captured && v.records.len() == declared as usize)
}

pub fn encode(data: Option<&UtilityData>, max_tick: i32) -> Result<(Vec<u8>, u32)> {
    super::utility_binary::encode(data, max_tick)
}

pub fn is_captured(payload: &[u8], declared: u32) -> bool {
    super::utility_binary::is_captured(payload, declared)
}

/// Explicit codec selection for reproducible format benchmarks (0 raw, 1 LZ4, 2 gzip, 3 zstd).
pub fn encode_with_codec(data: Option<&UtilityData>, max_tick: i32, codec: u8) -> Result<(Vec<u8>, u32)> {
    super::utility_binary::encode_with_codec(data, max_tick, codec)
}

/// Compatibility position tracks derived from the lossless authority. No owner
/// name requirement, no zero-filled coordinates, and no joining reused slots.
pub fn trajectories(data: &UtilityData, owners: &[u64], start: u32, end: u32)
    -> Vec<super::grenade_processor::GrenadeTrajectory> {
    use super::grenade_processor::{GrenadeTrajectory, GrenadeTrajectoryPoint};
    use std::collections::BTreeMap;
    struct Life { track: GrenadeTrajectory, ended: Option<i32> }
    let mut active = BTreeMap::new();
    let mut lives: Vec<Life> = Vec::new();
    for row in &data.records {
        if row.tick > end as i32 || row.name == "CInferno" { continue; }
        let (Some(id), Some(serial)) = (row.entity_id, row.serial) else { continue; };
        let key = (id, serial, row.name.clone());
        let index = *active.entry(key.clone()).or_insert_with(|| {
            lives.push(Life { track: GrenadeTrajectory { steamid: 0, grenade_type: row.name.clone(),
                entity_id: id, trajectory_points: Vec::new() }, ended: None });
            lives.len() - 1
        });
        let life = &mut lives[index];
        if let Some(owner) = row.thrower_steamid { life.track.steamid = owner; }
        if let Some([x, y, z]) = row.position {
            if [x, y, z].iter().all(|v| v.is_finite()) {
                let points = &mut life.track.trajectory_points;
                let point = GrenadeTrajectoryPoint { tick: row.tick, pos_x: x, pos_y: y, pos_z: z };
                // Legacy tracks have one position/tick; tag 12 retains every
                // within-message sample and its exact source order.
                if points.last().is_some_and(|p| p.tick == row.tick) { points.pop(); }
                points.push(point);
            }
        }
        if matches!(row.action.as_str(), "delete" | "leave" | "replaced") {
            life.ended = Some(row.tick);
            active.remove(&key);
        }
    }
    let mut tracks: Vec<_> = lives.into_iter().filter_map(|mut life| {
        if life.ended.is_some_and(|tick| tick < start as i32)
            || life.track.trajectory_points.is_empty()
            || life.track.steamid != 0 && !owners.is_empty() && !owners.contains(&life.track.steamid) { return None; }
        // Keep the final real sample before the window to seed a resting or
        // already airborne projectile. Never fabricate a window-start sample.
        let first_inside = life.track.trajectory_points.partition_point(|p| p.tick < start as i32);
        life.track.trajectory_points.drain(..first_inside.saturating_sub(1));
        Some(life.track)
    }).collect();
    tracks.sort_by_key(|t| (t.trajectory_points[0].tick, t.entity_id));
    tracks
}

#[cfg(test)]
mod tests {
    use super::*;
    fn row(tick: i32, serial: u32, action: &str, position: Option<[f32; 3]>) -> UtilityRecord {
        UtilityRecord { tick, frame_offset: tick as u64, message_index: 0, sequence: tick as u64,
            action: action.into(), entity_id: Some(17), serial: Some(serial), name: "CDecoyProjectile".into(),
            fields: vec![], position, thrower_steamid: None }
    }
    #[test]
    fn retains_unknown_owner_resting_seed_and_separates_lifetimes() {
        let data = UtilityData { captured: true, records: vec![
            row(1, 1, "create", None), row(2, 1, "update", Some([1., 2., 3.])),
            row(5, 1, "delete", Some([1., 2., 3.])),
            row(6, 2, "create", None), row(7, 2, "update", Some([9., 8., 7.])),
        ], ..Default::default() };
        let tracks = trajectories(&data, &[7656], 3, 10);
        assert_eq!(tracks.len(), 2);
        assert_eq!(tracks[0].steamid, 0);
        assert_eq!(tracks[0].trajectory_points[0].tick, 2);
        assert_eq!(tracks[1].trajectory_points.len(), 1);
        assert_eq!(trajectories(&data, &[], 8, 10).len(), 1);
        let (payload, count) = encode(Some(&data), 5).unwrap();
        assert_eq!(count, 3);
        assert!(is_captured(&payload, count));
        assert!(!is_captured(&payload, count + 1));
        let (missing, _) = encode(None, 5).unwrap();
        assert!(!is_captured(&missing, 0));
    }
}
