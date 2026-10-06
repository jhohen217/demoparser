use crate::core::demo_processor::types::RoundInfo;
use parser::parse_demo::DemoOutput;
use parser::second_pass::game_events::GameEvent;
use parser::second_pass::variants::Variant;

#[derive(Debug)]
struct RoundEnd {
    tick: i32,
    round: Option<i32>,
    winner: Option<String>,
    reason: Option<String>,
    warmup: bool,
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

/// `winner` arrives in two shapes depending on demo patch: newer demos emit the
/// parser's synthesised `round_end` with a "T"/"CT" string, older ones emit the native
/// game event with the raw team number (2 = T, 3 = CT).
///
/// When a round has no winning side (draw, game_start, ...) the parser has no entry in
/// its reason->winner table and falls back to stringifying the *reason code*. That is not
/// a winner, so it must not be passed through as one.
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

/// Older demos emit the raw win-reason code. Resolve it through the core parser's own
/// table so both demo shapes yield identical strings rather than two tables that drift.
fn reason_name(reason: i32) -> &'static str {
    parser::maps::ROUND_WIN_REASON
        .get(&reason)
        .copied()
        .unwrap_or("unknown")
}

fn build_rounds(events: &[GameEvent]) -> Result<Vec<RoundInfo>, String> {
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
    let mut round_ends: Vec<RoundEnd> = events
        .iter()
        .filter(|event| event.name == "round_end")
        .map(|event| RoundEnd {
            tick: event_tick(event),
            round: integer_field(event, "round"),
            winner: winner(event),
            reason: reason(event),
            warmup: is_warmup(event),
        })
        .filter(|event| !event.warmup)
        .collect();

    start_ticks.sort_unstable();
    freeze_ticks.sort_unstable();
    round_ends.sort_by_key(|event| event.tick);

    if round_ends.is_empty() {
        return Err("No non-warmup round_end events found in demo".to_string());
    }

    let mut rounds = Vec::with_capacity(round_ends.len());
    let mut previous_end = None;

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
        rounds.push(RoundInfo {
            next_start_tick: start_ticks.iter().copied().find(|tick| *tick > round_end.tick),
            round: round_number,
            start_tick,
            end_tick: round_end.tick,
            freeze_end,
            winner: round_end.winner.unwrap_or_else(|| "UNKNOWN".to_string()),
            win_reason: round_end.reason.unwrap_or_else(|| "unknown".to_string()),
        });
    }

    Ok(rounds)
}

pub fn parse_round_info_from_output(output: &DemoOutput) -> Result<Vec<RoundInfo>, String> {
    build_rounds(&output.game_events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use parser::second_pass::game_events::EventField;

    fn event(name: &str, tick: i32, fields: Vec<(&str, Variant)>) -> GameEvent {
        GameEvent {
            name: name.to_string(),
            fields: fields
                .into_iter()
                .map(|(name, value)| EventField {
                    name: name.to_string(),
                    data: Some(value),
                })
                .collect(),
            tick,
        }
    }

    #[test]
    fn uses_authoritative_round_fields_and_timings() {
        let events = vec![
            event("round_start", 100, vec![]),
            event("round_freeze_end", 250, vec![]),
            event(
                "round_end",
                900,
                vec![
                    ("round", Variant::U32(1)),
                    ("winner", Variant::String("CT".to_string())),
                    ("reason", Variant::String("t_killed".to_string())),
                ],
            ),
        ];

        let rounds = build_rounds(&events).unwrap();
        assert_eq!(rounds.len(), 1);
        assert_eq!(rounds[0].round, 1);
        assert_eq!(rounds[0].start_tick, 100);
        assert_eq!(rounds[0].freeze_end, 250);
        assert_eq!(rounds[0].end_tick, 900);
        assert_eq!(rounds[0].winner, "CT");
        assert_eq!(rounds[0].win_reason, "t_killed");
    }

    #[test]
    fn ignores_warmup_events_and_accepts_legacy_numeric_fields() {
        let events = vec![
            event(
                "round_start",
                10,
                vec![("is_warmup_period", Variant::Bool(true))],
            ),
            event(
                "round_end",
                50,
                vec![
                    ("is_warmup_period", Variant::Bool(true)),
                    ("winner", Variant::I32(2)),
                    ("reason", Variant::I32(9)),
                ],
            ),
            event("round_start", 100, vec![]),
            event("round_freeze_end", 200, vec![]),
            event(
                "round_end",
                800,
                vec![("winner", Variant::I32(2)), ("reason", Variant::I32(9))],
            ),
        ];

        let rounds = build_rounds(&events).unwrap();
        assert_eq!(rounds.len(), 1);
        assert_eq!(rounds[0].round, 1);
        assert_eq!(rounds[0].winner, "T");
        assert_eq!(rounds[0].win_reason, "ct_killed");
    }
}
