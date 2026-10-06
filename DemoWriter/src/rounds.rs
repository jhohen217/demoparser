//! Round boundaries, derived the same way the rest of this project derives them.
//!
//! Ported from `interface/src/core/demo_processor/round_parser.rs` rather than depended on,
//! so DemoWriter does not have to join the `demoparser_rust` workspace. Keep the two in
//! step: a round number here must mean the same round it means everywhere else.

use ahash::AHashMap;
use anyhow::{bail, Context, Result};
use parser::first_pass::parser_settings::{rm_user_friendly_names, ParserInputs};
use parser::parse_demo::{Parser, ParsingMode};
use parser::second_pass::game_events::GameEvent;
use parser::second_pass::parser_settings::create_huffman_lookup_table;
use parser::second_pass::variants::Variant;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Round {
    pub round: i32,
    pub start_tick: i32,
    pub freeze_end: i32,
    pub end_tick: i32,
    /// Next observed round start, including a round whose end was not recorded.
    pub next_start_tick: Option<i32>,
    /// `round_officially_ended` is a synthesised event and is not always emitted — notably
    /// never for round 1.
    pub officially_ended: Option<i32>,
    pub winner: String,
    pub win_reason: String,
}

fn field<'a>(event: &'a GameEvent, name: &str) -> Option<&'a Variant> {
    event
        .fields
        .iter()
        .find(|field| field.name == name)
        .and_then(|field| field.data.as_ref())
}

fn integer_field(event: &GameEvent, name: &str) -> Option<i32> {
    match field(event, name)? {
        Variant::I32(value) => Some(*value),
        Variant::U32(value) => i32::try_from(*value).ok(),
        Variant::U64(value) => i32::try_from(*value).ok(),
        Variant::String(value) => value.parse().ok(),
        _ => None,
    }
}

fn event_tick(event: &GameEvent) -> i32 {
    integer_field(event, "tick").unwrap_or(event.tick)
}

fn is_warmup(event: &GameEvent) -> bool {
    matches!(field(event, "is_warmup_period"), Some(Variant::Bool(true)))
}

fn winner(event: &GameEvent) -> Option<String> {
    match field(event, "winner")? {
        Variant::String(value) => match value.as_str() {
            "2" | "T" => Some("T".to_string()),
            "3" | "CT" => Some("CT".to_string()),
            _ => None,
        },
        Variant::I32(2) | Variant::U32(2) => Some("T".to_string()),
        Variant::I32(3) | Variant::U32(3) => Some("CT".to_string()),
        _ => None,
    }
}

fn reason(event: &GameEvent) -> Option<String> {
    match field(event, "reason")? {
        Variant::String(value) => Some(value.clone()),
        Variant::I32(value) => Some(reason_name(*value).to_string()),
        Variant::U32(value) => i32::try_from(*value)
            .ok()
            .map(|value| reason_name(value).to_string()),
        _ => None,
    }
}

fn reason_name(reason: i32) -> &'static str {
    parser::maps::ROUND_WIN_REASON
        .get(&reason)
        .copied()
        .unwrap_or("unknown")
}

struct RoundEnd {
    tick: i32,
    round: Option<i32>,
    winner: Option<String>,
    reason: Option<String>,
}

pub fn build_rounds(events: &[GameEvent]) -> Result<Vec<Round>> {
    let mut start_ticks: Vec<i32> = events
        .iter()
        .filter(|event| event.name == "round_start" && !is_warmup(event))
        .map(event_tick)
        .collect();
    let mut freeze_ticks: Vec<i32> = events
        .iter()
        .filter(|event| event.name == "round_freeze_end" && !is_warmup(event))
        .map(event_tick)
        .collect();
    let mut officially_ended: Vec<i32> = events
        .iter()
        .filter(|event| event.name == "round_officially_ended")
        .map(event_tick)
        .collect();
    let mut round_ends: Vec<RoundEnd> = events
        .iter()
        .filter(|event| event.name == "round_end" && !is_warmup(event))
        .map(|event| RoundEnd {
            tick: event_tick(event),
            round: integer_field(event, "round"),
            winner: winner(event),
            reason: reason(event),
        })
        .collect();

    start_ticks.sort_unstable();
    freeze_ticks.sort_unstable();
    officially_ended.sort_unstable();
    round_ends.sort_by_key(|event| event.tick);

    if round_ends.is_empty() {
        bail!("no non-warmup round_end events found in this demo");
    }

    let mut rounds: Vec<Round> = Vec::with_capacity(round_ends.len());
    let mut previous_end: Option<i32> = None;

    for round_end in round_ends {
        let lower_bound = previous_end.unwrap_or(i32::MIN);
        let start_tick = start_ticks
            .iter()
            .copied()
            .filter(|tick| *tick > lower_bound && *tick <= round_end.tick)
            .max()
            .unwrap_or_else(|| previous_end.map_or(0, |tick| tick.saturating_add(1)));
        let freeze_end = freeze_ticks
            .iter()
            .copied()
            .filter(|tick| *tick >= start_tick && *tick <= round_end.tick)
            .max()
            .unwrap_or(start_tick);

        previous_end = Some(round_end.tick);
        if start_tick >= round_end.tick {
            continue;
        }

        let ordinal = i32::try_from(rounds.len() + 1).unwrap_or(i32::MAX);
        let round_number = round_end
            .round
            .filter(|round| *round > 0)
            .unwrap_or(ordinal);
        rounds.push(Round {
            round: round_number,
            start_tick,
            freeze_end,
            end_tick: round_end.tick,
            next_start_tick: start_ticks
                .iter()
                .copied()
                .find(|tick| *tick > round_end.tick),
            officially_ended: None,
            winner: round_end.winner.unwrap_or_else(|| "UNKNOWN".to_string()),
            win_reason: round_end.reason.unwrap_or_else(|| "unknown".to_string()),
        });
    }

    // Attach the first officially-ended tick that falls after each round's end and before
    // the next round's start.
    for i in 0..rounds.len() {
        let end = rounds[i].end_tick;
        let next_start = rounds.get(i + 1).map(|r| r.start_tick).unwrap_or(i32::MAX);
        rounds[i].officially_ended = officially_ended
            .iter()
            .copied()
            .find(|tick| *tick >= end && *tick < next_start);
    }

    Ok(rounds)
}

/// Resolving rounds costs a full parse of the source demo — by far the most expensive step
/// in a trim. Only the four round events and the props they need are requested.
pub fn parse_rounds(demo: &[u8]) -> Result<Vec<Round>> {
    parse_rounds_with_animation_recipes(demo, false)
}

fn parse_rounds_with_animation_recipes(
    demo: &[u8],
    capture_animation_recipes: bool,
) -> Result<Vec<Round>> {
    let wanted_other_props = vec![
        "total_rounds_played".to_string(),
        "is_warmup_period".to_string(),
    ];
    let real_other_props = rm_user_friendly_names(&wanted_other_props)
        .map_err(|e| anyhow::anyhow!("could not resolve game properties: {e}"))?;
    let mut real_name_to_og_name = AHashMap::default();
    for (real_name, friendly) in real_other_props.iter().zip(&wanted_other_props) {
        real_name_to_og_name.insert(real_name.clone(), friendly.clone());
    }

    let huffman_lookup_table = create_huffman_lookup_table().to_vec();
    let settings = ParserInputs {
        real_name_to_og_name,
        wanted_players: vec![],
        wanted_player_props: vec![],
        wanted_other_props: real_other_props,
        wanted_prop_states: AHashMap::default(),
        wanted_ticks: vec![],
        wanted_events: vec![
            "round_start".to_string(),
            "round_freeze_end".to_string(),
            "round_end".to_string(),
            "round_officially_ended".to_string(),
        ],
        parse_ents: true,
        parse_projectiles: false,
        parse_grenades: false,
        only_header: false,
        only_convars: false,
        huffman_lookup_table: &huffman_lookup_table,
        order_by_steamid: false,
        list_props: false,
        fallback_bytes: None,
    };

    // Verification needs game-rule entities, but not per-tick animation snapshots.
    let mut parser = Parser::new(settings, ParsingMode::Normal)
        .with_animation_recipes(capture_animation_recipes);
    let output = parser
        .parse_demo(demo)
        .map_err(|e| anyhow::anyhow!("could not parse the source demo: {e}"))
        .context("round resolution")?;
    build_rounds(&output.game_events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use parser::second_pass::game_events::EventField;

    #[test]
    #[ignore = "set ROUND_VERIFICATION_DEMO to a real source or verified clip"]
    fn animation_capture_does_not_change_round_verification() {
        let path = std::env::var("ROUND_VERIFICATION_DEMO").expect("ROUND_VERIFICATION_DEMO");
        let demo = std::fs::read(path).unwrap();
        let started = std::time::Instant::now();
        let baseline = parse_rounds_with_animation_recipes(&demo, true).unwrap();
        let baseline_time = started.elapsed();
        let started = std::time::Instant::now();
        let lean = parse_rounds(&demo).unwrap();
        eprintln!(
            "round verification: animation={baseline_time:?}, event-only={:?}, rounds={}",
            started.elapsed(),
            lean.len()
        );
        assert!(!baseline.is_empty());
        assert_eq!(baseline, lean);
    }

    fn event(name: &str, tick: i32, fields: Vec<(&str, Variant)>) -> GameEvent {
        GameEvent {
            name: name.to_string(),
            tick,
            fields: fields
                .into_iter()
                .map(|(name, value)| EventField {
                    name: name.to_string(),
                    data: Some(value),
                })
                .collect(),
        }
    }

    #[test]
    fn builds_rounds_and_attaches_officially_ended() {
        let events = vec![
            event("round_start", 100, vec![]),
            event("round_freeze_end", 250, vec![]),
            event(
                "round_end",
                900,
                vec![
                    ("round", Variant::U32(1)),
                    ("winner", Variant::String("CT".to_string())),
                ],
            ),
            event("round_officially_ended", 1000, vec![]),
            event("round_start", 1100, vec![]),
            event(
                "round_end",
                1900,
                vec![("round", Variant::U32(2)), ("winner", Variant::U32(2))],
            ),
        ];
        let rounds = build_rounds(&events).unwrap();
        assert_eq!(rounds.len(), 2);
        assert_eq!(rounds[0].start_tick, 100);
        assert_eq!(rounds[0].freeze_end, 250);
        assert_eq!(rounds[0].end_tick, 900);
        assert_eq!(rounds[0].officially_ended, Some(1000));
        assert_eq!(rounds[0].next_start_tick, Some(1100));
        assert_eq!(rounds[0].winner, "CT");
        assert_eq!(rounds[1].round, 2);
        assert_eq!(rounds[1].winner, "T");
        assert_eq!(rounds[1].officially_ended, None);
    }

    #[test]
    fn warmup_rounds_are_ignored() {
        let events = vec![
            event(
                "round_end",
                50,
                vec![("is_warmup_period", Variant::Bool(true))],
            ),
            event("round_start", 100, vec![]),
            event("round_end", 900, vec![("round", Variant::U32(1))]),
        ];
        let rounds = build_rounds(&events).unwrap();
        assert_eq!(rounds.len(), 1);
        assert_eq!(rounds[0].start_tick, 100);
    }
}
