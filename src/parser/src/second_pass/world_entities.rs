//! World-entity lane: the doors, breakables, movers and props a map spawns, and what the demo
//! says happens to them during a round.
//!
//! Shape and contents follow the de_nuke audit
//! (`docs/DemosDirectParsing/world-entity-audit-2026-09-17.md` in the viewer repo), which
//! established three things this lane is built around:
//!
//! - A door's animation is fully networked: `m_angRotation` every tick of a swing, plus
//!   `m_eDoorState` and `m_flSimulationTime`.
//! - A break is an entity **delete**. There is no health, no broken flag, no model swap, and no
//!   game event — so the delete tick is the only record that a vent stopped existing.
//! - The spawn origin, decoded from the cell coordinates, matches the map's authored entity
//!   origin exactly. It is the only key that ties a networked entity to the scene node a viewer
//!   builds from the entity lump, so it is mandatory on every record.
//!
//! Off unless requested, like the weapon-entity lane.

use crate::first_pass::sendtables::Field;
use crate::second_pass::variants::Variant;
use std::collections::BTreeMap;

/// Magic wanted-player-prop name that turns this lane on.
pub const WORLD_ENTITY_PROPERTY: &str = "world_entities";

/// Cell-coordinate decoding, matching `collect_data.rs`: `cell * 2^CELL_BITS - MAX_COORD + offset`.
const CELL_BITS: i32 = 9;
const MAX_COORD: f32 = (1 << 14) as f32;

/// What kind of world entity a record is. Deliberately coarse: the viewer needs to know whether
/// to expect a swing or only a disappearance, not to re-derive Source's class hierarchy.
pub mod class {
    pub const OTHER: u8 = 0;
    pub const DOOR_ROTATING: u8 = 1;
    pub const BREAKABLE: u8 = 2;
    pub const DYNAMIC_PROP: u8 = 3;
    pub const PHYSICS_PROP: u8 = 4;
    pub const FUNC_BRUSH: u8 = 5;
    pub const FUNC_WATER: u8 = 6;
    pub const BUTTON: u8 = 7;
}

pub mod operation {
    pub const SPAWN: u8 = 0;
    pub const UPDATE: u8 = 1;
    pub const DELETE: u8 = 2;
}

pub mod field_flag {
    pub const ORIGIN: u16 = 1 << 0;
    pub const ANGLES: u16 = 1 << 1;
    pub const DOOR_STATE: u16 = 1 << 2;
    pub const SIMULATION_TIME: u16 = 1 << 3;
    pub const MODEL: u16 = 1 << 4;
    /// The entity left the PVS rather than being removed. A dormant door is not a broken one.
    pub const DORMANT: u16 = 1 << 5;
}

/// One create, change or removal of a world entity.
///
/// `flags` says which of the value fields this row actually carries; a field whose bit is clear
/// is unchanged, not zero. A `SPAWN` row carries everything known at creation.
#[derive(Debug, Clone, PartialEq)]
pub struct WorldEntityDelta {
    pub tick: i32,
    pub entity_id: i32,
    pub serial: u32,
    pub class_id: u8,
    pub operation: u8,
    pub flags: u16,
    pub origin: [f32; 3],
    pub angles: [f32; 3],
    pub simulation_time: f32,
    pub door_state: u8,
    pub model: u64,
}

#[derive(Debug, Clone, Default)]
struct EntityState {
    class_id: u8,
    create_tick: i32,
    cell: [Option<u32>; 3],
    offset: [Option<f32>; 3],
    origin: Option<[f32; 3]>,
    angles: Option<[f32; 3]>,
    door_state: Option<u8>,
    simulation_time: Option<f32>,
    model: Option<u64>,
    /// Emitted lazily: an entity's origin usually arrives in six separate field updates, so the
    /// spawn row cannot be written at create time.
    spawn_emitted: bool,
}

#[derive(Debug, Default)]
pub struct WorldEntityCapture {
    enabled: bool,
    active: BTreeMap<(i32, u32), EntityState>,
    deltas: Vec<WorldEntityDelta>,
}

impl WorldEntityCapture {
    pub fn new(enabled: bool) -> Self {
        Self { enabled, ..Self::default() }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn begin(&mut self, entity_id: i32, serial: u32, class_name: &str, tick: i32) {
        if !self.enabled {
            return;
        }
        let class_id = classify(class_name);
        if class_id == class::OTHER {
            return;
        }
        // A create replaces whatever the slot held. Index and serial repeat across a round
        // restart, and carrying the previous entity's pose into the new one would invent motion.
        self.active.insert(
            (entity_id, serial),
            EntityState { class_id, create_tick: tick, ..EntityState::default() },
        );
    }

    /// `dormant` distinguishes a PVS departure from a removal: only the latter is a break.
    pub fn end(&mut self, entity_id: i32, serial: u32, class_name: &str, tick: i32, dormant: bool) {
        if !self.enabled {
            return;
        }
        let class_id = classify(class_name);
        if class_id == class::OTHER {
            return;
        }
        // Flush the spawn first: an entity removed before its state was ever completed still has
        // to be placeable, or the viewer cannot know what disappeared.
        self.flush_spawn(entity_id, serial, tick);
        // The removal row carries the entity's last known pose. Origin is the key a viewer binds
        // by, so a delete that omitted it would name nothing.
        let row = match self.active.get(&(entity_id, serial)) {
            Some(entry) => {
                let mut flags = if dormant { field_flag::DORMANT } else { 0 };
                if entry.origin.is_some() {
                    flags |= field_flag::ORIGIN;
                }
                if entry.angles.is_some() {
                    flags |= field_flag::ANGLES;
                }
                row_from(entity_id, serial, tick, operation::DELETE, flags, entry)
            }
            None => WorldEntityDelta {
                tick,
                entity_id,
                serial,
                class_id,
                operation: operation::DELETE,
                flags: if dormant { field_flag::DORMANT } else { 0 },
                origin: [0.0; 3],
                angles: [0.0; 3],
                simulation_time: 0.0,
                door_state: 0,
                model: 0,
            },
        };
        self.deltas.push(row);
        if !dormant {
            self.active.remove(&(entity_id, serial));
        }
    }

    /// `is_delta` is false for a baseline or a re-sent full packet. Those seed state; only a
    /// delta update is evidence that something moved.
    pub fn observe(
        &mut self,
        entity_id: i32,
        serial: u32,
        class_name: &str,
        tick: i32,
        field: &Field,
        value: &Variant,
        is_delta: bool,
    ) {
        if !self.enabled {
            return;
        }
        let Some(full_name) = field_name(field) else {
            return;
        };
        let short = full_name.rsplit('.').next().unwrap_or(full_name);
        if !is_tracked_field(short) {
            return;
        }
        let class_id = classify(class_name);
        if class_id == class::OTHER {
            return;
        }
        let entry = self.active.entry((entity_id, serial)).or_insert_with(|| EntityState {
            class_id,
            create_tick: tick,
            ..EntityState::default()
        });
        let before = (entry.origin, entry.angles, entry.door_state, entry.simulation_time, entry.model);
        apply(entry, short, value);
        if !entry.spawn_emitted {
            // Everything before the spawn row is part of the spawn, whether it came from a
            // baseline or a delta.
            return;
        }
        if !is_delta {
            return;
        }
        let mut flags = 0u16;
        if entry.origin != before.0 {
            flags |= field_flag::ORIGIN;
        }
        if entry.angles != before.1 {
            flags |= field_flag::ANGLES;
        }
        if entry.door_state != before.2 {
            flags |= field_flag::DOOR_STATE;
        }
        if entry.simulation_time != before.3 {
            flags |= field_flag::SIMULATION_TIME;
        }
        if entry.model != before.4 {
            flags |= field_flag::MODEL;
        }
        if flags == 0 {
            return;
        }
        let row = row_from(entity_id, serial, tick, operation::UPDATE, flags, entry);
        // Several fields of one change arrive as separate updates in the same tick. Fold them
        // into the row already written for that tick rather than emitting a row per field.
        match self.deltas.last_mut() {
            Some(last)
                if last.tick == tick
                    && last.entity_id == entity_id
                    && last.serial == serial
                    && last.operation == operation::UPDATE =>
            {
                last.flags |= row.flags;
                last.origin = row.origin;
                last.angles = row.angles;
                last.simulation_time = row.simulation_time;
                last.door_state = row.door_state;
                last.model = row.model;
            }
            _ => self.deltas.push(row),
        }
    }

    /// Called as a tick closes, with the tick that just ended. Every entity created during it
    /// now has its complete spawn state, so its spawn row can be written and later ticks can
    /// report changes against it.
    pub fn flush_tick(&mut self, tick: i32) {
        if !self.enabled {
            return;
        }
        let pending: Vec<(i32, u32)> = self
            .active
            .iter()
            .filter(|(_, state)| !state.spawn_emitted)
            .map(|(key, _)| *key)
            .collect();
        for (entity_id, serial) in pending {
            self.flush_spawn(entity_id, serial, tick);
        }
    }

    pub fn finish(mut self, tick: i32) -> Vec<WorldEntityDelta> {
        if !self.enabled {
            return Vec::new();
        }
        let pending: Vec<(i32, u32)> = self
            .active
            .iter()
            .filter(|(_, state)| !state.spawn_emitted)
            .map(|(key, _)| *key)
            .collect();
        for (entity_id, serial) in pending {
            self.flush_spawn(entity_id, serial, tick);
        }
        sort(&mut self.deltas);
        self.deltas
    }

    fn flush_spawn(&mut self, entity_id: i32, serial: u32, _tick: i32) {
        let Some(entry) = self.active.get_mut(&(entity_id, serial)) else {
            return;
        };
        if entry.spawn_emitted {
            return;
        }
        entry.spawn_emitted = true;
        let mut flags = 0u16;
        if entry.origin.is_some() {
            flags |= field_flag::ORIGIN;
        }
        if entry.angles.is_some() {
            flags |= field_flag::ANGLES;
        }
        if entry.door_state.is_some() {
            flags |= field_flag::DOOR_STATE;
        }
        if entry.simulation_time.is_some() {
            flags |= field_flag::SIMULATION_TIME;
        }
        if entry.model.is_some() {
            flags |= field_flag::MODEL;
        }
        let create_tick = entry.create_tick;
        let row = row_from(entity_id, serial, create_tick, operation::SPAWN, flags, entry);
        self.deltas.push(row);
    }
}

/// Merges a parallel second-pass slice. Rows are positional records, so this concatenates and
/// re-sorts; the caller drops duplicates that two overlapping slices both saw.
pub fn merge(into: &mut Vec<WorldEntityDelta>, from: Vec<WorldEntityDelta>) {
    into.extend(from);
    sort(into);
    into.dedup();
}

fn sort(deltas: &mut [WorldEntityDelta]) {
    deltas.sort_by_key(|row| (row.tick, row.entity_id, row.serial, row.operation));
}

fn row_from(
    entity_id: i32,
    serial: u32,
    tick: i32,
    operation: u8,
    flags: u16,
    entry: &EntityState,
) -> WorldEntityDelta {
    WorldEntityDelta {
        tick,
        entity_id,
        serial,
        class_id: entry.class_id,
        operation,
        flags,
        origin: entry.origin.unwrap_or([0.0; 3]),
        angles: entry.angles.unwrap_or([0.0; 3]),
        simulation_time: entry.simulation_time.unwrap_or(0.0),
        door_state: entry.door_state.unwrap_or(0),
        model: entry.model.unwrap_or(0),
    }
}

fn apply(entry: &mut EntityState, short: &str, value: &Variant) {
    match short {
        "m_cellX" | "m_cellY" | "m_cellZ" => {
            let axis = axis_of(short);
            entry.cell[axis] = as_u32(value);
            recompute_origin(entry);
        }
        "m_vecX" | "m_vecY" | "m_vecZ" => {
            let axis = axis_of(short);
            entry.offset[axis] = as_f32(value);
            recompute_origin(entry);
        }
        "m_angRotation" => {
            if let Variant::VecXYZ(angles) = value {
                entry.angles = Some(*angles);
            }
        }
        "m_eDoorState" => {
            entry.door_state = as_u32(value).map(|state| state.min(u8::MAX as u32) as u8);
        }
        "m_flSimulationTime" => {
            entry.simulation_time = as_f32(value);
        }
        "m_hModel" => {
            entry.model = match value {
                Variant::U64(model) => Some(*model),
                Variant::U32(model) => Some(u64::from(*model)),
                _ => None,
            };
        }
        // `m_closedPosition` is the same number as the decoded cell origin on every door the
        // audit checked, and only doors have it. The cell route covers every class, so it is the
        // one the lane uses.
        _ => {}
    }
}

fn recompute_origin(entry: &mut EntityState) {
    let mut origin = [0.0f32; 3];
    for axis in 0..3 {
        let (Some(cell), Some(offset)) = (entry.cell[axis], entry.offset[axis]) else {
            return;
        };
        origin[axis] = (cell as f32 * (1 << CELL_BITS) as f32) - MAX_COORD + offset;
    }
    entry.origin = Some(origin);
}

fn axis_of(short: &str) -> usize {
    match short.as_bytes().last() {
        Some(b'X') => 0,
        Some(b'Y') => 1,
        _ => 2,
    }
}

fn is_tracked_field(short: &str) -> bool {
    matches!(
        short,
        "m_cellX"
            | "m_cellY"
            | "m_cellZ"
            | "m_vecX"
            | "m_vecY"
            | "m_vecZ"
            | "m_angRotation"
            | "m_eDoorState"
            | "m_flSimulationTime"
            | "m_hModel"
    )
}

/// Maps a networked class to the coarse kind the viewer needs. Unlisted classes are not captured
/// at all: the audit found nothing else in a CS2 map that changes during a round.
pub fn classify(class_name: &str) -> u8 {
    match class_name {
        "CPropDoorRotating" | "CPropDoorRotatingBreakable" => class::DOOR_ROTATING,
        "CBreakable" | "CBreakableProp" | "CBreakableSurface" => class::BREAKABLE,
        "CDynamicProp" => class::DYNAMIC_PROP,
        "CPhysicsProp" | "CPhysicsPropMultiplayer" | "CPhysicsPropOverride" => class::PHYSICS_PROP,
        "CFuncBrush" => class::FUNC_BRUSH,
        "CFuncWater" => class::FUNC_WATER,
        "CBaseButton" | "CFuncButton" | "CRotButton" => class::BUTTON,
        _ => class::OTHER,
    }
}

fn field_name(field: &Field) -> Option<&str> {
    match field {
        Field::Value(value) => Some(&value.full_name),
        Field::Vector(_) => match field.get_inner(0).ok()? {
            Field::Value(value) => Some(&value.full_name),
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
        Variant::Bool(value) => Some(u32::from(*value)),
        _ => None,
    }
}

fn as_f32(value: &Variant) -> Option<f32> {
    match value {
        Variant::F32(value) => Some(*value),
        Variant::I32(value) => Some(*value as f32),
        Variant::U32(value) => Some(*value as f32),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::first_pass::sendtables::ValueField;
    use crate::second_pass::decoder::Decoder;

    fn value_field(full_name: &str) -> Field {
        Field::Value(ValueField {
            decoder: Decoder::NoscaleDecoder,
            name: full_name.rsplit('.').next().unwrap_or(full_name).to_owned(),
            should_parse: false,
            prop_id: 0,
            full_name: full_name.to_owned(),
        })
    }

    fn seed_origin(capture: &mut WorldEntityCapture, tick: i32, cells: [u32; 3], offsets: [f32; 3]) {
        for (axis, name) in ["m_cellX", "m_cellY", "m_cellZ"].iter().enumerate() {
            capture.observe(99, 7, "CPropDoorRotating", tick, &value_field(name), &Variant::U32(cells[axis]), false);
        }
        for (axis, name) in ["m_vecX", "m_vecY", "m_vecZ"].iter().enumerate() {
            capture.observe(99, 7, "CPropDoorRotating", tick, &value_field(name), &Variant::F32(offsets[axis]), false);
        }
    }

    #[test]
    fn disabled_capture_emits_nothing() {
        let mut capture = WorldEntityCapture::new(false);
        capture.begin(99, 7, "CPropDoorRotating", 0);
        seed_origin(&mut capture, 0, [34, 30, 30], [23.0, -16.0, 256.0]);
        assert!(capture.finish(10).is_empty());
    }

    #[test]
    fn spawn_origin_decodes_to_the_authored_origin() {
        // de_nuke door_01: cells and offsets taken from the audited demo, authored origin
        // (1047, -1040, -768) read from de_nuke.vpk.
        let mut capture = WorldEntityCapture::new(true);
        capture.begin(99, 7, "CPropDoorRotating", 0);
        seed_origin(&mut capture, 0, [34, 30, 30], [23.0, -16.0, 256.0]);
        let deltas = capture.finish(0);
        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas[0].operation, operation::SPAWN);
        assert_eq!(deltas[0].class_id, class::DOOR_ROTATING);
        assert_eq!(deltas[0].origin, [1047.0, -1040.0, -768.0]);
        assert!(deltas[0].flags & field_flag::ORIGIN != 0);
    }

    #[test]
    fn a_partial_origin_is_not_reported_as_one() {
        let mut capture = WorldEntityCapture::new(true);
        capture.begin(99, 7, "CPropDoorRotating", 0);
        capture.observe(99, 7, "CPropDoorRotating", 0, &value_field("m_cellX"), &Variant::U32(34), false);
        capture.observe(99, 7, "CPropDoorRotating", 0, &value_field("m_vecX"), &Variant::F32(23.0), false);
        let deltas = capture.finish(0);
        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas[0].flags & field_flag::ORIGIN, 0, "two of six axes is not an origin");
    }

    #[test]
    fn a_swing_after_the_spawn_tick_is_an_update_per_tick() {
        let mut capture = WorldEntityCapture::new(true);
        capture.begin(99, 7, "CPropDoorRotating", 0);
        seed_origin(&mut capture, 0, [34, 30, 30], [23.0, -16.0, 256.0]);
        capture.flush_tick(1);
        for (index, tick) in (40..43).enumerate() {
            let yaw = -3.125 * (index + 1) as f32;
            capture.observe(99, 7, "CPropDoorRotating", tick, &value_field("m_angRotation"), &Variant::VecXYZ([0.0, yaw, 0.0]), true);
            capture.observe(99, 7, "CPropDoorRotating", tick, &value_field("m_flSimulationTime"), &Variant::F32(630.0 + index as f32), true);
        }
        let deltas = capture.finish(50);
        assert_eq!(deltas[0].operation, operation::SPAWN);
        let updates: Vec<_> = deltas.iter().filter(|row| row.operation == operation::UPDATE).collect();
        assert_eq!(updates.len(), 3, "one folded row per tick, not one per field");
        assert_eq!(updates[0].tick, 40);
        assert_eq!(updates[2].angles, [0.0, -9.375, 0.0]);
        for row in &updates {
            assert!(row.flags & field_flag::ANGLES != 0);
            assert!(row.flags & field_flag::SIMULATION_TIME != 0);
            // Every row keeps the origin so a reader never has to walk back for the join key.
            assert_eq!(row.origin, [1047.0, -1040.0, -768.0]);
        }
    }

    #[test]
    fn a_reseed_of_unchanged_state_is_not_an_update() {
        let mut capture = WorldEntityCapture::new(true);
        capture.begin(99, 7, "CPropDoorRotating", 0);
        seed_origin(&mut capture, 0, [34, 30, 30], [23.0, -16.0, 256.0]);
        capture.flush_tick(1);
        // The parser replays full packets; the same pose arrives again as a non-delta.
        seed_origin(&mut capture, 1, [34, 30, 30], [23.0, -16.0, 256.0]);
        let deltas = capture.finish(10);
        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas[0].operation, operation::SPAWN);
    }

    #[test]
    fn a_break_is_a_delete_row_carrying_the_origin() {
        let mut capture = WorldEntityCapture::new(true);
        capture.begin(140, 3, "CDynamicProp", 0);
        for (axis, name) in ["m_cellX", "m_cellY", "m_cellZ"].iter().enumerate() {
            capture.observe(140, 3, "CDynamicProp", 0, &value_field(name), &Variant::U32([34, 30, 30][axis]), false);
        }
        for (axis, name) in ["m_vecX", "m_vecY", "m_vecZ"].iter().enumerate() {
            capture.observe(140, 3, "CDynamicProp", 0, &value_field(name), &Variant::F32([23.0, -16.0, 256.0][axis]), false);
        }
        capture.end(140, 3, "CDynamicProp", 5155, false);
        let deltas = capture.finish(6000);
        assert_eq!(deltas.len(), 2);
        assert_eq!(deltas[0].operation, operation::SPAWN);
        assert_eq!(deltas[0].origin, [1047.0, -1040.0, -768.0]);
        assert_eq!(deltas[1].operation, operation::DELETE);
        assert_eq!(deltas[1].tick, 5155);
        assert_eq!(deltas[1].flags & field_flag::DORMANT, 0, "a break is a removal, not a PVS leave");
        assert_eq!(deltas[1].origin, [1047.0, -1040.0, -768.0], "a delete still names what vanished");
        assert!(deltas[1].flags & field_flag::ORIGIN != 0);
    }

    #[test]
    fn a_pvs_leave_is_marked_dormant_and_keeps_the_entity() {
        let mut capture = WorldEntityCapture::new(true);
        capture.begin(99, 7, "CPropDoorRotating", 0);
        seed_origin(&mut capture, 0, [34, 30, 30], [23.0, -16.0, 256.0]);
        capture.end(99, 7, "CPropDoorRotating", 100, true);
        capture.flush_tick(101);
        capture.observe(99, 7, "CPropDoorRotating", 120, &value_field("m_angRotation"), &Variant::VecXYZ([0.0, -45.0, 0.0]), true);
        let deltas = capture.finish(200);
        let kinds: Vec<u8> = deltas.iter().map(|row| row.operation).collect();
        assert_eq!(kinds, vec![operation::SPAWN, operation::DELETE, operation::UPDATE]);
        assert!(deltas[1].flags & field_flag::DORMANT != 0);
    }

    #[test]
    fn an_untracked_class_is_not_captured() {
        let mut capture = WorldEntityCapture::new(true);
        capture.begin(1, 1, "CCSPlayerPawn", 0);
        capture.observe(1, 1, "CCSPlayerPawn", 0, &value_field("m_cellX"), &Variant::U32(34), true);
        assert!(capture.finish(10).is_empty());
    }

    #[test]
    fn merge_orders_slices_by_tick_and_drops_duplicates() {
        let row = |tick: i32, operation: u8| WorldEntityDelta {
            tick,
            entity_id: 99,
            serial: 7,
            class_id: class::DOOR_ROTATING,
            operation,
            flags: field_flag::ORIGIN,
            origin: [1047.0, -1040.0, -768.0],
            angles: [0.0; 3],
            simulation_time: 0.0,
            door_state: 0,
            model: 0,
        };
        let mut into = vec![row(40, operation::UPDATE), row(0, operation::SPAWN)];
        merge(&mut into, vec![row(40, operation::UPDATE), row(20, operation::UPDATE)]);
        assert_eq!(
            into.iter().map(|value| value.tick).collect::<Vec<_>>(),
            vec![0, 20, 40]
        );
    }
}
