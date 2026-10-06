//! Death event parsing functionality
//!
//! This module handles parsing death events from demo output.

use crate::core::demo_processor::team_timeline::{team_name, TeamTimeline};
use crate::core::demo_processor::types::{PlayerInfo, RoundInfo};
use crate::models::kill::Kill;
use ahash::AHashMap;
use parser::first_pass::prop_controller::{PLAYER_X_ID, PLAYER_Y_ID, PLAYER_Z_ID, STEAMID_ID, TICK_ID};
use parser::parse_demo::DemoOutput;
use parser::second_pass::game_events::GameEvent as ParserGameEvent;
use parser::second_pass::variants::{PropColumn, VarVec, Variant};
use std::collections::HashMap;

/// Resolve a player's observed position at the death tick. Prefer the exact event
/// tick, then allow only the immediately preceding sample because Source may omit
/// the victim pawn's row on the tick that reports its death. Multiple identical
/// samples are harmless; conflicting or incomplete samples are ambiguous and are
/// rejected instead of choosing an arbitrary row or synthesizing coordinates.
fn dataframe_position_at_event_tick(
    df: &AHashMap<u32, PropColumn>,
    steamid: &str,
    event_tick: i32,
) -> Option<[f32; 3]> {
    let steamid = steamid.parse::<u64>().ok()?;
    let ticks = match df.get(&TICK_ID)?.data.as_ref()? {
        VarVec::I32(values) => values,
        _ => return None,
    };
    let steamids = match df.get(&STEAMID_ID)?.data.as_ref()? {
        VarVec::U64(values) => values,
        _ => return None,
    };
    let x = f32_column(df.get(&PLAYER_X_ID)?)?;
    let y = f32_column(df.get(&PLAYER_Y_ID)?)?;
    let z = f32_column(df.get(&PLAYER_Z_ID)?)?;
    let row_count = ticks.len().min(steamids.len()).min(x.len()).min(y.len()).min(z.len());

    for target_tick in [Some(event_tick), event_tick.checked_sub(1)].into_iter().flatten() {
        let mut position = None;
        for row in 0..row_count {
            if ticks[row] != Some(target_tick) || steamids[row] != Some(steamid) {
                continue;
            }
            let Some((x, y, z)) = x[row].zip(y[row]).zip(z[row]).map(|((x, y), z)| (x, y, z)) else {
                continue;
            };
            let candidate = [x, y, z];
            if !candidate.iter().all(|value| value.is_finite()) {
                continue;
            }
            match position {
                Some(previous) if position_bits(previous) != position_bits(candidate) => {
                    return None;
                }
                _ => position = Some(candidate),
            }
        }
        if position.is_some() {
            return position;
        }
    }
    None
}

fn f32_column(column: &PropColumn) -> Option<&[Option<f32>]> {
    match column.data.as_ref()? {
        VarVec::F32(values) => Some(values),
        _ => None,
    }
}

fn position_bits(position: [f32; 3]) -> [u32; 3] {
    position.map(f32::to_bits)
}

/// Kill sources that are not a weapon the attacker was holding. For these the attacker's
/// active-weapon def index describes something unrelated, so it must not be used as a
/// weapon id.
const NON_WEAPON_DAMAGE_SOURCES: &[&str] = &["world", "worldent", "planted_c4", "trigger_hurt"];

/// Helper function to look up player team by steamid or name
fn lookup_player_team(steamid: &str, name: &str, player_info: &[PlayerInfo]) -> String {
    // First try to match by steamid
    for player in player_info {
        if player.steamid == steamid {
            return player.team.clone();
        }
    }

    // Fall back to matching by name if steamid lookup fails
    for player in player_info {
        if player.name == name {
            return player.team.clone();
        }
    }

    "UNKNOWN".to_string()
}

/// Parse death events from parser output
pub fn parse_death_events_from_output(
    output: &DemoOutput,
    rounds: &[RoundInfo],
    player_info: &[PlayerInfo],
) -> Result<Vec<Kill>, String> {
    let team_timeline = TeamTimeline::from_output(output);
    parse_death_events_with_timeline(
        &output.game_events,
        rounds,
        player_info,
        Some(&team_timeline),
        Some(&output.df),
    )
}

/// Parse death events from the raw game-event stream.
pub fn parse_death_events(
    game_events: &[ParserGameEvent],
    rounds: &[RoundInfo],
    player_info: &[PlayerInfo],
) -> Result<Vec<Kill>, String> {
    parse_death_events_with_timeline(game_events, rounds, player_info, None, None)
}

fn parse_death_events_with_timeline(
    game_events: &[ParserGameEvent],
    rounds: &[RoundInfo],
    player_info: &[PlayerInfo],
    team_timeline: Option<&TeamTimeline>,
    dataframe: Option<&AHashMap<u32, PropColumn>>,
) -> Result<Vec<Kill>, String> {
    let mut death_events = Vec::new();
    let mut _total_player_death_events = 0;
    let mut _skipped_warmup = 0;
    let mut _skipped_missing_fields = 0;
    let _skipped_missing_team_data = 0;
    let _invalid_team_numbers = 0;
    let mut _successfully_parsed = 0;

    for event in game_events {
        if event.name == "player_death" {
            _total_player_death_events += 1;

            let mut tick = None;
            let mut attacker_name = None;
            let mut attacker_steamid = None;
            let mut attacker_team_num = None;
            let mut user_name = None;
            let mut user_steamid = None;
            let mut user_team_num = None;
            let mut weapon = None;
            let mut attacker_item_def_idx = None;
            let mut attacker_x = None;
            let mut attacker_y = None;
            let mut attacker_z = None;
            let mut attacker_pitch = None;
            let mut attacker_yaw = None;
            let mut user_x = None;
            let mut user_y = None;
            let mut user_z = None;
            let mut headshot = false;
            let mut penetrated = false;
            let mut attacker_blind = false;
            let mut thru_smoke = false;
            let mut no_scope = false;
            let mut is_warmup_period = false;
            let mut attacker_is_controlling_bot = false;

            for field in &event.fields {
                match field.name.as_str() {
                    "tick" => {
                        if let Some(Variant::I32(t)) = &field.data {
                            tick = Some(*t);
                        }
                    }
                    "attacker_name" => {
                        if let Some(Variant::String(s)) = &field.data {
                            attacker_name = Some(s.clone());
                        }
                    }
                    "attacker_steamid" => {
                        if let Some(Variant::U64(s)) = &field.data {
                            attacker_steamid = Some(s.to_string());
                        } else if let Some(Variant::String(s)) = &field.data {
                            attacker_steamid = Some(s.clone());
                        }
                    }
                    "attacker_team_num" => {
                        if let Some(Variant::I32(t)) = &field.data {
                            attacker_team_num = Some(*t);
                        }
                    }
                    "user_name" => {
                        if let Some(Variant::String(s)) = &field.data {
                            user_name = Some(s.clone());
                        }
                    }
                    "user_steamid" => {
                        if let Some(Variant::U64(s)) = &field.data {
                            user_steamid = Some(s.to_string());
                        } else if let Some(Variant::String(s)) = &field.data {
                            user_steamid = Some(s.clone());
                        }
                    }
                    "user_team_num" => {
                        if let Some(Variant::I32(t)) = &field.data {
                            user_team_num = Some(*t);
                        }
                    }
                    "weapon" => {
                        if let Some(Variant::String(s)) = &field.data {
                            weapon = Some(s.clone());
                        }
                    }
                    // Canonical m_iItemDefinitionIndex of the attacker's active weapon.
                    // Only trusted for weapon names the static table cannot resolve.
                    "attacker_item_def_idx" => match &field.data {
                        Some(Variant::U32(v)) => attacker_item_def_idx = Some(*v),
                        Some(Variant::I32(v)) if *v >= 0 => attacker_item_def_idx = Some(*v as u32),
                        _ => {}
                    },
                    "attacker_X" => {
                        if let Some(Variant::F32(x)) = &field.data {
                            attacker_x = Some(*x);
                        }
                    }
                    "attacker_Y" => {
                        if let Some(Variant::F32(y)) = &field.data {
                            attacker_y = Some(*y);
                        }
                    }
                    "attacker_Z" => {
                        if let Some(Variant::F32(z)) = &field.data {
                            attacker_z = Some(*z);
                        }
                    }
                    "attacker_pitch" => {
                        if let Some(Variant::F32(p)) = &field.data {
                            attacker_pitch = Some(*p);
                        }
                    }
                    "attacker_yaw" => {
                        if let Some(Variant::F32(y)) = &field.data {
                            attacker_yaw = Some(*y);
                        }
                    }
                    "user_X" => {
                        if let Some(Variant::F32(x)) = &field.data {
                            user_x = Some(*x);
                        }
                    }
                    "user_Y" => {
                        if let Some(Variant::F32(y)) = &field.data {
                            user_y = Some(*y);
                        }
                    }
                    "user_Z" => {
                        if let Some(Variant::F32(z)) = &field.data {
                            user_z = Some(*z);
                        }
                    }
                    "headshot" => {
                        if let Some(Variant::Bool(h)) = &field.data {
                            headshot = *h;
                        }
                    }
                    "penetrated" => {
                        penetrated = parser::second_pass::kill_modifiers::observed_bool(
                            field.data.as_ref(),
                        ).unwrap_or(false);
                    }
                    // The demo's own key names are unseparated; do not "tidy" these.
                    "attackerblind" => {
                        if let Some(Variant::Bool(b)) = &field.data {
                            attacker_blind = *b;
                        }
                    }
                    "thrusmoke" => {
                        if let Some(Variant::Bool(s)) = &field.data {
                            thru_smoke = *s;
                        }
                    }
                    "noscope" => {
                        if let Some(Variant::Bool(n)) = &field.data {
                            no_scope = *n;
                        }
                    }
                    "is_warmup_period" => {
                        if let Some(Variant::Bool(w)) = &field.data {
                            is_warmup_period = *w;
                        }
                    }
                    "attacker_is_controlling_bot" => {
                        if let Some(Variant::Bool(b)) = &field.data {
                            attacker_is_controlling_bot = *b;
                        }
                    }
                    _ => {}
                }
            }

            // Skip warmup kills
            if is_warmup_period {
                _skipped_warmup += 1;
                continue;
            }

            // Skip death events that are missing critical fields
            let Some(tick) = tick else {
                _skipped_missing_fields += 1;
                continue;
            };
            let Some(attacker_name) = attacker_name else {
                _skipped_missing_fields += 1;
                continue;
            };
            let Some(attacker_steamid) = attacker_steamid else {
                _skipped_missing_fields += 1;
                continue;
            };
            let Some(user_name) = user_name else {
                _skipped_missing_fields += 1;
                continue;
            };
            let Some(user_steamid) = user_steamid else {
                _skipped_missing_fields += 1;
                continue;
            };
            let Some(weapon) = weapon else {
                _skipped_missing_fields += 1;
                continue;
            };

            // The weapon *name* is authoritative for what did the killing; the attacker's
            // active-weapon def index is not (a molotov kill still reports the rifle in hand).
            // So only fall back to it for real weapons the static table doesn't know about,
            // which in practice means knife skins (bayonet 500, m9_bayonet 508, ...).
            let weapon_id = match crate::utils::weapon_mapper::map_weapon_name_to_id(&weapon) {
                id if id != "0" => id,
                _ if NON_WEAPON_DAMAGE_SOURCES.contains(&weapon.as_str()) => "0".to_string(),
                _ => attacker_item_def_idx
                    .map(|idx| idx.to_string())
                    .unwrap_or_else(|| "0".to_string()),
            };

            // Some demos carry no positions in player_death at all. Only in that
            // all-absent case, fall back to the same players' own dataframe rows:
            // exact event tick first, then the immediately preceding tick. If the
            // evidence is missing or ambiguous the death remains unsupported.
            if [
                attacker_x,
                attacker_y,
                attacker_z,
                user_x,
                user_y,
                user_z,
            ]
            .iter()
            .all(Option::is_none)
            {
                if let Some(position) = dataframe
                    .and_then(|df| dataframe_position_at_event_tick(df, &attacker_steamid, tick))
                {
                    [attacker_x, attacker_y, attacker_z] = position.map(Some);
                }
                if let Some(position) = dataframe
                    .and_then(|df| dataframe_position_at_event_tick(df, &user_steamid, tick))
                {
                    [user_x, user_y, user_z] = position.map(Some);
                }
            }

            // Skip if position data is missing
            let Some(attacker_x) = attacker_x else {
                continue;
            };
            let Some(attacker_y) = attacker_y else {
                continue;
            };
            let Some(attacker_z) = attacker_z else {
                continue;
            };
            let Some(attacker_pitch) = attacker_pitch else {
                continue;
            };
            let Some(attacker_yaw) = attacker_yaw else {
                continue;
            };
            let Some(user_x) = user_x else {
                continue;
            };
            let Some(user_y) = user_y else {
                continue;
            };
            let Some(user_z) = user_z else {
                continue;
            };

            // A player can switch sides at halftime. Prefer a tick-indexed direct
            // observation, then the event field, and only then the final roster.
            let attacker_team = team_timeline
                .and_then(|timeline| timeline.team_at(&attacker_steamid, tick))
                .or_else(|| attacker_team_num.map(team_name))
                .unwrap_or_else(|| {
                    lookup_player_team(&attacker_steamid, &attacker_name, player_info)
                });

            let victim_team = team_timeline
                .and_then(|timeline| timeline.team_at(&user_steamid, tick))
                .or_else(|| user_team_num.map(team_name))
                .unwrap_or_else(|| lookup_player_team(&user_steamid, &user_name, player_info));

            // Find the round for this kill
            let mut round_num = None;
            let mut round_start_tick = None;
            let mut round_end_tick = None;
            let mut round_freeze_end = None;

            for round in rounds {
                if round.start_tick <= tick && round.end_tick >= tick {
                    round_num = Some(round.round);
                    round_start_tick = Some(round.start_tick);
                    round_end_tick = Some(round.end_tick);
                    round_freeze_end = Some(round.freeze_end);
                    break;
                }
            }

            // If no rounds are available or kill doesn't belong to a round, use defaults
            let (round_num, round_start_tick, round_end_tick, round_freeze_end) =
                if let Some(rn) = round_num {
                    (
                        rn,
                        round_start_tick.unwrap(),
                        round_end_tick.unwrap(),
                        round_freeze_end.unwrap(),
                    )
                } else {
                    // Use default values when no round info is available
                    (1, tick, tick + 1000, tick + 450)
                };

            // Calculate distance to enemy
            let distance_to_enemy = crate::utils::calculations::calculate_distance(
                &[attacker_x, attacker_y, attacker_z],
                &[user_x, user_y, user_z],
            );

            // Create a kill event - no weapon ID mapping fallbacks
            use parser::second_pass::kill_modifiers::{KillModifiers, ATTACKER_AIRBORNE, VICTIM_AIRBORNE};
            let modifiers = KillModifiers::from_event(event);
            let kill = Kill {
                tick,
                killer_name: attacker_name.clone(),
                killer_steamid: attacker_steamid,
                killer_team: attacker_team.clone(),
                killer_pos_x: attacker_x,
                killer_pos_y: attacker_y,
                killer_pos_z: attacker_z,
                killer_view_pitch: attacker_pitch,
                killer_view_yaw: attacker_yaw,
                victim_name: user_name.clone(),
                victim_steamid: user_steamid,
                victim_team: victim_team.clone(),
                victim_pos_x: user_x,
                victim_pos_y: user_y,
                victim_pos_z: user_z,
                weapon,
                weapon_id,
                distance_to_enemy,
                headshot,
                penetrated,
                attacker_blind,
                thru_smoke,
                no_scope,
                attacker_airborne: (modifiers.known & ATTACKER_AIRBORNE != 0)
                    .then_some(modifiers.flags & ATTACKER_AIRBORNE != 0),
                victim_airborne: (modifiers.known & VICTIM_AIRBORNE != 0)
                    .then_some(modifiers.flags & VICTIM_AIRBORNE != 0),
                penetration_count: modifiers.penetrated,
                modifier_known_flags: modifiers.known,
                round: round_num,
                round_start_tick,
                round_end_tick,
                round_freeze_end,
                ticks_since_last_kill: 0, // Will be calculated later
                distance_moved_since_last_kill: 0.0, // Will be calculated later
                killer_index: 0,          // Will be set later
                victim_index: 0,          // Will be set later
                killer_is_controlling_bot: attacker_is_controlling_bot,
            };

            _successfully_parsed += 1;
            death_events.push(kill);
        }
    }

    if death_events.is_empty() {
        // Return empty list instead of failing - some demos may not have death events
        return Ok(Vec::new());
    }

    // Sort death events by tick
    death_events.sort_by_key(|k| k.tick);

    // Calculate ticks_since_last_kill and distance_moved_since_last_kill
    let mut last_kill_by_killer: HashMap<String, (i32, f32, f32, f32)> = HashMap::new();

    for kill in &mut death_events {
        if let Some((last_tick, last_x, last_y, last_z)) =
            last_kill_by_killer.get(&kill.killer_steamid)
        {
            kill.ticks_since_last_kill = kill.tick - last_tick;
            kill.distance_moved_since_last_kill = crate::utils::calculations::calculate_distance(
                &[*last_x, *last_y, *last_z],
                &[kill.killer_pos_x, kill.killer_pos_y, kill.killer_pos_z],
            );
        }

        last_kill_by_killer.insert(
            kill.killer_steamid.clone(),
            (
                kill.tick,
                kill.killer_pos_x,
                kill.killer_pos_y,
                kill.killer_pos_z,
            ),
        );
    }

    Ok(death_events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use parser::second_pass::game_events::EventField;

    fn position_test_df(
        ticks: Vec<Option<i32>>,
        steamids: Vec<Option<u64>>,
        positions: Vec<Option<[f32; 3]>>,
    ) -> AHashMap<u32, PropColumn> {
        let column = |data| PropColumn {
            data: Some(data),
            num_nones: 0,
        };
        AHashMap::from([
            (TICK_ID, column(VarVec::I32(ticks))),
            (STEAMID_ID, column(VarVec::U64(steamids))),
            (
                PLAYER_X_ID,
                column(VarVec::F32(positions.iter().map(|p| p.map(|v| v[0])).collect())),
            ),
            (
                PLAYER_Y_ID,
                column(VarVec::F32(positions.iter().map(|p| p.map(|v| v[1])).collect())),
            ),
            (
                PLAYER_Z_ID,
                column(VarVec::F32(positions.into_iter().map(|p| p.map(|v| v[2])).collect())),
            ),
        ])
    }

    #[test]
    fn death_position_fallback_is_exact_or_one_tick_prior_and_steamid_scoped() {
        let df = position_test_df(
            vec![Some(99), Some(100), Some(100)],
            vec![Some(1), Some(1), Some(2)],
            vec![Some([1.0, 2.0, 3.0]), Some([4.0, 5.0, 6.0]), Some([7.0, 8.0, 9.0])],
        );
        assert_eq!(dataframe_position_at_event_tick(&df, "1", 100), Some([4.0, 5.0, 6.0]));
        assert_eq!(dataframe_position_at_event_tick(&df, "2", 100), Some([7.0, 8.0, 9.0]));
        assert_eq!(dataframe_position_at_event_tick(&df, "1", 101), Some([4.0, 5.0, 6.0]));
        assert_eq!(dataframe_position_at_event_tick(&df, "3", 100), None);
    }

    #[test]
    fn death_position_fallback_rejects_conflicting_rows_at_the_same_tick() {
        let df = position_test_df(
            vec![Some(100), Some(100), Some(99)],
            vec![Some(1), Some(1), Some(1)],
            vec![Some([1.0, 2.0, 3.0]), Some([4.0, 5.0, 6.0]), Some([7.0, 8.0, 9.0])],
        );
        assert_eq!(dataframe_position_at_event_tick(&df, "1", 100), None);
    }

    /// Field names exactly as CS2 emits them, verified against real demos on patches
    /// 13984 and 14055. The unseparated spellings are deliberate: `attacker_blind`,
    /// `thru_smoke` and `no_scope` silently never matched and pinned those flags to false.
    fn death_event(extra: Vec<(&str, Variant)>) -> ParserGameEvent {
        let mut fields = vec![
            ("tick", Variant::I32(5000)),
            ("attacker_name", Variant::String("killer".into())),
            ("attacker_steamid", Variant::U64(1)),
            ("attacker_team_num", Variant::I32(2)),
            ("user_name", Variant::String("victim".into())),
            ("user_steamid", Variant::U64(2)),
            ("user_team_num", Variant::I32(3)),
            ("attacker_X", Variant::F32(0.0)),
            ("attacker_Y", Variant::F32(0.0)),
            ("attacker_Z", Variant::F32(0.0)),
            ("attacker_pitch", Variant::F32(0.0)),
            ("attacker_yaw", Variant::F32(0.0)),
            ("user_X", Variant::F32(3.0)),
            ("user_Y", Variant::F32(4.0)),
            ("user_Z", Variant::F32(0.0)),
        ];
        fields.extend(extra);
        ParserGameEvent {
            name: "player_death".to_string(),
            tick: 5000,
            fields: fields
                .into_iter()
                .map(|(name, data)| EventField {
                    name: name.to_string(),
                    data: Some(data),
                })
                .collect(),
        }
    }

    fn parse_one(extra: Vec<(&str, Variant)>) -> Kill {
        let events = vec![death_event(extra)];
        let mut kills = parse_death_events(&events, &[], &[]).unwrap();
        assert_eq!(kills.len(), 1);
        kills.remove(0)
    }

    #[test]
    fn reads_the_unseparated_flag_names_cs2_actually_emits() {
        let kill = parse_one(vec![
            ("weapon", Variant::String("ak47".into())),
            ("headshot", Variant::Bool(true)),
            ("penetrated", Variant::I32(2)),
            ("attackerblind", Variant::Bool(true)),
            ("thrusmoke", Variant::Bool(true)),
            ("noscope", Variant::Bool(true)),
            ("attacker_is_airborne", Variant::Bool(true)),
            ("user_is_airborne", Variant::Bool(false)),
        ]);
        assert!(kill.headshot);
        assert!(kill.penetrated);
        assert!(kill.attacker_blind);
        assert!(kill.thru_smoke);
        assert!(kill.no_scope);
        assert_eq!(kill.attacker_airborne, Some(true));
        assert_eq!(kill.victim_airborne, Some(false));
        assert_eq!(kill.penetration_count, Some(2));
        assert_eq!(kill.modifier_known_flags, 127);
    }

    #[test]
    fn ignores_the_separated_spellings_that_never_appear_in_demos() {
        let kill = parse_one(vec![
            ("weapon", Variant::String("ak47".into())),
            ("attacker_blind", Variant::Bool(true)),
            ("thru_smoke", Variant::Bool(true)),
            ("no_scope", Variant::Bool(true)),
        ]);
        assert!(!kill.attacker_blind);
        assert!(!kill.thru_smoke);
        assert!(!kill.no_scope);
    }

    #[test]
    fn weapon_name_wins_over_the_attackers_active_weapon() {
        // A molotov kill still reports the rifle in the attacker's hands.
        let kill = parse_one(vec![
            ("weapon", Variant::String("inferno".into())),
            ("attacker_item_def_idx", Variant::U32(7)),
        ]);
        assert_eq!(kill.weapon_id, "48");
    }

    #[test]
    fn unknown_weapon_names_fall_back_to_the_def_index() {
        let kill = parse_one(vec![
            ("weapon", Variant::String("knife_m9_bayonet".into())),
            ("attacker_item_def_idx", Variant::U32(508)),
        ]);
        assert_eq!(kill.weapon_id, "508");
    }

    #[test]
    fn non_weapon_kill_sources_never_borrow_the_def_index() {
        let kill = parse_one(vec![
            ("weapon", Variant::String("world".into())),
            ("attacker_item_def_idx", Variant::U32(7)),
        ]);
        assert_eq!(kill.weapon_id, "0");
    }

    #[test]
    fn resolves_missing_death_team_fields_from_the_tick_indexed_side_timeline() {
        // `player_md` is terminal: this roster is deliberately the inverse of
        // the early-half samples below. A parser must not label the first kill
        // with this terminal state merely because player_death omitted sides.
        let terminal_roster = vec![
            PlayerInfo {
                index: 4,
                name: "killer".into(),
                steamid: "1".into(),
                team: "CT".into(),
            },
            PlayerInfo {
                index: 5,
                name: "victim".into(),
                steamid: "2".into(),
                team: "T".into(),
            },
        ];

        let mut early = death_event(vec![("weapon", Variant::String("ak47".into()))]);
        early.tick = 100;
        for field in &mut early.fields {
            if field.name == "tick" {
                field.data = Some(Variant::I32(100));
            }
        }
        early
            .fields
            .retain(|field| field.name != "attacker_team_num" && field.name != "user_team_num");

        let mut late = early.clone();
        late.tick = 200;
        for field in &mut late.fields {
            if field.name == "tick" {
                field.data = Some(Variant::I32(200));
            }
        }

        // This production-shaped stream has no first-half dataframe samples.
        // Halftime's `oldteam` is direct evidence for the early side, while
        // `team` resolves the late side after the transition.
        let changes = vec![
            ParserGameEvent {
                name: "player_team".into(),
                tick: 150,
                fields: vec![
                    EventField {
                        name: "tick".into(),
                        data: Some(Variant::I32(150)),
                    },
                    EventField {
                        name: "user_steamid".into(),
                        data: Some(Variant::U64(1)),
                    },
                    EventField {
                        name: "team".into(),
                        data: Some(Variant::I32(3)),
                    },
                    EventField {
                        name: "oldteam".into(),
                        data: Some(Variant::I32(2)),
                    },
                ],
            },
            ParserGameEvent {
                name: "player_team".into(),
                tick: 150,
                fields: vec![
                    EventField {
                        name: "tick".into(),
                        data: Some(Variant::I32(150)),
                    },
                    EventField {
                        name: "user_steamid".into(),
                        data: Some(Variant::U64(2)),
                    },
                    EventField {
                        name: "team".into(),
                        data: Some(Variant::I32(2)),
                    },
                    EventField {
                        name: "oldteam".into(),
                        data: Some(Variant::I32(3)),
                    },
                ],
            },
        ];
        let timeline = TeamTimeline::from_samples(&[], &changes);
        let kills = parse_death_events_with_timeline(
            &[early, late],
            &[],
            &terminal_roster,
            Some(&timeline),
            None,
        )
        .unwrap();

        assert_eq!(
            (kills[0].killer_team.as_str(), kills[0].victim_team.as_str()),
            ("T", "CT")
        );
        assert_eq!(
            (kills[1].killer_team.as_str(), kills[1].victim_team.as_str()),
            ("CT", "T")
        );
    }
}
