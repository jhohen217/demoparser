//! S2R binary output writer for CS2 demo tick-by-tick data
//!
//! Writes shared round or explicitly padded collection replay data in the `.s2r` flat binary
//! format, designed for fast seekable loading in the S2DVRMod s&box editor.
//!
//! ## File Layout (little-endian, v18)
//! v18 adds opaque networked AG2 recipe snapshots in S2EX tag 15/schema 1.
//!
//! ```text
//! [Header          – 64 bytes]
//! [Players block   – player_count × 46 bytes (includes frame_count per player)]
//! [Meta block      – 4 + meta_len bytes (JSON)]
//! [Frames block    – variable size, only alive ticks stored with tick prefix]
//! [Kill events     – 2 + kill_count × 12 bytes]
//! [Util events     – 2 + util_count × 24 bytes]
//! [Trajectories    – 4 + traj_count × variable bytes]
//! [Weapon Fire     – 2 + wf_count × variable bytes (CSR format)]
//! [Raw audio       – length-delimited records (v7)]
//! [Smoke voxels    – entity/lifetime tracks with raw network payload frames (v8)]
//! [S2EX extensions – versioned authority-section directory (v9+)]
//! ```
//!
//! ## Header Layout (64 bytes)
//!
//! ```text
//! magic:           [u8; 4]  (offset  0)  "S2RF"
//! version:         u16      (offset  4)  = 17 (see S2R_VERSION)
//! player_count:    u8       (offset  6)
//! _pad:            u8       (offset  7)
//! tick_count:      u32      (offset  8)  total ticks in range (for reference)
//! min_tick:        i32      (offset 12)
//! players_offset:  u32      (offset 16)
//! meta_offset:     u32      (offset 20)
//! frames_offset:   u32      (offset 24)
//! kills_offset:    u32      (offset 28)
//! util_offset:     u32      (offset 32)
//! frame_stride:    u16      (offset 36)  = 47 bytes (4 tick + 43 data)
//! traj_offset:     u32      (offset 38)  trajectory block offset
//! wf_offset:       u32      (offset 42)  weapon fire block offset
//! audio_offset:    u32      (offset 46)  raw-audio block offset (v7+)
//! smoke_offset:    u32      (offset 50)  smoke-voxel block offset (v8+)
//! extension_offset:u32      (offset 54)  S2EX directory offset (v9+)
//! extension_length:u32      (offset 58)  S2EX directory + payload length
//! _reserved:       [u8; 2]  (offset 62)
//! ```
//!
//! ## Players Block (v3) — player_count × 46 bytes
//!
//! ```text
//! steamid:      u64      (8)
//! name:         [u8;32]  (32)
//! team:         u8       (1)   2=T  3=CT  0=unknown
//! frame_count:  u16      (2)   number of alive frames for this player
//! _pad:         [u8;3]   (3)
//! ```
//!
//! ## Frames Block (v6) — per-player variable length
//!
//! For each player, store only alive ticks:
//! ```text
//! tick:       i32   (4)   tick number
//! pos_x:      f32   (4)   CS2 world units
//! pos_y:      f32   (4)
//! pos_z:      f32   (4)
//! yaw:        i16   (2)   degrees × 182.044 (±180° → ±32767)
//! pitch:      i16   (2)   same scale
//! weapon_id:  u16   (2)   numeric item-def index
//! flags:      u16   (2)   see FLAGS_* constants below (no alive bit)
//! ammo:       u8    (1)
//! health:     u8    (1)   1–100
//! armor:      u8    (1)   0–100
//! vel_x:      i16   (2)   velocity_x × 10
//! vel_y:      i16   (2)   velocity_y × 10
//! vel_z:      i16   (2)   velocity_z × 10
//! mouse_vel:  i16   (2)   mouse_velocity × 10
//! ```
//! v6 adds `weapon_cosmetic_index: u32` and `glove_cosmetic_index: u32`
//! after the v5 39-byte prefix, for 47 bytes per alive frame.
//!
//! ## Flags Bitfield (u16) — v3 (no alive bit)
//!
//! ```text
//! bit  0  in_reload
//! bit  1  scoped
//! bit  2  inspecting
//! bit  3  airborne
//! bit  4  walking
//! bit  5  defusing
//! bit  6  fw (forward key)
//! bit  7  lf (left key)
//! bit  8  rt (right key)
//! bit  9  bk (back key)
//! bit 10  fire
//! bit 11  crouching
//! bit 12  secondary fire / right click
//! ```

use super::data_types::{
    GloveCosmeticObservation, KeychainObservation, StickerObservation, TickRecord,
    WeaponCosmeticObservation,
};
use super::grenade_processor::{GrenadeStats, GrenadeTrajectory, UtilityThrown, WeaponFireEvent};
use super::kill_collection_parser::{Collection, KillCollectionData};
use anyhow::{anyhow, Result};
use parser::second_pass::audio::{AudioEvent, AudioEventOrder, AudioEventPayload, SvcSoundEntry};
use parser::second_pass::game_events::GameEvent;
use parser::second_pass::kill_modifiers::{KillModifiers, ATTACKER_AIRBORNE, VICTIM_AIRBORNE};
use parser::second_pass::parser_settings::WeaponEntitySnapshot;
use parser::second_pass::smoke_voxels::SmokeVoxelTrack;
use parser::second_pass::variants::Variant;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

// ─── Format constants ─────────────────────────────────────────────────────────

const S2R_MAGIC: &[u8; 4] = b"S2RF";
/// Format revision written into the header. Recorded per asset in `tick_assets` so a stale
/// output is detectable in SQL rather than only by reading the file back.
// v11 deliberately keeps the v10 byte layout. It is a metadata semantics revision: collection
// killer/victim indexes now address the deterministic SteamID-sorted player table instead of
// parser entity indexes, which are unstable when the same round is parsed from a trimmed clip.
// v13 fills kill modifiers and repurposes their three padding bytes for a known
// mask and penetration count. Tag 9 adds eye-position and timed player states.
// v18 adds opaque networked AG2 recipe snapshots in S2EX tag 15.
pub const S2R_VERSION: u16 = 18;
// Deployment invariant: the tolerant C# reader accepting schema 1 | 2 must land before any
// production binary that emits this schema. An older all-or-nothing reader drops every finish.
pub const COSMETIC_SCHEMA_VERSION: u32 = 2;
const S2R_V6_VERSION: u16 = 6;
const S2R_V8_VERSION: u16 = 8;

const HEADER_SIZE: u32 = 64;
const PLAYER_ENTRY_SIZE: u32 = 46; // 8 + 32 + 1 + 2 + 3 (added frame_count: u16)
/// Frame data size in bytes (excluding tick prefix).
/// v6: 43 bytes — v5 data (35) followed by weapon and glove cosmetic indexes (u32 each).
const FRAME_DATA_SIZE: u16 = 43;
/// Tick prefix size for v3+ formats.
const TICK_PREFIX_SIZE: u16 = 4;
/// Total bytes per alive frame: tick (4) + data (43) = 47 bytes.
const FRAME_STRIDE: u16 = TICK_PREFIX_SIZE + FRAME_DATA_SIZE;
const KILL_ENTRY_SIZE: u32 = 12;
const AUDIO_OFFSET_HEADER_BYTE: usize = 46;
const SMOKE_OFFSET_HEADER_BYTE: usize = 50;
const EXTENSION_MAGIC: &[u8; 4] = b"S2EX";
const EXTENSION_SCHEMA_VERSION: u16 = 1;
const EXT_AGENT_LIVES: u16 = 1;
const EXT_WEAPON_LIFETIMES: u16 = 2;
const EXT_INVENTORY_DELTAS: u16 = 3;
const EXT_WORLD_WEAPON_DELTAS: u16 = 4;
const EXT_RAGDOLL_IMPACTS: u16 = 5;
const EXT_BULLET_IMPACTS: u16 = 6;
const EXT_GRENADE_DETONATIONS: u16 = 7;
const EXT_FIRE_BULLETS_INPUTS: u16 = 8;
const EXT_PLAYER_STATES: u16 = 9;
const EXT_WORLD_ENTITIES: u16 = 13;
const EXT_DEATH_INPUT_PROVENANCE: u16 = 14;
const EXT_AG2_RECIPES: u16 = 15;
/// Bytes per world-entity row. See `encode_world_entities`.
const WORLD_ENTITY_ROW_SIZE: usize = 56;
/// How far a spawn origin may differ before it is treated as a different entity. Network
/// quantisation of an authored origin was at most 0.018 units across the audited de_nuke match,
/// and the closest pair of authored world entities on that map is 82 units apart.
const WORLD_ENTITY_ORIGIN_EPSILON: f32 = 0.1;
const AUDIO_EVENT_HEADER_SIZE: usize = 8;
const AUDIO_COMMON_SIZE: usize = 16;
#[allow(dead_code)]
const UTIL_ENTRY_SIZE: u32 = 24; // documented for C# reader reference

/// Angle scale factor (kept for reference / backward-compat C# readers).
/// v5+ stores angles as f32 directly — this constant is no longer used by the writer.
#[allow(dead_code)]
const ANGLE_SCALE: f32 = 182.0444;

/// Velocity scale factor: encode units/s as i16 with 0.1 resolution.
/// max i16 (32767) / 10 = 3276.7 units/s — well above any CS2 speed cap.
const VEL_SCALE: f32 = 10.0;

/// Mouse velocity scale factor: encode degrees/tick as i16 with 0.1 resolution.
const MOUSE_VEL_SCALE: f32 = 10.0;

/// Clamp an f32, scale it, and convert to i16.
#[inline]
fn encode_scaled_i16(val: f32, scale: f32) -> i16 {
    (val * scale)
        .round()
        .clamp(i16::MIN as f32, i16::MAX as f32) as i16
}

// ─── Internal structures ──────────────────────────────────────────────────────

#[derive(Default)]
struct KillEvent {
    tick: i32,
    killer_idx: u8,
    victim_idx: u8,
    weapon_id: u16,
    kill_flags: u8, // bit 0=headshot, 1=thru_smoke, 2=no_scope, 3=attacker_blind, 4=penetrated
    known_flags: u8, // same bits; bit5=attacker airborne, bit6=victim airborne
    penetrated: Option<u32>,
}

// ─── v6 cosmetic catalog ────────────────────────────────────────────────────

/// S2R v6 keeps cosmetic identity out of the hot frame payload.  Each alive
/// frame stores two compact indexes into these tables; index zero is reserved
/// for "not observed" and is represented by the leading `null` JSON element.
///
/// The explicit `index` field is redundant by design: consumers can validate
/// that an array was not reordered while copying/serializing metadata.
#[derive(Debug, Clone, Serialize)]
struct CosmeticMetadata {
    schema_version: u32,
    unknown_index: u32,
    weapon_signatures: Vec<Option<WeaponCosmeticSignature>>,
    glove_signatures: Vec<Option<GloveCosmeticSignature>>,
}

#[derive(Debug, Clone, Serialize)]
struct WeaponCosmeticSignature {
    index: u32,
    item_definition_index: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    item_id: Option<u64>,
    paint_kit_id: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    paint_seed: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    wear: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    quality: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stattrak: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    custom_name: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    stickers: Vec<StickerSignature>,
    #[serde(skip_serializing_if = "Option::is_none")]
    keychain: Option<KeychainSignature>,
}

#[derive(Debug, Clone, Serialize)]
struct GloveCosmeticSignature {
    index: u32,
    item_definition_index: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    item_id: Option<u64>,
    paint_kit_id: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    paint_seed: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    wear: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    quality: Option<u16>,
}

#[derive(Debug, Clone, Serialize)]
struct StickerSignature {
    slot: u32,
    sticker_id: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    wear: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    scale: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rotation: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    offset_x: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    offset_y: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    schema: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
struct KeychainSignature {
    keychain_id: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    offset_x: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    offset_y: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    offset_z: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    seed: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    highlight: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sticker_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    display_case_keychain_id: Option<u32>,
}

/// Hashable, finite-only canonical form.  Float bits preserve an observed
/// cosmetic exactly and avoid treating `0.1f32` and a nearby parsed value as
/// the same signature.  Non-finite values are omitted before a key is made so
/// the JSON metadata can never contain NaN or infinity.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct WeaponCosmeticKey {
    item_definition_index: u16,
    item_id: Option<u64>,
    paint_kit_id: u32,
    paint_seed: Option<u32>,
    wear_bits: Option<u32>,
    quality: Option<u16>,
    stattrak: Option<i32>,
    custom_name: Option<String>,
    stickers: Vec<StickerKey>,
    keychain: Option<KeychainKey>,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct GloveCosmeticKey {
    item_definition_index: u16,
    item_id: Option<u64>,
    paint_kit_id: u32,
    paint_seed: Option<u32>,
    wear_bits: Option<u32>,
    quality: Option<u16>,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct StickerKey {
    slot: u32,
    sticker_id: u32,
    wear_bits: Option<u32>,
    scale_bits: Option<u32>,
    rotation_bits: Option<u32>,
    offset_x_bits: Option<u32>,
    offset_y_bits: Option<u32>,
    schema: Option<u32>,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct KeychainKey {
    keychain_id: u32,
    offset_x_bits: Option<u32>,
    offset_y_bits: Option<u32>,
    offset_z_bits: Option<u32>,
    seed: Option<u32>,
    highlight: Option<u32>,
    sticker_id: Option<u32>,
    display_case_keychain_id: Option<u32>,
}

struct CosmeticCatalog {
    weapon_indexes: HashMap<WeaponCosmeticKey, u32>,
    glove_indexes: HashMap<GloveCosmeticKey, u32>,
    weapon_signatures: Vec<Option<WeaponCosmeticSignature>>,
    glove_signatures: Vec<Option<GloveCosmeticSignature>>,
}

impl CosmeticCatalog {
    fn new() -> Self {
        // S2R's on-disk sentinel is 0, so both array index 0 and the explicit
        // metadata field convey the same contract to defensive readers.
        Self {
            weapon_indexes: HashMap::new(),
            glove_indexes: HashMap::new(),
            weapon_signatures: vec![None],
            glove_signatures: vec![None],
        }
    }

    fn from_records(
        sorted_steamids: &[u64],
        grouped_records: &HashMap<u64, Vec<TickRecord>>,
        weapon_entities: &[WeaponEntitySnapshot],
    ) -> Self {
        let mut catalog = Self::new();
        // This is intentionally the exact iteration order used by the frame
        // writer: sorted player SteamIDs, then source tick order.  That makes
        // assigned indexes deterministic for a deterministic parser output.
        for sid in sorted_steamids {
            if let Some(records) = grouped_records.get(sid) {
                for record in records.iter().filter(|record| record.alive > 0) {
                    catalog.intern_weapon(record.weapon_cosmetic.as_ref());
                    catalog.intern_glove(record.glove_cosmetic.as_ref());
                }
            }
        }
        // Authority tracks can contain inactive or dropped cosmetics that never
        // appear in an active-weapon frame. Intern them into the same catalog so
        // every concrete weapon lifetime can reference one canonical signature.
        let mut ordered_entities: Vec<_> = weapon_entities.iter().collect();
        ordered_entities
            .sort_by_key(|row| (row.tick, row.source_order, row.entity_id, row.entity_serial));
        for entity in ordered_entities {
            let observation = weapon_snapshot_cosmetic(entity);
            catalog.intern_weapon(observation.as_ref());
        }
        catalog
    }

    fn metadata(&self) -> CosmeticMetadata {
        CosmeticMetadata {
            schema_version: COSMETIC_SCHEMA_VERSION,
            unknown_index: 0,
            weapon_signatures: self.weapon_signatures.clone(),
            glove_signatures: self.glove_signatures.clone(),
        }
    }

    fn weapon_index(&self, observation: Option<&WeaponCosmeticObservation>) -> u32 {
        weapon_key(observation)
            .and_then(|key| self.weapon_indexes.get(&key).copied())
            .unwrap_or(0)
    }

    fn glove_index(&self, observation: Option<&GloveCosmeticObservation>) -> u32 {
        glove_key(observation)
            .and_then(|key| self.glove_indexes.get(&key).copied())
            .unwrap_or(0)
    }

    fn intern_weapon(&mut self, observation: Option<&WeaponCosmeticObservation>) {
        let Some(key) = weapon_key(observation) else {
            return;
        };
        if self.weapon_indexes.contains_key(&key) {
            return;
        }
        let index = self.weapon_signatures.len() as u32;
        self.weapon_indexes.insert(key.clone(), index);
        self.weapon_signatures
            .push(Some(weapon_signature(index, &key)));
    }

    fn intern_glove(&mut self, observation: Option<&GloveCosmeticObservation>) {
        let Some(key) = glove_key(observation) else {
            return;
        };
        if self.glove_indexes.contains_key(&key) {
            return;
        }
        let index = self.glove_signatures.len() as u32;
        self.glove_indexes.insert(key.clone(), index);
        self.glove_signatures
            .push(Some(glove_signature(index, &key)));
    }
}

fn weapon_snapshot_cosmetic(snapshot: &WeaponEntitySnapshot) -> Option<WeaponCosmeticObservation> {
    let attribute = |definition| {
        snapshot.econ_attributes.iter().find_map(|attribute| {
            (attribute.definition_index == definition).then_some(&attribute.raw_value)
        })
    };
    let raw_u32 = |value: &Variant| match value {
        Variant::U32(value) => Some(*value),
        Variant::I32(value) if *value >= 0 => Some(*value as u32),
        Variant::F32(value) => Some(value.to_bits()),
        _ => None,
    };
    let attribute_f32 = |definition| match attribute(definition) {
        Some(Variant::F32(value)) if value.is_finite() => Some(*value),
        Some(Variant::U32(value)) => Some(f32::from_bits(*value)).filter(|value| value.is_finite()),
        _ => None,
    };
    let attribute_raw_u32 = |definition| attribute(definition).and_then(raw_u32);
    let keychain = attribute_raw_u32(299)
        .filter(|id| *id != 0)
        .map(|keychain_id| KeychainObservation {
            keychain_id,
            offset_x: attribute_f32(300),
            offset_y: attribute_f32(301),
            offset_z: attribute_f32(302),
            seed: attribute_raw_u32(306),
            highlight: attribute_raw_u32(314),
            sticker_id: attribute_raw_u32(321),
            display_case_keychain_id: attribute_raw_u32(322),
        });
    Some(WeaponCosmeticObservation {
        item_definition_index: u16::try_from(snapshot.item_definition_index?).ok(),
        item_id: match (snapshot.item_id_high, snapshot.item_id_low) {
            (None, None) => None,
            (high, low) => Some((u64::from(high.unwrap_or(0)) << 32) | u64::from(low.unwrap_or(0))),
        },
        paint_kit_id: snapshot.paint_kit_id,
        paint_seed: snapshot.paint_seed,
        wear: snapshot.wear.filter(|value| value.is_finite()),
        quality: None,
        stattrak: None,
        custom_name: None,
        stickers: snapshot
            .stickers
            .iter()
            .map(|sticker| StickerObservation {
                sticker_id: sticker.id,
                wear: finite(sticker.wear),
                slot: Some(sticker.slot),
                scale: finite(sticker.scale),
                rotation: finite(sticker.rotation),
                offset_x: finite(sticker.offset_x),
                offset_y: finite(sticker.offset_y),
                schema: sticker.schema,
            })
            .collect(),
        keychain,
    })
}

fn finite(value: Option<f32>) -> Option<f32> {
    value.filter(|value| value.is_finite())
}

fn normalized_name(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn sticker_key(sticker: &StickerObservation) -> Option<StickerKey> {
    // CS2 item schema uses a positive sticker kit ID.  Ignore default/missing
    // rows instead of inventing a sticker with ID 0.
    let slot = sticker.slot?;
    (sticker.sticker_id != 0).then(|| StickerKey {
        slot,
        sticker_id: sticker.sticker_id,
        wear_bits: finite(sticker.wear).map(f32::to_bits),
        scale_bits: finite(sticker.scale).map(f32::to_bits),
        rotation_bits: finite(sticker.rotation).map(f32::to_bits),
        offset_x_bits: finite(sticker.offset_x).map(f32::to_bits),
        offset_y_bits: finite(sticker.offset_y).map(f32::to_bits),
        schema: sticker.schema,
    })
}

fn keychain_key(keychain: &KeychainObservation) -> Option<KeychainKey> {
    (keychain.keychain_id != 0).then(|| KeychainKey {
        keychain_id: keychain.keychain_id,
        offset_x_bits: finite(keychain.offset_x).map(f32::to_bits),
        offset_y_bits: finite(keychain.offset_y).map(f32::to_bits),
        offset_z_bits: finite(keychain.offset_z).map(f32::to_bits),
        seed: keychain.seed,
        highlight: keychain.highlight,
        sticker_id: keychain.sticker_id,
        display_case_keychain_id: keychain.display_case_keychain_id,
    })
}

fn weapon_key(observation: Option<&WeaponCosmeticObservation>) -> Option<WeaponCosmeticKey> {
    let observation = observation?;
    let mut stickers = observation
        .stickers
        .iter()
        .filter_map(sticker_key)
        .collect::<Vec<_>>();
    stickers.sort_by_key(|sticker| sticker.slot);
    if stickers.iter().any(|sticker| sticker.slot >= 6)
        || stickers.windows(2).any(|pair| pair[0].slot == pair[1].slot)
    {
        return None;
    }
    // Both fields must be explicitly observed.  Paint kit zero is accepted if
    // it was observed; `None` is the only missing/unknown representation.
    Some(WeaponCosmeticKey {
        item_definition_index: observation.item_definition_index?,
        item_id: observation.item_id,
        paint_kit_id: observation.paint_kit_id?,
        paint_seed: observation.paint_seed,
        wear_bits: finite(observation.wear).map(f32::to_bits),
        quality: observation.quality,
        stattrak: observation.stattrak,
        custom_name: normalized_name(&observation.custom_name),
        stickers,
        keychain: observation.keychain.as_ref().and_then(keychain_key),
    })
}

fn glove_key(observation: Option<&GloveCosmeticObservation>) -> Option<GloveCosmeticKey> {
    let observation = observation?;
    Some(GloveCosmeticKey {
        item_definition_index: observation.item_definition_index?,
        item_id: observation.item_id,
        paint_kit_id: observation.paint_kit_id?,
        paint_seed: observation.paint_seed,
        wear_bits: finite(observation.wear).map(f32::to_bits),
        quality: observation.quality,
    })
}

fn weapon_signature(index: u32, key: &WeaponCosmeticKey) -> WeaponCosmeticSignature {
    WeaponCosmeticSignature {
        index,
        item_definition_index: key.item_definition_index,
        item_id: key.item_id,
        paint_kit_id: key.paint_kit_id,
        paint_seed: key.paint_seed,
        wear: key.wear_bits.map(f32::from_bits),
        quality: key.quality,
        stattrak: key.stattrak,
        custom_name: key.custom_name.clone(),
        stickers: key
            .stickers
            .iter()
            .map(|sticker| StickerSignature {
                sticker_id: sticker.sticker_id,
                wear: sticker.wear_bits.map(f32::from_bits),
                slot: sticker.slot,
                scale: sticker.scale_bits.map(f32::from_bits),
                rotation: sticker.rotation_bits.map(f32::from_bits),
                offset_x: sticker.offset_x_bits.map(f32::from_bits),
                offset_y: sticker.offset_y_bits.map(f32::from_bits),
                schema: sticker.schema,
            })
            .collect(),
        keychain: key.keychain.as_ref().map(|keychain| KeychainSignature {
            keychain_id: keychain.keychain_id,
            offset_x: keychain.offset_x_bits.map(f32::from_bits),
            offset_y: keychain.offset_y_bits.map(f32::from_bits),
            offset_z: keychain.offset_z_bits.map(f32::from_bits),
            seed: keychain.seed,
            highlight: keychain.highlight,
            sticker_id: keychain.sticker_id,
            display_case_keychain_id: keychain.display_case_keychain_id,
        }),
    }
}

fn glove_signature(index: u32, key: &GloveCosmeticKey) -> GloveCosmeticSignature {
    GloveCosmeticSignature {
        index,
        item_definition_index: key.item_definition_index,
        item_id: key.item_id,
        paint_kit_id: key.paint_kit_id,
        paint_seed: key.paint_seed,
        wear: key.wear_bits.map(f32::from_bits),
        quality: key.quality,
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Encode a degree angle into an i16 (clamped before cast).
#[inline]
fn encode_angle(degrees: f32) -> i16 {
    let scaled = degrees * ANGLE_SCALE;
    scaled.round().clamp(i16::MIN as f32, i16::MAX as f32) as i16
}

/// Write exactly 32 bytes as null-padded UTF-8 (truncated if longer than 31 chars).
fn write_name32(w: &mut impl Write, name: &str) -> Result<()> {
    let bytes = name.as_bytes();
    let len = bytes.len().min(31);
    w.write_all(&bytes[..len])?;
    // Zero-pad to exactly 32 bytes
    let padding = 32 - len;
    w.write_all(&vec![0u8; padding])?;
    Ok(())
}

/// Classify utility weapon name to a type byte.
fn util_type_byte(weapon: &str) -> u8 {
    let s = weapon.to_ascii_lowercase();
    if s.contains("flash") {
        1
    } else if s.contains("hegren") || s == "he" {
        2
    } else if s.contains("smoke") {
        3
    } else if s.contains("molotov") || s.contains("incgren") {
        4
    } else if s.contains("decoy") {
        5
    } else {
        0
    }
}

/// Map weapon name to ID using the shared weapon mapper.
fn map_weapon_name_to_id(weapon_name: &str) -> u16 {
    let weapon_map = super::weapon_mapper::create_weapon_name_to_id_map();
    let normalized = weapon_name.trim().to_ascii_lowercase();
    let normalized = normalized.strip_prefix("weapon_").unwrap_or(&normalized);
    weapon_map.get(normalized).copied().unwrap_or(0) as u16
}

fn resolve_frame_weapon_id(recorded_id: &str, weapon_name: &str) -> u16 {
    recorded_id
        .parse::<u16>()
        .ok()
        .filter(|id| *id > 0)
        .unwrap_or_else(|| map_weapon_name_to_id(weapon_name))
}

/// Parse a semicolon-delimited list string like `[45230;45350;45475]` into Vec<i32>.
fn parse_list_i32(s: &str) -> Vec<i32> {
    s.trim_matches(|c| c == '[' || c == ']')
        .split(';')
        .filter_map(|p| p.trim().parse::<i32>().ok())
        .collect()
}

/// Parse a semicolon-delimited list into Vec<u16>.
fn parse_list_u16(s: &str) -> Vec<u16> {
    s.trim_matches(|c| c == '[' || c == ']')
        .split(';')
        .filter_map(|p| p.trim().parse::<u16>().ok())
        .collect()
}

fn stable_player_index_from_entity_index(
    entity_index: i32,
    collection_data: &KillCollectionData,
    steamid_to_idx: &HashMap<u64, usize>,
) -> Option<usize> {
    let entity_index = u32::try_from(entity_index).ok()?;
    let steam_id = collection_data
        .players
        .iter()
        .find(|player| player.killer_index == entity_index)?
        .steam_id;
    steamid_to_idx.get(&steam_id).copied()
}

fn stable_collection_player_indices(
    collection: &Collection,
    collection_data: &KillCollectionData,
    steamid_to_idx: &HashMap<u64, usize>,
) -> Result<(usize, Vec<usize>)> {
    let killer_index = steamid_to_idx
        .get(&collection.steam_id)
        .copied()
        .ok_or_else(|| {
            anyhow!(
                "collection {} killer SteamID {} is absent from the S2R player table",
                collection.collection_num,
                collection.steam_id
            )
        })?;

    let victim_indices = if let Some(details) = collection_data
        .collection_details
        .get(&collection.collection_num)
    {
        details
            .iter()
            .map(|detail| {
                steamid_to_idx
                    .get(&detail.victim_steamid)
                    .copied()
                    .ok_or_else(|| {
                        anyhow!(
                            "collection {} victim SteamID {} is absent from the S2R player table",
                            collection.collection_num,
                            detail.victim_steamid
                        )
                    })
            })
            .collect::<Result<Vec<_>>>()?
    } else {
        parse_list_i32(&collection.victims_index)
            .into_iter()
            .map(|entity_index| {
                stable_player_index_from_entity_index(
                    entity_index,
                    collection_data,
                    steamid_to_idx,
                )
                .ok_or_else(|| {
                    anyhow!(
                        "collection {} victim entity index {} cannot be mapped to the S2R player table",
                        collection.collection_num,
                        entity_index
                    )
                })
            })
            .collect::<Result<Vec<_>>>()?
    };

    Ok((killer_index, victim_indices))
}

// ─── Kill events builder ──────────────────────────────────────────────────────

/// Preserve all deaths, including other killers and world/unresolved attackers.
fn build_round_kill_events(events: &[GameEvent], players: &HashMap<u64, usize>, first: i32, last: i32) -> Vec<KillEvent> {
    events.iter().filter(|e| e.name == "player_death" && e.tick >= first && e.tick <= last).map(|e| {
        let index = |field| {
            let sid = match game_event_field(e, field) {
                Some(Variant::String(s)) => s.parse::<u64>().ok(),
                Some(Variant::U64(s)) => Some(*s), _ => None,
            };
            sid.and_then(|s| players.get(&s).copied()).unwrap_or(255) as u8
        };
        let weapon_id = match game_event_field(e, "weapon") {
            Some(Variant::String(s)) => map_weapon_name_to_id(s), _ => 0,
        };
        let modifiers = KillModifiers::from_event(e);
        KillEvent { tick: e.tick, killer_idx: index("attacker_steamid"), victim_idx: index("user_steamid"),
            weapon_id, kill_flags: modifiers.flags, known_flags: modifiers.known,
            penetrated: modifiers.penetrated }
    }).collect()
}

/// Join collection rows to authoritative deaths by tick AND both identities.
/// Airborne means at the kill tick; it is not an inferred jump-shot classification.
fn enrich_kill_events(
    kills: &mut [KillEvent], events: &[GameEvent], players: &HashMap<u64, usize>,
    records: &HashMap<u64, Vec<TickRecord>>,
) {
    let deaths = build_round_kill_events(events, players, i32::MIN, i32::MAX);
    let deaths: HashMap<_, _> = deaths.iter()
        .map(|e| ((e.tick, e.killer_idx, e.victim_idx), e)).collect();
    let by_index: HashMap<_, _> = players.iter().map(|(sid, idx)| (*idx as u8, *sid)).collect();
    for kill in kills {
        if let Some(death) = deaths.get(&(kill.tick, kill.killer_idx, kill.victim_idx)) {
            kill.kill_flags = death.kill_flags;
            kill.known_flags = death.known_flags;
            kill.penetrated = death.penetrated;
        }
        let mut modifiers = KillModifiers {
            flags: kill.kill_flags, known: kill.known_flags, penetrated: kill.penetrated,
        };
        for (index, bit) in [(kill.killer_idx, ATTACKER_AIRBORNE), (kill.victim_idx, VICTIM_AIRBORNE)] {
            if modifiers.known & bit != 0 || index == 255 { continue; }
            let observation = by_index.get(&index).and_then(|sid| records.get(sid))
                .and_then(|rows| rows.iter().filter(|r| r.alive > 0 && r.tick <= kill.tick
                    && i64::from(kill.tick) - i64::from(r.tick) <= 1).max_by_key(|r| r.tick))
                .and_then(|row| row.state.airborne);
            modifiers.set(bit, observation);
        }
        kill.kill_flags = modifiers.flags;
        kill.known_flags = modifiers.known;
    }
}

fn build_kill_events(
    collection: &Collection,
    collection_data: &KillCollectionData,
    steamid_to_idx: &HashMap<u64, usize>,
) -> Vec<KillEvent> {
    // Prefer per-kill details when available (from CSV collection details section)
    if let Some(details) = collection_data
        .collection_details
        .get(&collection.collection_num)
    {
        return details
            .iter()
            .map(|d| {
                let killer_idx = steamid_to_idx
                    .get(&d.killer_steamid)
                    .copied()
                    .unwrap_or(255) as u8;
                let victim_idx = steamid_to_idx
                    .get(&d.victim_steamid)
                    .copied()
                    .unwrap_or(255) as u8;
                KillEvent {
                    tick: d.kill_tick as i32,
                    killer_idx,
                    victim_idx,
                    weapon_id: d.player_weapon_id as u16,
                    kill_flags: 0,
                    ..Default::default()
                }
            })
            .collect();
    }

    // Fallback: parse the aggregated string fields on the Collection
    let ticks = parse_list_i32(&collection.kill_ticks);
    let victims = parse_list_i32(&collection.victims_index);
    let weapons = parse_list_u16(&collection.weapons_id);
    let killer_idx = steamid_to_idx
        .get(&collection.steam_id)
        .copied()
        .unwrap_or(0) as u8;

    ticks
        .iter()
        .enumerate()
        .map(|(i, &tick)| KillEvent {
            tick,
            killer_idx,
            victim_idx: victims
                .get(i)
                .and_then(|entity_index| {
                    stable_player_index_from_entity_index(
                        *entity_index,
                        collection_data,
                        steamid_to_idx,
                    )
                })
                .unwrap_or(255) as u8,
            weapon_id: weapons.get(i).copied().unwrap_or(0),
            kill_flags: 0,
            ..Default::default()
        })
        .collect()
}

// ─── Meta JSON builder ────────────────────────────────────────────────────────

fn build_meta_json(
    collection: &Collection,
    collection_data: &KillCollectionData,
    steamid_to_idx: &HashMap<u64, usize>,
    grouped_records: &HashMap<u64, Vec<TickRecord>>,
    grenade_stats: &GrenadeStats,
    weapon_switch_ticks: &[i32],
    pad_ticks: i32,
    cosmetic_catalog: &CosmeticCatalog,
) -> Result<String> {
    let (killer_index, victim_indices) =
        stable_collection_player_indices(collection, collection_data, steamid_to_idx)?;
    let victims_index = format!(
        "[{}]",
        victim_indices
            .iter()
            .map(usize::to_string)
            .collect::<Vec<_>>()
            .join(";")
    );

    // Build player list ordered by index.
    // Normalise team to "T"/"CT"/"?" here — PlayerInfo.team may hold a raw numeric
    // string ("2"/"3") or "Unknown" depending on the CSV parsing path. The reader
    // should never have to guess the team format.
    //
    // PRIORITY:
    // 1. CSV team (if valid T/CT)
    // 2. Demo-parsed team from TickRecords (fallback when CSV has no valid team)
    // 3. Default to "?" (unknown)
    let mut players: Vec<serde_json::Value> = collection_data
        .players
        .iter()
        .filter_map(|p| {
            steamid_to_idx.get(&p.steam_id).map(|&idx| {
                // Try CSV team first
                let csv_team = match p.team.trim() {
                    "T" | "2" | "Terrorist" => Some("T"),
                    "CT" | "3" | "Counter-Terrorist" => Some("CT"),
                    _ => None,
                };

                // Fallback to demo-parsed team from TickRecords
                let team_str = csv_team
                    .or_else(|| {
                        grouped_records
                            .get(&p.steam_id)
                            .and_then(|records| records.first())
                            .map(|rec| rec.team.as_str())
                            .and_then(|t| match t {
                                "T" | "2" | "Terrorist" => Some("T"),
                                "CT" | "3" | "Counter-Terrorist" => Some("CT"),
                                _ => None,
                            })
                    })
                    .unwrap_or("?");

                serde_json::json!({
                    "steamid": p.steam_id.to_string(),
                    "name":    p.player_name,
                    "index":   idx,
                    "team":    team_str,
                })
            })
        })
        .collect();
    players.sort_by_key(|v| v["index"].as_u64().unwrap_or(0));

    let padding_value = if pad_ticks == 0 { -1 } else { pad_ticks };

    let mut meta = serde_json::json!({
        "version":                  1,
        "collection_type":          collection.collection_type,
        "collection_num":           collection.collection_num,
        "tick_duration":            collection.tick_duration,
        "map_name":                 collection.map_name,
        "game_version":             collection_data.demo_info.game_version,
        "killer_index":             killer_index,
        "killer_team":              collection.killer_team,
        "start_tick":               collection.start_kill_tick,
        "end_tick":                 collection.end_kill_tick,
        "killer_name":              collection.killer_name,
        "killer_steamid":           collection.steam_id.to_string(),
        "demo_name":                collection.demo_name,
        "folder":                   collection.folder,
        "killer_radius":            collection.killer_radius,
        "victims_radius":           collection.victims_radius,
        "killer_move_distance":     collection.killer_move_distance,
        "victim_team":              collection.victim_team,
        "round_start_tick":         collection.round_start_tick,
        "round_end_tick":           collection.round_end_tick,
        "round_freeze_end":         collection.round_freeze_end,
        "round":                    collection.round,
        "weapons":                  collection.weapons,
        "weapons_id":               collection.weapons_id,
        "kill_ticks":               collection.kill_ticks,
        "victims_index":            victims_index,
        "weapon_switch_ticks":      weapon_switch_ticks,
        "padding":                  padding_value,
        "ticks_tracked":            collection.tick_duration,
        "parsed":                   1u32,
        "hits":                     grenade_stats.hits,
        "misses":                   grenade_stats.misses,
        "hit_rate":                 grenade_stats.hit_rate,
        "util_thrown":              grenade_stats.util_thrown,
        "util_thrown_ticks":        grenade_stats.util_thrown_ticks,
        "util_land_ticks":          grenade_stats.util_land_ticks,
        "weapons_damaged":          grenade_stats.weapons_damaged,
        "weapons_damaged_num_hits": grenade_stats.weapons_damaged_num_hits,
        "total_ticks":              collection_data.demo_info.total_ticks,
        "players":                  players,
    });
    // Keep this assignment outside the already-large `json!` invocation. It
    // also makes it clear that cosmetics is an object/arrays, not an encoded
    // JSON string in metadata.
    meta["cosmetics"] = serde_json::to_value(cosmetic_catalog.metadata())?;
    Ok(serde_json::to_string(&meta)?)
}

// ─── Main writer ──────────────────────────────────────────────────────────────

// ─── v7 raw-audio section ───────────────────────────────────────────────────

// Every record is `[tag:u8, flags:u8=0, reserved:u16=0, payload_len:u32,
// payload]`. The payload begins with the 16-byte common provenance prefix:
// tick:i32, demo_frame_offset:u64, network_message_index:u32. The outer record
// length makes unknown future tags safely skippable without changing v5/v6
// sections or their offsets.
const AUDIO_TAG_SVC_SOUNDS: u8 = 1;
const AUDIO_TAG_SVC_STOP_SOUND: u8 = 2;
const AUDIO_TAG_SOS_START: u8 = 3;
const AUDIO_TAG_SOS_STOP: u8 = 4;
const AUDIO_TAG_SOS_STOP_HASH: u8 = 5;
const AUDIO_TAG_SOS_SET_PARAMS: u8 = 6;
const AUDIO_TAG_SOS_SET_LIBRARY_STACK_FIELDS: u8 = 7;

fn push_len_prefixed(bytes: &mut Vec<u8>, value: &[u8]) -> Result<()> {
    let len = u32::try_from(value.len()).map_err(|_| anyhow!("audio byte payload exceeds u32"))?;
    bytes.extend_from_slice(&len.to_le_bytes());
    bytes.extend_from_slice(value);
    Ok(())
}

fn push_svc_sound_entry(bytes: &mut Vec<u8>, sound: &SvcSoundEntry) {
    let mut presence = 0_u32;
    let fields = [
        sound.origin_x.is_some(),
        sound.origin_y.is_some(),
        sound.origin_z.is_some(),
        sound.volume.is_some(),
        sound.delay_value.is_some(),
        sound.sequence_number.is_some(),
        sound.entity_index.is_some(),
        sound.channel.is_some(),
        sound.pitch.is_some(),
        sound.flags.is_some(),
        sound.sound_num.is_some(),
        sound.sound_num_handle.is_some(),
        sound.speaker_entity.is_some(),
        sound.random_seed.is_some(),
        sound.sound_level.is_some(),
        sound.is_sentence.is_some(),
        sound.is_ambient.is_some(),
        sound.guid.is_some(),
        sound.sound_resource_id.is_some(),
    ];
    for (bit, present) in fields.into_iter().enumerate() {
        if present {
            presence |= 1 << bit;
        }
    }
    bytes.extend_from_slice(&presence.to_le_bytes());
    if let Some(v) = sound.origin_x {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    if let Some(v) = sound.origin_y {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    if let Some(v) = sound.origin_z {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    if let Some(v) = sound.volume {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    if let Some(v) = sound.delay_value {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    if let Some(v) = sound.sequence_number {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    if let Some(v) = sound.entity_index {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    if let Some(v) = sound.channel {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    if let Some(v) = sound.pitch {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    if let Some(v) = sound.flags {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    if let Some(v) = sound.sound_num {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    if let Some(v) = sound.sound_num_handle {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    if let Some(v) = sound.speaker_entity {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    if let Some(v) = sound.random_seed {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    if let Some(v) = sound.sound_level {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    if let Some(v) = sound.is_sentence {
        bytes.push(u8::from(v));
    }
    if let Some(v) = sound.is_ambient {
        bytes.push(u8::from(v));
    }
    if let Some(v) = sound.guid {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    if let Some(v) = sound.sound_resource_id {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
}

fn encode_audio_payload(event: &AudioEvent) -> Result<(u8, Vec<u8>)> {
    let mut bytes = Vec::with_capacity(AUDIO_COMMON_SIZE + 48);
    bytes.extend_from_slice(&event.tick.to_le_bytes());
    bytes.extend_from_slice(&event.order.demo_frame_offset.to_le_bytes());
    bytes.extend_from_slice(&event.order.network_message_index.to_le_bytes());
    let tag = match &event.payload {
        AudioEventPayload::SvcSounds {
            reliable_sound,
            sounds,
        } => {
            let mut presence = 0_u8;
            if reliable_sound.is_some() {
                presence |= 1;
            }
            bytes.push(presence);
            if let Some(value) = reliable_sound {
                bytes.push(u8::from(*value));
            }
            let count = u32::try_from(sounds.len())
                .map_err(|_| anyhow!("svc_Sounds entry count exceeds u32"))?;
            bytes.extend_from_slice(&count.to_le_bytes());
            for sound in sounds {
                push_svc_sound_entry(&mut bytes, sound);
            }
            AUDIO_TAG_SVC_SOUNDS
        }
        AudioEventPayload::SvcStopSound { guid } => {
            bytes.push(u8::from(guid.is_some()));
            if let Some(value) = guid {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
            AUDIO_TAG_SVC_STOP_SOUND
        }
        AudioEventPayload::SosStartSoundEvent {
            soundevent_guid,
            soundevent_hash,
            source_entity_index,
            seed,
            packed_params,
            start_time,
        } => {
            let values = [
                soundevent_guid.is_some(),
                soundevent_hash.is_some(),
                source_entity_index.is_some(),
                seed.is_some(),
                packed_params.is_some(),
                start_time.is_some(),
            ];
            let mut presence = 0_u8;
            for (bit, present) in values.into_iter().enumerate() {
                if present {
                    presence |= 1 << bit;
                }
            }
            bytes.push(presence);
            if let Some(value) = soundevent_guid {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
            if let Some(value) = soundevent_hash {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
            if let Some(value) = source_entity_index {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
            if let Some(value) = seed {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
            if let Some(value) = packed_params {
                push_len_prefixed(&mut bytes, value)?;
            }
            if let Some(value) = start_time {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
            AUDIO_TAG_SOS_START
        }
        AudioEventPayload::SosStopSoundEvent { soundevent_guid } => {
            bytes.push(u8::from(soundevent_guid.is_some()));
            if let Some(value) = soundevent_guid {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
            AUDIO_TAG_SOS_STOP
        }
        AudioEventPayload::SosStopSoundEventHash {
            soundevent_hash,
            source_entity_index,
        } => {
            let presence = u8::from(soundevent_hash.is_some())
                | (u8::from(source_entity_index.is_some()) << 1);
            bytes.push(presence);
            if let Some(value) = soundevent_hash {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
            if let Some(value) = source_entity_index {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
            AUDIO_TAG_SOS_STOP_HASH
        }
        AudioEventPayload::SosSetSoundEventParams {
            soundevent_guid,
            packed_params,
        } => {
            let presence =
                u8::from(soundevent_guid.is_some()) | (u8::from(packed_params.is_some()) << 1);
            bytes.push(presence);
            if let Some(value) = soundevent_guid {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
            if let Some(value) = packed_params {
                push_len_prefixed(&mut bytes, value)?;
            }
            AUDIO_TAG_SOS_SET_PARAMS
        }
        AudioEventPayload::SosSetLibraryStackFields {
            stack_hash,
            packed_fields,
        } => {
            let presence =
                u8::from(stack_hash.is_some()) | (u8::from(packed_fields.is_some()) << 1);
            bytes.push(presence);
            if let Some(value) = stack_hash {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
            if let Some(value) = packed_fields {
                push_len_prefixed(&mut bytes, value)?;
            }
            AUDIO_TAG_SOS_SET_LIBRARY_STACK_FIELDS
        }
    };
    Ok((tag, bytes))
}

fn encode_audio_block(events: &[AudioEvent]) -> Result<Vec<u8>> {
    let count =
        u32::try_from(events.len()).map_err(|_| anyhow!("audio event count exceeds u32"))?;
    let mut ordered = events.to_vec();
    ordered.sort_by_key(AudioEvent::sort_key);
    if ordered
        .windows(2)
        .any(|window| window[0].sort_key() == window[1].sort_key())
    {
        return Err(anyhow!("audio events have duplicate source-order keys"));
    }
    let mut bytes =
        Vec::with_capacity(4 + ordered.len() * (AUDIO_EVENT_HEADER_SIZE + AUDIO_COMMON_SIZE));
    bytes.extend_from_slice(&count.to_le_bytes());
    for event in &ordered {
        let (tag, payload) = encode_audio_payload(event)?;
        let payload_len =
            u32::try_from(payload.len()).map_err(|_| anyhow!("audio record exceeds u32"))?;
        bytes.push(tag);
        bytes.push(0); // record flags reserved for an additive future revision
        bytes.extend_from_slice(&0_u16.to_le_bytes());
        bytes.extend_from_slice(&payload_len.to_le_bytes());
        bytes.extend_from_slice(&payload);
    }
    Ok(bytes)
}

/// Structural validator used by producer tests and future readers. It can skip
/// unknown tags because each record is length-delimited; it does not decode
/// opaque payload bytes.
pub fn validate_audio_block(bytes: &[u8]) -> Result<()> {
    if bytes.len() < 4 {
        return Err(anyhow!("audio block is missing event count"));
    }
    let count = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;
    let mut cursor = 4_usize;
    let mut last_order = None;
    for _ in 0..count {
        let header_end = cursor
            .checked_add(AUDIO_EVENT_HEADER_SIZE)
            .ok_or_else(|| anyhow!("audio record header overflow"))?;
        if header_end > bytes.len() {
            return Err(anyhow!("audio record header is truncated"));
        }
        let tag = bytes[cursor];
        if bytes[cursor + 1] != 0 || bytes[cursor + 2..cursor + 4] != [0, 0] {
            return Err(anyhow!("audio record has nonzero reserved header bytes"));
        }
        let payload_len =
            u32::from_le_bytes(bytes[cursor + 4..cursor + 8].try_into().unwrap()) as usize;
        let payload_end = header_end
            .checked_add(payload_len)
            .ok_or_else(|| anyhow!("audio record length overflow"))?;
        if payload_end > bytes.len() {
            return Err(anyhow!("audio record payload is truncated"));
        }
        // Future tags are skipped exclusively by their outer length. Known
        // tags share the common source prefix and have their current shape
        // checked so corrupt presence masks cannot masquerade as valid data.
        if (AUDIO_TAG_SVC_SOUNDS..=AUDIO_TAG_SOS_SET_LIBRARY_STACK_FIELDS).contains(&tag) {
            let payload = &bytes[header_end..payload_end];
            let order = validate_known_audio_payload(tag, payload)?;
            if last_order >= Some(order) {
                return Err(anyhow!("audio records are not strictly source ordered"));
            }
            last_order = Some(order);
        }
        cursor = payload_end;
    }
    if cursor != bytes.len() {
        return Err(anyhow!("audio block has trailing bytes"));
    }
    Ok(())
}

fn take_audio<'a>(bytes: &'a [u8], cursor: &mut usize, count: usize) -> Result<&'a [u8]> {
    let end = cursor
        .checked_add(count)
        .ok_or_else(|| anyhow!("audio payload length overflow"))?;
    if end > bytes.len() {
        return Err(anyhow!("audio payload is truncated"));
    }
    let value = &bytes[*cursor..end];
    *cursor = end;
    Ok(value)
}

fn take_audio_blob(bytes: &[u8], cursor: &mut usize) -> Result<()> {
    let len = u32::from_le_bytes(take_audio(bytes, cursor, 4)?.try_into().unwrap()) as usize;
    take_audio(bytes, cursor, len)?;
    Ok(())
}

fn validate_known_audio_payload(tag: u8, bytes: &[u8]) -> Result<AudioEventOrder> {
    if bytes.len() < AUDIO_COMMON_SIZE {
        return Err(anyhow!("known audio payload is missing common provenance"));
    }
    let order = AudioEventOrder {
        demo_frame_offset: u64::from_le_bytes(bytes[4..12].try_into().unwrap()),
        network_message_index: u32::from_le_bytes(bytes[12..16].try_into().unwrap()),
    };
    let mut cursor = AUDIO_COMMON_SIZE;
    let presence = take_audio(bytes, &mut cursor, 1)?[0];
    match tag {
        AUDIO_TAG_SVC_SOUNDS => {
            if presence & !1 != 0 {
                return Err(anyhow!("svc_Sounds has unknown presence bits"));
            }
            if presence & 1 != 0 {
                take_audio(bytes, &mut cursor, 1)?;
            }
            let count =
                u32::from_le_bytes(take_audio(bytes, &mut cursor, 4)?.try_into().unwrap()) as usize;
            for _ in 0..count {
                let fields =
                    u32::from_le_bytes(take_audio(bytes, &mut cursor, 4)?.try_into().unwrap());
                if fields >> 19 != 0 {
                    return Err(anyhow!("svc_Sounds entry has unknown presence bits"));
                }
                // All scalar fields except bools are four bytes; the final
                // resource-id field is u64. This mirrors push_svc_sound_entry.
                for bit in 0..15 {
                    if fields & (1 << bit) != 0 {
                        take_audio(bytes, &mut cursor, 4)?;
                    }
                }
                for bit in 15..17 {
                    if fields & (1 << bit) != 0 {
                        take_audio(bytes, &mut cursor, 1)?;
                    }
                }
                if fields & (1 << 17) != 0 {
                    take_audio(bytes, &mut cursor, 4)?;
                }
                if fields & (1 << 18) != 0 {
                    take_audio(bytes, &mut cursor, 8)?;
                }
            }
        }
        AUDIO_TAG_SVC_STOP_SOUND | AUDIO_TAG_SOS_STOP => {
            if presence & !1 != 0 {
                return Err(anyhow!("audio payload has unknown presence bits"));
            }
            if presence & 1 != 0 {
                take_audio(bytes, &mut cursor, 4)?;
            }
        }
        AUDIO_TAG_SOS_START => {
            if presence & !0x3f != 0 {
                return Err(anyhow!("SOS start has unknown presence bits"));
            }
            for bit in 0..4 {
                if presence & (1 << bit) != 0 {
                    take_audio(bytes, &mut cursor, 4)?;
                }
            }
            if presence & (1 << 4) != 0 {
                take_audio_blob(bytes, &mut cursor)?;
            }
            if presence & (1 << 5) != 0 {
                take_audio(bytes, &mut cursor, 4)?;
            }
        }
        AUDIO_TAG_SOS_STOP_HASH
        | AUDIO_TAG_SOS_SET_PARAMS
        | AUDIO_TAG_SOS_SET_LIBRARY_STACK_FIELDS => {
            if presence & !3 != 0 {
                return Err(anyhow!("audio payload has unknown presence bits"));
            }
            if presence & 1 != 0 {
                take_audio(bytes, &mut cursor, 4)?;
            }
            if presence & 2 != 0 {
                if tag == AUDIO_TAG_SOS_STOP_HASH {
                    take_audio(bytes, &mut cursor, 4)?;
                } else {
                    take_audio_blob(bytes, &mut cursor)?;
                }
            }
        }
        _ => unreachable!("known tag range was checked by caller"),
    }
    if cursor != bytes.len() {
        return Err(anyhow!("audio payload has trailing bytes"));
    }
    Ok(order)
}

/// Return the v7 raw-audio block from an in-memory S2R file. v6 and earlier
/// have no audio offset and deliberately remain readable as `None`.
pub fn audio_block_from_s2r_bytes(bytes: &[u8]) -> Result<Option<&[u8]>> {
    if bytes.len() < HEADER_SIZE as usize || &bytes[0..4] != S2R_MAGIC {
        return Err(anyhow!("not an S2R header"));
    }
    let version = u16::from_le_bytes(bytes[4..6].try_into().unwrap());
    if version <= S2R_V6_VERSION {
        return Ok(None);
    }
    let offset = u32::from_le_bytes(
        bytes[AUDIO_OFFSET_HEADER_BYTE..AUDIO_OFFSET_HEADER_BYTE + 4]
            .try_into()
            .unwrap(),
    ) as usize;
    if offset < HEADER_SIZE as usize || offset > bytes.len() {
        return Err(anyhow!("S2R audio offset is outside the file"));
    }
    let end = if version >= S2R_V8_VERSION {
        u32::from_le_bytes(
            bytes[SMOKE_OFFSET_HEADER_BYTE..SMOKE_OFFSET_HEADER_BYTE + 4]
                .try_into()
                .unwrap(),
        ) as usize
    } else {
        bytes.len()
    };
    if end < offset || end > bytes.len() {
        return Err(anyhow!("S2R smoke offset is outside the file"));
    }
    let block = &bytes[offset..end];
    validate_audio_block(block)?;
    Ok(Some(block))
}

fn encode_smoke_block(tracks: &[SmokeVoxelTrack], min_tick: i32, max_tick: i32) -> Result<Vec<u8>> {
    let mut included: Vec<(&SmokeVoxelTrack, Vec<_>)> = Vec::new();
    for track in tracks {
        if track.start_tick > max_tick || track.end_tick < min_tick || track.frames.is_empty() {
            continue;
        }
        // Keep the lifetime prefix: frame payloads are append-only and the decoder needs the
        // preceding chunks to reconstruct the occupancy state visible at the collection start.
        let frames: Vec<_> = track
            .frames
            .iter()
            .take_while(|frame| frame.tick <= max_tick)
            .collect();
        if !frames.is_empty() {
            included.push((track, frames));
        }
    }

    let mut bytes = Vec::new();
    bytes.extend_from_slice(&u32::try_from(included.len())?.to_le_bytes());
    for (track, frames) in included {
        bytes.extend_from_slice(&track.entity_id.to_le_bytes());
        bytes.extend_from_slice(&track.life_index.to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&u32::try_from(frames.len())?.to_le_bytes());
        bytes.extend_from_slice(&track.start_tick.to_le_bytes());
        bytes.extend_from_slice(&track.end_tick.min(max_tick).to_le_bytes());
        for frame in frames {
            bytes.extend_from_slice(&frame.tick.to_le_bytes());
            bytes.extend_from_slice(&frame.update.to_le_bytes());
            bytes.push(frame.flags);
            bytes.extend_from_slice(&[0u8; 3]);
            bytes.extend_from_slice(&frame.effect_tick_begin.to_le_bytes());
            for value in frame.detonation_position {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
            for value in frame.color {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
            bytes.extend_from_slice(&u32::try_from(frame.payload.len())?.to_le_bytes());
            bytes.extend_from_slice(&frame.payload);
        }
    }
    Ok(bytes)
}

#[derive(Clone)]
struct WeaponLifetime<'a> {
    id: u32,
    rows: Vec<&'a WeaponEntitySnapshot>,
}

fn authority_weapon_lifetimes<'a>(
    snapshots: &'a [WeaponEntitySnapshot],
    min_tick: i32,
    max_tick: i32,
) -> Vec<WeaponLifetime<'a>> {
    let mut grouped: BTreeMap<(i32, u32), Vec<&WeaponEntitySnapshot>> = BTreeMap::new();
    for row in snapshots
        .iter()
        .filter(|row| row.tick >= min_tick && row.tick <= max_tick)
    {
        grouped
            .entry((row.entity_id, row.entity_serial))
            .or_default()
            .push(row);
    }
    let mut groups: Vec<_> = grouped.into_values().collect();
    for rows in &mut groups {
        rows.sort_by_key(|row| (row.tick, row.source_order, !row.present));
        rows.dedup();
    }
    groups.sort_by_key(|rows| {
        let first = rows[0];
        (
            first.tick,
            first.source_order,
            first.entity_id,
            first.entity_serial,
        )
    });
    groups
        .into_iter()
        .enumerate()
        .map(|(index, rows)| WeaponLifetime {
            id: u32::try_from(index + 1).unwrap_or(u32::MAX),
            rows,
        })
        .collect()
}

fn encode_agent_lives(
    sorted_steamids: &[u64],
    grouped_records: &HashMap<u64, Vec<TickRecord>>,
    ag2_recipes: &[parser::second_pass::ag2_recipes::Ag2RecipeSnapshot],
    max_tick: i32,
) -> Result<(Vec<u8>, u32)> {
    #[derive(Clone, Copy)]
    struct Life {
        player: u8,
        ordinal: u16,
        start: i32,
        end: i32,
        pawn: Option<u32>,
        serial: Option<u32>,
        agent: Option<u32>,
    }
    // Trimmed donor clips sometimes retain one live player but omit the controller's pawn
    // handle from the dense player rows. Fill that gap only when the entire observed interval
    // identifies exactly one player pawn (entity index AND lifecycle serial). In a normal
    // multi-player demo, or where multiple AG2 pawn identities are present, leave it unknown.
    let ag2_identities: BTreeSet<(u32, u32)> = ag2_recipes.iter()
        .map(|row| (row.entity_id, row.entity_serial)).collect();
    let unique_ag2_identity = (ag2_identities.len() == 1)
        .then(|| *ag2_identities.iter().next().unwrap());
    let mut live_players_at_tick: HashMap<i32, usize> = HashMap::new();
    for rows in grouped_records.values() {
        for row in rows.iter().filter(|row| row.alive > 0) {
            *live_players_at_tick.entry(row.tick).or_default() += 1;
        }
    }
    let mut lives = Vec::new();
    for (player, steamid) in sorted_steamids.iter().enumerate() {
        let mut rows: Vec<_> = grouped_records.get(steamid).into_iter().flatten().collect();
        rows.sort_by_key(|row| row.tick);
        let mut current: Option<Life> = None;
        let mut ordinal = 0u16;
        for row in rows {
            if row.alive == 0 {
                if let Some(mut life) = current.take() {
                    life.end = row.tick;
                    lives.push(life);
                }
                continue;
            }
            let inferred_identity = (live_players_at_tick.get(&row.tick) == Some(&1))
                .then_some(unique_ag2_identity).flatten();
            let pawn = row.pawn_entity_id.or(inferred_identity.map(|identity| identity.0));
            let serial = pawn.and_then(|entity_id| {
                let mut serials = ag2_identities.iter()
                    .filter(|(candidate, _)| *candidate == entity_id)
                    .map(|(_, serial)| *serial);
                let serial = serials.next()?;
                serials.next().is_none().then_some(serial)
            });
            let changed = current
                .map(|life| {
                    life.pawn != pawn || life.agent != row.agent_definition_index
                        || matches!((life.serial, serial), (Some(old), Some(new)) if old != new)
                })
                .unwrap_or(true);
            if changed {
                if let Some(mut life) = current.take() {
                    life.end = row.tick;
                    lives.push(life);
                }
                current = Some(Life {
                    player: player as u8,
                    ordinal,
                    start: row.tick,
                    end: max_tick.saturating_add(1),
                    pawn,
                    serial,
                    agent: row.agent_definition_index,
                });
                ordinal = ordinal.saturating_add(1);
            }
        }
        if let Some(life) = current {
            lives.push(life);
        }
    }
    let count = u32::try_from(lives.len())?;
    let mut bytes = Vec::with_capacity(4 + lives.len() * 24);
    bytes.extend_from_slice(&count.to_le_bytes());
    for life in lives {
        bytes.push(life.player);
        bytes.push(0);
        bytes.extend_from_slice(&life.ordinal.to_le_bytes());
        bytes.extend_from_slice(&life.start.to_le_bytes());
        bytes.extend_from_slice(&life.end.to_le_bytes());
        bytes.extend_from_slice(&life.pawn.unwrap_or(u32::MAX).to_le_bytes());
        bytes.extend_from_slice(&life.agent.unwrap_or(u32::MAX).to_le_bytes());
        bytes.extend_from_slice(&life.serial.unwrap_or(u32::MAX).to_le_bytes());
    }
    Ok((bytes, count))
}

fn encode_weapon_lifetime_table(
    lifetimes: &[WeaponLifetime<'_>],
    catalog: &CosmeticCatalog,
    max_tick: i32,
) -> Result<(Vec<u8>, u32)> {
    let count = u32::try_from(lifetimes.len())?;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&count.to_le_bytes());
    for life in lifetimes {
        let first = life.rows[0];
        let end = life
            .rows
            .iter()
            .find(|row| !row.present)
            .map(|row| row.tick.saturating_add(1))
            .unwrap_or_else(|| max_tick.saturating_add(1));
        let item_definition = life
            .rows
            .iter()
            .find_map(|row| row.item_definition_index)
            .unwrap_or(u32::MAX);
        let item_id = life
            .rows
            .iter()
            .find_map(|row| match (row.item_id_high, row.item_id_low) {
                (None, None) => None,
                (high, low) => {
                    Some((u64::from(high.unwrap_or(0)) << 32) | u64::from(low.unwrap_or(0)))
                }
            })
            .unwrap_or(0);
        let cosmetic = life
            .rows
            .iter()
            .find_map(|row| {
                let observation = weapon_snapshot_cosmetic(row);
                let index = catalog.weapon_index(observation.as_ref());
                (index != 0).then_some(index)
            })
            .unwrap_or(0);
        let class_name = life
            .rows
            .iter()
            .map(|row| row.class_name.as_str())
            .find(|name| !name.is_empty())
            .unwrap_or("");
        let class_bytes = class_name.as_bytes();
        let class_len = u16::try_from(class_bytes.len())?;
        bytes.extend_from_slice(&life.id.to_le_bytes());
        bytes.extend_from_slice(&first.entity_id.to_le_bytes());
        bytes.extend_from_slice(&first.entity_serial.to_le_bytes());
        bytes.extend_from_slice(&first.tick.to_le_bytes());
        bytes.extend_from_slice(&end.to_le_bytes());
        bytes.extend_from_slice(&item_definition.to_le_bytes());
        bytes.extend_from_slice(&cosmetic.to_le_bytes());
        bytes.extend_from_slice(&item_id.to_le_bytes());
        bytes.extend_from_slice(&class_len.to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(class_bytes);
    }
    Ok((bytes, count))
}

fn is_unowned_weapon(row: &WeaponEntitySnapshot) -> bool {
    row.owner_steamid.is_none()
        && row
            .current_owner_handle
            .map(|handle| handle == 0 || handle == u32::MAX || (handle & 0x3fff) == 0x3fff)
            .unwrap_or(true)
}

fn encode_inventory_deltas(
    lifetimes: &[WeaponLifetime<'_>],
    steamid_to_idx: &HashMap<u64, usize>,
) -> Result<(Vec<u8>, u32)> {
    let mut records = Vec::new();
    for life in lifetimes {
        let mut previous: Option<(u8, i8, u8, i16, i16, i16)> = None;
        for row in &life.rows {
            let owner = row
                .owner_steamid
                .and_then(|steamid| steamid_to_idx.get(&steamid).copied())
                .and_then(|index| u8::try_from(index).ok())
                .unwrap_or(255);
            let mut flags = 0u8;
            if row.owner_steamid.is_some() {
                flags |= 1 << 0;
            }
            if row.active == Some(true) {
                flags |= 1 << 1;
            }
            if row.active.is_some() {
                flags |= 1 << 2;
            }
            if is_unowned_weapon(row) {
                flags |= 1 << 3;
            }
            if !row.present {
                flags |= 1 << 4;
            }
            let slot = row
                .inventory_slot
                .and_then(|value| i8::try_from(value).ok())
                .unwrap_or(i8::MIN);
            let clip = row
                .clip_ammo
                .and_then(|value| i16::try_from(value).ok())
                .unwrap_or(i16::MIN);
            let reserve = row
                .reserve_ammo
                .and_then(|value| i16::try_from(value).ok())
                .unwrap_or(i16::MIN);
            let econ = row
                .econ_inventory_position
                .and_then(|value| i16::try_from(value).ok())
                .unwrap_or(i16::MIN);
            let state = (owner, slot, flags, clip, reserve, econ);
            if previous == Some(state) {
                continue;
            }
            previous = Some(state);
            records.extend_from_slice(&row.tick.to_le_bytes());
            records.extend_from_slice(&life.id.to_le_bytes());
            records.push(owner);
            records.push(slot as u8);
            records.push(flags);
            records.push(0);
            records.extend_from_slice(&clip.to_le_bytes());
            records.extend_from_slice(&reserve.to_le_bytes());
            records.extend_from_slice(&econ.to_le_bytes());
            records.extend_from_slice(&0i16.to_le_bytes());
        }
    }
    let count = u32::try_from(records.len() / 20)?;
    let mut bytes = Vec::with_capacity(4 + records.len());
    bytes.extend_from_slice(&count.to_le_bytes());
    bytes.extend_from_slice(&records);
    Ok((bytes, count))
}

fn encode_world_weapon_deltas(lifetimes: &[WeaponLifetime<'_>]) -> Result<(Vec<u8>, u32)> {
    let mut records = Vec::new();
    for life in lifetimes {
        let mut previous_world = false;
        let mut previous_state: Option<(Option<[f32; 3]>, Option<[f32; 3]>, Option<u8>)> = None;
        let mut previous_position: Option<(i32, [f32; 3])> = None;
        for row in &life.rows {
            let world = row.present && is_unowned_weapon(row);
            let deleting = !row.present || (previous_world && !world);
            if !world && !deleting {
                previous_world = false;
                continue;
            }
            let state = (row.position, row.rotation, row.pvs_state);
            if world && previous_world && previous_state == Some(state) {
                continue;
            }
            let operation = if deleting {
                2
            } else if !previous_world {
                1
            } else {
                0
            };
            let mut flags = 0u8;
            if row.position.is_some() {
                flags |= 1 << 0;
            }
            if row.rotation.is_some() {
                flags |= 1 << 1;
            }
            if row.pvs_state.is_some() {
                flags |= 1 << 2;
            }
            let velocity = match (previous_position, row.position) {
                (Some((tick, old)), Some(position)) if row.tick > tick => {
                    let dt = (row.tick - tick) as f32;
                    flags |= 1 << 3;
                    [
                        (position[0] - old[0]) / dt,
                        (position[1] - old[1]) / dt,
                        (position[2] - old[2]) / dt,
                    ]
                }
                _ => [f32::NAN; 3],
            };
            records.extend_from_slice(&row.tick.to_le_bytes());
            records.extend_from_slice(&life.id.to_le_bytes());
            records.push(operation);
            records.push(flags);
            records.push(row.pvs_state.unwrap_or(u8::MAX));
            records.push(0);
            for value in row.position.unwrap_or([f32::NAN; 3]) {
                records.extend_from_slice(&value.to_le_bytes());
            }
            for value in row.rotation.unwrap_or([f32::NAN; 3]) {
                records.extend_from_slice(&value.to_le_bytes());
            }
            for value in velocity {
                records.extend_from_slice(&value.to_le_bytes());
            }
            if let Some(position) = row.position {
                previous_position = Some((row.tick, position));
            }
            previous_state = Some(state);
            previous_world = world && !deleting;
        }
    }
    let count = u32::try_from(records.len() / 48)?;
    let mut bytes = Vec::with_capacity(4 + records.len());
    bytes.extend_from_slice(&count.to_le_bytes());
    bytes.extend_from_slice(&records);
    Ok((bytes, count))
}

/// Encode the authoritative fatal-hit data carried by the victim pawn. The
/// death marker may trail the game event by a tick, but consumers join this
/// section to kill events, so the serialized key remains the kill-event tick.
fn selected_dead_ragdoll_record(
    player_records: &[TickRecord],
    kill_tick: i32,
) -> Option<&TickRecord> {
    let sample_end_tick = kill_tick.saturating_add(2);
    player_records
        .iter()
        .filter(|record| {
            record.alive == 0
                && record.tick >= kill_tick
                && record.tick <= sample_end_tick
                && (record.ragdoll_damage_bone.is_some()
                    || record.ragdoll_damage_position.is_some()
                    || record.ragdoll_damage_force.is_some())
        })
        .min_by_key(|record| record.tick)
}

fn encode_ragdoll_impacts(
    sorted_steamids: &[u64],
    grouped_records: &HashMap<u64, Vec<TickRecord>>,
    kill_events: &[KillEvent],
) -> Result<(Vec<u8>, u32)> {
    const ROW_SIZE: usize = 36;
    let mut records = Vec::new();

    for kill in kill_events {
        let Some(&steamid) = sorted_steamids.get(kill.victim_idx as usize) else {
            continue;
        };
        let Some(player_records) = grouped_records.get(&steamid) else {
            continue;
        };
        let Some(sample) = selected_dead_ragdoll_record(player_records, kill.tick) else {
            continue;
        };

        let bone = sample.ragdoll_damage_bone.filter(|bone| *bone >= 0);
        let position = sample
            .ragdoll_damage_position
            .filter(|value| value.iter().all(|component| component.is_finite()));
        let force = sample.ragdoll_damage_force.filter(|value| {
            value.iter().all(|component| component.is_finite())
                && value.iter().any(|component| component.abs() > f32::EPSILON)
        });

        let mut flags = 0u8;
        if bone.is_some() {
            flags |= 1 << 0;
        }
        if position.is_some() {
            flags |= 1 << 1;
        }
        if force.is_some() {
            flags |= 1 << 2;
        }
        if flags == 0 {
            continue;
        }

        records.extend_from_slice(&kill.tick.to_le_bytes());
        records.push(kill.victim_idx);
        records.push(flags);
        records.extend_from_slice(&0u16.to_le_bytes());
        records.extend_from_slice(&bone.unwrap_or(-1).to_le_bytes());
        for value in position.unwrap_or([0.0; 3]) {
            records.extend_from_slice(&value.to_le_bytes());
        }
        for value in force.unwrap_or([0.0; 3]) {
            records.extend_from_slice(&value.to_le_bytes());
        }
    }

    let count = u32::try_from(records.len() / ROW_SIZE)?;
    let mut bytes = Vec::with_capacity(4 + records.len());
    bytes.extend_from_slice(&count.to_le_bytes());
    bytes.extend_from_slice(&records);
    Ok((bytes, count))
}

/// Preserve the selected dead-row tick and nullable server origin as diagnostic provenance.
/// The selection policy intentionally matches `encode_ragdoll_impacts`; this lane is not wired
/// into replay ragdoll construction or physics.
fn encode_death_input_provenance(
    sorted_steamids: &[u64],
    grouped_records: &HashMap<u64, Vec<TickRecord>>,
    kill_events: &[KillEvent],
) -> Result<(Vec<u8>, u32)> {
    const ROW_SIZE: usize = 24;
    let mut rows = Vec::new();

    for kill in kill_events {
        let Some(&steamid) = sorted_steamids.get(kill.victim_idx as usize) else {
            continue;
        };
        let Some(player_records) = grouped_records.get(&steamid) else {
            continue;
        };
        let Some(sample) = selected_dead_ragdoll_record(player_records, kill.tick) else {
            continue;
        };
        let origin = sample
            .ragdoll_server_origin
            .filter(|value| value.iter().all(|component| component.is_finite()));

        rows.extend_from_slice(&kill.tick.to_le_bytes());
        rows.extend_from_slice(&sample.tick.to_le_bytes());
        rows.push(kill.victim_idx);
        rows.push(u8::from(origin.is_some()));
        rows.extend_from_slice(&0u16.to_le_bytes());
        for value in origin.unwrap_or([0.0; 3]) {
            rows.extend_from_slice(&value.to_le_bytes());
        }
        debug_assert_eq!(rows.len() % ROW_SIZE, 0);
    }

    let count = u32::try_from(rows.len() / ROW_SIZE)?;
    let mut bytes = Vec::with_capacity(4 + rows.len());
    bytes.extend_from_slice(&count.to_le_bytes());
    bytes.extend_from_slice(&rows);
    Ok((bytes, count))
}

fn game_event_field<'a>(event: &'a GameEvent, name: &str) -> Option<&'a Variant> {
    event
        .fields
        .iter()
        .find(|field| field.name == name)
        .and_then(|field| field.data.as_ref())
}

fn game_event_world_position(event: &GameEvent) -> Option<[f32; 3]> {
    let component = |name| match game_event_field(event, name) {
        Some(Variant::F32(value)) if value.is_finite() => Some(*value),
        _ => None,
    };
    Some([component("x")?, component("y")?, component("z")?])
}

fn game_event_player_index(event: &GameEvent, steamid_to_idx: &HashMap<u64, usize>) -> u8 {
    let steamid = ["user_steamid", "attacker_steamid"]
        .into_iter()
        .find_map(|name| match game_event_field(event, name) {
            Some(Variant::String(value)) => value.parse::<u64>().ok(),
            Some(Variant::U64(value)) => Some(*value),
            _ => None,
        });
    steamid
        .and_then(|value| steamid_to_idx.get(&value).copied())
        .and_then(|index| u8::try_from(index).ok())
        .unwrap_or(u8::MAX)
}

/// Ordinary `bullet_impact` game events are the authoritative world-space hit
/// points for transient surface deformation. They are deliberately separate
/// from fatal pawn/ragdoll impacts: every valid event in the replay window is
/// preserved, including events whose shooter cannot be attributed.
fn encode_bullet_impacts(
    events: &[GameEvent],
    steamid_to_idx: &HashMap<u64, usize>,
    min_tick: i32,
    max_tick: i32,
) -> Result<(Vec<u8>, u32)> {
    const ROW_SIZE: usize = 20;
    let mut records = Vec::new();
    for event in events.iter().filter(|event| {
        event.name == "bullet_impact" && event.tick >= min_tick && event.tick <= max_tick
    }) {
        let Some(position) = game_event_world_position(event) else {
            continue;
        };
        records.extend_from_slice(&event.tick.to_le_bytes());
        records.push(game_event_player_index(event, steamid_to_idx));
        records.push(0); // flags reserved for schema-1 readers
        records.extend_from_slice(&0u16.to_le_bytes());
        for value in position {
            records.extend_from_slice(&value.to_le_bytes());
        }
    }
    let count = u32::try_from(records.len() / ROW_SIZE)?;
    let mut bytes = Vec::with_capacity(4 + records.len());
    bytes.extend_from_slice(&count.to_le_bytes());
    bytes.extend_from_slice(&records);
    Ok((bytes, count))
}

fn grenade_detonation_type(event_name: &str) -> Option<u8> {
    match event_name {
        "flashbang_detonate" => Some(1),
        "hegrenade_detonate" => Some(2),
        "smokegrenade_detonate" => Some(3),
        // Source uses the same fire event for Molotov and incendiary grenades,
        // so schema 1 intentionally exposes the shared utility type.
        "inferno_startburn" => Some(4),
        "decoy_started" => Some(5),
        _ => None,
    }
}

fn game_event_entity_id(event: &GameEvent) -> Option<u32> {
    match game_event_field(event, "entityid") {
        Some(Variant::I32(value)) if *value >= 0 && *value != 2047 => Some(*value as u32),
        Some(Variant::U32(value)) if *value != 2047 && *value != u32::MAX => Some(*value),
        _ => None,
    }
}

/// Authoritative grenade detonation events. Position is mandatory because an
/// incomplete event cannot drive a replay effect; entity identity remains
/// optional and uses the section-wide u32::MAX sentinel when unavailable.
fn encode_grenade_detonations(
    events: &[GameEvent],
    min_tick: i32,
    max_tick: i32,
) -> Result<(Vec<u8>, u32)> {
    const ROW_SIZE: usize = 24;
    let mut records = Vec::new();
    for event in events
        .iter()
        .filter(|event| event.tick >= min_tick && event.tick <= max_tick)
    {
        let Some(grenade_type) = grenade_detonation_type(&event.name) else {
            continue;
        };
        let Some(position) = game_event_world_position(event) else {
            continue;
        };
        let entity_id = game_event_entity_id(event);
        records.extend_from_slice(&event.tick.to_le_bytes());
        records.push(grenade_type);
        records.push(u8::from(entity_id.is_some()));
        records.extend_from_slice(&0u16.to_le_bytes());
        records.extend_from_slice(&entity_id.unwrap_or(u32::MAX).to_le_bytes());
        for value in position {
            records.extend_from_slice(&value.to_le_bytes());
        }
    }
    let count = u32::try_from(records.len() / ROW_SIZE)?;
    let mut bytes = Vec::with_capacity(4 + records.len());
    bytes.extend_from_slice(&count.to_le_bytes());
    bytes.extend_from_slice(&records);
    Ok((bytes, count))
}

fn game_event_u32(event: &GameEvent, name: &str) -> Option<u32> {
    match game_event_field(event, name) {
        Some(Variant::U32(value)) => Some(*value),
        Some(Variant::I32(value)) if *value >= 0 => Some(*value as u32),
        _ => None,
    }
}

fn game_event_i32(event: &GameEvent, name: &str) -> Option<i32> {
    match game_event_field(event, name) {
        Some(Variant::I32(value)) => Some(*value),
        Some(Variant::U32(value)) => i32::try_from(*value).ok(),
        _ => None,
    }
}

fn game_event_f32(event: &GameEvent, name: &str) -> Option<f32> {
    match game_event_field(event, name) {
        Some(Variant::F32(value)) => Some(*value),
        _ => None,
    }
}

fn game_event_bool_byte(event: &GameEvent, name: &str) -> Option<u8> {
    match game_event_field(event, name) {
        Some(Variant::Bool(value)) => Some(u8::from(*value)),
        _ => None,
    }
}

fn game_event_vec3(event: &GameEvent, names: [&str; 3]) -> Option<[f32; 3]> {
    Some([
        game_event_f32(event, names[0])?,
        game_event_f32(event, names[1])?,
        game_event_f32(event, names[2])?,
    ])
}

/// Raw `CMsgTEFireBullets` inputs. This is intentionally not an impact stream:
/// a consumer may derive rays using CS2 collision/spread behavior, but any such
/// endpoint must retain derived provenance. Every source row is serialized in
/// the single-threaded parser's original order without coalescing.
fn encode_fire_bullets_inputs(
    events: &[GameEvent],
    steamid_to_idx: &HashMap<u64, usize>,
    min_tick: i32,
    max_tick: i32,
) -> Result<(Vec<u8>, u32)> {
    const ROW_SIZE: usize = 152;
    const MISSING_F32: f32 = f32::from_bits(0x7fc0_0000);
    let mut records = Vec::new();
    for (source_order, event) in events.iter().enumerate().filter(|(_, event)| {
        event.name == "fire_bullets" && event.tick >= min_tick && event.tick <= max_tick
    }) {
        let player_handle = game_event_u32(event, "player");
        let origin = game_event_vec3(event, ["origin_x", "origin_y", "origin_z"]);
        let angles = game_event_vec3(event, ["angles_x", "angles_y", "angles_z"]);
        let entity_origin =
            game_event_vec3(event, ["ent_origin_x", "ent_origin_y", "ent_origin_z"]);
        let weapon_id = game_event_u32(event, "weapon_id");
        let item_definition = game_event_u32(event, "item_def_index");
        let mode = game_event_u32(event, "mode");
        let attack_type = game_event_u32(event, "attack_type");
        let seed = game_event_u32(event, "seed");
        let inaccuracy = game_event_f32(event, "inaccuracy");
        let recoil_index = game_event_f32(event, "recoil_index");
        let spread = game_event_f32(event, "spread");
        let bullets_remaining = game_event_u32(event, "num_bullets_remaining");
        let message_tick = game_event_i32(event, "message_tick");
        let player_inair = game_event_bool_byte(event, "player_inair");
        let player_scoped = game_event_bool_byte(event, "player_scoped");
        let extra_type = game_event_i32(event, "extra_type");
        let attack_tick_count = game_event_i32(event, "attack_tick_count");
        let attack_tick_fraction = game_event_f32(event, "attack_tick_fraction");
        let render_tick_count = game_event_i32(event, "render_tick_count");
        let render_tick_fraction = game_event_f32(event, "render_tick_fraction");
        let inaccuracy_move = game_event_f32(event, "inaccuracy_move");
        let inaccuracy_air = game_event_f32(event, "inaccuracy_air");
        let aim_punch = game_event_vec3(event, ["aim_punch_x", "aim_punch_y", "aim_punch_z"]);
        let sound_type = game_event_i32(event, "sound_type");
        let sound_dsp_effect = game_event_u32(event, "sound_dsp_effect");

        let presence_values = [
            player_handle.is_some(),
            origin.is_some(),
            angles.is_some(),
            entity_origin.is_some(),
            weapon_id.is_some(),
            item_definition.is_some(),
            mode.is_some(),
            attack_type.is_some(),
            seed.is_some(),
            inaccuracy.is_some(),
            recoil_index.is_some(),
            spread.is_some(),
            bullets_remaining.is_some(),
            message_tick.is_some(),
            player_inair.is_some(),
            player_scoped.is_some(),
            extra_type.is_some(),
            attack_tick_count.is_some(),
            attack_tick_fraction.is_some(),
            render_tick_count.is_some(),
            render_tick_fraction.is_some(),
            inaccuracy_move.is_some(),
            inaccuracy_air.is_some(),
            aim_punch.is_some(),
            sound_type.is_some(),
            sound_dsp_effect.is_some(),
        ];
        let presence = presence_values
            .into_iter()
            .enumerate()
            .fold(0u32, |mask, (bit, present)| {
                mask | (u32::from(present) << bit)
            });
        let shooter = game_event_player_index(event, steamid_to_idx);
        let start = records.len();
        records.extend_from_slice(&event.tick.to_le_bytes());
        records.extend_from_slice(&u32::try_from(source_order)?.to_le_bytes());
        records.extend_from_slice(&presence.to_le_bytes());
        records.push(shooter);
        records.push(u8::from(shooter != u8::MAX));
        records.extend_from_slice(&0u16.to_le_bytes());
        records.extend_from_slice(&player_handle.unwrap_or(u32::MAX).to_le_bytes());
        records.extend_from_slice(&weapon_id.unwrap_or(u32::MAX).to_le_bytes());
        records.extend_from_slice(&item_definition.unwrap_or(u32::MAX).to_le_bytes());
        records.extend_from_slice(&mode.unwrap_or(u32::MAX).to_le_bytes());
        records.extend_from_slice(&attack_type.unwrap_or(u32::MAX).to_le_bytes());
        records.extend_from_slice(&seed.unwrap_or(u32::MAX).to_le_bytes());
        records.extend_from_slice(&bullets_remaining.unwrap_or(u32::MAX).to_le_bytes());
        records.extend_from_slice(&message_tick.unwrap_or(i32::MIN).to_le_bytes());
        for value in origin.unwrap_or([MISSING_F32; 3]) {
            records.extend_from_slice(&value.to_le_bytes());
        }
        for value in angles.unwrap_or([MISSING_F32; 3]) {
            records.extend_from_slice(&value.to_le_bytes());
        }
        for value in entity_origin.unwrap_or([MISSING_F32; 3]) {
            records.extend_from_slice(&value.to_le_bytes());
        }
        records.extend_from_slice(&inaccuracy.unwrap_or(MISSING_F32).to_le_bytes());
        records.extend_from_slice(&recoil_index.unwrap_or(MISSING_F32).to_le_bytes());
        records.extend_from_slice(&spread.unwrap_or(MISSING_F32).to_le_bytes());
        records.push(player_inair.unwrap_or(u8::MAX));
        records.push(player_scoped.unwrap_or(u8::MAX));
        records.extend_from_slice(&0u16.to_le_bytes());
        records.extend_from_slice(&extra_type.unwrap_or(i32::MIN).to_le_bytes());
        records.extend_from_slice(&attack_tick_count.unwrap_or(i32::MIN).to_le_bytes());
        records.extend_from_slice(&attack_tick_fraction.unwrap_or(MISSING_F32).to_le_bytes());
        records.extend_from_slice(&render_tick_count.unwrap_or(i32::MIN).to_le_bytes());
        records.extend_from_slice(&render_tick_fraction.unwrap_or(MISSING_F32).to_le_bytes());
        records.extend_from_slice(&inaccuracy_move.unwrap_or(MISSING_F32).to_le_bytes());
        records.extend_from_slice(&inaccuracy_air.unwrap_or(MISSING_F32).to_le_bytes());
        for value in aim_punch.unwrap_or([MISSING_F32; 3]) {
            records.extend_from_slice(&value.to_le_bytes());
        }
        records.extend_from_slice(&sound_type.unwrap_or(i32::MIN).to_le_bytes());
        records.extend_from_slice(&sound_dsp_effect.unwrap_or(u32::MAX).to_le_bytes());
        records.extend_from_slice(&0u32.to_le_bytes());
        debug_assert_eq!(records.len() - start, ROW_SIZE);
    }
    let count = u32::try_from(records.len() / ROW_SIZE)?;
    let mut bytes = Vec::with_capacity(4 + records.len());
    bytes.extend_from_slice(&count.to_le_bytes());
    bytes.extend_from_slice(&records);
    Ok((bytes, count))
}

fn encode_extension_block(sections: &[(u16, u16, Vec<u8>, u32)]) -> Result<Vec<u8>> {
    let directory_size = 8usize
        .checked_add(
            sections
                .len()
                .checked_mul(16)
                .ok_or_else(|| anyhow!("S2EX directory overflow"))?,
        )
        .ok_or_else(|| anyhow!("S2EX directory overflow"))?;
    let mut offset = u32::try_from(directory_size)?;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(EXTENSION_MAGIC);
    bytes.extend_from_slice(&EXTENSION_SCHEMA_VERSION.to_le_bytes());
    bytes.extend_from_slice(&u16::try_from(sections.len())?.to_le_bytes());
    for (tag, schema, payload, count) in sections {
        bytes.extend_from_slice(&tag.to_le_bytes());
        bytes.extend_from_slice(&schema.to_le_bytes());
        bytes.extend_from_slice(&offset.to_le_bytes());
        bytes.extend_from_slice(&u32::try_from(payload.len())?.to_le_bytes());
        bytes.extend_from_slice(&count.to_le_bytes());
        offset = offset
            .checked_add(u32::try_from(payload.len())?)
            .ok_or_else(|| anyhow!("S2EX payload exceeds u32"))?;
    }
    for (_, _, payload, _) in sections {
        bytes.extend_from_slice(payload);
    }
    Ok(bytes)
}

/// Compatibility entry point for collection-scoped fixtures and consumers.
/// Production full-round output uses `write_replay_s2r` with shared_round=true.
pub fn write_collection_s2r(
    output_path: &Path,
    grouped_records: &HashMap<u64, Vec<TickRecord>>,
    collection: &Collection,
    collection_data: &KillCollectionData,
    utility_thrown: &[UtilityThrown],
    grenade_trajectories: &[GrenadeTrajectory],
    weapon_fire_events: &[WeaponFireEvent],
    grenade_stats: &GrenadeStats,
    weapon_switch_ticks: &[i32],
    pad_ticks: i32,
    audio_events: &[AudioEvent],
    smoke_voxels: &[SmokeVoxelTrack],
    weapon_entities: &[WeaponEntitySnapshot],
    game_events: &[GameEvent],
    skip_buy_time: bool,
    playback_range: Option<(i32, i32)>,
) -> Result<()> {
    write_replay_s2r(output_path, grouped_records, collection, collection_data, utility_thrown, grenade_trajectories, weapon_fire_events, grenade_stats, weapon_switch_ticks, pad_ticks, audio_events, smoke_voxels, weapon_entities, game_events, skip_buy_time, playback_range, false, &[], None, &[], &[])
}

// Retain full history only for lives intersecting the window. Patch ignition age
// can precede min_tick; EOF is not removal and must not impose an invented cap.
fn inferno_rows_for_window(
    rows: &[parser::second_pass::infernos::InfernoPatchRecord], min_tick: i32, max_tick: i32,
) -> Vec<&parser::second_pass::infernos::InfernoPatchRecord> {
    if max_tick < min_tick { return Vec::new(); }
    let mut removed = BTreeMap::new();
    for row in rows.iter().filter(|r| r.flags & 16 != 0) {
        let end = removed.entry((row.entity_id, row.serial, row.start_tick)).or_insert(row.tick);
        *end = (*end).min(row.tick);
    }
    rows.iter().filter(|row| row.tick <= max_tick
        && !removed.get(&(row.entity_id, row.serial, row.start_tick))
            .is_some_and(|end| *end <= min_tick)).collect()
}

/// Encodes the world-entity lane as S2EX tag 13, schema 1.
///
/// Row layout, 56 bytes, little-endian:
///
/// ```text
/// +0   tick:i32
/// +4   entity_id:u32
/// +8   serial:u32
/// +12  class_id:u8        1 door_rotating, 2 breakable, 3 dynamic_prop, 4 physics_prop,
///                         5 func_brush, 6 func_water, 7 button
/// +13  operation:u8       0 spawn, 1 update, 2 delete
/// +14  flags:u16          bit0 origin, bit1 angles, bit2 door_state, bit3 simulation_time,
///                         bit4 model, bit5 dormant (a PVS leave, not a removal)
/// +16  origin:[f32;3]     world position, always present on spawn — the map-binding key
/// +28  angles:[f32;3]
/// +40  simulation_time:f32
/// +44  door_state:u8
/// +45  reserved:[u8;3]
/// +48  model:u64
/// ```
///
/// Every row carries the origin, so a reader binds each row to a map entity on its own without
/// having to walk back to a spawn. Rows are ordered by tick.
fn encode_world_entities(
    deltas: &[parser::second_pass::world_entities::WorldEntityDelta],
    min_tick: i32,
    max_tick: i32,
) -> Result<(Vec<u8>, u32)> {
    use parser::second_pass::world_entities::operation as op;

    let mut rows: Vec<&parser::second_pass::world_entities::WorldEntityDelta> = Vec::new();
    // A map entity is created once per round and the parser replays full packets, so the same
    // door arrives as several identical spawns. Keep one per (class, origin) while it stands: a
    // second spawn of a door already there is not a second door. A spawn *after* that entity was
    // removed is kept, though — a round restart destroys and re-creates the map's entities, and
    // dropping the re-creation would leave the door gone for the rest of the clip.
    let mut standing: Vec<(u8, [f32; 3])> = Vec::new();
    for delta in deltas.iter().filter(|row| row.tick <= max_tick) {
        let same = |entry: &(u8, [f32; 3])| entry.0 == delta.class_id && close(entry.1, delta.origin);
        if delta.operation == op::SPAWN {
            if standing.iter().any(same) {
                continue;
            }
            standing.push((delta.class_id, delta.origin));
            rows.push(delta);
            continue;
        }
        if delta.operation == op::DELETE
            && delta.flags & parser::second_pass::world_entities::field_flag::DORMANT == 0
        {
            standing.retain(|entry| !same(entry));
        }
        // Spawns before the window are kept and clamped: an entity that exists for the whole
        // round still has to be placed. Changes before the window are not — they describe a
        // pose the window never shows.
        if delta.tick >= min_tick {
            rows.push(delta);
        }
    }

    let count = u32::try_from(rows.len())?;
    let mut bytes = Vec::with_capacity(4 + rows.len() * WORLD_ENTITY_ROW_SIZE);
    bytes.extend_from_slice(&count.to_le_bytes());
    for row in rows {
        let start = bytes.len();
        bytes.extend_from_slice(&row.tick.max(min_tick).to_le_bytes());
        bytes.extend_from_slice(&(row.entity_id as u32).to_le_bytes());
        bytes.extend_from_slice(&row.serial.to_le_bytes());
        bytes.push(row.class_id);
        bytes.push(row.operation);
        bytes.extend_from_slice(&row.flags.to_le_bytes());
        for value in row.origin {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        for value in row.angles {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes.extend_from_slice(&row.simulation_time.to_le_bytes());
        bytes.push(row.door_state);
        bytes.extend_from_slice(&[0u8; 3]);
        bytes.extend_from_slice(&row.model.to_le_bytes());
        debug_assert_eq!(bytes.len() - start, WORLD_ENTITY_ROW_SIZE);
    }
    Ok((bytes, count))
}

/// S2EX tag 15/schema 1. Full opaque AG2 task-recipe snapshots keyed by network entity lifetime.
/// Record: tick i32, entity index u32, entity serial u32, life ordinal u32, active slot u32
/// (MAX means unknown), recipe version i32 (MIN means unknown), graph definition u64 and iteration
/// u32 (MAX means unknown), slot count u16, each topology as u32 length + bytes, then dynamic vector
/// as u32 length + bytes. The parser does not decode or truncate the recipe contents.
fn encode_ag2_recipes(
    snapshots: &[parser::second_pass::ag2_recipes::Ag2RecipeSnapshot],
    min_tick: i32,
    max_tick: i32,
) -> Result<(Vec<u8>, u32)> {
    use parser::second_pass::ag2_recipes::Ag2RecipeSnapshot;
    let mut latest_before: HashMap<(u32, u32), Ag2RecipeSnapshot> = HashMap::new();
    let mut in_window = Vec::new();
    for row in snapshots {
        let identity = (row.entity_id, row.entity_serial);
        if row.tick < min_tick {
            if latest_before.get(&identity).is_none_or(|previous|
                (previous.tick, previous.life_index) < (row.tick, row.life_index)) {
                latest_before.insert(identity, row.clone());
            }
        } else if row.tick <= max_tick {
            in_window.push(row.clone());
        }
    }
    let first_rows: HashMap<(u32, u32), (i32, u32)> = in_window.iter().fold(HashMap::new(), |mut out, row| {
        out.entry((row.entity_id, row.entity_serial))
            .and_modify(|first| {
                if row.tick < first.0 || (row.tick == first.0 && row.life_index > first.1) {
                    *first = (row.tick, row.life_index);
                }
            })
            .or_insert((row.tick, row.life_index));
        out
    });
    for (identity, mut seed) in latest_before {
        let first = first_rows.get(&identity).copied();
        // A later entity-index incarnation supersedes older snapshots, even if the numeric
        // entity serial was reused. Never seed an expired capture life into the new life.
        if first.is_none_or(|(tick, life_index)|
            tick > min_tick && life_index == seed.life_index) {
            seed.tick = min_tick;
            in_window.push(seed);
        }
    }
    in_window.sort_by_key(|row| (row.tick, row.entity_id, row.entity_serial, row.life_index));
    let mut deduped: Vec<Ag2RecipeSnapshot> = Vec::with_capacity(in_window.len());
    for row in in_window {
        if deduped.last().is_some_and(|last|
            (last.tick, last.entity_id, last.entity_serial)
                == (row.tick, row.entity_id, row.entity_serial)) {
            *deduped.last_mut().unwrap() = row;
        } else {
            deduped.push(row);
        }
    }
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&u32::try_from(deduped.len())?.to_le_bytes());
    for row in &deduped {
        anyhow::ensure!(row.topologies.len() <= u16::MAX as usize, "AG2 topology slot count exceeds u16");
        bytes.extend_from_slice(&row.tick.to_le_bytes());
        bytes.extend_from_slice(&row.entity_id.to_le_bytes());
        bytes.extend_from_slice(&row.entity_serial.to_le_bytes());
        bytes.extend_from_slice(&row.life_index.to_le_bytes());
        bytes.extend_from_slice(&row.active_slot.unwrap_or(u32::MAX).to_le_bytes());
        bytes.extend_from_slice(&row.recipe_version.unwrap_or(i32::MIN).to_le_bytes());
        bytes.extend_from_slice(&row.graph_definition.unwrap_or(u64::MAX).to_le_bytes());
        bytes.extend_from_slice(&row.graph_iteration.unwrap_or(u32::MAX).to_le_bytes());
        bytes.extend_from_slice(&u16::try_from(row.topologies.len())?.to_le_bytes());
        for topology in &row.topologies {
            bytes.extend_from_slice(&u32::try_from(topology.len())?.to_le_bytes());
            bytes.extend_from_slice(topology);
        }
        bytes.extend_from_slice(&u32::try_from(row.dynamic.len())?.to_le_bytes());
        bytes.extend_from_slice(&row.dynamic);
    }
    Ok((bytes, u32::try_from(deduped.len())?))
}

fn close(left: [f32; 3], right: [f32; 3]) -> bool {
    (0..3).all(|axis| (left[axis] - right[axis]).abs() <= WORLD_ENTITY_ORIGIN_EPSILON)
}

pub fn write_replay_s2r(
    output_path: &Path,
    grouped_records: &HashMap<u64, Vec<TickRecord>>,
    collection: &Collection,
    collection_data: &KillCollectionData,
    utility_thrown: &[UtilityThrown],
    grenade_trajectories: &[GrenadeTrajectory],
    weapon_fire_events: &[WeaponFireEvent],
    grenade_stats: &GrenadeStats,
    weapon_switch_ticks: &[i32],
    pad_ticks: i32,
    audio_events: &[AudioEvent],
    smoke_voxels: &[SmokeVoxelTrack],
    weapon_entities: &[WeaponEntitySnapshot],
    game_events: &[GameEvent],
    skip_buy_time: bool,
    playback_range: Option<(i32, i32)>,
    shared_round: bool,
    infernos: &[parser::second_pass::infernos::InfernoPatchRecord],
    utility: Option<&parser::second_pass::utility::UtilityData>,
    world_entities: &[parser::second_pass::world_entities::WorldEntityDelta],
    ag2_recipes: &[parser::second_pass::ag2_recipes::Ag2RecipeSnapshot],
) -> Result<()> {
    write_replay_s2r_inner(output_path, grouped_records, Some(collection), collection_data,
        utility_thrown, grenade_trajectories, weapon_fire_events, grenade_stats,
        weapon_switch_ticks, pad_ticks, audio_events, smoke_voxels, weapon_entities,
        game_events, skip_buy_time, playback_range, shared_round, infernos, utility,
        world_entities, ag2_recipes)
}

/// Explicit source-local diagnostic interval, without a collection or claimed round.
/// Uses the same payload encoders as production; callers own no-clobber publication.
pub fn write_diagnostic_interval_s2r(
    output_path: &Path,
    grouped_records: &HashMap<u64, Vec<TickRecord>>,
    source: &KillCollectionData,
    utility_thrown: &[UtilityThrown],
    grenade_trajectories: &[GrenadeTrajectory],
    weapon_fire_events: &[WeaponFireEvent],
    audio_events: &[AudioEvent],
    smoke_voxels: &[SmokeVoxelTrack],
    weapon_entities: &[WeaponEntitySnapshot],
    game_events: &[GameEvent],
    interval: (i32, i32),
    infernos: &[parser::second_pass::infernos::InfernoPatchRecord],
    utility: Option<&parser::second_pass::utility::UtilityData>,
    world_entities: &[parser::second_pass::world_entities::WorldEntityDelta],
    ag2_recipes: &[parser::second_pass::ag2_recipes::Ag2RecipeSnapshot],
) -> Result<()> {
    if interval.0 < 0 || interval.1 < interval.0
        || i64::from(interval.1) - i64::from(interval.0) >= 65_535 {
        return Err(anyhow!("Diagnostic interval must contain 1..65535 nonnegative ticks"));
    }
    for records in grouped_records.values() {
        if records.windows(2).any(|pair| pair[0].tick >= pair[1].tick) {
            return Err(anyhow!("Diagnostic player rows must have unique ascending ticks"));
        }
    }
    if !grouped_records.values().flatten().any(|row| row.alive > 0) {
        return Err(anyhow!("Diagnostic interval has no alive player samples"));
    }
    write_replay_s2r_inner(output_path, grouped_records, None, source,
        utility_thrown, grenade_trajectories, weapon_fire_events, &GrenadeStats::default(),
        &[], 0, audio_events, smoke_voxels, weapon_entities, game_events, false,
        Some(interval), true, infernos, utility, world_entities, ag2_recipes)
}

fn write_replay_s2r_inner(
    output_path: &Path,
    grouped_records: &HashMap<u64, Vec<TickRecord>>,
    collection: Option<&Collection>,
    collection_data: &KillCollectionData,
    utility_thrown: &[UtilityThrown],
    grenade_trajectories: &[GrenadeTrajectory],
    weapon_fire_events: &[WeaponFireEvent],
    grenade_stats: &GrenadeStats,
    weapon_switch_ticks: &[i32],
    pad_ticks: i32,
    audio_events: &[AudioEvent],
    smoke_voxels: &[SmokeVoxelTrack],
    weapon_entities: &[WeaponEntitySnapshot],
    game_events: &[GameEvent],
    skip_buy_time: bool,
    playback_range: Option<(i32, i32)>,
    shared_round: bool,
    infernos: &[parser::second_pass::infernos::InfernoPatchRecord],
    utility: Option<&parser::second_pass::utility::UtilityData>,
    world_entities: &[parser::second_pass::world_entities::WorldEntityDelta],
    ag2_recipes: &[parser::second_pass::ag2_recipes::Ag2RecipeSnapshot],
) -> Result<()> {
    // ── Steamid → index mapping (sorted for determinism) ──────────────────────
    let mut sorted_steamids: Vec<u64> = grouped_records.keys().copied().collect();
    sorted_steamids.sort();
    let steamid_to_idx: HashMap<u64, usize> = sorted_steamids
        .iter()
        .enumerate()
        .map(|(i, &s)| (s, i))
        .collect();
    let player_count = sorted_steamids.len();
    if player_count > 255 {
        return Err(anyhow!("Player count {} exceeds u8 maximum", player_count));
    }

    // ── Tick range ─────────────────────────────────────────────────────────────
    let mut min_tick = i32::MAX;
    let mut max_tick = i32::MIN;
    for records in grouped_records.values() {
        for r in records {
            if r.tick < min_tick {
                min_tick = r.tick;
            }
            if r.tick > max_tick {
                max_tick = r.tick;
            }
        }
    }
    if min_tick == i32::MAX || max_tick == i32::MIN {
        return Err(anyhow!(
            "No tick data found for collection {}",
            collection.map_or(0, |value| value.collection_num)
        ));
    }
    if let Some((first, last)) = playback_range {
        if first > min_tick || last < max_tick {
            return Err(anyhow!("Replay window does not contain its player records"));
        }
        min_tick = first;
        max_tick = last;
    }
    let tick_count = (max_tick - min_tick + 1) as u32;

    // Build cosmetic signatures before metadata, as the catalog is stored in
    // that JSON block and frame indexes refer to it below.
    let cosmetic_catalog =
        CosmeticCatalog::from_records(&sorted_steamids, grouped_records, weapon_entities);

    // ── Build variable-length sections ahead of time to compute offsets ───────
    let mut metadata: serde_json::Value = if let Some(collection) = collection {
        let meta_json = build_meta_json(
            collection,
            collection_data,
            &steamid_to_idx,
            grouped_records,
            grenade_stats,
            weapon_switch_ticks,
            pad_ticks,
            &cosmetic_catalog,
        )?;
        serde_json::from_str(&meta_json)?
    } else {
        let info = &collection_data.demo_info;
        let players: Vec<_> = sorted_steamids.iter().enumerate().map(|(index, sid)| {
            let player = collection_data.players.iter().find(|player| player.steam_id == *sid);
            serde_json::json!({ "index": index, "steamid": sid.to_string(),
                "name": player.map(|p| p.player_name.as_str()).unwrap_or("Unknown"),
                "team": player.map(|p| p.team.as_str()).unwrap_or("?") })
        }).collect();
        serde_json::json!({ "version": 1, "map_name": info.map_name,
            "game_version": info.game_version, "demo_name": info.demo_name,
            "source_demo_path": info.demo_path, "total_ticks": info.total_ticks,
            "interval_start_tick": min_tick, "interval_end_tick": max_tick,
            "tick_domain": "input_demo", "players": players,
            "cosmetics": cosmetic_catalog.metadata() })
    };
    metadata["replay_window_version"] = serde_json::json!(2);
    metadata["skip_buy_time"] = serde_json::json!(skip_buy_time);
    metadata["replay_scope"] = serde_json::json!(if collection.is_none() { "diagnostic_interval" }
        else if shared_round { "round" } else { "collection" });
    if collection.is_none() {
        let death_ticks: Vec<i32> = game_events.iter()
            .filter(|event| event.name == "player_death" && event.tick >= min_tick && event.tick <= max_tick)
            .map(|event| event.tick)
            .collect();
        metadata["tick_domain_description"] = serde_json::json!(
            "demo playback ticks shared by player frames and source game events"
        );
        metadata["diagnostic_player_death_ticks"] = serde_json::json!(death_ticks);
    }
    metadata["source_cache_key"] = serde_json::json!(source_cache_key(Path::new(&collection_data.demo_info.demo_path)));
    if shared_round && collection.is_some() {
        // Collection identity, highlights and statistics belong to DuckDB. Retain only
        // round/source identity and payload dictionaries in the shared metadata.
        let keep = ["version", "map_name", "game_version", "demo_name", "folder", "round",
            "round_start_tick", "round_end_tick", "round_freeze_end", "padding", "total_ticks",
            "players", "cosmetics", "replay_window_version",
            "skip_buy_time", "replay_scope", "source_cache_key"];
        metadata.as_object_mut().unwrap().retain(|key, _| keep.contains(&key.as_str()));
    }
    let meta_json = serde_json::to_string(&metadata)?;
    let meta_bytes = meta_json.as_bytes();
    let meta_len = meta_bytes.len() as u32;

    let mut kill_events = if shared_round {
        build_round_kill_events(game_events, &steamid_to_idx, min_tick, max_tick)
    } else { build_kill_events(collection.expect("collection scope has identity"), collection_data, &steamid_to_idx) };
    enrich_kill_events(&mut kill_events, game_events, &steamid_to_idx, grouped_records);
    let kill_count = kill_events.len() as u32;
    let util_count = utility_thrown.len() as u32;

    // ── Count alive frames per player for v3 format ────────────────────────────
    let mut alive_frame_counts: Vec<u16> = Vec::with_capacity(player_count);
    for &sid in &sorted_steamids {
        let count = grouped_records
            .get(&sid)
            .map(|records| records.iter().filter(|r| r.alive > 0).count() as u16)
            .unwrap_or(0);
        alive_frame_counts.push(count);
    }

    // ── Compute frames block size (v3: variable per player) ────────────────────
    let frames_block_size: u32 = alive_frame_counts
        .iter()
        .map(|&count| (count as u32) * (FRAME_STRIDE as u32))
        .sum();

    // ── Compute section offsets ────────────────────────────────────────────────
    let players_offset: u32 = HEADER_SIZE;
    let meta_offset: u32 = players_offset + (player_count as u32 * PLAYER_ENTRY_SIZE);
    let frames_offset: u32 = meta_offset + 4 + meta_len;
    let kills_offset: u32 = frames_offset + frames_block_size;
    let util_offset: u32 = kills_offset + 2 + kill_count * KILL_ENTRY_SIZE;

    // Trajectory block: 4 (count u32) + per-traj: 8 header + 16 * point_count
    // v5 traj header: thrower(1)+type(1)+point_count(2)+entity_id(4) = 8 bytes
    let traj_block_size: u32 = 4 + grenade_trajectories
        .iter()
        .map(|t| 8u32 + 16 * t.trajectory_points.len() as u32)
        .sum::<u32>();
    let traj_offset: u32 = util_offset + 2 + util_count * UTIL_ENTRY_SIZE;
    let wf_offset: u32 = traj_offset + traj_block_size;
    // Weapon-fire rows have a fixed 12-byte prefix followed by four bytes per
    // victim. Keep this v4/v5/v6 sizing separate from the new v7 append-only
    // audio block, so the existing section offsets and bytes remain unchanged.
    let wf_block_size = 2_u32
        + weapon_fire_events
            .iter()
            .map(|wf| 12_u32 + 4_u32 * wf.victim_steamids.len() as u32)
            .sum::<u32>();
    let audio_offset = wf_offset
        .checked_add(wf_block_size)
        .ok_or_else(|| anyhow!("S2R audio offset exceeds u32"))?;
    let audio_block = encode_audio_block(audio_events)?;
    let smoke_offset = audio_offset
        .checked_add(u32::try_from(audio_block.len())?)
        .ok_or_else(|| anyhow!("S2R smoke offset exceeds u32"))?;
    let smoke_block = encode_smoke_block(smoke_voxels, min_tick, max_tick)?;
    let weapon_lifetimes = authority_weapon_lifetimes(weapon_entities, min_tick, max_tick);
    let (agent_lives, agent_count) =
        encode_agent_lives(&sorted_steamids, grouped_records, ag2_recipes, max_tick)?;
    let (lifetime_table, lifetime_count) =
        encode_weapon_lifetime_table(&weapon_lifetimes, &cosmetic_catalog, max_tick)?;
    let (inventory_deltas, inventory_count) =
        encode_inventory_deltas(&weapon_lifetimes, &steamid_to_idx)?;
    let (world_deltas, world_count) = encode_world_weapon_deltas(&weapon_lifetimes)?;
    let (ragdoll_impacts, ragdoll_impact_count) =
        encode_ragdoll_impacts(&sorted_steamids, grouped_records, &kill_events)?;
    let (death_input_provenance, death_input_provenance_count) =
        encode_death_input_provenance(&sorted_steamids, grouped_records, &kill_events)?;
    let (bullet_impacts, bullet_impact_count) =
        encode_bullet_impacts(game_events, &steamid_to_idx, min_tick, max_tick)?;
    let (grenade_detonations, grenade_detonation_count) =
        encode_grenade_detonations(game_events, min_tick, max_tick)?;
    let (fire_bullets_inputs, fire_bullets_input_count) =
        encode_fire_bullets_inputs(game_events, &steamid_to_idx, min_tick, max_tick)?;
    let (player_states, player_state_count) = super::player_states::encode_player_states(
        &sorted_steamids, grouped_records, game_events, min_tick, max_tick,
    )?;
    let inferno_rows = inferno_rows_for_window(infernos, min_tick, max_tick);
    let mut inferno_payload = Vec::with_capacity(4 + inferno_rows.len() * 44);
    inferno_payload.extend_from_slice(&u32::try_from(inferno_rows.len())?.to_le_bytes());
    for row in &inferno_rows { inferno_payload.extend_from_slice(&row.encode()); }
    let (weapon_materials, weapon_material_count) = super::weapon_materials::encode(
        grouped_records.values().flatten(),
        weapon_lifetimes.iter().flat_map(|life| life.rows.iter().map(move |row| (life.id, *row))),
        min_tick, max_tick,
    )?;
    let (utility_payload, utility_count) = super::utility::encode(utility, max_tick)?;
    let (world_entity_payload, world_entity_count) =
        encode_world_entities(world_entities, min_tick, max_tick)?;
    let (ag2_recipe_payload, ag2_recipe_count) =
        encode_ag2_recipes(ag2_recipes, min_tick, max_tick)?;
    let extension_block = encode_extension_block(&[
        (12, 2, utility_payload, utility_count),
        (11, 1, weapon_materials, weapon_material_count),
        (EXT_AGENT_LIVES, 1, agent_lives, agent_count),
        (EXT_WEAPON_LIFETIMES, 1, lifetime_table, lifetime_count),
        (EXT_INVENTORY_DELTAS, 1, inventory_deltas, inventory_count),
        (EXT_WORLD_WEAPON_DELTAS, 1, world_deltas, world_count),
        (
            EXT_RAGDOLL_IMPACTS,
            1,
            ragdoll_impacts,
            ragdoll_impact_count,
        ),
        (EXT_BULLET_IMPACTS, 1, bullet_impacts, bullet_impact_count),
        (
            EXT_GRENADE_DETONATIONS,
            1,
            grenade_detonations,
            grenade_detonation_count,
        ),
        (
            EXT_FIRE_BULLETS_INPUTS,
            1,
            fire_bullets_inputs,
            fire_bullets_input_count,
        ),
        (EXT_PLAYER_STATES, 1, player_states, player_state_count),
        (10, 1, inferno_payload, u32::try_from(inferno_rows.len())?),
        // Appended rather than prepended: readers index the directory by tag, but keeping
        // the existing entries where they were means an older file and a new one differ only
        // by this section.
        (EXT_WORLD_ENTITIES, 1, world_entity_payload, world_entity_count),
        (
            EXT_DEATH_INPUT_PROVENANCE,
            1,
            death_input_provenance,
            death_input_provenance_count,
        ),
        (EXT_AG2_RECIPES, 1, ag2_recipe_payload, ag2_recipe_count),
    ])?;
    let extension_offset = smoke_offset
        .checked_add(u32::try_from(smoke_block.len())?)
        .ok_or_else(|| anyhow!("S2R extension offset exceeds u32"))?;
    let extension_length = u32::try_from(extension_block.len())?;

    // ── Ensure output directory exists ────────────────────────────────────────
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let file = File::create(output_path)?;
    let mut w = BufWriter::with_capacity(1 << 20, file); // 1 MB write buffer

    // =========================================================================
    // HEADER — 64 bytes
    // =========================================================================
    w.write_all(S2R_MAGIC)?; // [0..4]   magic "S2RF"
    w.write_all(&S2R_VERSION.to_le_bytes())?; // [4..6]   version
    w.write_all(&[player_count as u8])?; // [6]      player_count
    w.write_all(&[0u8])?; // [7]      _pad
    w.write_all(&tick_count.to_le_bytes())?; // [8..12]  tick_count
    w.write_all(&min_tick.to_le_bytes())?; // [12..16] min_tick
    w.write_all(&players_offset.to_le_bytes())?; // [16..20] players_offset
    w.write_all(&meta_offset.to_le_bytes())?; // [20..24] meta_offset
    w.write_all(&frames_offset.to_le_bytes())?; // [24..28] frames_offset
    w.write_all(&kills_offset.to_le_bytes())?; // [28..32] kills_offset
    w.write_all(&util_offset.to_le_bytes())?; // [32..36] util_offset
    w.write_all(&FRAME_STRIDE.to_le_bytes())?; // [36..38] frame_stride — use this to seek
    w.write_all(&traj_offset.to_le_bytes())?; // [38..42] traj_offset
    w.write_all(&wf_offset.to_le_bytes())?; // [42..46] wf_offset
    w.write_all(&audio_offset.to_le_bytes())?; // [46..50] audio_offset (v7)
    w.write_all(&smoke_offset.to_le_bytes())?; // [50..54] smoke_offset (v8)
    w.write_all(&extension_offset.to_le_bytes())?; // [54..58] extension offset (v9)
    w.write_all(&extension_length.to_le_bytes())?; // [58..62] extension length (v9)
    w.write_all(&[0u8; 2])?; // [62..64] reserved

    // =========================================================================
    // PLAYERS BLOCK (v3) — player_count × 46 bytes
    //   steamid:     u64   (8)
    //   name:        [u8;32]
    //   team:        u8    (1)   2=T  3=CT  0=unknown
    //   frame_count: u16   (2)   number of alive frames for this player
    //   _pad:        [u8;3]
    // =========================================================================
    for (idx, &sid) in sorted_steamids.iter().enumerate() {
        let info = collection_data.players.iter().find(|p| p.steam_id == sid);
        let name = info.map(|p| p.player_name.as_str()).unwrap_or("Unknown");

        // PRIORITY:
        // 1. CSV team (if valid T/CT)
        // 2. Demo-parsed team from TickRecords (fallback when CSV has no valid team)
        // 3. Default to 0 (unknown)
        let csv_team: Option<u8> = info.and_then(|p| match p.team.as_str() {
            "T" | "2" => Some(2),
            "CT" | "3" => Some(3),
            _ => None,
        });

        let team: u8 = csv_team.unwrap_or_else(|| {
            grouped_records
                .get(&sid)
                .and_then(|records| records.first())
                .map(|rec| match rec.team.as_str() {
                    "T" | "2" => 2,
                    "CT" | "3" => 3,
                    _ => 0,
                })
                .unwrap_or(0)
        });

        let frame_count = alive_frame_counts[idx];

        w.write_all(&sid.to_le_bytes())?; // steamid: u64
        write_name32(&mut w, name)?; // name: [u8; 32]
        w.write_all(&[team])?; // team: u8
        w.write_all(&frame_count.to_le_bytes())?; // frame_count: u16
        w.write_all(&[0u8; 3])?; // _pad
    }

    // =========================================================================
    // META BLOCK — 4 + meta_len bytes
    // =========================================================================
    w.write_all(&meta_len.to_le_bytes())?;
    w.write_all(meta_bytes)?;

    // =========================================================================
    // FRAMES BLOCK (v3) — per-player variable length, only alive frames
    //   For each player, in player index order:
    //     For each alive tick:
    //       tick:       i32   (4)   tick number
    //       pos_x:      f32   (4)
    //       pos_y:      f32   (4)
    //       pos_z:      f32   (4)
    //       yaw:        i16   (2)
    //       pitch:      i16   (2)
    //       weapon_id:  u16   (2)
    //       flags:      u16   (2)   no alive bit
    //       ammo:       u8    (1)
    //       health:     u8    (1)
    //       armor:      u8    (1)
    //       vel_x:      i16   (2)
    //       vel_y:      i16   (2)
    //       vel_z:      i16   (2)
    //       mouse_vel:  i16   (2)
    //     Total: 4 + 31 = 35 bytes per alive frame
    // =========================================================================
    for &sid in &sorted_steamids {
        let records = grouped_records
            .get(&sid)
            .map(|r| r.as_slice())
            .unwrap_or(&[]);

        // Write only alive frames, sorted by tick
        for rec in records.iter().filter(|r| r.alive > 0) {
            // Build flags (bits 0–11, no alive bit in v5)
            let mut flags: u16 = 0;
            if rec.in_reload > 0 {
                flags |= 1 << 0;
            }
            if rec.scoped > 0 {
                flags |= 1 << 1;
            }
            if rec.inspecting > 0 {
                flags |= 1 << 2;
            }
            if rec.airborne > 0 {
                flags |= 1 << 3;
            }
            if rec.walking > 0 {
                flags |= 1 << 4;
            }
            if rec.defusing > 0 {
                flags |= 1 << 5;
            }
            if rec.fw > 0 {
                flags |= 1 << 6;
            }
            if rec.lf > 0 {
                flags |= 1 << 7;
            }
            if rec.rt > 0 {
                flags |= 1 << 8;
            }
            if rec.bk > 0 {
                flags |= 1 << 9;
            }
            if rec.fire > 0 {
                flags |= 1 << 10;
            }
            if rec.crouching > 0 {
                flags |= 1 << 11;
            }
            if rec.right_click > 0 {
                flags |= 1 << 12;
            }

            if let Some(observation) = rec.button_observation {
                flags |= 1 << 13;
                if observation.source == super::button_press::ButtonSource::UserCommandState1 {
                    flags |= 1 << 14;
                }
            }

            let weapon_id = resolve_frame_weapon_id(&rec.weapon_id, &rec.weapon);
            let ammo = rec.ammo.min(255) as u8;

            // Velocity components scaled to i16
            let vel_x = encode_scaled_i16(rec.velocity_x, VEL_SCALE);
            let vel_y = encode_scaled_i16(rec.velocity_y, VEL_SCALE);
            let vel_z = encode_scaled_i16(rec.velocity_z, VEL_SCALE);
            let mouse_vel = encode_scaled_i16(rec.mouse_velocity, MOUSE_VEL_SCALE);

            // Write tick prefix
            w.write_all(&rec.tick.to_le_bytes())?; // tick: i32

            // Write frame data (35 bytes, v5: yaw/pitch as f32)
            w.write_all(&rec.pos_x.to_le_bytes())?; // pos_x:  f32
            w.write_all(&rec.pos_y.to_le_bytes())?; // pos_y:  f32
            w.write_all(&rec.pos_z.to_le_bytes())?; // pos_z:  f32
            w.write_all(&rec.view_yaw.to_le_bytes())?; // yaw:    f32 (v5: was i16)
            w.write_all(&rec.view_pitch.to_le_bytes())?; // pitch:  f32 (v5: was i16)
            w.write_all(&weapon_id.to_le_bytes())?; // weapon_id: u16
            w.write_all(&flags.to_le_bytes())?; // flags:  u16
            w.write_all(&[ammo])?; // ammo:   u8
            w.write_all(&[rec.health])?; // health: u8
            w.write_all(&[rec.armor])?; // armor:  u8
            w.write_all(&vel_x.to_le_bytes())?; // vel_x:  i16
            w.write_all(&vel_y.to_le_bytes())?; // vel_y:  i16
            w.write_all(&vel_z.to_le_bytes())?; // vel_z:  i16
            w.write_all(&mouse_vel.to_le_bytes())?; // mouse:  i16
                                                    // v6 extension.  The v5 bytes above remain byte-for-byte in
                                                    // place; 0 is an explicit unknown/missing sentinel, never an
                                                    // implicit "default finish".
            w.write_all(
                &cosmetic_catalog
                    .weapon_index(rec.weapon_cosmetic.as_ref())
                    .to_le_bytes(),
            )?;
            w.write_all(
                &cosmetic_catalog
                    .glove_index(rec.glove_cosmetic.as_ref())
                    .to_le_bytes(),
            )?;
        }
    }

    // =========================================================================
    // KILL EVENTS BLOCK — 2 + kill_count × 12 bytes
    //   count:      u16
    //   tick:       i32   (4)
    //   killer_idx: u8    (1)
    //   victim_idx: u8    (1)
    //   weapon_id:  u16   (2)
    //   kill_flags: u8    (1)   bit0=headshot, bit1=thru_smoke, bit2=no_scope
    //   known_flags:u8    (1)   same bit positions, absent means unknown
    //   penetrated: u16   (2)   surface count, 65535 unknown
    // =========================================================================
    w.write_all(&(kill_events.len() as u16).to_le_bytes())?;
    for ev in &kill_events {
        w.write_all(&ev.tick.to_le_bytes())?;
        w.write_all(&[ev.killer_idx])?;
        w.write_all(&[ev.victim_idx])?;
        w.write_all(&ev.weapon_id.to_le_bytes())?;
        w.write_all(&[ev.kill_flags])?;
        w.write_all(&[ev.known_flags])?;
        w.write_all(&ev.penetrated.and_then(|v| u16::try_from(v).ok())
            .filter(|v| *v != u16::MAX).unwrap_or(u16::MAX).to_le_bytes())?;
    }

    // =========================================================================
    // UTILITY EVENTS BLOCK — 2 + util_count × 24 bytes
    //   count:       u16
    //   tick_throw:  i32   (4)
    //   tick_land:   i32   (4)
    //   thrower_idx: u8    (1)
    //   type:        u8    (1)   1=flash 2=he 3=smoke 4=molotov 5=decoy
    //   entity_id:   u16   (2)   v5: grenade entity ID (was pad in v3/v4)
    //   land_pos:    [f32;3] (12)
    // =========================================================================
    w.write_all(&(utility_thrown.len() as u16).to_le_bytes())?;
    for ut in utility_thrown {
        let thrower_idx = steamid_to_idx
            .get(&ut.thrower_steamid)
            .copied()
            .unwrap_or(255) as u8;
        let utype = util_type_byte(&ut.weapon);
        let entity_id_u16 = (ut.entity_id as u32 & 0xFFFF) as u16; // truncate to u16

        w.write_all(&ut.tick_throw.to_le_bytes())?;
        w.write_all(&ut.tick_land.to_le_bytes())?;
        w.write_all(&[thrower_idx])?;
        w.write_all(&[utype])?;
        w.write_all(&entity_id_u16.to_le_bytes())?; // entity_id: u16 (v5)
        w.write_all(&ut.util_pos_x.to_le_bytes())?;
        w.write_all(&ut.util_pos_y.to_le_bytes())?;
        w.write_all(&ut.util_pos_z.to_le_bytes())?;
    }

    // =========================================================================
    // GRENADE TRAJECTORIES BLOCK — 4 + traj_count × variable bytes
    //   count:        u32   (4)
    //   For each trajectory (v5 header = 8 bytes):
    //     thrower_idx: u8    (1)
    //     type:        u8    (1)   1=flash 2=he 3=smoke 4=molotov 5=decoy
    //     point_count: u16   (2)
    //     entity_id:   u32   (4)   v5: grenade entity ID
    //     points:      [i32, f32, f32, f32] × point_count (each point = 16 bytes)
    //       tick:      i32   (4)
    //       pos_x:     f32   (4)
    //       pos_y:     f32   (4)
    //       pos_z:     f32   (4)
    // =========================================================================
    w.write_all(&(grenade_trajectories.len() as u32).to_le_bytes())?;
    for traj in grenade_trajectories {
        let thrower_idx = steamid_to_idx.get(&traj.steamid).copied().unwrap_or(255) as u8;
        let utype = util_type_byte(&traj.grenade_type);
        let point_count = u16::try_from(traj.trajectory_points.len())
            .map_err(|_| anyhow::anyhow!("Grenade trajectory exceeds S2R point limit"))?;

        w.write_all(&[thrower_idx])?;
        w.write_all(&[utype])?;
        w.write_all(&point_count.to_le_bytes())?;
        w.write_all(&(traj.entity_id as u32).to_le_bytes())?; // entity_id: u32 (v5)

        for point in &traj.trajectory_points {
            w.write_all(&point.tick.to_le_bytes())?;
            w.write_all(&point.pos_x.to_le_bytes())?;
            w.write_all(&point.pos_y.to_le_bytes())?;
            w.write_all(&point.pos_z.to_le_bytes())?;
        }
    }

    // =========================================================================
    // WEAPON FIRE EVENTS BLOCK (v4) — 2 + wf_count × variable bytes (CSR format)
    //   count:        u16   (2)
    //   For each weapon fire event:
    //     tick:         i32   (4)   when shot was fired
    //     impact_tick:  i32   (4)   when bullet landed (-1 if no impact)
    //     attacker_idx: u8    (1)   shooter player index
    //     weapon_id:    u16   (2)   weapon used
    //     victim_count: u8    (1)   number of victims hit (CSR row length)
    //     For each victim:
    //       victim_idx: u8    (1)   hit player index (255 = unknown/world)
    //       damage:     u16   (2)   damage dealt
    //       is_kill:    u8    (1)   1 = killed, 0 = not
    // =========================================================================
    // Resolve the killer's player index once.
    // process_weapon_fire_events() already filters to only the killer's shots,
    // so every event's attacker is collection.steam_id.  Resolving via
    // wf.attacker_steamid is unreliable because the CS2 weapon_fire game-event
    // only carries a user-id (not a steamid), which may not resolve correctly.
    let killer_attacker_idx: u8 = collection
        .and_then(|value| steamid_to_idx.get(&value.steam_id).copied())
        .unwrap_or(255) as u8;

    w.write_all(&(weapon_fire_events.len() as u16).to_le_bytes())?;
    for wf in weapon_fire_events {
        let attacker_idx = if shared_round {
            steamid_to_idx.get(&wf.attacker_steamid).copied().unwrap_or(255) as u8
        } else { killer_attacker_idx };
        let weapon_id = map_weapon_name_to_id(&wf.weapon);
        let impact_tick = wf.impact_tick.unwrap_or(-1);
        let victim_count = wf.victim_steamids.len() as u8;

        w.write_all(&wf.tick.to_le_bytes())?; // tick: i32
        w.write_all(&impact_tick.to_le_bytes())?; // impact_tick: i32
        w.write_all(&[attacker_idx])?; // attacker_idx: u8
        w.write_all(&weapon_id.to_le_bytes())?; // weapon_id: u16
        w.write_all(&[victim_count])?; // victim_count: u8

        // Write victims (CSR data)
        for i in 0..wf.victim_steamids.len() {
            let victim_idx = steamid_to_idx
                .get(&wf.victim_steamids[i])
                .copied()
                .unwrap_or(255) as u8;
            let damage = wf.damages[i].min(65535) as u16;
            let is_kill = if wf.kill_flags[i] { 1u8 } else { 0u8 };

            w.write_all(&[victim_idx])?; // victim_idx: u8
            w.write_all(&damage.to_le_bytes())?; // damage: u16
            w.write_all(&[is_kill])?; // is_kill: u8
        }
    }

    // =========================================================================
    // RAW AUDIO BLOCK (v7) — see `encode_audio_block` and S2R_FORMAT.md.
    // It is append-only and length-delimited so later tags remain skippable.
    // =========================================================================
    w.write_all(&audio_block)?;

    // =========================================================================
    // SMOKE VOXEL BLOCK (v8) — exact CSmokeGrenadeProjectile network payloads.
    // =========================================================================
    w.write_all(&smoke_block)?;

    // S2EX v1 authority sections. The existing 47-byte per-player frame is unchanged.
    w.write_all(&extension_block)?;

    w.flush()?;
    Ok(())
}

/// Read the format version from an existing S2R file.
///
/// Returns `None` if the file is missing, too short to hold a header, or does not carry the
/// S2RF magic - all of which mean "not a usable S2R output" for skip-check purposes.
pub fn read_s2r_version(path: &Path) -> Option<u16> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() < HEADER_SIZE as usize || &bytes[0..4] != S2R_MAGIC {
        return None;
    }
    Some(u16::from_le_bytes([bytes[4], bytes[5]]))
}

fn has_current_extensions(bytes: &[u8]) -> bool {
    if bytes.len() < HEADER_SIZE as usize || &bytes[0..4] != S2R_MAGIC {
        return false;
    }
    if u16::from_le_bytes([bytes[4], bytes[5]]) != S2R_VERSION {
        return false;
    }
    let extension_offset = u32::from_le_bytes(bytes[54..58].try_into().unwrap()) as usize;
    let extension_length = u32::from_le_bytes(bytes[58..62].try_into().unwrap()) as usize;
    let Some(extension_end) = extension_offset.checked_add(extension_length) else {
        return false;
    };
    if extension_offset < HEADER_SIZE as usize
        || extension_end != bytes.len()
        || extension_length < 8
        || &bytes[extension_offset..extension_offset + 4] != EXTENSION_MAGIC
    {
        return false;
    }
    let extension = &bytes[extension_offset..extension_end];
    if u16::from_le_bytes(extension[4..6].try_into().unwrap()) != EXTENSION_SCHEMA_VERSION {
        return false;
    }
    let section_count = u16::from_le_bytes(extension[6..8].try_into().unwrap()) as usize;
    if section_count > 64 { return false; }
    let Some(directory_end) = section_count
        .checked_mul(16)
        .and_then(|size| 8usize.checked_add(size))
    else {
        return false;
    };
    if directory_end > extension.len() {
        return false;
    }

    let mut required = [false; 13];
    let mut ranges = Vec::with_capacity(section_count);
    for index in 0..section_count {
        let entry = 8 + index * 16;
        let tag = u16::from_le_bytes(extension[entry..entry + 2].try_into().unwrap());
        let schema = u16::from_le_bytes(extension[entry + 2..entry + 4].try_into().unwrap());
        let offset =
            u32::from_le_bytes(extension[entry + 4..entry + 8].try_into().unwrap()) as usize;
        let length =
            u32::from_le_bytes(extension[entry + 8..entry + 12].try_into().unwrap()) as usize;
        let Some(end) = offset.checked_add(length) else {
            return false;
        };
        if offset < directory_end || end > extension.len() {
            return false;
        }
        ranges.push((offset, end));
        if tag == 12 && schema != 2 { return false; }
        if (schema == 1 && (1..=11).contains(&tag)) || (schema == 2 && tag == 12) {
            if required[tag as usize] { return false; }
            if tag == 12 {
                let count = u32::from_le_bytes(extension[entry + 12..entry + 16].try_into().unwrap());
                if !super::utility::is_captured(&extension[offset..end], count) { return false; }
            }
            required[tag as usize] = true;
        }
    }
    ranges.sort_by_key(|range| range.0);
    !ranges.windows(2).any(|pair| pair[0].1 > pair[1].0)
        && required[1..=12].iter().all(|present| *present)
}

/// Whether `path` is an S2R file this build considers current.
///
/// A file written by an older revision of the format is not a valid reason to skip
/// regeneration, which the previous existence-only check could not express.
pub fn is_current_s2r(path: &Path) -> bool {
    std::fs::read(path)
        .ok()
        .is_some_and(|bytes| has_current_extensions(&bytes))
}

/// Window policy changes must regenerate cached replays even when the binary layout
/// stays compatible. Missing metadata identifies files written before this policy.
pub fn has_tick_range(path: &Path, range: (u32, u32)) -> bool {
    use std::io::Read;
    let mut bytes = [0u8; 40];
    if std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut bytes)).is_err() { return false; }
    u32::from_le_bytes(bytes[12..16].try_into().unwrap()) == range.0 && u32::from_le_bytes(bytes[8..12].try_into().unwrap()) == range.1.saturating_sub(range.0) + 1
}

pub fn is_current_s2r_for(path: &Path, skip_buy_time: bool, pad_ticks: i32) -> bool {
    let Ok(bytes) = std::fs::read(path) else { return false; };
    has_current_extensions(&bytes) && matches_window_policy(&bytes, skip_buy_time, pad_ticks)
}

fn source_cache_key(source: &Path) -> Option<String> {
    let info = std::fs::metadata(source).ok()?;
    let modified = info.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(format!("{}:{}", info.len(), modified.as_nanos()))
}

/// A replaced/edited standalone DEM must not silently reuse its previous round data.
pub fn matches_source(path: &Path, source: &Path) -> bool {
    let Some(key) = source_cache_key(source) else { return false; };
    let Ok(bytes) = std::fs::read(path) else { return false; };
    let Some(offset) = bytes.get(20..24).map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize) else { return false; };
    let Some(length) = bytes.get(offset..offset + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize) else { return false; };
    let Some(json) = bytes.get(offset + 4..offset + 4 + length) else { return false; };
    serde_json::from_slice::<serde_json::Value>(json).ok()
        .is_some_and(|meta| meta["source_cache_key"].as_str() == Some(&key))
}

fn matches_window_policy(bytes: &[u8], skip_buy_time: bool, pad_ticks: i32) -> bool {
    let read_u32 = |offset: usize| -> Option<usize> {
        Some(u32::from_le_bytes(bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?) as usize)
    };
    let Some(offset) = read_u32(20) else { return false; };
    let Some(len) = read_u32(offset) else { return false; };
    let Some(start) = offset.checked_add(4) else { return false; };
    let Some(end) = start.checked_add(len) else { return false; };
    let Some(json) = bytes.get(start..end) else { return false; };
    let Ok(meta) = serde_json::from_slice::<serde_json::Value>(json) else { return false; };
    meta["replay_window_version"].as_u64() == Some(2)
        && meta["skip_buy_time"].as_bool() == Some(skip_buy_time)
        && meta["padding"].as_i64() == Some(if pad_ticks == 0 { -1 } else { pad_ticks as i64 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ag2_window_seed_does_not_carry_expired_capture_life_over_newer_life() {
        use parser::second_pass::ag2_recipes::Ag2RecipeSnapshot;
        let snapshot = |tick: i32, life_index: u32, header: u32| Ag2RecipeSnapshot {
            tick, entity_id: 250, entity_serial: 123, life_index,
            active_slot: Some(5), recipe_version: Some(3),
            graph_definition: Some(9_785_864_542_908_430_973), graph_iteration: Some(1),
            topologies: vec![vec![0xf3, 0x10, 0x10, 0x12, 0x90, 0xe6, 0x58, 0x0a]],
            dynamic: header.to_le_bytes().to_vec(),
        };
        let (bytes, count) = encode_ag2_recipes(
            &[snapshot(1, 0, 2399), snapshot(326, 1, 2724), snapshot(327, 1, 2725)], 327, 327).unwrap();
        assert_eq!(count, 1);
        assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 327);
        assert_eq!(u32::from_le_bytes(bytes[12..16].try_into().unwrap()), 123);
        assert_eq!(u32::from_le_bytes(bytes[58..62].try_into().unwrap()), 2725);
    }

    #[test]
    fn agent_life_uses_unique_live_ag2_pawn_identity_and_serial_when_dense_pawn_is_missing() {
        use parser::second_pass::ag2_recipes::Ag2RecipeSnapshot;
        let rows = HashMap::from([(11, vec![TickRecord {
            tick: 327, alive: 1, pawn_entity_id: None, agent_definition_index: Some(5109),
            ..Default::default()
        }])]);
        let recipes = vec![Ag2RecipeSnapshot {
            tick: 327, entity_id: 250, entity_serial: 123, life_index: 1,
            active_slot: Some(5), recipe_version: Some(3),
            graph_definition: Some(9_785_864_542_908_430_973), graph_iteration: Some(1),
            topologies: vec![], dynamic: vec![1],
        }];
        let (bytes, count) = encode_agent_lives(&[11], &rows, &recipes, 327).unwrap();
        assert_eq!(count, 1);
        assert_eq!(u32::from_le_bytes(bytes[16..20].try_into().unwrap()), 250);
        assert_eq!(u32::from_le_bytes(bytes[24..28].try_into().unwrap()), 123);
    }

    #[test]
    fn inferno_window_prunes_ended_lives_but_preserves_live_history_and_unknown_eof() {
        use parser::second_pass::infernos::InfernoPatchRecord;
        let row = |tick, serial, start_tick, flags| InfernoPatchRecord {
            tick, entity_id: 12, serial, start_tick, index: if flags == 16 { 255 } else { 0 },
            flags, inferno_type: 0, position: [0.; 3], normal: [0.; 3],
        };
        let rows = vec![row(1, 1, 1, 0), row(5, 1, 1, 7), row(20, 1, 1, 16),
            row(10, 2, 10, 0), row(11, 2, 10, 7), row(30, 2, 10, 3), row(40, 2, 10, 16),
            row(2, 3, 2, 0), row(3, 3, 2, 6), row(100_000, 3, 2, 7)];
        let selected = inferno_rows_for_window(&rows, 20, 30);
        assert_eq!(selected.len(), 5);
        assert!(selected.iter().all(|r| r.serial != 1 && r.tick <= 30));
        assert!(selected.iter().any(|r| r.serial == 2 && r.tick == 11 && r.start_tick == 10));
        assert!(selected.iter().any(|r| r.serial == 3 && r.flags == 6));
        // Unknown EOF is not an expiry, even for a very long intersecting life.
        let late = inferno_rows_for_window(&rows, 90_000, 100_000);
        assert_eq!(late.len(), 3);
        assert!(late.iter().all(|r| r.serial == 3 && r.start_tick == 2));
        assert!(inferno_rows_for_window(&rows, 20, 19).is_empty());
    }
    #[test]
    fn round_kills_preserve_other_killers_and_unresolved_world_deaths() {
        let event = |tick, killer: u64, victim: u64| authority_event("player_death", tick, vec![
            ("attacker_steamid", Variant::String(killer.to_string())),
            ("user_steamid", Variant::String(victim.to_string())),
            ("weapon", Variant::String("ak47".into())), ("headshot", Variant::Bool(true))]);
        let kills = build_round_kill_events(&[event(99, 11, 22), event(104, 11, 33), event(108, 22, 11), event(110, 0, 22)],
            &HashMap::from([(11, 0), (22, 1), (33, 2)]), 100, 110);
        assert_eq!(kills.len(), 3);
        assert_eq!((kills[0].killer_idx, kills[0].victim_idx), (0, 2));
        assert_eq!((kills[1].killer_idx, kills[1].victim_idx), (1, 0));
        assert_eq!(kills[2].killer_idx, 255);
        assert_eq!(kills[0].kill_flags, 1);
        assert_eq!(kills[0].weapon_id, 7);
    }

    #[test]
    fn replay_window_policy_invalidates_old_or_differently_configured_cache() {
        let json = br#"{"replay_window_version":2,"skip_buy_time":true,"padding":-1}"#;
        let mut bytes = vec![0; 24];
        bytes[20..24].copy_from_slice(&24_u32.to_le_bytes());
        bytes.extend_from_slice(&(json.len() as u32).to_le_bytes());
        bytes.extend_from_slice(json);
        assert!(matches_window_policy(&bytes, true, 0));
        assert!(!matches_window_policy(&bytes, false, 0));
        assert!(!matches_window_policy(&bytes, true, 256));
        bytes.truncate(bytes.len() - 1);
        assert!(!matches_window_policy(&bytes, true, 0));
        assert!(!matches_window_policy(&[], true, 0));
    }

    fn index_fixture_data(
        players: Vec<super::super::kill_collection_parser::PlayerInfo>,
    ) -> KillCollectionData {
        KillCollectionData {
            demo_info: super::super::kill_collection_parser::DemoInfo {
                demo_path: String::new(),
                demo_name: String::new(),
                map_name: String::new(),
                game_version: String::new(),
                folder: String::new(),
                total_ticks: 0,
                game_start_offset: 0.0,
                ace_count: 0,
                quad_count: 0,
                multi_count: 0,
                triple_count: 0,
                double_count: 0,
                single_count: 0,
            },
            rounds: Vec::new(),
            players,
            collections: Vec::new(),
            collection_details: HashMap::new(),
        }
    }

    #[test]
    fn parser_entity_indexes_map_to_stable_s2r_player_indexes() {
        use super::super::kill_collection_parser::PlayerInfo;

        let data = index_fixture_data(vec![
            PlayerInfo {
                player_name: "first".to_owned(),
                steam_id: 300,
                killer_index: 42,
                team: "CT".to_owned(),
            },
            PlayerInfo {
                player_name: "second".to_owned(),
                steam_id: 100,
                killer_index: 90,
                team: "T".to_owned(),
            },
        ]);
        let stable = HashMap::from([(100, 0), (300, 1)]);

        assert_eq!(
            stable_player_index_from_entity_index(42, &data, &stable),
            Some(1)
        );
        assert_eq!(
            stable_player_index_from_entity_index(90, &data, &stable),
            Some(0)
        );
        assert_eq!(
            stable_player_index_from_entity_index(7, &data, &stable),
            None
        );
    }

    #[test]
    fn frame_weapon_id_falls_back_to_display_name() {
        assert_eq!(resolve_frame_weapon_id("0", "MP9"), 34);
        assert_eq!(resolve_frame_weapon_id("invalid", "AK-47"), 7);
        assert_eq!(resolve_frame_weapon_id("60", "MP9"), 60);
    }

    /// The module docblock is the reference any consumer of this format reads, and it had
    /// drifted two revisions (documenting version 4 and a 35-byte stride while the writer
    /// emitted version 5 and 39). Pin the constants so the next change to them fails here
    /// and prompts updating the docs above.
    #[test]
    fn documented_header_values_match_the_constants() {
        assert_eq!(S2R_VERSION, 18, "docblock documents version 18");
        assert_eq!(
            FRAME_STRIDE, 47,
            "docblock documents a 47-byte frame stride"
        );
        assert_eq!(
            TICK_PREFIX_SIZE, 4,
            "docblock documents a 4-byte tick prefix"
        );
        assert_eq!(
            FRAME_DATA_SIZE, 43,
            "docblock documents 43 bytes of frame data"
        );
        assert_eq!(FRAME_STRIDE, TICK_PREFIX_SIZE + FRAME_DATA_SIZE);
    }

    #[test]
    fn ragdoll_impacts_join_death_samples_to_kill_event_ticks() {
        let mut grouped_records = HashMap::new();
        grouped_records.insert(
            22,
            vec![TickRecord {
                tick: 101,
                alive: 0,
                ragdoll_damage_bone: Some(17),
                ragdoll_damage_position: Some([10.0, 20.0, 30.0]),
                ragdoll_damage_force: Some([100.0, -200.0, 300.0]),
                ..Default::default()
            }],
        );
        let kills = vec![KillEvent {
            tick: 100,
            killer_idx: 0,
            victim_idx: 1,
            weapon_id: 7,
            kill_flags: 0,
            ..Default::default()
        }];

        let (payload, count) = encode_ragdoll_impacts(&[11, 22], &grouped_records, &kills).unwrap();

        assert_eq!(count, 1);
        assert_eq!(payload.len(), 40);
        assert_eq!(i32::from_le_bytes(payload[4..8].try_into().unwrap()), 100);
        assert_eq!(payload[8], 1);
        assert_eq!(payload[9], 0b111);
        assert_eq!(u16::from_le_bytes(payload[10..12].try_into().unwrap()), 0);
        assert_eq!(i32::from_le_bytes(payload[12..16].try_into().unwrap()), 17);
        assert_eq!(
            f32::from_le_bytes(payload[16..20].try_into().unwrap()),
            10.0
        );
        assert_eq!(
            f32::from_le_bytes(payload[28..32].try_into().unwrap()),
            100.0
        );
        assert_eq!(
            f32::from_le_bytes(payload[32..36].try_into().unwrap()),
            -200.0
        );
        assert_eq!(
            f32::from_le_bytes(payload[36..40].try_into().unwrap()),
            300.0
        );
    }

    #[test]
    fn death_input_provenance_preserves_source_tick_and_nullable_server_origin() {
        let kills = [KillEvent {
            tick: 100,
            killer_idx: 0,
            victim_idx: 1,
            weapon_id: 7,
            ..Default::default()
        }];
        for (origin, expected_flags) in [
            (Some([12.25, -3.5, 0.0]), 1u8),
            (None, 0u8),
        ] {
            let records = HashMap::from([(22, vec![TickRecord {
                tick: 101,
                alive: 0,
                ragdoll_damage_bone: Some(5),
                ragdoll_server_origin: origin,
                ..Default::default()
            }])]);
            let (payload, count) =
                encode_death_input_provenance(&[11, 22], &records, &kills).unwrap();

            assert_eq!(count, 1);
            assert_eq!(payload.len(), 28);
            assert_eq!(i32::from_le_bytes(payload[4..8].try_into().unwrap()), 100);
            assert_eq!(i32::from_le_bytes(payload[8..12].try_into().unwrap()), 101);
            assert_eq!(payload[12], 1);
            assert_eq!(payload[13], expected_flags);
            assert_eq!(u16::from_le_bytes(payload[14..16].try_into().unwrap()), 0);
            let expected_origin = origin.unwrap_or([0.0; 3]);
            for (axis, expected) in expected_origin.into_iter().enumerate() {
                let offset = 16 + axis * 4;
                assert_eq!(
                    f32::from_le_bytes(payload[offset..offset + 4].try_into().unwrap()),
                    expected
                );
            }
        }
    }

    fn authority_event(name: &str, tick: i32, fields: Vec<(&str, Variant)>) -> GameEvent {
        GameEvent {
            name: name.to_string(),
            fields: fields
                .into_iter()
                .map(
                    |(name, data)| parser::second_pass::game_events::EventField {
                        name: name.to_string(),
                        data: Some(data),
                    },
                )
                .collect(),
            tick,
        }
    }

    fn world_row(
        tick: i32,
        entity_id: i32,
        serial: u32,
        operation: u8,
        origin: [f32; 3],
    ) -> parser::second_pass::world_entities::WorldEntityDelta {
        use parser::second_pass::world_entities::{class, field_flag};
        parser::second_pass::world_entities::WorldEntityDelta {
            tick,
            entity_id,
            serial,
            class_id: class::DOOR_ROTATING,
            operation,
            flags: field_flag::ORIGIN | field_flag::ANGLES,
            origin,
            angles: [0.0, -45.0, 0.0],
            simulation_time: 630.5,
            door_state: 1,
            model: 0x1234_5678_9abc_def0,
        }
    }

    fn world_rows(payload: &[u8]) -> Vec<(i32, u32, u8, [f32; 3])> {
        let count = u32::from_le_bytes(payload[0..4].try_into().unwrap()) as usize;
        assert_eq!(payload.len(), 4 + count * WORLD_ENTITY_ROW_SIZE);
        (0..count)
            .map(|index| {
                let row = &payload[4 + index * WORLD_ENTITY_ROW_SIZE..][..WORLD_ENTITY_ROW_SIZE];
                let float = |at: usize| f32::from_le_bytes(row[at..at + 4].try_into().unwrap());
                (
                    i32::from_le_bytes(row[0..4].try_into().unwrap()),
                    u32::from_le_bytes(row[4..8].try_into().unwrap()),
                    row[13],
                    [float(16), float(20), float(24)],
                )
            })
            .collect()
    }

    #[test]
    fn world_entity_rows_round_trip_their_declared_layout() {
        use parser::second_pass::world_entities::operation as op;
        let door = world_row(120, 99, 7, op::UPDATE, [1047.0, -1040.0, -768.0]);
        let (payload, count) = encode_world_entities(&[door.clone()], 100, 200).unwrap();
        assert_eq!(count, 1);
        assert_eq!(payload.len(), 4 + WORLD_ENTITY_ROW_SIZE);
        let row = &payload[4..];
        assert_eq!(i32::from_le_bytes(row[0..4].try_into().unwrap()), 120);
        assert_eq!(u32::from_le_bytes(row[4..8].try_into().unwrap()), 99);
        assert_eq!(u32::from_le_bytes(row[8..12].try_into().unwrap()), 7);
        assert_eq!(row[12], door.class_id);
        assert_eq!(row[13], op::UPDATE);
        assert_eq!(u16::from_le_bytes(row[14..16].try_into().unwrap()), door.flags);
        assert_eq!(f32::from_le_bytes(row[16..20].try_into().unwrap()), 1047.0);
        assert_eq!(f32::from_le_bytes(row[28..32].try_into().unwrap()), 0.0);
        assert_eq!(f32::from_le_bytes(row[32..36].try_into().unwrap()), -45.0);
        assert_eq!(f32::from_le_bytes(row[40..44].try_into().unwrap()), 630.5);
        assert_eq!(row[44], 1);
        assert_eq!(&row[45..48], &[0, 0, 0], "reserved bytes stay zero");
        assert_eq!(
            u64::from_le_bytes(row[48..56].try_into().unwrap()),
            0x1234_5678_9abc_def0
        );
    }

    #[test]
    fn a_door_respawned_by_a_replayed_full_packet_is_written_once() {
        use parser::second_pass::world_entities::operation as op;
        // The same door arrives from two spawn passes under different entity slots, and its
        // origin is quantised slightly differently each time.
        let deltas = vec![
            world_row(0, 99, 7, op::SPAWN, [1047.0, -1040.0, -768.0]),
            world_row(1, 564, 644, op::SPAWN, [1047.004, -1040.002, -768.0]),
        ];
        let (payload, count) = encode_world_entities(&deltas, 0, 500).unwrap();
        assert_eq!(count, 1, "one authored door is one record");
        assert_eq!(world_rows(&payload)[0].1, 99, "the earliest spawn wins");
    }

    #[test]
    fn a_door_recreated_after_a_round_restart_is_written_again() {
        use parser::second_pass::world_entities::{field_flag, operation as op};
        let mut removed = world_row(100, 99, 7, op::DELETE, [1047.0, -1040.0, -768.0]);
        removed.flags = field_flag::ORIGIN;
        let deltas = vec![
            world_row(0, 99, 7, op::SPAWN, [1047.0, -1040.0, -768.0]),
            removed,
            // The restart re-creates the same door under a new entity slot. Dropping this as a
            // duplicate would leave the viewer hiding it for the rest of the clip.
            world_row(101, 564, 644, op::SPAWN, [1047.0, -1040.0, -768.0]),
        ];
        let (payload, count) = encode_world_entities(&deltas, 0, 500).unwrap();
        assert_eq!(count, 3);
        assert_eq!(
            world_rows(&payload).iter().map(|row| (row.0, row.2)).collect::<Vec<_>>(),
            vec![(0, op::SPAWN), (100, op::DELETE), (101, op::SPAWN)]
        );
    }

    #[test]
    fn a_dormant_entity_does_not_reopen_the_spawn_slot() {
        use parser::second_pass::world_entities::{field_flag, operation as op};
        let mut dormant = world_row(100, 99, 7, op::DELETE, [1047.0, -1040.0, -768.0]);
        dormant.flags = field_flag::ORIGIN | field_flag::DORMANT;
        let deltas = vec![
            world_row(0, 99, 7, op::SPAWN, [1047.0, -1040.0, -768.0]),
            dormant,
            // Re-entering the PVS is not a new door.
            world_row(101, 99, 7, op::SPAWN, [1047.0, -1040.0, -768.0]),
        ];
        let (_, count) = encode_world_entities(&deltas, 0, 500).unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn two_doors_at_different_origins_are_both_written() {
        use parser::second_pass::world_entities::operation as op;
        let deltas = vec![
            world_row(0, 99, 7, op::SPAWN, [1047.0, -1040.0, -768.0]),
            world_row(0, 100, 8, op::SPAWN, [1047.0, -920.0, -768.0]),
        ];
        let (_, count) = encode_world_entities(&deltas, 0, 500).unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn a_spawn_before_the_window_is_kept_and_clamped_but_its_earlier_motion_is_not() {
        use parser::second_pass::world_entities::operation as op;
        let deltas = vec![
            world_row(10, 99, 7, op::SPAWN, [1047.0, -1040.0, -768.0]),
            world_row(40, 99, 7, op::UPDATE, [1047.0, -1040.0, -768.0]),
            world_row(150, 99, 7, op::UPDATE, [1047.0, -1040.0, -768.0]),
            world_row(400, 99, 7, op::DELETE, [1047.0, -1040.0, -768.0]),
        ];
        let (payload, count) = encode_world_entities(&deltas, 100, 300).unwrap();
        assert_eq!(count, 2);
        let rows = world_rows(&payload);
        assert_eq!(
            rows.iter().map(|row| (row.0, row.2)).collect::<Vec<_>>(),
            vec![(100, op::SPAWN), (150, op::UPDATE)],
            "the door is placed at the window start; the pre-window swing and the post-window \
             delete are outside what this clip shows"
        );
    }

    #[test]
    fn an_empty_lane_still_encodes_a_valid_section() {
        let (payload, count) = encode_world_entities(&[], 0, 500).unwrap();
        assert_eq!(count, 0);
        assert_eq!(payload, 0u32.to_le_bytes().to_vec());
    }

    #[test]
    fn bullet_impacts_preserve_exact_positions_and_unknown_shooters() {
        let events = vec![
            authority_event(
                "bullet_impact",
                100,
                vec![
                    ("user_steamid", Variant::String("22".to_string())),
                    ("x", Variant::F32(10.25)),
                    ("y", Variant::F32(-20.5)),
                    ("z", Variant::F32(30.75)),
                ],
            ),
            authority_event(
                "bullet_impact",
                101,
                vec![
                    ("x", Variant::F32(-1.0)),
                    ("y", Variant::F32(2.0)),
                    ("z", Variant::F32(3.0)),
                ],
            ),
            authority_event(
                "bullet_impact",
                102,
                vec![
                    ("x", Variant::F32(f32::NAN)),
                    ("y", Variant::F32(2.0)),
                    ("z", Variant::F32(3.0)),
                ],
            ),
        ];
        let players = HashMap::from([(11, 0usize), (22, 1usize)]);
        let (payload, count) = encode_bullet_impacts(&events, &players, 100, 102).unwrap();

        assert_eq!(count, 2);
        assert_eq!(payload.len(), 4 + 2 * 20);
        assert_eq!(i32::from_le_bytes(payload[4..8].try_into().unwrap()), 100);
        assert_eq!(payload[8], 1);
        assert_eq!(payload[9], 0);
        assert_eq!(u16::from_le_bytes(payload[10..12].try_into().unwrap()), 0);
        assert_eq!(
            f32::from_le_bytes(payload[12..16].try_into().unwrap()),
            10.25
        );
        assert_eq!(
            f32::from_le_bytes(payload[16..20].try_into().unwrap()),
            -20.5
        );
        assert_eq!(
            f32::from_le_bytes(payload[20..24].try_into().unwrap()),
            30.75
        );
        assert_eq!(payload[28], u8::MAX);
    }

    #[test]
    fn grenade_detonations_reuse_utility_types_and_preserve_source_events() {
        let position = || {
            vec![
                ("x", Variant::F32(1.25)),
                ("y", Variant::F32(-2.5)),
                ("z", Variant::F32(3.75)),
            ]
        };
        let mut fire_fields = position();
        fire_fields.push(("entityid", Variant::I32(44)));
        let events = vec![
            authority_event("flashbang_detonate", 10, position()),
            authority_event("hegrenade_detonate", 11, position()),
            authority_event("smokegrenade_detonate", 12, position()),
            authority_event("inferno_startburn", 13, fire_fields),
            authority_event("inferno_startburn", 13, position()),
            authority_event("decoy_started", 14, position()),
        ];
        let (payload, count) = encode_grenade_detonations(&events, 10, 14).unwrap();

        assert_eq!(count, 6);
        assert_eq!(payload.len(), 4 + 6 * 24);
        for (index, expected_type) in [1u8, 2, 3, 4, 4, 5].into_iter().enumerate() {
            assert_eq!(payload[8 + index * 24], expected_type);
        }
        assert_eq!(payload[9], 0);
        assert_eq!(
            u32::from_le_bytes(payload[12..16].try_into().unwrap()),
            u32::MAX
        );
        let fire_start = 4 + 3 * 24;
        assert_eq!(payload[fire_start + 5], 1);
        assert_eq!(
            u32::from_le_bytes(payload[fire_start + 8..fire_start + 12].try_into().unwrap()),
            44
        );
        assert_eq!(
            f32::from_le_bytes(
                payload[fire_start + 12..fire_start + 16]
                    .try_into()
                    .unwrap()
            ),
            1.25
        );
    }

    #[test]
    fn eight_section_directory_offsets_are_relative_to_s2ex() {
        let sections: Vec<_> = (1u16..=8)
            .map(|tag| (tag, 1, vec![tag as u8; tag as usize], u32::from(tag)))
            .collect();
        let block = encode_extension_block(&sections).unwrap();
        assert_eq!(&block[0..4], b"S2EX");
        assert_eq!(u16::from_le_bytes(block[4..6].try_into().unwrap()), 1);
        assert_eq!(u16::from_le_bytes(block[6..8].try_into().unwrap()), 8);

        let mut expected_offset = 8 + 8 * 16;
        for index in 0..8 {
            let entry = 8 + index * 16;
            assert_eq!(
                u16::from_le_bytes(block[entry..entry + 2].try_into().unwrap()),
                index as u16 + 1
            );
            assert_eq!(
                u32::from_le_bytes(block[entry + 4..entry + 8].try_into().unwrap()),
                expected_offset as u32
            );
            expected_offset += index + 1;
        }
        assert_eq!(block.len(), expected_offset);
    }

    #[test]
    fn fire_bullets_inputs_preserve_raw_fields_source_order_and_missing_values() {
        let events = vec![
            authority_event("weapon_fire", 199, vec![]),
            authority_event(
                "fire_bullets",
                200,
                vec![
                    ("user_steamid", Variant::String("22".to_string())),
                    ("player", Variant::U32(0x0102_0304)),
                    ("weapon_id", Variant::U32(7)),
                    ("item_def_index", Variant::U32(8)),
                    ("mode", Variant::U32(9)),
                    ("attack_type", Variant::U32(10)),
                    ("seed", Variant::U32(11)),
                    ("num_bullets_remaining", Variant::U32(12)),
                    ("message_tick", Variant::I32(13)),
                    ("origin_x", Variant::F32(1.0)),
                    ("origin_y", Variant::F32(2.0)),
                    ("origin_z", Variant::F32(3.0)),
                    ("angles_x", Variant::F32(4.0)),
                    ("angles_y", Variant::F32(5.0)),
                    ("angles_z", Variant::F32(6.0)),
                    ("ent_origin_x", Variant::F32(7.0)),
                    ("ent_origin_y", Variant::F32(8.0)),
                    ("ent_origin_z", Variant::F32(9.0)),
                    ("inaccuracy", Variant::F32(0.1)),
                    ("recoil_index", Variant::F32(0.2)),
                    ("spread", Variant::F32(0.3)),
                    ("player_inair", Variant::Bool(true)),
                    ("player_scoped", Variant::Bool(false)),
                    ("extra_type", Variant::I32(14)),
                    ("attack_tick_count", Variant::I32(15)),
                    ("attack_tick_fraction", Variant::F32(0.4)),
                    ("render_tick_count", Variant::I32(16)),
                    ("render_tick_fraction", Variant::F32(0.5)),
                    ("inaccuracy_move", Variant::F32(0.6)),
                    ("inaccuracy_air", Variant::F32(0.7)),
                    ("aim_punch_x", Variant::F32(10.0)),
                    ("aim_punch_y", Variant::F32(11.0)),
                    ("aim_punch_z", Variant::F32(12.0)),
                    ("sound_type", Variant::I32(17)),
                    ("sound_dsp_effect", Variant::U32(18)),
                ],
            ),
            authority_event("fire_bullets", 200, vec![]),
        ];
        let players = HashMap::from([(22, 3usize)]);
        let (payload, count) = encode_fire_bullets_inputs(&events, &players, 200, 200).unwrap();

        assert_eq!(count, 2);
        assert_eq!(payload.len(), 4 + 2 * 152);
        let first = 4;
        assert_eq!(
            i32::from_le_bytes(payload[first..first + 4].try_into().unwrap()),
            200
        );
        assert_eq!(
            u32::from_le_bytes(payload[first + 4..first + 8].try_into().unwrap()),
            1
        );
        assert_eq!(
            u32::from_le_bytes(payload[first + 8..first + 12].try_into().unwrap()),
            (1u32 << 26) - 1
        );
        assert_eq!(payload[first + 12], 3);
        assert_eq!(payload[first + 13], 1);
        assert_eq!(
            u32::from_le_bytes(payload[first + 16..first + 20].try_into().unwrap()),
            0x0102_0304
        );
        assert_eq!(
            f32::from_le_bytes(payload[first + 48..first + 52].try_into().unwrap()),
            1.0
        );
        assert_eq!(payload[first + 96], 1);
        assert_eq!(payload[first + 97], 0);
        assert_eq!(
            f32::from_le_bytes(payload[first + 128..first + 132].try_into().unwrap()),
            10.0
        );
        assert_eq!(
            u32::from_le_bytes(payload[first + 144..first + 148].try_into().unwrap()),
            18
        );

        let second = first + 152;
        assert_eq!(payload[second + 12], u8::MAX);
        assert_eq!(payload[second + 13], 0);
        assert_eq!(
            u32::from_le_bytes(payload[second + 16..second + 20].try_into().unwrap()),
            u32::MAX
        );
        assert_eq!(
            u32::from_le_bytes(payload[second + 48..second + 52].try_into().unwrap()),
            0x7fc0_0000
        );
    }

    fn audio_event(order: u32, payload: AudioEventPayload) -> AudioEvent {
        AudioEvent {
            tick: 100 + order as i32,
            order: AudioEventOrder {
                demo_frame_offset: 1000 + order as u64,
                network_message_index: order,
            },
            payload,
        }
    }

    #[test]
    fn audio_block_covers_every_current_variant_and_sorts_by_source_key() {
        let events = vec![
            audio_event(
                6,
                AudioEventPayload::SosSetLibraryStackFields {
                    stack_hash: None,
                    packed_fields: None,
                },
            ),
            audio_event(
                5,
                AudioEventPayload::SosSetSoundEventParams {
                    soundevent_guid: None,
                    packed_params: None,
                },
            ),
            audio_event(
                4,
                AudioEventPayload::SosStopSoundEventHash {
                    soundevent_hash: None,
                    source_entity_index: None,
                },
            ),
            audio_event(
                3,
                AudioEventPayload::SosStopSoundEvent {
                    soundevent_guid: None,
                },
            ),
            audio_event(
                2,
                AudioEventPayload::SosStartSoundEvent {
                    soundevent_guid: None,
                    soundevent_hash: None,
                    source_entity_index: None,
                    seed: None,
                    packed_params: None,
                    start_time: None,
                },
            ),
            audio_event(1, AudioEventPayload::SvcStopSound { guid: None }),
            audio_event(
                0,
                AudioEventPayload::SvcSounds {
                    reliable_sound: None,
                    sounds: vec![SvcSoundEntry {
                        origin_x: None,
                        origin_y: None,
                        origin_z: None,
                        volume: None,
                        delay_value: None,
                        sequence_number: None,
                        entity_index: None,
                        channel: None,
                        pitch: None,
                        flags: None,
                        sound_num: None,
                        sound_num_handle: None,
                        speaker_entity: None,
                        random_seed: None,
                        sound_level: None,
                        is_sentence: None,
                        is_ambient: None,
                        guid: None,
                        sound_resource_id: None,
                    }],
                },
            ),
        ];
        let block = encode_audio_block(&events).unwrap();
        validate_audio_block(&block).unwrap();
        assert_eq!(u32::from_le_bytes(block[0..4].try_into().unwrap()), 7);

        let mut cursor = 4;
        for expected_tag in 1..=7 {
            assert_eq!(block[cursor], expected_tag);
            let payload_len =
                u32::from_le_bytes(block[cursor + 4..cursor + 8].try_into().unwrap()) as usize;
            assert!(payload_len >= AUDIO_COMMON_SIZE + 1);
            assert_eq!(
                u64::from_le_bytes(block[cursor + 12..cursor + 20].try_into().unwrap()),
                1000 + (expected_tag - 1) as u64
            );
            cursor += AUDIO_EVENT_HEADER_SIZE + payload_len;
        }
        assert_eq!(cursor, block.len());
    }

    #[test]
    fn audio_block_rejects_duplicate_keys_and_malformed_known_records() {
        let first = audio_event(0, AudioEventPayload::SvcStopSound { guid: Some(1) });
        let mut duplicate = first.clone();
        duplicate.payload = AudioEventPayload::SosStopSoundEvent {
            soundevent_guid: Some(1),
        };
        assert!(encode_audio_block(&[first, duplicate]).is_err());

        // A known tag with no common 16-byte provenance must not validate.
        let malformed_common = vec![1, 0, 0, 0, 1, 0, 0, 0, AUDIO_TAG_SVC_STOP_SOUND];
        assert!(validate_audio_block(&malformed_common).is_err());
        // Count and declared payload lengths are independently checked.
        assert!(validate_audio_block(&[1, 0, 0, 0]).is_err());
        let truncated_payload = vec![1, 0, 0, 0, 255, 0, 0, 0, 4, 0, 0, 0];
        assert!(validate_audio_block(&truncated_payload).is_err());
    }

    #[test]
    fn audio_block_skips_unknown_variants_by_declared_length() {
        let block = vec![1, 0, 0, 0, 99, 0, 0, 0, 3, 0, 0, 0, 9, 8, 7];
        validate_audio_block(&block).unwrap();
    }

    #[test]
    fn v6_has_no_audio_block_and_v7_offset_is_validated() {
        assert!(audio_block_from_s2r_bytes(&header_with_version(6))
            .unwrap()
            .is_none());

        let audio = encode_audio_block(&[]).unwrap();
        let mut bytes = header_with_version(7);
        bytes[AUDIO_OFFSET_HEADER_BYTE..AUDIO_OFFSET_HEADER_BYTE + 4]
            .copy_from_slice(&(HEADER_SIZE).to_le_bytes());
        bytes.extend_from_slice(&audio);
        assert_eq!(
            audio_block_from_s2r_bytes(&bytes).unwrap(),
            Some(audio.as_slice())
        );

        bytes[AUDIO_OFFSET_HEADER_BYTE..AUDIO_OFFSET_HEADER_BYTE + 4]
            .copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(audio_block_from_s2r_bytes(&bytes).is_err());
    }

    #[test]
    fn cosmetic_schema_two_catalog_is_normalized_contiguous_and_json_safe() {
        let observation = WeaponCosmeticObservation {
            item_definition_index: Some(7),
            item_id: Some(0x0102_0304_0506_0708),
            paint_kit_id: Some(180),
            paint_seed: Some(42),
            wear: Some(0.123),
            quality: Some(4),
            stattrak: Some(1337),
            custom_name: Some("fixture AK".to_string()),
            stickers: vec![
                StickerObservation {
                    sticker_id: 123,
                    wear: Some(0.5),
                    slot: Some(4),
                    scale: Some(f32::NAN),
                    rotation: Some(f32::INFINITY),
                    offset_x: Some(-0.2),
                    offset_y: Some(0.3),
                    schema: Some(2),
                },
                StickerObservation {
                    sticker_id: 0,
                    wear: Some(0.1),
                    slot: Some(5),
                    scale: Some(0.2),
                    rotation: Some(0.3),
                    offset_x: None,
                    offset_y: None,
                    schema: None,
                },
            ],
            keychain: Some(KeychainObservation {
                keychain_id: 17,
                offset_x: Some(0.1),
                offset_y: Some(0.2),
                offset_z: Some(0.3),
                seed: Some(42),
                highlight: Some(7),
                sticker_id: Some(456),
                display_case_keychain_id: Some(37),
            }),
        };
        let glove = GloveCosmeticObservation {
            item_definition_index: Some(5030),
            item_id: Some(99),
            paint_kit_id: Some(10018),
            paint_seed: Some(9),
            wear: Some(0.6),
            quality: Some(3),
        };
        let mut records = HashMap::new();
        records.insert(
            2,
            vec![TickRecord {
                alive: 1,
                weapon_cosmetic: Some(observation.clone()),
                glove_cosmetic: Some(glove.clone()),
                ..Default::default()
            }],
        );
        records.insert(
            1,
            vec![TickRecord {
                alive: 1,
                weapon_cosmetic: Some(observation),
                glove_cosmetic: Some(glove),
                ..Default::default()
            }],
        );

        let catalog = CosmeticCatalog::from_records(&[1, 2], &records, &[]);
        assert_eq!(catalog.weapon_signatures.len(), 2);
        assert_eq!(catalog.glove_signatures.len(), 2);
        assert_eq!(
            catalog.weapon_index(records[&1][0].weapon_cosmetic.as_ref()),
            1
        );
        assert_eq!(
            catalog.glove_index(records[&1][0].glove_cosmetic.as_ref()),
            1
        );

        let metadata = serde_json::to_value(catalog.metadata()).unwrap();
        assert!(metadata["weapon_signatures"][0].is_null());
        assert_eq!(metadata["weapon_signatures"][1]["index"], 1);
        assert_eq!(metadata["glove_signatures"][1]["index"], 1);
        assert_eq!(
            metadata["weapon_signatures"][1]["stickers"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(metadata["schema_version"], COSMETIC_SCHEMA_VERSION);
        assert_eq!(metadata["weapon_signatures"][1]["stickers"][0]["slot"], 4);
        assert!(metadata["weapon_signatures"][1]["stickers"][0]
            .get("scale")
            .is_none());
        assert!(metadata["weapon_signatures"][1]["stickers"][0]
            .get("rotation")
            .is_none());
        assert_eq!(
            metadata["weapon_signatures"][1]["stickers"][0]["offset_x"].as_f64(),
            Some(f64::from(-0.2f32))
        );
        assert_eq!(
            metadata["weapon_signatures"][1]["keychain"]["keychain_id"],
            17
        );
        assert_eq!(
            metadata["weapon_signatures"][1]["keychain"]["sticker_id"],
            456
        );
    }

    #[test]
    fn cosmetic_schema_version_is_pinned_independently_of_the_container() {
        assert_eq!(S2R_VERSION, 18, "AG2 recipe lane uses container v18");
        assert_eq!(COSMETIC_SCHEMA_VERSION, 2, "cosmetic metadata is schema 2");
    }

    #[test]
    fn v6_frame_fixture_places_cosmetic_indexes_after_the_v5_prefix() {
        // This is a deliberately small deterministic byte builder for any
        // external reader certification: a v6 frame is a v5 39-byte prefix
        // followed by little-endian weapon/glove table indexes.
        let weapon_index = 0x0102_0304u32;
        let glove_index = 0xA0B0_C0D0u32;
        let mut frame = vec![0u8; FRAME_STRIDE as usize];
        frame[39..43].copy_from_slice(&weapon_index.to_le_bytes());
        frame[43..47].copy_from_slice(&glove_index.to_le_bytes());

        assert_eq!(frame.len(), 47);
        assert_eq!(
            u32::from_le_bytes(frame[39..43].try_into().unwrap()),
            weapon_index
        );
        assert_eq!(
            u32::from_le_bytes(frame[43..47].try_into().unwrap()),
            glove_index
        );
    }

    #[test]
    fn missing_required_cosmetic_fields_use_the_zero_sentinel() {
        let catalog = CosmeticCatalog::new();
        assert_eq!(
            catalog.weapon_index(Some(&WeaponCosmeticObservation {
                paint_kit_id: Some(1),
                ..Default::default()
            })),
            0
        );
        assert_eq!(
            catalog.glove_index(Some(&GloveCosmeticObservation {
                item_definition_index: Some(5030),
                ..Default::default()
            })),
            0
        );
    }

    fn header_with_version(version: u16) -> Vec<u8> {
        let mut bytes = vec![0u8; HEADER_SIZE as usize];
        bytes[0..4].copy_from_slice(S2R_MAGIC);
        bytes[4..6].copy_from_slice(&version.to_le_bytes());
        bytes
    }

    fn current_file_bytes(section_count: u16) -> Vec<u8> {
        let sections: Vec<_> = (1..=section_count)
            .map(|tag| (tag, if tag == 12 { 2 } else { 1 }, if tag == 12 {
                super::super::utility::encode(Some(&parser::second_pass::utility::UtilityData {
                    captured: true, ..Default::default() }), 0).unwrap().0
            } else { 0u32.to_le_bytes().to_vec() }, 0))
            .collect();
        let extension = encode_extension_block(&sections).unwrap();
        let mut bytes = header_with_version(S2R_VERSION);
        bytes[54..58].copy_from_slice(&HEADER_SIZE.to_le_bytes());
        bytes[58..62].copy_from_slice(&(extension.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&extension);
        bytes
    }

    #[test]
    fn current_version_file_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.s2r");
        std::fs::write(&path, current_file_bytes(12)).unwrap();

        assert_eq!(read_s2r_version(&path), Some(S2R_VERSION));
        assert!(is_current_s2r(&path));
    }

    #[test]
    fn current_cache_rejects_overlapping_sections_and_legacy_utility_schema() {
        let mut bytes = current_file_bytes(12);
        let first = HEADER_SIZE as usize + 8;
        let offset = bytes[first+4..first+8].to_vec();
        bytes[first+16+4..first+16+8].copy_from_slice(&offset);
        assert!(!has_current_extensions(&bytes));
        let mut bytes = current_file_bytes(12);
        let utility = first + 11*16;
        bytes[utility+2..utility+4].copy_from_slice(&1u16.to_le_bytes());
        assert!(!has_current_extensions(&bytes));
    }

    #[test]
    fn current_version_missing_event_sections_is_not_current() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing-event-sections.s2r");
        std::fs::write(&path, current_file_bytes(5)).unwrap();

        assert_eq!(read_s2r_version(&path), Some(S2R_VERSION));
        assert!(!is_current_s2r(&path));
    }

    #[test]
    fn current_version_missing_fire_bullets_section_is_not_current() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing-fire-bullets-section.s2r");
        std::fs::write(&path, current_file_bytes(7)).unwrap();

        assert_eq!(read_s2r_version(&path), Some(S2R_VERSION));
        assert!(!is_current_s2r(&path));
    }

    #[test]
    fn current_version_missing_player_states_is_not_current() {
        assert!(!has_current_extensions(&current_file_bytes(8)));
        assert!(!has_current_extensions(&current_file_bytes(9)));
    }

    #[test]
    fn collection_modifiers_join_both_players_and_keep_simultaneous_flags() {
        use parser::second_pass::kill_modifiers::*;
        let event = authority_event("player_death", 100, vec![
            ("attacker_steamid", Variant::U64(11)),
            ("user_steamid", Variant::U64(22)),
            ("weapon", Variant::String("awp".into())),
            ("headshot", Variant::Bool(true)), ("thrusmoke", Variant::Bool(true)),
            ("noscope", Variant::Bool(true)), ("attackerblind", Variant::Bool(true)),
            ("penetrated", Variant::I32(2)),
        ]);
        let players = HashMap::from([(11, 0), (22, 1), (33, 2)]);
        let mut kills = vec![
            KillEvent { tick: 100, killer_idx: 0, victim_idx: 1, ..Default::default() },
            KillEvent { tick: 100, killer_idx: 0, victim_idx: 2, ..Default::default() },
        ];
        let mut attacker = TickRecord { tick: 100, alive: 1, ..Default::default() };
        attacker.state.airborne = Some(true);
        let mut victim = TickRecord { tick: 99, alive: 1, ..Default::default() };
        victim.state.airborne = Some(false);
        let records = HashMap::from([(11, vec![attacker]), (22, vec![victim])]);
        enrich_kill_events(&mut kills, &[event], &players, &records);
        assert_eq!(kills[0].kill_flags, HEADSHOT | THROUGH_SMOKE | NOSCOPE | ATTACKER_BLIND | WALLBANG | ATTACKER_AIRBORNE);
        assert_eq!(kills[0].known_flags, 127);
        assert_eq!(kills[0].penetrated, Some(2));
        assert_eq!(kills[1].kill_flags, ATTACKER_AIRBORNE);
        assert_eq!(kills[1].known_flags, ATTACKER_AIRBORNE);
        assert_eq!(kills[1].penetrated, None);
    }

    #[test]
    fn production_writer_exports_modifiers_and_states_for_rounds_and_collections() {
        use super::super::kill_collection_parser::PlayerInfo;
        let collection = Collection {
            collection_type: "SINGLE".into(), collection_num: 1, tick_duration: 2,
            map_name: "de_test".into(), killer_index: 10, killer_team: "T".into(),
            start_kill_tick: 100, end_kill_tick: 100, killer_name: "a".into(), steam_id: 11,
            demo_name: "fixture".into(), folder: String::new(), killer_radius: 0.0,
            victims_radius: 0.0, killer_move_distance: 0.0, victim_team: "CT".into(),
            round_start_tick: 99, round_end_tick: 101, round_freeze_end: 99, round: 1,
            weapons: "[awp]".into(), weapons_id: "[9]".into(), kill_ticks: "[100]".into(),
            victims_index: "[20]".into(), tick_parsed: 0,
        };
        let mut data = index_fixture_data(vec![
            PlayerInfo { player_name: "a".into(), steam_id: 11, killer_index: 10, team: "T".into() },
            PlayerInfo { player_name: "b".into(), steam_id: 22, killer_index: 20, team: "CT".into() },
        ]);
        data.collection_details.insert(1, vec![
            super::super::kill_collection_parser::CollectionDetail {
                collection_type: "SINGLE".into(), kill_tick: 100,
                killer_name: "a".into(), killer_steamid: 11,
                player_team: "T".into(), player_weapon: "awp".into(), player_weapon_id: 9,
                player_pos_x: 1.0, player_pos_y: 2.0, player_pos_z: 3.0,
                player_view_pitch: 0.0, player_view_yaw: 0.0,
                victim_name: "b".into(), victim_steamid: 22, victim_team: "CT".into(),
                victim_pos_x: 4.0, victim_pos_y: 5.0, victim_pos_z: 6.0,
                distance_to_enemy: 5.196, ticks_since_last_kill: 0,
                distance_moved_since_last_kill: 0.0, killer_index: 10, victim_index: 20,
            }
        ]);
        let rows = HashMap::from([(11, vec![TickRecord { tick: 100, alive: 1, ..Default::default() }]),
            (22, vec![TickRecord { tick: 99, alive: 1, ..Default::default() }])]);
        let events = vec![authority_event("player_death", 100, vec![
            ("attacker_steamid", Variant::U64(11)), ("user_steamid", Variant::U64(22)),
            ("weapon", Variant::String("awp".into())), ("headshot", Variant::Bool(true)),
            ("noscope", Variant::Bool(true)), ("thrusmoke", Variant::Bool(true)),
            ("attackerblind", Variant::Bool(false)), ("penetrated", Variant::I32(3)),
            ("attacker_is_airborne", Variant::Bool(true)), ("user_is_airborne", Variant::Bool(false)),
        ])];
        let dir = tempfile::tempdir().unwrap();
        for shared_round in [false, true] {
            let path = dir.path().join(format!("{shared_round}.s2r"));
            write_replay_s2r(&path, &rows, &collection, &data, &[], &[], &[],
                &GrenadeStats::default(), &[], 0, &[], &[], &[], &events, true,
                Some((99, 101)), shared_round, &[], Some(&parser::second_pass::utility::UtilityData { captured: true, ..Default::default() }), &[], &[]).unwrap();
            let bytes = std::fs::read(&path).unwrap();
            assert!(has_current_extensions(&bytes));
            let offset = u32::from_le_bytes(bytes[28..32].try_into().unwrap()) as usize;
            assert_eq!(&bytes[offset..offset + 2], &1u16.to_le_bytes());
            let kill = &bytes[offset + 2..offset + 14];
            assert_eq!(kill[8], 55); // headshot + smoke + noscope + wallbang + airborne
            assert_eq!(kill[9], 127);
            assert_eq!(&kill[10..12], &3u16.to_le_bytes());
            let extension = u32::from_le_bytes(bytes[54..58].try_into().unwrap()) as usize;
            // S2EX section count: tags 1-15 with tag 0 unused. Bumped when a lane is added.
            assert_eq!(u16::from_le_bytes(bytes[extension + 6..extension + 8].try_into().unwrap()), 15);
        }
    }

    #[test]
    fn airborne_fallback_does_not_use_future_dead_or_stale_rows() {
        let players = HashMap::from([(11, 0)]);
        let mut rows = vec![
            TickRecord { tick: 98, alive: 1, ..Default::default() },
            TickRecord { tick: 100, alive: 0, ..Default::default() },
            TickRecord { tick: 101, alive: 1, ..Default::default() },
        ];
        for row in &mut rows { row.state.airborne = Some(true); }
        let mut kills = vec![KillEvent { tick: 100, killer_idx: 0, victim_idx: 255, ..Default::default() }];
        enrich_kill_events(&mut kills, &[], &players, &HashMap::from([(11, rows)]));
        assert_eq!(kills[0].known_flags, 0);
    }

    #[test]
    fn diagnostic_interval_has_source_identity_and_exact_observations_without_collection_or_round() {
        use crate::tick_by_tick::kill_collection_parser::PlayerInfo;
        use super::super::button_press::{ButtonObservation, ButtonSource};
        let mut data = index_fixture_data(vec![PlayerInfo {
            player_name: "Observed player".into(), steam_id: 11, killer_index: 7, team: "T".into(),
        }]);
        data.demo_info.demo_path = "synthetic-source.dem".into();
        data.demo_info.demo_name = "synthetic-source".into();
        data.demo_info.map_name = "de_test".into();
        data.demo_info.game_version = "source-version".into();
        let rows = HashMap::from([(11, vec![
            TickRecord { tick: 100, steamid: 11, alive: 1,
                button_observation: Some(ButtonObservation { mask: 0, source: ButtonSource::MovementPrevious }), ..Default::default() },
            TickRecord { tick: 101, steamid: 11, alive: 1, fire: 1,
                button_observation: Some(ButtonObservation { mask: 1, source: ButtonSource::UserCommandState1 }), ..Default::default() },
            TickRecord { tick: 102, steamid: 11, alive: 1, fire: 1, ..Default::default() },
        ])]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("diagnostic.s2r");
        write_diagnostic_interval_s2r(&path, &rows, &data, &[], &[], &[], &[], &[], &[], &[], (99, 103), &[], None, &[], &[]).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(read_s2r_version(&path), Some(18));
        assert_eq!(u16::from_le_bytes(bytes[36..38].try_into().unwrap()), 47);
        assert_eq!(i32::from_le_bytes(bytes[12..16].try_into().unwrap()), 99);
        let meta_at = u32::from_le_bytes(bytes[20..24].try_into().unwrap()) as usize;
        let meta_len = u32::from_le_bytes(bytes[meta_at..meta_at+4].try_into().unwrap()) as usize;
        let meta: serde_json::Value = serde_json::from_slice(&bytes[meta_at+4..meta_at+4+meta_len]).unwrap();
        assert_eq!(meta["replay_scope"], "diagnostic_interval");
        assert_eq!(meta["source_demo_path"], "synthetic-source.dem");
        assert_eq!(meta["map_name"], "de_test");
        assert_eq!(meta["interval_start_tick"], 99);
        assert_eq!(meta["interval_end_tick"], 103);
        assert!(meta.as_object().unwrap().keys().all(|key|
            !key.starts_with("round") && !key.starts_with("killer") && !key.starts_with("collection")));
        let frames = u32::from_le_bytes(bytes[24..28].try_into().unwrap()) as usize;
        for (index, expected) in [0x2000u16, 0x6400, 0x0400].iter().enumerate() {
            assert_eq!(u16::from_le_bytes(bytes[frames+index*47+26..frames+index*47+28].try_into().unwrap()) & 0x7400, *expected);
        }
        let kills = u32::from_le_bytes(bytes[28..32].try_into().unwrap()) as usize;
        assert_eq!(u16::from_le_bytes(bytes[kills..kills+2].try_into().unwrap()), 0);
    }

    #[test]
    fn diagnostic_interval_rejects_bad_windows_before_creating_output() {
        let data = index_fixture_data(vec![]);
        let rows = HashMap::from([(11, vec![TickRecord { tick: 100, alive: 1, ..Default::default() }])]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("invalid.s2r");
        for interval in [(-1, 101), (102, 101), (0, 65535), (101, 102)] {
            assert!(write_diagnostic_interval_s2r(&path, &rows, &data, &[], &[], &[], &[], &[], &[], &[], interval, &[], None, &[], &[]).is_err());
            assert!(!path.exists());
        }
        let duplicate = HashMap::from([(11, vec![rows[&11][0].clone(), rows[&11][0].clone()])]);
        assert!(write_diagnostic_interval_s2r(&path, &duplicate, &data, &[], &[], &[], &[], &[], &[], &[], (100, 100), &[], None, &[], &[]).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn diagnostic_interval_routes_real_kill_and_fire_actors_without_a_collection_killer() {
        use crate::tick_by_tick::kill_collection_parser::PlayerInfo;
        let data = index_fixture_data(vec![
            PlayerInfo { player_name: "victim".into(), steam_id: 11, killer_index: 7, team: "CT".into() },
            PlayerInfo { player_name: "attacker".into(), steam_id: 22, killer_index: 3, team: "T".into() },
        ]);
        let rows = HashMap::from([
            (11, vec![TickRecord { tick: 100, steamid: 11, alive: 1, ..Default::default() }]),
            (22, vec![TickRecord { tick: 100, steamid: 22, alive: 1, ..Default::default() }]),
        ]);
        let death = |tick| authority_event("player_death", tick, vec![
            ("attacker_steamid", Variant::U64(22)), ("user_steamid", Variant::U64(11)),
            ("weapon", Variant::String("ak47".into())),
        ]);
        let fire = WeaponFireEvent { tick: 100, weapon: "ak47".into(), attacker_steamid: 22,
            hit_players: vec!["victim".into()], victim_steamids: vec![11], damages: vec![100],
            kill_flags: vec![true], impact_tick: Some(100) };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("actors.s2r");
        write_diagnostic_interval_s2r(&path, &rows, &data, &[], &[], &[fire], &[], &[], &[],
            &[death(50), death(100)], (100, 101), &[], None, &[], &[]).unwrap();
        let bytes = std::fs::read(path).unwrap();
        let kills = u32::from_le_bytes(bytes[28..32].try_into().unwrap()) as usize;
        assert_eq!(u16::from_le_bytes(bytes[kills..kills+2].try_into().unwrap()), 1);
        assert_eq!(i32::from_le_bytes(bytes[kills+2..kills+6].try_into().unwrap()), 100);
        assert_eq!(&bytes[kills+6..kills+8], &[1, 0]); // Sorted SteamID indices, not old entity indices.
        let shots = u32::from_le_bytes(bytes[42..46].try_into().unwrap()) as usize;
        assert_eq!(u16::from_le_bytes(bytes[shots..shots+2].try_into().unwrap()), 1);
        assert_eq!(i32::from_le_bytes(bytes[shots+2..shots+6].try_into().unwrap()), 100);
        assert_eq!(bytes[shots+10], 1); // Actual decoded attacker22.
        assert_eq!(bytes[shots+14], 0); // Victim11.
    }

    /// An output written by an older revision must not suppress regeneration.
    #[test]
    fn older_version_file_is_not_current() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.s2r");
        std::fs::write(&path, header_with_version(S2R_VERSION - 1)).unwrap();

        assert!(!is_current_s2r(&path));
    }

    #[test]
    fn missing_truncated_and_foreign_files_are_not_current() {
        let dir = tempfile::tempdir().unwrap();

        let missing = dir.path().join("absent.s2r");
        assert!(!is_current_s2r(&missing));

        // Survivor of an interrupted write: correct magic, too short to be a header.
        let truncated = dir.path().join("truncated.s2r");
        std::fs::write(&truncated, b"S2RF\x05\x00").unwrap();
        assert!(!is_current_s2r(&truncated));

        let foreign = dir.path().join("foreign.s2r");
        std::fs::write(&foreign, vec![0u8; HEADER_SIZE as usize]).unwrap();
        assert!(!is_current_s2r(&foreign));
    }
}
