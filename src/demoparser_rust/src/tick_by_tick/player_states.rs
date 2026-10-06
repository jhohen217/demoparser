//! Timed player states and eye positions. Smoke visibility deliberately requires
//! a density provider: network smoke collision masks are not visible smoke.
use super::data_types::TickRecord;
use anyhow::Result;
use parser::second_pass::game_events::GameEvent;
use parser::second_pass::kill_modifiers::field;
use parser::second_pass::variants::Variant;
use std::collections::HashMap;

pub const ALIVE: u8 = 1;
pub const AIRBORNE: u8 = 2;
pub const SCOPED: u8 = 4;
pub const BLIND: u8 = 8;
pub const SMOKE_VIEW_OBSTRUCTED: u8 = 16;
/// S2R's CS2 playback clock is fixed at 64 ticks per second.
pub const TICK_RATE: f64 = 64.0;
pub const EYE_UNKNOWN: u8 = 0;
pub const EYE_RECORDED: u8 = 1;
pub const EYE_RECONSTRUCTED: u8 = 2;

#[derive(Debug, Clone, PartialEq)]
pub struct PlayerState {
    pub tick: i32,
    pub flags: u8,
    pub known: u8,
    pub flash_remaining: Option<f32>,
    pub flash_duration: Option<f32>,
    pub flash_max_alpha: Option<f32>,
    pub eye_position: Option<[f32; 3]>,
    pub eye_source: u8,
}

fn eye_position(row: &TickRecord) -> (Option<[f32; 3]>, u8) {
    if row.alive == 0
        || ![row.pos_x, row.pos_y, row.pos_z]
            .iter()
            .all(|v| v.is_finite())
    {
        return (None, EYE_UNKNOWN);
    }
    let (offset, source) = if let Some(offset) = row
        .state
        .view_offset
        .filter(|offset| offset.iter().all(|v| v.is_finite()))
    {
        (offset, EYE_RECORDED)
    } else {
        // Same reconstruction as S2DVR's POV camera: 64 standing, 46 ducked.
        // Interpolate with the network duck fraction so transitions do not snap.
        let duck = row
            .state
            .duck_amount
            .filter(|v| v.is_finite())
            .unwrap_or(if row.crouching > 0 { 1.0 } else { 0.0 })
            .clamp(0.0, 1.0);
        ([0.0, 0.0, 64.0 - 18.0 * duck], EYE_RECONSTRUCTED)
    };
    (
        Some([
            row.pos_x + offset[0],
            row.pos_y + offset[1],
            row.pos_z + offset[2],
        ]),
        source,
    )
}

/// A renderer can answer this using reconstructed visible density at the eye,
/// including smoke growth/fade and bullet/grenade deformation. `None` means
/// unavailable. Feet, collision masks, and a radius around a grenade cannot
/// implement this contract. Target-specific line-of-sight is a separate query.
pub trait SmokeViewObstruction {
    fn at_eye(&self, tick: i32, eye: [f32; 3]) -> Option<bool>;
}

impl PlayerState {
    pub fn apply_smoke_density(&mut self, density: &impl SmokeViewObstruction) {
        self.flags &= !SMOKE_VIEW_OBSTRUCTED;
        self.known &= !SMOKE_VIEW_OBSTRUCTED;
        if self.flags & ALIVE == 0 {
            return;
        }
        if let Some(obstructed) = self
            .eye_position
            .and_then(|eye| density.at_eye(self.tick, eye))
        {
            self.known |= SMOKE_VIEW_OBSTRUCTED;
            self.flags = (self.flags & !SMOKE_VIEW_OBSTRUCTED)
                | if obstructed { SMOKE_VIEW_OBSTRUCTED } else { 0 };
        }
    }
}

fn steamid(event: &GameEvent) -> Option<u64> {
    match field(event, "user_steamid")? {
        Variant::U64(v) => Some(*v),
        Variant::String(v) => v.parse().ok(),
        _ => None,
    }
}

/// Input rows may be sparse or unordered. Event history before the output window
/// is retained so a flash just before a clip still expires at the correct time.
pub fn track_player_states(rows: &[TickRecord], events: &[&GameEvent]) -> Vec<PlayerState> {
    let mut rows: Vec<_> = rows.iter().collect();
    rows.sort_by_key(|row| row.tick);
    let mut events = events.to_vec();
    events.sort_by_key(|event| event.tick);
    let mut cursor = 0;
    let mut flash_end: Option<f64> = None;
    let mut previous_pawn = None;
    let mut previous_alive = None;
    let mut previous_duration = None;
    let mut result = Vec::with_capacity(rows.len());
    for row in rows {
        let alive = row.alive > 0;
        if previous_pawn.is_some()
            && row.pawn_entity_id.is_some()
            && previous_pawn != row.pawn_entity_id
            || previous_alive == Some(false) && alive
        {
            flash_end = None;
        }
        previous_pawn = row.pawn_entity_id;
        previous_alive = Some(alive);
        let mut observed_flash = false;
        while cursor < events.len() && events[cursor].tick <= row.tick {
            let event = events[cursor];
            match event.name.as_str() {
                "player_spawn" | "player_death" => flash_end = Some(f64::from(event.tick)),
                "player_blind" => {
                    if let Some(Variant::F32(duration)) = field(event, "blind_duration") {
                        if duration.is_finite() && *duration >= 0.0 {
                            observed_flash = true;
                            let end = f64::from(event.tick) + f64::from(*duration) * TICK_RATE;
                            flash_end = Some(flash_end.map_or(end, |old| old.max(end)));
                        }
                    }
                }
                _ => {}
            }
            cursor += 1;
        }
        let clean = |v: Option<f32>| v.filter(|v| v.is_finite() && *v >= 0.0);
        let duration = clean(row.state.flash_duration);
        // A newly changed positive duration without its originating event has
        // no reliable start time (e.g. a partial capture). Do not reuse an old expiry.
        if duration.is_some_and(|v| v > 0.0) && duration != previous_duration && !observed_flash {
            flash_end = None;
        }
        previous_duration = duration;
        let remaining = if !alive || duration == Some(0.0) {
            flash_end = Some(f64::from(row.tick));
            Some(0.0)
        } else {
            flash_end.map(|end| ((end - f64::from(row.tick)) / TICK_RATE).max(0.0) as f32)
        };
        let (eye_position, eye_source) = eye_position(row);
        let mut state = PlayerState {
            tick: row.tick,
            flags: u8::from(alive),
            known: ALIVE,
            flash_remaining: remaining,
            flash_duration: duration,
            flash_max_alpha: clean(row.state.flash_max_alpha),
            eye_position,
            eye_source,
        };
        for (bit, value) in [
            (AIRBORNE, row.state.airborne),
            (SCOPED, row.state.scoped),
            (BLIND, remaining.map(|v| v > 0.0)),
        ] {
            if let Some(value) = value {
                state.known |= bit;
                if value {
                    state.flags |= bit;
                }
            }
        }
        result.push(state);
    }
    result
}

pub fn encode_player_states(
    players: &[u64],
    records: &HashMap<u64, Vec<TickRecord>>,
    events: &[GameEvent],
    first: i32,
    last: i32,
) -> Result<(Vec<u8>, u32)> {
    let mut by_player: HashMap<u64, Vec<&GameEvent>> = HashMap::new();
    for event in events.iter().filter(|e| {
        matches!(
            e.name.as_str(),
            "player_blind" | "player_spawn" | "player_death"
        )
    }) {
        if let Some(sid) = steamid(event) {
            by_player.entry(sid).or_default().push(event);
        }
    }
    let mut bytes = vec![0; 4];
    let mut count = 0u32;
    for (index, sid) in players.iter().enumerate() {
        let Some(rows) = records.get(sid) else {
            continue;
        };
        let history = by_player.get(sid).map(Vec::as_slice).unwrap_or(&[]);
        let mut previous: Option<PlayerState> = None;
        for state in track_player_states(rows, history)
            .into_iter()
            .filter(|r| r.tick >= first && r.tick <= last)
        {
            let unchanged = previous.as_ref().is_some_and(|old| {
                i64::from(state.tick) == i64::from(old.tick) + 1
                    && state.flags == old.flags
                    && state.known == old.known
                    && state.flash_remaining == old.flash_remaining
                    && state.flash_duration == old.flash_duration
                    && state.flash_max_alpha == old.flash_max_alpha
                    && state.eye_source == old.eye_source
                    && state.eye_position == old.eye_position
            });
            if unchanged {
                previous = Some(state);
                continue;
            }
            bytes.extend_from_slice(&state.tick.to_le_bytes());
            bytes.extend_from_slice(&[
                u8::try_from(index)?,
                state.flags,
                state.known,
                state.eye_source,
            ]);
            let floats = [
                state.flash_remaining,
                state.flash_duration,
                state.flash_max_alpha,
                state.eye_position.map(|p| p[0]),
                state.eye_position.map(|p| p[1]),
                state.eye_position.map(|p| p[2]),
            ];
            for value in floats {
                bytes.extend_from_slice(&value.unwrap_or(f32::from_bits(0x7fc00000)).to_le_bytes());
            }
            count = count
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("too many player state rows"))?;
            previous = Some(state);
        }
    }
    bytes[..4].copy_from_slice(&count.to_le_bytes());
    Ok((bytes, count))
}

#[cfg(test)]
mod tests {
    use super::super::data_types::PlayerStateObservation;
    use super::*;
    use parser::second_pass::game_events::EventField;

    fn row(tick: i32) -> TickRecord {
        TickRecord {
            tick,
            alive: 1,
            pawn_entity_id: Some(10),
            pos_z: 100.0,
            state: PlayerStateObservation {
                flash_duration: Some(2.0),
                view_offset: Some([0.0, 0.0, 64.0]),
                ..Default::default()
            },
            ..Default::default()
        }
    }
    fn event(name: &str, tick: i32, duration: f32) -> GameEvent {
        GameEvent {
            name: name.into(),
            tick,
            fields: vec![
                EventField {
                    name: "user_steamid".into(),
                    data: Some(Variant::U64(11)),
                },
                EventField {
                    name: "blind_duration".into(),
                    data: Some(Variant::F32(duration)),
                },
            ],
        }
    }

    #[test]
    fn flash_before_clip_expires_even_when_raw_duration_stays_nonzero() {
        let flash = event("player_blind", 100, 2.0);
        let states = track_player_states(&[row(228), row(164)], &[&flash]);
        assert_eq!(states[0].flash_remaining, Some(1.0));
        assert_eq!(states[1].flash_remaining, Some(0.0));
        assert_eq!(states[1].flags & BLIND, 0);
        assert_ne!(states[1].known & BLIND, 0);
    }

    #[test]
    fn repeated_equal_duration_flashes_refresh_and_death_resets() {
        let first = event("player_blind", 100, 2.0);
        let second = event("player_blind", 200, 2.0);
        let death = event("player_death", 250, 0.0);
        let mut dead = row(250);
        dead.alive = 0;
        let mut respawn = row(300);
        respawn.pawn_entity_id = Some(20);
        let states = track_player_states(
            &[row(164), row(228), dead, respawn],
            &[&first, &second, &death],
        );
        assert_eq!(states[1].flash_remaining, Some(100.0 / 64.0));
        assert_eq!(states[2].flash_remaining, Some(0.0));
        assert_eq!(states[3].flash_remaining, None);
    }

    #[test]
    fn missing_timing_and_malformed_events_are_unknown_not_unblinded() {
        let bad = event("player_blind", 100, f32::NAN);
        let state = track_player_states(&[row(164)], &[&bad]).remove(0);
        assert_eq!(state.flash_remaining, None);
        assert_eq!(state.known & BLIND, 0);
        let mut clear = row(164);
        clear.state.flash_duration = Some(0.0);
        let state = track_player_states(&[clear, row(165)], &[]);
        assert_eq!(state[0].flash_remaining, Some(0.0));
        assert_eq!(state[1].flash_remaining, None);
    }

    #[test]
    fn smoke_queries_the_eyes_and_never_assumes_clear_without_density() {
        struct Density;
        impl SmokeViewObstruction for Density {
            fn at_eye(&self, _: i32, eye: [f32; 3]) -> Option<bool> {
                assert_eq!(eye, [0.0, 0.0, 164.0]);
                Some(true)
            }
        }
        let mut state = track_player_states(&[row(100)], &[]).remove(0);
        assert_eq!(state.known & SMOKE_VIEW_OBSTRUCTED, 0);
        state.apply_smoke_density(&Density);
        assert_ne!(state.known & SMOKE_VIEW_OBSTRUCTED, 0);
        assert_ne!(state.flags & SMOKE_VIEW_OBSTRUCTED, 0);
        let mut crouched = row(101);
        crouched.state.view_offset = Some([0.0, 0.0, 46.0]);
        assert_eq!(
            track_player_states(&[crouched], &[])[0].eye_position,
            Some([0.0, 0.0, 146.0])
        );
    }

    #[test]
    fn serialized_states_keep_player_identity_window_and_unknown_sentinels() {
        let flash = event("player_blind", 100, 2.0);
        let records = HashMap::from([(11, vec![row(164), row(228)])]);
        let (bytes, count) = encode_player_states(&[22, 11], &records, &[flash], 164, 164).unwrap();
        assert_eq!(count, 1);
        assert_eq!(bytes.len(), 4 + 32);
        assert_eq!(&bytes[..4], &1u32.to_le_bytes());
        assert_eq!(bytes[8], 1);
        assert_eq!(f32::from_le_bytes(bytes[12..16].try_into().unwrap()), 1.0);
        assert_eq!(&bytes[20..24], &0x7fc00000u32.to_le_bytes());
        assert_eq!(f32::from_le_bytes(bytes[32..36].try_into().unwrap()), 164.0);
        assert_eq!(bytes[10] & SMOKE_VIEW_OBSTRUCTED, 0);
    }

    #[test]
    fn reconstructs_64_unit_eyes_with_smooth_crouch_and_tracks_airborne_origin() {
        let mut standing = row(100);
        standing.state.view_offset = None;
        let mut halfway = standing.clone();
        halfway.tick = 101;
        halfway.state.duck_amount = Some(0.5);
        let mut crouched = standing.clone();
        crouched.tick = 102;
        crouched.crouching = 1;
        let mut jumping = standing.clone();
        jumping.tick = 103;
        jumping.pos_z = 150.0;
        let states = track_player_states(&[standing, halfway, crouched, jumping], &[]);
        assert_eq!(states[0].eye_position, Some([0.0, 0.0, 164.0]));
        assert_eq!(states[1].eye_position, Some([0.0, 0.0, 155.0]));
        assert_eq!(states[2].eye_position, Some([0.0, 0.0, 146.0]));
        assert_eq!(states[3].eye_position, Some([0.0, 0.0, 214.0]));
        assert!(states.iter().all(|s| s.eye_source == EYE_RECONSTRUCTED));
    }

    #[test]
    fn sparse_states_omit_unchanged_ticks_but_restart_after_gaps() {
        let records = HashMap::from([(11, vec![row(100), row(101), row(105)])]);
        let (bytes, count) = encode_player_states(&[11], &records, &[], 100, 105).unwrap();
        assert_eq!(count, 2);
        assert_eq!(i32::from_le_bytes(bytes[4..8].try_into().unwrap()), 100);
        assert_eq!(i32::from_le_bytes(bytes[36..40].try_into().unwrap()), 105);
    }
}
