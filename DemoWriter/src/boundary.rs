//! Build an exact logical boundary from the source checkpoint's raw entity fields.
//! Initialization is retained at start-1; all ordinary gameplay starts at the requested tick.
use crate::frame::*;
use crate::{bitwriter, index, plan};
use anyhow::{ensure, Context, Result};
use bitwriter::{read_messages, write_messages, BitWriter, NetMessage};
use csgoproto::{CDemoFullPacket, CDemoPacket, CDemoStringTables, CsvcMsgPacketEntities};
use parser::first_pass::{
    parser_settings::{FirstPassParser, ParserInputs},
    read_bits::Bitreader,
    sendtables::{find_field, get_decoder_from_field, Field, Serializer},
};
use parser::second_pass::usercmd_delta;
use parser::second_pass::{
    decoder::Decoder,
    decoder::QfMapper,
    other_netmessages::Class,
    parser_settings::create_huffman_lookup_table,
    path_ops::{do_op, generate_fp, FieldPath},
    variants::Variant,
};
use prost::Message;
use std::collections::{BTreeMap, BTreeSet};

#[path = "boundary_packet_inventory_audit.rs"]
mod packet_inventory_audit;
pub use packet_inventory_audit::write_packet_inventory_economy_jsonl;
#[path = "boundary_ag2_audit.rs"]
mod ag2_audit;
pub use ag2_audit::audit_cached_ag2_bounds;

#[derive(Clone, PartialEq, Eq)]
pub struct Bits {
    bytes: Vec<u8>,
    len: usize,
}
impl Bits {
    pub fn wire_hex(&self) -> String {
        let mut out = format!("{}:", self.len);
        for byte in &self.bytes {
            out.push_str(&format!("{byte:02x}"));
        }
        out
    }
}
fn slice_bits(data: &[u8], first: usize, last: usize) -> Bits {
    let mut writer = BitWriter::new();
    for pos in first..last {
        writer.write_nbits(((data[pos / 8] >> (pos % 8)) & 1) as u32, 1);
    }
    Bits {
        bytes: writer.finish(),
        len: last - first,
    }
}
/// Bits per angle component, and the span they cover.
///
/// `read_bit_coord_pres` in the parser reads twenty bits and maps them linearly onto a full turn:
/// `n * 360 / 2^20 - 180`. So the inverse is exact arithmetic rather than a search, and a written
/// angle lands within a third of a thousandth of a degree of whatever is asked for.
const ANGLE_BITS: u32 = 20;
const ANGLE_STEPS: f32 = (1u32 << ANGLE_BITS) as f32;

/// Raw thirty-two bits, for a value that is an integer wearing a float's clothes.
///
/// A sticker id lives in the same slot as a paint kit's float, but is not a float: 1507 arrives as
/// 2.112e-42, which is that integer reinterpreted. So it is written as bits, not converted.
fn encode_raw_u32(value: u32) -> Bits {
    let mut writer = BitWriter::new();
    writer.write_nbits(value, 32);
    Bits {
        bytes: writer.finish(),
        len: 32,
    }
}

/// A varint, the way an unsigned field is read.
fn encode_varint(value: u32) -> Bits {
    let mut writer = BitWriter::new();
    let before = writer.bits_written();
    writer.write_varint(value);
    let len = writer.bits_written() - before;
    Bits {
        bytes: writer.finish(),
        len,
    }
}

/// A resized vector must prune its outgoing delta as well as cached entity state.
/// Otherwise an old 96-byte recipe can follow a new length of 64 with child 64,
/// which the native client rejects before applying the rest of that update.
fn vector_child_in_bounds(child: &[i32], vector: &[i32], len: usize) -> bool {
    !child.starts_with(vector)
        || child.len() <= vector.len()
        || (child[vector.len()] >= 0 && (child[vector.len()] as usize) < len)
}

fn prune_vector_delta(fields: &mut Vec<(Vec<i32>, Bits)>, vector: &[i32], len: usize) {
    fields.retain(|(child, _)| vector_child_in_bounds(child, vector, len));
}

fn observe_slot_pool_input(
    previous: &mut Option<Vec<Vec<u8>>>,
    fields: &[(Vec<i32>, Bits)],
    body: i32,
    slots: i32,
    is_create: bool,
) {
    // The cached pool describes our last authored output, not inherited writes
    // from an earlier graft. Even an unscheduled update can invalidate it.
    // An unscheduled checkpoint/create can restore a baseline pool without
    // explicitly writing any of its fields in this delta.
    if is_create
        || fields
            .iter()
            .any(|(path, _)| path.starts_with(&[body, slots]))
    {
        *previous = None;
    }
}

fn slot_pool_writes(
    desired: &[Vec<u8>],
    previous: Option<&[Vec<u8>]>,
    body: i32,
    slots: i32,
) -> Vec<(Vec<i32>, Bits)> {
    let mut writes = Vec::new();
    if previous.map(<[Vec<u8>]>::len) != Some(desired.len()) {
        writes.push((vec![body, slots], encode_varint(desired.len() as u32)));
    }
    for (slot, topology) in desired.iter().enumerate() {
        if previous.and_then(|p| p.get(slot)) != Some(topology) {
            writes.push((
                vec![body, slots, slot as i32, 0],
                encode_binary_block(topology),
            ));
        }
    }
    writes
}

/// A plain thirty-two bit float, the way a no-scale field is read.
///
/// Econ attribute values arrive this way — a paint kit, a seed and a wear all sit in the same list
/// as raw floats, which is why 818.0 survives exactly. Nothing about them is quantised, so a new
/// value can simply be written.
fn encode_f32_noscale(value: f32) -> Bits {
    let mut writer = BitWriter::new();
    writer.write_nbits(value.to_bits(), 32);
    Bits {
        bytes: writer.finish(),
        len: 32,
    }
}

/// Encode a pitch/yaw/roll triple the way `QanglePresDecoder` reads it.
///
/// Three presence flags, then twenty bits for each component present.
///
/// Pitch and yaw are always sent, even when zero. Marking a component absent does not mean it is
/// zero — it means it is unchanged, so the client keeps whatever it last held. Omitting a level
/// pitch therefore leaves the recorded pitch standing, which is most of the way to the original
/// view and showed up as the spin still bobbing. Roll is genuinely always zero here and stays out.
fn encode_qangle_pres(angles: [f32; 3]) -> Bits {
    let mut writer = BitWriter::new();
    let mut len = 0usize;
    let present = [true, true, false];
    for sent in present {
        writer.write_nbits(u32::from(sent), 1);
        len += 1;
    }
    for (value, sent) in angles.into_iter().zip(present) {
        if !sent {
            continue;
        }
        let wrapped = value.rem_euclid(360.0);
        let shifted = if wrapped >= 180.0 {
            wrapped - 360.0
        } else {
            wrapped
        };
        let step = ((shifted + 180.0) / 360.0 * ANGLE_STEPS).round();
        let n = step.clamp(0.0, ANGLE_STEPS - 1.0) as u32;
        writer.write_nbits(n, ANGLE_BITS);
        len += ANGLE_BITS as usize;
    }
    Bits {
        bytes: writer.finish(),
        len,
    }
}

fn append_bits(writer: &mut BitWriter, bits: &Bits) {
    for pos in 0..bits.len {
        writer.write_nbits(((bits.bytes[pos / 8] >> (pos % 8)) & 1) as u32, 1);
    }
}
fn restore_alternates(packet: &mut CsvcMsgPacketEntities, prior: &BTreeMap<i32, i32>) {
    for (&entity_index, &baseline_index) in prior {
        // An assignment on the live packet supersedes the pre-cut assignment.
        if !packet
            .alternate_baselines
            .iter()
            .any(|b| b.entity_index() == entity_index)
        {
            packet.alternate_baselines.push(
                csgoproto::csvc_msg_packet_entities::AlternateBaselineT {
                    entity_index: Some(entity_index),
                    baseline_index: Some(baseline_index),
                },
            );
        }
    }
}
#[derive(Clone)]
struct Entity {
    class: u32,
    serial: u32,
    unknown: u32,
    values: BTreeMap<Vec<i32>, Bits>,
}

/// A binary-block field's payload, as the bytes actually on the wire.
///
/// The parser hands these back through `String::from_utf8_lossy`, which replaces every byte that
/// is not valid UTF-8 — fine for names, destructive for a field that is not text at all. The bits
/// are already captured verbatim for re-encoding, so the bytes are recovered from those instead:
/// a binary block is a varint length followed by that many bytes.
/// The bytes behind a census value rendered as `Bytes(<hex>)`.
///
/// The census formats a binary block for a person to read, so anything consuming it programmatically
/// has to turn it back. Kept next to the encoder it mirrors.
pub fn binary_block_of(value: &str) -> Option<Vec<u8>> {
    let inner = value.strip_prefix("Bytes(")?.strip_suffix(')')?;
    if inner.len() % 2 != 0 {
        return None;
    }
    (0..inner.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(inner.get(at..at + 2)?, 16).ok())
        .collect()
}

/// How far from a discharge a restart may land and still be the shot's own animation. The overlay
/// is retriggered as the shot is fired, and entity updates and discharges do not always share a
/// tick exactly.
/// How far off the wanted yaw a recorded direction may be and still be considered, so that one
/// continuous in pitch can be preferred instead. At a turn a second this is under a tick's worth of
/// rotation, and a wider choice of candidates is what keeps the pitch from jumping.
/// Attribute definition of the first sticker slot's id. Each slot takes four definitions — id,
/// wear, scale, rotation — so slot n's id is this plus four n.
const STICKER_SLOT_BASE: u32 = 113;

/// The aim field's neutral setting, and the bounds a recording actually uses. Writing outside these
/// crashed the client during playback, so a requested setting is mapped inside them.
const AIM_NEUTRAL: u32 = 32768;
const AIM_OBSERVED_MIN: u32 = 676;
const AIM_OBSERVED_MAX: u32 = 58020;

const SPIN_YAW_SLACK: f32 = 4.0;

const OVERLAY_SHOT_SLACK: i32 = 2;

/// How long after a discharge the overlay stays pinned. An attack animation outlives the shot that
/// started it, so releasing it the moment the discharge passes lets the tail play.
const OVERLAY_HOLD_AFTER: i32 = 24;

/// The normalised time an overlay is held at: the very end of its clip, which has nothing left to
/// play. Writing zero would instead hold it on its first frame, which is the muzzle flash.
const OVERLAY_HELD_TIME: u32 = u16::MAX as u32;

/// The sixteen bit time field at `time_at`, if the payload is long enough to hold it.
fn read_time(payload: &[u8], time_at: u32) -> Option<u32> {
    if time_at as usize + 16 > payload.len() * 8 {
        return None;
    }
    let mut value = 0u32;
    for step in 0..16u32 {
        let at = time_at + step;
        value |= (((payload[(at / 8) as usize] >> (at % 8)) & 1) as u32) << step;
    }
    Some(value)
}

fn binary_block(bits: &Bits) -> Option<Vec<u8>> {
    let bytes = &bits.bytes;
    let mut at = 0usize;
    let mut length = 0usize;
    for shift in (0..35).step_by(7) {
        let byte = *bytes.get(at)?;
        at += 1;
        length |= ((byte & 0x7f) as usize) << shift;
        if byte & 0x80 == 0 {
            return bytes.get(at..at + length).map(<[u8]>::to_vec);
        }
    }
    None
}
/// The payload buffer's size in bits, for reporting how much of it a recipe occupies.
pub const PAYLOAD_BITS: u32 = crate::poserecipe::PAYLOAD_BYTES as u32 * 8;

/// The inverse of [`binary_block`]: a varint length followed by the bytes.
///
/// Needed because appending a task makes the topology blob *longer*, and every edit before this one
/// replaced a field's value without changing its size. A `Bits` carries its own bit count, so a
/// replacement of a different length is legal as long as it is encoded the way the reader expects.
fn encode_binary_block(blob: &[u8]) -> Bits {
    let mut bytes = Vec::new();
    let mut length = blob.len();
    loop {
        let mut byte = (length & 0x7f) as u8;
        length >>= 7;
        if length != 0 {
            byte |= 0x80;
        }
        bytes.push(byte);
        if length == 0 {
            break;
        }
    }
    bytes.extend_from_slice(blob);
    let len = bytes.len() * 8;
    Bits { bytes, len }
}

struct State<'a> {
    non_transmitted: BTreeSet<i32>,
    entities: BTreeMap<i32, Entity>,
    classes: &'a [Class],
    qf: &'a QfMapper,
    huf: &'a [(u8, u8)],
    baselines: BTreeMap<u32, Vec<u8>>,
    baseline_entries: Vec<Vec<u8>>,
    alternate_baselines: BTreeMap<i32, i32>,
    template: Option<CsvcMsgPacketEntities>,
}

fn paths(reader: &mut Bitreader, huf: &[(u8, u8)]) -> Result<Vec<FieldPath>> {
    let mut fp = generate_fp();
    let mut result = Vec::new();
    loop {
        if reader.bits_left < 17 {
            reader.refill();
        }
        let (symbol, length) = huf[reader.peek(17) as usize];
        reader.consume(length as u32);
        if symbol == 39 {
            break;
        }
        do_op(symbol, reader, &mut fp)?;
        ensure!(result.len() < 100_000, "too many field paths");
        result.push(fp);
    }
    Ok(result)
}
impl State<'_> {
    /// Apply one entity packet, reporting what it wrote to one entity inside a tick range.
    ///
    /// Deliberately a second path rather than a flag on `apply`: the trim writer's accumulation
    /// must stay exactly as it is, and a census that silently changed it would be the worst kind
    /// of diagnostic.
    fn observe(
        &mut self,
        msg: &CsvcMsgPacketEntities,
        tick: i32,
        entity_id: i32,
        from_tick: i32,
        to_tick: i32,
        only_fields: Option<&[&str]>,
        out: &mut Vec<FieldWrite>,
        events: &mut Vec<EntityEvent>,
        checkpoint: bool,
    ) -> Result<()> {
        self.observe_audited(
            msg,
            tick,
            entity_id,
            from_tick,
            to_tick,
            only_fields,
            out,
            events,
            checkpoint,
            None,
        )
    }
    fn observe_audited(
        &mut self,
        msg: &CsvcMsgPacketEntities,
        tick: i32,
        entity_id: i32,
        from_tick: i32,
        to_tick: i32,
        only_fields: Option<&[&str]>,
        out: &mut Vec<FieldWrite>,
        events: &mut Vec<EntityEvent>,
        checkpoint: bool,
        mut audit: Option<&mut ag2_audit::Audit>,
    ) -> Result<()> {
        let watching = tick >= from_tick && tick <= to_tick;
        let data = msg.entity_data();
        let mut reader = Bitreader::new(data);
        let mut id = -1;
        let class_bits = (self.classes.len() as f32).log2().ceil() as u32;
        for command_ordinal in 0..msg.updated_entries() {
            id += 1 + reader.read_u_bit_var()? as i32;
            let command = reader.read_nbits(2)?;
            if command & 1 != 0 {
                if let Some(audit) = audit.as_deref_mut() {
                    audit.deleted(id, command_ordinal);
                }
                if let Some(entity) = self.entities.get(&id) {
                    events.push(EntityEvent {
                        tick,
                        entity: id,
                        class_id: entity.class,
                        class_name: self.classes[entity.class as usize].name.clone(),
                        serial: entity.serial,
                        kind: "delete",
                        checkpoint,
                    });
                }
                self.entities.remove(&id);
                if command == 3 {
                    self.alternate_baselines.remove(&id);
                }
                continue;
            }
            if command == 2 {
                let class = reader.read_nbits(class_bits)?;
                let serial = reader.read_nbits(17)?;
                let mut entity = Entity {
                    class,
                    serial,
                    unknown: reader.read_varint()?,
                    values: BTreeMap::new(),
                };
                events.push(EntityEvent {
                    tick,
                    entity: id,
                    class_id: class,
                    class_name: self.classes[class as usize].name.clone(),
                    serial,
                    kind: "create",
                    checkpoint,
                });
                if let Some(baseline) = self.baseline(id, class)? {
                    update(
                        &mut Bitreader::new(baseline),
                        baseline,
                        &mut entity,
                        &self.classes[class as usize].serializer,
                        self.qf,
                        self.huf,
                    )?;
                }
                if let Some(audit) = audit.as_deref_mut() {
                    audit.created(
                        id,
                        command_ordinal,
                        &entity,
                        &self.classes[class as usize].serializer,
                        self.qf,
                    )?;
                }
                self.entities.insert(id, entity);
            } else if msg.has_pvs_vis_bits_deprecated() != 0 {
                let pvs = reader.read_nbits(2)?;
                ensure!(
                    pvs == 0 || pvs == 2,
                    "entity {id} has dormant PVS bits {pvs}"
                );
            }
            let entity = self
                .entities
                .get_mut(&id)
                .with_context(|| format!("tick {tick} entity {id}: update before entity create"))?;
            let serializer = &self.classes[entity.class as usize].serializer;
            // A negative index means every entity, which is how a class is found when its index
            // is not known in advance.
            if watching && (id == entity_id || entity_id < 0) {
                // Decode the paths and values in step, recording each, then merge exactly as the
                // ordinary update would so the accumulated state stays correct either way.
                for (field_ordinal, fp) in paths(&mut reader, self.huf)?.into_iter().enumerate() {
                    let field = find_field(&fp, serializer)?;
                    let begin =
                        data.len() * 8 - reader.bits_remaining().context("missing bit offset")?;
                    let value = reader.decode(&get_decoder_from_field(field)?, self.qf)?;
                    let end =
                        data.len() * 8 - reader.bits_remaining().context("missing bit offset")?;
                    let name = field_name(field);
                    let bits = slice_bits(data, begin, end);
                    if let Some(audit) = audit.as_deref_mut() {
                        audit.field(
                            id,
                            command_ordinal,
                            field_ordinal,
                            entity,
                            serializer,
                            self.qf,
                            &fp.path[..=fp.last],
                            field,
                            &value,
                        )?;
                    }
                    if only_fields.is_none_or(|wanted| {
                        let bare = name.rsplit('.').next().unwrap_or(&name);
                        wanted.contains(&bare)
                    }) {
                        out.push(FieldWrite {
                            tick,
                            entity: id,
                            name,
                            value: match &value {
                                // A binary block is not text; show the bytes that were sent.
                                Variant::String(_) => binary_block(&bits)
                                    .map(|raw| {
                                        format!(
                                            "Bytes({})",
                                            raw.iter()
                                                .map(|b| format!("{b:02x}"))
                                                .collect::<String>()
                                        )
                                    })
                                    .unwrap_or_else(|| format!("{value:?}")),
                                _ => format!("{value:?}"),
                            },
                            path: fp.path[..=fp.last].to_vec(),
                            bits: bits.clone(),
                        });
                    }
                    let key = fp.path[..=fp.last].to_vec();
                    // Match `update`: a watched field walk must not retain
                    // children removed by a vector shrink or pointer clear.
                    match (field, &value) {
                        (Field::Vector(_), Variant::U32(size)) => {
                            entity.values.retain(|path, _| {
                                !path.starts_with(&key)
                                    || path.len() <= key.len()
                                    || path[key.len()] < *size as i32
                            })
                        }
                        (Field::Pointer(_), Variant::Bool(false)) => entity
                            .values
                            .retain(|path, _| !path.starts_with(&key) || path.len() <= key.len()),
                        _ => {}
                    }
                    entity.values.insert(key, bits);
                }
            } else {
                update(&mut reader, data, entity, serializer, self.qf, self.huf)?;
            }
        }
        Ok(())
    }
}

/// The schema name of a field, for a census a person reads.
fn field_name(field: &Field) -> String {
    match field {
        Field::Value(value) => value.full_name.clone(),
        Field::Vector(_) => "<vector length>".to_string(),
        Field::Pointer(_) => "<pointer>".to_string(),
        Field::Array(_) => "<array>".to_string(),
        Field::Serializer(_) => "<serializer>".to_string(),
        Field::None => "<none>".to_string(),
    }
}

fn update(
    reader: &mut Bitreader,
    data: &[u8],
    entity: &mut Entity,
    serializer: &Serializer,
    qf: &QfMapper,
    huf: &[(u8, u8)],
) -> Result<()> {
    for fp in paths(reader, huf)? {
        let field = find_field(&fp, serializer)?;
        let begin = data.len() * 8 - reader.bits_remaining().context("missing bit offset")?;
        let value = reader.decode(&get_decoder_from_field(field)?, qf)?;
        let end = data.len() * 8 - reader.bits_remaining().context("missing bit offset")?;
        let key = fp.path[..=fp.last].to_vec();
        // A shrink or cleared pointer discards its old children; later growth cannot
        // resurrect values which no longer belong to the source entity state.
        match (field, &value) {
            (Field::Vector(_), Variant::U32(size)) => entity.values.retain(|path, _| {
                !path.starts_with(&key) || path.len() <= key.len() || path[key.len()] < *size as i32
            }),
            (Field::Pointer(_), Variant::Bool(false)) => entity
                .values
                .retain(|path, _| !path.starts_with(&key) || path.len() <= key.len()),
            _ => {}
        }
        entity.values.insert(key, slice_bits(data, begin, end));
    }
    Ok(())
}
impl State<'_> {
    fn tables(&mut self, tables: &CDemoStringTables) {
        for table in &tables.tables {
            if table.table_name() == "instancebaseline" {
                // Full-packet checkpoints may carry an empty string-table
                // snapshot. That means no table replacement, not that the
                // signon class baselines ceased to exist.
                self.baselines.clear();
                self.baseline_entries.clear();
                for item in &table.items {
                    self.baseline_entries.push(item.data().to_vec());
                    if let Ok(id) = item.str().parse() {
                        self.baselines.insert(id, item.data().to_vec());
                    }
                }
            }
        }
    }
    fn baseline(&self, id: i32, class: u32) -> Result<Option<&[u8]>> {
        if let Some(&index) = self.alternate_baselines.get(&id) {
            Ok(Some(
                self.baseline_entries
                    .get(index as usize)
                    .context("alternate baseline table entry is missing")?
                    .as_slice(),
            ))
        } else {
            Ok(self.baselines.get(&class).map(Vec::as_slice))
        }
    }
    fn packet(&mut self, payload: &[u8]) -> Result<()> {
        let msg = CsvcMsgPacketEntities::decode(payload)?;
        ensure!(
            msg.outofpvs_entity_updates
                .as_ref()
                .is_none_or(|x| x.count() == 0),
            "unsupported extended entity encoding"
        );
        // These are persistent entity-to-string-table indices, not class ids or
        // suffixes in the table keys. A later recreation may omit the assignment.
        for alternate in &msg.alternate_baselines {
            ensure!(
                (0..32768).contains(&alternate.entity_index()),
                "invalid alternate baseline entity"
            );
            if alternate.baseline_index() == -1 {
                self.alternate_baselines.remove(&alternate.entity_index());
            } else {
                ensure!(
                    alternate.baseline_index() >= 0,
                    "invalid alternate baseline index"
                );
                self.alternate_baselines
                    .insert(alternate.entity_index(), alternate.baseline_index());
            }
        }
        let data = msg.entity_data();
        let mut reader = Bitreader::new(data);
        let mut id = -1;
        let class_bits = (self.classes.len() as f32).log2().ceil() as u32;
        for _ in 0..msg.updated_entries() {
            id += 1 + reader.read_u_bit_var()? as i32;
            let command = reader.read_nbits(2)?;
            if command & 1 != 0 {
                self.entities.remove(&id);
                // A leave is absent from the full transmitted snapshot, but may
                // re-enter using the same alternate baseline. Only deletion
                // releases that persistent assignment.
                if command == 3 {
                    self.alternate_baselines.remove(&id);
                }
                continue;
            }
            if command == 2 {
                let class = reader.read_nbits(class_bits)?;
                let mut entity = Entity {
                    class,
                    serial: reader.read_nbits(17)?,
                    unknown: reader.read_varint()?,
                    values: BTreeMap::new(),
                };
                if let Some(baseline) = self.baseline(id, class)? {
                    update(
                        &mut Bitreader::new(baseline),
                        baseline,
                        &mut entity,
                        &self.classes[class as usize].serializer,
                        self.qf,
                        self.huf,
                    )?;
                }
                self.entities.insert(id, entity);
            } else if msg.has_pvs_vis_bits_deprecated() != 0 {
                let pvs = reader.read_nbits(2)?;
                ensure!(
                    pvs == 0 || pvs == 2,
                    "entity {id} has dormant PVS bits {pvs}"
                );
            }
            let entity = self
                .entities
                .get_mut(&id)
                .context("update before entity create")?;
            update(
                &mut reader,
                data,
                entity,
                &self.classes[entity.class as usize].serializer,
                self.qf,
                self.huf,
            )?;
        }
        if let Some(non_transmitted) = &msg.non_transmitted_entities {
            let mut reader = Bitreader::new(non_transmitted.data());
            let mut id = -1i32;
            for _ in 0..non_transmitted.header_count() {
                id = id
                    .checked_add(1 + reader.read_u_bit_var()? as i32)
                    .context("non-transmitted entity index overflow")?;
                ensure!(
                    (0..32768).contains(&id),
                    "invalid non-transmitted entity index"
                );
                if reader.read_boolean()? {
                    self.non_transmitted.insert(id);
                } else {
                    self.non_transmitted.remove(&id);
                }
            }
            ensure!(
                reader.bits_remaining().unwrap_or(0) < 8,
                "unexpected non-transmitted entity payload"
            );
        }
        self.template = Some(msg);
        Ok(())
    }
    fn encode(&self) -> Result<Vec<u8>> {
        let mut writer = BitWriter::new();
        let mut lengths = BitWriter::new();
        let class_bits = (self.classes.len() as f32).log2().ceil() as u32;
        let mut previous = -1;
        for (&id, entity) in &self.entities {
            writer.write_u_bit_var((id - previous - 1) as u32);
            previous = id;
            writer.write_nbits(2, 2);
            writer.write_nbits(entity.class, class_bits);
            writer.write_nbits(entity.serial, 17);
            writer.write_varint(entity.unknown);
            let mut baseline_entity = Entity {
                class: entity.class,
                serial: entity.serial,
                unknown: entity.unknown,
                values: BTreeMap::new(),
            };
            if let Some(baseline) = self.baselines.get(&entity.class) {
                update(
                    &mut Bitreader::new(baseline),
                    baseline,
                    &mut baseline_entity,
                    &self.classes[entity.class as usize].serializer,
                    self.qf,
                    self.huf,
                )?;
            }
            let resets: BTreeSet<Vec<i32>> = baseline_entity
                .values
                .keys()
                .filter(|path| !entity.values.contains_key(*path))
                .flat_map(|path| (1..path.len()).map(|len| path[..len].to_vec()))
                .filter(|path| entity.values.contains_key(path))
                .collect();
            let changed = entity
                .values
                .iter()
                .filter(|(key, bits)| {
                    baseline_entity
                        .values
                        .get(*key)
                        .is_none_or(|b| b.len != bits.len || b.bytes != bits.bytes)
                        // Repeating a vector length or cleared pointer also
                        // removes baseline children. Equal scalar bits alone
                        // do not prove that this update can be omitted.
                        || resets.contains(*key)
                })
                .collect::<Vec<_>>();
            let start = writer.bits_written();
            encode_paths(&mut writer, changed.iter().map(|(k, _)| *k), self.huf)?;
            for (_, value) in changed {
                append_bits(&mut writer, value);
            }
            lengths.write_varint((writer.bits_written() - start) as u32);
        }
        let mut msg = self.template.clone().context("no entity packets")?;
        msg.updated_entries = Some(self.entities.len() as i32);
        msg.legacy_is_delta = Some(false);
        msg.delta_from = None;
        msg.update_baseline = Some(false);
        // Flatten the current fields against the ordinary class baseline. An
        // alternate assigned since this entity was created may contain additional
        // owner-only fields which must not be injected into its present state.
        // Restore those future-create assignments in the first live entity packet.
        msg.alternate_baselines.clear();
        msg.has_pvs_vis_bits_deprecated = Some(0);
        msg.entity_data = Some(writer.finish().into());
        msg.serialized_entities = Some(lengths.finish().into());
        let mut headers = BitWriter::new();
        let mut previous = -1;
        for &id in &self.non_transmitted {
            headers.write_u_bit_var((id - previous - 1) as u32);
            headers.write_nbits(1, 1);
            previous = id;
        }
        msg.non_transmitted_entities = Some(
            csgoproto::csvc_msg_packet_entities::NonTransmittedEntitiesT {
                header_count: Some(self.non_transmitted.len() as i32),
                data: Some(headers.finish().into()),
            },
        );
        Ok(msg.encode_to_vec())
    }
}
fn symbol(writer: &mut BitWriter, symbol: u8, huf: &[(u8, u8)]) -> Result<()> {
    let (bits, (_, len)) = huf
        .iter()
        .enumerate()
        .find(|(_, (s, _))| *s == symbol)
        .context("missing huffman symbol")?;
    writer.write_nbits(bits as u32, *len as u32);
    Ok(())
}
fn fp_uint(writer: &mut BitWriter, n: u32) {
    for bits in [2, 4, 10, 17] {
        if n < (1 << bits) {
            writer.write_nbits(1, 1);
            writer.write_nbits(n, bits);
            return;
        }
        writer.write_nbits(0, 1);
    }
    writer.write_nbits(n, 31);
}
fn signed(writer: &mut BitWriter, n: i32) {
    writer.write_varint(((n as u32) << 1) ^ ((n >> 31) as u32));
}
fn encode_paths<'a>(
    writer: &mut BitWriter,
    paths: impl Iterator<Item = &'a Vec<i32>>,
    huf: &[(u8, u8)],
) -> Result<()> {
    let mut current = vec![-1];
    for target in paths {
        if target.len() < current.len() {
            symbol(writer, 35, huf)?;
            fp_uint(writer, (current.len() - target.len()) as u32);
            current.truncate(target.len());
            for (old, new) in current.iter_mut().zip(target) {
                writer.write_nbits(u32::from(*old != *new), 1);
                if *old != *new {
                    signed(writer, *new - *old);
                }
                *old = *new;
            }
        } else if target.len() == current.len() {
            symbol(writer, 36, huf)?;
            for (old, new) in current.iter_mut().zip(target) {
                writer.write_nbits(u32::from(*old != *new), 1);
                if *old != *new {
                    signed(writer, *new - *old);
                }
                *old = *new;
            }
        } else {
            symbol(writer, 26, huf)?;
            for (old, new) in current.iter_mut().zip(target) {
                writer.write_nbits(u32::from(*old != *new), 1);
                if *old != *new {
                    signed(writer, *new - *old - 1);
                }
                *old = *new;
            }
            writer.write_u_bit_var((target.len() - current.len()) as u32);
            for &item in &target[current.len()..] {
                fp_uint(writer, item as u32);
            }
            current = target.clone();
        }
    }
    symbol(writer, 39, huf)
}

fn encode_varint_u64(mut value: u64) -> Bits {
    let mut bytes = Vec::new();
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        bytes.push(byte);
        if value == 0 {
            break;
        }
    }
    Bits {
        len: bytes.len() * 8,
        bytes,
    }
}

/// Experimental restoration probe: append one scalar field to an instance
/// baseline while retaining every existing decoded value bit for bit.
pub fn append_baseline_scalar(
    demo: &[u8],
    baseline: &[u8],
    class_id: usize,
    path: &[i32],
    value: u32,
) -> Result<Vec<u8>> {
    append_baseline_encoded(demo, baseline, class_id, path, &encode_varint(value))
}

pub fn append_baseline_raw(
    demo: &[u8],
    baseline: &[u8],
    class_id: usize,
    path: &[i32],
    bytes: &[u8],
) -> Result<Vec<u8>> {
    append_baseline_bits(demo, baseline, class_id, path, bytes, bytes.len() * 8)
}

pub fn append_baseline_bits(
    demo: &[u8],
    baseline: &[u8],
    class_id: usize,
    path: &[i32],
    bytes: &[u8],
    bit_length: usize,
) -> Result<Vec<u8>> {
    ensure!(
        !bytes.is_empty() && bit_length > 0 && bit_length <= bytes.len() * 8,
        "invalid encoded field bit length"
    );
    append_baseline_encoded(
        demo,
        baseline,
        class_id,
        path,
        &Bits {
            bytes: bytes.to_vec(),
            len: bit_length,
        },
    )
}

fn append_baseline_encoded(
    demo: &[u8],
    baseline: &[u8],
    class_id: usize,
    path: &[i32],
    encoded_value: &Bits,
) -> Result<Vec<u8>> {
    ensure!(!path.is_empty() && path.len() <= 7, "invalid field path");
    let idx = index::DemoIndex::build(demo)?;
    let builder = BoundaryBuilder::new(demo, &idx)?;
    let serializer = &builder
        .classes
        .get(class_id)
        .context("class not present")?
        .serializer;
    let mut target = generate_fp();
    target.last = path.len() - 1;
    target.path[..path.len()].copy_from_slice(path);
    let target_field = find_field(&target, serializer).context("resolve injected field")?;
    let mut reader = Bitreader::new(baseline);
    let original_paths = paths(&mut reader, &builder.huf).context("decode baseline paths")?;
    ensure!(
        !original_paths
            .iter()
            .any(|fp| fp.last == target.last && fp.path[..=fp.last] == *path),
        "target field already in baseline"
    );
    let values_start = baseline.len() * 8 - reader.bits_remaining().context("no field values")?;
    for fp in &original_paths {
        let field = find_field(fp, serializer).context("resolve existing baseline field")?;
        reader
            .decode(&get_decoder_from_field(field)?, &builder.qf)
            .context("decode existing baseline field")?;
    }
    let values_end = baseline.len() * 8 - reader.bits_remaining().context("no field end")?;
    ensure!(
        baseline.len() * 8 - values_end < 8,
        "unexpected trailing baseline bits"
    );
    let mut value_check = Bitreader::new(&encoded_value.bytes);
    let proposed = value_check.decode(&get_decoder_from_field(target_field)?, &builder.qf)?;
    ensure!(
        encoded_value.bytes.len() * 8 - value_check.bits_remaining().unwrap_or(0)
            == encoded_value.len,
        "injected field did not consume all encoded bits"
    );
    let mut writer = BitWriter::new();
    let all_paths = original_paths
        .iter()
        .map(|fp| fp.path[..=fp.last].to_vec())
        .chain(std::iter::once(path.to_vec()))
        .collect::<Vec<_>>();
    encode_paths(&mut writer, all_paths.iter(), &builder.huf)?;
    for bit in values_start..values_end {
        writer.write_nbits(((baseline[bit / 8] >> (bit % 8)) & 1) as u32, 1);
    }
    append_bits(&mut writer, encoded_value);
    let output = writer.finish();
    let mut verify = Bitreader::new(&output);
    let decoded_paths = paths(&mut verify, &builder.huf)?;
    ensure!(
        decoded_paths.len() == all_paths.len(),
        "baseline path count changed"
    );
    for (at, (decoded, expected)) in decoded_paths.iter().zip(&all_paths).enumerate() {
        ensure!(
            decoded.path[..=decoded.last] == *expected,
            "baseline path changed"
        );
        let field = find_field(decoded, serializer)?;
        let decoded_value = verify.decode(&get_decoder_from_field(field)?, &builder.qf)?;
        if at + 1 == all_paths.len() {
            ensure!(
                decoded_value == proposed,
                "injected scalar did not round-trip"
            );
        }
    }
    ensure!(
        verify.bits_remaining().unwrap_or(8) < 8,
        "unexpected baseline remainder"
    );
    Ok(output)
}

/// Replace one existing model resource ID in a class baseline. Field paths and
/// every other encoded value are preserved exactly, including non-byte-aligned
/// values that a raw byte search cannot safely reach.
pub fn replace_baseline_model_id(
    demo: &[u8],
    baseline: &[u8],
    class_id: usize,
    path: &[i32],
    old: u64,
    new: u64,
) -> Result<Vec<u8>> {
    let idx = index::DemoIndex::build(demo)?;
    let builder = BoundaryBuilder::new(demo, &idx)?;
    let serializer = &builder
        .classes
        .get(class_id)
        .context("class not present")?
        .serializer;
    let mut reader = Bitreader::new(baseline);
    let field_paths = paths(&mut reader, &builder.huf)?;
    let replacement = encode_varint_u64(new);
    let mut values = Vec::with_capacity(field_paths.len());
    let mut replaced = 0usize;
    let mut target_span = None;
    for fp in &field_paths {
        let field = find_field(fp, serializer)?;
        let begin = baseline.len() * 8 - reader.bits_remaining().context("baseline value start")?;
        let value = reader.decode(&get_decoder_from_field(field)?, &builder.qf)?;
        let end = baseline.len() * 8 - reader.bits_remaining().context("baseline value end")?;
        let mut bits = slice_bits(baseline, begin, end);
        if fp.path[..=fp.last] == *path {
            ensure!(
                field_name(field).rsplit('.').next() == Some("m_hModel"),
                "target is not a model field"
            );
            ensure!(
                value == Variant::U64(old),
                "baseline model ID did not match expected old ID"
            );
            target_span = Some((begin, end));
            bits = replacement.clone();
            replaced += 1;
        }
        values.push(bits);
    }
    ensure!(
        replaced == 1,
        "expected exactly one baseline model field, found {replaced}"
    );
    ensure!(
        reader.bits_remaining().unwrap_or(8) < 8,
        "unexpected baseline remainder"
    );
    let (begin, end) = target_span.context("target model bit span missing")?;
    let output = if end - begin == replacement.len {
        let mut out = baseline.to_vec();
        for bit in 0..replacement.len {
            let position = begin + bit;
            let mask = 1u8 << (position % 8);
            if (replacement.bytes[bit / 8] >> (bit % 8)) & 1 != 0 {
                out[position / 8] |= mask;
            } else {
                out[position / 8] &= !mask;
            }
        }
        out
    } else {
        let mut writer = BitWriter::new();
        let path_keys = field_paths
            .iter()
            .map(|fp| fp.path[..=fp.last].to_vec())
            .collect::<Vec<_>>();
        encode_paths(&mut writer, path_keys.iter(), &builder.huf)?;
        for bits in &values {
            append_bits(&mut writer, bits);
        }
        writer.finish()
    };
    let mut verify = Bitreader::new(&output);
    let decoded = paths(&mut verify, &builder.huf)?;
    ensure!(
        decoded.len() == field_paths.len(),
        "baseline path count changed"
    );
    for ((actual, expected), bits) in decoded.iter().zip(&field_paths).zip(&values) {
        ensure!(
            actual.path[..=actual.last] == expected.path[..=expected.last],
            "baseline path changed"
        );
        let field = find_field(actual, serializer)?;
        let begin = output.len() * 8
            - verify
                .bits_remaining()
                .context("verification value start")?;
        verify.decode(&get_decoder_from_field(field)?, &builder.qf)?;
        let end = output.len() * 8 - verify.bits_remaining().context("verification value end")?;
        let actual_bits = slice_bits(&output, begin, end);
        ensure!(
            actual_bits == *bits,
            "baseline value bits changed unexpectedly"
        );
    }
    ensure!(
        verify.bits_remaining().unwrap_or(8) < 8,
        "unexpected rewritten baseline remainder"
    );
    Ok(output)
}

/// Inspect the exact encoded fields in one class's instance baseline.
pub fn inspect_restore_baseline(demo: &[u8], baseline: &[u8], class_id: usize) -> Result<String> {
    let idx = index::DemoIndex::build(demo)?;
    let builder = BoundaryBuilder::new(demo, &idx)?;
    let serializer = &builder
        .classes
        .get(class_id)
        .context("class not present")?
        .serializer;
    let mut reader = Bitreader::new(baseline);
    let field_paths = paths(&mut reader, &builder.huf)?;
    let mut result = Vec::new();
    for fp in field_paths {
        let path = &fp.path[..=fp.last];
        let field = find_field(&fp, serializer)
            .with_context(|| format!("resolve baseline field path {path:?}"))?;
        let begin = baseline.len() * 8 - reader.bits_remaining().context("missing value start")?;
        let value = reader
            .decode(&get_decoder_from_field(field)?, &builder.qf)
            .with_context(|| format!("decode baseline field path {path:?}"))?;
        let end = baseline.len() * 8 - reader.bits_remaining().context("missing value end")?;
        let bits = slice_bits(baseline, begin, end);
        result.push(serde_json::json!({
            "path": fp.path[..=fp.last], "name": field_name(field),
            "value": format!("{value:?}"), "bits": bits.bytes.iter().map(|b| format!("{b:02x}")).collect::<String>(),
            "bit_length": bits.len,
        }));
    }
    Ok(serde_json::to_string_pretty(&result)?)
}
fn payload(demo: &[u8], frame: &FrameRef) -> Result<Vec<u8>> {
    Ok(if frame.compressed {
        snap::raw::Decoder::new().decompress_vec(frame.payload(demo))?
    } else {
        frame.payload(demo).to_vec()
    })
}
#[derive(Clone, Default)]
struct Tables {
    formats: Vec<csgoproto::CsvcMsgCreateStringTable>,
    snapshot: CDemoStringTables,
}
impl Tables {
    fn overlay(&mut self, tables: CDemoStringTables) {
        for table in tables.tables {
            if let Some(old) = self
                .snapshot
                .tables
                .iter_mut()
                .find(|t| t.table_name == table.table_name)
            {
                *old = table;
            } else {
                self.snapshot.tables.push(table);
            }
        }
    }
    fn message(&mut self, message: &NetMessage) -> Result<()> {
        if message.msg_type == 51 {
            self.formats.clear();
            self.snapshot.tables.clear();
            return Ok(());
        }
        let (format, data, count) = match message.msg_type {
            44 => {
                let format =
                    csgoproto::CsvcMsgCreateStringTable::decode(message.payload.as_slice())?;
                self.formats.push(format.clone());
                let data = if format.data_compressed() {
                    snap::raw::Decoder::new().decompress_vec(format.string_data())?
                } else {
                    format.string_data().to_vec()
                };
                let count = format.num_entries();
                (format, data, count)
            }
            45 => {
                let update =
                    csgoproto::CsvcMsgUpdateStringTable::decode(message.payload.as_slice())?;
                let format = self
                    .formats
                    .get(update.table_id() as usize)
                    .context("unknown string-table update id")?
                    .clone();
                (
                    format,
                    update.string_data().to_vec(),
                    update.num_changed_entries(),
                )
            }
            _ => return Ok(()),
        };
        if !self
            .snapshot
            .tables
            .iter()
            .any(|t| t.table_name() == format.name())
        {
            self.snapshot
                .tables
                .push(csgoproto::c_demo_string_tables::TableT {
                    table_name: format.name.clone(),
                    table_flags: format.flags,
                    ..Default::default()
                });
        }
        let table = self
            .snapshot
            .tables
            .iter_mut()
            .find(|t| t.table_name() == format.name())
            .unwrap();
        let mut reader = Bitreader::new(&data);
        let mut index = -1i32;
        let mut history: Vec<String> = Vec::new();
        for _ in 0..count {
            index = if reader.read_boolean()? {
                index
                    .checked_add(1)
                    .context("string-table index overflow")?
            } else {
                reader
                    .read_varint()?
                    .checked_add(1)
                    .context("string-table index overflow")? as i32
            };
            ensure!(
                (0..1_000_000).contains(&index),
                "invalid string-table index"
            );
            if table.items.len() <= index as usize {
                table
                    .items
                    .resize_with(index as usize + 1, Default::default);
            }
            let item = &mut table.items[index as usize];
            if reader.read_boolean()? {
                let key = if reader.read_boolean()? {
                    let pos = reader.read_nbits(5)? as usize;
                    let len = reader.read_nbits(5)? as usize;
                    let prefix = history
                        .get(pos)
                        .and_then(|s| s.get(..len))
                        .context("invalid string-table key history")?;
                    format!("{}{}", prefix, reader.read_string()?)
                } else {
                    reader.read_string()?
                };
                if history.len() == 32 {
                    history.remove(0);
                }
                history.push(key.clone());
                item.str = Some(key);
            }
            if reader.read_boolean()? {
                let mut compressed = false;
                let bits = if format.user_data_fixed_size() {
                    format.user_data_size_bits() as u32
                } else {
                    if format.flags() & 1 != 0 {
                        compressed = reader.read_boolean()?;
                    }
                    (if format.using_varint_bitcounts() {
                        reader.read_u_bit_var()?
                    } else {
                        reader.read_nbits(17)?
                    }) * 8
                };
                let mut value = Vec::with_capacity(bits.div_ceil(8) as usize);
                for _ in 0..bits / 8 {
                    value.push(reader.read_nbits(8)? as u8);
                }
                if bits % 8 != 0 {
                    value.push(reader.read_nbits(bits % 8)? as u8);
                }
                if compressed {
                    value = snap::raw::Decoder::new().decompress_vec(&value)?;
                }
                item.data = Some(value.into());
            }
        }
        Ok(())
    }
}

/// One entity field an update actually wrote, named and decoded.
pub struct FieldWrite {
    pub tick: i32,
    pub entity: i32,
    pub name: String,
    pub value: String,
    /// The field path the engine addressed. For an array field the trailing element is the index,
    /// which is the only way to tell one element of a serialized blob from another.
    pub path: Vec<i32>,
    /// The value exactly as it sat on the wire.
    ///
    /// There is no value encoder here, so writing a value means reusing bits already seen carrying
    /// one. That is hopeless for something like an item index, where only one exact value will do,
    /// and works well for a continuous quantity like an angle: a round contains thousands of
    /// distinct look directions, so the nearest available encoding is never far from the wanted one.
    pub bits: Bits,
}

/// Body rotation inherited when this entity is created. A later packet write
/// to `m_angRotation` takes precedence over this instancebaseline value.
pub struct BodyYawCreate {
    pub tick: i32,
    pub value: Option<f32>,
}

/// Entity lifecycle command observed on the wire. A create in a full packet
/// restores a snapshot and does not establish the original spawn tick.
pub struct EntityEvent {
    pub tick: i32,
    pub entity: i32,
    pub class_id: u32,
    pub class_name: String,
    pub serial: u32,
    pub kind: &'static str,
    pub checkpoint: bool,
}

/// Every view direction one player was seen looking in, with the bits that encoded it.
///
/// `m_angEyeAngles` arrives as a rendered vector rather than a number, so the pitch and yaw are
/// read back out of the text. Only the entity asked about is collected; its own history is the
/// richest source of encodings for it.
pub fn eye_angle_encodings(
    demo: &[u8],
    idx: &index::DemoIndex,
    entity: i32,
) -> Result<Vec<(f32, f32, Bits)>> {
    let mut out = Vec::new();
    for write in field_writes(demo, idx, entity, i32::MIN, i32::MAX)? {
        if !write.name.ends_with("m_angEyeAngles") {
            continue;
        }
        let Some(inner) = write
            .value
            .split_once('[')
            .and_then(|(_, rest)| rest.split_once(']'))
            .map(|(inner, _)| inner)
        else {
            continue;
        };
        let parts = inner
            .split(',')
            .map(|p| p.trim().parse::<f32>())
            .collect::<std::result::Result<Vec<_>, _>>();
        if let Ok(parts) = parts {
            if parts.len() >= 2 {
                out.push((parts[0], parts[1], write.bits));
            }
        }
    }
    Ok(out)
}

/// Every field written to one entity over a tick range.
///
/// This is the census behind a suppression: a shot is only removable from the entity stream once
/// it is known which fields it writes, and that is a property of the demo rather than something to
/// be assumed from field names. It re-walks the packets with the real serializers, so the names
/// and values are the ones the engine wrote, not a guess at the schema.
///
/// Entity state is cumulative, so the walk starts at the demo's first checkpoint and runs forward;
/// it cannot begin at the range of interest.
pub fn field_writes(
    demo: &[u8],
    idx: &index::DemoIndex,
    entity_id: i32,
    from_tick: i32,
    to_tick: i32,
) -> Result<Vec<FieldWrite>> {
    Ok(scan_entity_stream(demo, idx, entity_id, from_tick, to_tick, None)?.0)
}

/// Packet writes plus the actual class/alternate-baseline body rotation at
/// each create. Kept separate so `fields` still means packet writes only.
pub fn field_writes_with_body_yaw_creates(
    demo: &[u8],
    idx: &index::DemoIndex,
    entity_id: i32,
    from_tick: i32,
    to_tick: i32,
) -> Result<(Vec<FieldWrite>, Vec<BodyYawCreate>)> {
    let (writes, _, creates) = scan_entity_stream(demo, idx, entity_id, from_tick, to_tick, None)?;
    Ok((writes, creates))
}

/// Like [`field_writes`], but stores only the named leaf fields. The parser
/// still applies every write so sparse values and checkpoint state remain valid.
pub fn field_writes_matching(
    demo: &[u8],
    idx: &index::DemoIndex,
    entity_id: i32,
    from_tick: i32,
    to_tick: i32,
    bare_names: &[&str],
) -> Result<Vec<FieldWrite>> {
    Ok(scan_entity_stream(demo, idx, entity_id, from_tick, to_tick, Some(bare_names))?.0)
}

/// Every field of one class's serializer with its path, as `fields` would name a write to it.
/// Vectors list the vector itself, not its dynamic elements; arrays list each element.
pub fn class_field_paths(
    demo: &[u8],
    idx: &index::DemoIndex,
    class_name: &str,
) -> Result<Vec<(String, Vec<i32>)>> {
    fn walk(fields: &[Field], prefix: &mut Vec<i32>, out: &mut Vec<(String, Vec<i32>)>) {
        for (i, field) in fields.iter().enumerate() {
            prefix.push(i as i32);
            match field {
                Field::Value(value) => out.push((value.full_name.clone(), prefix.clone())),
                Field::Serializer(inner) => walk(&inner.serializer.fields, prefix, out),
                Field::Pointer(inner) => walk(&inner.serializer.fields, prefix, out),
                Field::Vector(_) => out.push(("<vector>".to_string(), prefix.clone())),
                Field::Array(array) => {
                    for element in 0..array.length {
                        prefix.push(element as i32);
                        match array.field_enum.as_ref() {
                            Field::Value(value) => {
                                out.push((value.full_name.clone(), prefix.clone()))
                            }
                            Field::Serializer(inner) => walk(&inner.serializer.fields, prefix, out),
                            _ => out.push(("<array element>".to_string(), prefix.clone())),
                        }
                        prefix.pop();
                    }
                }
                Field::None => {}
            }
            prefix.pop();
        }
    }
    let builder = BoundaryBuilder::new(demo, idx)?;
    let class = builder
        .classes
        .iter()
        .find(|c| c.name == class_name)
        .with_context(|| format!("class {class_name} is not in this demo's class table"))?;
    let mut out = Vec::new();
    walk(&class.serializer.fields, &mut Vec::new(), &mut out);
    Ok(out)
}

pub fn entity_events(demo: &[u8], idx: &index::DemoIndex) -> Result<Vec<EntityEvent>> {
    Ok(scan_entity_stream(demo, idx, -1, i32::MAX, i32::MIN, None)?.1)
}

fn scan_entity_stream(
    demo: &[u8],
    idx: &index::DemoIndex,
    entity_id: i32,
    from_tick: i32,
    to_tick: i32,
    only_fields: Option<&[&str]>,
) -> Result<(Vec<FieldWrite>, Vec<EntityEvent>, Vec<BodyYawCreate>)> {
    let builder = BoundaryBuilder::new(demo, idx)?;
    let mut state = State {
        classes: &builder.classes,
        qf: &builder.qf,
        huf: &builder.huf,
        entities: BTreeMap::new(),
        baselines: BTreeMap::new(),
        baseline_entries: Vec::new(),
        alternate_baselines: BTreeMap::new(),
        non_transmitted: BTreeSet::new(),
        template: None,
    };
    let mut written = Vec::new();
    let mut events = Vec::new();
    let mut body_yaw_creates = Vec::new();
    for frame in &idx.frames {
        let tick = frame.tick();
        match frame.cmd {
            CMD_STRING_TABLES => state.tables(&CDemoStringTables::decode(
                payload(demo, frame)?.as_slice(),
            )?),
            CMD_FULL_PACKET => {
                let full = CDemoFullPacket::decode(payload(demo, frame)?.as_slice())?;
                if let Some(snapshot) = full.string_table {
                    state.tables(&snapshot);
                }
                if let Some(packet) = full.packet {
                    for message in read_messages(packet.data())? {
                        if message.msg_type == 55 {
                            let msg = CsvcMsgPacketEntities::decode(message.payload.as_slice())?;
                            let previous_events = events.len();
                            state.observe(
                                &msg,
                                tick,
                                entity_id,
                                from_tick,
                                to_tick,
                                only_fields,
                                &mut written,
                                &mut events,
                                true,
                            )?;
                            collect_body_yaw_creates(
                                &state,
                                &events[previous_events..],
                                entity_id,
                                tick,
                                &mut body_yaw_creates,
                            )?;
                        }
                    }
                }
            }
            CMD_SIGNON_PACKET | CMD_PACKET => {
                let packet = CDemoPacket::decode(payload(demo, frame)?.as_slice())?;
                for message in read_messages(packet.data())? {
                    if message.msg_type == 55 {
                        let msg = CsvcMsgPacketEntities::decode(message.payload.as_slice())?;
                        let previous_events = events.len();
                        state.observe(
                            &msg,
                            tick,
                            entity_id,
                            from_tick,
                            to_tick,
                            only_fields,
                            &mut written,
                            &mut events,
                            false,
                        )?;
                        collect_body_yaw_creates(
                            &state,
                            &events[previous_events..],
                            entity_id,
                            tick,
                            &mut body_yaw_creates,
                        )?;
                    }
                }
            }
            _ => {}
        }
    }
    Ok((written, events, body_yaw_creates))
}

fn collect_body_yaw_creates(
    state: &State<'_>,
    events: &[EntityEvent],
    entity_id: i32,
    tick: i32,
    output: &mut Vec<BodyYawCreate>,
) -> Result<()> {
    for event in events {
        if event.entity != entity_id || event.kind != "create" {
            continue;
        }
        let baseline = state.baseline(entity_id, event.class_id)?;
        let value = baseline
            .map(|data| {
                let serializer = &state.classes[event.class_id as usize].serializer;
                baseline_body_yaw(data, serializer, state.qf, state.huf)
            })
            .transpose()?
            .flatten();
        output.push(BodyYawCreate { tick, value });
    }
    Ok(())
}

fn baseline_body_yaw(
    baseline: &[u8],
    serializer: &Serializer,
    qf: &QfMapper,
    huf: &[(u8, u8)],
) -> Result<Option<f32>> {
    let mut reader = Bitreader::new(baseline);
    for path in paths(&mut reader, huf)? {
        let field = find_field(&path, serializer)?;
        let value = reader.decode(&get_decoder_from_field(field)?, qf)?;
        if field_name(field).ends_with(".CBodyComponentBaseAnimGraph.m_angRotation") {
            return Ok(match value {
                Variant::VecXYZ(angles) => Some(angles[1]),
                _ => None,
            });
        }
    }
    Ok(None)
}

/// The pawn fields a discharge writes, as the census on a real shot found them.
///
/// Not a guess at the schema: every name here was observed being written on the tick a player
/// fired and on no quiet tick around it. Only `m_SerializePoseRecipeAG2Dynamic` is deliberately
/// absent, because it carries the whole body's animation as opaque bytes and dropping it would
/// freeze the player rather than stop them firing.
pub const SHOT_PAWN_FIELDS: [&str; 12] = [
    // Playback of an earlier build showed an automatic burst collapsing to a single fire
    // animation rather than none, which says the animation is driven by networked state and not
    // only by the pose payload. This is the weapon services' own timing channel and the most
    // likely remaining trigger; it is an array under `CCSPlayer_WeaponServices` rather than part
    // of the whole-body pose, so dropping it should not freeze the player.
    "CCSPlayerPawn.CCSPlayer_WeaponServices.m_networkAnimTiming",
    "CCSPlayerPawn.m_iShotsFired",
    "CCSPlayerPawn.CCSPlayer_CameraServices.m_vecCsViewPunchAngle",
    "CCSPlayerPawn.CCSPlayer_CameraServices.m_nCsViewPunchAngleTick",
    "CCSPlayerPawn.CCSPlayer_CameraServices.m_flCsViewPunchAngleTickRatio",
    "CCSPlayerPawn.CCSPlayer_AimPunchServices.m_predictableBaseAngle",
    "CCSPlayerPawn.CCSPlayer_AimPunchServices.m_predictableBaseAngleVel",
    "CCSPlayerPawn.CCSPlayer_AimPunchServices.m_predictableBaseTick",
    "CCSPlayerPawn.CCSPlayer_AimPunchServices.m_predictableBaseTickInterpAmount",
    "CCSPlayerPawn.CCSPlayer_WeaponServices.m_bBlockInspectUntilNextGraphUpdate",
    // Hits landed, counted on the shooter rather than on whoever was hit — which is why dropping
    // it from the victim's fields missed it entirely. The client watches this to decide it landed
    // a bullet, and draws the blood and the spark from that: those two effects survive removing
    // every particle the demo sends, because the demo never sends them.
    "CCSPlayerPawn.CCSPlayer_BulletServices.m_totalHitsOnServer",
    "CCSPlayerPawn.m_bRagdollDamageHeadshot",
];

/// One entity's contribution to a rewritten packet, kept so the result can be read back.
struct RewrittenEntity {
    id: i32,
    step: u32,
    command: u32,
    create: Option<(u32, u32, u32)>,
    pvs: Option<u32>,
    fields: Vec<(Vec<i32>, Bits)>,
}

/// Decode a rewritten entity stream and check it says what the editor meant it to say.
///
/// Byte identity is the wrong test here. `encode_paths` emits the three general field-path
/// opcodes, while the engine picks from forty specialised ones, so a faithful re-encoding of an
/// unedited packet is routinely a different — usually longer — byte string. What has to hold is
/// that reading the result back yields the same field paths, in the same order, carrying the same
/// value bits. That is checked on every packet, edited or not, because an encoder that is wrong
/// on an untouched packet is wrong on an edited one too.
fn verify_rebuilt(
    rebuilt: &[u8],
    entries: &[RewrittenEntity],
    class_bits: u32,
    huf: &[(u8, u8)],
) -> Result<bool> {
    let mut reader = Bitreader::new(rebuilt);
    for entry in entries {
        if reader.read_u_bit_var()? != entry.step {
            return Ok(false);
        }
        if reader.read_nbits(2)? != entry.command {
            return Ok(false);
        }
        if entry.command & 1 != 0 {
            continue;
        }
        if let Some((class, serial, unknown)) = entry.create {
            if reader.read_nbits(class_bits)? != class
                || reader.read_nbits(17)? != serial
                || reader.read_varint()? != unknown
            {
                return Ok(false);
            }
        }
        if let Some(pvs) = entry.pvs {
            if reader.read_nbits(2)? != pvs {
                return Ok(false);
            }
        }
        let decoded = paths(&mut reader, huf)?;
        if decoded.len() != entry.fields.len() {
            return Ok(false);
        }
        for (fp, (key, bits)) in decoded.iter().zip(&entry.fields) {
            if &fp.path[..=fp.last] != key.as_slice() {
                return Ok(false);
            }
            let begin =
                rebuilt.len() * 8 - reader.bits_remaining().context("missing bit offset")?;
            // The value bits are copied verbatim, so they are compared rather than re-decoded:
            // this checks the transport, and leaves the meaning to the decoder that wrote them.
            for _ in 0..bits.len {
                reader.read_nbits(1)?;
            }
            let end = rebuilt.len() * 8 - reader.bits_remaining().context("missing bit offset")?;
            let read_back = slice_bits(rebuilt, begin, end);
            if read_back.len != bits.len || read_back.bytes != bits.bytes {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

/// The weapon fields a discharge writes, from the census of the weapon entity a shot names.
///
/// Written without a class prefix on purpose: the schema names these per weapon class
/// (`CWeaponHKP2000.m_iClip1`, `CAK47.m_iClip1`), so a prefixed list would silently stop matching
/// the moment the player picked up something else.
pub const SHOT_WEAPON_FIELDS: [&str; 11] = [
    // The weapon carries no pose recipe of its own — a census of a MAC-10 while it was being fired
    // shows only a transform under its CBodyComponentBaseAnimGraph — so its attack animation is
    // driven entirely by these two fields. The timestamp advances once per shot and retriggers the
    // animation, which was already dropped; the state is what moves the weapon from idle into
    // firing, and it is written once at the start of a burst rather than per shot. Dropping only
    // the timestamp therefore removed every repeat but left the first animation of each burst,
    // which is exactly the single shot seen during playback. Both have to go.
    "m_iWeaponGameplayAnimState",
    "m_iClip1",
    "m_fLastShotTime",
    "m_flRecoilIndex",
    "m_fAccuracyPenalty",
    "m_nNextPrimaryAttackTick",
    "m_flNextPrimaryAttackTickRatio",
    "m_nNextSecondaryAttackTick",
    "m_flNextSecondaryAttackTickRatio",
    "m_flWeaponGameplayAnimStateTimestamp",
    "m_flWatTickOffset",
];

/// The magazine's arithmetic across a suppression.
///
/// Every other suppressed weapon field is state that decays or is overwritten — an accuracy
/// penalty, a next-attack tick — and simply holding it is right. `m_iClip1` is the one that
/// carries arithmetic: a shot that did not happen leaves the magazine one round richer for the
/// rest of that magazine's life, and the field is transmitted as an absolute count rather than as
/// a decrement. Dropping the writes alone would therefore be wrong twice over — a reload inside
/// the window would be swallowed with them, and the first shot after the window would snap the
/// count back down to what was really there.
///
/// So the credit is tracked, and later counts are rewritten to carry it. Rewriting needs no
/// encoder: a magazine only counts down between reloads, so the value wanted now is one this
/// field already held a few ticks ago, and its exact bits were recorded when it did. A value that
/// was never observed is left alone and counted, rather than guessed at.
#[derive(Default)]
struct ClipCredit {
    /// Rounds owed back to this magazine.
    credit: i64,
    /// The last count the source transmitted, which is what distinguishes a shot from a reload.
    last_source: Option<i64>,
    /// Exact encodings this field has been seen carrying, keyed by the value they encode.
    seen: BTreeMap<i64, Bits>,
    /// Counts that had to be left as recorded because that value had never been observed.
    misses: usize,
}

/// Whether a field is the magazine count, whatever weapon class owns it.
/// What a weapon writes about itself when it is brought out.
///
/// Dropping the pawn's active-weapon handle stops a weapon being *selected*, but the weapon also
/// announces its own deploy, and a weapon that deploys is drawn whether or not anything points at
/// it. Skipping a switch cleanly means silencing both halves — otherwise the old gun stays in the
/// pawn's hand while the new one appears anyway, which is a worse picture than either.
pub const WEAPON_DEPLOY_FIELDS: [&str; 4] = [
    "m_iWeaponGameplayAnimState",
    "m_flWeaponGameplayAnimStateTimestamp",
    "m_nDeployTick",
    "m_flWatTickOffset",
];

/// Whether a field is the pawn's active weapon handle.
fn is_active_weapon_field(field: &Field) -> bool {
    let full = field_name(field);
    full.rsplit('.').next().unwrap_or(full.as_str()) == "m_hActiveWeapon"
}

/// The value `m_iWeaponGameplayAnimState` carries while a weapon is being fired.
///
/// The same field also carries holstering and deploying, so dropping it wholesale stops a weapon
/// ever leaving the hand — which shows up as the old gun still drawn while the next one appears.
/// Only the firing value is ours to remove.
const WEAPON_STATE_FIRING: i64 = 100;

fn is_weapon_state_field(field: &Field) -> bool {
    let full = field_name(field);
    full.rsplit('.').next().unwrap_or(full.as_str()) == "m_iWeaponGameplayAnimState"
}

fn is_clip_field(field: &Field) -> bool {
    let full = field_name(field);
    full.rsplit('.').next().unwrap_or(full.as_str()) == "m_iClip1"
}

fn as_integer(value: &Variant) -> Option<i64> {
    match value {
        Variant::U32(v) => Some(i64::from(*v)),
        Variant::I32(v) => Some(i64::from(*v)),
        Variant::U64(v) => Some(*v as i64),
        _ => None,
    }
}

/// What to do with one magazine count.
enum ClipAction {
    /// Pass the recorded bits through unchanged.
    Keep,
    /// Drop the write; the client keeps the fuller magazine it already has.
    Drop,
    /// Write a different count, using bits this field was seen carrying earlier.
    Rewrite(Bits),
}

impl ClipCredit {
    fn decide(&mut self, value: i64, bits: &Bits, suppressing: bool) -> ClipAction {
        self.seen.entry(value).or_insert_with(|| bits.clone());
        let previous = self.last_source.replace(value);

        // A count that went up is a reload or a pickup: the magazine is authoritative again and
        // owes nothing. This is what keeps a reload inside the suppressed window visible.
        if previous.is_some_and(|last| value > last) {
            self.credit = 0;
            return ClipAction::Keep;
        }

        if suppressing {
            self.credit += 1;
            return ClipAction::Drop;
        }

        if self.credit == 0 {
            return ClipAction::Keep;
        }

        match self.seen.get(&(value + self.credit)) {
            Some(bits) => ClipAction::Rewrite(bits.clone()),
            None => {
                self.misses += 1;
                ClipAction::Keep
            }
        }
    }
}

/// The reaction a hit writes on the player who took it.
///
/// Measured the same way as the shooter's fields — every field the victim's pawn wrote on a
/// `player_hurt` tick — and deliberately excluding health and armour, which are the damage itself
/// rather than the reaction to it. Suppressing these asks a narrow question: does the visible
/// flinch come from networked state, or from the animation payload that also moves on that tick?
pub const HIT_REACTION_FIELDS: [&str; 9] = [
    "m_flFlinchStack",
    "m_flTimeOfLastInjury",
    "m_nForceBone",
    "m_unpredictableBaseAngle",
    "m_unpredictableBaseTick",
    // The blood mist and the impact spark are drawn by the client, not sent by the demo: every
    // particle create can be removed and they still appear. This counter is what the client
    // watches — it ticks up once per bullet that landed — along with the bone and force describing
    // where. Health and armour are deliberately not here: the wound stays, only its decoration
    // goes, so the round still ends the way it was recorded.
    "m_totalHitsOnServer",
    "m_nRagdollDamageBone",
    "m_vRagdollDamageForce",
    "m_szRagdollDamageWeaponName",
];

/// The animation payload, withheld only on the ticks a discharge lands.
///
/// These carry the graph's own state as opaque bytes, so they cannot be edited — only withheld.
/// Withholding all of them for a single tick crashed CS2 (POSE_DECODE_PLAN §9), which pointed at
/// element zero: it is a per-tick counter, and a gap in it is evidently not something the client
/// will accept. So element zero is exempted by [`KEEPS_FIRST_ELEMENT`] and only the parameters
/// are withheld — the sequence keeps advancing while the values hold for one tick.
pub const SHOT_TICK_POSE_FIELDS: [&str; 3] = [
    "m_SerializePoseRecipeAG2Dynamic",
    "m_nSerializePoseRecipeAG2ActiveSlot",
    "m_topology",
];

/// Fields whose element zero is never withheld, because it is a sequence counter rather than data.
pub const KEEPS_FIRST_ELEMENT: [&str; 1] = ["m_SerializePoseRecipeAG2Dynamic"];

/// Which entity fields to stop transmitting, and when.
///
/// Several entities at once, because one shot is written to two of them: the shooter's pawn holds
/// the counter and the punch, and the weapon it names holds the magazine and the fire timing.
#[derive(Clone, Default)]
pub struct EntityEdit {
    /// Entity index to the fields to drop on it. A name matches either the field's full schema
    /// name or its bare name after the class prefix.
    pub targets: BTreeMap<i32, Vec<String>>,
    /// Resource IDs for player models moved between game builds.
    pub model_remap: BTreeMap<u64, u64>,
    /// Model-guarded remaps of explicitly transmitted pawn mesh-group masks.
    pub pawn_mesh_group_remaps: Vec<PawnMeshGroupRemap>,
    /// Experimental AG2 recipe sent on a pawn's create update, after its empty baseline.
    pub pose_seed: Option<RestorePoseSeed>,
    /// Diagnostic 14184 HUD pilot: copy one pawn's packet controller handle
    /// into the newly appended default-controller field on create updates.
    pub default_controller_seed: Option<DefaultControllerSeed>,
    /// Diagnostic 14184 HUD timing vector seeded at one verified weapon switch.
    pub weapon_timing_seed: Option<WeaponTimingSeed>,
    /// Diagnostic-only, exact-lifetime AK gameplay-state write.
    pub hud_weapon_state_seed: Option<HudWeaponStateSeed>,
    /// Caller-supplied scalar writes at exact ticks and entity lifetimes.
    pub scheduled_writes: Vec<ScheduledFieldWrite>,
    /// Read-only decoded snapshots of selected entity fields at exact lifetimes.
    pub audit_entity_fields: Vec<EntityFieldAuditQuery>,
    /// Explicit opt-in for placing scheduled writes inside matching create entries.
    /// Missing entities are still never created from a schedule.
    pub allow_scheduled_create_writes: bool,
    /// Apply scheduled rows only to matching existing create entries, never to
    /// same-tick deltas or synthetic update entries. Used for create-boundary repairs.
    pub create_only_scheduled_writes: bool,
    pub from_tick: i32,
    pub to_tick: i32,
    /// Fields dropped only on the exact ticks a discharge happened, keyed by entity.
    ///
    /// The animation payload is rewritten every tick, so holding it across a whole window would
    /// leave the player stuck in one pose. Skipping it for the single tick a discharge lands on
    /// costs about sixteen milliseconds of held pose and the next tick resumes normally — while
    /// the state that tick was carrying never reaches the client at all.
    pub pulse_targets: BTreeMap<i32, Vec<String>>,
    /// The ticks `pulse_targets` applies on.
    pub pulse_ticks: BTreeSet<i32>,
    /// Entities whose weapon attack overlay should be held still.
    ///
    /// The firing animation is not a clip that can be dropped: it is a permanent overlay whose
    /// playback time restarts on each trigger pull. Freezing it means writing the time back to
    /// what it already was, so the overlay simply keeps playing out instead of starting again.
    /// Nothing is withheld and no length changes anywhere, which matters — withholding payload
    /// bytes crashed the client twice.
    pub freeze_overlay: BTreeSet<i32>,
    /// Stickers to put on a weapon, per weapon entity: `(slot, sticker id)`.
    ///
    /// A weapon's econ attributes are a networked list whose length is itself a field, so slots
    /// beyond the four the game offers are a matter of writing a longer list.
    ///
    /// Slot zero begins at attribute definition **113** and each slot takes four: id, wear, scale
    /// and rotation. Definition 80 is *not* a sticker — it is the StatTrak kill counter, which is
    /// what a MAC-10 carrying 1507 of them was really saying.
    pub add_stickers: BTreeMap<i32, Vec<(u32, u32)>>,
    /// Repaint a weapon, per weapon entity: the paint kit to give it.
    ///
    /// The kit is the first econ attribute on the item, alongside its seed and wear. Unlike the
    /// item definition — which crashed the client, because the attributes beside it then described
    /// an item that cannot exist — a different kit on the same knife is an ordinary combination.
    pub swap_paint: BTreeMap<i32, f32>,
    /// Weapon entities a player should never be seen switching to.
    ///
    /// The active weapon is a handle on the pawn, and it is delta encoded like anything else: drop
    /// the write that selects a weapon and the client simply keeps holding the previous one until
    /// the next switch it is told about. So a switch is removed by omission, with no need to
    /// invent a handle for something else — which matters, because there is no value encoder here.
    ///
    /// The weapon entity itself is left in the world. Nothing draws it while it is not in anyone's
    /// hands, and removing it would mean unpicking every reference the demo makes to it.
    pub skip_weapons: BTreeSet<i32>,
    /// Turn one player's view steadily, at this many degrees per tick.
    ///
    /// Nothing validates a look direction against anything else, which is what makes this reachable
    /// where a model swap was not: angles have no consistency partner to contradict.
    pub spin: Option<(i32, f32)>,
    /// Pitch to hold the view at while it turns. Level by default.
    pub spin_pitch: Option<f32>,
    /// Hold the aim task's two values at fixed settings, as a fraction of their usable range.
    ///
    /// Sweeping the raw field across its full sixteen bits crashed the client about half a minute
    /// into playback — the load survives, the edited ticks do not. In a recording the field only
    /// ever spans roughly 676 to 58020, and those bounds are evidently not decoration: the
    /// animation system will not accept an aim it considers unreachable. So a setting is given as
    /// -1..1 and mapped inside the observed range rather than written raw.
    pub aim_hold: Option<(i32, f32, f32)>,
    /// Nudge the aim task's two values away from what was recorded, rather than replacing them.
    ///
    /// Holding the field at a constant contorts the model: in a recording it moves every tick, and
    /// a rig asked to solve toward one fixed offset for a whole round does not stay well behaved —
    /// pinned at the recorded maximum the body collapses, and past the recorded range the client
    /// crashes outright. Shifting the value it already had keeps the motion underneath intact.
    pub aim_offset: Option<(i32, f32, f32)>,
    /// Set the aim task's three eight bit weights, per entity. `None` leaves one as recorded.
    ///
    /// They are not continuous: in a recording the first sits at 255 almost always and the other
    /// two at 0, which is the shape of chain links being switched in rather than dialled. If one
    /// of the idle two is the neck's share of the turn, enabling it is what moves the head — and
    /// unlike the angles, both 0 and 255 are values the recording already visits, so it is inside
    /// the range the solver has been shown to accept.
    pub aim_weights: Option<(i32, [Option<u32>; 3])>,
    /// Which of the aim task's five routines runs, per entity.
    ///
    /// The three bit field after the weights is a mode selector, not a magnitude: the execute
    /// function dispatches on it to one of five different solves. A recording sits on mode 2
    /// almost always, so the others are whole behaviours this player never exercised — which makes
    /// this the most direct control over *how* the aim is applied rather than how far.
    pub aim_mode: Option<(i32, u32)>,
    /// Sweep one of the aim task's two values across its range, to learn what it steers.
    ///
    /// Both are angle-shaped and both move in a recording, but correlating them against the eye
    /// angles gave weak coefficients, so they are an offset relative to something rather than an
    /// absolute direction. Ramping one while holding the other neutral shows which axis it drives
    /// and how far, which is cheaper than guessing at a mapping.
    pub aim_sweep: Option<(i32, u8)>,
    /// Clips to substitute in the pose recipe, per entity: the clip played becomes the clip
    /// mapped to it.
    ///
    /// This is how a weapon swap gets the hands right. The pose is recorded, so a player handed a
    /// different weapon keeps the old grip — unless the sampler clips themselves are changed. It
    /// only works where both weapons build the same task list: a clip index can be rewritten in
    /// place, but there is no way to add a sampler that is not there, which is why a knife cannot
    /// be turned into a rifle this way.
    pub swap_clips: BTreeMap<i32, BTreeMap<u32, u32>>,
    /// Per entity: a clip to append to the recipe, the blend weight it comes in at, and the bone
    /// mask to confine it to, if any. Unlike every other pose edit this adds tasks rather than
    /// changing values, so it is the one that needs the topology rewritten.
    pub append_clip: BTreeMap<i32, (u32, u32, Option<u32>)>,
    /// The attack overlay clip to hold still, per entity.
    ///
    /// Rewriting only the tick an overlay restarts does not stop it: the next update carries the
    /// animation playing on from where it restarted, so the restart is merely moved. The clip is
    /// therefore pinned to its end for as long as the player is firing.
    pub overlay_clip: BTreeMap<i32, BTreeSet<u32>>,
}

fn decode_stored_variant(bits: &Bits, decoder: &Decoder, qf: &QfMapper) -> Result<Variant> {
    let mut reader = Bitreader::new(&bits.bytes);
    let value = reader
        .decode(decoder, qf)
        .context("decode stored entity field")?;
    ensure!(
        bits.bytes.len() * 8 - reader.bits_remaining().context("stored field bit length")?
            == bits.len,
        "stored entity field did not consume its complete bit span"
    );
    Ok(value)
}

fn decode_stored_u64(bits: &Bits, decoder: &Decoder, qf: &QfMapper) -> Result<u64> {
    let value = decode_stored_variant(bits, decoder, qf)?;
    match value {
        Variant::U64(value) => Ok(value),
        _ => anyhow::bail!("stored model field is not an unsigned 64-bit resource handle"),
    }
}

fn model_handle_matches_guard(model_handle: Option<u64>, expected: u64) -> bool {
    model_handle == Some(expected)
}

fn stored_entity_model_handle(
    entity: &Entity,
    path: &[i32],
    serializer: &Serializer,
    qf: &QfMapper,
) -> Result<Option<u64>> {
    let Some(bits) = entity.values.get(path) else {
        return Ok(None);
    };
    let mut fp = generate_fp();
    fp.last = path
        .len()
        .checked_sub(1)
        .context("empty model guard path")?;
    fp.path[..path.len()].copy_from_slice(path);
    let field = find_field(&fp, serializer).context("resolve stored model guard field")?;
    ensure!(
        field_name(field).rsplit('.').next() == Some("m_hModel"),
        "stored model guard path does not name m_hModel"
    );
    decode_stored_u64(bits, &get_decoder_from_field(field)?, qf).map(Some)
}

/// A narrowly scoped mesh-group correction. The model guard is the decoded
/// unsigned resource handle in the specified field path; both it and the
/// current mask must match before the new mask is written.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct PawnMeshGroupRemap {
    pub entity_id: i32,
    #[serde(default)]
    pub serial: Option<u32>,
    pub class_name: String,
    pub model_field_path: Vec<i32>,
    pub model_handle: u64,
    pub mask_path: Vec<i32>,
    pub old_mask: u64,
    pub new_mask: u64,
    /// Optional semantic selection bits to carry from `old_mask` into `new_mask`.
    /// Only set bits whose meaning is confirmed for both models.
    #[serde(default)]
    pub preserve_mask_bits: u64,
}

#[derive(Clone)]
pub struct DefaultControllerSeed {
    pub entity_id: i32,
    pub class_id: u32,
    pub source_path: i32,
    pub target_path: i32,
    pub expected_handle: u32,
}

#[derive(Clone)]
pub struct WeaponTimingSeed {
    pub entity_id: i32,
    pub class_id: u32,
    pub service_path: i32,
    pub active_field: i32,
    pub timing_field: i32,
    pub switch_tick: i32,
    pub end_tick: i32,
    pub expected_handle: u32,
}

#[derive(Clone)]
pub struct HudWeaponStateSeed {
    pub entity_id: i32,
    pub class_id: u32,
    pub serial: u32,
    pub tick: i32,
    pub field_path: i32,
    pub value: u32,
}

/// One explicitly scheduled scalar write, guarded by the entity's class and serial lifetime.
#[derive(Clone, Debug)]
pub enum ScheduledScalar {
    U32(u32),
    U64(u64),
    I32(i32),
    F32(f32),
    /// Exact wire ticks, with no seconds conversion; simulation-time decoder only.
    TimeTicks(u32),
    String(String),
}

fn encode_scheduled_time_ticks(value: u32, decoder: &Decoder) -> Result<Bits> {
    ensure!(
        matches!(decoder, Decoder::FloatSimulationTimeDecoder),
        "scheduled TimeTicks does not match schema decoder {decoder:?}"
    );
    Ok(encode_varint(value))
}

fn encode_scheduled_u64(value: u64, decoder: &Decoder) -> Result<Bits> {
    ensure!(
        matches!(decoder, Decoder::Unsigned64Decoder),
        "scheduled U64 does not match schema decoder {decoder:?}"
    );
    Ok(encode_varint_u64(value))
}

// StringDecoder reads eight-bit UTF-8 bytes to the first NUL. The normal entity
// serializer writes these bits at their actual stream offset without alignment.
fn encode_scheduled_string(value: &str) -> Result<Bits> {
    ensure!(
        !value.as_bytes().contains(&0),
        "scheduled String contains an embedded NUL"
    );
    ensure!(
        value.len() <= 1024,
        "scheduled String exceeds 1024 UTF-8 bytes"
    );
    let mut bytes = value.as_bytes().to_vec();
    bytes.push(0);
    let len = bytes.len() * 8;
    Ok(Bits { bytes, len })
}

#[derive(Clone, Debug)]
pub struct ScheduledFieldWrite {
    pub tick: i32,
    pub entity_id: i32,
    pub class_id: u32,
    pub serial: u32,
    pub field_path: Vec<i32>,
    pub value: ScheduledScalar,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct EntityFieldAuditQuery {
    pub tick: i32,
    pub entity_id: i32,
    pub class_id: u32,
    pub serial: u32,
    pub field_paths: Vec<Vec<i32>>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct AuditedEntityField {
    pub tick: i32,
    pub entity_id: i32,
    pub class_id: u32,
    pub serial: u32,
    pub field_path: Vec<i32>,
    pub field_name: String,
    pub value: String,
}

/// Select resident audit targets from the state after a complete entity message.
/// Checking class and serial here prevents a deleted pawn/weapon or a reused
/// entity index from producing a stale snapshot.
fn matching_entity_field_audits<'a>(
    queries: &'a [EntityFieldAuditQuery],
    entities: &'a BTreeMap<i32, Entity>,
    tick: i32,
    last_entity_message: bool,
) -> Vec<(&'a EntityFieldAuditQuery, &'a Entity)> {
    if !last_entity_message {
        return Vec::new();
    }
    queries
        .iter()
        .filter_map(|query| {
            if query.tick != tick {
                return None;
            }
            let entity = entities.get(&query.entity_id)?;
            (entity.class == query.class_id && entity.serial == query.serial)
                .then_some((query, entity))
        })
        .collect()
}

fn scheduled_create_copy_matches(
    scheduled: &ScheduledFieldWrite,
    tick: i32,
    entity: i32,
    class_id: u32,
    serial: u32,
) -> bool {
    scheduled.tick == tick
        && scheduled.entity_id == entity
        && scheduled.class_id == class_id
        && scheduled.serial == serial
}

fn scheduled_write_targets_entry(
    allow_create_writes: bool,
    create_only: bool,
    command: u32,
) -> bool {
    if create_only {
        command == 2
    } else {
        command == 0 || (allow_create_writes && command == 2)
    }
}

/// 24-byte AK vector observed on the current 14184 Phoenix control's simple
/// deploy segment. Only the first 32 ticks are calibrated; later state changes
/// have different clocks and must not be inferred from this function.
fn simple_ak_timing_vector(elapsed: u32) -> Result<[u8; 24]> {
    ensure!(
        elapsed < 32,
        "AK timing pilot exceeds calibrated 32-tick segment"
    );
    let now = ((elapsed + 1) * 1024) as u16;
    let prior = (elapsed * 1024) as u16;
    let mut data = [0u8; 24];
    data[7..11].copy_from_slice(&[1, 0xc7, 0, 0]);
    data[16..24].copy_from_slice(&[2, 0x17, 0, 0x80, 0x19, 0, 0, 0]);
    for offset in [1, 4, 14] {
        data[offset..offset + 2].copy_from_slice(&now.to_le_bytes());
    }
    data[11..13].copy_from_slice(&prior.to_le_bytes());
    Ok(data)
}

#[derive(Clone)]
pub struct RestorePoseSeed {
    pub entity_id: i32,
    pub class_id: u32,
    /// Optional exact entity lifetime guard for bounded donor replacement.
    pub serial: Option<u32>,
    pub body_path: i32,
    pub slots_field: i32,
    pub active_slot_field: i32,
    pub dynamic_field: i32,
    pub version_field: i32,
    pub context_field: i32,
    pub topology: Vec<u8>,
    pub dynamic: Vec<u8>,
    pub emit_shape: bool,
    pub emit_topology: bool,
    pub emit_version: bool,
    pub emit_context: bool,
    /// Preserve an existing current-build entity's creation recipe in values-only trials.
    pub seed_on_create: bool,
    pub recipe_version: u32,
    pub context_iteration: u32,
    /// For current AG2 payloads, write the target demo's server tick into bytes 0..4.
    pub dynamic_tick_offset: Option<i32>,
    /// Refresh the dynamic recipe on delta updates as well as entity creation.
    pub refresh_dynamic_on_update: bool,
    /// Optional inclusive demo-tick window for refreshes; create seeds are unaffected.
    pub refresh_window: Option<(i32, i32)>,
    /// For sparse donor schedules, refresh only ticks with an explicit payload.
    pub donor_ticks_only: bool,
    /// Experimental sampler-time advance during a bounded delta refresh.
    /// (time bit offset, base demo tick, units per tick)
    pub sampler_advance: Option<(u32, i32, u32)>,
    /// Per-tick AG2 payloads sampled from a same-build donor recipe stream.
    pub donor_dynamic_by_tick: BTreeMap<i32, Vec<u8>>,
    /// Fixed slot pool and active-slot selection from a current-build donor.
    pub donor_topologies: Vec<Vec<u8>>,
    pub donor_slot_by_tick: BTreeMap<i32, u32>,
    /// Prefix of an already-authored slot pool. Verify it on every create and
    /// append only the later slots, preserving all existing slot indices.
    pub preserved_slot_topologies: Vec<Vec<u8>>,
    /// Slot-stream mode: the donor's full slot table at each tick where it changes. When
    /// non-empty, the seed acts only on scheduled donor ticks, writes only changed slots on
    /// updates and the whole table on creates, reproducing a same-build slot pool that the
    /// server reassigns over time.
    pub donor_slot_tables: BTreeMap<i32, Vec<Vec<u8>>>,
    pub suppress_legacy_predicted: bool,
}

impl EntityEdit {
    pub fn for_model_remap(model_remap: BTreeMap<u64, u64>) -> Self {
        Self {
            model_remap,
            from_tick: i32::MIN,
            to_tick: i32::MAX,
            ..Self::default()
        }
    }
}

impl EntityEdit {
    fn drops(&self, entity: i32, field: &Field, tick: i32, key: &[i32]) -> bool {
        let full = field_name(field);
        let bare = full.rsplit('.').next().unwrap_or(full.as_str());
        let matches = |fields: &Vec<String>| {
            fields
                .iter()
                .any(|wanted| wanted == &full || wanted == bare)
        };
        if self.targets.get(&entity).is_some_and(matches) {
            return true;
        }
        if !self.pulse_ticks.contains(&tick)
            || !self.pulse_targets.get(&entity).is_some_and(matches)
        {
            return false;
        }

        // Element zero of the animation payload is a sequence counter, not a parameter. Holding
        // it back is what the client refused; holding back the rest leaves the sequence intact.
        !(KEEPS_FIRST_ELEMENT.contains(&bare) && key.last() == Some(&0))
    }
}

#[derive(Debug, Default)]
pub struct EntityEditStats {
    pub fields_dropped: usize,
    pub packets_rewritten: usize,
    pub models_rewritten: usize,
    /// Explicit pawn mesh masks corrected, keyed by pawn entity index.
    pub pawn_mesh_group_masks_rewritten: BTreeMap<i32, usize>,
    /// Explicit guarded mask fields seen, including masks already at target value.
    pub pawn_mesh_group_masks_matched: BTreeMap<i32, usize>,
    /// Nonzero model resources written by entity packets, including creates.
    pub packet_model_ids: BTreeMap<u64, usize>,
    pub audited_entity_fields: Vec<AuditedEntityField>,
    pub pose_seeds: usize,
    /// Distinct donor ticks actually applied to a matching pawn lifetime.
    pub donor_pose_ticks: BTreeSet<i32>,
    pub default_controller_seeds: usize,
    pub weapon_timing_ticks: usize,
    pub weapon_timing_fields: usize,
    pub hud_weapon_state_writes: usize,
    pub scheduled_writes: usize,
    pub checkpoints_crossed: usize,
    /// Packets whose re-encoded entity stream did not read back as the editor meant it.
    ///
    /// Every packet is checked, edited or not, because an encoder that is wrong on an untouched
    /// packet is wrong on an edited one too. A non-zero count means the output cannot be trusted.
    pub identity_failures: usize,
    pub identity_checks: usize,
    /// Attack overlay restarts held still.
    pub overlays_frozen: usize,
    /// Restarts that could not be held because a byte value had never been seen encoded.
    pub overlays_missed: usize,
    /// Aim values written.
    pub aim_written: usize,
    /// Paint kits rewritten, and stickers appended.
    pub paints_swapped: usize,
    pub stickers_added: usize,
    /// Payload byte values whose observed encoding is a plain varint, and those where it is not.
    ///
    /// Appending a task needs byte values the demo may never have carried, so the encoding has to
    /// be synthesised rather than copied from a sample. This counts the agreement between what the
    /// demo actually wrote and what `encode_varint` would write, which is the evidence for whether
    /// synthesising is safe.
    pub byte_encoding_varint: usize,
    pub byte_encoding_other: usize,
    pub append_written: usize,
    pub append_no_room: usize,
    /// Ticks skipped because a slot other than the pinned one was active.
    pub append_wrong_slot: usize,
    /// Slots handed back their original topology as the append moved off them.
    pub append_restored: usize,
    /// Field writes actually pushed into the update, as opposed to appends merely attempted.
    pub append_paths_emitted: usize,
    /// The furthest any recipe's payload reached, in bits — the measured occupancy of the buffer.
    pub append_worst_end: u32,
    /// View directions rewritten, and how many the editor was offered at all. A gap between the
    /// two means the field is reaching the output by a path this edit never sees.
    pub angles_rewritten: usize,
    pub angles_seen: usize,
    /// View directions added on ticks the recording had none.
    pub angles_injected: usize,
    /// Sampler clips substituted, and those that could not be reached.
    pub clips_swapped: usize,
    pub clips_missed: usize,
}

/// Removes entity field updates from a demo as it is walked forward, frame by frame.
///
/// Suppressing a field means omitting it from the update rather than writing a new value: entity
/// data is a delta against the previous tick, so a field the server stops mentioning keeps
/// whatever it last held on the client. That is why no value encoder is needed — every surviving
/// value is copied verbatim as bits, and only the field-path list is rebuilt around the gap.
///
/// Entity state is cumulative, so frames must be handed over in order from the start of the demo.
pub struct EntityEditor<'a> {
    state: State<'a>,
    edit: EntityEdit,
    /// Last explicitly written model handle for each entity/model field.
    model_handles: BTreeMap<(i32, Vec<i32>), u64>,
    /// Slot-stream mode: the slot table as last written for the seeded pawn.
    pose_slots_written: Option<Vec<Vec<u8>>>,
    scheduled_applied: BTreeSet<usize>,
    clips: BTreeMap<i32, ClipCredit>,
    /// The pose recipe as it stands for each entity whose overlay is being frozen.
    poses: BTreeMap<i32, crate::poserecipe::PoseTracker>,
    /// The topology as the *game* last sent it, per entity and slot.
    ///
    /// The tracker holds what the client will believe, which after an append is our modified
    /// version. Appending to that again each tick stacks two more tasks every tick until the count
    /// overflows its width. The append has to be computed from the recording's own recipe, so the
    /// original is kept separately and refreshed whenever the game sends a new one.
    append_base: BTreeMap<(i32, u32), Vec<u8>>,
    /// Every slot an entity's append has modified, so they can be handed back.
    ///
    /// A modified topology stays in the client until something replaces it. Without this the effect
    /// does not stop at the end of its window — measured at 17 units of pose difference thirty
    /// ticks past the last appended tick — and modified recipes pile up across slots, which is the
    /// state a long range eventually crashes in.
    append_touched: BTreeMap<i32, BTreeSet<u32>>,
    /// The one slot an entity's append is made against.
    ///
    /// A recipe is several topology slots sharing a single payload buffer, and the payload is only
    /// meaningful under whichever slot is active. Appending to whatever happens to be active leaves
    /// a modified topology behind in every slot it touched, and the first rotation back to one of
    /// them reads our two extra tasks against a payload that no longer describes them. Pinning to
    /// one slot makes the effect intermittent — it applies only while that slot is active — and
    /// makes it correct.
    append_slot: BTreeMap<i32, u32>,
    /// The field path the body's own rotation sits at.
    ///
    /// Third person shows the model, and the model is turned by this rather than by the view. In a
    /// recording its yaw is simply the eye yaw with the pitch flattened, which is why the body
    /// normally follows the head — and why spinning only the view leaves the body facing the way it
    /// really did.
    spin_body_path: Option<Vec<i32>>,
    /// The field path a view angle sits at, learned from the first one seen.
    ///
    /// Needed to *add* an angle on a tick that never had one. The demo only carries an update when
    /// the player actually turned, so a spin built by replacing existing writes stalls wherever
    /// they held still — up to two hundred ticks in one stretch here. Injecting fills those gaps.
    spin_path: Option<Vec<i32>>,
    /// Whether the packet being walked had a value replaced rather than removed.
    ///
    /// A packet used to be re-emitted only when it *lost* a field, so every edit that substitutes
    /// — a spun view angle, a swapped pose clip, a pinned overlay — was silently discarded unless
    /// that same packet happened to drop something too. The edits landed intermittently and looked
    /// like they were failing at random.
    substituted: bool,
    /// Bit patterns observed for each payload byte value.
    ///
    /// Writing a byte back needs its encoding, and there is no value encoder here. There does not
    /// need to be: every payload byte is the same field type, so a pattern seen for a value once
    /// is the pattern for that value anywhere. A value never yet observed is left alone rather
    /// than guessed at.
    byte_bits: BTreeMap<u8, Bits>,
    weapon_timing_previous: Option<Vec<u8>>,
    weapon_timing_switch_seen: bool,
    pub stats: EntityEditStats,
}

/// One pose-relevant field seen while walking an entity update.
enum PoseWrite {
    Topology { slot: u32, blob: Vec<u8> },
    Byte { index: usize, value: u8 },
    Active { slot: u32 },
}

impl<'a> EntityEditor<'a> {
    pub fn new(builder: &'a BoundaryBuilder, edit: EntityEdit) -> Self {
        Self {
            state: State {
                classes: &builder.classes,
                qf: &builder.qf,
                huf: &builder.huf,
                entities: BTreeMap::new(),
                baselines: BTreeMap::new(),
                baseline_entries: Vec::new(),
                alternate_baselines: BTreeMap::new(),
                non_transmitted: BTreeSet::new(),
                template: None,
            },
            edit,
            model_handles: BTreeMap::new(),
            scheduled_applied: BTreeSet::new(),
            clips: BTreeMap::new(),
            poses: BTreeMap::new(),
            append_base: BTreeMap::new(),
            append_slot: BTreeMap::new(),
            append_touched: BTreeMap::new(),
            substituted: false,
            spin_path: None,
            spin_body_path: None,
            byte_bits: BTreeMap::new(),
            weapon_timing_previous: None,
            weapon_timing_switch_seen: false,
            stats: EntityEditStats::default(),
            pose_slots_written: None,
        }
    }

    /// Counts the magazine could not be rewritten to, because that value had never been seen.
    pub fn clip_misses(&self) -> usize {
        self.clips.values().map(|clip| clip.misses).sum()
    }

    /// Feed one entity message. Returns a replacement payload when this packet lost a field, or
    /// `None` when it is unchanged and the original bytes should be kept.
    pub fn packet_entities(&mut self, message: &NetMessage, tick: i32) -> Result<Option<Vec<u8>>> {
        self.packet_entities_with_packet_end(message, tick, true)
    }

    /// Feed an entity message while allowing scheduled writes to wait for a later entity message
    /// in the same outer packet before synthesizing an otherwise absent delta entry.
    pub fn packet_entities_with_packet_end(
        &mut self,
        message: &NetMessage,
        tick: i32,
        last_entity_message: bool,
    ) -> Result<Option<Vec<u8>>> {
        let mut msg = CsvcMsgPacketEntities::decode(message.payload.as_slice())?;
        self.substituted = false;
        let dropped = self.rewrite(&mut msg, tick, last_entity_message)?;
        if dropped == 0 && !self.substituted {
            return Ok(None);
        }
        self.stats.fields_dropped += dropped;
        self.stats.packets_rewritten += 1;
        let mut buffer = Vec::with_capacity(msg.encoded_len());
        msg.encode(&mut buffer)?;
        Ok(Some(buffer))
    }

    pub fn string_tables(&mut self, tables: &CDemoStringTables) {
        self.state.tables(tables);
    }

    /// A checkpoint inside the edited range restores absolute state, so the caller is told when
    /// one is crossed rather than left to assume the edit held for the whole range.
    pub fn targets(&self) -> usize {
        self.edit.targets.len()
    }

    pub fn note_checkpoint(&mut self, tick: i32) {
        if tick >= self.edit.from_tick && tick <= self.edit.to_tick {
            self.stats.checkpoints_crossed += 1;
        }
    }

    fn scheduled_field_bits(&self, write: &ScheduledFieldWrite) -> Result<Bits> {
        ensure!(
            !write.field_path.is_empty() && write.field_path.len() <= 6,
            "scheduled field path must contain 1 to 6 indices"
        );
        ensure!(
            write.field_path.iter().all(|index| *index >= 0),
            "scheduled field path contains a negative index"
        );
        let class = self
            .state
            .classes
            .get(write.class_id as usize)
            .context("scheduled write class id is outside the schema")?;
        let mut fp = generate_fp();
        for (slot, index) in write.field_path.iter().enumerate() {
            fp.path[slot] = *index;
        }
        fp.last = write.field_path.len() - 1;
        let field = find_field(&fp, &class.serializer)?;
        // A vector's own path carries its length as a plain varint (as the pose-slot writer
        // encodes it); its elements are ordinary value fields one level deeper.
        if matches!(field, Field::Vector(_)) {
            return match write.value.clone() {
                ScheduledScalar::U32(length) if length <= 1024 => Ok(encode_varint(length)),
                _ => anyhow::bail!("scheduled vector length must be a U32 of at most 1024"),
            };
        }
        ensure!(
            matches!(field, Field::Value(_)),
            "scheduled writes require a scalar value field"
        );
        let decoder = get_decoder_from_field(field)?;
        match (write.value.clone(), decoder) {
            (ScheduledScalar::TimeTicks(value), decoder) => {
                encode_scheduled_time_ticks(value, &decoder)
            }
            (ScheduledScalar::U64(value), decoder) => encode_scheduled_u64(value, &decoder),
            (
                ScheduledScalar::U32(value),
                Decoder::UnsignedDecoder | Decoder::BaseDecoder | Decoder::CentityHandleDecoder,
            ) => Ok(encode_varint(value)),
            // zigzag, as read_varint32 decodes it
            (ScheduledScalar::I32(value), Decoder::SignedDecoder) => {
                Ok(encode_varint(((value << 1) ^ (value >> 31)) as u32))
            }
            (ScheduledScalar::F32(value), Decoder::NoscaleDecoder) => {
                ensure!(value.is_finite(), "scheduled float must be finite");
                Ok(encode_f32_noscale(value))
            }
            (ScheduledScalar::F32(value), Decoder::FloatSimulationTimeDecoder) => {
                ensure!(
                    value.is_finite() && value >= 0.0,
                    "scheduled GameTime float must be finite and nonnegative"
                );
                let ticks = (value * 30.0).round();
                ensure!(
                    (ticks / 30.0 - value).abs() <= 0.00001,
                    "scheduled GameTime value {value} is not representable at 30 Hz"
                );
                ensure!(
                    ticks <= u32::MAX as f32,
                    "scheduled GameTime value overflows wire varint"
                );
                Ok(encode_varint(ticks as u32))
            }
            (ScheduledScalar::String(value), Decoder::StringDecoder) => {
                encode_scheduled_string(&value)
            }
            (ScheduledScalar::String(_), decoder) => {
                anyhow::bail!("scheduled String does not match schema decoder {decoder:?}")
            }
            (ScheduledScalar::I32(_), decoder) => {
                anyhow::bail!("scheduled I32 does not match schema decoder {decoder:?}")
            }
            (ScheduledScalar::U32(_), decoder) => {
                anyhow::bail!("scheduled U32 does not match schema decoder {decoder:?}")
            }
            (ScheduledScalar::F32(_), decoder) => {
                anyhow::bail!("scheduled F32 does not match schema decoder {decoder:?}")
            }
        }
    }

    fn scheduled_vector_length(&self, write: &ScheduledFieldWrite) -> Result<Option<usize>> {
        let class = self
            .state
            .classes
            .get(write.class_id as usize)
            .context("scheduled write class id is outside the schema")?;
        let mut fp = generate_fp();
        for (slot, index) in write.field_path.iter().enumerate() {
            fp.path[slot] = *index;
        }
        fp.last = write.field_path.len() - 1;
        if matches!(find_field(&fp, &class.serializer)?, Field::Vector(_)) {
            return match write.value.clone() {
                ScheduledScalar::U32(length) if length <= 1024 => Ok(Some(length as usize)),
                _ => anyhow::bail!("scheduled vector length must be a U32 of at most 1024"),
            };
        }
        Ok(None)
    }

    /// Walk one entity packet, dropping the edited fields and re-encoding what remains.
    /// Hold a weapon's attack overlay still for one entity update.
    ///
    /// The overlay is found rather than named, because its clip differs per weapon: it is the
    /// sampler whose normalised time runs *backwards*, which only happens when the animation is
    /// retriggered. Holding it means writing that time back to what it was a tick ago, so the
    /// overlay carries on playing out instead of starting over.
    ///
    /// Only bytes this update already carried are rewritten. A byte the update did not mention
    /// cannot be reached without adding a field to the delta, and one it did mention is exactly
    /// the one that moved.
    /// Returns the entries of `kept` to replace, so the caller can apply them to the update and
    /// to the accumulated state together. The entity itself is not borrowed here: it is already
    /// held mutably by the walk this is called from.
    fn freeze_overlay(
        &mut self,
        id: i32,
        tick: i32,
        pose: &[(usize, PoseWrite)],
    ) -> Result<Vec<(usize, Bits)>> {
        let tracker = self.poses.entry(id).or_default();
        let before = tracker.payload().to_vec();
        for (_, write) in pose {
            match write {
                PoseWrite::Topology { slot, blob } => tracker.set_topology(*slot, blob.clone()),
                PoseWrite::Byte { index, value } => tracker.set_byte(*index, *value),
                PoseWrite::Active { slot } => tracker.set_active(*slot),
            }
        }
        let swaps = self.edit.swap_clips.get(&id).cloned().unwrap_or_default();
        // Only this player's own attack overlay is touched. Every looping animation restarts, and
        // holding them all would lock the player's legs as surely as it stops the gun.
        let sweeping = matches!(self.edit.aim_sweep, Some((swept, _)) if swept == id)
            || matches!(self.edit.aim_hold, Some((held, _, _)) if held == id)
            || matches!(self.edit.aim_offset, Some((nudged, _, _)) if nudged == id)
            || matches!(self.edit.aim_weights, Some((weighted, _)) if weighted == id)
            || matches!(self.edit.aim_mode, Some((moded, _)) if moded == id);
        let overlay = match self.edit.overlay_clip.get(&id) {
            Some(overlay) => overlay.clone(),
            None if swaps.is_empty() && !sweeping => return Ok(Vec::new()),
            None => Default::default(),
        };
        let firing = self
            .edit
            .pulse_ticks
            .range(tick - OVERLAY_SHOT_SLACK..=tick + OVERLAY_HOLD_AFTER)
            .next()
            .is_some();
        let found = tracker.samplers().unwrap_or_default();
        tracker.step();
        let in_range = tick >= self.edit.from_tick && tick <= self.edit.to_tick;
        // Pin the overlay to the end of its clip on every firing tick, not merely where it
        // restarts. An animation held at its last frame has nothing left to show.
        // The overlay is pinned only while firing; a clip substitution applies whenever the clip
        // is on screen, which is most of the time and nowhere near a shot.
        let restarts: Vec<crate::poserecipe::Restart> = if firing && in_range {
            found
                .iter()
                .filter(|s| overlay.contains(&s.clip) && s.time != OVERLAY_HELD_TIME)
                .map(|s| crate::poserecipe::Restart {
                    clip: s.clip,
                    was: Some(OVERLAY_HELD_TIME),
                    now: s.time,
                    time_at: s.time_at,
                })
                .collect()
        } else {
            Vec::new()
        };
        // The aim sweep applies on every tick, not only where an overlay restarts — it is a
        // continuous steer rather than a reaction to a shot.
        if restarts.is_empty() && (swaps.is_empty() || !in_range) && !(sweeping && in_range) {
            return Ok(Vec::new());
        }
        let mut patches: Vec<(usize, Bits)> = Vec::new();

        // Where each payload byte index was written in this update, so a rewrite lands on the
        // right entry rather than appending a second write for the same path.
        let written: BTreeMap<usize, usize> = pose
            .iter()
            .filter_map(|(at, write)| match write {
                PoseWrite::Byte { index, .. } => Some((*index, *at)),
                _ => None,
            })
            .collect();

        let mut payload = tracker.payload().to_vec();

        // The aim sweep, written before anything else so the offsets below are still the ones the
        // walk computed.
        if let Some((moded, mode)) = self.edit.aim_mode {
            if moded == id {
                if let Some(sequence) = tracker.sequence() {
                    if let Some((first, _)) = crate::poserecipe::aim_fields(&sequence, &payload) {
                        // Two sixteen bit angles, then four bytes, then the three bit mode.
                        let at = first + 64;
                        let mut patched = payload.clone();
                        if crate::poserecipe::write_bits(&mut patched, at, 3, mode).is_ok() {
                            let touched = (at / 8) as usize..((at + 2) / 8) as usize + 1;
                            let reachable = touched.clone().all(|index| {
                                patched.get(index) == payload.get(index)
                                    || (written.contains_key(&index)
                                        && patched
                                            .get(index)
                                            .is_some_and(|v| self.byte_bits.contains_key(v)))
                            });
                            if reachable {
                                for index in touched {
                                    if patched.get(index) == payload.get(index) {
                                        continue;
                                    }
                                    let (Some(&at), Some(&value)) =
                                        (written.get(&index), patched.get(index))
                                    else {
                                        continue;
                                    };
                                    if let Some(bits) = self.byte_bits.get(&value) {
                                        patches.push((at, bits.clone()));
                                    }
                                }
                                payload = patched;
                                self.stats.aim_written += 1;
                            }
                        }
                    }
                }
            }
        }

        if let Some((weighted, wanted)) = self.edit.aim_weights {
            if weighted == id {
                if let Some(sequence) = tracker.sequence() {
                    if let Some((first, _)) = crate::poserecipe::aim_fields(&sequence, &payload) {
                        // The weights follow the two sixteen bit angles.
                        let base = first + 32;
                        let mut patched = payload.clone();
                        let mut ok = true;
                        for (slot, value) in wanted.iter().enumerate() {
                            if let Some(value) = value {
                                let at = base + (slot as u32 * 8);
                                if crate::poserecipe::write_u8(&mut patched, at, *value).is_err() {
                                    ok = false;
                                }
                            }
                        }
                        if ok {
                            let touched = (base / 8) as usize..((base + 23) / 8) as usize + 1;
                            let reachable = touched.clone().all(|index| {
                                patched.get(index) == payload.get(index)
                                    || (written.contains_key(&index)
                                        && patched
                                            .get(index)
                                            .is_some_and(|v| self.byte_bits.contains_key(v)))
                            });
                            if reachable {
                                for index in touched {
                                    if patched.get(index) == payload.get(index) {
                                        continue;
                                    }
                                    let (Some(&at), Some(&value)) =
                                        (written.get(&index), patched.get(index))
                                    else {
                                        continue;
                                    };
                                    if let Some(bits) = self.byte_bits.get(&value) {
                                        patches.push((at, bits.clone()));
                                    }
                                }
                                payload = patched;
                                self.stats.aim_written += 1;
                            }
                        }
                    }
                }
            }
        }

        if let Some((nudged, first_delta, second_delta)) = self.edit.aim_offset {
            if nudged == id {
                if let Some(sequence) = tracker.sequence() {
                    if let Some((first, second)) =
                        crate::poserecipe::aim_fields(&sequence, &payload)
                    {
                        let shift = |at: u32, delta: f32| -> Option<u32> {
                            let now = crate::poserecipe::read_u16(&payload, at)?;
                            let span = if delta >= 0.0 {
                                (AIM_OBSERVED_MAX - AIM_NEUTRAL) as f32
                            } else {
                                (AIM_NEUTRAL - AIM_OBSERVED_MIN) as f32
                            };
                            let moved = now as f32 + delta * span;
                            Some(
                                moved.clamp(AIM_OBSERVED_MIN as f32, AIM_OBSERVED_MAX as f32)
                                    as u32,
                            )
                        };
                        let mut patched = payload.clone();
                        let ok = shift(first, first_delta)
                            .zip(shift(second, second_delta))
                            .is_some_and(|(a, b)| {
                                crate::poserecipe::write_u16(&mut patched, first, a).is_ok()
                                    && crate::poserecipe::write_u16(&mut patched, second, b).is_ok()
                            });
                        if ok {
                            let touched = (first.min(second) / 8) as usize
                                ..((first.max(second) + 15) / 8) as usize + 1;
                            let reachable = touched.clone().all(|index| {
                                patched.get(index) == payload.get(index)
                                    || (written.contains_key(&index)
                                        && patched
                                            .get(index)
                                            .is_some_and(|v| self.byte_bits.contains_key(v)))
                            });
                            if reachable {
                                for index in touched {
                                    if patched.get(index) == payload.get(index) {
                                        continue;
                                    }
                                    let (Some(&at), Some(&value)) =
                                        (written.get(&index), patched.get(index))
                                    else {
                                        continue;
                                    };
                                    if let Some(bits) = self.byte_bits.get(&value) {
                                        patches.push((at, bits.clone()));
                                    }
                                }
                                payload = patched;
                                self.stats.aim_written += 1;
                            }
                        }
                    }
                }
            }
        }

        if let Some((held, first_setting, second_setting)) = self.edit.aim_hold {
            if held == id {
                if let Some(sequence) = tracker.sequence() {
                    if let Some((first, second)) =
                        crate::poserecipe::aim_fields(&sequence, &payload)
                    {
                        let map = |setting: f32| {
                            let clamped = setting.clamp(-1.0, 1.0);
                            let span = if clamped >= 0.0 {
                                (AIM_OBSERVED_MAX - AIM_NEUTRAL) as f32
                            } else {
                                (AIM_NEUTRAL - AIM_OBSERVED_MIN) as f32
                            };
                            (AIM_NEUTRAL as f32 + clamped * span).round() as u32
                        };
                        let mut patched = payload.clone();
                        let ok =
                            crate::poserecipe::write_u16(&mut patched, first, map(first_setting))
                                .is_ok()
                                && crate::poserecipe::write_u16(
                                    &mut patched,
                                    second,
                                    map(second_setting),
                                )
                                .is_ok();
                        if ok {
                            let touched = (first.min(second) / 8) as usize
                                ..((first.max(second) + 15) / 8) as usize + 1;
                            let reachable = touched.clone().all(|index| {
                                patched.get(index) == payload.get(index)
                                    || (written.contains_key(&index)
                                        && patched
                                            .get(index)
                                            .is_some_and(|v| self.byte_bits.contains_key(v)))
                            });
                            if reachable {
                                for index in touched {
                                    if patched.get(index) == payload.get(index) {
                                        continue;
                                    }
                                    let (Some(&at), Some(&value)) =
                                        (written.get(&index), patched.get(index))
                                    else {
                                        continue;
                                    };
                                    if let Some(bits) = self.byte_bits.get(&value) {
                                        patches.push((at, bits.clone()));
                                    }
                                }
                                payload = patched;
                                self.stats.aim_written += 1;
                            }
                        }
                    }
                }
            }
        }

        if let Some((swept, which)) = self.edit.aim_sweep {
            if swept == id {
                if let Some(sequence) = tracker.sequence() {
                    if let Some((first, second)) =
                        crate::poserecipe::aim_fields(&sequence, &payload)
                    {
                        // One full ramp every four seconds, so a single viewing shows the whole
                        // range without waiting.
                        let phase = (tick.rem_euclid(256) as f32) / 256.0;
                        let value = (phase * 65535.0) as u32;
                        let (at, held) = if which == 0 {
                            (first, second)
                        } else {
                            (second, first)
                        };
                        let mut patched = payload.clone();
                        let ok = crate::poserecipe::write_u16(&mut patched, at, value).is_ok()
                            && crate::poserecipe::write_u16(&mut patched, held, 32768).is_ok();
                        if ok {
                            let touched =
                                (at.min(held) / 8) as usize..((at.max(held) + 15) / 8) as usize + 1;
                            let reachable = touched.clone().all(|index| {
                                patched.get(index) == payload.get(index)
                                    || (written.contains_key(&index)
                                        && patched
                                            .get(index)
                                            .is_some_and(|v| self.byte_bits.contains_key(v)))
                            });
                            if reachable {
                                for index in touched {
                                    if patched.get(index) == payload.get(index) {
                                        continue;
                                    }
                                    let (Some(&at), Some(&value)) =
                                        (written.get(&index), patched.get(index))
                                    else {
                                        continue;
                                    };
                                    if let Some(bits) = self.byte_bits.get(&value) {
                                        patches.push((at, bits.clone()));
                                    }
                                }
                                payload = patched;
                                self.stats.aim_written += 1;
                            }
                        }
                    }
                }
            }
        }

        // Clip substitutions first: they change no length and do not disturb where anything sits,
        // so the overlay pin below still reads the offsets it computed.
        for sampler in &found {
            let Some(&want) = swaps.get(&sampler.clip) else {
                continue;
            };
            let mut patched = payload.clone();
            if crate::poserecipe::write_clip(&mut patched, sampler.time_at, want).is_err() {
                continue;
            }
            let touched = crate::poserecipe::clip_bytes(sampler.time_at);
            let reachable = touched.clone().all(|index| {
                patched.get(index) == payload.get(index)
                    || (written.contains_key(&index)
                        && patched
                            .get(index)
                            .is_some_and(|value| self.byte_bits.contains_key(value)))
            });
            if !reachable {
                self.stats.clips_missed += 1;
                continue;
            }
            for index in touched {
                if patched.get(index) == payload.get(index) {
                    continue;
                }
                let (Some(&at), Some(&value)) = (written.get(&index), patched.get(index)) else {
                    continue;
                };
                let Some(bits) = self.byte_bits.get(&value) else {
                    continue;
                };
                patches.push((at, bits.clone()));
            }
            payload = patched;
            self.stats.clips_swapped += 1;
        }

        for restart in &restarts {
            // A rewind goes back to the time it held; an overlay that was just added to the recipe
            // has no earlier time, so it is written as already finished instead. Either way the
            // animation has nothing left to play.
            let was = match restart.was {
                Some(was) => was,
                None => u16::MAX as u32,
            };
            let _ = &before;
            let touched = crate::poserecipe::time_bytes(restart.time_at);
            let mut patched = payload.clone();
            if crate::poserecipe::write_time(&mut patched, restart.time_at, was).is_err() {
                self.stats.overlays_missed += 1;
                continue;
            }
            // Every byte that has to change must be one this update carried, and its new value
            // must be one already seen encoded. Otherwise leave the restart alone rather than
            // emit something unverifiable.
            let reachable = touched.clone().all(|index| {
                patched.get(index) == payload.get(index)
                    || (written.contains_key(&index)
                        && patched
                            .get(index)
                            .is_some_and(|value| self.byte_bits.contains_key(value)))
            });
            if !reachable {
                self.stats.overlays_missed += 1;
                continue;
            }
            for index in touched {
                if patched.get(index) == payload.get(index) {
                    continue;
                }
                let (Some(&at), Some(&value)) = (written.get(&index), patched.get(index)) else {
                    continue;
                };
                let Some(bits) = self.byte_bits.get(&value) else {
                    continue;
                };
                patches.push((at, bits.clone()));
            }
            payload = patched;
            self.stats.overlays_frozen += 1;
        }

        // The tracker must believe what the client will now believe, or the next tick compares
        // against a state that was never sent.
        let tracker = self.poses.entry(id).or_default();
        for (index, value) in payload.iter().enumerate() {
            tracker.set_byte(index, *value);
        }
        Ok(patches)
    }

    /// Append a sampled clip and a blend onto the active slot's recipe, as new field writes.
    ///
    /// This is the first edit in this file that changes a recipe's *shape* rather than a value
    /// inside it. It works by appending, which is what makes it safe: dependency indices only name
    /// earlier tasks, so every existing entry keeps its meaning, and the two new tasks' payload
    /// fields land past the end of the existing ones rather than displacing them.
    ///
    /// The blend consumes the previous last task and the new sample, so it becomes the recipe's
    /// result — appending is how a task takes effect as well as how it is added.
    fn append_clip_writes(&mut self, id: i32, tick: i32) -> Result<Vec<(Vec<i32>, Bits)>> {
        let Some(&(clip, weight, mask)) = self.edit.append_clip.get(&id) else {
            return Ok(Vec::new());
        };
        if tick < self.edit.from_tick || tick > self.edit.to_tick {
            // Past the window, give every slot we modified its original topology back. Leaving them
            // means the appended tasks keep being evaluated long after the beat is over.
            let touched = self.append_touched.remove(&id).unwrap_or_default();
            let mut writes = Vec::new();
            for slot in touched {
                if let Some(original) = self.append_base.get(&(id, slot)) {
                    writes.push((vec![9, 32, slot as i32, 0], encode_binary_block(original)));
                    self.stats.append_restored += 1;
                }
            }
            return Ok(writes);
        }

        let from = self.edit.from_tick;
        let tracker = self.poses.entry(id).or_default();
        let Some(active) = tracker.active_slot() else {
            return Ok(Vec::new());
        };
        let Some(current) = tracker.topology_blob(active).map(<[u8]>::to_vec) else {
            return Ok(Vec::new());
        };

        // Follow the active slot. Pinning to one was tried, to avoid leaving modified topologies
        // in slots the client might come back to, and it does stop the crashes — but measurement
        // at verified ticks shows a pinned append never reaches the rendered pose at all, because
        // the pinned slot is rarely the one being drawn. Following the active slot drives the pose
        // on every tick, which is the point.
        //
        // The cost is that a long range eventually crashes: modified topologies accumulate across
        // slots and one is read against a payload that has moved on. A short window survives a full
        // playthrough, and a short window is what a choreographed beat wants anyway. Restoring
        // vacated slots was tried and does not help, so the envelope is the window length, not a
        // cleverer write pattern.
        self.append_slot.insert(id, active);
        self.append_touched.entry(id).or_default().insert(active);
        let restore: Option<(u32, Vec<u8>)> = None;
        // Whatever the game last sent for this slot, never our own output.
        let blob = self
            .append_base
            .entry((id, active))
            .or_insert(current)
            .clone();
        let Some(original) = crate::poserecipe::parse_topology(&blob) else {
            return Ok(Vec::new());
        };
        let sequence: Vec<u32> = original.tasks.iter().map(|task| task.type_id).collect();
        let tracker = self.poses.entry(id).or_default();

        let mut payload = tracker.payload().to_vec();
        payload.resize(crate::poserecipe::PAYLOAD_BYTES, 0);
        let Some(end) = crate::poserecipe::payload_end(&sequence, &payload) else {
            return Ok(Vec::new());
        };

        // Refuse rather than corrupt when the two tasks would not fit. The payload is a fixed
        // buffer and running past it is the one mistake here that the client would read as
        // plausible data rather than as an error.
        // A reference pose task carries no payload at all and takes no dependencies, so it costs
        // only the blend. It is also the most visible thing that can be appended: blended in at
        // full weight the character snaps to its bind pose, which cannot be mistaken for a subtle
        // difference or for nothing happening.
        // Three shapes, chosen so the failure can be bisected: a clip sample, a bare reference
        // pose, or no new producer at all — just a blend of two tasks the recipe already has. The
        // last one adds a task without adding anything that produces a pose from nothing, which is
        // the difference that matters if the crash is about uninitialised task state.
        let reference_pose = clip == u32::MAX;
        let blend_only = clip == u32::MAX - 1;
        // 0x8000_0000 | (deps << 8) | type_id — a candidate task class to probe. See
        // append_unknown_task: eight of the seventeen classes never occur in a recipe, so their
        // ids are unknown, and trying one is the only way to find out which is which.
        // The ref and blend sentinels are u32::MAX and u32::MAX - 1, which also have the high bit
        // set, so a probe has to exclude them explicitly or `ref` is read as type id 255.
        let probe = (clip & 0x8000_0000) != 0 && !reference_pose && !blend_only;
        let probe_type = clip & 0xFF;
        let probe_deps = (clip >> 8) & 0xFF;
        let sample_bits = if reference_pose || blend_only || probe {
            0
        } else {
            crate::poserecipe::CLIP_ID_BITS + 16
        };
        let needed = sample_bits + 8 + 1 + if mask.is_some() { 12 } else { 0 };
        if end + needed > (crate::poserecipe::PAYLOAD_BYTES as u32) * 8 {
            self.stats.append_no_room += 1;
            self.stats.append_worst_end = self.stats.append_worst_end.max(end);
            return Ok(Vec::new());
        }
        self.stats.append_written += 1;
        self.stats.append_worst_end = self.stats.append_worst_end.max(end);

        let mut topology = original.clone();
        let (first, second) = if blend_only {
            // No new producer: blend the recipe's result with the task feeding it.
            let last = topology.tasks.len() as u32 - 1;
            if last == 0 {
                return Ok(Vec::new());
            }
            (last - 1, last)
        } else {
            let added = if probe {
                let last = topology.tasks.len() as u32 - 1;
                let deps = (0..probe_deps).map(|n| last.saturating_sub(n)).collect();
                crate::poserecipe::append_unknown_task(&mut topology, probe_type, deps)?
            } else if reference_pose {
                crate::poserecipe::append_task(&mut topology, 6, vec![])?
            } else {
                crate::poserecipe::append_task(&mut topology, 1, vec![])?
            };
            (added - 1, added)
        };
        crate::poserecipe::append_task(&mut topology, 7, vec![first, second])?;
        let new_blob = crate::poserecipe::encode_topology(&topology)?;

        // A sixteen bit normalised time, advanced so the clip plays rather than holding its first
        // frame. The rate is arbitrary until a real clip's duration is known; what matters here is
        // that the value moves, because a still pose and a broken append look identical.
        const TIME_PER_TICK: u32 = 600;
        let time = (tick.saturating_sub(from) as u32).wrapping_mul(TIME_PER_TICK) & 0xFFFF;

        let before = payload.clone();
        let after_added = if reference_pose || blend_only || probe {
            end
        } else {
            crate::poserecipe::append_sample_payload(&mut payload, end, clip, time)?
        };
        crate::poserecipe::append_blend_payload(&mut payload, after_added, weight, mask)?;

        let mut writes = Vec::new();
        if let Some((slot, original)) = restore {
            writes.push((vec![9, 32, slot as i32, 0], encode_binary_block(&original)));
            self.stats.append_restored += 1;
        }
        writes.push((
            vec![9, 32, active as i32, 0],
            encode_binary_block(&new_blob),
        ));
        for (index, (old, new)) in before.iter().zip(payload.iter()).enumerate() {
            if old != new {
                writes.push((vec![9, 33, index as i32], encode_varint(u32::from(*new))));
            }
        }

        // The tracker must believe what the client will now believe, or the next tick is computed
        // against a state that was never sent.
        tracker.set_topology(active, new_blob);
        for (index, value) in payload.iter().enumerate() {
            tracker.set_byte(index, *value);
        }
        Ok(writes)
    }

    fn rewrite(
        &mut self,
        msg: &mut CsvcMsgPacketEntities,
        tick: i32,
        last_entity_message: bool,
    ) -> Result<usize> {
        let watching = tick >= self.edit.from_tick && tick <= self.edit.to_tick;
        let data = msg.entity_data().to_vec();
        let mut reader = Bitreader::new(&data);
        let mut id = -1;
        let class_bits = (self.state.classes.len() as f32).log2().ceil() as u32;
        let mut dropped = 0usize;
        let mut entries: Vec<RewrittenEntity> =
            Vec::with_capacity(msg.updated_entries() as usize + self.edit.scheduled_writes.len());

        for _ in 0..msg.updated_entries() {
            let step = reader.read_u_bit_var()?;
            id += 1 + step as i32;
            let command = reader.read_nbits(2)?;
            let mut entry = RewrittenEntity {
                id,
                step,
                command,
                create: None,
                pvs: None,
                fields: Vec::new(),
            };
            if command & 1 != 0 {
                entries.push(entry);
                self.state.entities.remove(&id);
                self.model_handles
                    .retain(|(entity_id, _), _| *entity_id != id);
                if command == 3 {
                    self.state.alternate_baselines.remove(&id);
                }
                continue;
            }
            if command == 2 {
                let class = reader.read_nbits(class_bits)?;
                let serial = reader.read_nbits(17)?;
                let unknown = reader.read_varint()?;
                entry.create = Some((class, serial, unknown));
                let mut entity = Entity {
                    class,
                    serial,
                    unknown,
                    values: BTreeMap::new(),
                };
                if let Some(baseline) = self.state.baseline(id, class)? {
                    update(
                        &mut Bitreader::new(baseline),
                        baseline,
                        &mut entity,
                        &self.state.classes[class as usize].serializer,
                        self.state.qf,
                        self.state.huf,
                    )?;
                }
                self.state.entities.insert(id, entity);
                self.model_handles
                    .retain(|(entity_id, _), _| *entity_id != id);
            } else if msg.has_pvs_vis_bits_deprecated() != 0 {
                let pvs = reader.read_nbits(2)?;
                entry.pvs = Some(pvs);
            }

            let mut scheduled_for_entity = Vec::new();
            for (schedule_index, scheduled) in self.edit.scheduled_writes.iter().enumerate() {
                let create_only = self.edit.create_only_scheduled_writes;
                let create_copy =
                    (self.edit.allow_scheduled_create_writes || create_only) && command == 2;
                if scheduled.tick != tick
                    || scheduled.entity_id != id
                    || !scheduled_write_targets_entry(
                        self.edit.allow_scheduled_create_writes, create_only, command,
                    )
                    || (!last_entity_message
                        && !create_copy)
                    // Create writes are repeated in every checkpoint/ordinary copy. Delta
                    // writes retain their historical once-per-schedule-row behavior.
                    || (!create_copy && self.scheduled_applied.contains(&schedule_index))
                {
                    continue;
                }
                ensure!(command == 0 || create_copy,
                    "scheduled write at tick {tick} for entity {id} requires an existing delta entry; create writes require explicit opt-in");
                let current = self.state.entities.get(&id).with_context(|| {
                    format!("scheduled target entity {id} is not alive at tick {tick}")
                })?;
                if create_copy
                    && !scheduled_create_copy_matches(
                        scheduled,
                        tick,
                        id,
                        current.class,
                        current.serial,
                    )
                {
                    // The same numeric index can occur in another create at this tick. Only
                    // matching class/serial copies receive this schedule row.
                    continue;
                }
                ensure!(
                    current.class == scheduled.class_id && current.serial == scheduled.serial,
                    "scheduled target entity {id} lifetime differs at tick {tick}"
                );
                scheduled_for_entity.push((
                    schedule_index,
                    scheduled.field_path.clone(),
                    self.scheduled_field_bits(scheduled)?,
                    self.scheduled_vector_length(scheduled)?,
                ));
            }

            let entity = self
                .state
                .entities
                .get_mut(&id)
                .context("update before entity create")?;
            let serializer = &self.state.classes[entity.class as usize].serializer;
            let editing = watching;

            let mut kept: Vec<(Vec<i32>, Bits)> = Vec::new();
            let mut saw_angle = false;
            let mut saw_body = false;
            // Where appended attributes start, set when the list length is grown.
            let mut sticker_base: Option<i32> = None;
            // Pose writes have to be collected for anything that edits the recipe, not only for
            // an overlay freeze — an append needs the tracker current for the same reason.
            let freezing =
                self.edit.freeze_overlay.contains(&id) || self.edit.append_clip.contains_key(&id);
            // Index into `kept` alongside what the write means for the recipe, so the payload can
            // be reconsidered once the whole update is known rather than field by field.
            let mut pose: Vec<(usize, PoseWrite)> = Vec::new();
            for fp in paths(&mut reader, self.state.huf)? {
                let field = find_field(&fp, serializer)?;
                let begin =
                    data.len() * 8 - reader.bits_remaining().context("missing bit offset")?;
                let value = reader.decode(&get_decoder_from_field(field)?, self.state.qf)?;
                let end = data.len() * 8 - reader.bits_remaining().context("missing bit offset")?;
                let key = fp.path[..=fp.last].to_vec();

                let bits = slice_bits(&data, begin, end);

                if key.as_slice() == [12, 17] {
                    let applicable = self
                        .edit
                        .pawn_mesh_group_remaps
                        .iter()
                        .filter(|map| {
                            map.entity_id == id
                                && map.serial.is_none_or(|serial| serial == entity.serial)
                        })
                        .collect::<Vec<_>>();
                    if !applicable.is_empty() {
                        let class_name = &self.state.classes[entity.class as usize].name;
                        ensure!(class_name == "CCSPlayerPawn",
                            "mesh-group remap target entity {id} is schema class {class_name}, expected CCSPlayerPawn (runtime RTTI C_CSPlayerPawn)");
                        let map = applicable[0];
                        ensure!(
                            applicable.len() == 1,
                            "multiple mesh-group remaps apply to pawn entity {id}"
                        );
                        ensure!(
                            map.class_name == *class_name,
                            "mesh-group manifest class {:?} differs from entity {id} class {:?}",
                            map.class_name,
                            class_name
                        );
                        ensure!(
                            map.mask_path == key,
                            "mesh-group manifest mask path {:?} differs from observed path {:?}",
                            map.mask_path,
                            key
                        );
                        let model_handle = if let Some(&handle) =
                            self.model_handles.get(&(id, map.model_field_path.clone()))
                        {
                            Some(handle)
                        } else {
                            stored_entity_model_handle(
                                entity,
                                &map.model_field_path,
                                serializer,
                                self.state.qf,
                            )?
                        };
                        ensure!(model_handle_matches_guard(model_handle, map.model_handle),
                            "pawn {id} mesh mask model guard mismatch at path {:?}: expected {}, decoded {:?}",
                            map.model_field_path, map.model_handle, model_handle);
                        let old_value = match &value {
                            Variant::U64(value) => *value,
                            _ => anyhow::bail!("pawn {id} mesh-group mask at [12,17] is not an unsigned 64-bit value"),
                        };
                        let restored_mask = (map.new_mask & !map.preserve_mask_bits)
                            | (map.old_mask & map.preserve_mask_bits);
                        ensure!(old_value == map.old_mask || old_value == restored_mask,
                            "pawn {id} mesh-group mask {old_value} is neither expected old mask {} nor already-restored mask {}",
                            map.old_mask, restored_mask);
                        *self
                            .stats
                            .pawn_mesh_group_masks_matched
                            .entry(id)
                            .or_default() += 1;
                        if old_value != restored_mask {
                            let replacement = encode_varint_u64(restored_mask);
                            entity.values.insert(key.clone(), replacement.clone());
                            kept.push((key, replacement));
                            *self
                                .stats
                                .pawn_mesh_group_masks_rewritten
                                .entry(id)
                                .or_default() += 1;
                            self.substituted = true;
                            continue;
                        }
                    }
                }

                if field_name(field).rsplit('.').next() == Some("m_hModel") {
                    if let Variant::U64(old_id) = &value {
                        self.model_handles.insert((id, key.clone()), *old_id);
                        if *old_id != 0 {
                            *self.stats.packet_model_ids.entry(*old_id).or_default() += 1;
                        }
                        if editing {
                            if let Some(&new_id) = self.edit.model_remap.get(old_id) {
                                let replacement = encode_varint_u64(new_id);
                                entity.values.insert(key.clone(), replacement.clone());
                                kept.push((key, replacement));
                                self.stats.models_rewritten += 1;
                                self.substituted = true;
                                continue;
                            }
                        }
                    }
                }

                // The magazine is decided rather than simply dropped: it is an absolute count,
                // and a suppressed shot changes what every later count should say.
                if self.edit.targets.contains_key(&id) && is_clip_field(field) {
                    if let Some(number) = as_integer(&value) {
                        let action = self
                            .clips
                            .entry(id)
                            .or_default()
                            .decide(number, &bits, editing);
                        let carried = match action {
                            ClipAction::Drop => {
                                dropped += 1;
                                continue;
                            }
                            ClipAction::Keep => bits,
                            ClipAction::Rewrite(replacement) => {
                                dropped += 1;
                                replacement
                            }
                        };
                        entity.values.insert(key.clone(), carried.clone());
                        kept.push((key, carried));
                        continue;
                    }
                }

                // A steady sweep, written from whichever recorded direction is closest to the one
                // wanted. The residual is a fraction of a degree, because a round holds thousands
                // of distinct directions to choose from.
                if editing {
                    if let Some((spun, per_tick)) = self.edit.spin {
                        let name = field_name(field);
                        let is_view = name.ends_with("m_angEyeAngles");
                        let is_body = name.ends_with("m_angRotation");
                        if spun == id && is_view {
                            self.stats.angles_seen += 1;
                        }
                        if spun == id && (is_view || is_body) {
                            // Straight off the tick, not an offset from the range start: that
                            // start defaults to i32::MIN, and subtracting it overflows into a
                            // target angle that wanders instead of sweeping. Wrapping makes the
                            // absolute tick perfectly good as a phase.
                            let wanted = tick as f32 * per_tick;
                            let wrap = |a: f32| {
                                let mut d = a % 360.0;
                                if d > 180.0 {
                                    d -= 360.0;
                                } else if d < -180.0 {
                                    d += 360.0;
                                }
                                d
                            };
                            let target = wrap(wanted);
                            // Written exactly rather than chosen from what was recorded. The
                            // component encoding is a plain linear quantisation, so the wanted
                            // angle can simply be encoded — which removes both the yaw error and
                            // the pitch jitter that picking a nearby recorded direction caused.
                            let pitch = self.edit.spin_pitch.unwrap_or(0.0);
                            let replacement = encode_qangle_pres([pitch, target, 0.0]);
                            if is_body {
                                self.spin_body_path = Some(key.clone());
                                saw_body = true;
                            } else {
                                self.spin_path = Some(key.clone());
                                saw_angle = true;
                            }
                            entity.values.insert(key.clone(), replacement.clone());
                            kept.push((key, replacement));
                            self.stats.angles_rewritten += 1;
                            self.substituted = true;
                            continue;
                        }
                    }
                }

                // The attribute list's own length. Growing it is what makes room for stickers the
                // recording never had; the entries themselves are appended below.
                if editing {
                    if let Some(extra) = self.edit.add_stickers.get(&id) {
                        if key.as_slice() == [90] && !extra.is_empty() {
                            if let Some(current) = as_integer(&value) {
                                let grown = current as u32 + (extra.len() as u32 * 2);
                                let bits = encode_varint(grown);
                                entity.values.insert(key.clone(), bits.clone());
                                kept.push((key, bits));
                                sticker_base = Some(current as i32);
                                self.substituted = true;
                                continue;
                            }
                        }
                    }
                }

                // The paint kit, rewritten in place. Attribute zero of the econ list is the kit;
                // one is the seed and two the wear, and those are left alone so the pattern and
                // condition stay as recorded.
                if editing {
                    if let Some(&kit) = self.edit.swap_paint.get(&id) {
                        let bare = field_name(field);
                        let bare = bare.rsplit('.').next().unwrap_or(bare.as_str());
                        let is_attribute_value =
                            bare == "m_iRawValue32" || bare == "m_flInitialValue";
                        if is_attribute_value && key.len() == 3 && key[0] == 90 && key[1] == 0 {
                            let bits = encode_f32_noscale(kit);
                            entity.values.insert(key.clone(), bits.clone());
                            kept.push((key, bits));
                            self.stats.paints_swapped += 1;
                            self.substituted = true;
                            continue;
                        }
                    }
                }

                // A switch to a skipped weapon is dropped outright, so the previous one stays in
                // hand. Value-aware, because the field is the same one for every weapon and only
                // the handle it carries says which.
                if editing
                    && !self.edit.skip_weapons.is_empty()
                    && is_active_weapon_field(field)
                    && as_integer(&value).is_some_and(|handle| {
                        self.edit
                            .skip_weapons
                            .contains(&crate::suppress::entity_index(handle as u32))
                    })
                {
                    dropped += 1;
                    continue;
                }

                // Holstering and deploying share this field with firing, and only firing is ours.
                if editing
                    && is_weapon_state_field(field)
                    && as_integer(&value) != Some(WEAPON_STATE_FIRING)
                {
                    entity.values.insert(key.clone(), bits.clone());
                    kept.push((key, bits));
                    continue;
                }

                if editing && self.edit.drops(id, field, tick, &key) {
                    // Dropped from the wire and from the accumulated state together, so a
                    // checkpoint later rebuilt from this state carries the suppressed value too.
                    dropped += 1;
                    continue;
                }

                match (field, &value) {
                    (Field::Vector(_), Variant::U32(size)) => entity.values.retain(|path, _| {
                        !path.starts_with(&key)
                            || path.len() <= key.len()
                            || path[key.len()] < *size as i32
                    }),
                    (Field::Pointer(_), Variant::Bool(false)) => entity
                        .values
                        .retain(|path, _| !path.starts_with(&key) || path.len() <= key.len()),
                    _ => {}
                }
                if freezing {
                    let bare = field_name(field);
                    let bare = bare.rsplit('.').next().unwrap_or(bare.as_str()).to_string();
                    let note = match (bare.as_str(), &value) {
                        ("m_topology", _) => binary_block(&bits).and_then(|blob| {
                            key.get(2).map(|slot| PoseWrite::Topology {
                                slot: *slot as u32,
                                blob,
                            })
                        }),
                        ("m_SerializePoseRecipeAG2Dynamic", value) => {
                            as_integer(value).zip(key.last()).and_then(|(byte, index)| {
                                (*index >= 0).then_some(PoseWrite::Byte {
                                    index: *index as usize,
                                    value: byte as u8,
                                })
                            })
                        }
                        ("m_nSerializePoseRecipeAG2ActiveSlot", value) => {
                            as_integer(value).map(|slot| PoseWrite::Active { slot: slot as u32 })
                        }
                        _ => None,
                    };
                    if let Some(note) = note {
                        if let PoseWrite::Byte { value, .. } = &note {
                            if encode_varint(*value as u32) == bits {
                                self.stats.byte_encoding_varint += 1;
                            } else {
                                self.stats.byte_encoding_other += 1;
                            }
                            self.byte_bits.entry(*value).or_insert_with(|| bits.clone());
                        }
                        pose.push((kept.len(), note));
                    }
                }
                entity.values.insert(key.clone(), bits.clone());
                kept.push((key, bits));
            }

            if freezing && !pose.is_empty() {
                for (at, bits) in self.freeze_overlay(id, tick, &pose)? {
                    self.substituted = true;
                    kept[at].1 = bits.clone();
                    let entity = self
                        .state
                        .entities
                        .get_mut(&id)
                        .context("update before entity create")?;
                    entity.values.insert(kept[at].0.clone(), bits);
                }
            }

            // A topology the game itself sent replaces our recorded original, so a real animation
            // change is picked up rather than being masked by a stale base.
            for (_, write) in &pose {
                if let PoseWrite::Topology { slot, blob } = write {
                    self.append_base.insert((id, *slot), blob.clone());
                }
            }

            // Appending tasks to the recipe. This runs after the pose writes above have been
            // folded into the tracker, so it appends onto the recipe as the client will hold it
            // rather than onto a stale copy.
            if !pose.is_empty() {
                for (path, bits) in self.append_clip_writes(id, tick)? {
                    self.stats.append_paths_emitted += 1;
                    self.substituted = true;
                    if let Some(entity) = self.state.entities.get_mut(&id) {
                        entity.values.insert(path.clone(), bits.clone());
                    }
                    // A path already present is replaced in place; only a genuinely new one is
                    // added, or the update would carry the same field twice.
                    //
                    // A new one is *inserted in order*, not appended. Field paths are encoded as
                    // deltas from the previous path, so a list that is not ascending encodes to
                    // something the reader decodes as different paths entirely — which is exactly
                    // what happened when these were pushed onto the end: 916 writes were emitted
                    // and not one of them could be read back.
                    if let Some(slot) = kept.iter_mut().find(|(key, _)| *key == path) {
                        slot.1 = bits;
                    } else {
                        let at = kept
                            .iter()
                            .position(|(key, _)| *key > path)
                            .unwrap_or(kept.len());
                        kept.insert(at, (path, bits));
                    }
                }
            }

            // The appended attributes themselves, written on the update that made room for them.
            if let (Some(base), Some(extra)) = (sticker_base, self.edit.add_stickers.get(&id)) {
                for (offset, (slot, sticker)) in extra.iter().enumerate() {
                    let at = base + (offset as i32 * 2);
                    // Slot zero begins at 113; each slot takes four definitions, of which the id
                    // and the wear are the two that matter. Scale and rotation are left for the
                    // game to default, exactly as an item with no explicit placement does.
                    for (index, (definition, raw)) in [
                        (at, (STICKER_SLOT_BASE + slot * 4, *sticker)),
                        (at + 1, (STICKER_SLOT_BASE + 1 + slot * 4, 0u32)),
                    ] {
                        for (leaf, bits) in [
                            (0, encode_varint(definition)),
                            (1, encode_raw_u32(raw)),
                            (2, encode_raw_u32(raw)),
                        ] {
                            let path = vec![90, index, leaf];
                            if let Some(entity) = self.state.entities.get_mut(&id) {
                                entity.values.insert(path.clone(), bits.clone());
                            }
                            kept.push((path, bits));
                        }
                    }
                }
                self.stats.stickers_added += extra.len();
                self.substituted = true;
            }

            // A spin needs a new direction every tick, and the recording only supplies one when
            // the player turned. Where this entry has none, add it: the entity is already in the
            // packet, so this appends a field to an update rather than inventing an update.
            if let Some((spun, per_tick)) = self.edit.spin {
                if spun == id
                    && !saw_angle
                    && watching
                    && tick >= self.edit.from_tick
                    && tick <= self.edit.to_tick
                {
                    if let Some(path) = self.spin_path.clone() {
                        let wrap = |a: f32| {
                            let mut d = a % 360.0;
                            if d > 180.0 {
                                d -= 360.0;
                            } else if d < -180.0 {
                                d += 360.0;
                            }
                            d
                        };
                        let target = wrap(tick as f32 * per_tick);
                        let pitch = self.edit.spin_pitch.unwrap_or(0.0);
                        let bits = encode_qangle_pres([pitch, target, 0.0]);
                        let at = kept
                            .iter()
                            .position(|(key, _)| *key > path)
                            .unwrap_or(kept.len());
                        if let Some(entity) = self.state.entities.get_mut(&id) {
                            entity.values.insert(path.clone(), bits.clone());
                        }
                        kept.insert(at, (path, bits));
                        self.stats.angles_injected += 1;
                        self.substituted = true;
                    }
                }
                // The model's own rotation needs filling on the same ticks, or the body stutters
                // while the view turns smoothly.
                if spun == id
                    && !saw_body
                    && watching
                    && tick >= self.edit.from_tick
                    && tick <= self.edit.to_tick
                {
                    if let Some(path) = self.spin_body_path.clone() {
                        let wrap = |a: f32| {
                            let mut d = a % 360.0;
                            if d > 180.0 {
                                d -= 360.0;
                            } else if d < -180.0 {
                                d += 360.0;
                            }
                            d
                        };
                        let bits = encode_qangle_pres([0.0, wrap(tick as f32 * per_tick), 0.0]);
                        let at = kept
                            .iter()
                            .position(|(key, _)| *key > path)
                            .unwrap_or(kept.len());
                        if let Some(entity) = self.state.entities.get_mut(&id) {
                            entity.values.insert(path.clone(), bits.clone());
                        }
                        kept.insert(at, (path, bits));
                        self.stats.angles_injected += 1;
                        self.substituted = true;
                    }
                }
            }

            if let Some(seed) = &self.edit.pose_seed {
                let is_create = command == 2;
                let in_refresh_window = if seed.donor_ticks_only {
                    seed.donor_dynamic_by_tick.contains_key(&tick)
                } else {
                    seed.refresh_window
                        .is_none_or(|(from, to)| tick >= from && tick <= to)
                };
                let slot_stream = !seed.donor_slot_tables.is_empty();
                if slot_stream
                    && id == seed.entity_id
                    && self.state.entities.get(&id).is_some_and(|entity| {
                        entity.class == seed.class_id
                            && seed.serial.is_none_or(|serial| entity.serial == serial)
                    })
                {
                    observe_slot_pool_input(
                        &mut self.pose_slots_written,
                        &kept,
                        seed.body_path,
                        seed.slots_field,
                        is_create,
                    );
                }
                if ((seed.seed_on_create && is_create)
                    || (seed.refresh_dynamic_on_update && command == 0 && in_refresh_window))
                    && (!slot_stream || seed.donor_dynamic_by_tick.contains_key(&tick))
                    && id == seed.entity_id
                    && self.state.entities.get(&id).is_some_and(|entity| {
                        entity.class == seed.class_id
                            && seed.serial.is_none_or(|serial| entity.serial == serial)
                    })
                {
                    let body = seed.body_path;
                    let mut writes = Vec::new();
                    let scheduled_dynamic = seed.donor_dynamic_by_tick.contains_key(&tick);
                    let mut wrote_scheduled_dynamic = false;
                    if seed.emit_shape {
                        if is_create && !seed.preserved_slot_topologies.is_empty() {
                            let preserved = &seed.preserved_slot_topologies;
                            ensure!(
                                seed.donor_topologies.starts_with(preserved),
                                "pinned AG2 slot pool does not begin with preserved slots"
                            );
                            ensure!(
                                seed.donor_topologies.len() > preserved.len(),
                                "pinned AG2 slot pool appends no topology"
                            );
                            let size_path = vec![body, seed.slots_field];
                            let existing_size = kept
                                .iter()
                                .find(|(path, _)| *path == size_path)
                                .context("pinned AG2 create lacks existing slot-vector length")?;
                            ensure!(
                                existing_size.1 == encode_varint(preserved.len() as u32),
                                "pinned AG2 create has a different existing slot count"
                            );
                            for (slot, expected) in preserved.iter().enumerate() {
                                let path = vec![body, seed.slots_field, slot as i32, 0];
                                let existing = kept
                                    .iter()
                                    .find(|(candidate, _)| *candidate == path)
                                    .with_context(|| {
                                        format!("pinned AG2 create lacks slot {slot}")
                                    })?;
                                ensure!(
                                    binary_block(&existing.1).as_deref()
                                        == Some(expected.as_slice()),
                                    "pinned AG2 create slot {slot} topology differs"
                                );
                            }
                            writes.push((
                                size_path,
                                encode_varint(seed.donor_topologies.len() as u32),
                            ));
                            for (slot, topology) in seed
                                .donor_topologies
                                .iter()
                                .enumerate()
                                .skip(preserved.len())
                            {
                                let path = vec![body, seed.slots_field, slot as i32, 0];
                                ensure!(
                                    !kept.iter().any(|(existing, _)| *existing == path),
                                    "pinned AG2 create already contains appended slot {slot}"
                                );
                                writes.push((path, encode_binary_block(topology)));
                            }
                        }
                        // Preserve an unscheduled creation recipe. A full-packet
                        // checkpoint can recreate the entity on a scheduled
                        // donor tick; grow the pool and write that tick's
                        // authored active slot/dynamic on the same create.
                        if !is_create
                            || seed.preserved_slot_topologies.is_empty()
                            || scheduled_dynamic
                        {
                            let mut dynamic = seed
                                .donor_dynamic_by_tick
                                .get(&tick)
                                .cloned()
                                .unwrap_or_else(|| seed.dynamic.clone());
                            if let Some(offset) = seed.dynamic_tick_offset {
                                let server_tick = tick
                                    .checked_add(offset)
                                    .context("pose seed server tick overflow")?;
                                ensure!(
                                    dynamic.len() >= 4,
                                    "pose seed payload has no server-tick word"
                                );
                                dynamic[..4].copy_from_slice(&server_tick.to_le_bytes());
                            }
                            if !is_create && in_refresh_window {
                                if let Some((time_at, base_tick, units_per_tick)) =
                                    seed.sampler_advance
                                {
                                    let elapsed = u32::try_from(
                                        tick.checked_sub(base_tick)
                                            .context("sampler advance tick difference overflow")?,
                                    )
                                    .context("sampler advance precedes base tick")?;
                                    let base = crate::poserecipe::samplers(
                                        &crate::poserecipe::topology(&seed.topology)
                                            .context("invalid seed topology")?,
                                        &dynamic,
                                    )
                                    .context("invalid seed sampler values")?
                                    .into_iter()
                                    .find(|sampler| sampler.time_at == time_at)
                                    .context("sampler time offset not found")?
                                    .time;
                                    let advanced = elapsed
                                        .checked_mul(units_per_tick)
                                        .and_then(|delta| base.checked_add(delta))
                                        .context("sampler advance overflow")?;
                                    ensure!(
                                        advanced <= u16::MAX as u32,
                                        "advanced sampler time exceeds 16 bits"
                                    );
                                    crate::poserecipe::write_time(&mut dynamic, time_at, advanced)?;
                                }
                            }
                            if seed.emit_topology
                                && is_create
                                && seed.preserved_slot_topologies.is_empty()
                            {
                                let topologies = if seed.donor_topologies.is_empty() {
                                    vec![seed.topology.clone()]
                                } else {
                                    seed.donor_topologies.clone()
                                };
                                writes.push((
                                    vec![body, seed.slots_field],
                                    encode_varint(topologies.len() as u32),
                                ));
                                for (slot, topology) in topologies.iter().enumerate() {
                                    writes.push((
                                        vec![body, seed.slots_field, slot as i32, 0],
                                        encode_binary_block(topology),
                                    ));
                                }
                            }
                            if slot_stream {
                                let desired = seed
                                    .donor_slot_tables
                                    .range(..=tick)
                                    .next_back()
                                    .map(|(_, table)| table.clone())
                                    .context("slot-stream tick precedes the first slot table")?;
                                let previous = if is_create {
                                    None
                                } else {
                                    self.pose_slots_written.as_deref()
                                };
                                writes.extend(slot_pool_writes(
                                    &desired,
                                    previous,
                                    body,
                                    seed.slots_field,
                                ));
                                self.pose_slots_written = Some(desired);
                            }
                            if !seed.donor_slot_by_tick.is_empty() {
                                let slot = seed.donor_slot_by_tick.get(&tick).copied().unwrap_or(0);
                                writes.push((
                                    vec![body, seed.active_slot_field],
                                    encode_varint(slot),
                                ));
                            }
                            writes.push((
                                vec![body, seed.dynamic_field],
                                encode_varint(dynamic.len() as u32),
                            ));
                            for (index, byte) in dynamic.iter().enumerate() {
                                writes.push((
                                    vec![body, seed.dynamic_field, index as i32],
                                    encode_varint(*byte as u32),
                                ));
                            }
                            wrote_scheduled_dynamic = scheduled_dynamic;
                        }
                    }
                    // Both signed fields use ZigZag varints on the wire.
                    if seed.emit_version && is_create {
                        writes.push((
                            vec![body, seed.version_field],
                            encode_varint(seed.recipe_version * 2),
                        ));
                    }
                    if seed.emit_context && is_create {
                        writes.push((
                            vec![body, seed.context_field],
                            encode_varint(seed.context_iteration * 2),
                        ));
                    }
                    let dynamic_len = seed
                        .donor_dynamic_by_tick
                        .get(&tick)
                        .map_or(seed.dynamic.len(), Vec::len);
                    for (path, bits) in writes {
                        let resized_len = if path == vec![body, seed.dynamic_field] {
                            Some(dynamic_len)
                        } else if slot_stream && path == vec![body, seed.slots_field] {
                            Some(self.pose_slots_written.as_ref().map_or(0, Vec::len))
                        } else {
                            None
                        };
                        if let Some(len) = resized_len {
                            prune_vector_delta(&mut kept, &path, len);
                        }
                        if let Some(slot) = kept.iter_mut().find(|(key, _)| *key == path) {
                            slot.1 = bits.clone();
                        } else {
                            let at = kept
                                .iter()
                                .position(|(key, _)| *key > path)
                                .unwrap_or(kept.len());
                            kept.insert(at, (path.clone(), bits.clone()));
                        }
                        if let Some(entity) = self.state.entities.get_mut(&id) {
                            if let Some(len) = resized_len {
                                entity
                                    .values
                                    .retain(|child, _| vector_child_in_bounds(child, &path, len));
                            }
                            entity.values.insert(path, bits);
                        }
                    }
                    self.stats.pose_seeds += 1;
                    if wrote_scheduled_dynamic {
                        self.stats.donor_pose_ticks.insert(tick);
                    }
                    self.substituted = true;
                }
            }

            if watching && command == 2 {
                if let Some(seed) = &self.edit.default_controller_seed {
                    if id == seed.entity_id
                        && self
                            .state
                            .entities
                            .get(&id)
                            .is_some_and(|entity| entity.class == seed.class_id)
                    {
                        let source = kept
                            .iter()
                            .find(|(path, _)| path.as_slice() == [seed.source_path]);
                        if let Some((_, bits)) = source {
                            // CHandle fields use unsigned varint wire encoding here. The
                            // 32-bit *length* of this particular value is four varint bytes,
                            // not a little-endian u32. Match the exact source encoding before
                            // copying it to the equivalent current-build descriptor.
                            ensure!(
                                *bits == encode_varint(seed.expected_handle),
                                "source controller wire value {} differs from expected {}",
                                bits.wire_hex(),
                                seed.expected_handle
                            );
                            let path = vec![seed.target_path];
                            ensure!(
                                !kept.iter().any(|(key, _)| *key == path),
                                "default controller already written on create"
                            );
                            let copied = bits.clone();
                            let at = kept
                                .iter()
                                .position(|(key, _)| *key > path)
                                .unwrap_or(kept.len());
                            kept.insert(at, (path.clone(), copied.clone()));
                            if let Some(entity) = self.state.entities.get_mut(&id) {
                                entity.values.insert(path, copied);
                            }
                            self.stats.default_controller_seeds += 1;
                            self.substituted = true;
                        }
                    }
                }
            }

            // Isolated 14184 timing pilot. The exact old active-handle write
            // starts the calibrated AK phase; no donor tick supplies a dynamic
            // byte. Append to the old service at its new field index so every
            // pre-existing path keeps its old meaning.
            if let Some(seed) = self.edit.weapon_timing_seed.clone() {
                if id == seed.entity_id && tick >= seed.switch_tick && tick <= seed.end_tick {
                    ensure!(
                        self.state
                            .entities
                            .get(&id)
                            .is_some_and(|current| current.class == seed.class_id),
                        "timing pilot pawn class changed at tick {tick}"
                    );
                    if tick == seed.switch_tick {
                        let path = [seed.service_path, seed.active_field];
                        let active = kept.iter().find(|(key, _)| key.as_slice() == path);
                        let Some((_, bits)) = active else {
                            anyhow::bail!(
                                "timing pilot expected active-weapon switch at tick {tick}"
                            );
                        };
                        ensure!(
                            *bits == encode_varint(seed.expected_handle),
                            "timing pilot switch handle differs from source proof at tick {tick}"
                        );
                        ensure!(
                            !self.weapon_timing_switch_seen,
                            "duplicate timing pilot switch update at tick {tick}"
                        );
                        self.weapon_timing_switch_seen = true;
                    }
                    ensure!(
                        self.weapon_timing_switch_seen,
                        "timing pilot reached AK window without verified switch"
                    );
                    let elapsed = u32::try_from(tick - seed.switch_tick)?;
                    let timing = simple_ak_timing_vector(elapsed)?;
                    let previous = self.weapon_timing_previous.as_deref();
                    let mut writes = Vec::new();
                    if previous.is_none() {
                        writes.push((
                            vec![seed.service_path, seed.timing_field],
                            encode_varint(timing.len() as u32),
                        ));
                    }
                    for (index, byte) in timing.iter().enumerate() {
                        if previous.is_none_or(|old| old[index] != *byte) {
                            writes.push((
                                vec![seed.service_path, seed.timing_field, index as i32],
                                encode_varint(*byte as u32),
                            ));
                        }
                    }
                    for (path, bits) in writes {
                        ensure!(
                            !kept.iter().any(|(existing, _)| *existing == path),
                            "timing pilot path already written by source at tick {tick}"
                        );
                        let at = kept
                            .iter()
                            .position(|(existing, _)| *existing > path)
                            .unwrap_or(kept.len());
                        kept.insert(at, (path.clone(), bits.clone()));
                        if let Some(current) = self.state.entities.get_mut(&id) {
                            current.values.insert(path, bits);
                        }
                        self.stats.weapon_timing_fields += 1;
                    }
                    self.weapon_timing_previous = Some(timing.to_vec());
                    self.stats.weapon_timing_ticks += 1;
                    self.substituted = true;
                }
            }

            if let Some(seed) = &self.edit.hud_weapon_state_seed {
                if id == seed.entity_id && tick == seed.tick {
                    let current = self
                        .state
                        .entities
                        .get(&id)
                        .context("HUD weapon state target was not created")?;
                    ensure!(
                        current.class == seed.class_id && current.serial == seed.serial,
                        "HUD weapon state target lifetime differs at tick {tick}"
                    );
                    ensure!(
                        command == 0,
                        "HUD weapon state target is not a delta update"
                    );
                    ensure!(
                        kept.iter()
                            .any(|(path, bits)| path.as_slice() == [101]
                                && *bits == encode_varint(2)),
                        "HUD weapon state seed did not coincide with old m_iState=2"
                    );
                    let path = vec![seed.field_path];
                    ensure!(
                        !kept.iter().any(|(old, _)| *old == path),
                        "HUD weapon state field already written"
                    );
                    let bits = encode_varint(seed.value);
                    let at = kept
                        .iter()
                        .position(|(old, _)| *old > path)
                        .unwrap_or(kept.len());
                    kept.insert(at, (path.clone(), bits.clone()));
                    self.state
                        .entities
                        .get_mut(&id)
                        .unwrap()
                        .values
                        .insert(path, bits);
                    self.stats.hud_weapon_state_writes += 1;
                    self.substituted = true;
                }
            }

            let mut scheduled_state_updates = Vec::new();
            scheduled_for_entity.sort_by(|a, b| a.1.cmp(&b.1));
            for (schedule_index, path, bits, vector_len) in scheduled_for_entity {
                if let Some((_, current_bits)) = kept.iter_mut().find(|(old, _)| *old == path) {
                    ensure!(
                        command == 2 && self.edit.allow_scheduled_create_writes,
                        "scheduled field {:?} already appears in entity {id} update at tick {tick}",
                        path
                    );
                    *current_bits = bits.clone();
                } else {
                    let at = kept
                        .iter()
                        .position(|(old, _)| *old > path)
                        .unwrap_or(kept.len());
                    kept.insert(at, (path.clone(), bits.clone()));
                }
                if let Some(length) = vector_len {
                    prune_vector_delta(&mut kept, &path, length);
                }
                scheduled_state_updates.push((path, bits, vector_len));
                if self.scheduled_applied.insert(schedule_index) {
                    self.stats.scheduled_writes += 1;
                }
                self.substituted = true;
            }

            entry.fields = kept;
            entries.push(entry);
            for (path, bits, vector_len) in scheduled_state_updates {
                let values = &mut self.state.entities.get_mut(&id).unwrap().values;
                if let Some(length) = vector_len {
                    values.retain(|child, _| vector_child_in_bounds(child, &path, length));
                }
                values.insert(path, bits);
            }
        }

        // A scheduled delta can name an entity that had no entry in this packet. Keep the
        // entity command stream sorted and let the same encoder rebuild all index gaps below.
        let remaining = self
            .edit
            .scheduled_writes
            .iter()
            .enumerate()
            .filter(|(index, write)| write.tick == tick && !self.scheduled_applied.contains(index))
            .map(|(index, write)| (index, write.clone()))
            .collect::<Vec<_>>();
        let mut synthetic: BTreeMap<i32, RewrittenEntity> = BTreeMap::new();
        for (schedule_index, scheduled) in remaining
            .into_iter()
            .filter(|_| last_entity_message && !self.edit.create_only_scheduled_writes)
        {
            ensure!(
                !entries.iter().any(|entry| entry.id == scheduled.entity_id),
                "scheduled write at tick {tick} targets a create/delete entry for entity {}",
                scheduled.entity_id
            );
            let current = self
                .state
                .entities
                .get(&scheduled.entity_id)
                .with_context(|| {
                    format!(
                        "scheduled target entity {} is not alive at tick {tick}",
                        scheduled.entity_id
                    )
                })?;
            ensure!(
                current.class == scheduled.class_id && current.serial == scheduled.serial,
                "scheduled target entity {} lifetime differs at tick {tick}",
                scheduled.entity_id
            );
            let bits = self.scheduled_field_bits(&scheduled)?;
            let entry = synthetic
                .entry(scheduled.entity_id)
                .or_insert_with(|| RewrittenEntity {
                    id: scheduled.entity_id,
                    step: 0,
                    command: 0,
                    create: None,
                    pvs: (msg.has_pvs_vis_bits_deprecated() != 0).then_some(0),
                    fields: Vec::new(),
                });
            ensure!(
                !entry
                    .fields
                    .iter()
                    .any(|(old, _)| *old == scheduled.field_path),
                "duplicate scheduled field {:?} for entity {} at tick {tick}",
                scheduled.field_path,
                scheduled.entity_id
            );
            let at = entry
                .fields
                .iter()
                .position(|(old, _)| *old > scheduled.field_path)
                .unwrap_or(entry.fields.len());
            entry
                .fields
                .insert(at, (scheduled.field_path.clone(), bits.clone()));
            self.state
                .entities
                .get_mut(&scheduled.entity_id)
                .unwrap()
                .values
                .insert(scheduled.field_path, bits);
            self.scheduled_applied.insert(schedule_index);
            self.stats.scheduled_writes += 1;
            self.substituted = true;
        }
        entries.extend(synthetic.into_values());
        entries.sort_by_key(|entry| entry.id);

        // Audit only after every entity entry and scheduled write in the final
        // entity message has updated resident state. This also exports unchanged
        // entities that have no entry in this packet.
        for (query, current) in matching_entity_field_audits(
            &self.edit.audit_entity_fields,
            &self.state.entities,
            tick,
            last_entity_message,
        ) {
            let serializer = &self.state.classes[current.class as usize].serializer;
            let field_paths = if query.field_paths.is_empty() {
                current.values.keys().cloned().collect::<Vec<_>>()
            } else {
                query.field_paths.clone()
            };
            for path in field_paths {
                ensure!(path.len() <= 7, "audited field path exceeds max length");
                let mut fp = generate_fp();
                fp.last = path
                    .len()
                    .checked_sub(1)
                    .context("empty audited field path")?;
                fp.path[..path.len()].copy_from_slice(&path);
                let field = find_field(&fp, serializer)
                    .with_context(|| format!("resolve audited field path {path:?}"))?;
                let value = if let Some(bits) = current.values.get(&path) {
                    format!(
                        "{:?}",
                        decode_stored_variant(
                            bits,
                            &get_decoder_from_field(field)?,
                            self.state.qf,
                        )?
                    )
                } else {
                    "<absent/default>".to_owned()
                };
                self.stats.audited_entity_fields.push(AuditedEntityField {
                    tick,
                    entity_id: query.entity_id,
                    class_id: current.class,
                    serial: current.serial,
                    field_path: path,
                    field_name: field_name(field),
                    value,
                });
            }
        }

        let mut writer = BitWriter::new();
        let mut lengths = BitWriter::new();
        let mut previous = -1;
        for entry in &mut entries {
            let step = u32::try_from(entry.id - previous - 1)
                .context("entity index gap is negative or overflows")?;
            entry.step = step;
            previous = entry.id;
            writer.write_u_bit_var(step);
            writer.write_nbits(entry.command, 2);
            if entry.command & 1 != 0 {
                continue;
            }
            if let Some((class, serial, unknown)) = entry.create {
                writer.write_nbits(class, class_bits);
                writer.write_nbits(serial, 17);
                writer.write_varint(unknown);
            }
            if let Some(pvs) = entry.pvs {
                writer.write_nbits(pvs, 2);
            }
            let start = writer.bits_written();
            encode_paths(
                &mut writer,
                entry.fields.iter().map(|(key, _)| key),
                self.state.huf,
            )?;
            for (_, bits) in &entry.fields {
                append_bits(&mut writer, bits);
            }
            lengths.write_varint((writer.bits_written() - start) as u32);
        }

        let rebuilt = writer.finish();
        self.stats.identity_checks += 1;
        if !verify_rebuilt(&rebuilt, &entries, class_bits, self.state.huf)? {
            self.stats.identity_failures += 1;
        }
        // Dropping is not the only reason the rebuilt data differs from the original. Anything
        // that substitutes a value or adds a field changes it too, and returning early here threw
        // that work away: the caller re-emitted the packet because `substituted` was set, but the
        // packet still carried the original bytes. An append is invisible in exactly that way —
        // the writes are made, counted, and discarded.
        if dropped == 0 && !self.substituted {
            return Ok(0);
        }

        msg.entity_data = Some(rebuilt.into());
        msg.updated_entries = Some(entries.len() as i32);
        if msg.serialized_entities.is_some() {
            msg.serialized_entities = Some(lengths.finish().into());
        }
        Ok(dropped)
    }
}

/// The animation payload as it stood at one tick, with the state needed to label it.
pub struct PoseSample {
    pub tick: i32,
    pub slot: Option<u32>,
    /// The active slot's accumulated AG2 task topology, when transmitted.
    pub topology: Option<Vec<u8>>,
    /// The accumulated recipe. `None` for an element never yet transmitted.
    pub bytes: Vec<Option<u8>>,
    /// Dynamic vector length transmitted by the recipe's vector field.
    pub payload_len: Option<usize>,
    pub position: (f32, f32, f32),
    /// Both horizontal body-component position fields have been observed.
    pub position_valid: bool,
    /// The body-component world-height field has been observed.
    pub position_z_valid: bool,
    /// The body-component world-height field was written on this tick.
    pub position_z_written: bool,
    pub body_yaw: Option<f32>,
    /// `instancebaseline` until a packet `m_angRotation` write overrides it.
    pub body_yaw_provenance: Option<&'static str>,
    pub eye_pitch: Option<f32>,
    pub eye_yaw: Option<f32>,
    pub aim_punch_angle: Option<(f32, f32)>,
    pub aim_punch_velocity: Option<(f32, f32)>,
    pub aim_punch_tick: Option<i32>,
    pub aim_punch_fraction: Option<f32>,
    pub view_punch_angle: Option<(f32, f32)>,
    pub active_weapon_handle: Option<u32>,
    pub flags: Option<u32>,
    pub ground_handle: Option<u32>,
    pub duck_amount: Option<f32>,
    pub desires_duck: Option<bool>,
    pub walking: Option<bool>,
    pub life_state: Option<u32>,
    /// Elements rewritten on this tick, which is itself a signal worth correlating.
    pub writes: usize,
}

/// The pose recipe as it stood on every tick of a range, accumulated rather than sampled.
///
/// The writes are sparse — a tick rewrites a handful of the payload elements — so a census of
/// writes alone cannot say what the payload *was* at a tick. Correlating the payload against what
/// the player was doing needs its full state, which means carrying it forward.
pub fn pose_samples(
    demo: &[u8],
    idx: &index::DemoIndex,
    entity_id: i32,
    from_tick: i32,
    to_tick: i32,
) -> Result<Vec<PoseSample>> {
    const RECIPE: &str = "m_SerializePoseRecipeAG2Dynamic";
    const SLOT: &str = "m_nSerializePoseRecipeAG2ActiveSlot";
    const TOPOLOGY: &str = "m_topology";

    // A requested window still needs the preceding writes to establish its
    // active slot, weapon and position. Emit only the requested ticks below.
    let (writes, body_yaw_creates) =
        field_writes_with_body_yaw_creates(demo, idx, entity_id, i32::MIN, to_tick)?;
    let mut bytes: Vec<Option<u8>> = vec![None; crate::poserecipe::PAYLOAD_BYTES];
    let recipe_path = writes
        .iter()
        .find(|write| write.name.rsplit('.').next() == Some(RECIPE))
        .and_then(|write| write.path.split_last().map(|(_, parent)| parent.to_vec()));
    let mut payload_len = None;
    let mut slot = None;
    let mut topologies: BTreeMap<u32, Vec<u8>> = BTreeMap::new();
    let mut position = (0f32, 0f32, 0f32);
    let mut seen_x = false;
    let mut seen_y = false;
    let mut seen_z = false;
    let mut body_yaw = None;
    let mut body_yaw_provenance = None;
    let mut next_body_yaw_create = 0;
    let mut eye_pitch = None;
    let mut eye_yaw = None;
    let mut aim_punch_angle = None;
    let mut aim_punch_velocity = None;
    let mut aim_punch_tick = None;
    let mut aim_punch_fraction = None;
    let mut view_punch_angle = None;
    let mut active_weapon_handle = None;
    let mut flags = None;
    let mut ground_handle = None;
    let mut duck_amount = None;
    let mut desires_duck = None;
    let mut walking = None;
    let mut life_state = None;
    let mut samples: Vec<PoseSample> = Vec::new();
    let mut current: Option<i32> = None;
    let mut touched = 0usize;
    let mut touched_z = false;

    for write in &writes {
        if current != Some(write.tick) {
            if let Some(tick) = current {
                if tick >= from_tick {
                    samples.push(PoseSample {
                        tick,
                        slot,
                        topology: slot.and_then(|active| topologies.get(&active).cloned()),
                        bytes: bytes.clone(),
                        payload_len,
                        position,
                        position_valid: seen_x && seen_y,
                        position_z_valid: seen_z,
                        position_z_written: touched_z,
                        body_yaw,
                        body_yaw_provenance,
                        eye_pitch,
                        eye_yaw,
                        aim_punch_angle,
                        aim_punch_velocity,
                        aim_punch_tick,
                        aim_punch_fraction,
                        view_punch_angle,
                        active_weapon_handle,
                        flags,
                        ground_handle,
                        duck_amount,
                        desires_duck,
                        walking,
                        life_state,
                        writes: touched,
                    });
                }
            }
            current = Some(write.tick);
            touched = 0;
            touched_z = false;
        }

        while let Some(create) = body_yaw_creates.get(next_body_yaw_create) {
            if create.tick > write.tick {
                break;
            }
            body_yaw = create.value;
            body_yaw_provenance = create.value.map(|_| "instancebaseline");
            next_body_yaw_create += 1;
        }

        let bare = write.name.rsplit('.').next().unwrap_or(write.name.as_str());
        // Values arrive already formatted for a human, as `U32(90)` or `F32(611.2)`; take what
        // the parentheses hold. Anything that is not a lone scalar simply does not parse.
        let number = write
            .value
            .split_once('(')
            .and_then(|(_, rest)| rest.strip_suffix(')'))
            .and_then(|inner| inner.parse::<f64>().ok());

        let vector = || -> Option<Vec<f32>> {
            let inner = write.value.strip_prefix("VecXYZ([")?.strip_suffix("])")?;
            inner
                .split(',')
                .map(|part| part.trim().parse::<f32>().ok())
                .collect()
        };
        let body = write.name.contains(".CBodyComponentBaseAnimGraph.");
        if write.name == "<vector length>" && recipe_path.as_deref() == Some(write.path.as_slice())
        {
            if let Some(size) = number.and_then(|value| usize::try_from(value as u64).ok()) {
                if size <= bytes.len() {
                    payload_len = Some(size);
                    bytes[size..].fill(None);
                }
            }
        }
        match bare {
            TOPOLOGY => {
                let decoded = write
                    .value
                    .strip_prefix("Bytes(")
                    .and_then(|value| value.strip_suffix(')'))
                    .filter(|hex| hex.len() % 2 == 0)
                    .and_then(|hex| {
                        (0..hex.len())
                            .step_by(2)
                            .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).ok())
                            .collect::<Option<Vec<_>>>()
                    })
                    .or_else(|| {
                        write
                            .value
                            .strip_prefix("Binary([")
                            .and_then(|value| value.strip_suffix("])"))
                            .and_then(|values| {
                                values
                                    .split(',')
                                    .map(|value| value.trim().parse::<u8>().ok())
                                    .collect::<Option<Vec<_>>>()
                            })
                    });
                if let (Some(&slot_index), Some(decoded)) =
                    (write.path.get(write.path.len().saturating_sub(2)), decoded)
                {
                    if slot_index >= 0 {
                        topologies.insert(slot_index as u32, decoded);
                    }
                }
            }
            RECIPE => {
                // The element index is the last step of the field path.
                if let (Some(&index), Some(value)) = (write.path.last(), number) {
                    if index >= 0 && (index as usize) < bytes.len() {
                        bytes[index as usize] = Some(value as u8);
                        touched += 1;
                    }
                }
            }
            SLOT => slot = number.map(|v| v as u32),
            "m_vecX" if body => {
                if let Some(value) = number {
                    position.0 = value as f32;
                    seen_x = true;
                }
            }
            "m_vecY" if body => {
                if let Some(value) = number {
                    position.1 = value as f32;
                    seen_y = true;
                }
            }
            "m_vecZ" if body => {
                if let Some(value) = number {
                    position.2 = value as f32;
                    seen_z = true;
                    touched_z = true;
                }
            }
            "m_angRotation" if body => {
                body_yaw = vector().and_then(|v| v.get(1).copied());
                body_yaw_provenance = body_yaw.map(|_| "packet");
            }
            "m_angEyeAngles" => {
                if let Some(v) = vector() {
                    eye_pitch = v.first().copied();
                    eye_yaw = v.get(1).copied();
                }
            }
            "m_aimPunchAngle" | "m_predictableBaseAngle" => {
                if let Some(v) = vector() {
                    aim_punch_angle = v.first().copied().zip(v.get(1).copied());
                }
            }
            "m_aimPunchAngleVel" | "m_predictableBaseAngleVel" => {
                if let Some(v) = vector() {
                    aim_punch_velocity = v.first().copied().zip(v.get(1).copied());
                }
            }
            "m_aimPunchTickBase" | "m_predictableBaseTick" => {
                aim_punch_tick = number.map(|v| v as i32);
            }
            "m_aimPunchTickFraction" | "m_predictableBaseTickInterpAmount" => {
                aim_punch_fraction = number.map(|v| v as f32);
            }
            "m_vecCsViewPunchAngle" => {
                if let Some(v) = vector() {
                    view_punch_angle = v.first().copied().zip(v.get(1).copied());
                }
            }
            "m_hActiveWeapon" => active_weapon_handle = number.map(|v| v as u32),
            "m_fFlags" => flags = number.map(|v| v as u32),
            "m_hGroundEntity" => ground_handle = number.map(|v| v as u32),
            "m_flDuckAmount" if write.name.contains(".CCSPlayer_MovementServices.") => {
                duck_amount = number.map(|v| v as f32);
            }
            "m_bDesiresDuck" => {
                desires_duck = match write.value.as_str() {
                    "Bool(true)" => Some(true),
                    "Bool(false)" => Some(false),
                    _ => desires_duck,
                }
            }
            "m_bIsWalking" => {
                walking = match write.value.as_str() {
                    "Bool(true)" => Some(true),
                    "Bool(false)" => Some(false),
                    _ => walking,
                }
            }
            "m_lifeState" => life_state = number.map(|v| v as u32),
            _ => {}
        }
    }

    if let Some(tick) = current {
        if tick >= from_tick {
            samples.push(PoseSample {
                tick,
                slot,
                topology: slot.and_then(|active| topologies.get(&active).cloned()),
                bytes,
                payload_len,
                position,
                position_valid: seen_x && seen_y,
                position_z_valid: seen_z,
                position_z_written: touched_z,
                body_yaw,
                body_yaw_provenance,
                eye_pitch,
                eye_yaw,
                aim_punch_angle,
                aim_punch_velocity,
                aim_punch_tick,
                aim_punch_fraction,
                view_punch_angle,
                active_weapon_handle,
                flags,
                ground_handle,
                duck_amount,
                desires_duck,
                walking,
                life_state,
                writes: touched,
            });
        }
    }
    Ok(samples)
}

pub struct BoundaryBuilder {
    classes: Vec<Class>,
    qf: QfMapper,
    huf: Vec<(u8, u8)>,
    checkpoints: BTreeMap<usize, (Tables, Vec<NetMessage>)>,
}
impl BoundaryBuilder {
    /// Add entities that are live in the sequential stream but absent from the DEM_FullPacket
    /// checkpoint at `checkpoint_tick`. A seek rebuilds from that checkpoint, so such entities
    /// (e.g. created only in a tick-0 sign-on packet) are otherwise missing after a seek, and a
    /// later delta for one crashes the client. The checkpoint's entity message is re-encoded
    /// from decoded values (generic path opcodes, so bits differ) and decoded back to prove every
    /// entity's values are unchanged. Returns (entity, class, serial) per inserted entity.
    pub fn insert_checkpoint_entities(
        &self,
        demo: &[u8],
        idx: &index::DemoIndex,
        checkpoint_tick: i32,
        ids: &[i32],
    ) -> Result<(Vec<u8>, Vec<(i32, u32, u32)>)> {
        self.materialise_entities(demo, idx, checkpoint_tick, ids, false)
    }

    /// As `insert_checkpoint_entities`, but for the first ordinary DEM_Packet at `tick`, whose
    /// entity message must be a non-delta (full) update.
    pub fn materialise_packet_entities(
        &self,
        demo: &[u8],
        idx: &index::DemoIndex,
        tick: i32,
        ids: &[i32],
    ) -> Result<(Vec<u8>, Vec<(i32, u32, u32)>)> {
        self.materialise_entities(demo, idx, tick, ids, true)
    }

    fn materialise_entities(
        &self,
        demo: &[u8],
        idx: &index::DemoIndex,
        checkpoint_tick: i32,
        ids: &[i32],
        ordinary: bool,
    ) -> Result<(Vec<u8>, Vec<(i32, u32, u32)>)> {
        let target = if ordinary {
            idx.frames
                .iter()
                .position(|f| f.cmd == CMD_PACKET && f.tick() == checkpoint_tick)
                .with_context(|| format!("no DEM_Packet at tick {checkpoint_tick}"))?
        } else {
            *idx.full_packets
                .iter()
                .find(|&&i| idx.frames[i].tick() == checkpoint_tick)
                .with_context(|| format!("no DEM_FullPacket at tick {checkpoint_tick}"))?
        };
        let new_state = || State {
            non_transmitted: BTreeSet::new(),
            entities: BTreeMap::new(),
            classes: &self.classes,
            qf: &self.qf,
            huf: &self.huf,
            baselines: BTreeMap::new(),
            baseline_entries: Vec::new(),
            alternate_baselines: BTreeMap::new(),
            template: None,
        };
        let mut tables = Tables::default();
        let mut sequential = new_state();
        // Entity state from sign-on packets only: the checkpoint's entity message is applied on
        // top of it (it updates sign-on entities without re-creating them).
        let mut signon = new_state();
        for frame in &idx.frames[..target] {
            match frame.cmd {
                CMD_STRING_TABLES => {
                    tables.overlay(CDemoStringTables::decode(payload(demo, frame)?.as_slice())?);
                    sequential.tables(&tables.snapshot);
                    signon.tables(&tables.snapshot);
                }
                CMD_FULL_PACKET => {
                    let full = CDemoFullPacket::decode(payload(demo, frame)?.as_slice())?;
                    if let Some(snapshot) = full.string_table {
                        tables.overlay(snapshot);
                        sequential.tables(&tables.snapshot);
                    }
                }
                CMD_SIGNON_PACKET | CMD_PACKET => {
                    let packet = CDemoPacket::decode(payload(demo, frame)?.as_slice())?;
                    for message in read_messages(packet.data())? {
                        match message.msg_type {
                            44 | 45 | 51 => {
                                tables.message(&message)?;
                                sequential.tables(&tables.snapshot);
                                signon.tables(&tables.snapshot);
                            }
                            55 => {
                                sequential
                                    .packet(&message.payload)
                                    .with_context(|| format!("sequential tick {}", frame.tick()))?;
                                if frame.cmd == CMD_SIGNON_PACKET {
                                    signon
                                        .packet(&message.payload)
                                        .context("sign-on entities")?;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        let frame = &idx.frames[target];
        let full = if ordinary {
            CDemoFullPacket {
                string_table: None,
                packet: Some(CDemoPacket::decode(payload(demo, frame)?.as_slice())?),
            }
        } else {
            CDemoFullPacket::decode(payload(demo, frame)?.as_slice())?
        };
        let mut checkpoint_tables = tables.clone();
        if let Some(snapshot) = full.string_table.clone() {
            checkpoint_tables.overlay(snapshot);
        }
        // The checkpoint's entity message updates entities it never creates: the client applies
        // it over its live entity list. Materialise it over the sequential state instead, so
        // the rewritten checkpoint is a self-contained snapshot.
        drop(signon);
        let mut checkpoint = sequential;
        checkpoint.tables(&checkpoint_tables.snapshot);
        let mut messages = read_messages(full.packet.clone().context("empty checkpoint")?.data())?;
        let at = messages
            .iter()
            .position(|m| m.msg_type == 55)
            .context("checkpoint has no entity message")?;
        let original = CsvcMsgPacketEntities::decode(messages[at].payload.as_slice())?;
        ensure!(
            !ordinary || !original.legacy_is_delta(),
            "the DEM_Packet entity message is a delta; only a full update can be materialised"
        );
        ensure!(
            original.alternate_baselines.is_empty(),
            "checkpoint carries alternate baselines; re-encoding would drop them"
        );
        eprintln!("checkpoint entity message: is_delta={} delta_from={} updated_entries={} sign-on entities={}",
            original.legacy_is_delta(), original.delta_from(), original.updated_entries(), checkpoint.entities.len());
        checkpoint
            .packet(&messages[at].payload)
            .context("checkpoint entities")?;
        let mut inserted = Vec::new();
        for &id in ids {
            let entity = checkpoint
                .entities
                .get(&id)
                .with_context(|| format!("entity {id} is not alive at the checkpoint"))?;
            inserted.push((id, entity.class, entity.serial));
        }
        eprintln!(
            "materialised checkpoint: {} entities (message listed {})",
            checkpoint.entities.len(),
            original.updated_entries()
        );
        let encoded = checkpoint.encode()?;
        let mut decoded = new_state();
        decoded.baselines = checkpoint.baselines.clone();
        decoded.baseline_entries = checkpoint.baseline_entries.clone();
        decoded.packet(&encoded)?;
        ensure!(
            decoded.entities.len() == checkpoint.entities.len(),
            "re-encoded checkpoint lost entities"
        );
        ensure!(
            decoded.non_transmitted == checkpoint.non_transmitted,
            "re-encoded checkpoint changed the non-transmitted set"
        );
        for (id, entity) in &checkpoint.entities {
            let other = decoded
                .entities
                .get(id)
                .with_context(|| format!("entity {id} missing after re-encode"))?;
            ensure!(
                other.class == entity.class && other.serial == entity.serial,
                "entity {id} identity changed"
            );
            ensure!(
                other.values.len() == entity.values.len(),
                "entity {id} field count changed"
            );
            for (key, bits) in &entity.values {
                let o = other
                    .values
                    .get(key)
                    .with_context(|| format!("entity {id} lost field {key:?}"))?;
                ensure!(
                    o.len == bits.len && o.bytes == bits.bytes,
                    "entity {id} field {key:?} changed"
                );
            }
        }
        messages[at].payload = encoded;
        let packet = CDemoPacket {
            data: Some(write_messages(&messages).into()),
        };
        let (command, rebuilt) = if ordinary {
            (CMD_PACKET, packet.encode_to_vec())
        } else {
            (
                CMD_FULL_PACKET,
                CDemoFullPacket {
                    string_table: full.string_table,
                    packet: Some(packet),
                }
                .encode_to_vec(),
            )
        };
        let mut out = demo[..16].to_vec();
        let mut offsets = BTreeMap::new();
        for f in &idx.frames {
            offsets.insert(f.frame_offset, out.len() as u64);
            if f.index == target {
                out.extend_from_slice(&frame_header(
                    command,
                    false,
                    f.tick_raw,
                    rebuilt.len() as u32,
                ));
                out.extend_from_slice(&rebuilt);
            } else {
                out.extend_from_slice(f.bytes(demo));
            }
        }
        for at in [8usize, 12] {
            let old = u32::from_le_bytes(demo[at..at + 4].try_into()?) as u64;
            if old != 0 {
                let new = *offsets
                    .get(&old)
                    .context("header offset does not start a frame")?;
                out[at..at + 4].copy_from_slice(&u32::try_from(new)?.to_le_bytes());
            }
        }
        Ok((out, inserted))
    }

    pub fn new(demo: &[u8], idx: &index::DemoIndex) -> Result<Self> {
        let huf = create_huffman_lookup_table().to_vec();
        let settings = ParserInputs {
            real_name_to_og_name: Default::default(),
            wanted_players: vec![],
            wanted_player_props: vec![],
            wanted_other_props: vec![],
            wanted_prop_states: Default::default(),
            wanted_ticks: vec![],
            wanted_events: vec![],
            parse_ents: true,
            parse_projectiles: false,
            parse_grenades: false,
            only_header: false,
            only_convars: false,
            huffman_lookup_table: &huf,
            order_by_steamid: false,
            list_props: false,
            fallback_bytes: None,
        };
        let mut first = FirstPassParser::new(&settings);
        let schema = first.parse_demo(demo, true)?;
        let mut tables = Tables::default();
        let first_full = *idx
            .full_packets
            .first()
            .context("source has no checkpoint")?;
        let mut checkpoints = BTreeMap::new();
        let mut stateful = Vec::new();
        for frame in &idx.frames {
            if frame.cmd == CMD_STRING_TABLES {
                tables.overlay(CDemoStringTables::decode(payload(demo, frame)?.as_slice())?);
            }
            if frame.cmd == CMD_FULL_PACKET {
                let raw = payload(demo, frame)?;
                let full = CDemoFullPacket::decode(raw.as_slice())?;
                if let Some(snapshot) = full.string_table {
                    tables.overlay(snapshot);
                }
                checkpoints.insert(frame.index, (tables.clone(), stateful.clone()));
            }
            if matches!(frame.cmd, CMD_SIGNON_PACKET | CMD_PACKET) {
                let raw = payload(demo, frame)?;
                let packet = CDemoPacket::decode(raw.as_slice())?;
                for message in read_messages(packet.data())? {
                    tables.message(&message)?;
                    if frame.index >= first_full
                        && matches!(
                            message.msg_type,
                            6 | 8 | 9 | 11 | 12 | 13 | 43 | 46 | 50 | 53 | 54 | 63 | 75
                        )
                    {
                        stateful.push(message);
                    }
                }
            }
        }
        Ok(Self {
            classes: schema.cls_by_id.to_vec(),
            qf: schema.qfmap.clone(),
            huf,
            checkpoints,
        })
    }

    /// Returns source-timeline frames for the ordinary writer to verify and rebase.
    pub fn prepare(
        &self,
        demo: &[u8],
        idx: &index::DemoIndex,
        start: i32,
        end: i32,
        policies: plan::Policies,
    ) -> Result<Vec<u8>> {
        ensure!(
            policies.include_startup && policies.sync_string_tables,
            "exact-boundary playback requires startup and string tables"
        );
        let selected = plan::plan_trim(idx, start, end, policies)?;
        // The opening source checkpoint already has its complete original startup.
        if selected.checkpoint_tick >= start {
            return Ok(demo.to_vec());
        }
        let huf = &self.huf;
        let (mut table_state, mut preserved) = self
            .checkpoints
            .get(&selected.checkpoint_frame)
            .context("missing checkpoint state")?
            .clone();
        let mut state = State {
            non_transmitted: BTreeSet::new(),
            entities: BTreeMap::new(),
            classes: &self.classes,
            qf: &self.qf,
            huf: &huf,
            baselines: BTreeMap::new(),
            baseline_entries: Vec::new(),
            alternate_baselines: BTreeMap::new(),
            template: None,
        };
        state.tables(&table_state.snapshot);
        let mut last_tick_message = None;
        for frame in &idx.frames[selected.checkpoint_frame..=selected.body_last_frame] {
            if frame.tick() >= start {
                break;
            }
            if !matches!(frame.cmd, CMD_PACKET | CMD_FULL_PACKET) {
                continue;
            }
            let raw = payload(&demo, frame)?;
            let packet = if frame.cmd == CMD_FULL_PACKET {
                let full = CDemoFullPacket::decode(raw.as_slice())?;
                if let Some(tables) = full.string_table {
                    table_state.overlay(tables);
                    state.tables(&table_state.snapshot);
                }
                full.packet.context("empty checkpoint")?
            } else {
                CDemoPacket::decode(raw.as_slice())?
            };
            for message in read_messages(packet.data())? {
                match message.msg_type {
                    76 => {
                        preserved.push(message);
                    }
                    55 => state
                        .packet(&message.payload)
                        .with_context(|| format!("source tick {}", frame.tick()))?,
                    4 => last_tick_message = Some(message),
                    // Preserve stateful network channels; transient events/audio are excluded.
                    44 | 45 | 51 => {
                        table_state.message(&message)?;
                        state.tables(&table_state.snapshot);
                        preserved.push(message);
                    }
                    6 | 8 | 9 | 11 | 12 | 13 | 43 | 46 | 50 | 53 | 54 | 63 | 75 => {
                        preserved.push(message)
                    }
                    _ => {}
                }
            }
        }
        let encoded = state.encode()?;
        // Verify the raw-field checkpoint decodes to exactly the same wire values.
        let mut roundtrip = State {
            non_transmitted: BTreeSet::new(),
            entities: BTreeMap::new(),
            classes: &self.classes,
            qf: &self.qf,
            huf: &huf,
            baselines: state.baselines.clone(),
            baseline_entries: state.baseline_entries.clone(),
            alternate_baselines: BTreeMap::new(),
            template: None,
        };
        roundtrip.packet(&encoded)?;
        ensure!(
            state.non_transmitted == roundtrip.non_transmitted,
            "snapshot changed non-transmitted entities"
        );
        ensure!(
            state.entities.len() == roundtrip.entities.len(),
            "snapshot lost entities"
        );
        for (id, entity) in &state.entities {
            let decoded = &roundtrip.entities[id];
            ensure!(
                entity.values.len() == decoded.values.len(),
                "entity {id} fields changed: expected {}, decoded {}, missing {:?}, extra {:?}",
                entity.values.len(),
                decoded.values.len(),
                entity
                    .values
                    .keys()
                    .filter(|k| !decoded.values.contains_key(*k))
                    .collect::<Vec<_>>(),
                decoded
                    .values
                    .keys()
                    .filter(|k| !entity.values.contains_key(*k))
                    .collect::<Vec<_>>()
            );
            for (key, bits) in &entity.values {
                let other = &decoded.values[key];
                ensure!(
                    bits.len == other.len && bits.bytes == other.bytes,
                    "entity {id} field {key:?} changed"
                );
            }
        }
        if let Some(tick) = last_tick_message.clone() {
            preserved.push(tick);
        }
        let mut commands_by_slot: BTreeMap<
            i32,
            (csgoproto::CMsgServerUserCmd, csgoproto::CsgoUserCmdPb),
        > = BTreeMap::new();
        for message in preserved.iter().filter(|m| m.msg_type == 76) {
            for mut cmd in
                csgoproto::CsvcMsgUserCommands::decode(message.payload.as_slice())?.commands
            {
                let slot = cmd.player_slot();
                let mut data = if !cmd.data().is_empty() {
                    csgoproto::CsgoUserCmdPb::decode(cmd.data())?
                } else {
                    commands_by_slot
                        .get(&slot)
                        .context("usercmd delta without seed")?
                        .1
                        .clone()
                };
                if !cmd.delta_data().is_empty() {
                    data=usercmd_delta::apply_delta(&data,cmd.delta_data()).with_context(||format!("unsupported usercmd delta slot {slot} cmd {} tick {} bytes {:02x?}",cmd.cmd_number(),cmd.server_tick_executed(),cmd.delta_data()))?;
                }
                cmd.data = Some(data.encode_to_vec().into());
                cmd.delta_data = None;
                commands_by_slot.insert(slot, (cmd, data));
            }
        }
        let commands = vec![NetMessage {
            msg_type: 76,
            payload: csgoproto::CsvcMsgUserCommands {
                commands: commands_by_slot.into_values().map(|(cmd, _)| cmd).collect(),
            }
            .encode_to_vec(),
        }];
        preserved.retain(|m| m.msg_type != 76);
        preserved.push(NetMessage {
            msg_type: 55,
            payload: encoded,
        });
        preserved.extend(commands.clone());
        let init_packet = CDemoPacket {
            data: Some(write_messages(&preserved).into()),
        };
        let full = CDemoFullPacket {
            string_table: Some(table_state.snapshot),
            packet: Some(init_packet.clone()),
        }
        .encode_to_vec();
        let mut intermediate = demo[..16].to_vec();
        for &i in &selected.selected[..selected.prefix_count()] {
            let f = &idx.frames[i];
            if f.cmd == CMD_PACKET {
                let raw = payload(&demo, f)?;
                let packet = CDemoPacket::decode(raw.as_slice())?;
                let mut messages = Vec::new();
                for m in read_messages(packet.data())? {
                    if m.msg_type == 55 {
                        messages.extend(preserved.clone());
                    } else if m.msg_type == 4 {
                        if let Some(tick) = last_tick_message.clone() {
                            messages.push(tick);
                        } else {
                            messages.push(m);
                        }
                    } else if m.msg_type != 76 {
                        messages.push(m);
                    }
                }
                let raw = CDemoPacket {
                    data: Some(write_messages(&messages).into()),
                }
                .encode_to_vec();
                intermediate.extend_from_slice(&frame_header(
                    CMD_PACKET,
                    false,
                    f.tick() as u32,
                    raw.len() as u32,
                ));
                intermediate.extend_from_slice(&raw);
            } else {
                intermediate.extend_from_slice(f.bytes(&demo));
            }
        }
        if policies.animation != plan::AnimationPolicy::Drop {
            let prefix: std::collections::HashSet<_> = selected.selected[..selected.prefix_count()]
                .iter()
                .copied()
                .collect();
            for f in &idx.frames[..=selected.body_last_frame] {
                if !prefix.contains(&f.index) && is_animation(f.cmd) && f.tick() < start {
                    intermediate.extend_from_slice(&frame_header(
                        f.cmd,
                        f.compressed,
                        (start - 2).max(0) as u32,
                        f.payload_len,
                    ));
                    intermediate.extend_from_slice(f.payload(demo));
                }
            }
        }
        intermediate.extend_from_slice(&frame_header(
            CMD_FULL_PACKET,
            false,
            (start - 1) as u32,
            full.len() as u32,
        ));
        intermediate.extend_from_slice(&full);
        let command_init = CDemoPacket {
            data: Some(write_messages(&commands).into()),
        }
        .encode_to_vec();
        intermediate.extend_from_slice(&frame_header(
            CMD_PACKET,
            false,
            (start - 1) as u32,
            command_init.len() as u32,
        ));
        intermediate.extend_from_slice(&command_init);

        let mut restore_pending = !state.alternate_baselines.is_empty();
        for &i in &selected.selected[selected.prefix_count()..] {
            let f = &idx.frames[i];
            if f.tick() >= start {
                if restore_pending && f.cmd == CMD_FULL_PACKET {
                    // A source checkpoint supplies its own complete assignments.
                    restore_pending = false;
                }
                if restore_pending && f.cmd == CMD_PACKET {
                    let raw = payload(demo, f)?;
                    let mut packet = CDemoPacket::decode(raw.as_slice())?;
                    let mut messages = read_messages(packet.data())?;
                    if let Some(message) = messages.iter_mut().find(|m| m.msg_type == 55) {
                        let mut entities =
                            CsvcMsgPacketEntities::decode(message.payload.as_slice())?;
                        restore_alternates(&mut entities, &state.alternate_baselines);
                        message.payload = entities.encode_to_vec();
                        packet.data = Some(write_messages(&messages).into());
                        let raw = packet.encode_to_vec();
                        let raw = if f.compressed {
                            snap::raw::Encoder::new().compress_vec(&raw)?
                        } else {
                            raw
                        };
                        intermediate.extend_from_slice(&frame_header(
                            CMD_PACKET,
                            f.compressed,
                            f.tick_raw,
                            raw.len() as u32,
                        ));
                        intermediate.extend_from_slice(&raw);
                        restore_pending = false;
                        continue;
                    }
                }
                intermediate.extend_from_slice(f.bytes(&demo));
            }
        }
        intermediate.extend_from_slice(&frame_header(CMD_STOP, false, end as u32, 0));
        let spawn = intermediate.len() as u32;
        if let Some(i) = idx.spawn_groups_trailer {
            intermediate.extend_from_slice(idx.frames[i].bytes(&demo));
        } else {
            intermediate.extend_from_slice(&frame_header(CMD_SPAWN_GROUPS, false, end as u32, 0));
        }
        let info = intermediate.len() as u32;
        intermediate
            .extend_from_slice(idx.frames[idx.file_info.context("no file info")?].bytes(&demo));
        ensure!(
            intermediate.len() <= u32::MAX as usize,
            "trim exceeds demo container limit"
        );
        intermediate[8..12].copy_from_slice(&info.to_le_bytes());
        intermediate[12..16].copy_from_slice(&spawn.to_le_bytes());
        Ok(intermediate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheduled_strings_roundtrip_through_the_actual_decoder() {
        for value in ["", "active_weapon", "weapon_é"] {
            let bits = encode_scheduled_string(value).unwrap();
            let mut reader = Bitreader::new(&bits.bytes);
            assert_eq!(reader.read_string().unwrap(), value);
            assert_eq!(bits.len, (value.len() + 1) * 8);
        }
    }

    #[test]
    fn scheduled_strings_roundtrip_without_byte_alignment() {
        let bits = encode_scheduled_string("active_weapon").unwrap();
        let mut writer = BitWriter::new();
        writer.write_nbits(5, 3);
        writer.write_bytes(&bits.bytes);
        writer.write_nbits(6, 3);
        let bytes = writer.finish();
        let mut reader = Bitreader::new(&bytes);
        assert_eq!(reader.read_nbits(3).unwrap(), 5);
        assert_eq!(reader.read_string().unwrap(), "active_weapon");
        assert_eq!(reader.read_nbits(3).unwrap(), 6);
    }

    #[test]
    fn scheduled_strings_reject_nul_and_excessive_byte_length() {
        assert!(encode_scheduled_string("active\0weapon").is_err());
        assert!(encode_scheduled_string(&"é".repeat(513)).is_err());
        assert!(encode_scheduled_string(&"a".repeat(1024)).is_ok());
    }

    fn audit_query(tick: i32, entity_id: i32, class_id: u32, serial: u32) -> EntityFieldAuditQuery {
        EntityFieldAuditQuery {
            tick,
            entity_id,
            class_id,
            serial,
            field_paths: vec![vec![1]],
        }
    }

    fn audit_entity(class: u32, serial: u32) -> Entity {
        Entity {
            class,
            serial,
            unknown: 0,
            values: BTreeMap::new(),
        }
    }

    #[test]
    fn entity_field_audit_includes_unchanged_resident_at_next_packet_tick() {
        let query = audit_query(34, 368, 73, 420);
        let mut entities = BTreeMap::new();
        entities.insert(368, audit_entity(73, 420));

        assert!(matching_entity_field_audits(&[query.clone()], &entities, 34, false).is_empty());
        let queries = [query];
        let matches = matching_entity_field_audits(&queries, &entities, 34, true);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].1.serial, 420);
    }

    #[test]
    fn entity_field_audit_rejects_deleted_and_reused_entity_indices() {
        let query = audit_query(34, 368, 73, 420);
        let entities = BTreeMap::new();
        assert!(matching_entity_field_audits(&[query.clone()], &entities, 34, true).is_empty());

        let mut entities = BTreeMap::new();
        entities.insert(368, audit_entity(73, 421));
        assert!(matching_entity_field_audits(&[query.clone()], &entities, 34, true).is_empty());
        entities.insert(368, audit_entity(74, 420));
        assert!(matching_entity_field_audits(&[query], &entities, 34, true).is_empty());
    }

    #[test]
    fn entity_field_audit_reads_values_after_final_packet_updates() {
        let query = audit_query(34, 368, 73, 420);
        let mut entities = BTreeMap::new();
        let mut current = audit_entity(73, 420);
        current.values.insert(
            vec![1],
            Bits {
                bytes: vec![0x2a],
                len: 8,
            },
        );
        entities.insert(368, current);

        let queries = [query];
        let matches = matching_entity_field_audits(&queries, &entities, 34, true);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].1.values.get(&vec![1]).unwrap().bytes, [0x2a]);
    }

    #[test]
    fn scheduled_u64_preserves_varint_boundaries_without_narrowing() {
        let qf = QfMapper {
            idx: 0,
            map: Default::default(),
        };
        for value in [
            0,
            1,
            127,
            128,
            u32::MAX as u64,
            u32::MAX as u64 + 1,
            1u64 << 63,
            u64::MAX,
        ] {
            let bits = encode_scheduled_u64(value, &Decoder::Unsigned64Decoder).unwrap();
            assert_eq!(
                decode_stored_u64(&bits, &Decoder::Unsigned64Decoder, &qf).unwrap(),
                value
            );
        }
    }

    #[test]
    fn scheduled_u64_rejects_other_schema_decoders() {
        for decoder in [
            Decoder::UnsignedDecoder,
            Decoder::BaseDecoder,
            Decoder::CentityHandleDecoder,
            Decoder::NoscaleDecoder,
        ] {
            assert!(encode_scheduled_u64(1, &decoder).is_err());
        }
    }

    #[test]
    fn scheduled_time_ticks_preserve_exact_varint_wire_values() {
        for (value, expected) in [
            (0, vec![0x00]),
            (23356, vec![0xbc, 0xb6, 0x01]),
            (114171, vec![0xfb, 0xfb, 0x06]),
            (u32::MAX, vec![0xff, 0xff, 0xff, 0xff, 0x0f]),
        ] {
            let bits =
                encode_scheduled_time_ticks(value, &Decoder::FloatSimulationTimeDecoder).unwrap();
            assert_eq!(bits.bytes, expected);
            assert_eq!(bits.len, expected.len() * 8);
            let mut reader = Bitreader::new(&bits.bytes);
            assert_eq!(reader.read_varint().unwrap(), value);
            assert_eq!(reader.bits_remaining().unwrap(), 0);
        }
    }

    #[test]
    fn scheduled_time_ticks_reject_non_time_schema_decoders() {
        for decoder in [
            Decoder::UnsignedDecoder,
            Decoder::Unsigned64Decoder,
            Decoder::BaseDecoder,
            Decoder::CentityHandleDecoder,
            Decoder::NoscaleDecoder,
            Decoder::SignedDecoder,
        ] {
            assert!(encode_scheduled_time_ticks(23356, &decoder).is_err());
        }
    }

    #[test]
    fn baseline_stored_model_handle_satisfies_only_its_exact_manifest_guard() {
        let baseline_model_handle = 0x00a1_b2c3_d4e5_u64;
        let encoded = encode_varint_u64(baseline_model_handle);
        let qf = QfMapper {
            idx: 0,
            map: Default::default(),
        };
        let decoded = decode_stored_u64(&encoded, &Decoder::Unsigned64Decoder, &qf).unwrap();
        assert_eq!(decoded, baseline_model_handle);
        assert!(model_handle_matches_guard(
            Some(decoded),
            baseline_model_handle
        ));
        assert!(!model_handle_matches_guard(
            Some(decoded),
            baseline_model_handle ^ 1
        ));
    }

    #[test]
    fn create_schedule_matches_every_exact_copy_but_never_an_absent_or_reused_serial() {
        let row = ScheduledFieldWrite {
            tick: 1550,
            entity_id: 341,
            class_id: 195,
            serial: 632,
            field_path: vec![101],
            value: ScheduledScalar::U32(11),
        };
        // Ordinary packet and FullPacket checkpoint entries are two views of the
        // same extant create; each must qualify independently.
        assert!(scheduled_create_copy_matches(&row, 1550, 341, 195, 632));
        assert!(scheduled_create_copy_matches(&row, 1550, 341, 195, 632));
        // A numerically reused entity index is a different lifetime, and an absent
        // entity has no create entry to match at all.
        assert!(!scheduled_create_copy_matches(&row, 1550, 341, 195, 633));
        assert!(!scheduled_create_copy_matches(&row, 1550, 341, 196, 632));
        assert!(!scheduled_create_copy_matches(&row, 1551, 341, 195, 632));
        assert!(!scheduled_create_copy_matches(&row, 1550, 342, 195, 632));
    }

    #[test]
    fn create_only_schedule_targets_create_copy_and_preserves_same_tick_delta() {
        // A same-tick ordinary delta may legitimately write the same field after
        // the create. Create-only repairs must land on extant create copies and
        // leave that delta to the original stream unchanged.
        assert!(scheduled_write_targets_entry(false, true, 2));
        assert!(!scheduled_write_targets_entry(false, true, 0));
        assert!(!scheduled_write_targets_entry(false, true, 1));
        // Existing modes retain their established delta and opt-in create rules.
        assert!(scheduled_write_targets_entry(false, false, 0));
        assert!(!scheduled_write_targets_entry(false, false, 2));
        assert!(scheduled_write_targets_entry(true, false, 2));
    }

    fn graft_pool_test_tick(
        inherited: Vec<(Vec<i32>, Bits)>,
        desired: &[Vec<u8>],
        previous: &mut Option<Vec<Vec<u8>>>,
        checkpoint: bool,
        cached: &mut BTreeMap<Vec<i32>, Bits>,
    ) -> Vec<(Vec<i32>, Bits)> {
        let mut kept = inherited;
        observe_slot_pool_input(previous, &kept, 12, 58, checkpoint);
        let mut writes = slot_pool_writes(
            desired,
            if checkpoint {
                None
            } else {
                previous.as_deref()
            },
            12,
            58,
        );
        // Same authored dynamic and active-slot writes as the slot-stream branch.
        writes.push((vec![12, 59], encode_varint(4)));
        writes.extend((0..4).map(|i| (vec![12, 59, i], encode_varint(42 + i as u32))));
        writes.push((vec![12, 60], encode_varint(1)));
        for (path, bits) in writes {
            if path == [12, 58] || path == [12, 59] {
                let len = if path[1] == 58 { desired.len() } else { 4 };
                prune_vector_delta(&mut kept, &path, len);
            }
            if let Some(old) = kept.iter_mut().find(|(p, _)| *p == path) {
                old.1 = bits;
            } else {
                kept.push((path, bits));
            }
        }
        kept.sort_by(|a, b| a.0.cmp(&b.0));
        // Decode the outgoing wire, not the desired table, into resident state.
        let huf = create_huffman_lookup_table().to_vec();
        let mut writer = BitWriter::new();
        encode_paths(&mut writer, kept.iter().map(|(p, _)| p), &huf).unwrap();
        for (_, value) in &kept {
            append_bits(&mut writer, value);
        }
        let wire = writer.finish();
        let mut reader = Bitreader::new(&wire);
        let fps = paths(&mut reader, &huf).unwrap();
        for fp in fps {
            let path = fp.path[..=fp.last].to_vec();
            let value = if path.starts_with(&[12, 58]) && path.len() == 4 {
                let len = reader.read_varint().unwrap() as usize;
                let bytes = (0..len)
                    .map(|_| reader.read_nbits(8).unwrap() as u8)
                    .collect::<Vec<_>>();
                encode_binary_block(&bytes)
            } else {
                let value = reader.read_varint().unwrap();
                if path == [12, 58] || path == [12, 59] {
                    cached.retain(|child, _| vector_child_in_bounds(child, &path, value as usize));
                }
                encode_varint(value)
            };
            cached.insert(path, value);
        }
        *previous = Some(desired.to_vec());
        kept
    }

    #[test]
    fn unchanged_scheduled_pool_overrides_inherited_old_graft_and_checkpoint() {
        let desired = vec![vec![0xb1, 0xb2], vec![0xb3]];
        for checkpoint in [false, true] {
            let mut previous = None;
            let mut cached = BTreeMap::new();
            graft_pool_test_tick(Vec::new(), &desired, &mut previous, false, &mut cached);
            // The second scheduled tick asks for the same B table, but the base
            // demo contains an old A pool delta (including an oversized tail).
            let old = vec![
                (vec![12, 58], encode_varint(3)),
                (vec![12, 58, 0, 0], encode_binary_block(&[0xa1])),
                (vec![12, 58, 1, 0], encode_binary_block(&[0xa2])),
                (vec![12, 58, 2, 0], encode_binary_block(&[0xa3])),
                (vec![12, 59], encode_varint(8)),
                (vec![12, 59, 7], encode_varint(99)),
                (vec![12, 60], encode_varint(2)),
                (vec![99], encode_varint(123)),
            ];
            let emitted =
                graft_pool_test_tick(old, &desired, &mut previous, checkpoint, &mut cached);
            assert!(cached[&vec![12, 58]] == encode_varint(2));
            assert_eq!(
                binary_block(&cached[&vec![12, 58, 0, 0]]),
                Some(desired[0].clone())
            );
            assert_eq!(
                binary_block(&cached[&vec![12, 58, 1, 0]]),
                Some(desired[1].clone())
            );
            assert!(!cached.contains_key(&vec![12, 58, 2, 0]));
            assert!(cached[&vec![12, 59]] == encode_varint(4));
            assert!(!cached.contains_key(&vec![12, 59, 7]));
            assert!(cached[&vec![12, 60]] == encode_varint(1));
            assert!(cached[&vec![99]] == encode_varint(123));
            assert!(emitted
                .iter()
                .any(|(p, b)| *p == [12, 58] && *b == encode_varint(2)));
            assert_eq!(previous, Some(desired.clone()));
        }
    }

    #[test]
    fn unscheduled_pool_write_invalidates_cache_without_editing_input() {
        let desired = vec![vec![0xb1], vec![0xb2]];
        let mut previous = Some(desired.clone());
        let inherited = vec![(vec![12, 58, 0, 0], encode_binary_block(&[0xa1]))];
        let before = inherited.clone();
        // Observation also runs on a selected pawn's unscheduled updates, but
        // authoring remains behind the existing exact tick/class/serial guard.
        observe_slot_pool_input(&mut previous, &inherited, 12, 58, false);
        assert!(inherited == before);
        assert_eq!(previous, None);
        let next = slot_pool_writes(&desired, previous.as_deref(), 12, 58);
        assert_eq!(next.len(), 3); // complete B pool, even with no new incoming delta
        let mut other = Some(desired.clone());
        observe_slot_pool_input(&mut other, &inherited, 9, 32, false);
        assert_eq!(other, Some(desired));
    }

    #[test]
    fn unscheduled_checkpoint_without_pool_delta_invalidates_cache() {
        let desired = vec![vec![0xb1], vec![0xb2]];
        let mut previous = Some(desired.clone());
        observe_slot_pool_input(&mut previous, &[], 12, 58, true);
        assert_eq!(previous, None);
        let next = slot_pool_writes(&desired, previous.as_deref(), 12, 58);
        assert_eq!(next.len(), 3);
    }

    #[test]
    fn pose_vector_shrink_prunes_same_packet_children_on_the_wire() {
        // The original update contains all 96 bytes. Replacing only its size and
        // first 64 values used to leave 32 illegal children in the same delta.
        let vector = vec![12, 59];
        let mut fields = vec![(vector.clone(), encode_varint(64))];
        fields.extend((0..96).map(|i| (vec![12, 59, i], encode_varint(i as u32))));
        fields.push((vec![12, 60], encode_varint(7)));
        prune_vector_delta(&mut fields, &vector, 64);
        let huf = create_huffman_lookup_table().to_vec();
        let mut writer = BitWriter::new();
        writer.write_u_bit_var(152);
        writer.write_nbits(0, 2); // entity update
        encode_paths(&mut writer, fields.iter().map(|(p, _)| p), &huf).unwrap();
        for (_, value) in &fields {
            append_bits(&mut writer, value);
        }
        let message = CsvcMsgPacketEntities {
            updated_entries: Some(1),
            entity_data: Some(writer.finish().into()),
            ..Default::default()
        };
        let encoded = message.encode_to_vec();
        let decoded = CsvcMsgPacketEntities::decode(encoded.as_slice()).unwrap();
        let mut reader = Bitreader::new(decoded.entity_data());
        assert_eq!(reader.read_u_bit_var().unwrap(), 152);
        assert_eq!(reader.read_nbits(2).unwrap(), 0);
        let decoded_paths = paths(&mut reader, &huf).unwrap();
        let mut length = None;
        let mut child_count = 0;
        for fp in decoded_paths {
            let path = &fp.path[..=fp.last];
            let value = reader.read_varint().unwrap();
            if path == vector {
                length = Some(value);
            } else if path.starts_with(&vector) {
                assert!(
                    (path[2] as u32) < length.unwrap(),
                    "native out-of-bounds child"
                );
                assert_eq!(path[2] as u32, value);
                child_count += 1;
            } else {
                assert_eq!(path, &[12, 60]);
                assert_eq!(value, 7);
            }
        }
        assert_eq!(length, Some(64));
        assert_eq!(child_count, 64);
    }

    #[test]
    fn topology_vector_shrink_removes_nested_children_but_keeps_siblings() {
        let mut fields = vec![
            (vec![12, 58], encode_varint(1)),
            (vec![12, 58, 0, 0], encode_binary_block(&[1, 2])),
            (vec![12, 58, 1, 0], encode_binary_block(&[3, 4])),
            (vec![12, 59, 95], encode_varint(17)),
        ];
        prune_vector_delta(&mut fields, &[12, 58], 1);
        assert_eq!(
            fields.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>(),
            vec![vec![12, 58], vec![12, 58, 0, 0], vec![12, 59, 95]]
        );
        prune_vector_delta(&mut fields, &[12, 58], 0);
        assert_eq!(fields.len(), 2);
        prune_vector_delta(&mut fields, &[12, 59], 128);
        assert_eq!(fields.len(), 2); // expansion must preserve existing values
    }

    #[test]
    fn empty_checkpoint_snapshot_preserves_signon_instancebaseline() {
        let classes = [];
        let qf = QfMapper {
            idx: 0,
            map: Default::default(),
        };
        let huf = [];
        let mut state = State {
            non_transmitted: BTreeSet::new(),
            entities: BTreeMap::new(),
            classes: &classes,
            qf: &qf,
            huf: &huf,
            baselines: BTreeMap::new(),
            baseline_entries: Vec::new(),
            alternate_baselines: BTreeMap::new(),
            template: None,
        };
        let table = |data: u8| CDemoStringTables {
            tables: vec![csgoproto::c_demo_string_tables::TableT {
                table_name: Some("instancebaseline".into()),
                items: vec![csgoproto::c_demo_string_tables::ItemsT {
                    str: Some("45".into()),
                    data: Some(vec![data].into()),
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        state.tables(&table(7));
        assert_eq!(state.baseline(421, 45).unwrap(), Some(&[7][..]));
        state.tables(&CDemoStringTables::default());
        assert_eq!(state.baseline(421, 45).unwrap(), Some(&[7][..]));
        state.tables(&table(9));
        assert_eq!(state.baseline(421, 45).unwrap(), Some(&[9][..]));
    }

    #[test]
    fn a_binary_block_round_trips_at_every_length_that_changes_the_varint() {
        // Lengths either side of the one and two byte varint boundary, plus a real topology blob,
        // because appending a task is precisely the case where the blob grows across that edge.
        let topology: &[u8] = &[
            0x15, 0x43, 0x08, 0x27, 0x88, 0x70, 0x06, 0x39, 0xa0, 0x84, 0x70, 0x0e, 0x42, 0x26,
            0x05, 0xa5, 0x56, 0x40, 0xac, 0x39, 0x57, 0x1e,
        ];
        let mut cases: Vec<Vec<u8>> = vec![
            vec![],
            vec![0xff],
            vec![7; 126],
            vec![7; 127],
            vec![7; 128],
            vec![7; 300],
            topology.to_vec(),
        ];
        // And one longer than any real blob, to be sure the length prefix is not assumed single byte.
        cases.push(vec![0xab; 1000]);

        for blob in cases {
            let bits = encode_binary_block(&blob);
            assert_eq!(
                bits.len,
                bits.bytes.len() * 8,
                "bit count disagrees with bytes"
            );
            assert_eq!(
                binary_block(&bits).as_deref(),
                Some(blob.as_slice()),
                "round trip failed at length {}",
                blob.len()
            );
        }
    }

    #[test]
    fn restoring_alternates_keeps_live_overrides_and_entity_data() {
        let mut packet = CsvcMsgPacketEntities {
            entity_data: Some(vec![1, 2, 3].into()),
            alternate_baselines: vec![csgoproto::csvc_msg_packet_entities::AlternateBaselineT {
                entity_index: Some(98),
                baseline_index: Some(42),
            }],
            ..Default::default()
        };
        restore_alternates(&mut packet, &BTreeMap::from([(98, 37), (203, 34)]));
        assert_eq!(packet.entity_data(), &[1, 2, 3]);
        assert_eq!(
            packet
                .alternate_baselines
                .iter()
                .map(|b| (b.entity_index(), b.baseline_index()))
                .collect::<Vec<_>>(),
            [(98, 42), (203, 34)]
        );
    }

    #[test]
    fn string_table_explicit_indices_are_absolute_and_values_can_omit_keys() {
        let mut tables = Tables::default();
        tables.formats.push(csgoproto::CsvcMsgCreateStringTable {
            name: Some("instancebaseline".into()),
            ..Default::default()
        });
        tables
            .snapshot
            .tables
            .push(csgoproto::c_demo_string_tables::TableT {
                table_name: Some("instancebaseline".into()),
                items: (0..4)
                    .map(|i| csgoproto::c_demo_string_tables::ItemsT {
                        str: Some(i.to_string()),
                        data: Some(vec![i as u8].into()),
                    })
                    .collect(),
                ..Default::default()
            });
        let mut bits = BitWriter::new();
        // Absolute index 3, followed by absolute index 1; each retains its key.
        for (index, value) in [(3, 99), (1, 42)] {
            bits.write_nbits(0, 1);
            bits.write_varint(index - 1);
            bits.write_nbits(0, 1);
            bits.write_nbits(1, 1);
            bits.write_nbits(1, 17);
            bits.write_nbits(value, 8);
        }
        let update = csgoproto::CsvcMsgUpdateStringTable {
            table_id: Some(0),
            num_changed_entries: Some(2),
            string_data: Some(bits.finish().into()),
        };
        tables
            .message(&NetMessage {
                msg_type: 45,
                payload: update.encode_to_vec(),
            })
            .unwrap();
        let items = &tables.snapshot.tables[0].items;
        assert_eq!(items.len(), 4);
        assert_eq!(items[3].str(), "3");
        assert_eq!(items[3].data(), &[99]);
        assert_eq!(items[1].str(), "1");
        assert_eq!(items[1].data(), &[42]);
        assert_eq!(items[2].data(), &[2]);
    }

    #[test]
    fn non_transmitted_delta_accumulates_and_snapshot_roundtrips() {
        let classes = [];
        let qf = QfMapper {
            idx: 0,
            map: Default::default(),
        };
        let huf = [];
        let mut state = State {
            non_transmitted: BTreeSet::new(),
            entities: BTreeMap::new(),
            classes: &classes,
            qf: &qf,
            huf: &huf,
            baselines: BTreeMap::new(),
            baseline_entries: Vec::new(),
            alternate_baselines: BTreeMap::new(),
            template: None,
        };
        state.baselines.insert(43, vec![10]);
        state.baseline_entries = vec![vec![20], vec![30]];
        state.alternate_baselines.insert(98, 1);
        assert_eq!(state.baseline(98, 43).unwrap(), Some([30].as_slice()));
        assert_eq!(state.baseline(99, 43).unwrap(), Some([10].as_slice()));
        for command in [1, 3] {
            let mut bits = BitWriter::new();
            bits.write_u_bit_var(98);
            bits.write_nbits(command, 2);
            state
                .packet(
                    &CsvcMsgPacketEntities {
                        updated_entries: Some(1),
                        entity_data: Some(bits.finish().into()),
                        ..Default::default()
                    }
                    .encode_to_vec(),
                )
                .unwrap();
            assert_eq!(state.alternate_baselines.contains_key(&98), command == 1);
        }
        for entries in [
            vec![(67, true), (501, true)],
            vec![(67, false), (400, true)],
        ] {
            let mut bits = BitWriter::new();
            let mut previous = -1;
            for &(id, value) in &entries {
                bits.write_u_bit_var((id - previous - 1) as u32);
                bits.write_nbits(value as u32, 1);
                previous = id;
            }
            let packet = CsvcMsgPacketEntities {
                updated_entries: Some(0),
                non_transmitted_entities: Some(
                    csgoproto::csvc_msg_packet_entities::NonTransmittedEntitiesT {
                        header_count: Some(entries.len() as i32),
                        data: Some(bits.finish().into()),
                    },
                ),
                ..Default::default()
            };
            state.packet(&packet.encode_to_vec()).unwrap();
        }
        assert_eq!(state.non_transmitted, BTreeSet::from([400, 501]));
        let encoded = state.encode().unwrap();
        state.non_transmitted.clear();
        state.packet(&encoded).unwrap();
        assert_eq!(state.non_transmitted, BTreeSet::from([400, 501]));
        assert_eq!(
            CsvcMsgPacketEntities::decode(encoded.as_slice())
                .unwrap()
                .delta_from,
            None
        );
    }
}

mod lights {
    use super::*;
    include!("light_extension.rs");
}
pub use lights::{light_inject, light_snapshot};
