//! Independent death-event flags. Missing observations stay distinct from false.
use super::game_events::GameEvent;
use super::variants::Variant;

pub const HEADSHOT: u8 = 1 << 0;
pub const THROUGH_SMOKE: u8 = 1 << 1;
pub const NOSCOPE: u8 = 1 << 2;
pub const ATTACKER_BLIND: u8 = 1 << 3;
pub const WALLBANG: u8 = 1 << 4;
pub const ATTACKER_AIRBORNE: u8 = 1 << 5;
pub const VICTIM_AIRBORNE: u8 = 1 << 6;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct KillModifiers {
    pub flags: u8,
    pub known: u8,
    /// Number of penetrated surfaces. Legacy boolean events do not supply a count.
    pub penetrated: Option<u32>,
}

pub fn field<'a>(event: &'a GameEvent, name: &str) -> Option<&'a Variant> {
    event.fields.iter().find(|f| f.name == name)?.data.as_ref()
}

pub fn observed_bool(value: Option<&Variant>) -> Option<bool> {
    match value {
        Some(Variant::Bool(v)) => Some(*v),
        Some(Variant::I32(v)) if *v >= 0 => Some(*v > 0),
        Some(Variant::U32(v)) => Some(*v > 0),
        _ => None,
    }
}

impl KillModifiers {
    pub fn set(&mut self, bit: u8, value: Option<bool>) {
        if let Some(value) = value {
            self.known |= bit;
            self.flags = (self.flags & !bit) | if value { bit } else { 0 };
        }
    }

    pub fn from_event(event: &GameEvent) -> Self {
        let mut result = Self::default();
        for (name, bit) in [
            ("headshot", HEADSHOT),
            ("thrusmoke", THROUGH_SMOKE),
            ("noscope", NOSCOPE),
            ("attackerblind", ATTACKER_BLIND),
            ("penetrated", WALLBANG),
            ("attacker_is_airborne", ATTACKER_AIRBORNE),
            ("user_is_airborne", VICTIM_AIRBORNE),
        ] {
            result.set(bit, observed_bool(field(event, name)));
        }
        result.penetrated = match field(event, "penetrated") {
            Some(Variant::I32(v)) => u32::try_from(*v).ok(),
            Some(Variant::U32(v)) => Some(*v),
            _ => None,
        };
        result
    }
}

#[cfg(test)]
mod tests {
    use super::super::game_events::EventField;
    use super::*;

    #[test]
    fn combined_flags_integer_penetration_and_unknowns() {
        let event = GameEvent {
            name: "player_death".into(),
            tick: 10,
            fields: vec![
                EventField {
                    name: "headshot".into(),
                    data: Some(Variant::Bool(true)),
                },
                EventField {
                    name: "penetrated".into(),
                    data: Some(Variant::I32(2)),
                },
                EventField {
                    name: "noscope".into(),
                    data: Some(Variant::Bool(false)),
                },
                EventField {
                    name: "attacker_is_airborne".into(),
                    data: Some(Variant::Bool(true)),
                },
            ],
        };
        let flags = KillModifiers::from_event(&event);
        assert_eq!(flags.flags, HEADSHOT | WALLBANG | ATTACKER_AIRBORNE);
        assert_eq!(flags.known, flags.flags | NOSCOPE);
        assert_eq!(flags.penetrated, Some(2));
        assert_eq!(observed_bool(Some(&Variant::I32(-1))), None);
    }
}
