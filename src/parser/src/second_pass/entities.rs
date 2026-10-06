use crate::first_pass::prop_controller::is_grenade_or_weapon;
use crate::first_pass::prop_controller::ITEM_PURCHASE_DEF_IDX;
use crate::first_pass::prop_controller::{ECON_ATTRIBUTE_COUNT_ID, ECON_ATTRIBUTE_DEF_INDEX_BASE, ECON_ATTRIBUTE_RAW_VALUE_BASE, ECON_ATTRIBUTE_LEGACY_DEF_ID, ECON_ATTRIBUTE_LEGACY_DEF_SLOT_ID, FLATTENED_VEC_MAX_LEN};
use crate::first_pass::read_bits::Bitreader;
use crate::first_pass::read_bits::DemoParserError;
use crate::first_pass::sendtables::find_field;
use crate::first_pass::sendtables::get_decoder_from_field;
use crate::first_pass::sendtables::get_propinfo;
use crate::first_pass::sendtables::Field;
use crate::first_pass::sendtables::FieldInfo;
use crate::second_pass::game_events::GameEventInfo;
use crate::second_pass::other_netmessages::Class;
use crate::second_pass::parser_settings::SecondPassParser;
use crate::second_pass::path_ops::*;
use crate::second_pass::variants::Variant;
use ahash::AHashMap;
use csgoproto::CsvcMsgPacketEntities;
use prost::Message;

const NSERIALBITS: u32 = 17;
const STOP_READING_SYMBOL: u8 = 39;
const HUFFMAN_CODE_MAXLEN: u32 = 17;

#[derive(Debug, Clone)]
pub struct Entity {
    pub cls_id: u32,
    pub entity_id: i32,
    /// Source entity serial paired with the index to distinguish reused slots.
    pub serial: u32,
    pub props: AHashMap<u32, Variant>,
    pub entity_type: EntityType,
    /// Raw two-bit PVS transition code when the packet supplied one.
    pub pvs_state: Option<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlayerMetaData {
    pub player_entity_id: Option<i32>,
    pub steamid: Option<u64>,
    pub controller_entid: Option<i32>,
    pub name: Option<String>,
    pub team_num: Option<u32>,
}
#[derive(Debug, Clone, PartialEq)]
pub enum EntityType {
    PlayerController,
    Rules,
    Projectile,
    Team,
    Normal,
    C4,
}
enum EntityCmd {
    Delete,
    CreateAndUpdate,
    Update,
}

impl<'a> SecondPassParser<'a> {
    pub fn parse_packet_ents(&mut self, bytes: &[u8], is_fullpacket: bool) -> Result<(), DemoParserError> {
        if !self.parse_entities {
            return Ok(());
        }
        let msg = match CsvcMsgPacketEntities::decode(bytes) {
            Err(_) => return Err(DemoParserError::MalformedMessage),
            Ok(msg) => msg,
        };

        let mut bitreader = Bitreader::new(msg.entity_data());
        let mut entity_id: i32 = -1;
        let mut events_to_emit = vec![];
        for _ in 0..msg.updated_entries() {
            entity_id += 1 + (bitreader.read_u_bit_var()? as i32);
            // Read 2 bits to know which operation should be done to the entity.
            let raw_cmd = bitreader.read_nbits(2)?;
            let cmd = match raw_cmd {
                0b01 => EntityCmd::Delete,
                0b11 => EntityCmd::Delete,
                0b10 => EntityCmd::CreateAndUpdate,
                0b00 => EntityCmd::Update,
                _ => return Err(DemoParserError::ImpossibleCmd),
            };

            match cmd {
                EntityCmd::Delete => {
                    let transition = if raw_cmd == 0b11 { "delete" } else { "leave" };
                    let recipe_identity = self.entities.get(entity_id as usize)
                        .and_then(|entry| entry.as_ref())
                        .map(|entity| (entity.serial, entity.cls_id));
                    if raw_cmd == 0b11 {
                        if let Some((serial, _cls_id)) = recipe_identity {
                            self.ag2_recipes.end(entity_id, serial, self.tick);
                        }
                    }
                    self.capture_utility_entity(entity_id, Some(transition));
                    self.audit_lifecycle(entity_id, transition);
                    self.world_entity_lifecycle(entity_id, raw_cmd != 0b11);
                    self.projectiles.remove(&entity_id);
                    self.smoke_voxels.finish_entity(entity_id, self.tick);
                    self.infernos.remove(entity_id, self.tick);
                    if self.prop_controller.capture_weapon_entities {
                        let deleted = self
                            .entities
                            .get(entity_id as usize)
                            .and_then(|entry| entry.as_ref())
                            .cloned()
                            .and_then(|entity| self.snapshot_weapon_entity(&entity));
                        if let Some(mut snapshot) = deleted {
                            snapshot.present = false;
                            self.weapon_entity_snapshots.push(snapshot);
                        }
                    }
                    if let Some(entry) = self.entities.get_mut(entity_id as usize) {
                        *entry = None;
                    }
                }
                EntityCmd::CreateAndUpdate => {
                    self.create_new_entity(&mut bitreader, &entity_id, &mut events_to_emit, is_fullpacket)?;
                    self.update_entity(&mut bitreader, entity_id, false, &mut events_to_emit, is_fullpacket)?;
                    // Do not wait for the periodic collection callback: short-lived
                    // dropped weapons can otherwise be deleted before their create is seen.
                    if self.prop_controller.capture_weapon_entities {
                        let created = self
                            .entities
                            .get(entity_id as usize)
                            .and_then(|entry| entry.as_ref())
                            .and_then(|entity| self.snapshot_weapon_entity(entity));
                        if let Some(snapshot) = created {
                            self.weapon_entity_snapshots.push(snapshot);
                        }
                    }
                }
                EntityCmd::Update => {
                    let active_weapon_before = if self.prop_controller.capture_weapon_entities {
                        self.active_weapon_entity(entity_id)
                    } else {
                        None
                    };
                    if msg.has_pvs_vis_bits_deprecated() != 0 {
                        // Most entities pass trough here. Seems like entities that are not updated.
                        let pvs_state = bitreader.read_nbits(2)? as u8;
                        if let Some(Some(entity)) = self.entities.get_mut(entity_id as usize) {
                            entity.pvs_state = Some(pvs_state);
                        }
                        if pvs_state & 0x01 == 1 {
                            self.capture_utility_entity(entity_id, Some("dormant"));
                            self.audit_lifecycle(entity_id, "dormant");
                            self.world_entity_lifecycle(entity_id, true);
                            // Odd PVS codes leave visibility/dormancy. Emit immediately: waiting
                            // for the periodic weapon snapshot can miss a short leave/enter cycle.
                            if self.prop_controller.capture_weapon_entities {
                                let snapshot = self.entities.get(entity_id as usize)
                                    .and_then(|entry| entry.as_ref())
                                    .and_then(|entity| self.snapshot_weapon_entity(entity));
                                if let Some(snapshot) = snapshot {
                                    self.weapon_entity_snapshots.push(snapshot);
                                }
                            }
                            continue;
                        }
                    } else if let Some(Some(entity)) = self.entities.get_mut(entity_id as usize) {
                        // An ordinary entity update is authoritative evidence that it is back in
                        // the visible PVS; do not retain a preceding leave code indefinitely.
                        entity.pvs_state = Some(0);
                    }
                    self.update_entity(&mut bitreader, entity_id, false, &mut events_to_emit, is_fullpacket)?;
                    let active_weapon_after = if self.prop_controller.capture_weapon_entities {
                        self.active_weapon_entity(entity_id)
                    } else {
                        None
                    };
                    if active_weapon_before != active_weapon_after {
                        for weapon_id in [active_weapon_before, active_weapon_after]
                            .into_iter()
                            .flatten()
                        {
                            let snapshot = self.entities.get(weapon_id as usize)
                                .and_then(|entry| entry.as_ref())
                                .and_then(|weapon| self.snapshot_weapon_entity(weapon));
                            if let Some(snapshot) = snapshot {
                                self.weapon_entity_snapshots.push(snapshot);
                            }
                        }
                    }
                    // Every authoritative weapon update can change ownership, inventory state,
                    // ammo, cosmetics, or transform. Emit it immediately; the periodic full
                    // inventory walk is now needed only once to seed the requested range.
                    if self.prop_controller.capture_weapon_entities {
                        let snapshot = self.entities.get(entity_id as usize)
                            .and_then(|entry| entry.as_ref())
                            .and_then(|entity| self.snapshot_weapon_entity(entity));
                        if let Some(snapshot) = snapshot {
                            self.weapon_entity_snapshots.push(snapshot);
                        }
                    }
                }
            }
            if !matches!(cmd, EntityCmd::Delete) {
                self.capture_utility_entity(entity_id, None);
            }
        }
        if !events_to_emit.is_empty() {
            self.emit_events(events_to_emit)?;
        }
        Ok(())
    }

    pub fn update_entity(
        &mut self,
        bitreader: &mut Bitreader,
        entity_id: i32,
        is_baseline: bool,
        events_to_emit: &mut Vec<GameEventInfo>,
        is_fullpacket: bool,
    ) -> Result<(), DemoParserError> {
        let _pp = crate::second_pass::parser::prof_on().then(std::time::Instant::now);
        let n_updates = self.parse_paths(bitreader)?;
        if let Some(t) = _pp {
            crate::second_pass::parser::PROF_PATHS_NS.with(|c| c.set(c.get() + t.elapsed().as_nanos() as u64));
        }
        let _pd = crate::second_pass::parser::prof_on().then(std::time::Instant::now);
        let n_updated_values = self.decode_entity_update(bitreader, entity_id, n_updates, is_fullpacket, is_baseline, events_to_emit)?;
        if let Some(t) = _pd {
            crate::second_pass::parser::PROF_DECODE_NS.with(|c| c.set(c.get() + t.elapsed().as_nanos() as u64));
        }
        if n_updated_values > 0 {
            self.gather_extra_info(&entity_id, is_baseline)?;
        }
        Ok(())
    }
    pub fn parse_paths(&mut self, bitreader: &mut Bitreader) -> Result<usize, DemoParserError> {
        /*
        Create a field path by decoding using a Huffman tree.
        The huffman tree can be found at the bottom of entities_utils.rs

        A field path is a "path trough a struct" where
        the struct can have normal fields but also pointers
        to other (nested) structs.

        Example:

        The array will be filled with these:

        Struct Field{
            wanted_information: Option<T>,
            Pointer: bool,
            fields: Option<Vec<Field>>
        },

        (struct is simplified for this example. In reality it also includes field name etc.)


        Path to each of the fields in the below fields list: [
            [0], [1, 0], [1, 1], [2]
        ]
        and they would map to:
        [0] => FloatDecoder,
        [1, 0] => IntegerDecoder,
        [1, 1] => StringDecoder,
        [2] => VectorDecoder,

        fields = [
            Field{
                wanted_information: FloatDecoder,
                pointer: false,
                fields: None,
            },
            Field{
                wanted_information: None,
                pointer: true,
                fields: Some(
                    [
                        Field{
                            wanted_information: IntegerDecoder,
                            pointer: false,
                            fields: Some(
                        },
                        Field{
                            wanted_information: StringDecoder,
                            pointer: flase,
                            fields: Some(
                        }
                    ]
                ),
            },
            Field{
                wanted_information: VectorDecoder,
                pointer: false,
                fields: None,
            },
        ]
        Not sure what the maximum depth of these structs are, but others seem to use
        7 as the max length of field path so maybe that?

        Personally I find this path idea horribly complicated. Why is this chosen over
        the way it was done in source 1 demos?
        */

        // Create an "empty" path ([-1, 0, 0, 0, 0, 0, 0])
        // For performance reasons have them always the same len
        let mut fp = generate_fp();
        let mut idx = 0;
        // Do huffman decoding with a lookup table instead of reading one bit at a time
        // and traversing a tree.
        // Here we peek ("HUFFMAN_CODE_MAXLEN" == 17) amount of bits and see from a table which
        // symbol it maps to and how many bits should be consumed from the stream.
        // The symbol is then mapped into an op for filling the field path.
        loop {
            if bitreader.bits_left < HUFFMAN_CODE_MAXLEN {
                bitreader.refill();
            }

            let peeked_bits = bitreader.peek(HUFFMAN_CODE_MAXLEN);
            // SAFETY: peek(17) yields a value in [0, 2^17-1] (it masks with (1<<17)-1), and the
            // huffman table is built with exactly 2^17 entries (huf.b = 131071 pairs + 1 sentinel,
            // see create_huffman_lookup_table). So `peeked_bits` is always a valid index. Eliding
            // the bounds check removes a per-symbol branch in the hottest decode loop.
            let (symbol, code_len) = unsafe { *self.huffman_lookup_table.get_unchecked(peeked_bits as usize) };
            bitreader.consume(code_len as u32);
            if symbol == STOP_READING_SYMBOL {
                break;
            }
            do_op(symbol, bitreader, &mut fp)?;
            self.write_fp(&mut fp, idx)?;
            idx += 1;
        }
        Ok(idx)
    }

    pub fn decode_entity_update(
        &mut self,
        bitreader: &mut Bitreader,
        entity_id: i32,
        n_updates: usize,
        is_fullpacket: bool,
        is_baseline: bool,
        events_to_emit: &mut Vec<GameEventInfo>,
    ) -> Result<usize, DemoParserError> {
        let (cls_id, serial) = match self.entities.get(entity_id as usize) {
            Some(Some(entity)) => (entity.cls_id, entity.serial),
            _ => return Err(DemoParserError::EntityNotFound),
        };
        let class = match self.cls_by_id.get(cls_id as usize) {
            Some(cls) => cls,
            None => return Err(DemoParserError::ClassNotFound),
        };

        for path in self.paths.iter().take(n_updates) {
            let field = find_field(&path, &class.serializer)?;
            let field_info = get_propinfo(&field, path);
            let decoder = get_decoder_from_field(field)?;
            let result = bitreader.decode(&decoder, self.qf_mapper)?;

            self.smoke_voxels.observe(entity_id, &class.name, field, path, &result);
            self.ag2_recipes.observe(entity_id, serial, &class.name, self.tick, field, path, &result);
            self.infernos.observe(entity_id, field, path, &result);
            self.utility.observe(entity_id, field, path, &result);
            // A baseline is class default data and a full packet is a re-sent snapshot.
            // Neither is evidence that anything moved; only a delta update is.
            let is_delta_update = !is_baseline && !is_fullpacket;
            self.world_entity_audit.observe(
                entity_id, serial, &class.name, self.tick, field, &result, is_delta_update);
            self.world_entities.observe(
                entity_id, serial, &class.name, self.tick, field, &result, is_delta_update);

            let entity = match self.entities.get_mut(entity_id as usize) {
                Some(Some(entity)) => entity,
                _ => return Err(DemoParserError::EntityNotFound),
            };

            // listen_to_props()
            if self.list_props {
                if let Field::Value(_v) = field {
                    if should_emit_prop_to_listen(&_v.full_name) {
                        self.uniq_prop_names.insert(convert_weapon_prefix_to_general(&_v.full_name));
                    }
                }
            }
            // Custom events
            if !is_baseline {
                SecondPassParser::listen_for_events(
                    entity,
                    &result,
                    field,
                    field_info,
                    &self.prop_controller,
                    &self.prop_controller.special_ids,
                    is_fullpacket,
                    events_to_emit,
                );
            }
            // Debug
            if self.is_debug_mode {
                SecondPassParser::debug_inspect(
                    &result,
                    field,
                    self.tick,
                    field_info,
                    path,
                    is_fullpacket,
                    is_baseline,
                    class,
                    &cls_id,
                    &entity_id,
                );
            }
            SecondPassParser::insert_field(entity, result, field_info);
        }
        Ok(n_updates)
    }

    pub fn debug_inspect(
        _result: &Variant,
        field: &Field,
        _tick: i32,
        field_info: Option<FieldInfo>,
        _path: &FieldPath,
        _is_fullpacket: bool,
        _is_baseline: bool,
        _cls: &Class,
        _cls_id: &u32,
        _entity_id: &i32,
    ) {
        if let Field::Value(_v) = field {
            println!("{:?} {:?} {:?} {:?} {:?}", _path, field_info, _v.full_name, _result, _cls.name);
        }
    }

    pub fn insert_field(entity: &mut Entity, result: Variant, field_info: Option<FieldInfo>) {
        if let Some(fi) = field_info {
            if fi.should_parse {
                if (ECON_ATTRIBUTE_DEF_INDEX_BASE..ECON_ATTRIBUTE_DEF_INDEX_BASE + FLATTENED_VEC_MAX_LEN).contains(&fi.prop_id) {
                    entity.props.insert(ECON_ATTRIBUTE_LEGACY_DEF_ID, result.clone());
                    entity.props.insert(ECON_ATTRIBUTE_LEGACY_DEF_SLOT_ID,
                        Variant::U32(fi.prop_id - ECON_ATTRIBUTE_DEF_INDEX_BASE));
                }
                if fi.prop_id == ECON_ATTRIBUTE_COUNT_ID {
                    if let Variant::U32(count) = &result {
                        // The legacy scalar is a diagnostic of the last decoded
                        // live element, never permission to resurrect a removed one.
                        if matches!(entity.props.get(&ECON_ATTRIBUTE_LEGACY_DEF_SLOT_ID), Some(Variant::U32(slot)) if slot >= count) {
                            entity.props.remove(&ECON_ATTRIBUTE_LEGACY_DEF_ID);
                            entity.props.remove(&ECON_ATTRIBUTE_LEGACY_DEF_SLOT_ID);
                        }
                        // Truncate storage as well as filtering reads: a later grow
                        // must not resurrect attributes removed by an earlier shrink.
                        entity.props.retain(|id, _| {
                            [ECON_ATTRIBUTE_DEF_INDEX_BASE, ECON_ATTRIBUTE_RAW_VALUE_BASE]
                                .iter().all(|base| !(*base..*base + FLATTENED_VEC_MAX_LEN).contains(id)
                                    || *id - *base < *count)
                        });
                    }
                }
                entity.props.insert(fi.prop_id, result);
            }
        }
    }

    #[inline]
    fn write_fp(&mut self, fp_src: &mut FieldPath, idx: usize) -> Result<(), DemoParserError> {
        match self.paths.get_mut(idx) {
            Some(entry) => *entry = *fp_src,
            // need to extend vec (rare)
            None => {
                // If we have over 100k fields for an entity then something definitely went wrong. Do this to avoid infinite loop/oom
                if idx > 100_000 {
                    return Err(DemoParserError::VectorResizeFailure);
                }
                self.paths.resize(idx + 1, generate_fp());
                match self.paths.get_mut(idx) {
                    Some(entry) => *entry = *fp_src,
                    None => return Err(DemoParserError::VectorResizeFailure),
                }
            }
        }
        Ok(())
    }
    fn create_new_entity(&mut self, bitreader: &mut Bitreader, entity_id: &i32, _events_to_emit: &mut Vec<GameEventInfo>, is_fullpacket: bool) -> Result<(), DemoParserError> {
        // Class id width is dynamic: ceil(log2(num_classes + 1)). Hardcoded 8 bits
        // capped at 256 classes and broke on patches with more (14154+), causing
        // bitstream desync and cascading EntityNotFound errors. cls_by_id.len()
        // already equals num_classes + 1 (see first_pass::parser::parse_class_info).
        let cls_bits = (self.cls_by_id.len() as f32).log2().ceil() as u32;
        let cls_id: u32 = bitreader.read_nbits(cls_bits)?;
        // Both of these are not used. Don't think they are interesting for the parser
        let serial = bitreader.read_nbits(NSERIALBITS)?;
        let _unknown = bitreader.read_varint();
        let entity_type = self.check_entity_type(&cls_id)?;
        if let Some(class) = self.cls_by_id.get(cls_id as usize) {
            if self.utility.is_replaced(*entity_id, serial, &class.name) {
                self.capture_utility_entity(*entity_id, Some("replaced"));
            }
        }
        if let Some(class) = self.cls_by_id.get(cls_id as usize) {
            self.utility.begin(*entity_id, serial, &class.name, is_fullpacket);
            self.ag2_recipes.begin(*entity_id, serial, &class.name);
            self.world_entity_audit.begin(*entity_id, serial, &class.name, self.tick);
        self.world_entities.begin(*entity_id, serial, &class.name, self.tick);
        }
        let is_smoke = self
            .cls_by_id
            .get(cls_id as usize)
            .map(|class| class.name.contains("SmokeGrenadeProjectile"))
            .unwrap_or(false);
        if is_smoke {
            self.smoke_voxels.begin(*entity_id, self.tick);
        } else {
            self.smoke_voxels.finish_entity(*entity_id, self.tick);
        }
        if self.cls_by_id.get(cls_id as usize).map(|c| c.name == "CInferno").unwrap_or(false) {
            self.infernos.begin(*entity_id, serial, self.tick);
        } else {
            self.infernos.remove(*entity_id, self.tick);
        }
        if entity_type != EntityType::Projectile { self.projectiles.remove(entity_id); }
        match entity_type {
            EntityType::Projectile => {
                self.projectiles.insert(*entity_id);
            }
            EntityType::Rules => self.rules_entity_id = Some(*entity_id),
            EntityType::C4 => self.c4_entity_id = Some(*entity_id),
            _ => {}
        };
        let entity = Entity {
            entity_id: *entity_id,
            cls_id,
            serial,
            props: AHashMap::with_capacity(0),
            entity_type,
            // Creation makes the entity visible; subsequent odd transition codes mark it as
            // outside PVS/dormant until an enter or ordinary update resets this to an even code.
            pvs_state: Some(0),
        };
        if self.entities.len() as i32 <= *entity_id {
            // if corrupt, this can cause oom allocations
            if *entity_id > 100000 {
                return Err(DemoParserError::EntityNotFound);
            }
            self.entities.resize(*entity_id as usize + 1, None);
        }
        match self.entities.get_mut(*entity_id as usize) {
            Some(entry) => *entry = Some(entity),
            None => return Err(DemoParserError::VectorResizeFailure),
        };
        // Insert baselines
        if let Some(baseline_bytes) = self.baselines.get(&cls_id) {
            let b = &baseline_bytes.clone();
            let mut br = Bitreader::new(&b);
            self.update_entity(&mut br, *entity_id, true, &mut vec![], false)?;
        }
        Ok(())
    }

    /// Ends a world entity's life in the capture lane. A `leave` code is dormancy, not a
    /// removal; only the latter can mean a breakable was destroyed.
    fn world_entity_lifecycle(&mut self, entity_id: i32, dormant: bool) {
        if !self.world_entities.enabled() {
            return;
        }
        let Some((cls_id, serial)) = self
            .entities
            .get(entity_id as usize)
            .and_then(|entry| entry.as_ref())
            .map(|entity| (entity.cls_id, entity.serial))
        else {
            return;
        };
        if let Some(class) = self.cls_by_id.get(cls_id as usize) {
            self.world_entities.end(entity_id, serial, &class.name, self.tick, dormant);
        }
    }

    /// Records a lifecycle transition for the discovery audit. A no-op unless the audit is
    /// on, and deliberately reads the entity before the caller clears it.
    fn audit_lifecycle(&mut self, entity_id: i32, transition: &'static str) {
        if !self.world_entity_audit.enabled() {
            return;
        }
        let Some((cls_id, serial)) = self
            .entities
            .get(entity_id as usize)
            .and_then(|entry| entry.as_ref())
            .map(|entity| (entity.cls_id, entity.serial))
        else {
            return;
        };
        if let Some(class) = self.cls_by_id.get(cls_id as usize) {
            self.world_entity_audit.end(entity_id, serial, &class.name, self.tick, transition);
        }
    }

    pub fn check_entity_type(&self, cls_id: &u32) -> Result<EntityType, DemoParserError> {
        let class = match self.cls_by_id.get(*cls_id as usize) {
            Some(cls) => cls,
            None => {
                return Err(DemoParserError::ClassNotFound);
            }
        };
        match class.name.as_str() {
            "CCSPlayerController" => return Ok(EntityType::PlayerController),
            "CCSGameRulesProxy" => return Ok(EntityType::Rules),
            "CCSTeam" => return Ok(EntityType::Team),
            "CC4" => return Ok(EntityType::C4),
            _ => {}
        }
        let is_projectile_prop =
            (class.name.contains("Projectile") || class.name.contains("Grenade") || class.name.contains("Flash")) && !class.name.contains("Player");
        if is_projectile_prop {
            return Ok(EntityType::Projectile);
        }
        return Ok(EntityType::Normal);
    }
}

fn should_emit_prop_to_listen(prop_name: &str) -> bool {
    match prop_name.split(".").next() {
        Some("CCSGameRulesProxy") => return true,
        Some("CCSTeam") => return true,
        Some("CCSPlayerPawn") => return true,
        Some("CCSPlayerController") => return true,
        _ => {}
    };
    if is_weapon_prop(prop_name) || is_grenade_prop(prop_name) {
        return true;
    }
    false
}
fn convert_weapon_prefix_to_general(full_name: &str) -> String {
    let split_at_dot: Vec<&str> = full_name.split(".").collect();
    let grenade_or_weapon = is_grenade_or_weapon(full_name);
    // Strip first part of name from grenades and weapons.
    // if weapon prop: CAK47.m_iClip1 => m_iClip1
    // if grenade: CSmokeGrenadeProjectile.CBodyComponentBaseAnimGraph.m_cellX => CBodyComponentBaseAnimGraph.m_cellX
    if is_grenade_prop(full_name) {
        return "Grenade.".to_owned() + &split_at_dot[1..].join(".");
    }
    match grenade_or_weapon {
        true => "Weapon.".to_owned() + &split_at_dot[1..].join("."),
        false => full_name.to_string(),
    }
}
fn is_weapon_prop(full_name: &str) -> bool {
    let split_at_dot: Vec<&str> = full_name.split(".").collect();
    let is_weapon_prop =
        (split_at_dot[0].contains("Weapon") || split_at_dot[0].contains("AK")) && !split_at_dot[0].contains("Player") || split_at_dot[0].contains("CDEagle");
    is_weapon_prop
}
fn is_grenade_prop(full_name: &str) -> bool {
    if full_name.contains("CCSPlayer") {
        return false;
    }
    let parts = vec!["Molo", "Inc", "Infer", "Projectile", "Grenade", "Flash"];
    for part in parts {
        if full_name.contains(part) {
            return true;
        }
    }
    false
}
