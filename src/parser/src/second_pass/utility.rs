//! Utility authority independent of player resolution and dataframe filters.
//! Float values retain IEEE bits, including unknown/nonfinite payloads. Field
//! paths retain array indices. Embedded network ticks are deliberately raw;
//! the S2R envelope supplies the server clock offset separately.
use super::{path_ops::FieldPath, variants::Variant};
use crate::first_pass::sendtables::Field;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value")]
pub enum UtilityValue {
    Bool(bool), U32(u32), I32(i32), U64(u64), F32Bits(u32), String(String),
    VecXYBits([u32; 2]), VecXYZBits([u32; 3]),
    U32Vec(Vec<u32>), U64Vec(Vec<u64>), StringVec(Vec<String>),
    // These variants are not emitted by entity field decoders, but preserving
    // their typed JSON avoids silently discarding a future/event value.
    Other(serde_json::Value),
}
impl From<&Variant> for UtilityValue {
    fn from(value: &Variant) -> Self {
        match value {
            Variant::Bool(v) => Self::Bool(*v), Variant::U32(v) => Self::U32(*v),
            Variant::I32(v) => Self::I32(*v), Variant::U64(v) => Self::U64(*v),
            Variant::F32(v) => Self::F32Bits(v.to_bits()),
            Variant::String(v) => Self::String(v.clone()),
            Variant::VecXY(v) => Self::VecXYBits(v.map(f32::to_bits)),
            Variant::VecXYZ(v) => Self::VecXYZBits(v.map(f32::to_bits)),
            Variant::U32Vec(v) => Self::U32Vec(v.clone()),
            Variant::U64Vec(v) => Self::U64Vec(v.clone()),
            Variant::StringVec(v) => Self::StringVec(v.clone()),
            v => Self::Other(serde_json::to_value(v).expect("Variant is serializable")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UtilityField {
    pub name: String,
    pub path: Vec<i32>,
    pub value: Option<UtilityValue>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UtilityRecord {
    pub tick: i32,
    pub frame_offset: u64,
    pub message_index: u32,
    /// Order within the parser lane, including multiple updates in one message.
    pub sequence: u64,
    pub action: String,
    pub entity_id: Option<i32>,
    pub serial: Option<u32>,
    /// Network class for entity records, event name for events.
    pub name: String,
    pub fields: Vec<UtilityField>,
    pub position: Option<[f32; 3]>,
    pub thrower_steamid: Option<u64>,
}
#[derive(Debug, Clone, Default)]
pub struct UtilityData {
    pub captured: bool,
    pub server_tick_offset: Option<i32>,
    pub records: Vec<UtilityRecord>,
}
#[derive(Debug)]
struct Active {
    serial: u32, name: String, action: String, pending: Vec<UtilityField>,
}
#[derive(Debug, Default)]
pub struct UtilityCapture {
    pub enabled: bool,
    active: BTreeMap<i32, Active>,
    records: Vec<UtilityRecord>,
}
pub fn utility_class(name: &str) -> bool {
    matches!(name, "CFlashbangProjectile" | "CHEGrenadeProjectile" | "CSmokeGrenadeProjectile"
        | "CMolotovProjectile" | "CDecoyProjectile" | "CInferno")
}
pub fn utility_event(name: &str) -> bool {
    name.starts_with("decoy_") || name.starts_with("inferno_")
        || name.starts_with("smokegrenade_") || name.starts_with("flashbang_")
        || name.starts_with("hegrenade_") || name.starts_with("molotov_")
        || name.starts_with("incgrenade_") || name.starts_with("grenade_")
        || matches!(name, "player_blind" | "player_hurt")
}
impl UtilityCapture {
    pub fn new(enabled: bool) -> Self { Self { enabled, ..Self::default() } }
    pub fn contains(&self, id: i32) -> bool { self.active.contains_key(&id) }
    pub fn is_replaced(&self, id: i32, serial: u32, name: &str) -> bool {
        self.active.get(&id).is_some_and(|a| a.serial != serial || a.name != name)
    }
    pub fn begin(&mut self, id: i32, serial: u32, name: &str, snapshot: bool) {
        if !self.enabled { return; }
        if !utility_class(name) { self.active.remove(&id); return; }
        // A repeated full packet is a snapshot, not a second grenade birth.
        self.active.insert(id, Active { serial, name: name.into(),
            action: if snapshot { "snapshot" } else { "create" }.into(), pending: Vec::new() });
    }
    pub fn observe(&mut self, id: i32, field: &Field, path: &FieldPath, value: &Variant) {
        let Some(active) = self.active.get_mut(&id) else { return; };
        let name = match field {
            Field::Value(field) => field.full_name.as_str(),
            Field::Vector(_) => "$vector_count", Field::Pointer(_) => "$pointer",
            Field::Array(_) => "$array", Field::Serializer(_) => "$serializer", Field::None => "$unknown",
        };
        // The dedicated smoke block already retains the large bit-exact voxel
        // payload. All scalar smoke state is retained here as well.
        if name.ends_with(".m_VoxelFrameData") { return; }
        active.pending.push(UtilityField { name: name.into(),
            path: path.path[..=path.last as usize].to_vec(), value: Some(value.into()) });
    }
    pub fn emit(&mut self, id: i32, tick: i32, frame_offset: u64, message_index: u32,
        action: Option<&str>, position: Option<[f32; 3]>, thrower_steamid: Option<u64>) {
        let Some(active) = self.active.get_mut(&id) else { return; };
        self.records.push(UtilityRecord { tick, frame_offset, message_index,
            sequence: self.records.len() as u64,
            action: action.unwrap_or(&active.action).into(), entity_id: Some(id),
            serial: Some(active.serial), name: active.name.clone(),
            fields: std::mem::take(&mut active.pending), position,
            thrower_steamid: thrower_steamid.filter(|id| *id != 0) });
        active.action = "update".into();
        if matches!(action, Some("delete" | "leave" | "replaced")) { self.active.remove(&id); }
    }
    pub fn event(&mut self, tick: i32, frame_offset: u64, message_index: u32,
        name: &str, fields: Vec<UtilityField>) {
        if !self.enabled || !utility_event(name) { return; }
        self.records.push(UtilityRecord { tick, frame_offset, message_index,
            sequence: self.records.len() as u64, action: "event".into(),
            entity_id: None, serial: None, name: name.into(), fields,
            position: None, thrower_steamid: None });
    }
    pub fn finish(self) -> UtilityData {
        // EOF is not a delete. No invented lifetime/detonation/expiry.
        UtilityData { captured: self.enabled, server_tick_offset: None, records: self.records }
    }
}

// Infernos use BaseModelEntity, not the BaseAnimGraph component whose IDs
// collect_cell_coordinate_grenade caches. Missing components stay unknown.
fn inferno_position_from_fields(mut field: impl FnMut(&str) -> Option<Variant>) -> Option<[f32; 3]> {
    use super::collect_data::{coord_from_cell, PropCollectionError};
    let mut position = [0.0; 3];
    for (index, (cell_name, offset_name)) in [
        ("CBodyComponentBaseModelEntity.m_cellX", "CBodyComponentBaseModelEntity.m_vecX"),
        ("CBodyComponentBaseModelEntity.m_cellY", "CBodyComponentBaseModelEntity.m_vecY"),
        ("CBodyComponentBaseModelEntity.m_cellZ", "CBodyComponentBaseModelEntity.m_vecZ"),
    ].into_iter().enumerate() {
        let cell = field(cell_name)
            .ok_or(PropCollectionError::CoordinateCellNone);
        let offset = field(offset_name)
            .ok_or(PropCollectionError::CoordinateOffsetNone);
        position[index] = coord_from_cell(cell, offset).ok()?;
        if !position[index].is_finite() { return None; }
    }
    Some(position)
}

impl<'a> super::parser_settings::SecondPassParser<'a> {
    pub fn capture_utility_entity(&mut self, id: i32, action: Option<&str>) {
        if !self.utility.contains(id) { return; }
        use super::collect_data::CoordinateAxis;
        let coordinate = |axis| match self.collect_cell_coordinate_grenade(axis, &id).ok() {
            Some(Variant::F32(v)) if v.is_finite() => Some(v), _ => None,
        };
        let position = if self.utility.active.get(&id).is_some_and(|active| active.name == "CInferno") {
            inferno_position_from_fields(|name| {
                let prop = *self.prop_controller.name_to_id.get(name)?;
                self.get_prop_from_ent(&prop, &id).ok()
            })
        } else {
            match (coordinate(CoordinateAxis::X), coordinate(CoordinateAxis::Y), coordinate(CoordinateAxis::Z)) {
                (Some(x), Some(y), Some(z)) => Some([x, y, z]), _ => None,
            }
        };
        let thrower = self.find_thrower_steamid(&id).ok();
        self.utility.emit(id, self.tick, self.current_demo_frame_offset,
            self.current_network_message_index, action, position, thrower);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn origin_fields() -> BTreeMap<String, Variant> {
        let mut fields = BTreeMap::new();
        for (axis, cell, offset) in [("X", 30, 867.65625), ("Y", 34, 439.21875), ("Z", 32, 1.78125)] {
            fields.insert(format!("CBodyComponentBaseModelEntity.m_cell{axis}"), Variant::U32(cell));
            fields.insert(format!("CBodyComponentBaseModelEntity.m_vec{axis}"), Variant::F32(offset));
        }
        fields
    }
    #[test]
    fn inferno_origin_uses_base_model_entity_coordinates() {
        let fields = origin_fields();
        assert_eq!(inferno_position_from_fields(|name| fields.get(name).cloned()),
            Some([-156.34375, 1463.21875, 1.78125]));
        assert_eq!(inferno_position_from_fields(|name| Some(if name.contains("m_cell") {
            Variant::U32(32)
        } else { Variant::F32(0.0) })), Some([0.0; 3]));
    }
    #[test]
    fn inferno_origin_never_defaults_missing_or_invalid_components() {
        let fields = origin_fields();
        for name in fields.keys() {
            let mut missing = fields.clone();
            missing.remove(name);
            assert_eq!(inferno_position_from_fields(|key| missing.get(key).cloned()), None);
            let mut wrong_type = fields.clone();
            wrong_type.insert(name.clone(), Variant::Bool(false));
            assert_eq!(inferno_position_from_fields(|key| wrong_type.get(key).cloned()), None);
        }
        for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut invalid = fields.clone();
            invalid.insert("CBodyComponentBaseModelEntity.m_vecZ".into(), Variant::F32(value));
            assert_eq!(inferno_position_from_fields(|key| invalid.get(key).cloned()), None);
        }
        let wrong_component: BTreeMap<_, _> = fields.into_iter().map(|(key, value)|
            (key.replace("BaseModelEntity", "BaseAnimGraph"), value)).collect();
        assert_eq!(inferno_position_from_fields(|key| wrong_component.get(key).cloned()), None);
    }
    #[test]
    fn missing_owner_and_reused_id_are_retained_and_eof_is_open() {
        let mut capture = UtilityCapture::new(true);
        capture.begin(17, 2, "CDecoyProjectile", true);
        capture.emit(17, 0, 100, 1, None, None, None);
        capture.emit(17, 30, 200, 2, Some("delete"), Some([1., 2., 3.]), None);
        capture.begin(17, 3, "CFlashbangProjectile", false);
        capture.emit(17, 31, 250, 2, None, Some([0., 0., 0.]), Some(7656));
        let data = capture.finish();
        assert!(data.captured);
        assert_eq!(data.records.len(), 3);
        assert_eq!(data.records[0].serial, Some(2));
        assert_eq!(data.records[0].thrower_steamid, None);
        assert_eq!(data.records[2].serial, Some(3));
        assert_eq!(data.records[2].action, "create");
    }
    #[test]
    fn non_utility_replacement_cannot_inherit_grenade_fields() {
        let mut capture = UtilityCapture::new(true);
        capture.begin(17, 2, "CInferno", true);
        assert!(!capture.is_replaced(17, 2, "CInferno"));
        assert!(capture.is_replaced(17, 3, "CWeaponGlock"));
        capture.emit(17, 30, 200, 2, Some("replaced"), None, None);
        capture.begin(17, 3, "CWeaponGlock", false);
        capture.emit(17, 31, 210, 3, None, None, None);
        assert_eq!(capture.finish().records.len(), 1);
    }
    #[test]
    fn event_families_and_float_bits_survive() {
        for name in ["decoy_firing", "decoy_detonate", "inferno_expire", "inferno_extinguish",
            "smokegrenade_expired", "grenade_bounce", "player_blind"] { assert!(utility_event(name)); }
        assert!(!utility_event("player_footstep"));
        let value = UtilityValue::from(&Variant::F32(f32::from_bits(0x7fc01234)));
        let bytes = serde_json::to_vec(&value).unwrap();
        assert_eq!(serde_json::from_slice::<UtilityValue>(&bytes).unwrap(), value);
    }
}
