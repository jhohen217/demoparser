//! S2R (S2 Replay flat binary) loader for round replay visualization
//!
//! Loads the compact binary format produced by the tick-by-tick parser,
//! designed for fast seekable loading in the S2DVRMod s&box editor.

use crate::npz_loader::{NpzData, PlayerMeta, WeaponFireData};
use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;

// ─── Format constants ─────────────────────────────────────────────────────────

const HEADER_SIZE: usize = 64;
const KILL_ENTRY_SIZE: usize = 12;

/// Angle scale factor: encode degrees as i16.
/// 32767 / 180.0 ≈ 182.044 — gives ~0.0055° precision.
const ANGLE_SCALE: f32 = 182.0444;

// ─── S2R-specific metadata structures ─────────────────────────────────────────

/// S2R meta JSON structure (different from NPZ's kill_collection wrapper)
#[derive(Debug, Clone, serde::Deserialize)]
struct S2rMetadata {
    version: u32,
    collection_type: String,
    collection_num: u32,
    map_name: String,
    killer_index: u32,
    killer_steamid: String,
    start_tick: i32,
    end_tick: i32,
    kill_ticks: String,
    players: Vec<PlayerMeta>,
}

// ─── Public loader ────────────────────────────────────────────────────────────

impl NpzData {
    /// Load from a `.s2r` (S2 Replay flat binary) file, producing the same
    /// `NpzData` the radar view expects. All data is decoded from the binary
    /// frame block and the embedded meta JSON — no NPZ dependency.
    pub fn load_from_s2r_file(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Err(anyhow!("S2R file not found: {}", path.display()));
        }

        let mut file = File::open(path)?;
        let mut data: Vec<u8> = Vec::new();
        file.read_to_end(&mut data)?;

        // ── Header (64 bytes) ─────────────────────────────────────────────
        if data.len() < HEADER_SIZE {
            return Err(anyhow!("S2R file too small ({} bytes)", data.len()));
        }
        if &data[0..4] != b"S2RF" {
            return Err(anyhow!("Invalid S2R magic bytes"));
        }

        let version = u16::from_le_bytes([data[4], data[5]]);
        let player_count = data[6] as usize;
        let tick_count = u32::from_le_bytes(data[8..12].try_into()?) as usize;
        let min_tick = i32::from_le_bytes(data[12..16].try_into()?);
        let players_offset = u32::from_le_bytes(data[16..20].try_into()?) as usize;
        let meta_offset = u32::from_le_bytes(data[20..24].try_into()?) as usize;
        let frames_offset = u32::from_le_bytes(data[24..28].try_into()?) as usize;
        let kills_offset = u32::from_le_bytes(data[28..32].try_into()?) as usize;
        let frame_stride = u16::from_le_bytes([data[36], data[37]]) as usize;

        // v4 has traj_offset at 38 and wf_offset at 42.
        // Guard against 0 / below-header values — old files written before the
        // offset fields were populated will have zeros here, and parsing from
        // offset 0 would corrupt weapon-fire data with header bytes.
        let _traj_offset: Option<usize> = if version >= 4 {
            let off = u32::from_le_bytes(data[38..42].try_into()?) as usize;
            if off >= HEADER_SIZE {
                Some(off)
            } else {
                None
            }
        } else {
            None
        };
        let wf_offset: Option<usize> = if version >= 4 {
            let off = u32::from_le_bytes(data[42..46].try_into()?) as usize;
            if off >= HEADER_SIZE {
                Some(off)
            } else {
                None
            }
        } else {
            None
        };

        // v7 uses reserved header bytes 46..49 for an append-only audio
        // section. CollectionBrowser does not schedule audio yet, but it must
        // accept current producer output and validate that it can skip this
        // length-delimited section without disturbing legacy replay bytes.
        if version >= 7 {
            let audio_offset = u32::from_le_bytes(data[46..50].try_into()?) as usize;
            let audio_end = if version >= 8 {
                u32::from_le_bytes(data[50..54].try_into()?) as usize
            } else {
                data.len()
            };
            if audio_end < audio_offset || audio_end > data.len() {
                return Err(anyhow!("S2R audio/smoke boundary is outside the file"));
            }
            validate_skippable_audio_block(&data[..audio_end], audio_offset)?;
        }

        // v9's S2EX tail is deliberately opaque to this radar-only reader, but
        // its bounded range must be valid so corrupt offsets are not silently accepted.
        if version >= 9 {
            let extension_offset = u32::from_le_bytes(data[54..58].try_into()?) as usize;
            let extension_length = u32::from_le_bytes(data[58..62].try_into()?) as usize;
            let extension_end = extension_offset
                .checked_add(extension_length)
                .ok_or_else(|| anyhow!("S2R v9 extension range overflow"))?;
            if extension_offset < HEADER_SIZE || extension_end != data.len() {
                return Err(anyhow!("S2R v9 extension range is invalid"));
            }
            if extension_length < 8 || &data[extension_offset..extension_offset + 4] != b"S2EX" {
                return Err(anyhow!("S2R v9 extension signature is invalid"));
            }
        }

        // ── Players block (v3: 46 bytes, v2: 44 bytes) ───────────────────────
        let raw_players = parse_players_block(&data, players_offset, player_count, version)?;

        // ── Meta block (4-byte length prefix + JSON) ──────────────────────
        let meta_json = parse_meta_block(&data, meta_offset)?;

        // Parse using S2R-specific metadata structure
        let metadata: S2rMetadata = serde_json::from_value(meta_json.clone())
            .map_err(|e| anyhow!("Failed to parse S2R metadata: {}", e))?;

        let map_name = metadata.map_name;
        let killer_index = metadata.killer_index as usize;
        let killer_steamid = metadata.killer_steamid.parse::<u64>().unwrap_or(0);
        let start_tick = metadata.start_tick;
        let end_tick = metadata.end_tick;

        // Parse kill_ticks - handle both "[123;456]" and "123;456" formats
        let kill_ticks: Vec<i32> = metadata
            .kill_ticks
            .trim_matches(|c| c == '[' || c == ']')
            .split(';')
            .filter_map(|s| s.trim().parse::<i32>().ok())
            .collect();

        // Build player metadata
        let player_meta = build_player_meta(&raw_players, &meta_json);

        // ── Frames block ──────────────────────────────────────────────────────
        let (frames_vec, pos, angles) = if version >= 3 {
            parse_frames_v3(
                &data,
                frames_offset,
                &raw_players,
                tick_count,
                player_count,
                min_tick,
                version,
            )?
        } else {
            parse_frames_v2(
                &data,
                frames_offset,
                tick_count,
                player_count,
                min_tick,
                frame_stride,
            )?
        };

        // ── Kill events block ─────────────────────────────────────────────────
        // Parse kill events (keep metadata kill_ticks separately)
        let (_kills_from_block, _kills_weapon_fire) = parse_kills_block(&data, kills_offset)?;

        // ── Weapon fire block (v4) ────────────────────────────────────────────
        // v4 has dedicated weapon fire block; v3 and earlier use kills as fallback
        let weapon_fire = if let Some(wf_off) = wf_offset {
            parse_weapon_fire_block(&data, wf_off)?
        } else {
            _kills_weapon_fire
        };

        Ok(NpzData {
            frames: frames_vec,
            pos,
            angles,
            player_meta,
            kill_ticks,
            map_name,
            killer_index,
            killer_steamid,
            start_tick,
            end_tick,
            weapon_fire,
        })
    }
}

/// Validate the v7 audio envelope sufficiently for this legacy reader to skip
/// it. Known payloads intentionally remain opaque here; every record has a
/// length prefix so a future tag never changes the frame/weapon-fire reader.
fn validate_skippable_audio_block(data: &[u8], audio_offset: usize) -> Result<()> {
    let count_end = audio_offset
        .checked_add(4)
        .ok_or_else(|| anyhow!("S2R v7 audio offset overflow"))?;
    if audio_offset < HEADER_SIZE || count_end > data.len() {
        return Err(anyhow!("S2R v7 audio offset is outside the file"));
    }
    let count = u32::from_le_bytes(data[audio_offset..count_end].try_into()?) as usize;
    let mut cursor = count_end;
    for _ in 0..count {
        let header_end = cursor
            .checked_add(8)
            .ok_or_else(|| anyhow!("S2R v7 audio record header overflow"))?;
        if header_end > data.len() {
            return Err(anyhow!("S2R v7 audio record header is truncated"));
        }
        if data[cursor + 1] != 0 || data[cursor + 2..cursor + 4] != [0, 0] {
            return Err(anyhow!("S2R v7 audio record has nonzero reserved bytes"));
        }
        let payload_len = u32::from_le_bytes(data[cursor + 4..cursor + 8].try_into()?) as usize;
        cursor = header_end
            .checked_add(payload_len)
            .ok_or_else(|| anyhow!("S2R v7 audio record length overflow"))?;
        if cursor > data.len() {
            return Err(anyhow!("S2R v7 audio record payload is truncated"));
        }
    }
    if cursor != data.len() {
        return Err(anyhow!("S2R v7 audio block has trailing bytes"));
    }
    Ok(())
}

// ─── Internal types ──────────────────────────────────────────────────────────

/// Raw player entry from the binary players block.
struct RawPlayer {
    steamid: u64,
    name: String,
    /// Team byte: 2=T, 3=CT, 0=unknown
    team: u8,
    /// Number of alive frames (v3 only)
    frame_count: u16,
}

// ─── Block parsers ───────────────────────────────────────────────────────────

fn parse_players_block(
    data: &[u8],
    players_offset: usize,
    player_count: usize,
    version: u16,
) -> Result<Vec<RawPlayer>> {
    let player_entry_size: usize = if version >= 3 { 46 } else { 44 };
    let mut raw_players = Vec::with_capacity(player_count);

    for i in 0..player_count {
        let base = players_offset + i * player_entry_size;
        if base + player_entry_size > data.len() {
            return Err(anyhow!("Players block out of bounds at index {}", i));
        }

        let steamid = u64::from_le_bytes(data[base..base + 8].try_into()?);
        let raw_name = &data[base + 8..base + 40];
        let name_end = raw_name.iter().position(|&b| b == 0).unwrap_or(32);
        let name = String::from_utf8_lossy(&raw_name[..name_end]).to_string();
        let team = data[base + 40];

        // v3 has frame_count at offset 41-42
        let frame_count = if version >= 3 {
            u16::from_le_bytes([data[base + 41], data[base + 42]])
        } else {
            0
        };

        raw_players.push(RawPlayer {
            steamid,
            name,
            team,
            frame_count,
        });
    }

    Ok(raw_players)
}

fn parse_meta_block(data: &[u8], meta_offset: usize) -> Result<serde_json::Value> {
    if meta_offset + 4 > data.len() {
        return Err(anyhow!("Meta offset out of bounds"));
    }

    let meta_len = u32::from_le_bytes(data[meta_offset..meta_offset + 4].try_into()?) as usize;
    let meta_end = meta_offset + 4 + meta_len;
    if meta_end > data.len() {
        return Err(anyhow!("Meta JSON extends past end of file"));
    }

    serde_json::from_slice(&data[meta_offset + 4..meta_end])
        .map_err(|e| anyhow!("Failed to parse S2R meta JSON: {}", e))
}

fn build_player_meta(raw_players: &[RawPlayer], meta_json: &serde_json::Value) -> Vec<PlayerMeta> {
    // Build a steamid → raw team byte lookup from the binary players block.
    let raw_team_lookup: HashMap<u64, u8> =
        raw_players.iter().map(|p| (p.steamid, p.team)).collect();

    if let Some(arr) = meta_json["players"].as_array() {
        arr.iter()
            .filter_map(|p| {
                let steamid_str = p["steamid"].as_str().unwrap_or("0");
                let steamid_u64 = steamid_str.parse::<u64>().unwrap_or(0);

                // Prefer binary block team byte (always 2/3/0) over JSON string.
                let team = if let Some(&byte) = raw_team_lookup.get(&steamid_u64) {
                    match byte {
                        2 => "T",
                        3 => "CT",
                        _ => "?",
                    }
                    .to_string()
                } else {
                    // Older file or steamid mismatch — fall back to JSON string.
                    normalize_team(p["team"].as_str().unwrap_or("?")).to_string()
                };

                Some(PlayerMeta {
                    steamid: steamid_str.to_string(),
                    name: p["name"].as_str().unwrap_or("Unknown").to_string(),
                    index: p["index"].as_u64().unwrap_or(0) as usize,
                    team,
                })
            })
            .collect()
    } else {
        // Fallback: build entirely from the raw binary players block.
        raw_players
            .iter()
            .enumerate()
            .map(|(i, p)| PlayerMeta {
                steamid: p.steamid.to_string(),
                name: p.name.clone(),
                index: i,
                team: match p.team {
                    2 => "T",
                    3 => "CT",
                    _ => "?",
                }
                .to_string(),
            })
            .collect()
    }
}

/// Parse v3+ format frames (variable-length per player with tick prefix).
///
/// v3/v4: angles stored as i16 scaled by ANGLE_SCALE — frame = 4+31 = 35 bytes
/// v5:    angles stored as f32 directly            — frame = 4+35 = 39 bytes
/// v6:    v5 layout plus two cosmetic u32 indexes  — frame = 4+43 = 47 bytes
fn parse_frames_v3(
    data: &[u8],
    frames_offset: usize,
    raw_players: &[RawPlayer],
    tick_count: usize,
    player_count: usize,
    min_tick: i32,
    version: u16,
) -> Result<(Vec<i32>, ndarray::Array3<f32>, ndarray::Array3<f32>)> {
    let use_f32_angles = version >= 5;
    // Total bytes per frame: tick(4) + pos(12) + angles(4 or 8) + rest(15)
    let frame_total: usize = if version >= 6 {
        47
    } else if use_f32_angles {
        39
    } else {
        35
    };
    // Data bytes after reading tick, when tick_idx is out of range (skip the rest)
    let data_bytes: usize = frame_total - 4;

    let mut frames_vec: Vec<i32> = Vec::with_capacity(tick_count);
    let mut pos_flat: Vec<f32> = vec![f32::NAN; tick_count * player_count * 3];
    let mut ang_flat: Vec<f32> = vec![f32::NAN; tick_count * player_count * 2];

    let mut cursor = frames_offset;

    for player_idx in 0..player_count {
        let frame_count = raw_players[player_idx].frame_count as usize;

        for _ in 0..frame_count {
            if cursor + frame_total > data.len() {
                break;
            }

            // Read tick (4 bytes)
            let tick = i32::from_le_bytes(data[cursor..cursor + 4].try_into().unwrap_or([0; 4]));
            cursor += 4;

            // Calculate tick_idx from tick; skip data if out of range
            let tick_idx = (tick - min_tick) as usize;
            if tick_idx >= tick_count {
                cursor += data_bytes;
                continue;
            }

            // Read position (12 bytes)
            let px = f32::from_le_bytes(data[cursor..cursor + 4].try_into().unwrap_or([0; 4]));
            let py = f32::from_le_bytes(data[cursor + 4..cursor + 8].try_into().unwrap_or([0; 4]));
            let pz = f32::from_le_bytes(data[cursor + 8..cursor + 12].try_into().unwrap_or([0; 4]));
            cursor += 12;

            // Read angles: f32×2 (v5+) or i16×2 (v3/v4)
            let (yaw_deg, pitch_deg) = if use_f32_angles {
                let yaw = f32::from_le_bytes(data[cursor..cursor + 4].try_into().unwrap_or([0; 4]));
                let pitch =
                    f32::from_le_bytes(data[cursor + 4..cursor + 8].try_into().unwrap_or([0; 4]));
                cursor += 8;
                (yaw, pitch)
            } else {
                let yaw_raw =
                    i16::from_le_bytes(data[cursor..cursor + 2].try_into().unwrap_or([0; 2]));
                let pitch_raw =
                    i16::from_le_bytes(data[cursor + 2..cursor + 4].try_into().unwrap_or([0; 2]));
                cursor += 4;
                (yaw_raw as f32 / ANGLE_SCALE, pitch_raw as f32 / ANGLE_SCALE)
            };

            // Skip remaining base data: weapon_id(2)+flags(2)+ammo+health+armor(3)+vel×4(8) = 15 bytes.
            // v6 then appends `weapon_cosmetic_index` and `glove_cosmetic_index` (u32 each).
            cursor += 15 + if version >= 6 { 8 } else { 0 };

            // Store in arrays
            let bp = (tick_idx * player_count + player_idx) * 3;
            pos_flat[bp] = px;
            pos_flat[bp + 1] = py;
            pos_flat[bp + 2] = pz;

            let ba = (tick_idx * player_count + player_idx) * 2;
            ang_flat[ba] = pitch_deg;
            ang_flat[ba + 1] = yaw_deg;
        }
    }

    // Build frames_vec
    for tick_idx in 0..tick_count {
        frames_vec.push(min_tick + tick_idx as i32);
    }

    let pos = ndarray::Array3::from_shape_vec((tick_count, player_count, 3), pos_flat)
        .map_err(|e| anyhow!("Failed to build pos array from S2R: {}", e))?;
    let angles = ndarray::Array3::from_shape_vec((tick_count, player_count, 2), ang_flat)
        .map_err(|e| anyhow!("Failed to build angles array from S2R: {}", e))?;

    Ok((frames_vec, pos, angles))
}

/// Parse v2 format frames (fixed grid layout)
fn parse_frames_v2(
    data: &[u8],
    frames_offset: usize,
    tick_count: usize,
    player_count: usize,
    min_tick: i32,
    frame_stride: usize,
) -> Result<(Vec<i32>, ndarray::Array3<f32>, ndarray::Array3<f32>)> {
    let mut frames_vec: Vec<i32> = Vec::with_capacity(tick_count);
    let mut pos_flat: Vec<f32> = vec![f32::NAN; tick_count * player_count * 3];
    let mut ang_flat: Vec<f32> = vec![f32::NAN; tick_count * player_count * 2];

    for tick_idx in 0..tick_count {
        frames_vec.push(min_tick + tick_idx as i32);
        for player_idx in 0..player_count {
            let off = frames_offset + (tick_idx * player_count + player_idx) * frame_stride;
            if off + 18 > data.len() {
                continue;
            }
            let px = f32::from_le_bytes(data[off..off + 4].try_into().unwrap_or([0; 4]));
            let py = f32::from_le_bytes(data[off + 4..off + 8].try_into().unwrap_or([0; 4]));
            let pz = f32::from_le_bytes(data[off + 8..off + 12].try_into().unwrap_or([0; 4]));
            let yaw_raw = i16::from_le_bytes(data[off + 12..off + 14].try_into().unwrap_or([0; 2]));
            let pitch_raw =
                i16::from_le_bytes(data[off + 14..off + 16].try_into().unwrap_or([0; 2]));
            let weapon_id =
                u16::from_le_bytes(data[off + 16..off + 18].try_into().unwrap_or([0xff, 0xff]));

            if weapon_id == 0xFFFF || px.is_nan() {
                continue;
            }

            let bp = (tick_idx * player_count + player_idx) * 3;
            pos_flat[bp] = px;
            pos_flat[bp + 1] = py;
            pos_flat[bp + 2] = pz;

            let ba = (tick_idx * player_count + player_idx) * 2;
            ang_flat[ba] = pitch_raw as f32 / ANGLE_SCALE;
            ang_flat[ba + 1] = yaw_raw as f32 / ANGLE_SCALE;
        }
    }

    let pos = ndarray::Array3::from_shape_vec((tick_count, player_count, 3), pos_flat)
        .map_err(|e| anyhow!("Failed to build pos array from S2R: {}", e))?;
    let angles = ndarray::Array3::from_shape_vec((tick_count, player_count, 2), ang_flat)
        .map_err(|e| anyhow!("Failed to build angles array from S2R: {}", e))?;

    Ok((frames_vec, pos, angles))
}

fn parse_kills_block(
    data: &[u8],
    kills_offset: usize,
) -> Result<(Vec<i32>, Option<WeaponFireData>)> {
    if kills_offset + 2 > data.len() {
        return Err(anyhow!("Kills block out of bounds"));
    }

    let kill_count = u16::from_le_bytes([data[kills_offset], data[kills_offset + 1]]) as usize;

    let mut kill_ticks = Vec::with_capacity(kill_count);
    let mut wf_tick: Vec<i32> = Vec::with_capacity(kill_count);
    let mut wf_attacker: Vec<u8> = Vec::with_capacity(kill_count);
    let mut wf_weapon_id: Vec<u16> = Vec::with_capacity(kill_count);
    let mut wf_offsets: Vec<i32> = vec![0];
    let mut wf_victim_idx: Vec<u8> = Vec::with_capacity(kill_count);
    let mut wf_damage: Vec<u16> = Vec::new();
    let mut wf_kill: Vec<u8> = Vec::new();

    for i in 0..kill_count {
        let base = kills_offset + 2 + i * KILL_ENTRY_SIZE;
        if base + KILL_ENTRY_SIZE > data.len() {
            break;
        }

        let tick = i32::from_le_bytes(data[base..base + 4].try_into()?);
        let killer_idx = data[base + 4];
        let victim_idx = data[base + 5];
        let weapon_id = u16::from_le_bytes([data[base + 6], data[base + 7]]);

        kill_ticks.push(tick);
        wf_tick.push(tick);
        wf_attacker.push(killer_idx);
        wf_weapon_id.push(weapon_id);
        wf_victim_idx.push(victim_idx);
        wf_damage.push(0);
        wf_kill.push(1);
        wf_offsets.push(wf_victim_idx.len() as i32);
    }

    let weapon_fire = if !wf_tick.is_empty() {
        Some(WeaponFireData {
            tick: wf_tick.clone(),
            impact_tick: wf_tick,
            attacker: wf_attacker,
            weapon_id: wf_weapon_id,
            offsets: wf_offsets,
            victim_idx: wf_victim_idx,
            damage: wf_damage,
            kill: wf_kill,
        })
    } else {
        None
    };

    Ok((kill_ticks, weapon_fire))
}

/// Parse weapon fire block (v4 format)
///
/// Format:
///   count:        u16   (2)
///   For each event:
///     tick:         i32   (4)
///     impact_tick:  i32   (4)
///     attacker_idx: u8    (1)
///     weapon_id:    u16   (2)
///     victim_count: u8    (1)
///     For each victim:
///       victim_idx: u8    (1)
///       damage:     u16   (2)
///       is_kill:    u8    (1)
fn parse_weapon_fire_block(data: &[u8], wf_offset: usize) -> Result<Option<WeaponFireData>> {
    if wf_offset + 2 > data.len() {
        return Ok(None);
    }

    let wf_count = u16::from_le_bytes([data[wf_offset], data[wf_offset + 1]]) as usize;
    if wf_count == 0 {
        return Ok(None);
    }

    let mut wf_tick: Vec<i32> = Vec::with_capacity(wf_count);
    let mut wf_impact_tick: Vec<i32> = Vec::with_capacity(wf_count);
    let mut wf_attacker: Vec<u8> = Vec::with_capacity(wf_count);
    let mut wf_weapon_id: Vec<u16> = Vec::with_capacity(wf_count);
    let mut wf_offsets: Vec<i32> = vec![0];
    let mut wf_victim_idx: Vec<u8> = Vec::new();
    let mut wf_damage: Vec<u16> = Vec::new();
    let mut wf_kill: Vec<u8> = Vec::new();

    let mut cursor = wf_offset + 2;

    for _ in 0..wf_count {
        if cursor + 14 > data.len() {
            break;
        }

        let tick = i32::from_le_bytes(data[cursor..cursor + 4].try_into().unwrap_or([0; 4]));
        cursor += 4;

        let impact_tick = i32::from_le_bytes(data[cursor..cursor + 4].try_into().unwrap_or([0; 4]));
        cursor += 4;

        let attacker_idx = data[cursor];
        cursor += 1;

        let weapon_id = u16::from_le_bytes([data[cursor], data[cursor + 1]]);
        cursor += 2;

        let victim_count = data[cursor];
        cursor += 1;

        wf_tick.push(tick);
        wf_impact_tick.push(impact_tick);
        wf_attacker.push(attacker_idx);
        wf_weapon_id.push(weapon_id);

        // Read victims
        for _ in 0..victim_count {
            if cursor + 4 > data.len() {
                break;
            }

            let victim_idx = data[cursor];
            cursor += 1;

            let damage = u16::from_le_bytes([data[cursor], data[cursor + 1]]);
            cursor += 2;

            let is_kill = data[cursor];
            cursor += 1;

            wf_victim_idx.push(victim_idx);
            wf_damage.push(damage);
            wf_kill.push(is_kill);
        }

        wf_offsets.push(wf_victim_idx.len() as i32);
    }

    if wf_tick.is_empty() {
        return Ok(None);
    }

    Ok(Some(WeaponFireData {
        tick: wf_tick,
        impact_tick: wf_impact_tick,
        attacker: wf_attacker,
        weapon_id: wf_weapon_id,
        offsets: wf_offsets,
        victim_idx: wf_victim_idx,
        damage: wf_damage,
        kill: wf_kill,
    }))
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Normalise a raw team string to the canonical two-letter form.
fn normalize_team(raw: &str) -> &str {
    match raw {
        "2" | "Terrorist" => "T",
        "3" | "Counter-Terrorist" => "CT",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal deterministic S2R byte builder used to certify the two frame
    /// layouts.  It deliberately contains no optional trailing sections.
    fn one_frame_fixture(version: u16) -> Vec<u8> {
        let metadata = serde_json::json!({
            "version": 1,
            "collection_type": "fixture",
            "collection_num": 1,
            "map_name": "de_fixture",
            "killer_index": 0,
            "killer_steamid": "76561198000000000",
            "start_tick": 100,
            "end_tick": 100,
            "kill_ticks": "[]",
            "players": [{
                "steamid": "76561198000000000",
                "name": "fixture",
                "index": 0,
                "team": "CT"
            }],
            "cosmetics": {
                "schema_version": 1,
                "unknown_index": 0,
                "weapon_signatures": [null, {"index": 1, "item_definition_index": 7, "paint_kit_id": 180}],
                "glove_signatures": [null, {"index": 1, "item_definition_index": 5030, "paint_kit_id": 10018}]
            }
        });
        let meta = serde_json::to_vec(&metadata).unwrap();
        let stride = if version >= 6 { 47usize } else { 39usize };
        let players_offset = HEADER_SIZE;
        let meta_offset = players_offset + 46;
        let frames_offset = meta_offset + 4 + meta.len();
        let kills_offset = frames_offset + stride;
        let util_offset = kills_offset + 2;
        let mut bytes = vec![0u8; util_offset];
        bytes[0..4].copy_from_slice(b"S2RF");
        bytes[4..6].copy_from_slice(&version.to_le_bytes());
        bytes[6] = 1;
        bytes[8..12].copy_from_slice(&1u32.to_le_bytes());
        bytes[12..16].copy_from_slice(&100i32.to_le_bytes());
        bytes[16..20].copy_from_slice(&(players_offset as u32).to_le_bytes());
        bytes[20..24].copy_from_slice(&(meta_offset as u32).to_le_bytes());
        bytes[24..28].copy_from_slice(&(frames_offset as u32).to_le_bytes());
        bytes[28..32].copy_from_slice(&(kills_offset as u32).to_le_bytes());
        bytes[32..36].copy_from_slice(&(util_offset as u32).to_le_bytes());
        bytes[36..38].copy_from_slice(&(stride as u16).to_le_bytes());

        let player = players_offset;
        bytes[player..player + 8].copy_from_slice(&76561198000000000u64.to_le_bytes());
        bytes[player + 8..player + 15].copy_from_slice(b"fixture");
        bytes[player + 40] = 3;
        bytes[player + 41..player + 43].copy_from_slice(&1u16.to_le_bytes());

        bytes[meta_offset..meta_offset + 4].copy_from_slice(&(meta.len() as u32).to_le_bytes());
        bytes[meta_offset + 4..frames_offset].copy_from_slice(&meta);

        let frame = frames_offset;
        bytes[frame..frame + 4].copy_from_slice(&100i32.to_le_bytes());
        bytes[frame + 4..frame + 8].copy_from_slice(&1.0f32.to_le_bytes());
        bytes[frame + 8..frame + 12].copy_from_slice(&2.0f32.to_le_bytes());
        bytes[frame + 12..frame + 16].copy_from_slice(&3.0f32.to_le_bytes());
        bytes[frame + 16..frame + 20].copy_from_slice(&90.0f32.to_le_bytes());
        bytes[frame + 20..frame + 24].copy_from_slice(&45.0f32.to_le_bytes());
        bytes[frame + 24..frame + 26].copy_from_slice(&7u16.to_le_bytes());
        bytes[frame + 28] = 30;
        bytes[frame + 29] = 100;
        bytes[frame + 30] = 100;
        if version >= 6 {
            bytes[frame + 39..frame + 43].copy_from_slice(&1u32.to_le_bytes());
            bytes[frame + 43..frame + 47].copy_from_slice(&1u32.to_le_bytes());
        }
        if version >= 7 {
            let audio_offset = bytes.len() as u32;
            bytes[46..50].copy_from_slice(&audio_offset.to_le_bytes());
            bytes.extend_from_slice(&0u32.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn loader_keeps_v5_fixture_readable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy-v5.s2r");
        std::fs::write(&path, one_frame_fixture(5)).unwrap();
        let replay = NpzData::load_from_s2r_file(&path).unwrap();
        assert_eq!(replay.frames, vec![100]);
        assert_eq!(replay.pos[[0, 0, 0]], 1.0);
        assert_eq!(replay.angles[[0, 0, 0]], 45.0);
        assert_eq!(replay.angles[[0, 0, 1]], 90.0);
    }

    #[test]
    fn loader_skips_v6_cosmetic_extension_without_desynchronizing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cosmetics-v6.s2r");
        std::fs::write(&path, one_frame_fixture(6)).unwrap();
        let replay = NpzData::load_from_s2r_file(&path).unwrap();
        assert_eq!(replay.frames, vec![100]);
        assert_eq!(replay.pos[[0, 0, 2]], 3.0);
        assert_eq!(replay.angles[[0, 0, 0]], 45.0);
    }

    #[test]
    fn loader_accepts_v7_and_skips_an_empty_audio_section() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audio-v7.s2r");
        std::fs::write(&path, one_frame_fixture(7)).unwrap();
        let replay = NpzData::load_from_s2r_file(&path).unwrap();
        assert_eq!(replay.frames, vec![100]);
    }
}
