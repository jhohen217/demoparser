//! Lossless capture of the networked AnimGraph2 pose-recipe fields.
//!
//! This lane records serialized task recipes from player pawns. It does not decode task payloads
//! and is deliberately separate from DEM_AnimationData / DEM_AnimationHeader records.

use crate::first_pass::sendtables::Field;
use crate::second_pass::path_ops::FieldPath;
use crate::second_pass::variants::Variant;
use std::collections::BTreeMap;

const MAX_VECTOR_BYTES: usize = 4096;
const MAX_SLOTS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ag2RecipeSnapshot {
    pub tick: i32,
    pub entity_id: u32,
    pub entity_serial: u32,
    /// Ordinal of this observed entity-index lifetime within the parser segment.
    pub life_index: u32,
    pub active_slot: Option<u32>,
    pub recipe_version: Option<i32>,
    pub graph_definition: Option<u64>,
    pub graph_iteration: Option<u32>,
    /// Slot index order is preserved. An empty entry means the source had no topology for it.
    pub topologies: Vec<Vec<u8>>,
    /// Complete serialized task payload vector for the active slot.
    pub dynamic: Vec<u8>,
}

#[derive(Debug, Default, Clone)]
struct RecipeState {
    entity_serial: u32,
    life_index: u32,
    active_slot: Option<u32>,
    recipe_version: Option<i32>,
    graph_definition: Option<u64>,
    graph_iteration: Option<u32>,
    topologies: BTreeMap<usize, Vec<u8>>,
    topology_lengths: BTreeMap<usize, usize>,
    dynamic: Vec<u8>,
    dynamic_length: Option<usize>,
    touched: bool,
}

#[derive(Debug, Default)]
pub struct Ag2RecipeCapture {
    disabled: bool,
    active: BTreeMap<u32, RecipeState>,
    life_counts: BTreeMap<u32, u32>,
    rows: BTreeMap<(i32, u32, u32, u32), Ag2RecipeSnapshot>,
}

impl Ag2RecipeCapture {
    pub fn new(enabled: bool) -> Self {
        Self { disabled: !enabled, ..Self::default() }
    }

    pub fn begin(&mut self, entity_id: i32, serial: u32, class_name: &str) {
        if self.disabled { return; }
        let Some(id) = u32::try_from(entity_id).ok() else { return };
        if !is_player_pawn(class_name) { return; }
        let life_index = self.life_counts.entry(id).or_insert(0);
        let current_life = *life_index;
        *life_index = life_index.saturating_add(1);
        self.active.insert(id, RecipeState {
            entity_serial: serial,
            life_index: current_life,
            ..RecipeState::default()
        });
    }

    pub fn end(&mut self, entity_id: i32, serial: u32, tick: i32) {
        let Some(id) = u32::try_from(entity_id).ok() else { return; };
        if self.active.get(&id).is_some_and(|state| state.entity_serial == serial) {
            // Keep the final known state at the lifecycle boundary. If a replacement starts
            // at this same tick, the S2R encoder coalesces by entity+serial and keeps the
            // snapshot with the newer capture-life ordinal.
            self.capture_snapshot(id, tick);
            self.active.remove(&id);
        }
    }

    pub fn observe(&mut self, entity_id: i32, serial: u32, class_name: &str, _tick: i32,
        field: &Field, path: &FieldPath, value: &Variant)
    {
        if self.disabled { return; }
        if !is_player_pawn(class_name) { return; }
        let Some(id) = u32::try_from(entity_id).ok() else { return; };
        let Some(state) = self.active.get_mut(&id) else { return; };
        if state.entity_serial != serial { return; }
        let Some((name, is_vector)) = field_name(field) else { return; };
        let short = name.rsplit('.').next().unwrap_or(name);
        match short {
            "m_nSerializePoseRecipeAG2ActiveSlot" => { state.active_slot = as_u32(value); state.touched = true; }
            "m_nSerializePoseRecipeVersionAG2" => { state.recipe_version = as_i32(value); state.touched = true; }
            "m_hGraphDefinitionAG2" => { state.graph_definition = as_u64(value); state.touched = true; }
            "m_nServerGraphInstanceIteration" => { state.graph_iteration = as_u32(value); state.touched = true; }
            "m_SerializePoseRecipeAG2Dynamic" => {
                if is_vector {
                    if let Some(length) = as_usize(value).filter(|n| *n <= MAX_VECTOR_BYTES) {
                        state.dynamic.resize(length, 0);
                        state.dynamic_length = Some(length);
                    }
                } else if let (Some(index), Some(byte)) = (path_index(path), as_u8(value)) {
                    write_byte(&mut state.dynamic, index, byte);
                    state.dynamic_length = Some(state.dynamic.len());
                } else if let Some(bytes) = as_bytes(value) {
                    state.dynamic = bytes;
                    state.dynamic_length = Some(state.dynamic.len());
                }
                state.touched = true;
            }
            "m_topology" if name.contains("AnimGraph2SerializedPoseRecipeSlot_t") => {
                if let Some(slot) = topology_slot(path) {
                    if is_vector {
                        if let Some(length) = as_usize(value).filter(|n| *n <= MAX_VECTOR_BYTES) {
                            state.topology_lengths.insert(slot, length);
                            state.topologies.entry(slot).or_default().resize(length, 0);
                        }
                    } else if let Some(bytes) = as_bytes(value) {
                        state.topology_lengths.insert(slot, bytes.len());
                        state.topologies.insert(slot, bytes);
                    } else if let (Some(index), Some(byte)) = (topology_byte_index(path), as_u8(value)) {
                        write_byte(state.topologies.entry(slot).or_default(), index, byte);
                        state.topology_lengths.entry(slot).or_insert_with(|| state.topologies[&slot].len());
                    }
                    state.touched = true;
                }
            }
            _ => return,
        }
        // Only one row per tick/lifetime survives in `rows`. Materialize it at the
        // tick/lifecycle boundary instead of cloning all slots after every byte update.
        // read_frame flushes before changing ticks; end flushes before removing a pawn.
    }

    /// Emit one complete latest-state snapshot per active recipe entity at each source tick.
    pub fn flush_tick(&mut self, tick: i32) {
        let ids: Vec<u32> = self.active.iter()
            .filter(|(_, state)| state.touched)
            .map(|(id, _)| *id).collect();
        for id in ids {
            self.capture_snapshot(id, tick);
            if let Some(state) = self.active.get_mut(&id) { state.touched = false; }
        }
    }

    pub fn finish(mut self, tick: i32) -> Vec<Ag2RecipeSnapshot> {
        self.flush_tick(tick);
        self.rows.into_values().collect()
    }

    fn capture_snapshot(&mut self, id: u32, tick: i32) {
        let Some(state) = self.active.get(&id) else { return; };
        if !state.touched || state.dynamic.is_empty() { return; }
        let max_slot = state.topologies.keys().copied().max();
        let Some(max_slot) = max_slot else { return; };
        if max_slot >= MAX_SLOTS { return; }
        let topologies: Vec<Vec<u8>> = (0..=max_slot)
            .map(|slot| {
                let mut bytes = state.topologies.get(&slot).cloned().unwrap_or_default();
                if let Some(length) = state.topology_lengths.get(&slot) {
                    bytes.truncate(*length);
                    bytes.resize(*length, 0);
                }
                bytes
            }).collect();
        let row = Ag2RecipeSnapshot {
            tick, entity_id: id, entity_serial: state.entity_serial, life_index: state.life_index,
            active_slot: state.active_slot, recipe_version: state.recipe_version,
            graph_definition: state.graph_definition, graph_iteration: state.graph_iteration,
            topologies, dynamic: state.dynamic.clone(),
        };
        self.rows.insert((tick, id, row.entity_serial, row.life_index), row);
    }
}

fn is_player_pawn(class_name: &str) -> bool {
    class_name.contains("PlayerPawn") || class_name.contains("Player pawn")
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
    match value { Variant::U32(v) => Some(*v), Variant::I32(v) => u32::try_from(*v).ok(), Variant::U64(v) => u32::try_from(*v).ok(), _ => None }
}
fn as_u64(value: &Variant) -> Option<u64> {
    match value { Variant::U64(v) => Some(*v), Variant::U32(v) => Some(u64::from(*v)), Variant::I32(v) => u64::try_from(*v).ok(), _ => None }
}
fn as_i32(value: &Variant) -> Option<i32> {
    match value { Variant::I32(v) => Some(*v), Variant::U32(v) => i32::try_from(*v).ok(), Variant::U64(v) => i32::try_from(*v).ok(), _ => None }
}
fn as_usize(value: &Variant) -> Option<usize> { as_u32(value).and_then(|v| usize::try_from(v).ok()) }
fn as_u8(value: &Variant) -> Option<u8> { as_u32(value).and_then(|v| u8::try_from(v).ok()) }
fn as_bytes(value: &Variant) -> Option<Vec<u8>> {
    if let Variant::Binary(bytes) = value {
        return (bytes.len() <= MAX_VECTOR_BYTES).then(|| bytes.clone());
    }
    let values: Vec<u32> = match value {
        Variant::U32Vec(values) => values.clone(),
        Variant::U32(value) => vec![*value],
        _ => return None,
    };
    if values.len() > MAX_VECTOR_BYTES { return None; }
    values.into_iter().map(u8::try_from).collect::<Result<Vec<_>, _>>().ok()
}
fn path_index(path: &FieldPath) -> Option<usize> {
    let index = *path.path.get(path.last)?;
    (index >= 0).then_some(index as usize).filter(|index| *index < MAX_VECTOR_BYTES)
}
fn topology_slot(path: &FieldPath) -> Option<usize> {
    path.path.get(2).copied().filter(|slot| *slot >= 0).map(|slot| slot as usize).filter(|slot| *slot < MAX_SLOTS)
}
fn topology_byte_index(path: &FieldPath) -> Option<usize> {
    let index = if path.last >= 4 { path.path[4] } else { path.path[path.last] };
    (index >= 0).then_some(index as usize).filter(|index| *index < MAX_VECTOR_BYTES)
}
fn write_byte(bytes: &mut Vec<u8>, index: usize, value: u8) {
    if index >= MAX_VECTOR_BYTES { return; }
    if bytes.len() <= index { bytes.resize(index + 1, 0); }
    bytes[index] = value;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_updates_coalesce_without_losing_tick_or_lifetime_boundaries() {
        use crate::first_pass::sendtables::ValueField;
        use crate::second_pass::decoder::Decoder;
        let field = Field::Value(ValueField {
            decoder: Decoder::UnsignedDecoder, name: "m_SerializePoseRecipeAG2Dynamic".into(),
            full_name: "CCSPlayerPawn.m_SerializePoseRecipeAG2Dynamic".into(),
            should_parse: true, prop_id: 0,
        });
        let mut capture = Ag2RecipeCapture::default();
        capture.begin(7, 12, "CCSPlayerPawn");
        capture.active.get_mut(&7).unwrap().topologies.insert(0, vec![1, 2, 3]);
        for byte in 0..32 {
            let path = FieldPath { last: 0, path: [byte, 0, 0, 0, 0, 0, 0] };
            capture.observe(7, 12, "CCSPlayerPawn", 20, &field, &path, &Variant::U32(byte as u32));
        }
        assert!(capture.rows.is_empty(), "byte writes must not clone partial recipes");
        capture.flush_tick(20);
        assert_eq!(capture.rows.values().next().unwrap().dynamic, (0..32).collect::<Vec<u8>>());
        let path = FieldPath { last: 0, path: [0, 0, 0, 0, 0, 0, 0] };
        capture.observe(7, 12, "CCSPlayerPawn", 21, &field, &path, &Variant::U32(99));
        capture.end(7, 12, 21);
        let rows = capture.finish(21);
        assert_eq!(rows.len(), 2);
        assert_eq!((rows[0].tick, rows[1].tick), (20, 21));
        assert_eq!(rows[0].dynamic[0], 0);
        assert_eq!(rows[1].dynamic[0], 99);
        assert_eq!(rows[1].entity_serial, 12);
    }

    #[test]
    fn disabled_capture_does_not_retain_pawn_lifetimes() {
        let mut capture = Ag2RecipeCapture::new(false);
        capture.begin(7, 12, "CCSPlayerPawn");
        capture.begin(7, 13, "CCSPlayerPawn");
        capture.end(7, 13, 20);
        capture.flush_tick(20);
        assert!(capture.active.is_empty());
        assert!(capture.life_counts.is_empty());
        assert!(capture.finish(20).is_empty());
    }

    #[test]
    fn unknown_or_incomplete_recipe_is_not_emitted() {
        let mut capture = Ag2RecipeCapture::default();
        capture.begin(7, 12, "CCSPlayerPawn");
        capture.flush_tick(10);
        assert!(capture.finish(10).is_empty());
    }

    #[test]
    fn complete_snapshots_keep_all_slots_and_replace_same_tick_state() {
        let mut capture = Ag2RecipeCapture::default();
        let mut state = RecipeState { entity_serial: 12, life_index: 0, active_slot: Some(1),
            recipe_version: Some(3), graph_definition: Some(55), graph_iteration: Some(8),
            topologies: BTreeMap::from([(0, vec![1, 2]), (1, vec![3])]),
            dynamic: vec![9, 10], touched: true, ..RecipeState::default() };
        capture.active.insert(7, state.clone());
        capture.capture_snapshot(7, 20);
        state.dynamic = vec![11, 12, 13];
        capture.active.insert(7, state);
        capture.capture_snapshot(7, 20);
        let rows = capture.finish(20);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].topologies, vec![vec![1, 2], vec![3]]);
        assert_eq!(rows[0].dynamic, vec![11, 12, 13]);
        assert_eq!(rows[0].graph_definition, Some(55));
    }

    #[test]
    fn binary_blocks_preserve_non_utf8_bytes_and_graph_handles_keep_64_bits() {
        let raw = vec![0x00, 0xff, 0xc3, 0x28, 0x80];
        assert_eq!(as_bytes(&Variant::Binary(raw.clone())), Some(raw));
        assert_eq!(as_u64(&Variant::U64(9_785_864_542_908_430_973)), Some(9_785_864_542_908_430_973));
    }
}
