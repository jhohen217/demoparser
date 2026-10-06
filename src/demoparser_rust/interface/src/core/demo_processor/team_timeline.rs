//! Tick-indexed player-side information.
//!
//! `player_md` and the controller roster describe the final state of a demo.  They
//! are therefore not valid evidence for a kill before halftime.  This module keeps
//! only time-stamped observations from the parsed dataframe and `player_team`
//! events, then resolves the most recent observation at a requested tick.

use ahash::AHashMap;
use parser::first_pass::prop_controller::{PropController, STEAMID_ID, TICK_ID};
use parser::parse_demo::DemoOutput;
use parser::second_pass::game_events::GameEvent;
use parser::second_pass::variants::{PropColumn, VarVec, Variant};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy)]
struct TeamSample {
    tick: i32,
    /// Preserves source order for multiple observations at the same tick.  A
    /// `player_team` event is appended after dataframe values and wins that tie.
    order: usize,
    team_number: i32,
    /// A `player_team` transition tells us which valid side the player was on
    /// immediately before the event. This is essential for first-half clips
    /// when Source only emitted the halftime transition, not live dataframe
    /// rows, as in the production Anubis demo.
    previous_team_number: Option<i32>,
}

#[derive(Debug, Default)]
pub(super) struct TeamTimeline {
    samples_by_steamid: HashMap<u64, Vec<TeamSample>>,
}

impl TeamTimeline {
    pub(super) fn from_output(output: &DemoOutput) -> Self {
        let mut timeline = Self::default();
        let mut order = 0;

        let team_columns = team_column_ids(&output.df, &output.prop_controller);
        let ticks = output
            .df
            .get(&TICK_ID)
            .and_then(|column| column.data.as_ref());
        let steamids = output
            .df
            .get(&STEAMID_ID)
            .and_then(|column| column.data.as_ref());

        if let (Some(VarVec::I32(ticks)), Some(VarVec::U64(steamids))) = (ticks, steamids) {
            for row in 0..ticks.len().min(steamids.len()) {
                let (Some(tick), Some(steamid)) = (ticks[row], steamids[row]) else {
                    continue;
                };
                for team_column in &team_columns {
                    let Some(team_number) = team_value(team_column, row) else {
                        continue;
                    };
                    if valid_team_number(team_number) {
                        timeline.push(steamid, tick, order, team_number, None);
                        order += 1;
                        // The columns are ordered by confidence. One direct sample is
                        // enough; do not accidentally replace it with an alias column.
                        break;
                    }
                }
            }
        }

        // The event stream fills sparse/missing dataframe samples and is also the
        // authoritative tie-breaker when Source emits a side update at a tick.
        for event in &output.game_events {
            if event.name != "player_team" {
                continue;
            }
            if let Some(change) = player_team_event(event) {
                timeline.push(
                    change.steamid,
                    change.tick,
                    order,
                    change.team_number,
                    change.old_team_number,
                );
                order += 1;
            }
        }

        timeline.finish();
        timeline
    }

    #[cfg(test)]
    pub(super) fn from_samples(samples: &[(u64, i32, i32)], events: &[GameEvent]) -> Self {
        let mut timeline = Self::default();
        let mut order = 0;
        for &(steamid, tick, team_number) in samples {
            if valid_team_number(team_number) {
                timeline.push(steamid, tick, order, team_number, None);
                order += 1;
            }
        }
        for event in events {
            if event.name == "player_team" {
                if let Some(change) = player_team_event(event) {
                    timeline.push(
                        change.steamid,
                        change.tick,
                        order,
                        change.team_number,
                        change.old_team_number,
                    );
                    order += 1;
                }
            }
        }
        timeline.finish();
        timeline
    }

    pub(super) fn team_at(&self, steamid: &str, tick: i32) -> Option<String> {
        let steamid = steamid.parse::<u64>().ok()?;
        let samples = self.samples_by_steamid.get(&steamid)?;
        let index = samples.partition_point(|sample| sample.tick <= tick);
        if let Some(index) = index.checked_sub(1) {
            return Some(team_name(samples[index].team_number));
        }

        // No sample at or before the queried tick. A later `player_team` event
        // with `oldteam` is still direct event-time evidence for the preceding
        // side; use the closest such transition rather than the terminal roster.
        samples
            .iter()
            .find_map(|sample| sample.previous_team_number)
            .map(team_name)
    }

    fn push(
        &mut self,
        steamid: u64,
        tick: i32,
        order: usize,
        team_number: i32,
        previous_team_number: Option<i32>,
    ) {
        self.samples_by_steamid
            .entry(steamid)
            .or_default()
            .push(TeamSample {
                tick,
                order,
                team_number,
                previous_team_number,
            });
    }

    fn finish(&mut self) {
        for samples in self.samples_by_steamid.values_mut() {
            samples.sort_by_key(|sample| (sample.tick, sample.order));
        }
    }
}

pub(super) fn team_name(team_number: i32) -> String {
    match team_number {
        2 => "T".to_string(),
        3 => "CT".to_string(),
        _ => "UNKNOWN".to_string(),
    }
}

fn valid_team_number(team_number: i32) -> bool {
    matches!(team_number, 2 | 3)
}

fn team_column_ids<'a>(
    df: &'a AHashMap<u32, PropColumn>,
    prop_controller: &'a PropController,
) -> Vec<&'a PropColumn> {
    // Prefer the pawn's live side, then controller and friendly aliases. Avoid
    // iterating the map directly because AHashMap order is intentionally unstable.
    const NAMES: &[&str] = &[
        "CCSPlayerPawn.m_iTeamNum",
        "CCSPlayerController.m_iTeamNum",
        "team_num",
        "m_iTeamNum",
        "team_number",
        "team",
    ];

    NAMES
        .iter()
        .filter_map(|name| {
            prop_controller
                .id_to_name
                .iter()
                .find_map(|(id, actual_name)| (actual_name == name).then_some(*id))
                .and_then(|id| df.get(&id))
        })
        .collect()
}

fn team_value(column: &PropColumn, row: usize) -> Option<i32> {
    match column.data.as_ref()? {
        VarVec::I32(values) => values.get(row).copied().flatten(),
        VarVec::U32(values) => values.get(row).copied().flatten().map(|value| value as i32),
        _ => None,
    }
}

struct PlayerTeamChange {
    tick: i32,
    steamid: u64,
    team_number: i32,
    old_team_number: Option<i32>,
}

fn player_team_event(event: &GameEvent) -> Option<PlayerTeamChange> {
    let mut tick = None;
    let mut steamid = None;
    let mut team_number = None;
    let mut old_team_number = None;

    for field in &event.fields {
        match field.name.as_str() {
            "tick" => match field.data.as_ref() {
                Some(Variant::I32(value)) => tick = Some(*value),
                Some(Variant::U32(value)) => tick = Some(*value as i32),
                _ => {}
            },
            "user_steamid" => match field.data.as_ref() {
                Some(Variant::U64(value)) => steamid = Some(*value),
                Some(Variant::String(value)) => steamid = value.parse().ok(),
                _ => {}
            },
            "team" => match field.data.as_ref() {
                Some(Variant::I32(value)) => team_number = Some(*value),
                Some(Variant::U32(value)) => team_number = Some(*value as i32),
                _ => {}
            },
            "oldteam" => match field.data.as_ref() {
                Some(Variant::I32(value)) => old_team_number = Some(*value),
                Some(Variant::U32(value)) => old_team_number = Some(*value as i32),
                _ => {}
            },
            _ => {}
        }
    }

    let (tick, steamid, team_number) = (tick?, steamid?, team_number?);
    valid_team_number(team_number).then_some(PlayerTeamChange {
        tick,
        steamid,
        team_number,
        old_team_number: old_team_number.filter(|team| valid_team_number(*team)),
    })
}
