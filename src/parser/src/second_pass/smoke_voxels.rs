//! Tick-ordered reconstruction of CS2 smoke voxel network vectors.

use crate::first_pass::sendtables::Field;
use crate::second_pass::path_ops::FieldPath;
use crate::second_pass::variants::Variant;
use std::collections::BTreeMap;

pub const SMOKE_VOXEL_PROPERTY: &str = "m_VoxelFrameData";

#[derive(Debug, Clone, PartialEq)]
pub struct SmokeVoxelFrame {
    pub tick: i32,
    pub update: u32,
    pub flags: u8,
    pub effect_tick_begin: i32,
    pub detonation_position: [f32; 3],
    pub color: [f32; 3],
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SmokeVoxelTrack {
    pub entity_id: i32,
    pub life_index: u16,
    pub start_tick: i32,
    pub end_tick: i32,
    pub frames: Vec<SmokeVoxelFrame>,
}

#[derive(Debug)]
struct ActiveSmoke {
    life_index: u16,
    start_tick: i32,
    vector: Vec<u8>,
    frame_size: usize,
    update: u32,
    last_emitted_update: u32,
    flags: u8,
    effect_tick_begin: i32,
    detonation_position: [f32; 3],
    color: [f32; 3],
    frames: Vec<SmokeVoxelFrame>,
}

impl ActiveSmoke {
    fn new(life_index: u16, tick: i32) -> Self {
        Self {
            life_index,
            start_tick: tick,
            vector: Vec::new(),
            frame_size: 0,
            update: 0,
            last_emitted_update: 0,
            flags: 0,
            effect_tick_begin: 0,
            detonation_position: [0.0; 3],
            color: [255.0; 3],
            frames: Vec::new(),
        }
    }

    fn flush(&mut self, tick: i32) {
        if self.update == 0 || self.update == self.last_emitted_update {
            return;
        }
        if self.frame_size > self.vector.len() {
            return;
        }
        self.frames.push(SmokeVoxelFrame {
            tick,
            update: self.update,
            flags: self.flags,
            effect_tick_begin: self.effect_tick_begin,
            detonation_position: self.detonation_position,
            color: self.color,
            payload: self.vector[..self.frame_size].to_vec(),
        });
        self.last_emitted_update = self.update;
    }
}

#[derive(Debug, Default)]
pub struct SmokeVoxelCapture {
    enabled: bool,
    active: BTreeMap<i32, ActiveSmoke>,
    life_counts: BTreeMap<i32, u16>,
    tracks: Vec<SmokeVoxelTrack>,
}

impl SmokeVoxelCapture {
    pub fn new(enabled: bool) -> Self {
        Self { enabled, ..Self::default() }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn begin(&mut self, entity_id: i32, tick: i32) {
        if !self.enabled {
            return;
        }
        self.finish_entity(entity_id, tick);
        let life_index = self.life_counts.entry(entity_id).or_insert(0);
        let current = *life_index;
        *life_index = life_index.saturating_add(1);
        self.active.insert(entity_id, ActiveSmoke::new(current, tick));
    }

    pub fn finish_entity(&mut self, entity_id: i32, tick: i32) {
        let Some(mut smoke) = self.active.remove(&entity_id) else {
            return;
        };
        smoke.flush(tick);
        self.tracks.push(SmokeVoxelTrack {
            entity_id,
            life_index: smoke.life_index,
            start_tick: smoke.start_tick,
            end_tick: tick,
            frames: smoke.frames,
        });
    }

    pub fn flush_tick(&mut self, tick: i32) {
        if !self.enabled {
            return;
        }
        for smoke in self.active.values_mut() {
            smoke.flush(tick);
        }
    }

    pub fn finish(mut self, tick: i32) -> Vec<SmokeVoxelTrack> {
        self.flush_tick(tick);
        let ids: Vec<i32> = self.active.keys().copied().collect();
        for id in ids {
            self.finish_entity(id, tick);
        }
        self.tracks.retain(|track| !track.frames.is_empty());
        self.tracks.sort_by_key(|track| (track.start_tick, track.entity_id, track.life_index));
        self.tracks
    }

    pub fn observe(&mut self, entity_id: i32, class_name: &str, field: &Field, path: &FieldPath, value: &Variant) {
        if !self.enabled || !class_name.contains("SmokeGrenadeProjectile") {
            return;
        }
        let Some(smoke) = self.active.get_mut(&entity_id) else {
            return;
        };
        let Some((field_name, vector_length)) = field_name(field) else {
            return;
        };
        match field_name.rsplit('.').next().unwrap_or(field_name) {
            "m_VoxelFrameData" if vector_length => {
                let Some(length) = as_usize(value) else {
                    return;
                };
                smoke.vector.resize(length, 0);
                smoke.frame_size = smoke.frame_size.min(length);
            }
            "m_VoxelFrameData" => {
                let Some(byte) = as_u32(value).and_then(|v| u8::try_from(v).ok()) else {
                    return;
                };
                let index = path.path[path.last];
                if index >= 0 {
                    let index = index as usize;
                    // Some POV demos update indexed bytes and m_nVoxelFrameDataSize without a
                    // separate vector-length field-path operation. The byte index is itself
                    // authoritative; retain it rather than silently dropping the whole stream.
                    write_indexed_byte(&mut smoke.vector, index, byte);
                }
            }
            "m_nVoxelFrameDataSize" => {
                if let Some(size) = as_usize(value) {
                    smoke.frame_size = size;
                    if smoke.vector.len() < size {
                        smoke.vector.resize(size, 0);
                    }
                }
            }
            "m_nVoxelUpdate" => {
                if let Some(update) = as_u32(value) {
                    smoke.update = update;
                }
            }
            "m_nSmokeEffectTickBegin" => {
                if let Some(tick) = as_i32(value) {
                    smoke.effect_tick_begin = tick;
                }
            }
            "m_vSmokeDetonationPos" => {
                if let Variant::VecXYZ(position) = value {
                    smoke.detonation_position = *position;
                }
            }
            "m_vSmokeColor" => {
                if let Variant::VecXYZ(color) = value {
                    smoke.color = *color;
                }
            }
            _ => {}
        }
    }
}

fn field_name(field: &Field) -> Option<(&str, bool)> {
    match field {
        Field::Value(value) => Some((&value.full_name, false)),
        Field::Vector(_) => match field.get_inner(0).ok()? {
            Field::Value(value) => Some((&value.full_name, true)),
            _ => None,
        },
        _ => None,
    }
}

fn as_u32(value: &Variant) -> Option<u32> {
    match value {
        Variant::U32(value) => Some(*value),
        Variant::I32(value) => u32::try_from(*value).ok(),
        Variant::U64(value) => u32::try_from(*value).ok(),
        _ => None,
    }
}

fn as_i32(value: &Variant) -> Option<i32> {
    match value {
        Variant::I32(value) => Some(*value),
        Variant::U32(value) => i32::try_from(*value).ok(),
        Variant::U64(value) => i32::try_from(*value).ok(),
        _ => None,
    }
}

fn as_usize(value: &Variant) -> Option<usize> {
    as_u32(value).and_then(|value| usize::try_from(value).ok())
}

fn write_indexed_byte(vector: &mut Vec<u8>, index: usize, byte: u8) {
    if index >= vector.len() {
        vector.resize(index + 1, 0);
    }
    vector[index] = byte;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_capture_finishes_without_tracks() {
        assert!(SmokeVoxelCapture::new(true).finish(10).is_empty());
    }

    #[test]
    fn accepts_current_sendtable_unsigned_counters_without_truncation() {
        assert_eq!(as_u32(&Variant::U64(42)), Some(42));
        assert_eq!(as_i32(&Variant::U64(42)), Some(42));
        assert_eq!(as_u32(&Variant::U64(u32::MAX as u64 + 1)), None);
        assert_eq!(as_i32(&Variant::U64(i32::MAX as u64 + 1)), None);
    }

    #[test]
    fn indexed_bytes_grow_a_vector_when_pov_demo_omits_length_operation() {
        let mut vector = Vec::new();
        write_indexed_byte(&mut vector, 3, 0x7f);
        assert_eq!(vector, vec![0, 0, 0, 0x7f]);
    }
}
