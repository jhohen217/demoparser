//! Recorded inferno patch changes. No client-side spread or lifetime simulation.
use crate::first_pass::sendtables::Field;
use crate::second_pass::{path_ops::FieldPath, variants::Variant};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub struct InfernoPatchRecord {
    pub tick: i32,
    pub entity_id: i32,
    pub serial: u32,
    pub start_tick: i32,
    pub index: u8,
    // position known, burning known, burning, normal known, entity removed.
    // flags=0/index=0 is a nonphysical life-observed/unknown marker; XYZ is not known.
    pub flags: u8,
    pub inferno_type: u16,
    pub position: [f32; 3],
    pub normal: [f32; 3],
}
impl InfernoPatchRecord {
    pub fn encode(&self) -> [u8; 44] {
        let mut bytes = [0; 44];
        bytes[0..4].copy_from_slice(&self.tick.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.entity_id.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.serial.to_le_bytes());
        bytes[12..16].copy_from_slice(&self.start_tick.to_le_bytes());
        bytes[16] = self.index; bytes[17] = self.flags;
        bytes[18..20].copy_from_slice(&self.inferno_type.to_le_bytes());
        for (i, v) in self.position.iter().chain(self.normal.iter()).enumerate() {
            bytes[20+i*4..24+i*4].copy_from_slice(&v.to_le_bytes());
        }
        bytes
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
struct Patch { flags: u8, position: [f32; 3], normal: [f32; 3] }
#[derive(Debug)]
struct Active {
    serial: u32, start: i32, count: usize, kind: u16,
    patches: [Patch; 64], emitted: [Option<(Patch, u16)>; 64],
}
#[derive(Debug, Default)]
pub struct InfernoCapture {
    pub enabled: bool,
    active: BTreeMap<i32, Active>,
    rows: Vec<InfernoPatchRecord>,
}
impl InfernoCapture {
    pub fn new(enabled: bool) -> Self { Self { enabled, ..Self::default() } }
    pub fn begin(&mut self, id: i32, serial: u32, tick: i32) {
        if !self.enabled { return; }
        // Fullpacket snapshots re-create an already observed entity. Subsequent
        // decoded fields still resynchronize it; this is not a death/new life.
        if self.active.get(&id).is_some_and(|active| active.serial == serial) { return; }
        self.remove(id, tick);
        // Even wholly unsupported/count-zero infernos must not look like no
        // inferno was observed. This marker never supplies a position or flame.
        self.rows.push(InfernoPatchRecord { tick, entity_id: id, serial, start_tick: tick,
            index: 0, flags: 0, inferno_type: u16::MAX, position: [0.; 3], normal: [0.; 3] });
        self.active.insert(id, Active { serial, start: tick, count: 0, kind: u16::MAX,
            patches: std::array::from_fn(|_| Patch::default()), emitted: std::array::from_fn(|_| None) });
    }
    pub fn observe(&mut self, id: i32, field: &Field, path: &FieldPath, value: &Variant) {
        let Some(active) = self.active.get_mut(&id) else { return; };
        let Field::Value(field) = field else { return; };
        let name = field.full_name.rsplit('.').next().unwrap_or(&field.full_name);
        let integer = match value { Variant::I32(v) if *v >= 0 => Some(*v as u32), Variant::U32(v) => Some(*v), _ => None };
        match name {
            "m_fireCount" => { if let Some(n) = integer.filter(|n| *n <= 64) { active.count = n as usize; } }
            "m_nInfernoType" => { if let Some(n) = integer.and_then(|n| u16::try_from(n).ok()) { active.kind = n; } }
            "m_firePositions" | "m_bFireIsBurning" | "m_BurnNormal" => {
                let index = path.path[path.last];
                if !(0..64).contains(&index) { return; }
                let patch = &mut active.patches[index as usize];
                match (name, value) {
                    ("m_firePositions", Variant::VecXYZ(v)) if v.iter().all(|v| v.is_finite()) => { patch.position = *v; patch.flags |= 1; }
                    ("m_BurnNormal", Variant::VecXYZ(v)) if v.iter().all(|v| v.is_finite()) => { patch.normal = *v; patch.flags |= 8; }
                    ("m_bFireIsBurning", Variant::Bool(v)) => { patch.flags = (patch.flags & !4) | 2 | if *v { 4 } else { 0 }; }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    pub fn flush_tick(&mut self, tick: i32) {
        for (&id, active) in &mut self.active {
            for index in 0..active.count {
                let patch = &active.patches[index];
                let state = (patch.clone(), active.kind);
                if active.emitted[index].as_ref() == Some(&state) { continue; }
                self.rows.push(InfernoPatchRecord { tick, entity_id: id, serial: active.serial, start_tick: active.start,
                    index: index as u8, flags: patch.flags, inferno_type: active.kind, position: patch.position, normal: patch.normal });
                active.emitted[index] = Some(state);
            }
        }
    }
    pub fn remove(&mut self, id: i32, tick: i32) {
        if let Some(active) = self.active.remove(&id) {
            self.rows.push(InfernoPatchRecord { tick, entity_id: id, serial: active.serial, start_tick: active.start,
                index: 255, flags: 16, inferno_type: active.kind, position: [0.; 3], normal: [0.; 3] });
        }
    }
    pub fn finish(mut self, tick: i32) -> Vec<InfernoPatchRecord> {
        self.flush_tick(tick);
        // EOF is not evidence of entity removal.
        self.rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::first_pass::sendtables::ValueField;
    use crate::second_pass::decoder::Decoder;

    fn observe(c: &mut InfernoCapture, name: &str, index: i32, value: Variant) {
        let field = Field::Value(ValueField { decoder: Decoder::BooleanDecoder,
            name: name.into(), full_name: format!("CInferno.{name}"), should_parse: false, prop_id: 0 });
        let path = FieldPath { path: [7, index, 0, 0, 0, 0, 0], last: 1 };
        c.observe(12, &field, &path, &value);
    }
    #[test]
    fn changes_only_and_removal_does_not_invent_burn_duration() {
        let mut c = InfernoCapture::new(true); c.begin(12, 3, 10);
        let a = c.active.get_mut(&12).unwrap(); a.count = 1;
        a.patches[0] = Patch { flags: 7, position: [1.,2.,3.], normal: [0.;3] };
        c.flush_tick(10); c.flush_tick(11);
        c.active.get_mut(&12).unwrap().patches[0].flags = 3;
        c.flush_tick(12); c.remove(12, 13);
        let rows = c.finish(15); assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].flags, 0);
        assert_eq!((rows[1].tick, rows[2].flags, rows[3].flags), (10, 3, 16));
    }
    #[test]
    fn reused_entity_has_new_serial_and_eof_is_not_removal() {
        let mut c = InfernoCapture::new(true); c.begin(12, 3, 10); c.begin(12, 4, 20);
        let a = c.active.get_mut(&12).unwrap(); a.count = 1; a.patches[0].flags = 7;
        let rows = c.finish(21); assert_eq!(rows.len(), 4);
        assert_eq!((rows[1].serial, rows[1].flags, rows[3].serial, rows[3].flags), (3, 16, 4, 7));
    }

    #[test]
    fn decoded_fields_and_same_serial_rebaseline_preserve_life_and_patch_index() {
        let mut c = InfernoCapture::new(true); c.begin(12, 3, 10);
        observe(&mut c, "m_fireCount", 0, Variant::U32(3));
        observe(&mut c, "m_firePositions", 2, Variant::VecXYZ([100., 200., 30.]));
        observe(&mut c, "m_bFireIsBurning", 2, Variant::Bool(true));
        observe(&mut c, "m_BurnNormal", 2, Variant::VecXYZ([0., 0., 1.]));
        observe(&mut c, "m_nInfernoType", 0, Variant::U32(1));
        c.flush_tick(10); c.begin(12, 3, 20);
        observe(&mut c, "m_bFireIsBurning", 2, Variant::Bool(false));
        let rows = c.finish(21);
        assert!(rows.iter().all(|r| r.start_tick == 10 && r.flags & 16 == 0));
        let patch: Vec<_> = rows.iter().filter(|r| r.index == 2).collect();
        assert_eq!(patch.len(), 2);
        assert_eq!((patch[0].flags, patch[1].flags), (15, 11));
        assert_eq!(patch[0].position, [100., 200., 30.]);
        assert_eq!(patch[0].normal, [0., 0., 1.]);
        assert_eq!(patch[0].inferno_type, 1);
    }

    #[test]
    fn unsupported_count_zero_and_removed_before_flush_remain_observed_unknown() {
        let mut c = InfernoCapture::new(true); c.begin(12, 3, 10);
        observe(&mut c, "m_fireXDelta", 0, Variant::I32(42));
        c.remove(12, 11);
        let rows = c.finish(12);
        assert_eq!(rows.len(), 2);
        assert_eq!((rows[0].flags, rows[0].index, rows[1].flags), (0, 0, 16));
        assert_eq!(rows[0].position, [0.; 3]);
        assert!(InfernoCapture::new(true).finish(12).is_empty());
    }

    #[test]
    fn legacy_burning_without_position_is_not_silently_dropped_or_fabricated() {
        let mut c = InfernoCapture::new(true); c.begin(12, 3, 10);
        observe(&mut c, "m_fireCount", 0, Variant::I32(1));
        observe(&mut c, "m_fireXDelta", 0, Variant::I32(42));
        observe(&mut c, "m_bFireIsBurning", 0, Variant::Bool(true));
        c.flush_tick(11);
        observe(&mut c, "m_firePositions", 0, Variant::VecXYZ([1., 2., 3.]));
        let rows = c.finish(12);
        assert_eq!(rows.len(), 3);
        assert_eq!((rows[1].flags, rows[2].flags), (6, 7));
        assert_eq!(rows[1].position, [0.; 3]);
    }
}
