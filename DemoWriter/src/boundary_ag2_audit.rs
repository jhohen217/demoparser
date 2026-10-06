//! Optional immutable-state observer. Does not change decoder merge decisions.
use super::*;
use serde_json::{json, Value};

#[derive(Clone, serde::Serialize)]
struct Origin {
    tick: i32,
    frame: usize,
    message: usize,
    command: i32,
    field: Option<usize>,
    checkpoint: bool,
    kind: &'static str,
    length: u32,
}
#[derive(Default)]
pub(super) struct Audit {
    pawns: BTreeSet<i32>,
    tick: i32,
    frame: usize,
    message: usize,
    checkpoint: bool,
    origins: BTreeMap<(i32, i32), Origin>,
    last_explicit_tick: BTreeMap<(i32, i32), i32>,
    counters: BTreeMap<i32, BTreeMap<String, usize>>,
    vector_counters: BTreeMap<i32, BTreeMap<i32, BTreeMap<String, usize>>>,
    lifetimes: Vec<Value>,
    inherited: Vec<Value>,
    issues: Vec<Value>,
}

fn cached_length(
    entity: &Entity,
    serializer: &Serializer,
    qf: &QfMapper,
    vector: i32,
) -> Result<Option<u32>> {
    let key = vec![12, vector];
    let Some(bits) = entity.values.get(&key) else {
        return Ok(None);
    };
    let mut fp = generate_fp();
    fp.path[0] = 12;
    fp.path[1] = vector;
    fp.last = 1;
    let field = find_field(&fp, serializer)?;
    ensure!(
        matches!(field, Field::Vector(_)),
        "AG2 parent is not a vector"
    );
    let value = Bitreader::new(&bits.bytes).decode(&get_decoder_from_field(field)?, qf)?;
    Ok(match value {
        Variant::U32(length) => Some(length),
        _ => anyhow::bail!("AG2 vector length is not unsigned"),
    })
}

impl Audit {
    fn count(&mut self, id: i32, key: &str) {
        *self
            .counters
            .entry(id)
            .or_default()
            .entry(key.into())
            .or_default() += 1;
    }
    fn count_vector(&mut self, id: i32, vector: i32, key: &str) {
        *self
            .vector_counters
            .entry(id)
            .or_default()
            .entry(vector)
            .or_default()
            .entry(key.into())
            .or_default() += 1;
    }
    fn origin(
        &self,
        command: i32,
        field: Option<usize>,
        kind: &'static str,
        length: u32,
    ) -> Origin {
        Origin {
            tick: self.tick,
            frame: self.frame,
            message: self.message,
            command,
            field,
            checkpoint: self.checkpoint,
            kind,
            length,
        }
    }
    pub(super) fn deleted(&mut self, id: i32, command: i32) {
        if !self.pawns.contains(&id) {
            return;
        }
        self.origins.retain(|(entity, _), _| *entity != id);
        self.count(id, "deletes");
        self.lifetimes.push(json!({"kind":"delete","entity":id,"tick":self.tick,
            "frame":self.frame,"message":self.message,"command":command,"checkpoint":self.checkpoint}));
    }
    pub(super) fn created(
        &mut self,
        id: i32,
        command: i32,
        entity: &Entity,
        serializer: &Serializer,
        qf: &QfMapper,
    ) -> Result<()> {
        if !self.pawns.contains(&id) {
            return Ok(());
        }
        ensure!(
            serializer.name == "CCSPlayerPawn",
            "selected entity {id} is {}, not CCSPlayerPawn",
            serializer.name
        );
        self.origins.retain(|(which, _), _| *which != id);
        let mut lengths = BTreeMap::new();
        for vector in [58, 59] {
            let length = cached_length(entity, serializer, qf, vector)?;
            lengths.insert(vector, length);
            if let Some(length) = length {
                self.origins.insert(
                    (id, vector),
                    self.origin(command, None, "baseline_at_create", length),
                );
            }
        }
        self.count(id, "creates");
        self.lifetimes.push(
            json!({"kind":"create","entity":id,"serial":entity.serial,"class_id":entity.class,
            "tick":self.tick,"frame":self.frame,"message":self.message,"command":command,
            "checkpoint":self.checkpoint,"baseline_lengths":lengths}),
        );
        Ok(())
    }
    pub(super) fn field(
        &mut self,
        id: i32,
        command: i32,
        ordinal: usize,
        entity: &Entity,
        serializer: &Serializer,
        qf: &QfMapper,
        path: &[i32],
        field: &Field,
        value: &Variant,
    ) -> Result<()> {
        if !self.pawns.contains(&id) {
            return Ok(());
        }
        if path == [12] && matches!((field, value), (Field::Pointer(_), Variant::Bool(false))) {
            self.origins.retain(|(which, _), _| *which != id);
            self.count(id, "component_pointer_clears");
        }
        if path.len() < 2 || path[0] != 12 || ![58, 59].contains(&path[1]) {
            return Ok(());
        }
        let vector = path[1];
        if path.len() == 2 {
            if let (Field::Vector(_), Variant::U32(length)) = (field, value) {
                self.origins.insert(
                    (id, vector),
                    self.origin(command, Some(ordinal), "packet_length_write", *length),
                );
                self.last_explicit_tick.insert((id, vector), self.tick);
                self.count(id, "explicit_lengths");
                self.count_vector(id, vector, "explicit_lengths");
            }
            return Ok(());
        }
        // Read the decoder's real pre-child state, not a tick-grouped shadow length.
        let length = cached_length(entity, serializer, qf, vector)?;
        let origin = self.origins.get(&(id, vector)).cloned();
        let provenance_consistent = match (&origin, length) {
            (Some(origin), Some(length)) => origin.length == length,
            (None, None) => true,
            _ => false,
        };
        let outcome = if !provenance_consistent || length.is_none() {
            "unknown"
        } else if path[2] < 0 || path[2] as u32 >= length.unwrap() {
            "violation"
        } else {
            "checked"
        };
        let inherited = self.last_explicit_tick.get(&(id, vector)) != Some(&self.tick);
        self.count(id, &format!("{outcome}_children"));
        self.count_vector(id, vector, &format!("{outcome}_children"));
        if inherited {
            self.count(id, &format!("{outcome}_inherited_children"));
            self.count_vector(id, vector, &format!("{outcome}_inherited_children"));
        }
        if inherited || outcome != "checked" {
            let row = json!({"entity":id,"serial":entity.serial,"class_id":entity.class,"tick":self.tick,
                "frame":self.frame,"message":self.message,"command":command,"field":ordinal,
                "checkpoint":self.checkpoint,"path":path,"cached_length":length,"length_origin":origin,
                "provenance_consistent":provenance_consistent,"outcome":outcome,"inherited_by_original_tick_census":inherited});
            if inherited {
                self.inherited.push(row.clone());
            }
            if outcome != "checked" {
                self.issues.push(row);
            }
        }
        Ok(())
    }
}

pub fn audit_cached_ag2_bounds(demo: &[u8], pawns: &[i32]) -> Result<Value> {
    let idx = index::DemoIndex::build(demo)?;
    let builder = BoundaryBuilder::new(demo, &idx)?;
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
    let mut audit = Audit {
        pawns: pawns.iter().copied().collect(),
        ..Default::default()
    };
    let mut fields = Vec::new();
    let mut events = Vec::new();
    let mut tables = Tables::default();
    let mut alternate_assignments = 0usize;
    for frame in &idx.frames {
        audit.tick = frame.tick();
        audit.frame = frame.index;
        audit.checkpoint = frame.cmd == CMD_FULL_PACKET;
        let packet = match frame.cmd {
            CMD_STRING_TABLES => {
                tables.overlay(CDemoStringTables::decode(payload(demo, frame)?.as_slice())?);
                state.tables(&tables.snapshot);
                None
            }
            CMD_FULL_PACKET => {
                let full = CDemoFullPacket::decode(payload(demo, frame)?.as_slice())?;
                if let Some(snapshot) = full.string_table {
                    tables.overlay(snapshot);
                    state.tables(&tables.snapshot);
                }
                full.packet
            }
            CMD_SIGNON_PACKET | CMD_PACKET => {
                Some(CDemoPacket::decode(payload(demo, frame)?.as_slice())?)
            }
            _ => None,
        };
        if let Some(packet) = packet {
            for (ordinal, message) in read_messages(packet.data())?.into_iter().enumerate() {
                if matches!(message.msg_type, 44 | 45 | 51) {
                    tables.message(&message)?;
                    if message.msg_type == 51 {
                        state.baselines.clear();
                        state.baseline_entries.clear();
                    }
                    state.tables(&tables.snapshot);
                }
                if message.msg_type != 55 {
                    continue;
                }
                audit.message = ordinal;
                let msg = CsvcMsgPacketEntities::decode(message.payload.as_slice())?;
                ensure!(
                    msg.outofpvs_entity_updates
                        .as_ref()
                        .is_none_or(|x| x.count() == 0),
                    "unsupported extended entity encoding"
                );
                // Match State::packet's persistent alternate-baseline assignments.
                // This preparation is local to the audit; ordinary observe callers
                // and their established behavior are not changed.
                for alternate in &msg.alternate_baselines {
                    ensure!(
                        (0..32768).contains(&alternate.entity_index())
                            && alternate.baseline_index() >= -1,
                        "invalid alternate baseline"
                    );
                    alternate_assignments += 1;
                    if alternate.baseline_index() == -1 {
                        state.alternate_baselines.remove(&alternate.entity_index());
                    } else {
                        state
                            .alternate_baselines
                            .insert(alternate.entity_index(), alternate.baseline_index());
                    }
                }
                state.observe_audited(
                    &msg,
                    frame.tick(),
                    -1,
                    i32::MIN,
                    i32::MAX,
                    Some(&[]),
                    &mut fields,
                    &mut events,
                    audit.checkpoint,
                    Some(&mut audit),
                )?;
                events.clear();
            }
        }
    }
    ensure!(
        fields.is_empty(),
        "audit accidentally retained field census"
    );
    Ok(
        json!({"schema":"ag2-cached-vector-bounds-audit.v1","scope":"Sequential packet child writes for selected pawns, using baseline-applied cached state at exact field order. Full-checkpoint creates replace state; baseline child writes themselves and independent native seek semantics are outside scope.",
        "all_packet_children_proven":audit.issues.is_empty(),"alternate_baseline_assignments":alternate_assignments,"pawns":pawns,"counts":audit.counters,"vector_counts":audit.vector_counters,
        "lifecycle":audit.lifetimes,"inherited_children":audit.inherited,"issues":audit.issues}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use parser::first_pass::sendtables::{PointerField, ValueField, VectorField};
    fn fixture() -> (Entity, Serializer, QfMapper, Audit) {
        let mut inner = vec![Field::None; 60];
        inner[58] = Field::Vector(VectorField {
            field_enum: Box::new(Field::Value(ValueField {
                decoder: Decoder::UnsignedDecoder,
                name: "byte".into(),
                full_name: "byte".into(),
                should_parse: true,
                prop_id: 0,
            })),
            decoder: Decoder::UnsignedDecoder,
        });
        inner[59] = inner[58].clone();
        let mut fields = vec![Field::None; 13];
        fields[12] = Field::Pointer(PointerField::new(&Serializer {
            name: "body".into(),
            fields: inner,
        }));
        (
            Entity {
                class: 41,
                serial: 9,
                unknown: 0,
                values: BTreeMap::new(),
            },
            Serializer {
                name: "CCSPlayerPawn".into(),
                fields,
            },
            QfMapper {
                idx: 0,
                map: Default::default(),
            },
            Audit {
                pawns: BTreeSet::from([95]),
                ..Default::default()
            },
        )
    }
    #[test]
    fn baseline_length_is_read_from_actual_cache_without_mutation() {
        let (mut e, s, q, mut a) = fixture();
        e.values.insert(vec![12, 58], encode_varint(32));
        let before = e.values.clone();
        a.created(95, 0, &e, &s, &q).unwrap();
        a.field(
            95,
            0,
            0,
            &e,
            &s,
            &q,
            &[12, 58, 17, 0],
            &Field::None,
            &Variant::U32(0),
        )
        .unwrap();
        assert!(a.issues.is_empty());
        assert_eq!(a.inherited[0]["cached_length"], 32);
        assert_eq!(
            a.inherited[0]["length_origin"]["kind"],
            "baseline_at_create"
        );
        assert!(e.values == before);
    }
    #[test]
    fn checkpoint_create_discards_previous_length_and_new_serial_does_not_inherit() {
        let (mut e, s, q, mut a) = fixture();
        e.values.insert(vec![12, 59], encode_varint(96));
        a.created(95, 0, &e, &s, &q).unwrap();
        e.values.clear();
        e.serial = 10;
        a.tick = 3841;
        a.checkpoint = true;
        a.created(95, 0, &e, &s, &q).unwrap();
        a.field(
            95,
            0,
            0,
            &e,
            &s,
            &q,
            &[12, 59, 64],
            &Field::None,
            &Variant::U32(0),
        )
        .unwrap();
        assert_eq!(a.issues[0]["outcome"], "unknown");
        assert!(a.issues[0]["length_origin"].is_null());
        a.deleted(95, 1);
        assert!(a.origins.is_empty());
    }
    #[test]
    fn same_tick_order_checks_actual_shrink_then_growth() {
        let (mut e, s, q, mut a) = fixture();
        let vector = &s.fields[12].get_inner(59).unwrap();
        for (ordinal, size, child, expected) in [
            (0, 96, 95, "checked"),
            (2, 64, 64, "violation"),
            (4, 96, 95, "checked"),
        ] {
            a.field(
                95,
                0,
                ordinal,
                &e,
                &s,
                &q,
                &[12, 59],
                vector,
                &Variant::U32(size),
            )
            .unwrap();
            e.values.insert(vec![12, 59], encode_varint(size));
            let old = a
                .counters
                .get(&95)
                .and_then(|c| c.get(&format!("{expected}_children")))
                .copied()
                .unwrap_or(0);
            a.field(
                95,
                0,
                ordinal + 1,
                &e,
                &s,
                &q,
                &[12, 59, child],
                &Field::None,
                &Variant::U32(0),
            )
            .unwrap();
            assert_eq!(a.counters[&95][&format!("{expected}_children")], old + 1);
        }
        assert_eq!(a.issues.len(), 1);
    }
    #[test]
    fn optional_observer_preserves_actual_decoder_state_across_packet_lifecycle() {
        let (_, serializer, qf, mut audit) = fixture();
        let classes = [
            Class {
                class_id: 0,
                name: "CCSPlayerPawn".into(),
                serializer: serializer.clone(),
            },
            Class {
                class_id: 1,
                name: "unused".into(),
                serializer,
            },
        ];
        let huf = create_huffman_lookup_table().to_vec();
        let make_state = || State {
            classes: &classes,
            qf: &qf,
            huf: &huf,
            entities: BTreeMap::new(),
            baselines: BTreeMap::new(),
            baseline_entries: Vec::new(),
            alternate_baselines: BTreeMap::new(),
            non_transmitted: BTreeSet::new(),
            template: None,
        };
        let mut plain = make_state();
        let mut watched = make_state();
        let baseline_fields = vec![(vec![12, 59], encode_varint(96))];
        let mut baseline = BitWriter::new();
        encode_paths(&mut baseline, baseline_fields.iter().map(|(p, _)| p), &huf).unwrap();
        for (_, v) in &baseline_fields {
            append_bits(&mut baseline, v);
        }
        let baseline = baseline.finish();
        plain.baselines.insert(0, baseline.clone());
        watched.baselines.insert(0, baseline);
        let cases = vec![
            (2, 9, false, vec![(vec![12, 59, 95], encode_varint(1))]),
            (
                0,
                9,
                false,
                vec![
                    (vec![12, 59], encode_varint(64)),
                    (vec![12, 59, 63], encode_varint(1)),
                ],
            ),
            (0, 9, false, vec![(vec![12, 59, 64], encode_varint(1))]),
            (2, 9, true, vec![(vec![12, 59, 95], encode_varint(1))]),
            (
                0,
                9,
                false,
                vec![(
                    vec![12],
                    Bits {
                        bytes: vec![0],
                        len: 1,
                    },
                )],
            ),
            (0, 9, false, vec![(vec![12, 59, 0], encode_varint(1))]),
            (3, 9, false, vec![]),
            (2, 10, false, vec![(vec![12, 59, 94], encode_varint(1))]),
        ];
        for (step, (command, serial, checkpoint, fields)) in cases.into_iter().enumerate() {
            let mut writer = BitWriter::new();
            writer.write_u_bit_var(95);
            writer.write_nbits(command, 2);
            if command == 2 {
                writer.write_nbits(0, 1);
                writer.write_nbits(serial, 17);
                writer.write_varint(0);
            }
            if command & 1 == 0 {
                encode_paths(&mut writer, fields.iter().map(|(p, _)| p), &huf).unwrap();
                for (_, value) in &fields {
                    append_bits(&mut writer, value);
                }
            }
            let packet = CsvcMsgPacketEntities {
                updated_entries: Some(1),
                entity_data: Some(writer.finish().into()),
                ..Default::default()
            };
            audit.tick = step as i32;
            audit.frame = step;
            audit.checkpoint = checkpoint;
            let (mut fields_a, mut fields_b, mut events_a, mut events_b) =
                (Vec::new(), Vec::new(), Vec::new(), Vec::new());
            plain
                .observe(
                    &packet,
                    step as i32,
                    -1,
                    i32::MIN,
                    i32::MAX,
                    Some(&[]),
                    &mut fields_a,
                    &mut events_a,
                    checkpoint,
                )
                .unwrap();
            watched
                .observe_audited(
                    &packet,
                    step as i32,
                    -1,
                    i32::MIN,
                    i32::MAX,
                    Some(&[]),
                    &mut fields_b,
                    &mut events_b,
                    checkpoint,
                    Some(&mut audit),
                )
                .unwrap();
            assert_eq!(plain.entities.len(), watched.entities.len());
            for (id, a) in &plain.entities {
                let b = &watched.entities[id];
                assert_eq!(
                    (a.class, a.serial, a.unknown),
                    (b.class, b.serial, b.unknown)
                );
                assert!(
                    a.values == b.values,
                    "observer changed state at packet {step}"
                );
            }
            assert_eq!(events_a.len(), events_b.len());
            assert!(fields_a.is_empty() && fields_b.is_empty());
        }
        assert_eq!(audit.counters[&95]["violation_children"], 1);
        assert_eq!(audit.counters[&95]["unknown_children"], 1);
        assert_eq!(audit.counters[&95]["component_pointer_clears"], 1);
        assert_eq!(audit.counters[&95]["creates"], 3);
        assert_eq!(audit.counters[&95]["deletes"], 1);
    }
}
