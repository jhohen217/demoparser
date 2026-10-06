use crate::first_pass::frameparser::{StartEndOffset, StartEndType};
use crate::first_pass::parser::FirstPassOutput;
use crate::first_pass::parser_settings::check_multithreadability;
use crate::first_pass::parser_settings::{FirstPassParser, ParserInputs};
use crate::first_pass::prop_controller::{
    PropController, NAME_ID, PLAYER_X_ID, PLAYER_Y_ID, PLAYER_Z_ID, STEAMID_ID, TICK_ID, VELOCITY_ID, VELOCITY_X_ID, VELOCITY_Y_ID, VELOCITY_Z_ID,
};
use crate::first_pass::read_bits::DemoParserError;
use crate::second_pass::audio::AudioEvent;
use crate::second_pass::smoke_voxels::SmokeVoxelTrack;
use crate::second_pass::world_entities::WorldEntityDelta;
use crate::second_pass::world_entity_audit::WorldEntityAuditReport;
use crate::second_pass::collect_data::ProjectileRecord;
use crate::second_pass::game_events::{EventField, GameEvent};
use crate::second_pass::parser::SecondPassOutput;
use crate::second_pass::parser_settings::*;
use crate::second_pass::variants::VarVec;
use crate::second_pass::variants::{PropColumn, Variant};
use ahash::AHashMap;
use ahash::AHashSet;
use csgoproto::CsvcMsgVoiceData;
use itertools::Itertools;
use rayon::iter::IntoParallelIterator;
use rayon::iter::IntoParallelRefIterator;
use rayon::prelude::ParallelIterator;
use std::sync::mpsc::Receiver;
use std::thread;
use std::time::Duration;

pub const HEADER_ENDS_AT_BYTE: usize = 16;

/// Velocity is derived from the two most recent positions for a SteamID. Each
/// multithreaded second-pass chunk starts with an empty output, so its first
/// two lagged samples cannot see the preceding chunk's position history.
/// Rebuild the derived columns after merging to give the multithreaded path the
/// same history as the single-threaded path.
fn rebuild_velocity_columns(df: &mut AHashMap<u32, PropColumn>) {
    let wants_speed = df.contains_key(&VELOCITY_ID);
    let wants_x = df.contains_key(&VELOCITY_X_ID);
    let wants_y = df.contains_key(&VELOCITY_Y_ID);
    let wants_z = df.contains_key(&VELOCITY_Z_ID);
    if !(wants_speed || wants_x || wants_y || wants_z) {
        return;
    }

    let steamids = match df.get(&STEAMID_ID).and_then(|column| column.data.as_ref()) {
        Some(VarVec::U64(values)) => values,
        _ => return,
    };
    let xs = df.get(&PLAYER_X_ID).and_then(|column| match column.data.as_ref() {
        Some(VarVec::F32(values)) => Some(values),
        _ => None,
    });
    let ys = df.get(&PLAYER_Y_ID).and_then(|column| match column.data.as_ref() {
        Some(VarVec::F32(values)) => Some(values),
        _ => None,
    });
    let zs = df.get(&PLAYER_Z_ID).and_then(|column| match column.data.as_ref() {
        Some(VarVec::F32(values)) => Some(values),
        _ => None,
    });

    let row_count = steamids.len();
    let mut speed = vec![None; row_count];
    let mut velocity_x = vec![None; row_count];
    let mut velocity_y = vec![None; row_count];
    let mut velocity_z = vec![None; row_count];
    let mut previous_by_steamid: AHashMap<u64, usize> = AHashMap::default();
    let mut penultimate_by_steamid: AHashMap<u64, usize> = AHashMap::default();

    let delta = |values: Option<&Vec<Option<f32>>>, current: usize, previous: usize| {
        let current_value = values?.get(current).copied().flatten()?;
        let previous_value = values?.get(previous).copied().flatten()?;
        Some((current_value * 64.0) - (previous_value * 64.0))
    };

    for row in 0..row_count {
        let Some(steamid) = steamids[row] else {
            continue;
        };
        let Some(previous) = previous_by_steamid.insert(steamid, row) else {
            continue;
        };
        // The collector evaluates velocity before this row's coordinate props
        // are appended, so an output row carries the preceding position delta.
        // Preserve that established (and S2R-observable) one-sample lag.
        let Some(penultimate) = penultimate_by_steamid.insert(steamid, previous) else {
            continue;
        };

        velocity_x[row] = delta(xs, previous, penultimate);
        velocity_y[row] = delta(ys, previous, penultimate);
        velocity_z[row] = delta(zs, previous, penultimate);
        if let (Some(x), Some(y)) = (velocity_x[row], velocity_y[row]) {
            speed[row] = Some((f32::powi(x, 2) + f32::powi(y, 2)).sqrt());
        }
    }

    let mut replace = |id, values: Vec<Option<f32>>| {
        let num_nones = values.iter().filter(|value| value.is_none()).count();
        df.insert(
            id,
            PropColumn {
                data: Some(VarVec::F32(values)),
                num_nones,
            },
        );
    };
    if wants_speed {
        replace(VELOCITY_ID, speed);
    }
    if wants_x {
        replace(VELOCITY_X_ID, velocity_x);
    }
    if wants_y {
        replace(VELOCITY_Y_ID, velocity_y);
    }
    if wants_z {
        replace(VELOCITY_Z_ID, velocity_z);
    }
}

#[derive(Debug)]
pub struct DemoOutput {
    /// Networked AG2 task-recipe snapshots, independent of opaque DEM_AnimationData records.
    pub ag2_recipes: Vec<crate::second_pass::ag2_recipes::Ag2RecipeSnapshot>,
    /// Raw Source 2 audio transport records. Only populated by the
    /// projectile/event parser lane, then globally sorted by source position.
    pub audio_events: Vec<AudioEvent>,
    /// Tick-ordered network payloads for every smoke entity lifetime.
    pub smoke_voxels: Vec<SmokeVoxelTrack>,
    pub infernos: Vec<crate::second_pass::infernos::InfernoPatchRecord>,
    pub utility: crate::second_pass::utility::UtilityData,
    /// Discovery-only world-entity audit. Empty unless the capture was requested.
    pub world_entity_audit: WorldEntityAuditReport,
    /// Door, breakable and mover lifecycle. Empty unless the lane was requested.
    pub world_entities: Vec<WorldEntityDelta>,
    pub df: AHashMap<u32, PropColumn>,
    pub game_events: Vec<GameEvent>,
    pub skins: Vec<EconItem>,
    pub item_drops: Vec<EconItem>,
    /// Opt-in full weapon-entity lane. Rows are tick-local and sorted at merge.
    pub weapon_entity_snapshots: Vec<WeaponEntitySnapshot>,
    pub chat_messages: Vec<ChatMessageRecord>,
    pub convars: AHashMap<String, String>,
    pub header: Option<AHashMap<String, String>>,
    pub player_md: Vec<PlayerEndMetaData>,
    /// Live player roster from CCSPlayerController entities (final per-player state,
    /// deduplicated by steamid). Populated even when the end-of-match scoreboard message
    /// is absent (community/casual demos). Fallback when `player_md` is empty.
    pub roster: Vec<PlayerEndMetaData>,
    pub game_events_counter: AHashSet<String>,
    pub uniq_prop_names: Vec<String>,
    pub projectiles: Vec<ProjectileRecord>,
    pub voice_data: Vec<(i32, CsvcMsgVoiceData)>,
    pub prop_controller: PropController,
    pub df_per_player: AHashMap<u64, AHashMap<u32, PropColumn>>,
}

pub struct Parser<'a> {
    input: ParserInputs<'a>,
    pub parsing_mode: ParsingMode,
    capture_animation_recipes: bool,
}
#[derive(PartialEq)]
pub enum ParsingMode {
    ForceSingleThreaded,
    ForceMultiThreaded,
    Normal,
}

impl<'a> Parser<'a> {
    pub fn new(input: ParserInputs<'a>, parsing_mode: ParsingMode) -> Self {
        Parser {
            input: input,
            parsing_mode: parsing_mode,
            capture_animation_recipes: true,
        }
    }
    /// Event-only consumers can omit the animation lane while retaining entity decoding.
    /// Replay consumers keep the existing lossless capture by default.
    pub fn with_animation_recipes(mut self, enabled: bool) -> Self {
        self.capture_animation_recipes = enabled;
        self
    }
    pub fn parse_demo(&mut self, demo_bytes: &[u8]) -> Result<DemoOutput, DemoParserError> {
        let _prof = std::env::var("CS2_PROF").is_ok();
        let _t = std::time::Instant::now();
        let mut first_pass_parser = FirstPassParser::new(&self.input);
        let mut first_pass_output = first_pass_parser.parse_demo(demo_bytes, false)?;
        first_pass_output.capture_animation_recipes = self.capture_animation_recipes;
        if _prof {
            eprintln!("[prof] first_pass: {:.3}s", _t.elapsed().as_secs_f64());
        }
        if self.parsing_mode == ParsingMode::Normal
            && check_multithreadability(&self.input.wanted_player_props)
            && !(self.parsing_mode == ParsingMode::ForceSingleThreaded)
            || self.parsing_mode == ParsingMode::ForceMultiThreaded
        {
            return self.second_pass_multi_threaded(demo_bytes, first_pass_output);
        } else {
            self.second_pass_single_threaded(demo_bytes, first_pass_output)
        }
    }

    fn second_pass_multi_threaded(&self, outer_bytes: &[u8], first_pass_output: FirstPassOutput) -> Result<DemoOutput, DemoParserError> {
        let second_pass_outputs: Vec<Result<SecondPassOutput, DemoParserError>> = first_pass_output
            .fullpacket_offsets
            .par_iter()
            .map(|offset| {
                let mut parser = SecondPassParser::new(first_pass_output.clone(), *offset, false, None)?;
                parser.start(outer_bytes)?;
                Ok(parser.create_output())
            })
            .collect();
        // check for errors
        let mut ok = vec![];
        for result in second_pass_outputs {
            match result {
                Err(e) => return Err(e),
                Ok(r) => ok.push(r),
            };
        }
        let mut outputs = self.combine_outputs(&mut ok, first_pass_output);
        if !self.input.parse_projectiles {
            rebuild_velocity_columns(&mut outputs.df);
        }
        if let Some(new_df) = self.rm_unwanted_ticks(&mut outputs.df) {
            outputs.df = new_df;
        }
        Parser::remove_duplicate_player_connects(&mut outputs.game_events);
        Parser::add_item_purchase_sell_column(&mut outputs.game_events);
        Parser::remove_item_sold_events(&mut outputs.game_events);
        Ok(outputs)
    }
    fn remove_duplicate_player_connects(events: &mut Vec<GameEvent>) {
        let mut v = events.iter().filter(|x| x.name == "player_first_connect").collect_vec();
        v.sort_by_key(|x| x.tick);
        let mut ids = AHashMap::default();
        for x in v {
            for f in &x.fields {
                if f.name == "steamid" {
                    if let Some(Variant::U64(s)) = f.data {
                        match ids.get(&s) {
                            Some(_) => {}
                            None => {
                                ids.insert(s, x.clone());
                            }
                        }
                    }
                }
            }
        }
        events.retain(|x| x.name != "player_first_connect");
        events.extend(ids.values().map(|x| x.clone()));
    }
    fn second_pass_single_threaded(&self, outer_bytes: &[u8], first_pass_output: FirstPassOutput) -> Result<DemoOutput, DemoParserError> {
        let prof = std::env::var("CS2_PROF").is_ok();
        let mut t = std::time::Instant::now();
        let mut parser = SecondPassParser::new(first_pass_output.clone(), 16, true, None)?;
        parser.start(outer_bytes)?;
        if prof {
            eprintln!("[prof] second_pass start(): {:.3}s", t.elapsed().as_secs_f64());
            t = std::time::Instant::now();
        }
        let second_pass_output = parser.create_output();
        if prof {
            eprintln!("[prof] create_output: {:.3}s", t.elapsed().as_secs_f64());
            t = std::time::Instant::now();
        }
        let mut outputs = self.combine_outputs(&mut vec![second_pass_output], first_pass_output);
        if prof {
            eprintln!("[prof] combine_outputs: {:.3}s", t.elapsed().as_secs_f64());
            t = std::time::Instant::now();
        }
        if let Some(new_df) = self.rm_unwanted_ticks(&mut outputs.df) {
            outputs.df = new_df;
        }
        Parser::add_item_purchase_sell_column(&mut outputs.game_events);
        Parser::remove_item_sold_events(&mut outputs.game_events);
        if prof {
            eprintln!("[prof] post-proc: {:.3}s", t.elapsed().as_secs_f64());
        }
        Ok(outputs)
    }
    #[allow(dead_code)]
    fn second_pass_threaded_with_channels(
        &self,
        outer_bytes: &[u8],
        first_pass_output: FirstPassOutput,
        receiver: Receiver<StartEndOffset>,
    ) -> Result<DemoOutput, DemoParserError> {
        thread::scope(|s| {
            let mut handles = vec![];
            let mut channel_threading_was_ok = true;
            loop {
                if let Ok(start_end_offset) = receiver.recv_timeout(Duration::from_secs(3)) {
                    match start_end_offset.msg_type {
                        StartEndType::EndOfMessages => break,
                        StartEndType::OK => {}
                        StartEndType::MultithreadingWasNotOk => {
                            channel_threading_was_ok = false;
                            break;
                        }
                    }
                    let my_first_out = first_pass_output.clone();
                    handles.push(s.spawn(move || {
                        let mut parser = SecondPassParser::new(my_first_out, start_end_offset.start, false, Some(start_end_offset))?;
                        parser.start(outer_bytes)?;
                        Ok(parser.create_output())
                    }));
                } else {
                    channel_threading_was_ok = false;
                    break;
                }
            }
            // Fallback if channels failed to find all fullpackets. Should be rare.
            if !channel_threading_was_ok {
                let mut first_pass_parser = FirstPassParser::new(&self.input);
                let first_pass_output = first_pass_parser.parse_demo(outer_bytes, false)?;
                return self.second_pass_multi_threaded_no_channels(outer_bytes, first_pass_output);
            }
            // check for errors
            let mut ok = vec![];
            for result in handles {
                match result.join() {
                    Err(_e) => return Err(DemoParserError::MalformedMessage),
                    Ok(r) => {
                        ok.push(r?);
                    }
                };
            }
            let mut outputs = self.combine_outputs(&mut ok, first_pass_output);
            if !self.input.parse_projectiles {
                rebuild_velocity_columns(&mut outputs.df);
            }
            if let Some(new_df) = self.rm_unwanted_ticks(&mut outputs.df) {
                outputs.df = new_df;
            }
            Parser::add_item_purchase_sell_column(&mut outputs.game_events);
            Parser::remove_item_sold_events(&mut outputs.game_events);
            return Ok(outputs);
        })
    }
    #[allow(dead_code)]
    fn second_pass_multi_threaded_no_channels(&self, outer_bytes: &[u8], first_pass_output: FirstPassOutput) -> Result<DemoOutput, DemoParserError> {
        let second_pass_outputs: Vec<Result<SecondPassOutput, DemoParserError>> = first_pass_output
            .fullpacket_offsets
            .par_iter()
            .map(|offset| {
                let mut parser = SecondPassParser::new(first_pass_output.clone(), *offset, false, None)?;
                parser.start(outer_bytes)?;
                Ok(parser.create_output())
            })
            .collect();
        // check for errors
        let mut ok = vec![];
        for result in second_pass_outputs {
            match result {
                Err(e) => return Err(e),
                Ok(r) => ok.push(r),
            };
        }
        let mut outputs = self.combine_outputs(&mut ok, first_pass_output);
        if !self.input.parse_projectiles {
            rebuild_velocity_columns(&mut outputs.df);
        }
        if let Some(new_df) = self.rm_unwanted_ticks(&mut outputs.df) {
            outputs.df = new_df;
        }
        Parser::add_item_purchase_sell_column(&mut outputs.game_events);
        Parser::remove_item_sold_events(&mut outputs.game_events);
        Ok(outputs)
    }
    fn remove_item_sold_events(events: &mut Vec<GameEvent>) {
        events.retain(|x| x.name != "item_sold")
    }
    fn add_item_purchase_sell_column(events: &mut Vec<GameEvent>) {
        // Checks each item_purchase event for if the item was eventually sold

        let purchases = events.iter().filter(|x| x.name == "item_purchase").collect_vec();
        let sells = events.iter().filter(|x| x.name == "item_sold").collect_vec();

        let purchases = purchases.iter().filter_map(|event| SellBackHelper::from_event(event)).collect_vec();
        let sells = sells.iter().filter_map(|event| SellBackHelper::from_event(event)).collect_vec();

        let mut was_sold = vec![];
        for purchase in &purchases {
            let wanted_sells = sells
                .iter()
                .filter(|sell| sell.tick > purchase.tick && sell.steamid == purchase.steamid && sell.inventory_slot == purchase.inventory_slot);
            let wanted_buys = purchases
                .iter()
                .filter(|buy| buy.tick > purchase.tick && buy.steamid == purchase.steamid && buy.inventory_slot == purchase.inventory_slot);
            let min_tick_sells = wanted_sells.min_by_key(|x| x.tick);
            let min_tick_buys = wanted_buys.min_by_key(|x| x.tick);
            if let (Some(sell_tick), Some(buy_tick)) = (min_tick_sells, min_tick_buys) {
                if sell_tick.tick < buy_tick.tick {
                    was_sold.push(true);
                } else {
                    was_sold.push(false);
                }
            } else {
                was_sold.push(false);
            }
        }
        let mut idx = 0;
        for event in events {
            if event.name == "item_purchase" {
                event.fields.push(EventField {
                    name: "was_sold".to_string(),
                    data: Some(Variant::Bool(was_sold[idx])),
                });
                idx += 1;
            }
        }
    }
    fn rm_unwanted_ticks(&self, hm: &mut AHashMap<u32, PropColumn>) -> Option<AHashMap<u32, PropColumn>> {
        // Used for removing ticks when velocity is needed
        if self.input.wanted_ticks.is_empty() {
            return None;
        }
        let wanted_ticks: AHashSet<i32> = self.input.wanted_ticks.iter().copied().collect();
        let mut wanted_indicies = vec![];
        if let Some(ticks) = hm.get(&TICK_ID) {
            if let Some(VarVec::I32(t)) = &ticks.data {
                for (idx, val) in t.iter().enumerate() {
                    if let Some(tick) = val {
                        if wanted_ticks.contains(tick) {
                            wanted_indicies.push(idx);
                        }
                    }
                }
            }
        }
        let mut new_df = AHashMap::default();
        for (k, v) in hm {
            if let Some(new) = v.slice_to_new(&wanted_indicies) {
                new_df.insert(*k, new);
            }
        }
        Some(new_df)
    }

    fn combine_outputs(&self, second_pass_outputs: &mut Vec<SecondPassOutput>, first_pass_output: FirstPassOutput) -> DemoOutput {
        // Combines all inner DemoOutputs into one big output
        let mut outputs = std::mem::take(second_pass_outputs);
        outputs.sort_by_key(|x| x.ptr);

        if outputs.len() == 1 {
            let mut output = outputs.pop().unwrap();
            output
                .weapon_entity_snapshots
                .sort_by_key(|row| {
                    (
                        row.tick,
                        row.source_order,
                        row.entity_id,
                        row.entity_serial,
                        !row.present,
                    )
                });
            output.weapon_entity_snapshots.dedup();
            output.audio_events.sort_by_key(AudioEvent::sort_key);
            output.audio_events.dedup_by_key(|event| event.sort_key());
            let mut prop_controller = first_pass_output.prop_controller.clone();
            for prop in first_pass_output.added_temp_props {
                prop_controller.wanted_player_props.retain(|x| x != &prop);
                prop_controller.prop_infos.retain(|x| &x.prop_name != &prop);
            }

            let mut pp = AHashMap::default();
            for (steamid, mut df) in output.df_per_player {
                df.remove(&STEAMID_ID);
                df.remove(&NAME_ID);
                pp.insert(steamid, df);
            }

            let mut all_prop_names: Vec<String> = output.uniq_prop_names.into_iter().collect();
            all_prop_names.sort();
            all_prop_names.dedup();

            let roster = {
                let mut by_sid: std::collections::BTreeMap<u64, PlayerEndMetaData> = std::collections::BTreeMap::new();
                for p in output.roster {
                    if let Some(sid) = p.steamid {
                        if sid != 0 {
                            by_sid.insert(sid, p);
                        }
                    }
                }
                by_sid.into_values().collect()
            };

            return DemoOutput {
                ag2_recipes: output.ag2_recipes,
                audio_events: output.audio_events,
                smoke_voxels: output.smoke_voxels,
                infernos: output.infernos,
                utility: output.utility,
                world_entity_audit: output.world_entity_audit,
                world_entities: output.world_entities,
                prop_controller,
                chat_messages: output.chat_messages,
                item_drops: output.item_drops,
                weapon_entity_snapshots: output.weapon_entity_snapshots,
                player_md: output.player_md,
                roster,
                game_events: output.game_events,
                skins: output.skins,
                convars: output.convars,
                df: output.df,
                header: Some(first_pass_output.header),
                game_events_counter: output.game_events_counter,
                projectiles: output.projectiles,
                voice_data: output.voice_data,
                df_per_player: pp,
                uniq_prop_names: all_prop_names,
            };
        }

        let mut dfs = Vec::with_capacity(outputs.len());
        let mut per_players: AHashMap<u64, Vec<AHashMap<u32, PropColumn>>> = AHashMap::default();
        let mut all_game_events = AHashSet::default();
        let mut all_prop_names = Vec::new();
        let mut chat_messages = Vec::new();
        let mut item_drops = Vec::new();
        let mut weapon_entity_snapshots = Vec::new();
        let mut player_md = Vec::new();
        let mut roster_by_sid: std::collections::BTreeMap<u64, PlayerEndMetaData> = std::collections::BTreeMap::new();
        let mut game_events = Vec::new();
        let mut skins = Vec::new();
        let mut convars = AHashMap::default();
        let mut projectiles = Vec::new();
        let mut voice_data = Vec::new();
        let mut audio_events = Vec::new();
        let mut smoke_voxels = Vec::new();
        let mut infernos = Vec::new();
        let mut utility = crate::second_pass::utility::UtilityData::default();
        let mut world_entity_audit = WorldEntityAuditReport::default();
        let mut world_entities: Vec<WorldEntityDelta> = Vec::new();
        let mut ag2_recipes = Vec::new();

        for output in outputs {
            dfs.push(output.df);
            for event_name in output.game_events_counter {
                all_game_events.insert(event_name);
            }
            all_prop_names.extend(output.uniq_prop_names);
            for (steamid, df) in output.df_per_player {
                per_players.entry(steamid).or_default().push(df);
            }
            chat_messages.extend(output.chat_messages);
            item_drops.extend(output.item_drops);
            weapon_entity_snapshots.extend(output.weapon_entity_snapshots);
            player_md.extend(output.player_md);
            for p in output.roster {
                if let Some(sid) = p.steamid {
                    if sid != 0 {
                        roster_by_sid.insert(sid, p);
                    }
                }
            }
            game_events.extend(output.game_events);
            skins.extend(output.skins);
            convars.extend(output.convars);
            projectiles.extend(output.projectiles);
            voice_data.extend(output.voice_data);
            audio_events.extend(output.audio_events);
            smoke_voxels.extend(output.smoke_voxels);
            infernos.extend(output.infernos);
            utility.captured |= output.utility.captured;
            utility.records.extend(output.utility.records);
            crate::second_pass::world_entity_audit::merge(&mut world_entity_audit, output.world_entity_audit);
            crate::second_pass::world_entities::merge(&mut world_entities, output.world_entities);
            ag2_recipes.extend(output.ag2_recipes);
        }

        audio_events.sort_by_key(AudioEvent::sort_key);
        utility.records.sort_by_key(|r| (r.frame_offset, r.message_index, r.sequence));
        utility.records.dedup();
        // Slices are merged in pointer order, but a world-entity timeline is only readable
        // in tick order, and a full-packet worker can re-observe a tick another already saw.
        world_entity_audit.changes.sort_by(|left, right| {
            (left.tick, left.entity_id, &left.field).cmp(&(right.tick, right.entity_id, &right.field))
        });
        world_entity_audit.changes.dedup();
        world_entity_audit.lifecycle.sort_by(|left, right| {
            (left.tick, left.entity_id, left.transition).cmp(&(right.tick, right.entity_id, right.transition))
        });
        world_entity_audit.lifecycle.dedup();
        audio_events.dedup_by_key(|event| event.sort_key());
        smoke_voxels.sort_by_key(|track| (track.start_tick, track.entity_id, track.life_index));
        smoke_voxels.dedup();
        // A full-packet worker sees a still-live entity as a new baseline. Collapse that
        // segment-local start to the earliest observed start of the same network lifetime.
        let mut inferno_starts = std::collections::BTreeMap::new();
        for row in &infernos {
            let start = inferno_starts.entry((row.entity_id, row.serial)).or_insert(row.start_tick);
            *start = (*start).min(row.start_tick);
        }
        for row in &mut infernos { row.start_tick = inferno_starts[&(row.entity_id, row.serial)]; }
        infernos.sort_by_key(|r| (r.tick, r.entity_id, r.serial, r.index));
        infernos.dedup();

        let all_dfs_combined = self.combine_dfs(dfs, false);
        // Chunk starts can overlap at full packets. A stable sort and exact
        // dedupe makes the opt-in entity lane deterministic across Rayon runs.
        weapon_entity_snapshots.sort_by_key(|row| {
            (
                row.tick,
                row.source_order,
                row.entity_id,
                row.entity_serial,
                !row.present,
            )
        });
        weapon_entity_snapshots.dedup();
        ag2_recipes.sort_by_key(|row| (row.tick, row.entity_id, row.entity_serial, row.life_index));
        ag2_recipes.dedup_by_key(|row| (row.tick, row.entity_id, row.entity_serial, row.life_index));
        all_prop_names.sort();
        all_prop_names.dedup();
        // Remove temp props
        let mut prop_controller = first_pass_output.prop_controller.clone();
        for prop in first_pass_output.added_temp_props {
            prop_controller.wanted_player_props.retain(|x| x != &prop);
            prop_controller.prop_infos.retain(|x| &x.prop_name != &prop);
        }
        let mut pp = AHashMap::default();
        for (steamid, v) in per_players {
            let combined = self.combine_dfs(v, true);
            pp.insert(steamid, combined);
        }

        DemoOutput {
            ag2_recipes,
            audio_events,
            smoke_voxels,
            infernos,
            utility,
            world_entity_audit,
            world_entities,
            prop_controller: prop_controller,
            chat_messages,
            item_drops,
            weapon_entity_snapshots,
            player_md,
            // Second-pass segments are sorted by ascending tick. Each captures a player's state
            // at its last tick; dedup by steamid keeping the LAST entry -> final name/team.
            roster: roster_by_sid.into_values().collect(),
            game_events,
            skins,
            convars,
            df: all_dfs_combined,
            header: Some(first_pass_output.header),
            game_events_counter: all_game_events,
            projectiles,
            voice_data,
            df_per_player: pp,
            uniq_prop_names: all_prop_names,
        }
    }

    fn combine_dfs(&self, mut v: Vec<AHashMap<u32, PropColumn>>, remove_name_and_steamid: bool) -> AHashMap<u32, PropColumn> {
        let mut big: AHashMap<u32, PropColumn> = AHashMap::default();
        if v.len() == 1 {
            let mut result = v.remove(0);
            if remove_name_and_steamid {
                result.remove(&STEAMID_ID);
                result.remove(&NAME_ID);
            }
            return result;
        }

        // Pre-group each chunk's columns into per-prop ordered buckets. This only MOVES the
        // PropColumn structs (no row-data copy) and preserves chunk (offset) order, so the
        // first bucket entry is the seed and the rest are appended in order — identical to the
        // serial insert/extend_from it replaces.
        let mut groups: AHashMap<u32, Vec<PropColumn>> = AHashMap::default();
        for part_df in v {
            for (k, col) in part_df {
                if remove_name_and_steamid && (k == STEAMID_ID || k == NAME_ID) {
                    continue;
                }
                groups.entry(k).or_default().push(col);
            }
        }
        // Concatenate each prop's segments in parallel. Columns are independent and the per-prop
        // order is preserved, so the result is byte-identical to the serial merge. This is the
        // dominant serial cost in the multi-threaded path (~25% of MT wall-clock on large demos).
        let groups_vec: Vec<(u32, Vec<PropColumn>)> = groups.into_iter().collect();
        let combined: Vec<(u32, PropColumn)> = groups_vec
            .into_par_iter()
            .map(|(k, mut segs)| {
                let mut acc = segs.remove(0);
                for mut seg in segs {
                    acc.extend_from(&mut seg);
                }
                (k, acc)
            })
            .collect();
        big.extend(combined);
        big
    }
}

#[derive(Debug)]
pub struct SellBackHelper {
    pub tick: i32,
    pub steamid: u64,
    pub inventory_slot: u32,
}
impl SellBackHelper {
    pub fn from_event(event: &GameEvent) -> Option<Self> {
        if let Some(Variant::I32(tick)) = SellBackHelper::extract_field("tick", &event.fields) {
            if let Some(Variant::U64(steamid)) = SellBackHelper::extract_field("steamid", &event.fields) {
                if let Some(Variant::U32(slot)) = SellBackHelper::extract_field("inventory_slot", &event.fields) {
                    return Some(SellBackHelper {
                        tick: tick,
                        steamid: steamid,
                        inventory_slot: slot,
                    });
                }
            }
        }
        None
    }
    fn extract_field(name: &str, fields: &[EventField]) -> Option<Variant> {
        for field in fields {
            if field.name == name {
                return field.data.clone();
            }
        }
        None
    }
}

#[cfg(test)]
mod velocity_rebuild_tests {
    use super::*;

    fn column(data: VarVec) -> PropColumn {
        PropColumn {
            data: Some(data),
            num_nones: 0,
        }
    }

    #[test]
    fn rebuilds_interleaved_player_velocity_across_a_merged_boundary() {
        let mut df = AHashMap::default();
        df.insert(STEAMID_ID, column(VarVec::U64(vec![Some(1), Some(2), Some(1), Some(2), Some(1), Some(2)])));
        df.insert(
            PLAYER_X_ID,
            column(VarVec::F32(vec![Some(0.0), Some(10.0), Some(1.0), Some(12.0), Some(4.0), Some(15.0)])),
        );
        df.insert(
            PLAYER_Y_ID,
            column(VarVec::F32(vec![Some(0.0), Some(5.0), Some(2.0), Some(4.0), Some(7.0), Some(1.0)])),
        );
        df.insert(
            PLAYER_Z_ID,
            column(VarVec::F32(vec![Some(1.0), Some(2.0), Some(4.0), Some(6.0), Some(9.0), Some(12.0)])),
        );
        for id in [VELOCITY_ID, VELOCITY_X_ID, VELOCITY_Y_ID, VELOCITY_Z_ID] {
            df.insert(id, column(VarVec::F32(vec![None; 6])));
        }

        rebuild_velocity_columns(&mut df);

        assert_eq!(
            df[&VELOCITY_X_ID].data,
            Some(VarVec::F32(vec![None, None, None, None, Some(64.0), Some(128.0)]))
        );
        assert_eq!(
            df[&VELOCITY_Y_ID].data,
            Some(VarVec::F32(vec![None, None, None, None, Some(128.0), Some(-64.0)]))
        );
        assert_eq!(
            df[&VELOCITY_Z_ID].data,
            Some(VarVec::F32(vec![None, None, None, None, Some(192.0), Some(256.0)]))
        );
        assert_eq!(
            df[&VELOCITY_ID].data,
            Some(VarVec::F32(vec![
                None,
                None,
                None,
                None,
                Some((64.0_f32.powi(2) + 128.0_f32.powi(2)).sqrt()),
                Some((128.0_f32.powi(2) + (-64.0_f32).powi(2)).sqrt()),
            ]))
        );
    }
}
