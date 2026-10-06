use crate::entity_handle::entity_handle_index;
use crate::first_pass::parser::Frame;
use crate::first_pass::parser::HEADER_ENDS_AT_BYTE;
use crate::first_pass::parser_settings::FirstPassParser;
use crate::first_pass::prop_controller::PropController;
use crate::first_pass::prop_controller::*;
use crate::first_pass::read_bits::read_varint;
use crate::first_pass::read_bits::Bitreader;
use crate::first_pass::read_bits::DemoParserError;
use crate::first_pass::stringtables::parse_userinfo;
use crate::maps::demo_cmd_type_from_int;
use crate::second_pass::audio::AudioEvent;
use crate::second_pass::smoke_voxels::SmokeVoxelTrack;
use crate::second_pass::collect_data::ProjectileRecord;
use crate::second_pass::entities::Entity;
use crate::second_pass::game_events::GameEvent;
use crate::second_pass::parser_settings::SecondPassParser;
use crate::second_pass::parser_settings::*;
use crate::second_pass::variants::PropColumn;
use crate::second_pass::variants::Variant;
use ahash::AHashMap;
use ahash::AHashSet;
use csgoproto::message_type::NetMessageType::{self, *};
use csgoproto::CDemoFullPacket;
use csgoproto::CDemoPacket;
use csgoproto::CDemoStringTables;
use csgoproto::CnetMsgTick;
use csgoproto::CsgoUserCmdPb;
use csgoproto::CsvcMsgServerInfo;
use csgoproto::CsvcMsgUserCommands;
use csgoproto::CsvcMsgVoiceData;
use csgoproto::EDemoCommands::*;
use prost::Message;
use snap::raw::decompress_len;
use snap::raw::Decoder as SnapDecoder;

use super::usercmd_delta::apply_delta;
use super::variants::{InputHistory, UserCmdSubtickMove};

const OUTER_BUF_DEFAULT_LEN: usize = 400_000;
const INNER_BUF_DEFAULT_LEN: usize = 8192 * 15;

fn button_state_masks(state1: u64, state2: u64, state3: u64) -> (u64, u64, u64) {
    (state1, state3 | (state1 & state2), state3 | (!state1 & state2))
}

// --- env-gated phase profiling (CS2_PROF=1) ---------------------------------
#[inline]
pub(crate) fn prof_on() -> bool {
    static PROF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *PROF.get_or_init(|| std::env::var("CS2_PROF").is_ok())
}
thread_local! {
    static PROF_ENTS_NS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static PROF_COLLECT_NS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    pub(crate) static PROF_PATHS_NS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    pub(crate) static PROF_DECODE_NS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[derive(Debug)]
pub struct SecondPassOutput {
    pub ag2_recipes: Vec<crate::second_pass::ag2_recipes::Ag2RecipeSnapshot>,
    pub audio_events: Vec<AudioEvent>,
    pub smoke_voxels: Vec<SmokeVoxelTrack>,
    pub infernos: Vec<crate::second_pass::infernos::InfernoPatchRecord>,
    pub utility: crate::second_pass::utility::UtilityData,
    pub df: AHashMap<u32, PropColumn>,
    pub game_events: Vec<GameEvent>,
    pub skins: Vec<EconItem>,
    pub item_drops: Vec<EconItem>,
    pub weapon_entity_snapshots: Vec<WeaponEntitySnapshot>,
    pub chat_messages: Vec<ChatMessageRecord>,
    pub convars: AHashMap<String, String>,
    pub header: Option<AHashMap<String, String>>,
    pub player_md: Vec<PlayerEndMetaData>,
    /// Live player roster from CCSPlayerController entities (final per-player state).
    /// Populated even when CCSUsrMsg_EndOfMatchAllPlayersData is absent (community/casual
    /// demos), where `player_md` ends up empty. Use as a fallback when `player_md` is empty.
    pub roster: Vec<PlayerEndMetaData>,
    pub game_events_counter: AHashSet<String>,
    pub uniq_prop_names: AHashSet<String>,
    pub prop_info: PropController,
    pub projectiles: Vec<ProjectileRecord>,
    pub ptr: usize,
    pub voice_data: Vec<(i32, CsvcMsgVoiceData)>,
    pub df_per_player: AHashMap<u64, AHashMap<u32, PropColumn>>,
    pub entities: Vec<Option<Entity>>,
    pub last_tick: i32,
    /// Discovery-only world-entity audit. Empty unless the capture was requested.
    pub world_entity_audit: crate::second_pass::world_entity_audit::WorldEntityAuditReport,
    /// Door, breakable and mover lifecycle. Empty unless the lane was requested.
    pub world_entities: Vec<crate::second_pass::world_entities::WorldEntityDelta>,
}
impl<'a> SecondPassParser<'a> {
    pub fn start(&mut self, demo_bytes: &'a [u8]) -> Result<(), DemoParserError> {
        if prof_on() {
            PROF_ENTS_NS.with(|c| c.set(0));
            PROF_COLLECT_NS.with(|c| c.set(0));
            PROF_PATHS_NS.with(|c| c.set(0));
            PROF_DECODE_NS.with(|c| c.set(0));
        }
        let started_at = self.ptr;
        // re-use these to avoid allocation
        let mut buf = vec![0_u8; INNER_BUF_DEFAULT_LEN];
        let mut buf2 = vec![0_u8; OUTER_BUF_DEFAULT_LEN];

        loop {
            // Need at least a few bytes to read frame header (3 varints, minimum 1 byte each)
            if self.ptr + 3 > demo_bytes.len() {
                break;
            }
            let frame = match self.read_frame(demo_bytes) {
                Ok(f) => f,
                Err(DemoParserError::OutOfBytesError) => break,
                Err(e) => return Err(e),
            };
            self.current_demo_frame_offset = frame.frame_starts_at as u64;
            if frame.demo_cmd == DemAnimationData || frame.demo_cmd == DemSendTables {
                self.ptr += frame.size as usize;
                continue;
            }
            let bytes = match self.slice_packet_bytes(demo_bytes, frame.size) {
                Ok(b) => b,
                Err(_) => {
                    self.ptr += frame.size;
                    continue;
                }
            };
            let bytes = self.decompress_if_needed(&mut buf, bytes, &frame)?;
            self.ptr += frame.size;

            let ok = match frame.demo_cmd {
                DemSignonPacket => self.parse_packet(&bytes, &mut buf2),
                DemStringTables => {
                    let tables = CDemoStringTables::decode(bytes).map_err(|_| DemoParserError::MalformedMessage)?;
                    self.parse_full_packet_stringtables(&CDemoFullPacket {
                        string_table: Some(tables), ..Default::default()
                    });
                    Ok(())
                }
                DemPacket => {
                    self.last_packet_tick = frame.tick;
                    self.parse_packet(&bytes, &mut buf2)
                }
                DemStop => break,
                DemUserCmd => Ok(()),
                DemFullPacket => {
                    if self.parse_full_packet_and_break_if_needed(&bytes, &mut buf2, started_at)? {
                        break;
                    }
                    Ok(())
                }
                _ => Ok(()),
            };
            ok?;
        }
        if prof_on() {
            let ents = PROF_ENTS_NS.with(|c| c.get());
            let coll = PROF_COLLECT_NS.with(|c| c.get());
            let paths = PROF_PATHS_NS.with(|c| c.get());
            let dec = PROF_DECODE_NS.with(|c| c.get());
            eprintln!("[prof] parse_packet_ents: {:.3}s | collect_*: {:.3}s", ents as f64 / 1e9, coll as f64 / 1e9);
            eprintln!(
                "[prof]   within ents: parse_paths {:.3}s | decode_entity_update {:.3}s",
                paths as f64 / 1e9,
                dec as f64 / 1e9
            );
        }
        Ok(())
    }
    fn parse_full_packet_and_break_if_needed(&mut self, bytes: &[u8], buf: &mut Vec<u8>, started_at: usize) -> Result<bool, DemoParserError> {
        if let Some(start_end_offset) = self.start_end_offset {
            if self.ptr > start_end_offset.end {
                return Ok(true);
            } else {
                self.parse_full_packet(&bytes, true, buf)?;
                return Ok(false);
            }
        }
        match self.parse_all_packets {
            true => {
                // Establish entity state from the first full packet, then leave later ones
                // to the deltas as before.
                //
                // This path skips the snapshot on the assumption that the deltas have
                // already produced the same state. That holds for a demo read from its
                // first tick, but not for one spliced to start at a mid-match checkpoint:
                // there the deltas have nothing to apply to and parsing fails with
                // EntityNotFound. Taking the first snapshot costs one decode and changes
                // nothing for ordinary demos, where it lands at the opening tick and
                // agrees with the delta that precedes it. Applying *every* snapshot is not
                // equivalent — it overwrites accumulated values such as m_flSimulationTime
                // and reserve ammo, and fails the e2e suite.
                let establish = self.fullpackets_parsed == 0;
                self.parse_full_packet(&bytes, establish, buf)?;
                self.fullpackets_parsed += 1;
            }
            false => {
                if self.fullpackets_parsed == 0 && started_at != HEADER_ENDS_AT_BYTE {
                    self.parse_full_packet(&bytes, true, buf)?;
                    self.fullpackets_parsed += 1;
                } else {
                    return Ok(true);
                }
            }
        }
        return Ok(false);
    }
    fn read_frame(&mut self, demo_bytes: &[u8]) -> Result<Frame, DemoParserError> {
        let frame_starts_at = self.ptr;
        let cmd = read_varint(demo_bytes, &mut self.ptr)?;
        let tick = read_varint(demo_bytes, &mut self.ptr)?;
        let size = read_varint(demo_bytes, &mut self.ptr)?;
        let tick = tick as i32;
        if tick != self.tick {
            self.ag2_recipes.flush_tick(self.tick);
            self.smoke_voxels.flush_tick(self.tick);
            self.infernos.flush_tick(self.tick);
            self.world_entities.flush_tick(self.tick);
        }
        self.tick = tick;

        let msg_type = cmd & !64;
        let is_compressed = (cmd & 64) == 64;
        let demo_cmd = demo_cmd_type_from_int(msg_type as i32)?;

        Ok(Frame {
            size: size as usize,
            frame_starts_at,
            is_compressed,
            demo_cmd,
            tick: self.tick,
        })
    }
    fn slice_packet_bytes(&mut self, demo_bytes: &'a [u8], frame_size: usize) -> Result<&'a [u8], DemoParserError> {
        if self.ptr + frame_size as usize >= demo_bytes.len() {
            return Err(DemoParserError::MalformedMessage);
        }
        Ok(&demo_bytes[self.ptr..self.ptr + frame_size])
    }
    fn decompress_if_needed<'b>(&mut self, buf: &'b mut Vec<u8>, possibly_uncompressed_bytes: &'b [u8], frame: &Frame) -> Result<&'b [u8], DemoParserError> {
        match frame.is_compressed {
            true => {
                FirstPassParser::resize_if_needed(buf, decompress_len(possibly_uncompressed_bytes))?;
                match SnapDecoder::new().decompress(possibly_uncompressed_bytes, buf) {
                    Ok(idx) => Ok(&buf[..idx]),
                    Err(e) => return Err(DemoParserError::DecompressionFailure(format!("{}", e))),
                }
            }
            false => Ok(possibly_uncompressed_bytes),
        }
    }
    pub fn resize_if_needed(buf: &mut Vec<u8>, needed_len: Result<usize, snap::Error>) -> Result<(), DemoParserError> {
        match needed_len {
            Ok(len) => {
                if buf.len() < len {
                    buf.resize(len, 0)
                }
            }
            Err(e) => return Err(DemoParserError::DecompressionFailure(e.to_string())),
        };
        Ok(())
    }

    pub fn parse_packet(&mut self, bytes: &[u8], buf: &mut Vec<u8>) -> Result<(), DemoParserError> {
        let msg = match CDemoPacket::decode(bytes) {
            Err(_) => return Err(DemoParserError::MalformedMessage),
            Ok(msg) => msg,
        };
        let mut bitreader = Bitreader::new(msg.data());
        self.parse_packet_from_bitreader(&mut bitreader, buf, true, false)?;
        Ok(())
    }

    pub fn parse_packet_from_bitreader(
        &mut self,
        bitreader: &mut Bitreader,
        buf: &mut Vec<u8>,
        should_parse_entities: bool,
        is_fullpacket: bool,
    ) -> Result<(), DemoParserError> {
        let mut wrong_order_events = vec![];

        let mut network_message_index = 0_u32;
        while bitreader.bits_remaining().unwrap_or(0) > 8 {
            let msg_type = bitreader.read_u_bit_var()?;
            let size = bitreader.read_varint()?;
            if buf.len() < size as usize {
                buf.resize(size as usize, 0)
            }
            bitreader.read_n_bytes_mut(size as usize, buf)?;
            let msg_bytes = &buf[..size as usize];
            self.current_network_message_index = network_message_index;
            if self.parse_projectiles {
                self.capture_audio_message(msg_type, msg_bytes)?;
            }
            let ok = match NetMessageType::from(msg_type as i32) {
                svc_PacketEntities => {
                    if should_parse_entities {
                        let _pt = prof_on().then(std::time::Instant::now);
                        self.parse_packet_ents(msg_bytes, is_fullpacket)?;
                        if let Some(t) = _pt {
                            PROF_ENTS_NS.with(|c| c.set(c.get() + t.elapsed().as_nanos() as u64));
                        }
                        if !is_fullpacket {
                            let _ct = prof_on().then(std::time::Instant::now);
                            self.collect_entities();
                            if let Some(t) = _ct {
                                PROF_COLLECT_NS.with(|c| c.set(c.get() + t.elapsed().as_nanos() as u64));
                            }
                        }
                    }
                    Ok(())
                }
                svc_CreateStringTable => self.parse_create_stringtable(msg_bytes),
                svc_UpdateStringTable => self.update_string_table(msg_bytes),
                svc_ServerInfo => self.parse_server_info(msg_bytes),
                CS_UM_SendPlayerItemDrops => self.parse_item_drops(msg_bytes),
                CS_UM_EndOfMatchAllPlayersData => self.parse_player_end_msg(msg_bytes),
                UM_SayText2 => self.create_custom_event_chat_message(msg_bytes),
                UM_SayText => self.create_custom_event_server_message(msg_bytes),
                net_SetConVar => self.create_custom_event_parse_convars(msg_bytes),
                CS_UM_PlayerStatsUpdate => self.parse_player_stats_update(msg_bytes),
                CS_UM_ServerRankUpdate => self.create_custom_event_rank_update(msg_bytes),
                net_Tick => self.parse_net_tick(msg_bytes),
                svc_ClearAllStringTables => self.clear_stringtables(),
                svc_VoiceData => self.parse_voice_data(msg_bytes),
                GE_Source1LegacyGameEvent => self.parse_game_event(msg_bytes, &mut wrong_order_events),
                svc_UserCmds => self.parse_user_cmd(msg_bytes),
                GE_FireBulletsId => self.create_custom_event_fire_bullets(msg_bytes),
                GE_PlayerBulletHitId => self.create_custom_event_player_bullet_hit(msg_bytes),
                _ => Ok(()),
            };
            ok?;
            network_message_index = network_message_index.saturating_add(1);
        }
        if !wrong_order_events.is_empty() {
            self.resolve_wrong_order_event(&mut wrong_order_events)?;
        }
        Ok(())
    }
    pub fn parse_user_cmd(&mut self, bytes: &[u8]) -> Result<(), DemoParserError> {
        // We simply inject the values into the entities as if they came from packet_ents like any other val.

        // This method is quite expensive so early exit it if not needed.
        if !self.parse_usercmd {
            return Ok(());
        }

        let msg = match CsvcMsgUserCommands::decode(bytes) {
            Ok(m) => m,
            _ => return Ok(()),
        };
        for cmd in msg.commands {
            let player_slot = cmd.player_slot();
            if player_slot < 0 {
                continue;
            }
            let data = cmd.data.as_ref().filter(|data| !data.is_empty());
            let delta_data = cmd.delta_data.as_ref().filter(|data| !data.is_empty());
            let mut next = if let Some(data) = data {
                match CsgoUserCmdPb::decode(data.as_ref()) {
                    Ok(command) => Some(command),
                    Err(_) => continue,
                }
            } else if delta_data.is_some() {
                self.usercmd_baselines.get(&player_slot).cloned()
            } else {
                continue;
            };

            if let Some(delta_data) = delta_data {
                next = next.as_ref().and_then(|baseline| apply_delta(baseline, delta_data.as_ref()));
            }
            let Some(next) = next else {
                continue;
            };
            self.usercmd_baselines.insert(player_slot, next.clone());
            self.apply_user_cmd(&next);
        }
        Ok(())
    }

    fn apply_user_cmd(&mut self, user_cmd: &CsgoUserCmdPb) {
        let Some(base) = user_cmd.base.as_ref() else {
            return;
        };
        let Some(ent) = user_cmd_pawn(&mut self.entities, base.pawn_entity_handle) else {
            return;
        };

        let history = user_cmd
            .input_history
            .iter()
            .map(|input| {
                let view_angles = input.view_angles.clone().unwrap_or_default();
                InputHistory {
                    player_tick_count: input.player_tick_count(),
                    player_tick_fraction: input.player_tick_fraction(),
                    render_tick_count: input.render_tick_count(),
                    render_tick_fraction: input.render_tick_fraction(),
                    x: view_angles.x(),
                    y: view_angles.y(),
                    z: view_angles.z(),
                }
            })
            .collect();
        ent.props.insert(USERCMD_INPUT_HISTORY_BASEID, Variant::InputHistory(history));
        let subtick_moves = base
            .subtick_moves
            .iter()
            .map(|subtick| UserCmdSubtickMove {
                when: subtick.when(),
                button: subtick.button(),
                pressed: subtick.pressed(),
                analog_forward: subtick.analog_forward_delta(),
                analog_left: subtick.analog_left_delta(),
                pitch_delta: subtick.pitch_delta(),
                yaw_delta: subtick.yaw_delta(),
            })
            .collect();
        ent.props
            .insert(USERCMD_SUBTICK_MOVES_BASEID, Variant::UserCmdSubtickMoves(subtick_moves));
        ent.props.insert(USERCMD_LEFTMOVE, Variant::F32(base.leftmove()));
        ent.props.insert(USERCMD_FORWARDMOVE, Variant::F32(base.forwardmove()));
        ent.props.insert(USERCMD_IMPULSE, Variant::I32(base.impulse()));
        ent.props.insert(USERCMD_MOUSE_DX, Variant::I32(base.mousedx()));
        ent.props.insert(USERCMD_MOUSE_DY, Variant::I32(base.mousedy()));
        ent.props.insert(USERCMD_WEAPON_SELECT, Variant::I32(base.weaponselect()));
        ent.props.insert(USERCMD_SUBTICK_LEFT_HAND_DESIRED, Variant::Bool(user_cmd.left_hand_desired()));
        if let Some(viewangles) = base.viewangles.as_ref() {
            ent.props.insert(USERCMD_VIEWANGLE_X, Variant::F32(viewangles.x()));
            ent.props.insert(USERCMD_VIEWANGLE_Y, Variant::F32(viewangles.y()));
            ent.props.insert(USERCMD_VIEWANGLE_Z, Variant::F32(viewangles.z()));
        }
        apply_button_state_observation(&mut ent.props, base.buttons_pb.as_ref());
        ent.props
            .insert(USERCMD_CONSUMED_SERVER_ANGLE_CHANGES, Variant::U32(base.consumed_server_angle_changes()));
    }

    pub fn parse_voice_data(&mut self, bytes: &[u8]) -> Result<(), DemoParserError> {
        if let Ok(m) = CsvcMsgVoiceData::decode(bytes) {
            self.voice_data.push((self.tick, m));
        }
        Ok(())
    }
    pub fn parse_game_event(&mut self, bytes: &[u8], wrong_order_events: &mut Vec<GameEvent>) -> Result<(), DemoParserError> {
        match self.parse_event(bytes) {
            Ok(Some(event)) => {
                wrong_order_events.push(event);
                Ok(())
            }
            Ok(None) => Ok(()),
            Err(e) => return Err(e),
        }
    }

    pub fn parse_net_tick(&mut self, bytes: &[u8]) -> Result<(), DemoParserError> {
        let message = match CnetMsgTick::decode(bytes) {
            Ok(message) => message,
            Err(_) => return Err(DemoParserError::MalformedMessage),
        };
        self.net_tick = message.tick();
        Ok(())
    }

    pub fn parse_full_packet(&mut self, bytes: &[u8], should_parse_entities: bool, buf: &mut Vec<u8>) -> Result<(), DemoParserError> {
        self.string_tables = vec![];
        let full_packet = match CDemoFullPacket::decode(bytes) {
            Err(_e) => return Err(DemoParserError::MalformedMessage),
            Ok(p) => p,
        };
        self.parse_full_packet_stringtables(&full_packet);
        if let Some(packet) = full_packet.packet {
            let mut bitreader = Bitreader::new(packet.data());
            self.parse_packet_from_bitreader(&mut bitreader, buf, should_parse_entities, true)
        } else {
            Ok(())
        }
    }

    pub fn parse_full_packet_stringtables(&mut self, full_packet: &CDemoFullPacket) {
        if let Some(string_table) = &full_packet.string_table {
            for item in &string_table.tables {
                if item.table_name == Some("instancebaseline".to_string()) {
                    for i in &item.items {
                        let k = i.str().parse::<u32>().unwrap_or(u32::MAX);
                        self.baselines.insert(k, i.data().to_vec());
                    }
                }
                if item.table_name == Some("userinfo".to_string()) {
                    for i in &item.items {
                        if let Ok(player) = parse_userinfo(&i.data()) {
                            if player.steamid != 0 {
                                self.stringtable_players.insert(player.userid, player);
                            }
                        }
                    }
                }
            }
        }
    }
    fn clear_stringtables(&mut self) -> Result<(), DemoParserError> {
        self.string_tables = vec![];
        Ok(())
    }
    pub fn parse_server_info(&mut self, bytes: &[u8]) -> Result<(), DemoParserError> {
        let server_info = match CsvcMsgServerInfo::decode(bytes) {
            Err(_e) => return Err(DemoParserError::MalformedMessage),
            Ok(p) => p,
        };
        let class_count = server_info.max_classes();
        self.cls_bits = Some((class_count as f32 + 1.).log2().ceil() as u32);
        Ok(())
    }
    pub fn parse_user_command_cmd(&mut self, _data: &[u8]) -> Result<(), DemoParserError> {
        // Only in pov demos. Maybe implement sometime. Includes buttons etc.
        Ok(())
    }
}

/// Retains the existing index-based lookup; this does not establish serial/life freshness.
fn user_cmd_pawn(entities: &mut [Option<Entity>], handle: Option<u32>) -> Option<&mut Entity> {
    entities.get_mut(entity_handle_index(handle?) as usize)?.as_mut()
}

/// Applies already merged optional state. Inherited Some is known decoded state;
/// absent fields must not leave a stale property or turn into observed zero.
fn apply_button_state_observation(props: &mut AHashMap<u32, Variant>, buttons: Option<&csgoproto::CInButtonStatePb>) {
    for (id, mask) in [
        (USERCMD_BUTTONSTATE_1, buttons.and_then(|b| b.buttonstate1)),
        (USERCMD_BUTTONSTATE_2, buttons.and_then(|b| b.buttonstate2)),
        (USERCMD_BUTTONSTATE_3, buttons.and_then(|b| b.buttonstate3)),
    ] {
        if let Some(mask) = mask { props.insert(id, Variant::U64(mask)); }
        else { props.remove(&id); }
    }
    // Derived masks are known only when all three raw state observations are present.
    let derived = buttons.and_then(|b| Some(button_state_masks(b.buttonstate1?, b.buttonstate2?, b.buttonstate3?)));
    for (id, mask) in [
        (USERCMD_BUTTONS_HELD, derived.map(|x| x.0)),
        (USERCMD_BUTTONS_PRESSED, derived.map(|x| x.1)),
        (USERCMD_BUTTONS_RELEASED, derived.map(|x| x.2)),
    ] {
        if let Some(mask) = mask { props.insert(id, Variant::U64(mask)); }
        else { props.remove(&id); }
    }

}

#[cfg(test)]
mod input_presence_tests {
    use super::*;
    #[test]
    fn input_presence_preserves_zero_and_clears_absent_fields_and_payload() {
        let mut props = AHashMap::from([(USERCMD_BUTTONSTATE_1, Variant::U64(1)), (USERCMD_BUTTONSTATE_2, Variant::U64(2))]);
        apply_button_state_observation(&mut props, Some(&csgoproto::CInButtonStatePb { buttonstate1: Some(0), ..Default::default() }));
        assert_eq!(props.get(&USERCMD_BUTTONSTATE_1), Some(&Variant::U64(0)));
        assert!(!props.contains_key(&USERCMD_BUTTONSTATE_2));
        apply_button_state_observation(&mut props, Some(&Default::default()));
        assert!(!props.contains_key(&USERCMD_BUTTONSTATE_1));
        props.insert(USERCMD_BUTTONSTATE_1, Variant::U64(1));
        apply_button_state_observation(&mut props, None);
        assert!(!props.contains_key(&USERCMD_BUTTONSTATE_1));
    }
    #[test]
    fn input_presence_routes_only_present_handles_to_the_selected_entity() {
        fn entity(index: i32) -> Entity {
            Entity { cls_id: 0, entity_id: index, serial: 0, props: AHashMap::new(),
                entity_type: crate::second_pass::entities::EntityType::Normal, pvs_state: None }
        }
        let mut entities = vec![Some(entity(0)), Some(entity(1)), None];
        entities[0].as_mut().unwrap().props.insert(USERCMD_BUTTONSTATE_1, Variant::U64(9));
        let observed = csgoproto::CInButtonStatePb { buttonstate1: Some(0), ..Default::default() };
        assert!(user_cmd_pawn(&mut entities, None).is_none());
        assert!(user_cmd_pawn(&mut entities, Some(2)).is_none());
        assert!(user_cmd_pawn(&mut entities, Some(3)).is_none());
        apply_button_state_observation(&mut user_cmd_pawn(&mut entities, Some(1)).unwrap().props, Some(&observed));
        assert_eq!(entities[0].as_ref().unwrap().props.get(&USERCMD_BUTTONSTATE_1), Some(&Variant::U64(9)));
        assert_eq!(entities[1].as_ref().unwrap().props.get(&USERCMD_BUTTONSTATE_1), Some(&Variant::U64(0)));
        // A replacement slot starts without synthetic state; routing uses the current slot.
        // Serial validation and stale merged handles are outside this presence contract.
        entities[1] = Some(entity(1));
        assert!(!user_cmd_pawn(&mut entities, Some(1)).unwrap().props.contains_key(&USERCMD_BUTTONSTATE_1));
        apply_button_state_observation(&mut user_cmd_pawn(&mut entities, Some(1)).unwrap().props, Some(&observed));
        apply_button_state_observation(&mut user_cmd_pawn(&mut entities, Some(1)).unwrap().props, None);
        assert!(!entities[1].as_ref().unwrap().props.contains_key(&USERCMD_BUTTONSTATE_1));
    }
}

#[cfg(test)]
mod button_mask_tests {
    use super::button_state_masks;

    #[test]
    fn reconstructs_all_button_state_sequences() {
        let expected = [
            (false, false, false),
            (true, false, false),
            (false, false, true),
            (true, true, false),
            (false, true, true),
            (true, true, true),
            (false, true, true),
            (true, true, true),
        ];
        let bit = 1 << 5;

        for (code, expected) in expected.into_iter().enumerate() {
            let state1 = u64::from(code & 1 != 0) * bit;
            let state2 = u64::from(code & 2 != 0) * bit;
            let state3 = u64::from(code & 4 != 0) * bit;
            let (held, pressed, released) = button_state_masks(state1, state2, state3);
            assert_eq!((held != 0, pressed != 0, released != 0), expected, "button state {code}");
        }
    }
}

#[cfg(test)]
mod publication_input_tests {
    use super::*;
    #[test]
    fn derived_masks_require_complete_observations_and_clear_stale_state() {
        let mut props = AHashMap::new();
        apply_button_state_observation(&mut props, Some(&csgoproto::CInButtonStatePb {
            buttonstate1: Some(32), buttonstate2: Some(32), buttonstate3: Some(0),
        }));
        assert_eq!(props.get(&USERCMD_BUTTONS_HELD), Some(&Variant::U64(32)));
        assert_eq!(props.get(&USERCMD_BUTTONS_PRESSED), Some(&Variant::U64(32)));
        assert_eq!(props.get(&USERCMD_BUTTONS_RELEASED), Some(&Variant::U64(0)));
        apply_button_state_observation(&mut props, Some(&csgoproto::CInButtonStatePb {
            buttonstate1: Some(0), ..Default::default()
        }));
        assert_eq!(props.get(&USERCMD_BUTTONSTATE_1), Some(&Variant::U64(0)));
        assert!(!props.contains_key(&USERCMD_BUTTONS_HELD));
        assert!(!props.contains_key(&USERCMD_BUTTONS_PRESSED));
        assert!(!props.contains_key(&USERCMD_BUTTONS_RELEASED));
    }
}
