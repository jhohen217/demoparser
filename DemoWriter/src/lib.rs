// The library owns the shared command implementation; standalone and embedded entry points
// call run_from so their behavior stays consistent.
include!("cli.rs");

pub use boundary::HudWeaponStateSeed;
pub use boundary::PawnMeshGroupRemap;
pub use boundary::RestorePoseSeed;
pub use boundary::WeaponTimingSeed;
pub use boundary::{AuditedEntityField, EntityFieldAuditQuery};
pub use boundary::{ScheduledFieldWrite, ScheduledScalar};
pub use boundary::write_packet_inventory_economy_jsonl;

/// Decode selected resident entity fields at exact tick/class/serial boundaries without editing
/// the demo. This is used to inspect values inherited from class baselines as well as packet writes.
pub fn audit_restore_entity_fields(
    demo: &[u8],
    queries: Vec<EntityFieldAuditQuery>,
) -> anyhow::Result<Vec<AuditedEntityField>> {
    use anyhow::ensure;
    ensure!(
        !queries.is_empty(),
        "entity field audit query list is empty"
    );
    for query in &queries {
        ensure!(
            (0..32768).contains(&query.entity_id),
            "audited entity index is invalid"
        );
        ensure!(
            query.serial < (1 << 17),
            "audited entity serial exceeds 17 bits"
        );
        // An empty list requests all resident fields, including fields inherited from baseline.
        ensure!(
            query
                .field_paths
                .iter()
                .all(|path| !path.is_empty() && path.len() <= 7),
            "audited field path is empty or too long"
        );
    }
    let edit = boundary::EntityEdit {
        audit_entity_fields: queries.clone(),
        from_tick: i32::MIN,
        to_tick: i32::MAX,
        ..Default::default()
    };
    let (unchanged, stats) = rewrite_restore_entity_fields(demo, edit)?;
    ensure!(
        unchanged == demo,
        "read-only entity field audit modified the demo"
    );
    let found = stats
        .audited_entity_fields
        .iter()
        .map(|row| (row.tick, row.entity_id, row.serial, row.field_path.clone()))
        .collect::<std::collections::BTreeSet<_>>();
    for query in &queries {
        if query.field_paths.is_empty() {
            ensure!(
                found
                    .iter()
                    .any(|(tick, entity, serial, _)| *tick == query.tick
                        && *entity == query.entity_id
                        && *serial == query.serial),
                "audited entity {} serial {} had no snapshot at tick {}",
                query.entity_id,
                query.serial,
                query.tick
            );
            continue;
        }
        for path in &query.field_paths {
            ensure!(
                found.contains(&(query.tick, query.entity_id, query.serial, path.clone())),
                "audited field {:?} was absent on entity {} serial {} at tick {}",
                path,
                query.entity_id,
                query.serial,
                query.tick
            );
        }
    }
    Ok(stats.audited_entity_fields)
}

#[derive(Clone, Debug, Default, serde::Deserialize, serde::Serialize)]
pub struct PawnMeshGroupRemapStats {
    pub matched: std::collections::BTreeMap<i32, usize>,
    pub rewritten: std::collections::BTreeMap<i32, usize>,
}

/// Rewrite explicitly transmitted pawn mesh-group masks under a strict
/// entity/class/model/current-value guard. Packet creates, ordinary updates,
/// and full-packet checkpoint copies all pass through the same entity walker.
pub fn remap_restore_pawn_mesh_groups(
    demo: &[u8],
    remaps: Vec<PawnMeshGroupRemap>,
) -> anyhow::Result<(Vec<u8>, PawnMeshGroupRemapStats)> {
    use anyhow::ensure;
    validate_pawn_mesh_group_remaps(&remaps)?;
    let expected_ids = remaps
        .iter()
        .map(|r| r.entity_id)
        .collect::<std::collections::BTreeSet<_>>();
    let edit = boundary::EntityEdit {
        pawn_mesh_group_remaps: remaps,
        from_tick: i32::MIN,
        to_tick: i32::MAX,
        ..Default::default()
    };
    let (out, stats) = rewrite_restore_entity_fields(demo, edit)?;
    for id in expected_ids {
        ensure!(
            stats
                .pawn_mesh_group_masks_matched
                .get(&id)
                .copied()
                .unwrap_or(0)
                > 0,
            "mesh-group manifest target pawn {id} had no matching explicit mask field"
        );
    }
    ensure!(
        stats.identity_failures == 0,
        "{} entity packet rewrites failed readback",
        stats.identity_failures
    );
    Ok((
        out,
        PawnMeshGroupRemapStats {
            matched: stats.pawn_mesh_group_masks_matched,
            rewritten: stats.pawn_mesh_group_masks_rewritten,
        },
    ))
}

fn validate_pawn_mesh_group_remaps(remaps: &[PawnMeshGroupRemap]) -> anyhow::Result<()> {
    use anyhow::ensure;
    ensure!(!remaps.is_empty(), "mesh-group remap manifest is empty");
    let mut keys = std::collections::BTreeSet::new();
    for remap in remaps {
        ensure!(
            (0..32768).contains(&remap.entity_id),
            "mesh-group pawn entity index is invalid"
        );
        ensure!(
            remap.class_name == "CCSPlayerPawn",
            "mesh-group manifest schema class must be CCSPlayerPawn (runtime RTTI C_CSPlayerPawn)"
        );
        ensure!(
            !remap.model_field_path.is_empty(),
            "mesh-group model guard path is empty"
        );
        ensure!(
            remap.mask_path == [12, 17],
            "mesh-group mask path must be [12,17]"
        );
        ensure!(
            remap.model_handle != 0,
            "mesh-group model guard handle is zero"
        );
        if let Some(serial) = remap.serial {
            ensure!(serial < (1 << 17), "mesh-group pawn serial exceeds 17 bits");
        }
        ensure!(
            keys.insert(remap.entity_id),
            "duplicate mesh-group manifest target for entity {}",
            remap.entity_id
        );
    }
    Ok(())
}

#[cfg(test)]
mod pawn_mesh_group_manifest_tests {
    use super::*;

    fn valid() -> PawnMeshGroupRemap {
        PawnMeshGroupRemap {
            entity_id: 298,
            serial: Some(4),
            class_name: "CCSPlayerPawn".into(),
            model_field_path: vec![12, 10],
            model_handle: 0x1234,
            mask_path: vec![12, 17],
            old_mask: 4,
            new_mask: 1,
            preserve_mask_bits: 0,
        }
    }

    #[test]
    fn accepts_only_explicit_pawn_model_and_old_mask_guards() {
        assert!(validate_pawn_mesh_group_remaps(&[valid()]).is_ok());
        let mut wrong_class = valid();
        wrong_class.class_name = "C_CSPlayerPawn".into();
        assert!(validate_pawn_mesh_group_remaps(&[wrong_class]).is_err());
        let mut wrong_mask_path = valid();
        wrong_mask_path.mask_path = vec![12, 16];
        assert!(validate_pawn_mesh_group_remaps(&[wrong_mask_path]).is_err());
        let mut identity = valid();
        identity.new_mask = identity.old_mask;
        assert!(validate_pawn_mesh_group_remaps(&[identity]).is_ok());
        assert!(validate_pawn_mesh_group_remaps(&[valid(), valid()]).is_err());
    }
}

/// Read-only cached vector bounds with exact packet/create/field ordering.
pub fn audit_restore_cached_ag2_bounds(
    demo: &[u8],
    pawns: &[i32],
) -> anyhow::Result<serde_json::Value> {
    boundary::audit_cached_ag2_bounds(demo, pawns)
}

/// Apply exact scheduled scalar writes while walking the original entity stream.
/// Field paths are explicit and checked against the target class serializer.
pub fn apply_scheduled_entity_writes(
    demo: &[u8],
    writes: Vec<ScheduledFieldWrite>,
) -> anyhow::Result<(Vec<u8>, usize)> {
    apply_scheduled_entity_writes_with_create(demo, writes, false)
}

/// Apply exact scheduled scalar writes while walking the original entity stream.
///
/// When `allow_create_writes` is true, a scheduled write may be inserted directly
/// into an existing create entry for the exact class/serial lifetime at that tick.
/// Writes to absent entities are never converted into synthetic creates. The
/// existing API remains delta-only by calling this with `false`.
pub fn apply_scheduled_entity_writes_with_create(
    demo: &[u8],
    writes: Vec<ScheduledFieldWrite>,
    allow_create_writes: bool,
) -> anyhow::Result<(Vec<u8>, usize)> {
    apply_scheduled_entity_writes_mode(demo, writes, allow_create_writes, false)
}

/// Apply scheduled scalar writes exclusively inside matching existing create
/// entries. Same-tick deltas are ignored, and unmatched rows fail the final
/// applied-count check instead of becoming synthetic updates.
pub fn apply_scheduled_entity_writes_create_only(
    demo: &[u8],
    writes: Vec<ScheduledFieldWrite>,
) -> anyhow::Result<(Vec<u8>, usize)> {
    apply_scheduled_entity_writes_mode(demo, writes, true, true)
}

fn apply_scheduled_entity_writes_mode(
    demo: &[u8],
    writes: Vec<ScheduledFieldWrite>,
    allow_create_writes: bool,
    create_only: bool,
) -> anyhow::Result<(Vec<u8>, usize)> {
    use anyhow::ensure;
    ensure!(!writes.is_empty(), "scheduled write list is empty");
    let mut keys = std::collections::BTreeSet::new();
    for write in &writes {
        ensure!(
            (0..32768).contains(&write.entity_id),
            "scheduled entity index is invalid"
        );
        ensure!(write.serial < (1 << 17), "scheduled serial exceeds 17 bits");
        ensure!(
            !write.field_path.is_empty(),
            "scheduled field path is empty"
        );
        ensure!(
            keys.insert((write.tick, write.entity_id, write.field_path.clone())),
            "duplicate scheduled field write at tick {} entity {} path {:?}",
            write.tick,
            write.entity_id,
            write.field_path
        );
    }
    let expected = writes.len();
    let edit = boundary::EntityEdit {
        from_tick: i32::MIN,
        to_tick: i32::MAX,
        scheduled_writes: writes,
        allow_scheduled_create_writes: allow_create_writes,
        create_only_scheduled_writes: create_only,
        ..Default::default()
    };
    let (out, stats) = rewrite_restore_entity_fields(demo, edit)?;
    ensure!(
        stats.scheduled_writes == expected,
        "applied {} of {expected} scheduled writes",
        stats.scheduled_writes
    );
    ensure!(
        stats.identity_failures == 0,
        "{} entity packet rewrites failed readback",
        stats.identity_failures
    );
    Ok((out, stats.scheduled_writes))
}

/// One guarded CAK47 packet write for the 14184 HUD render-gate experiment.
/// The appended descriptor is provided by augment_hud_weapon_state_schema.py.
pub fn seed_restore_hud_weapon_state(
    demo: &[u8],
    seed: HudWeaponStateSeed,
) -> anyhow::Result<Vec<u8>> {
    use anyhow::ensure;
    ensure!(
        seed.entity_id == 808
            && seed.class_id == 0
            && seed.serial == 140
            && seed.tick == 425
            && seed.field_path == 137
            && seed.value == 50,
        "HUD weapon-state pilot is pinned to old AK 808 at tick 425"
    );
    let edit = boundary::EntityEdit {
        from_tick: seed.tick,
        to_tick: seed.tick,
        hud_weapon_state_seed: Some(seed),
        ..Default::default()
    };
    let (out, stats) = rewrite_restore_entity_fields(demo, edit)?;
    ensure!(
        stats.hud_weapon_state_writes == 1,
        "expected exactly one HUD weapon-state write, got {}",
        stats.hud_weapon_state_writes
    );
    ensure!(
        stats.identity_failures == 0,
        "entity packet readback failed"
    );
    Ok(out)
}

/// Diagnostic-only 14184 HUD pilot. The schema must already contain the
/// appended scalar descriptor; the source handle is copied from the same
/// pawn's create packet, preserving every other packet value.
pub fn seed_restore_default_controller(
    demo: &[u8],
    entity_id: i32,
    class_id: u32,
    source_path: i32,
    target_path: i32,
    expected_handle: u32,
) -> anyhow::Result<(Vec<u8>, usize)> {
    use anyhow::ensure;
    let edit = boundary::EntityEdit {
        default_controller_seed: Some(boundary::DefaultControllerSeed {
            entity_id,
            class_id,
            source_path,
            target_path,
            expected_handle,
        }),
        from_tick: 0,
        to_tick: 1,
        ..Default::default()
    };
    let (out, stats) = rewrite_restore_entity_fields(demo, edit)?;
    ensure!(
        stats.default_controller_seeds > 0,
        "selected pawn had no eligible controller create writes"
    );
    ensure!(
        stats.identity_failures == 0,
        "{} packet identity verification failures",
        stats.identity_failures
    );
    Ok((out, stats.default_controller_seeds))
}

/// Diagnostic-only, source-switched 14184 AK timing vector. The input demo
/// must already have the appended uint8-vector descriptor in its old service
/// serializer. This does not run in the broad restoration pipeline.
pub fn seed_restore_weapon_timing(
    demo: &[u8],
    seed: boundary::WeaponTimingSeed,
) -> anyhow::Result<(Vec<u8>, usize, usize)> {
    use anyhow::ensure;
    ensure!(
        seed.end_tick - seed.switch_tick == 31,
        "the 14184 AK clock is calibrated for exactly 32 ticks"
    );
    ensure!(
        seed.service_path == 88 && seed.active_field == 1 && seed.timing_field == 7,
        "the pilot only supports verified old pawn paths [88,1] and [88,7]"
    );
    let edit = boundary::EntityEdit {
        from_tick: seed.switch_tick,
        to_tick: seed.end_tick,
        weapon_timing_seed: Some(seed),
        ..Default::default()
    };
    let (out, stats) = rewrite_restore_entity_fields(demo, edit)?;
    ensure!(
        stats.weapon_timing_ticks == 32,
        "expected 32 source pawn updates, got {}",
        stats.weapon_timing_ticks
    );
    ensure!(
        stats.identity_failures == 0,
        "{} packet identity verification failures",
        stats.identity_failures
    );
    Ok((out, stats.weapon_timing_ticks, stats.weapon_timing_fields))
}

/// Read-only create/delete command inventory for cross-build restoration.
pub fn inspect_restore_entity_events(demo: &[u8]) -> anyhow::Result<String> {
    let idx = index::DemoIndex::build(demo)?;
    let events = boundary::entity_events(demo, &idx)?;
    let rows = events
        .into_iter()
        .map(|event| {
            serde_json::json!({
                "tick": event.tick,
                "entity": event.entity,
                "class_id": event.class_id,
                "class_name": event.class_name,
                "serial": event.serial,
                "kind": event.kind,
                "checkpoint": event.checkpoint,
            })
        })
        .collect::<Vec<_>>();
    Ok(serde_json::to_string(&rows)?)
}

/// Diagnostic for a cross-build playback gap: add the checkpoint's absolute
/// packet to the sequential DEM_Packet stream at the same tick. Keep the
/// DEM_FullPacket unchanged so ordinary seeking can still use it.
pub fn duplicate_restore_checkpoint_packet(
    demo: &[u8],
    checkpoint_tick: i32,
) -> anyhow::Result<Vec<u8>> {
    use anyhow::{ensure, Context};
    use prost::Message as _;
    let idx = index::DemoIndex::build(demo)?;
    let checkpoint = idx
        .frames
        .iter()
        .find(|frame| frame.cmd == frame::CMD_FULL_PACKET && frame.tick() == checkpoint_tick)
        .context("requested checkpoint tick is absent")?;
    ensure!(
        !idx.frames
            .iter()
            .any(|frame| frame.cmd == frame::CMD_PACKET && frame.tick() == checkpoint_tick),
        "sequential packet already exists at checkpoint tick"
    );
    let full =
        csgoproto::CDemoFullPacket::decode(suppress::frame_payload(demo, checkpoint)?.as_slice())?;
    let packet = full
        .packet
        .context("checkpoint has no packet")?
        .encode_to_vec();
    let mut out = demo[..frame::HEADER_LEN].to_vec();
    let mut offsets = std::collections::BTreeMap::new();
    for frame in &idx.frames {
        offsets.insert(frame.frame_offset as u32, out.len() as u32);
        out.extend_from_slice(frame.bytes(demo));
        if frame.index == checkpoint.index {
            out.extend(frame::frame_header(
                frame::CMD_PACKET,
                false,
                checkpoint.tick_raw,
                packet.len() as u32,
            ));
            out.extend_from_slice(&packet);
        }
    }
    ensure!(
        out.len() <= u32::MAX as usize,
        "diagnostic demo exceeds header offset range"
    );
    for at in [8usize, 12] {
        let old = u32::from_le_bytes(demo[at..at + 4].try_into()?);
        if old != 0 {
            let new = *offsets.get(&old).context("header offset is not a frame")?;
            out[at..at + 4].copy_from_slice(&new.to_le_bytes());
        }
    }
    let rewritten = index::DemoIndex::build(&out)?;
    ensure!(
        rewritten.frames.len() == idx.frames.len() + 1,
        "diagnostic packet insertion did not add exactly one frame"
    );
    Ok(out)
}

/// Experimental helper for cross-build demo restoration probes.
pub fn append_restore_baseline_scalar(
    demo: &[u8],
    baseline: &[u8],
    class_id: usize,
    path: &[i32],
    value: u32,
) -> anyhow::Result<Vec<u8>> {
    boundary::append_baseline_scalar(demo, baseline, class_id, path, value)
}

pub fn append_restore_baseline_raw(
    demo: &[u8],
    baseline: &[u8],
    class_id: usize,
    path: &[i32],
    bytes: &[u8],
) -> anyhow::Result<Vec<u8>> {
    boundary::append_baseline_raw(demo, baseline, class_id, path, bytes)
}

pub fn append_restore_baseline_bits(
    demo: &[u8],
    baseline: &[u8],
    class_id: usize,
    path: &[i32],
    bytes: &[u8],
    bit_length: usize,
) -> anyhow::Result<Vec<u8>> {
    boundary::append_baseline_bits(demo, baseline, class_id, path, bytes, bit_length)
}

pub fn inspect_restore_baseline(
    demo: &[u8],
    baseline: &[u8],
    class_id: usize,
) -> anyhow::Result<String> {
    boundary::inspect_restore_baseline(demo, baseline, class_id)
}

pub fn replace_restore_baseline_model_id(
    demo: &[u8],
    baseline: &[u8],
    class_id: usize,
    path: &[i32],
    old: u64,
    new: u64,
) -> anyhow::Result<Vec<u8>> {
    boundary::replace_baseline_model_id(demo, baseline, class_id, path, old, new)
}

fn remap_restore_packet(
    data: &[u8],
    tick: i32,
    editor: &mut boundary::EntityEditor,
) -> anyhow::Result<Option<Vec<u8>>> {
    let mut messages = bitwriter::read_messages(data)?;
    let mut changed = false;
    let last_entity_message = messages
        .iter()
        .rposition(|message| message.msg_type == suppress::SVC_PACKET_ENTITIES);
    for (index, message) in messages.iter_mut().enumerate() {
        if message.msg_type == suppress::SVC_PACKET_ENTITIES {
            if let Some(replacement) = editor.packet_entities_with_packet_end(
                message,
                tick,
                Some(index) == last_entity_message,
            )? {
                message.payload = replacement;
                changed = true;
            }
        }
    }
    Ok(changed.then(|| bitwriter::write_messages(&messages)))
}

/// Rewrite only matching model-handle fields inside entity updates, retaining
/// every other message and frame. The class baseline must already be patched
/// separately, because it lives in DEM_StringTables rather than entity packets.
pub fn remap_restore_model_fields(
    demo: &[u8],
    mappings: &[(u64, u64)],
) -> anyhow::Result<(Vec<u8>, usize)> {
    use anyhow::ensure;
    let map = mappings
        .iter()
        .copied()
        .collect::<std::collections::BTreeMap<_, _>>();
    ensure!(
        !map.is_empty() && map.len() == mappings.len(),
        "empty or duplicate model map"
    );
    ensure!(
        map.iter().all(|(old, new)| old != new),
        "model map contains an identity entry"
    );
    let (out, stats) =
        rewrite_restore_entity_fields(demo, boundary::EntityEdit::for_model_remap(map))?;
    ensure!(
        stats.models_rewritten > 0,
        "no model fields matched the mapping"
    );
    Ok((out, stats.models_rewritten))
}

/// Census every nonzero model handle explicitly written by entity packets.
/// The shared packet walker leaves the demo byte-for-byte unchanged.
pub fn inventory_restore_packet_models(
    demo: &[u8],
) -> anyhow::Result<std::collections::BTreeMap<u64, usize>> {
    let (unchanged, stats) = rewrite_restore_entity_fields(demo, boundary::EntityEdit::default())?;
    anyhow::ensure!(
        unchanged == demo,
        "read-only packet model census modified the demo"
    );
    Ok(stats.packet_model_ids)
}

/// Seed an AG2 pose on the selected pawn's create update. Experimental and
/// intended only for a demo whose schema and baseline were already grafted.
pub fn seed_restore_pose_fields(
    demo: &[u8],
    seed: RestorePoseSeed,
) -> anyhow::Result<(Vec<u8>, usize)> {
    use anyhow::ensure;
    ensure!(
        !seed.topology.is_empty() && !seed.dynamic.is_empty(),
        "empty AG2 recipe"
    );
    let expected_donor_ticks = seed
        .donor_dynamic_by_tick
        .keys()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    let require_all_donor_ticks =
        !seed.preserved_slot_topologies.is_empty() || !seed.donor_slot_tables.is_empty();
    let mut targets = std::collections::BTreeMap::new();
    if seed.suppress_legacy_predicted {
        let mut fields = Vec::new();
        for prefix in ["m_Pred", "m_OwnerOnlyPredNet"] {
            for suffix in [
                "Bool",
                "Byte",
                "UInt16",
                "Int",
                "UInt32",
                "UInt64",
                "Float",
                "Vector",
                "Quaternion",
                "GlobalSymbol",
            ] {
                fields.push(format!("{prefix}{suffix}Variables"));
            }
        }
        targets.insert(seed.entity_id, fields);
    }
    let edit = boundary::EntityEdit {
        targets,
        pose_seed: Some(seed),
        from_tick: i32::MIN,
        to_tick: i32::MAX,
        ..Default::default()
    };
    let (out, stats) = rewrite_restore_entity_fields(demo, edit)?;
    ensure!(stats.pose_seeds > 0, "selected pawn was never created");
    if require_all_donor_ticks {
        let missing = expected_donor_ticks
            .difference(&stats.donor_pose_ticks)
            .copied()
            .collect::<Vec<_>>();
        let extra = stats
            .donor_pose_ticks
            .difference(&expected_donor_ticks)
            .copied()
            .collect::<Vec<_>>();
        ensure!(missing.is_empty() && extra.is_empty(),
            "pinned AG2 graft did not apply every scheduled tick: {} missing (first {:?}), {} unexpected (first {:?})",
            missing.len(), &missing[..missing.len().min(24)], extra.len(), &extra[..extra.len().min(24)]);
    }
    Ok((out, stats.pose_seeds))
}

fn rewrite_restore_entity_fields(
    demo: &[u8],
    edit: boundary::EntityEdit,
) -> anyhow::Result<(Vec<u8>, boundary::EntityEditStats)> {
    use anyhow::{bail, ensure};
    use prost::Message as _;
    let idx = index::DemoIndex::build(demo)?;
    let builder = boundary::BoundaryBuilder::new(demo, &idx)?;
    let mut editor = boundary::EntityEditor::new(&builder, edit);
    let mut out = demo[..16].to_vec();
    let mut offsets = std::collections::BTreeMap::new();
    for frame in &idx.frames {
        offsets.insert(frame.frame_offset as u32, out.len() as u32);
        let tick = frame.tick();
        let replacement = match frame.cmd {
            frame::CMD_STRING_TABLES => {
                let tables = csgoproto::CDemoStringTables::decode(
                    suppress::frame_payload(demo, frame)?.as_slice(),
                )?;
                editor.string_tables(&tables);
                None
            }
            frame::CMD_FULL_PACKET => {
                editor.note_checkpoint(tick);
                let mut full = csgoproto::CDemoFullPacket::decode(
                    suppress::frame_payload(demo, frame)?.as_slice(),
                )?;
                if let Some(snapshot) = &full.string_table {
                    editor.string_tables(snapshot);
                }
                if let Some(packet) = &mut full.packet {
                    if let Some(data) = remap_restore_packet(packet.data(), tick, &mut editor)? {
                        packet.data = Some(data.into());
                        Some(full.encode_to_vec())
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            frame::CMD_PACKET | frame::CMD_SIGNON_PACKET => {
                let mut packet = csgoproto::CDemoPacket::decode(
                    suppress::frame_payload(demo, frame)?.as_slice(),
                )?;
                if let Some(data) = remap_restore_packet(packet.data(), tick, &mut editor)? {
                    packet.data = Some(data.into());
                    Some(packet.encode_to_vec())
                } else {
                    None
                }
            }
            _ => None,
        };
        if let Some(payload) = replacement {
            out.extend(frame::frame_header(
                frame.cmd,
                false,
                frame.tick_raw,
                payload.len() as u32,
            ));
            out.extend(payload);
        } else {
            out.extend(frame.bytes(demo));
        }
    }
    if editor.stats.identity_failures != 0 {
        bail!(
            "{} entity packet rewrites failed readback",
            editor.stats.identity_failures
        );
    }
    ensure!(
        out.len() <= u32::MAX as usize,
        "rewritten demo exceeds short-header offset range"
    );
    for at in [8usize, 12] {
        let old = u32::from_le_bytes(demo[at..at + 4].try_into()?);
        if old != 0 {
            let new = *offsets.get(&old).context("header target was not a frame")?;
            out[at..at + 4].copy_from_slice(&new.to_le_bytes());
        }
    }
    Ok((out, editor.stats))
}

#[derive(Debug, Clone)]
pub struct VerifiedClipInspection {
    pub path: std::path::PathBuf,
    pub output_bytes: u64,
    pub checksum: String,
    pub first_full_packet_tick: i32,
    pub last_packet_tick: i32,
}

/// Validate and hash an already-published clip without rewriting it. This is the migration seam
/// for legacy clips: only files that still satisfy the writer's structural and parser checks may
/// be registered as complete DEM assets.
pub fn inspect_verified_clip(
    path: &std::path::Path,
    parse_check: bool,
) -> anyhow::Result<VerifiedClipInspection> {
    let mapped = map_demo(path)?;
    let index = DemoIndex::build(&mapped)
        .with_context(|| format!("{} is not frame-aligned", path.display()))?;
    let failed_checks = index
        .structural_report()
        .into_iter()
        .filter(|check| !check.passed)
        .map(|check| format!("{}: {}", check.name, check.detail))
        .collect::<Vec<_>>();
    if !failed_checks.is_empty() {
        anyhow::bail!(
            "structural verification failed for {} ({})",
            path.display(),
            failed_checks.join("; ")
        );
    }
    let first_full_packet_tick = index
        .frames
        .iter()
        .find(|frame| frame.cmd == CMD_FULL_PACKET)
        .map(|frame| frame.tick())
        .ok_or_else(|| anyhow::anyhow!("{} has no DEM_FullPacket", path.display()))?;
    if parse_check {
        rounds::parse_rounds(&mapped)
            .with_context(|| format!("{} does not reparse", path.display()))?;
    }
    Ok(VerifiedClipInspection {
        path: path.to_path_buf(),
        output_bytes: mapped.len() as u64,
        checksum: fnv1a64(&mapped),
        first_full_packet_tick,
        last_packet_tick: index
            .frames
            .iter()
            .filter(|f| f.cmd == CMD_PACKET || f.cmd == CMD_FULL_PACKET)
            .map(|f| f.tick())
            .max()
            .unwrap_or(first_full_packet_tick),
    })
}

/// Source-timeline identity of a requested round window.
///
/// This is the repair seam for clips produced before their checkpoint and logical window were
/// persisted in DuckDB. It runs the exact same round resolver and trim planner as
/// `trim_rounds_verified`, but stops before estimating, writing, hashing, or publishing output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoundTrimMetadata {
    pub round: i32,
    pub checkpoint_tick: i32,
    pub logical_start_tick: i32,
    pub logical_end_tick: i32,
}

/// Repair legacy metadata from the bytes actually retained, never from today's trim
/// policy. A previous writer may have used a shorter tail or a different checkpoint.
pub fn inspect_existing_round_clips(
    source: &std::path::Path,
    clips: &[(i32, std::path::PathBuf)],
) -> anyhow::Result<Vec<RoundTrimMetadata>> {
    let source_data = map_demo(source)?;
    let source_index = DemoIndex::build(&source_data)?;
    let rounds = rounds::parse_rounds(&source_data)?;
    clips
        .iter()
        .map(|(number, path)| {
            let clip = map_demo(path)?;
            let clip_index = DemoIndex::build(&clip)?;
            let checkpoint = &clip_index.frames[*clip_index
                .full_packets
                .first()
                .context("legacy clip has no checkpoint")?];
            let matches = source_index
                .full_packets
                .iter()
                .map(|i| &source_index.frames[*i])
                .filter(|frame| {
                    frame.compressed == checkpoint.compressed
                        && frame.payload(&source_data) == checkpoint.payload(&clip)
                })
                .collect::<Vec<_>>();
            if matches.len() != 1 {
                anyhow::bail!("{} has no unique source checkpoint", path.display());
            }
            let source_checkpoint = matches[0];
            let shift = source_checkpoint
                .tick()
                .checked_sub(checkpoint.tick())
                .context("tick shift overflow")?;
            let last = &clip_index.frames[clip_index
                .last_packet
                .context("legacy clip has no final packet")?];
            let source_end = last
                .tick()
                .checked_add(shift)
                .context("end tick overflow")?;
            if !source_index.frames[source_checkpoint.index..]
                .iter()
                .any(|frame| {
                    frame.cmd == CMD_PACKET
                        && frame.tick() == source_end
                        && frame.compressed == last.compressed
                        && frame.payload(&source_data) == last.payload(&clip)
                })
            {
                anyhow::bail!(
                    "{} final packet does not match its source timeline",
                    path.display()
                );
            }
            let round = rounds
                .iter()
                .find(|r| r.round == *number)
                .context("legacy round not found")?;
            Ok(RoundTrimMetadata {
                round: *number,
                checkpoint_tick: source_checkpoint.tick(),
                logical_start_tick: round.start_tick.max(source_checkpoint.tick()),
                logical_end_tick: source_end,
            })
        })
        .collect()
}

pub fn inspect_round_trim_metadata(
    source: &std::path::Path,
    requested_rounds: &[i32],
    tail_ticks: i32,
) -> anyhow::Result<Vec<RoundTrimMetadata>> {
    if requested_rounds.is_empty() {
        return Ok(Vec::new());
    }
    let mut unique = std::collections::HashSet::with_capacity(requested_rounds.len());
    for round in requested_rounds {
        if *round <= 0 {
            anyhow::bail!("round numbers must be positive");
        }
        if !unique.insert(*round) {
            anyhow::bail!("round {} was requested more than once", round);
        }
    }

    let policies = Policies {
        animation: AnimationPolicy::Keep,
        bootstrap: BootstrapPolicy::Full,
        metadata: MetadataPolicy::Absolute,
        spawn_groups: SpawnGroupsPolicy::Preserve,
        tickrate: 64.0,
        include_startup: true,
        sync_string_tables: true,
        align_startup: true,
        rebase_ticks: true,
    };
    let demo = map_demo(source)?;
    let index = DemoIndex::build(&demo)?;
    let all_rounds = rounds::parse_rounds(&demo)?;
    requested_rounds
        .iter()
        .map(|round| {
            let (start_tick, end_tick) = round_tick_window(&all_rounds, *round, tail_ticks, false)?;
            let plan = plan_trim(&index, start_tick, end_tick, policies)?;
            Ok(RoundTrimMetadata {
                round: *round,
                checkpoint_tick: plan.checkpoint_tick,
                logical_start_tick: plan.requested_start_tick,
                logical_end_tick: plan.requested_end_tick,
            })
        })
        .collect()
}

/// Builds parser-only checkpoint-backed demo windows. These windows preserve source tick
/// numbers and the network bootstrap required by the parser, but intentionally omit animation
/// and playback-only trailer data. They are an internal acceleration structure, not a user-facing
/// trimmed demo.
pub struct ParserWindowBuilder<'a> {
    demo: &'a [u8],
    index: DemoIndex,
    policies: Policies,
}

impl<'a> ParserWindowBuilder<'a> {
    pub fn new(demo: &'a [u8]) -> Result<Self> {
        Ok(Self {
            demo,
            index: DemoIndex::build(demo)?,
            policies: Policies {
                animation: AnimationPolicy::Keep,
                bootstrap: BootstrapPolicy::Full,
                metadata: MetadataPolicy::Absolute,
                spawn_groups: SpawnGroupsPolicy::Preserve,
                tickrate: 64.0,
                include_startup: true,
                sync_string_tables: true,
                align_startup: false,
                rebase_ticks: false,
            },
        })
    }

    pub fn source_bytes(&self) -> u64 {
        self.index.file_len
    }

    pub fn estimate_window_bytes(&self, start_tick: i32, end_tick: i32) -> Result<u64> {
        let plan = plan_trim(&self.index, start_tick, end_tick, self.policies)?;
        write::estimate_size(self.demo, &self.index, &plan, self.policies)
    }

    /// Materialize a short-lived parser window entirely in memory. User-requested trim output
    /// still uses the durable atomic file writer; this path exists only for parser scheduling.
    pub fn build_window(&self, start_tick: i32, end_tick: i32) -> Result<Vec<u8>> {
        let plan = plan_trim(&self.index, start_tick, end_tick, self.policies)?;
        write::write_trimmed_bytes(self.demo, &self.index, &plan, self.policies)
    }
}

#[cfg(test)]
mod metadata_repair_tests {
    use super::*;

    #[test]
    fn empty_metadata_request_does_not_open_the_source() {
        let missing = std::path::Path::new("this-source-does-not-exist.dem");
        assert!(inspect_round_trim_metadata(missing, &[], 256)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn metadata_request_rejects_invalid_rounds_before_source_io() {
        let missing = std::path::Path::new("this-source-does-not-exist.dem");
        assert!(inspect_round_trim_metadata(missing, &[0], 256).is_err());
        assert!(inspect_round_trim_metadata(missing, &[2, 2], 256).is_err());
    }
}

/// Insert entities that are live in the sequential stream but missing from a DEM_FullPacket
/// checkpoint (see `BoundaryBuilder::insert_checkpoint_entities`).
pub fn insert_checkpoint_entities(
    demo: &[u8],
    checkpoint_tick: i32,
    ids: &[i32],
) -> anyhow::Result<(Vec<u8>, Vec<(i32, u32, u32)>)> {
    let idx = index::DemoIndex::build(demo)?;
    let builder = boundary::BoundaryBuilder::new(demo, &idx)?;
    builder.insert_checkpoint_entities(demo, &idx, checkpoint_tick, ids)
}

/// Materialise the first ordinary DEM_Packet at `tick` (a full, non-delta entity update) over
/// the sequential state, keeping entities it omits alive (see `insert_checkpoint_entities`).
pub fn materialise_packet_entities(
    demo: &[u8],
    tick: i32,
    ids: &[i32],
) -> anyhow::Result<(Vec<u8>, Vec<(i32, u32, u32)>)> {
    let idx = index::DemoIndex::build(demo)?;
    let builder = boundary::BoundaryBuilder::new(demo, &idx)?;
    builder.materialise_packet_entities(demo, &idx, tick, ids)
}

/// Experimental same-schema static light snapshot.
pub fn light_snapshot(demo: &[u8], entity: i32, tick: i32) -> anyhow::Result<serde_json::Value> {
    let index = index::DemoIndex::build(demo)?;
    boundary::light_snapshot(demo, &index, entity, tick)
}
/// Experimental static light transplant; rejects structural errors before returning bytes.
pub fn light_inject(demo: &[u8], donor: &[u8], snapshot: &serde_json::Value) -> anyhow::Result<(Vec<u8>, serde_json::Value)> {
    let (output, report) = boundary::light_inject(demo, donor, snapshot, 2047)?;
    let index = index::DemoIndex::build(&output)?;
    for check in index.structural_report() { anyhow::ensure!(check.passed, "{}: {}", check.name, check.detail); }
    Ok((output, report))
}
