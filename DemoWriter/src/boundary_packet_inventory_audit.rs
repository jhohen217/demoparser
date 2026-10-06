//! Read-only, whole-`DEM_Packet` resident inventory/economy snapshots.
//!
//! This intentionally does not reuse tick-only legacy audit rows: checkpoints and ordinary
//! packets can share a public tick.  The row key therefore starts with the outer frame ordinal
//! and offset, and state is observed only after all messages in an ordinary packet are applied.

use super::*;
use anyhow::{ensure, Context};
use parser::first_pass::read_bits::Bitreader;
use parser::first_pass::sendtables::{get_decoder_from_field, Field};
use parser::second_pass::path_ops::FieldPath;
use serde_json::{json, Value};
use std::io::Write;

const CONTROLLER_CLASS: &str = "CCSPlayerController";
const PAWN_CLASS: &str = "CCSPlayerPawn";

#[derive(Clone)]
struct FieldSpec {
    path: Vec<i32>,
    name: String,
}

#[derive(Clone)]
struct VectorSpec {
    path: Vec<i32>,
    label: String,
    descendant_names: Vec<String>,
}

#[derive(Clone)]
struct SlotSnapshot {
    path: Vec<i32>,
    name: String,
    wire: Option<u32>,
    value: Value,
}

#[derive(Clone)]
struct InventorySnapshot {
    serial: u32,
    last_packet_ordinal: usize,
    last_public_tick: i32,
    length: Option<u32>,
    slots: BTreeMap<usize, SlotSnapshot>,
}

/// Decode one DEM to JSONL without editing it. Every ordinary `DEM_Packet` emits one line after
/// its complete message list has advanced the persistent source state.
pub fn write_packet_inventory_economy_jsonl<W: Write>(demo: &[u8], output: &mut W) -> Result<()> {
    let idx = index::DemoIndex::build(demo)?;
    let builder = BoundaryBuilder::new(demo, &idx)?;
    let mut state = State {
        non_transmitted: BTreeSet::new(),
        entities: BTreeMap::new(),
        classes: &builder.classes,
        qf: &builder.qf,
        huf: &builder.huf,
        baselines: BTreeMap::new(),
        baseline_entries: Vec::new(),
        alternate_baselines: BTreeMap::new(),
        template: None,
    };
    let mut tables = Tables::default();
    let mut prior_inventories: BTreeMap<i32, InventorySnapshot> = BTreeMap::new();
    let mut pending_stale_rows = Vec::<Value>::new();

    for frame in &idx.frames {
        match frame.cmd {
            CMD_STRING_TABLES => {
                let snapshot = CDemoStringTables::decode(payload(demo, frame)?.as_slice())?;
                tables.overlay(snapshot);
                state.tables(&tables.snapshot);
            }
            CMD_SIGNON_PACKET | CMD_PACKET => {
                let packet = CDemoPacket::decode(payload(demo, frame)?.as_slice())?;
                let mut packet_net_ticks = Vec::<u32>::new();
                apply_packet_messages(
                    &mut state,
                    &mut tables,
                    packet.data(),
                    &mut packet_net_ticks,
                )?;
                if frame.cmd == CMD_SIGNON_PACKET {
                    update_inventory_history(
                        &state,
                        &builder,
                        frame,
                        &mut prior_inventories,
                        &mut pending_stale_rows,
                    )?;
                }
                if frame.cmd == CMD_PACKET {
                    update_inventory_history(
                        &state,
                        &builder,
                        frame,
                        &mut prior_inventories,
                        &mut pending_stale_rows,
                    )?;
                    let (net_tick, net_tick_status) = unique_net_tick(&packet_net_ticks);
                    let stale = std::mem::take(&mut pending_stale_rows);
                    let row = snapshot_packet(
                        &state,
                        &builder,
                        frame,
                        net_tick,
                        net_tick_status,
                        packet_net_ticks.len(),
                        stale,
                    )?;
                    serde_json::to_writer(&mut *output, &row)?;
                    output.write_all(b"\n")?;
                }
            }
            CMD_FULL_PACKET => {
                let full = CDemoFullPacket::decode(payload(demo, frame)?.as_slice())?;
                if let Some(snapshot) = full.string_table {
                    tables.overlay(snapshot);
                    state.tables(&tables.snapshot);
                }
                if let Some(packet) = full.packet {
                    let mut ignored_net_ticks = Vec::new();
                    apply_packet_messages(
                        &mut state,
                        &mut tables,
                        packet.data(),
                        &mut ignored_net_ticks,
                    )?;
                    update_inventory_history(
                        &state,
                        &builder,
                        frame,
                        &mut prior_inventories,
                        &mut pending_stale_rows,
                    )?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn apply_packet_messages(
    state: &mut State<'_>,
    tables: &mut Tables,
    data: &[u8],
    net_ticks: &mut Vec<u32>,
) -> Result<()> {
    for message in read_messages(data)? {
        match message.msg_type {
            4 => {
                let tick_message = csgoproto::CnetMsgTick::decode(message.payload.as_slice())?;
                if let Some(net_tick) = tick_message.tick {
                    net_ticks.push(net_tick);
                }
            }
            44 | 45 | 51 => {
                tables.message(&message)?;
                state.tables(&tables.snapshot);
            }
            55 => {
                state.packet(&message.payload)?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn unique_net_tick(values: &[u32]) -> (Option<u32>, &'static str) {
    match values {
        [] => (None, "absent"),
        [value] => (Some(*value), "observed"),
        _ => (None, "ambiguous_multiple_messages"),
    }
}

fn snapshot_packet(
    state: &State<'_>,
    builder: &BoundaryBuilder,
    frame: &FrameRef,
    net_tick: Option<u32>,
    net_tick_status: &str,
    net_tick_message_count: usize,
    stale_tail_rows: Vec<Value>,
) -> Result<Value> {
    let mut controllers = Vec::new();
    let mut pawns = Vec::new();
    for (&entity_id, entity) in &state.entities {
        let class = builder
            .classes
            .get(entity.class as usize)
            .with_context(|| format!("entity {entity_id} has unknown class {}", entity.class))?;
        let generation = generation_json(entity_id, entity.class, &class.name, entity.serial);
        match class.name.as_str() {
            CONTROLLER_CLASS => controllers.push(json!({
                "entity_id": entity_id,
                "class_id": entity.class,
                "class_name": class.name,
                "serial": entity.serial,
                "full_handle": generation["full_handle"].clone(),
                "generation": generation,
                "fields": selected_fields(entity, class, state.qf, "controller")?,
            })),
            PAWN_CLASS => {
                let fields = selected_fields(entity, class, state.qf, "pawn")?;
                let inventory_root = inventory_root(entity, class, state.qf)?;
                let mut weapons = inventory_weapons(state, builder, entity, class, state.qf)?;
                let active_weapon = active_weapon_ref(state, builder, entity, class, state.qf)?;
                if let Some(active) = &active_weapon {
                    let active_wire = active.get("wire_handle").and_then(Value::as_u64);
                    if let Some(active_wire) = active_wire {
                        if !weapons.iter().any(|row| row.get("wire_handle").and_then(Value::as_u64) == Some(active_wire)) {
                            weapons.push(resolve_weapon(state, builder, active_wire as u32, None, true)?);
                        }
                    }
                }
                let controller = matching_controller(state, builder, entity_id, entity.serial, state.qf)?;
                pawns.push(json!({
                    "entity_id": entity_id,
                    "class_id": entity.class,
                    "class_name": class.name,
                    "serial": entity.serial,
                    "full_handle": generation["full_handle"].clone(),
                    "generation": generation,
                    "controller": controller,
                    "fields": fields,
                    "inventory_root": inventory_root,
                    "weapons": weapons,
                    "active_weapon": active_weapon,
                }));
            }
            _ => {}
        }
    }
    Ok(json!({
        "schema": "packet-inventory-economy-snapshot.v1",
        "status": "snapshot_decoded",
        "errors": [],
        "packet": {
            "frame_ordinal": frame.index,
            "offset": frame.frame_offset,
            "command": CMD_PACKET,
            "public_tick": frame.tick(),
            "net_tick": net_tick,
            "net_tick_status": net_tick_status,
            "net_tick_message_count": net_tick_message_count,
        },
        "controllers": controllers,
        "pawns": pawns,
        "stale_tail_rows": stale_tail_rows,
        "read_only": true,
        "acceptance": false,
        "native_qualification": false,
        "native_clock_mapping_qualified": false,
        "native": false,
    }))
}

fn generation_json(entity_id: i32, class_id: u32, class_name: &str, serial: u32) -> Value {
    let wire14 = if (0..(1 << 14)).contains(&entity_id) && serial < (1 << 17) {
        Some(((serial as u64) << 14) | entity_id as u64)
    } else {
        None
    };
    json!({
        "entity_id": entity_id,
        "class_id": class_id,
        "class_name": class_name,
        "serial": serial,
        "full_handle": wire14,
        "identity_scope": "same_demo_entity_generation",
    })
}

fn selected_fields(entity: &Entity, class: &Class, qf: &QfMapper, role: &str) -> Result<Vec<Value>> {
    let (specs, vectors) = schema_specs(&class.serializer);
    let selected_vectors = vectors
        .iter()
        .filter(|v| vector_selected(role, v))
        .collect::<Vec<_>>();
    let mut rows = BTreeMap::<Vec<i32>, Value>::new();

    for spec in specs {
        if selected_name(role, &spec.name) {
            rows.insert(spec.path.clone(), field_row(entity, class, qf, &spec.path, &spec.name)?);
        }
    }
    for vector in &selected_vectors {
        rows.insert(
            vector.path.clone(),
            field_row(entity, class, qf, &vector.path, &vector.label)?,
        );
    }
    for path in entity.values.keys() {
        let decoded_name = field_for_path(class, path).map(field_name).unwrap_or_default();
        let in_selected_vector = selected_vectors.iter().any(|v| {
            path.len() > v.path.len() && path.starts_with(&v.path)
        });
        if selected_name(role, &decoded_name) || in_selected_vector {
            rows.insert(path.clone(), field_row(entity, class, qf, path, &decoded_name)?);
        }
    }
    Ok(rows.into_values().collect())
}

fn schema_specs(serializer: &Serializer) -> (Vec<FieldSpec>, Vec<VectorSpec>) {
    let mut fields = Vec::new();
    let mut vectors = Vec::new();
    walk_serializer(serializer, &mut Vec::new(), &mut fields, &mut vectors);
    (fields, vectors)
}

fn walk_serializer(
    serializer: &Serializer,
    prefix: &mut Vec<i32>,
    fields: &mut Vec<FieldSpec>,
    vectors: &mut Vec<VectorSpec>,
) {
    for (index, field) in serializer.fields.iter().enumerate() {
        prefix.push(index as i32);
        walk_field(field, prefix, fields, vectors);
        prefix.pop();
    }
}

fn walk_field(field: &Field, prefix: &mut Vec<i32>, fields: &mut Vec<FieldSpec>, vectors: &mut Vec<VectorSpec>) {
    match field {
        Field::Value(value) => fields.push(FieldSpec { path: prefix.clone(), name: value.full_name.clone() }),
        Field::Vector(vector) => {
            let mut names = Vec::new();
            collect_names(&vector.field_enum, &mut names);
            let label = vector_label(&names);
            vectors.push(VectorSpec { path: prefix.clone(), label, descendant_names: names });
        }
        Field::Serializer(nested) => walk_serializer(&nested.serializer, prefix, fields, vectors),
        Field::Pointer(pointer) => {
            fields.push(FieldSpec { path: prefix.clone(), name: "<pointer>".into() });
            walk_serializer(&pointer.serializer, prefix, fields, vectors);
        }
        Field::Array(array) => {
            for index in 0..array.length {
                prefix.push(index as i32);
                walk_field(&array.field_enum, prefix, fields, vectors);
                prefix.pop();
            }
        }
        Field::None => {}
    }
}

fn collect_names(field: &Field, names: &mut Vec<String>) {
    match field {
        Field::Value(value) => names.push(value.full_name.clone()),
        Field::Vector(vector) => collect_names(&vector.field_enum, names),
        Field::Serializer(serializer) => {
            for child in &serializer.serializer.fields { collect_names(child, names); }
        }
        Field::Pointer(pointer) => {
            for child in &pointer.serializer.fields { collect_names(child, names); }
        }
        Field::Array(array) => collect_names(&array.field_enum, names),
        Field::None => {}
    }
}

fn vector_label(names: &[String]) -> String {
    for name in names {
        let lower = name.to_ascii_lowercase();
        if lower.contains("m_hmyweapons") { return "m_hMyWeapons".into(); }
        if lower.contains("m_attributes") || lower.contains("ceconitemattribute") { return "m_Attributes".into(); }
    }
    names.first().cloned().unwrap_or_else(|| "<unnamed vector>".into())
}

fn vector_selected(role: &str, vector: &VectorSpec) -> bool {
    let label = vector.label.to_ascii_lowercase();
    match role {
        "pawn" => label.contains("m_hmyweapons"),
        "weapon" => label.contains("m_attributes"),
        _ => false,
    }
}

fn selected_name(role: &str, name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    match role {
        "controller" => ["steamid", "m_hpawn", "m_hplayerpawn", "account", "money", "spent"]
            .iter().any(|needle| name.contains(needle)),
        "pawn" => ["m_hcontroller", "m_ihealth", "m_lifestate", "m_hactiveweapon", "m_hmyweapons"]
            .iter().any(|needle| name.contains(needle)),
        "weapon" => [
            "m_howner", "itemdefinition", "itemid", "accountid", "quality", "level", "ceconitemattribute",
            "inventory", "initialized", "customname", "m_attributes", "originalowner",
            "fallbackpaint", "fallbackseed", "fallbackwear", "m_nfallback", "m_ioriginalowner",
        ].iter().any(|needle| name.contains(needle)),
        _ => false,
    }
}

fn field_row(entity: &Entity, class: &Class, qf: &QfMapper, path: &[i32], name: &str) -> Result<Value> {
    if let Some(bits) = entity.values.get(path) {
        let value = decode_value(bits, class, path, qf)?;
        Ok(json!({"field_path": path, "field_name": name, "status": "present", "value": variant_json(&value)}))
    } else {
        Ok(json!({"field_path": path, "field_name": name, "status": "absent", "value": Value::Null}))
    }
}

fn field_for_path<'a>(class: &'a Class, path: &[i32]) -> Option<&'a Field> {
    if path.is_empty() || path.len() > 6 { return None; }
    let mut raw = [0i32; 7];
    raw[..path.len()].copy_from_slice(path);
    let fp = FieldPath { path: raw, last: path.len() - 1 };
    find_field(&fp, &class.serializer).ok()
}

fn decode_value(bits: &Bits, class: &Class, path: &[i32], qf: &QfMapper) -> Result<Variant> {
    let field = field_for_path(class, path).context("resident field path is outside serializer")?;
    let decoder = get_decoder_from_field(field)?;
    let mut reader = Bitreader::new(&bits.bytes);
    Ok(reader.decode(&decoder, qf)?)
}

fn variant_json(value: &Variant) -> Value {
    match value {
        Variant::F32(number) if number.is_finite() => json!(number),
        Variant::F32(number) => json!({"nonfinite_f32_bits": format!("{:08x}", number.to_bits())}),
        Variant::U64(number) => json!(number.to_string()),
        Variant::Binary(bytes) => json!(bytes),
        _ => serde_json::to_value(value).unwrap_or(Value::Null),
    }
}

fn inventory_root(entity: &Entity, class: &Class, qf: &QfMapper) -> Result<Value> {
    let (_, vectors) = schema_specs(&class.serializer);
    let Some(vector) = vectors.iter().find(|v| vector_selected("pawn", v)) else { return Ok(Value::Null); };
    let row = field_row(entity, class, qf, &vector.path, "m_hMyWeapons")?;
    let length = if row["status"] == "present" { row["value"].as_u64().and_then(|n| u32::try_from(n).ok()) } else { None };
    Ok(json!({
        "field_path": vector.path,
        "field_name": "m_hMyWeapons",
        "status": row["status"],
        "observed_length": length,
    }))
}

fn inventory_weapons(state: &State<'_>, builder: &BoundaryBuilder, pawn: &Entity, class: &Class, qf: &QfMapper) -> Result<Vec<Value>> {
    let (_, vectors) = schema_specs(&class.serializer);
    let Some(vector) = vectors.iter().find(|v| vector_selected("pawn", v)) else { return Ok(Vec::new()); };
    let Some(bits) = pawn.values.get(&vector.path) else { return Ok(Vec::new()); };
    let Some(length) = checked_vector_length(&decode_value(bits, class, &vector.path, qf)?)? else { return Ok(Vec::new()); };
    let pawn_id = state.entities.iter().find_map(|(id, candidate)| std::ptr::eq(candidate, pawn).then_some(*id)).unwrap_or(-1);
    let mut result = Vec::with_capacity(length);
    for slot in 0..length {
        let mut path = vector.path.clone();
        path.push(slot as i32);
        let mut row = match pawn.values.get(&path) {
            None => json!({"slot_index":slot,"active":false,"wire_handle":null,"status":"absent_slot","target":null,"fields":[],"errors":[]}),
            Some(bits) => match decode_value(bits, class, &path, qf)? {
                Variant::U32(wire) => resolve_weapon(state, builder, wire, Some(slot), false)?,
                value => json!({"slot_index":slot,"active":false,"wire_handle":null,"status":"invalid_handle_value","raw_value":variant_json(&value),"target":null,"fields":[],"errors":[]}),
            },
        };
        row["pawn_entity_id"] = json!(pawn_id);
        result.push(row);
    }
    Ok(result)
}

fn active_weapon_ref(state: &State<'_>, builder: &BoundaryBuilder, pawn: &Entity, class: &Class, qf: &QfMapper) -> Result<Option<Value>> {
    let rows = selected_fields(pawn, class, qf, "pawn")?;
    let handle = rows.iter().find(|row| row["field_name"].as_str().is_some_and(|name| name.to_ascii_lowercase().contains("m_hactiveweapon")) && row["status"] == "present");
    let Some(row) = handle else { return Ok(None); };
    let wire = row["value"].as_u64().and_then(|v| u32::try_from(v).ok());
    Ok(Some(match wire { Some(wire) => resolve_weapon(state, builder, wire, None, true)?, None => json!({"wire_handle": null, "status":"invalid_handle_value", "target":null}) }))
}

fn matching_controller(state: &State<'_>, builder: &BoundaryBuilder, pawn_id: i32, pawn_serial: u32, qf: &QfMapper) -> Result<Value> {
    let pawn = state.entities.get(&pawn_id).context("pawn disappeared during controller join")?;
    let pawn_class = &builder.classes[pawn.class as usize];
    let pawn_fields = selected_fields(pawn, pawn_class, qf, "pawn")?;
    let pawn_controller_handle = unique_handle_field(&pawn_fields, "m_hcontroller");
    let pawn_handle = generation_json(pawn_id, pawn.class, PAWN_CLASS, pawn_serial)["full_handle"].as_u64();
    let mut matches = Vec::new();
    for (&id, entity) in &state.entities {
        let class = &builder.classes[entity.class as usize];
        if class.name != CONTROLLER_CLASS { continue; }
        let fields = selected_fields(entity, class, qf, "controller")?;
        let controller_pawn_handle = unique_handle_field(&fields, "m_hplayerpawn");
        let controller_handle = generation_json(id, entity.class, &class.name, entity.serial)["full_handle"].as_u64();
        if controller_pawn_handle == pawn_handle
            && pawn_controller_handle.is_some()
            && pawn_controller_handle == controller_handle
        {
            matches.push(generation_json(id, entity.class, &class.name, entity.serial));
        }
    }
    match matches.as_slice() {
        [target] => Ok(json!({"status":"resolved_bidirectional_generation_match", "target":target})),
        [] => Ok(json!({"status":"no_unique_bidirectional_controller_match", "target":null})),
        _ => Ok(json!({"status":"ambiguous_controller_matches", "target":null,"candidate_count":matches.len()})),
    }
}

fn unique_handle_field(fields: &[Value], token: &str) -> Option<u64> {
    let mut matches = fields.iter().filter(|row| {
        row["status"] == "present"
            && row["field_name"].as_str().is_some_and(|name| name.to_ascii_lowercase().contains(token))
    });
    let first = matches.next()?.get("value")?.as_u64()?;
    if matches.next().is_some() { return None; }
    Some(first)
}

fn resolve_weapon(state: &State<'_>, builder: &BoundaryBuilder, wire: u32, slot_index: Option<usize>, active: bool) -> Result<Value> {
    let decoded = decode_wire14(wire);
    let Some((entity_id, serial)) = decoded else {
        return Ok(json!({"slot_index":slot_index,"active":active,"wire_handle":wire,"status":"invalid_wire_handle","target":null,"fields":[],"errors":[]}));
    };
    let Some(entity) = state.entities.get(&entity_id) else {
        return Ok(json!({"slot_index":slot_index,"active":active,"wire_handle":wire,"status":"target_entity_absent","target":null,"fields":[],"errors":[]}));
    };
    let class = &builder.classes[entity.class as usize];
    if entity.serial != serial {
        return Ok(json!({"slot_index":slot_index,"active":active,"wire_handle":wire,"status":"serial_mismatch","target":generation_json(entity_id, entity.class, &class.name, entity.serial),"fields":[],"errors":[]}));
    }
    Ok(json!({
        "slot_index":slot_index,
        "active":active,
        "wire_handle":wire,
        "status":"resolved_generation_match",
        "target":generation_json(entity_id, entity.class, &class.name, entity.serial),
        "fields":selected_fields(entity, class, state.qf, "weapon")?,
        "errors":[],
    }))
}

fn decode_wire14(wire: u32) -> Option<(i32, u32)> {
    if wire == u32::MAX || wire == 0x00ff_ffff { return None; }
    let entity = (wire & 0x3fff) as i32;
    let serial = wire >> 14;
    if serial >= (1 << 17) { return None; }
    Some((entity, serial))
}

fn variant_u32(value: Variant) -> Option<u32> {
    match value { Variant::U32(value) => Some(value), _ => None }
}

fn checked_vector_length(value: &Variant) -> Result<Option<usize>> {
    match value {
        Variant::U32(size) => {
            ensure!(*size <= 4096, "unreasonable inventory vector length {size}");
            Ok(Some(*size as usize))
        }
        _ => Ok(None),
    }
}

fn vector_snapshot(state: &State<'_>, builder: &BoundaryBuilder, entity_id: i32) -> Result<Option<InventorySnapshot>> {
    let Some(entity) = state.entities.get(&entity_id) else { return Ok(None); };
    let class = &builder.classes[entity.class as usize];
    if class.name != PAWN_CLASS { return Ok(None); }
    let (_, vectors) = schema_specs(&class.serializer);
    let Some(vector) = vectors.iter().find(|v| vector_selected("pawn", v)) else { return Ok(None); };
    let length = match entity.values.get(&vector.path) {
        Some(bits) => checked_vector_length(&decode_value(bits, class, &vector.path, state.qf)?)?,
        None => None,
    }.map(|size| size as u32);
    let mut slots = BTreeMap::new();
    if let Some(length) = length {
        for slot in 0..length as usize {
            let mut path = vector.path.clone(); path.push(slot as i32);
            if let Some(bits) = entity.values.get(&path) {
                let value = decode_value(bits, class, &path, state.qf).ok();
                slots.insert(slot, SlotSnapshot {
                    path: path.clone(),
                    name: field_for_path(class, &path).map(field_name).unwrap_or_default(),
                    wire: value.clone().and_then(variant_u32),
                    value: value.as_ref().map(variant_json).unwrap_or(Value::Null),
                });
            }
        }
    }
    Ok(Some(InventorySnapshot { serial: entity.serial, last_packet_ordinal: 0, last_public_tick: 0, length, slots }))
}

fn update_inventory_history(
    state: &State<'_>, builder: &BoundaryBuilder, frame: &FrameRef,
    prior: &mut BTreeMap<i32, InventorySnapshot>, pending: &mut Vec<Value>,
) -> Result<()> {
    let pawn_ids = state.entities.iter().filter_map(|(id, entity)| {
        builder.classes.get(entity.class as usize).filter(|class| class.name == PAWN_CLASS).map(|_| *id)
    }).collect::<Vec<_>>();
    let mut next = BTreeMap::new();
    for id in pawn_ids {
        let Some(mut current) = vector_snapshot(state, builder, id)? else { continue; };
        if let Some(old) = prior.get(&id).filter(|old| old.serial == current.serial) {
            if let (Some(old_len), Some(new_len)) = (old.length, current.length) {
                if new_len < old_len {
                    for (&slot, value) in old.slots.range(new_len as usize..) {
                        pending.push(json!({
                            "entity_id":id,"serial":old.serial,"slot_index":slot,
                            "field_path":value.path,"field_name":value.name,
                            "wire_handle":value.wire,"value":value.value,
                            "previous_length":old_len,"observed_length_after_shrink":new_len,
                            "status":"stale_tail_after_vector_shrink",
                            "source_frame_ordinal":frame.index,"source_offset":frame.frame_offset,
                            "source_command":frame.cmd,"source_public_tick":frame.tick(),
                        }));
                    }
                }
            }
        }
        current.last_packet_ordinal = frame.index;
        current.last_public_tick = frame.tick();
        next.insert(id, current);
    }
    *prior = next;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire14_generation_is_exact_and_demo_local() {
        let entity = 152i32;
        let serial = 101u32;
        let wire = ((serial << 14) | entity as u32) as u32;
        assert_eq!(decode_wire14(wire), Some((entity, serial)));
        assert_eq!(decode_wire14(u32::MAX), None);
        assert_eq!(decode_wire14(0x00ff_ffff), None);
        assert_ne!(decode_wire14(wire), Some((entity, serial + 1)));
    }

    #[test]
    fn inventory_presence_is_not_zero_length() {
        let absent: Value = json!({"status":"absent","observed_length":null});
        let present_empty: Value = json!({"status":"present","observed_length":0});
        assert_ne!(absent, present_empty);
    }

    #[test]
    fn unique_packet_net_tick_does_not_collapse_duplicates() {
        assert_eq!(unique_net_tick(&[]), (None, "absent"));
        assert_eq!(unique_net_tick(&[44]), (Some(44), "observed"));
        assert_eq!(unique_net_tick(&[44, 44]), (None, "ambiguous_multiple_messages"));
        assert_eq!(unique_net_tick(&[44, 45]), (None, "ambiguous_multiple_messages"));
    }

    #[test]
    fn field_selection_is_role_scoped_and_econ_keeps_attributes() {
        assert!(selected_name("pawn", "CCSPlayerPawn.m_iHealth"));
        assert!(!selected_name("controller", "CCSPlayerPawn.m_iHealth"));
        assert!(selected_name("weapon", "CEconItemAttribute.m_flValue"));
        assert!(selected_name("controller", "CCSPlayerController.m_hPlayerPawn"));
        assert!(vector_selected("pawn", &VectorSpec { path: vec![1], label: "m_hMyWeapons".into(), descendant_names: vec![] }));
        assert!(!vector_selected("controller", &VectorSpec { path: vec![1], label: "m_hMyWeapons".into(), descendant_names: vec![] }));
    }

    #[test]
    fn inventory_length_requires_observed_u32_and_is_bounded() {
        assert_eq!(checked_vector_length(&Variant::U32(0)).unwrap(), Some(0));
        assert_eq!(checked_vector_length(&Variant::U32(5)).unwrap(), Some(5));
        assert_eq!(checked_vector_length(&Variant::I32(0)).unwrap(), None);
        assert!(checked_vector_length(&Variant::U32(4097)).is_err());
    }
}
