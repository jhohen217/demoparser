//! Discovery-only audit of world entities — doors, breakables, movers, and every other
//! non-player class the demo networks.
//!
//! This exists to answer a question the capture contract lists as unproven: whether a CS2 demo
//! carries enough state to animate a vent breaking or a door swinging, and what identity a
//! networked entity shares with the map-authored one. It therefore aggregates *everything*
//! rather than modelling one class, and is off unless explicitly requested.
//!
//! It is not a production lane. Nothing here is written to S2R; a capture section is designed
//! from what the report proves, not from this.

use crate::first_pass::sendtables::Field;
use crate::second_pass::variants::Variant;
use std::collections::{BTreeMap, BTreeSet};

/// Magic wanted-player-prop name that turns this capture on, matching how the weapon-entity
/// lane is requested. A demo parsed without it pays one bool test per decoded field.
pub const WORLD_ENTITY_AUDIT_PROPERTY: &str = "world_entity_audit";

/// Per field: how many example values are kept verbatim in the report.
const MAX_SAMPLE_VALUES: usize = 8;
/// Per field: how far distinct-value counting runs before it reports a floor instead.
const MAX_DISTINCT_TRACKED: usize = 64;
/// Whole capture: ceiling on the per-change evidence stream for candidate classes.
const MAX_CHANGE_RECORDS: usize = 40_000;
/// Per class: how many entity ids are listed individually.
const MAX_ENTITY_IDS: usize = 32;

/// One (class, field) pair's churn across the parsed window.
#[derive(Debug, Clone, PartialEq)]
pub struct WorldEntityFieldStat {
    pub field: String,
    pub kind: &'static str,
    pub updates: u32,
    pub first_tick: i32,
    pub last_tick: i32,
    /// Distinct values seen, or `MAX_DISTINCT_TRACKED` when `distinct_capped` is set.
    pub distinct: usize,
    pub distinct_capped: bool,
    pub samples: Vec<String>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    /// How many times one entity's own value for this field changed after its first
    /// observation. This is the only number that means "it animates": `distinct` pools every
    /// entity of the class, so two doors hung at different angles inflate it without either
    /// one ever moving. Counted for candidate classes only.
    pub transitions: u32,
    /// Distinct entities that saw at least one transition.
    pub entities_with_transitions: usize,
}

/// One networked class across the parsed window.
#[derive(Debug, Clone, PartialEq)]
pub struct WorldEntityClassStat {
    pub class_name: String,
    pub is_candidate: bool,
    pub entity_count: usize,
    pub entity_ids: Vec<i32>,
    pub entity_ids_capped: bool,
    pub creates: u32,
    pub deletes: u32,
    pub pvs_leaves: u32,
    pub field_updates: u32,
    pub first_tick: i32,
    pub last_tick: i32,
    pub fields: Vec<WorldEntityFieldStat>,
}

/// One observed field change on a candidate class, kept so a timeline can be read directly.
#[derive(Debug, Clone, PartialEq)]
pub struct WorldEntityChange {
    /// `spawn` for an entity's first observed value, `transition` for a later change of it.
    pub kind: &'static str,
    pub tick: i32,
    pub entity_id: i32,
    pub serial: u32,
    pub class_name: String,
    pub field: String,
    pub value: String,
}

/// One entity lifecycle transition on a candidate class.
#[derive(Debug, Clone, PartialEq)]
pub struct WorldEntityLifecycle {
    pub tick: i32,
    pub entity_id: i32,
    pub serial: u32,
    pub class_name: String,
    /// `create`, `delete`, `leave`, or `dormant`.
    pub transition: &'static str,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct WorldEntityAuditReport {
    pub classes: Vec<WorldEntityClassStat>,
    pub changes: Vec<WorldEntityChange>,
    pub lifecycle: Vec<WorldEntityLifecycle>,
    /// True when the change stream hit `MAX_CHANGE_RECORDS` and stopped recording.
    pub changes_truncated: bool,
}

#[derive(Debug, Default)]
struct FieldAccumulator {
    kind: &'static str,
    updates: u32,
    first_tick: i32,
    last_tick: i32,
    distinct: BTreeSet<String>,
    distinct_capped: bool,
    samples: Vec<String>,
    min: Option<f64>,
    max: Option<f64>,
    transitions: u32,
    transitioned_entities: BTreeSet<(i32, u32)>,
}

#[derive(Debug, Default)]
struct ClassAccumulator {
    is_candidate: bool,
    entity_ids: BTreeSet<i32>,
    entity_ids_capped: bool,
    entities: BTreeSet<(i32, u32)>,
    creates: u32,
    deletes: u32,
    pvs_leaves: u32,
    field_updates: u32,
    first_tick: i32,
    last_tick: i32,
    fields: BTreeMap<String, FieldAccumulator>,
}

#[derive(Debug, Default)]
pub struct WorldEntityAuditCapture {
    enabled: bool,
    classes: BTreeMap<String, ClassAccumulator>,
    /// Last value each candidate entity sent for each field, so a repeat of an unchanged
    /// value is not mistaken for movement. A full packet is replayed by the parser, and
    /// every entity's spawn state therefore arrives more than once.
    last_values: BTreeMap<(i32, u32), BTreeMap<String, String>>,
    changes: Vec<WorldEntityChange>,
    lifecycle: Vec<WorldEntityLifecycle>,
    changes_truncated: bool,
}

impl WorldEntityAuditCapture {
    pub fn new(enabled: bool) -> Self {
        Self { enabled, ..Self::default() }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn begin(&mut self, entity_id: i32, serial: u32, class_name: &str, tick: i32) {
        if !self.enabled {
            return;
        }
        let candidate = is_world_candidate(class_name);
        // A create is a new entity in that slot. Whatever the slot last held is not this
        // entity's history, and comparing against it would report a spawn as movement.
        self.last_values.remove(&(entity_id, serial));
        let class = self.class_mut(class_name, tick);
        class.creates += 1;
        class.entities.insert((entity_id, serial));
        if class.entity_ids.len() < MAX_ENTITY_IDS {
            class.entity_ids.insert(entity_id);
        } else {
            class.entity_ids_capped = true;
        }
        self.record_lifecycle(candidate, tick, entity_id, serial, class_name, "create");
    }

    /// `transition` is `delete`, `leave`, or `dormant` — the raw packet distinction is kept
    /// because a dormant door is not a removed one, and conflating them is how a replay ends
    /// up claiming an entity vanished.
    pub fn end(&mut self, entity_id: i32, serial: u32, class_name: &str, tick: i32, transition: &'static str) {
        if !self.enabled {
            return;
        }
        let candidate = is_world_candidate(class_name);
        let class = self.class_mut(class_name, tick);
        if transition == "delete" {
            class.deletes += 1;
        } else {
            class.pvs_leaves += 1;
        }
        self.record_lifecycle(candidate, tick, entity_id, serial, class_name, transition);
    }

    pub fn observe(
        &mut self,
        entity_id: i32,
        serial: u32,
        class_name: &str,
        tick: i32,
        field: &Field,
        value: &Variant,
        is_delta: bool,
    ) {
        if !self.enabled {
            return;
        }
        let Some(full_name) = field_name(field) else {
            return;
        };
        let candidate = is_world_candidate(class_name);
        let rendered = render(value);
        let numeric = numeric(value);
        let kind = kind(value);

        let class = self.class_mut(class_name, tick);
        class.field_updates += 1;
        class.is_candidate = candidate;
        let short = full_name.rsplit('.').next().unwrap_or(full_name).to_owned();
        let stat = class.fields.entry(short.clone()).or_insert_with(|| FieldAccumulator {
            kind,
            first_tick: tick,
            last_tick: tick,
            ..FieldAccumulator::default()
        });
        stat.updates += 1;
        stat.first_tick = stat.first_tick.min(tick);
        stat.last_tick = stat.last_tick.max(tick);
        class.entities.insert((entity_id, serial));
        if stat.kind != kind {
            // A field that decodes as two different variants is itself a finding.
            stat.kind = "mixed";
        }
        if let Some(number) = numeric {
            stat.min = Some(stat.min.map_or(number, |current: f64| current.min(number)));
            stat.max = Some(stat.max.map_or(number, |current: f64| current.max(number)));
        }
        if stat.distinct.len() < MAX_DISTINCT_TRACKED {
            if stat.distinct.insert(rendered.clone()) && stat.samples.len() < MAX_SAMPLE_VALUES {
                stat.samples.push(rendered.clone());
            }
        } else {
            stat.distinct_capped = true;
        }

        if !candidate {
            return;
        }
        // Only a change in one entity's own value is evidence of movement. The parser replays
        // full packets, so an unchanged repeat arrives routinely and must not be recorded.
        let entity = self.last_values.entry((entity_id, serial)).or_default();
        let kind_of_change = match entity.get(&short) {
            Some(previous) if *previous == rendered => return,
            // A re-seed can still disagree with what is stored: the parser replays a full
            // packet, and an entity index can hold different state in each. Take the new value
            // as the baseline to compare against, but never call it motion.
            Some(_) => {
                if !is_delta {
                    entity.insert(short, rendered);
                    return;
                }
                "transition"
            }
            None => "spawn",
        };
        entity.insert(short.clone(), rendered.clone());
        if kind_of_change == "transition" {
            let stat = self
                .classes
                .get_mut(class_name)
                .and_then(|class| class.fields.get_mut(&short));
            if let Some(stat) = stat {
                stat.transitions += 1;
                stat.transitioned_entities.insert((entity_id, serial));
            }
        }
        if self.changes.len() >= MAX_CHANGE_RECORDS {
            self.changes_truncated = true;
            return;
        }
        self.changes.push(WorldEntityChange {
            kind: kind_of_change,
            tick,
            entity_id,
            serial,
            class_name: class_name.to_owned(),
            field: short,
            value: rendered,
        });
    }

    pub fn finish(self) -> WorldEntityAuditReport {
        let classes = self
            .classes
            .into_iter()
            .map(|(class_name, accumulator)| WorldEntityClassStat {
                class_name,
                is_candidate: accumulator.is_candidate,
                entity_count: accumulator.entities.len(),
                entity_ids: accumulator.entity_ids.into_iter().collect(),
                entity_ids_capped: accumulator.entity_ids_capped,
                creates: accumulator.creates,
                deletes: accumulator.deletes,
                pvs_leaves: accumulator.pvs_leaves,
                field_updates: accumulator.field_updates,
                first_tick: accumulator.first_tick,
                last_tick: accumulator.last_tick,
                fields: accumulator
                    .fields
                    .into_iter()
                    .map(|(field, stat)| WorldEntityFieldStat {
                        field,
                        kind: stat.kind,
                        updates: stat.updates,
                        first_tick: stat.first_tick,
                        last_tick: stat.last_tick,
                        distinct: stat.distinct.len(),
                        distinct_capped: stat.distinct_capped,
                        samples: stat.samples,
                        min: stat.min,
                        max: stat.max,
                        transitions: stat.transitions,
                        entities_with_transitions: stat.transitioned_entities.len(),
                    })
                    .collect(),
            })
            .collect();
        WorldEntityAuditReport {
            classes,
            changes: self.changes,
            lifecycle: self.lifecycle,
            changes_truncated: self.changes_truncated,
        }
    }

    fn class_mut(&mut self, class_name: &str, tick: i32) -> &mut ClassAccumulator {
        let entry = self.classes.entry(class_name.to_owned()).or_insert_with(|| ClassAccumulator {
            is_candidate: is_world_candidate(class_name),
            first_tick: tick,
            last_tick: tick,
            ..ClassAccumulator::default()
        });
        entry.first_tick = entry.first_tick.min(tick);
        entry.last_tick = entry.last_tick.max(tick);
        entry
    }

    fn record_lifecycle(
        &mut self,
        candidate: bool,
        tick: i32,
        entity_id: i32,
        serial: u32,
        class_name: &str,
        transition: &'static str,
    ) {
        if !candidate || self.lifecycle.len() >= MAX_CHANGE_RECORDS {
            return;
        }
        self.lifecycle.push(WorldEntityLifecycle {
            tick,
            entity_id,
            serial,
            class_name: class_name.to_owned(),
            transition,
        });
    }
}

/// Merges a parallel second-pass slice into this one. Counters add, extents widen, and the
/// capped sample sets are unioned back up to the same caps, so a threaded parse reports the
/// same classes and fields a single-threaded one does.
pub fn merge(into: &mut WorldEntityAuditReport, from: WorldEntityAuditReport) {
    let mut by_name: BTreeMap<String, WorldEntityClassStat> = std::mem::take(&mut into.classes)
        .into_iter()
        .map(|class| (class.class_name.clone(), class))
        .collect();
    for class in from.classes {
        match by_name.get_mut(&class.class_name) {
            None => {
                by_name.insert(class.class_name.clone(), class);
            }
            Some(existing) => merge_class(existing, class),
        }
    }
    into.classes = by_name.into_values().collect();
    into.changes.extend(from.changes);
    into.lifecycle.extend(from.lifecycle);
    into.changes_truncated |= from.changes_truncated;
    if into.changes.len() > MAX_CHANGE_RECORDS {
        into.changes.truncate(MAX_CHANGE_RECORDS);
        into.changes_truncated = true;
    }
    if into.lifecycle.len() > MAX_CHANGE_RECORDS {
        into.lifecycle.truncate(MAX_CHANGE_RECORDS);
    }
}

fn merge_class(into: &mut WorldEntityClassStat, from: WorldEntityClassStat) {
    into.is_candidate |= from.is_candidate;
    into.entity_count += from.entity_count;
    into.creates += from.creates;
    into.deletes += from.deletes;
    into.pvs_leaves += from.pvs_leaves;
    into.field_updates += from.field_updates;
    into.first_tick = into.first_tick.min(from.first_tick);
    into.last_tick = into.last_tick.max(from.last_tick);
    let mut ids: BTreeSet<i32> = into.entity_ids.iter().copied().collect();
    for id in from.entity_ids {
        if ids.len() < MAX_ENTITY_IDS {
            ids.insert(id);
        } else {
            into.entity_ids_capped = true;
        }
    }
    into.entity_ids_capped |= from.entity_ids_capped;
    into.entity_ids = ids.into_iter().collect();

    let mut fields: BTreeMap<String, WorldEntityFieldStat> = std::mem::take(&mut into.fields)
        .into_iter()
        .map(|stat| (stat.field.clone(), stat))
        .collect();
    for stat in from.fields {
        match fields.get_mut(&stat.field) {
            None => {
                fields.insert(stat.field.clone(), stat);
            }
            Some(existing) => {
                if existing.kind != stat.kind {
                    existing.kind = "mixed";
                }
                existing.updates += stat.updates;
                existing.transitions += stat.transitions;
                existing.entities_with_transitions =
                    existing.entities_with_transitions.max(stat.entities_with_transitions);
                existing.first_tick = existing.first_tick.min(stat.first_tick);
                existing.last_tick = existing.last_tick.max(stat.last_tick);
                existing.distinct_capped |= stat.distinct_capped;
                existing.min = min_option(existing.min, stat.min);
                existing.max = max_option(existing.max, stat.max);
                // Distinct counts from two slices can overlap; the union of the retained
                // samples is exact only up to the sample cap, so the merged count is a floor.
                for sample in stat.samples {
                    if !existing.samples.contains(&sample) && existing.samples.len() < MAX_SAMPLE_VALUES {
                        existing.samples.push(sample);
                    }
                }
                existing.distinct = existing.distinct.max(stat.distinct);
            }
        }
    }
    into.fields = fields.into_values().collect();
}

fn min_option(left: Option<f64>, right: Option<f64>) -> Option<f64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (value, None) | (None, value) => value,
    }
}

fn max_option(left: Option<f64>, right: Option<f64>) -> Option<f64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (value, None) | (None, value) => value,
    }
}

/// Classes whose per-change timeline is kept. Deliberately broad: the class table covers every
/// class regardless, so anything this misses is still visible in the report and can be promoted.
pub fn is_world_candidate(class_name: &str) -> bool {
    if class_name.contains("Player") || class_name.contains("Weapon") || class_name.contains("Projectile") {
        return false;
    }
    const MARKERS: [&str; 12] = [
        "Door", "Break", "Mover", "Button", "Toggle", "Physbox", "PhysicsProp", "DynamicProp",
        "Track", "Rotating", "Gib", "Func",
    ];
    MARKERS.iter().any(|marker| class_name.contains(marker))
}

fn field_name(field: &Field) -> Option<&str> {
    match field {
        Field::Value(value) => Some(&value.full_name),
        Field::Vector(_) => match field.get_inner(0).ok()? {
            Field::Value(value) => Some(&value.full_name),
            _ => None,
        },
        _ => None,
    }
}

fn kind(value: &Variant) -> &'static str {
    match value {
        Variant::Bool(_) => "bool",
        Variant::U32(_) => "u32",
        Variant::I32(_) => "i32",
        Variant::F32(_) => "f32",
        Variant::U64(_) => "u64",
        Variant::String(_) => "string",
        Variant::Binary(_) => "binary",
        Variant::VecXY(_) => "vec2",
        Variant::VecXYZ(_) => "vec3",
        Variant::StringVec(_) => "string[]",
        Variant::U32Vec(_) => "u32[]",
        Variant::U64Vec(_) => "u64[]",
        Variant::Stickers(_) => "stickers",
        Variant::InputHistory(_) => "input_history",
        Variant::UserCmdSubtickMoves(_) => "subtick_moves",
    }
}

fn numeric(value: &Variant) -> Option<f64> {
    match value {
        Variant::Bool(value) => Some(if *value { 1.0 } else { 0.0 }),
        Variant::U32(value) => Some(*value as f64),
        Variant::I32(value) => Some(*value as f64),
        Variant::F32(value) => Some(*value as f64),
        Variant::U64(value) => Some(*value as f64),
        _ => None,
    }
}

fn render(value: &Variant) -> String {
    match value {
        Variant::Bool(value) => value.to_string(),
        Variant::U32(value) => value.to_string(),
        Variant::I32(value) => value.to_string(),
        Variant::F32(value) => format!("{value:.4}"),
        Variant::U64(value) => value.to_string(),
        Variant::String(value) => value.clone(),
        Variant::Binary(value) => format!("<{} bytes>", value.len()),
        Variant::VecXY(value) => format!("[{:.4}, {:.4}]", value[0], value[1]),
        Variant::VecXYZ(value) => format!("[{:.4}, {:.4}, {:.4}]", value[0], value[1], value[2]),
        Variant::StringVec(value) => format!("{value:?}"),
        Variant::U32Vec(value) => format!("{value:?}"),
        Variant::U64Vec(value) => format!("{value:?}"),
        // Length only: these carry no world-entity meaning and their debug form is enormous.
        Variant::Stickers(value) => format!("<{} stickers>", value.len()),
        Variant::InputHistory(value) => format!("<{} input history>", value.len()),
        Variant::UserCmdSubtickMoves(value) => format!("<{} subtick moves>", value.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::first_pass::sendtables::ValueField;
    use crate::second_pass::decoder::Decoder;

    /// A field the way the decoder hands one to `observe`: only `full_name` matters here.
    fn value_field(full_name: &str) -> Field {
        Field::Value(ValueField {
            decoder: Decoder::NoscaleDecoder,
            name: full_name.rsplit('.').next().unwrap_or(full_name).to_owned(),
            should_parse: false,
            prop_id: 0,
            full_name: full_name.to_owned(),
        })
    }

    #[test]
    fn disabled_capture_reports_nothing() {
        let mut capture = WorldEntityAuditCapture::new(false);
        capture.begin(3, 1, "CPropDoorRotating", 100);
        capture.end(3, 1, "CPropDoorRotating", 120, "delete");
        let report = capture.finish();
        assert!(report.classes.is_empty());
        assert!(report.lifecycle.is_empty());
    }

    #[test]
    fn candidate_filter_keeps_world_classes_and_drops_player_ones() {
        assert!(is_world_candidate("CPropDoorRotating"));
        assert!(is_world_candidate("CBreakableProp"));
        assert!(is_world_candidate("CFuncMover"));
        assert!(!is_world_candidate("CCSPlayerPawn"));
        assert!(!is_world_candidate("CSmokeGrenadeProjectile"));
        assert!(!is_world_candidate("CCSGameRulesProxy"));
    }

    #[test]
    fn lifecycle_keeps_leave_distinct_from_delete() {
        let mut capture = WorldEntityAuditCapture::new(true);
        capture.begin(3, 7, "CPropDoorRotating", 100);
        capture.end(3, 7, "CPropDoorRotating", 110, "leave");
        capture.end(3, 7, "CPropDoorRotating", 120, "delete");
        let report = capture.finish();
        let class = &report.classes[0];
        assert_eq!(class.creates, 1);
        assert_eq!(class.deletes, 1);
        assert_eq!(class.pvs_leaves, 1);
        assert_eq!(
            report.lifecycle.iter().map(|row| row.transition).collect::<Vec<_>>(),
            vec!["create", "leave", "delete"]
        );
    }

    #[test]
    fn repeated_spawn_state_is_not_movement() {
        let mut capture = WorldEntityAuditCapture::new(true);
        let field = value_field("CPropDoorRotating.m_angRotation");
        let closed = Variant::VecXYZ([0.0, 0.0, 0.0]);
        let open = Variant::VecXYZ([0.0, 90.0, 0.0]);
        // The parser replays the full packet, so the same closed angle arrives twice.
        capture.observe(99, 7, "CPropDoorRotating", 0, &field, &closed, true);
        capture.observe(99, 7, "CPropDoorRotating", 1, &field, &closed, true);
        // A second door hangs at a different angle without either one moving.
        capture.observe(266, 8, "CPropDoorRotating", 1, &field, &open, true);
        let report = capture.finish();
        let stat = &report.classes[0].fields[0];
        assert_eq!(stat.updates, 3);
        assert_eq!(stat.distinct, 2, "pooled distinct sees both doors");
        assert_eq!(stat.transitions, 0, "but neither door moved");
        assert_eq!(report.classes[0].entity_count, 2);
        assert_eq!(
            report.changes.iter().map(|row| (row.kind, row.entity_id)).collect::<Vec<_>>(),
            vec![("spawn", 99), ("spawn", 266)]
        );
    }

    #[test]
    fn one_entity_changing_its_own_value_is_a_transition() {
        let mut capture = WorldEntityAuditCapture::new(true);
        let field = value_field("CPropDoorRotating.m_angRotation");
        capture.observe(99, 7, "CPropDoorRotating", 0, &field, &Variant::VecXYZ([0.0, 0.0, 0.0]), true);
        capture.observe(99, 7, "CPropDoorRotating", 40, &field, &Variant::VecXYZ([0.0, 45.0, 0.0]), true);
        capture.observe(99, 7, "CPropDoorRotating", 48, &field, &Variant::VecXYZ([0.0, 90.0, 0.0]), true);
        let report = capture.finish();
        let stat = &report.classes[0].fields[0];
        assert_eq!(stat.transitions, 2);
        assert_eq!(stat.entities_with_transitions, 1);
        assert_eq!(
            report.changes.iter().map(|row| (row.kind, row.tick)).collect::<Vec<_>>(),
            vec![("spawn", 0), ("transition", 40), ("transition", 48)]
        );
    }

    #[test]
    fn a_full_packet_reseed_that_disagrees_is_not_counted_as_motion() {
        let mut capture = WorldEntityAuditCapture::new(true);
        let field = value_field("CFuncWater.m_nEntityId");
        capture.observe(871, 152, "CFuncWater", 0, &field, &Variant::U32(541852519), false);
        // The replayed pre-round full packet puts different state at the same index.
        capture.observe(871, 152, "CFuncWater", 1, &field, &Variant::U32(2303066658), false);
        capture.observe(871, 152, "CFuncWater", 1, &field, &Variant::U32(541852519), false);
        let report = capture.finish();
        assert_eq!(report.classes[0].fields[0].transitions, 0);
        assert_eq!(
            report.changes.iter().map(|row| row.kind).collect::<Vec<_>>(),
            vec!["spawn"]
        );
    }

    #[test]
    fn a_delta_update_after_a_reseed_is_still_motion() {
        let mut capture = WorldEntityAuditCapture::new(true);
        let field = value_field("CPropDoorRotating.m_eDoorState");
        capture.observe(564, 644, "CPropDoorRotating", 0, &field, &Variant::U32(0), false);
        capture.observe(564, 644, "CPropDoorRotating", 1, &field, &Variant::U32(3), false);
        capture.observe(564, 644, "CPropDoorRotating", 521, &field, &Variant::U32(1), true);
        let report = capture.finish();
        assert_eq!(report.classes[0].fields[0].transitions, 1);
        assert_eq!(
            report.changes.iter().map(|row| (row.kind, row.tick)).collect::<Vec<_>>(),
            vec![("spawn", 0), ("transition", 521)]
        );
    }

    #[test]
    fn a_create_clears_the_slots_remembered_values() {
        let mut capture = WorldEntityAuditCapture::new(true);
        let field = value_field("CFuncWater.m_nEntityId");
        capture.observe(871, 152, "CFuncWater", 0, &field, &Variant::U32(541852519), true);
        // A second spawn pass re-creates the same slot with different state. Neither the
        // create nor the value that follows it is movement.
        capture.begin(871, 152, "CFuncWater", 1);
        capture.observe(871, 152, "CFuncWater", 1, &field, &Variant::U32(2303066658), true);
        let report = capture.finish();
        assert_eq!(report.classes[0].fields[0].transitions, 0);
        assert_eq!(
            report.changes.iter().map(|row| row.kind).collect::<Vec<_>>(),
            vec!["spawn", "spawn"]
        );
    }

    #[test]
    fn a_reused_entity_slot_does_not_inherit_the_previous_entitys_value() {
        let mut capture = WorldEntityAuditCapture::new(true);
        let field = value_field("CBreakableProp.m_iHealth");
        capture.observe(99, 7, "CBreakableProp", 0, &field, &Variant::I32(100), true);
        // Same index, new serial: a different entity, so its first value is a spawn.
        capture.observe(99, 8, "CBreakableProp", 60, &field, &Variant::I32(100), true);
        let report = capture.finish();
        assert_eq!(report.classes[0].fields[0].transitions, 0);
        assert_eq!(report.classes[0].entity_count, 2);
        assert_eq!(
            report.changes.iter().map(|row| row.kind).collect::<Vec<_>>(),
            vec!["spawn", "spawn"]
        );
    }

    #[test]
    fn merge_sums_counters_and_widens_extents() {
        let mut left = WorldEntityAuditReport {
            classes: vec![WorldEntityClassStat {
                class_name: "CBreakableProp".to_owned(),
                is_candidate: true,
                entity_count: 1,
                entity_ids: vec![4],
                entity_ids_capped: false,
                creates: 1,
                deletes: 0,
                pvs_leaves: 0,
                field_updates: 5,
                first_tick: 100,
                last_tick: 140,
                fields: vec![WorldEntityFieldStat {
                    field: "m_iHealth".to_owned(),
                    kind: "i32",
                    updates: 5,
                    first_tick: 100,
                    last_tick: 140,
                    distinct: 2,
                    distinct_capped: false,
                    samples: vec!["100".to_owned(), "0".to_owned()],
                    min: Some(0.0),
                    max: Some(100.0),
                    transitions: 4,
                    entities_with_transitions: 1,
                }],
            }],
            ..WorldEntityAuditReport::default()
        };
        let right = WorldEntityAuditReport {
            classes: vec![WorldEntityClassStat {
                class_name: "CBreakableProp".to_owned(),
                is_candidate: true,
                entity_count: 1,
                entity_ids: vec![9],
                entity_ids_capped: false,
                creates: 1,
                deletes: 1,
                pvs_leaves: 2,
                field_updates: 3,
                first_tick: 80,
                last_tick: 200,
                fields: vec![WorldEntityFieldStat {
                    field: "m_iHealth".to_owned(),
                    kind: "i32",
                    updates: 3,
                    first_tick: 80,
                    last_tick: 200,
                    distinct: 3,
                    distinct_capped: false,
                    samples: vec!["55".to_owned()],
                    min: Some(55.0),
                    max: Some(255.0),
                    transitions: 2,
                    entities_with_transitions: 1,
                }],
            }],
            ..WorldEntityAuditReport::default()
        };
        merge(&mut left, right);
        let class = &left.classes[0];
        assert_eq!(class.entity_ids, vec![4, 9]);
        assert_eq!(class.creates, 2);
        assert_eq!(class.deletes, 1);
        assert_eq!(class.pvs_leaves, 2);
        assert_eq!(class.field_updates, 8);
        assert_eq!((class.first_tick, class.last_tick), (80, 200));
        let field = &class.fields[0];
        assert_eq!(field.updates, 8);
        assert_eq!(field.transitions, 6);
        assert_eq!(field.entities_with_transitions, 1);
        assert_eq!((field.min, field.max), (Some(0.0), Some(255.0)));
        assert_eq!(field.samples, vec!["100".to_owned(), "0".to_owned(), "55".to_owned()]);
    }
}
