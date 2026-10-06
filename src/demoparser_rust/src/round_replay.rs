//! Materialize catalog and replay work on each verified clip's own timeline.
//! Source tick offsets exist only during this conversion; no sidecar is needed.
use crate::tick_by_tick::kill_collection_parser::{KillCollectionData, RoundInfo};
use anyhow::{Context, Result};
use interface::models::{
    collection::KillCollection,
    tick_asset::{AssetFormat, TickAsset},
};
use std::collections::HashMap;

pub const LOCAL_DEM_VERSION: i32 = 5;
type Assets = HashMap<(String, String), Vec<TickAsset>>;
fn name(value: &str) -> &str {
    value
        .trim_end_matches(".gz")
        .trim_end_matches(".zst")
        .trim_end_matches(".dem")
}
fn key(folder: &str, demo: &str, number: i32) -> (String, String, i32) {
    (folder.into(), name(demo).into(), number)
}
fn local(tick: i32, shift: i32) -> i32 {
    tick.saturating_sub(shift).max(1)
}
fn ticks(value: &str, shift: i32) -> String {
    value
        .split_inclusive(|c: char| !(c.is_ascii_digit() || c == '-'))
        .map(|part| {
            let end = part
                .find(|c: char| !(c.is_ascii_digit() || c == '-'))
                .unwrap_or(part.len());
            match part[..end].parse::<i32>() {
                Ok(tick) => format!("{}{}", local(tick, shift), &part[end..]),
                Err(_) => part.into(),
            }
        })
        .collect()
}

/// Preserve stable demo/collection identities while rebasing every absolute tick
/// and directing the expensive authority parser to one physical clip per round.
pub fn prepare(
    collections: &mut [Vec<KillCollection>],
    inputs: Vec<KillCollectionData>,
    assets: &mut Assets,
    skip_buy_time: bool,
) -> Result<Vec<KillCollectionData>> {
    let selected: std::collections::HashSet<_> = collections
        .iter()
        .flatten()
        .map(|c| key(&c.folder, &c.demo_name, c.collection_num))
        .collect();
    let lookup: HashMap<_, _> = assets
        .values()
        .flatten()
        .filter(|a| a.format == AssetFormat::Dem)
        .map(|a| {
            let shift = (a.checkpoint_tick.unwrap_or(1) - 1).max(0);
            (
                key(&a.folder, &a.demo_name, a.collection_num),
                (
                    a.path.clone(),
                    shift,
                    local(a.logical_end_tick.unwrap_or(shift + 1), shift),
                ),
            )
        })
        .collect();
    for collection in collections.iter_mut().flatten() {
        let Some((path, shift, _)) = lookup.get(&key(
            &collection.folder,
            &collection.demo_name,
            collection.collection_num,
        )) else {
            continue;
        };
        collection.demo_path = path.clone();
        collection.start_kill_tick = local(collection.start_kill_tick, *shift);
        collection.end_kill_tick = local(collection.end_kill_tick, *shift);
        collection.round_start_tick = 1;
        collection.round_end_tick = local(collection.round_end_tick, *shift);
        collection.round_freeze_end = local(collection.round_freeze_end, *shift).max(2);
        for tick in &mut collection.kill_ticks {
            *tick = local(*tick, *shift);
        }
        for kill in &mut collection.kills {
            kill.tick = local(kill.tick, *shift);
            kill.round_start_tick = 1;
            kill.round_end_tick = collection.round_end_tick;
            kill.round_freeze_end = collection.round_freeze_end;
        }
        collection.util_thrown_ticks = ticks(&collection.util_thrown_ticks, *shift);
        collection.util_land_ticks = ticks(&collection.util_land_ticks, *shift);
    }
    let mut result = Vec::new();
    for input in inputs {
        let mut by_path = HashMap::<String, KillCollectionData>::new();
        let mut untouched = input.clone();
        untouched.collections.clear();
        untouched.collection_details.clear();
        for collection in &input.collections {
            if !selected.contains(&key(
                &collection.folder,
                &collection.demo_name,
                collection.collection_num as i32,
            )) {
                continue;
            }
            let Some((path, shift, end)) = lookup.get(&key(
                &collection.folder,
                &collection.demo_name,
                collection.collection_num as i32,
            )) else {
                untouched.collections.push(collection.clone());
                if let Some(details) = input.collection_details.get(&collection.collection_num) {
                    untouched
                        .collection_details
                        .insert(collection.collection_num, details.clone());
                }
                continue;
            };
            let output = by_path.entry(path.clone()).or_insert_with(|| {
                let mut output = input.clone();
                output.collections.clear();
                output.collection_details.clear();
                output.rounds.clear();
                output.demo_info.demo_path = path.clone();
                output.demo_info.total_ticks = *end as u32;
                output.demo_info.game_start_offset = 0.0;
                output
            });
            let mut col = collection.clone();
            col.start_kill_tick = local(col.start_kill_tick as i32, *shift) as u32;
            col.end_kill_tick = local(col.end_kill_tick as i32, *shift) as u32;
            col.round_start_tick = 1;
            col.round_end_tick = local(col.round_end_tick as i32, *shift) as u32;
            col.round_freeze_end = local(col.round_freeze_end as i32, *shift).max(2) as u32;
            col.kill_ticks = ticks(&col.kill_ticks, *shift);
            if output.rounds.is_empty() {
                output.rounds.push(RoundInfo {
                    round: col.round,
                    start_tick: 1,
                    end_tick: col.round_end_tick,
                    round_freeze_end: col.round_freeze_end,
                    next_start_tick: Some(*end as u32 + 1),
                });
            }
            if let Some(details) = input.collection_details.get(&col.collection_num) {
                let mut details = details.clone();
                for detail in &mut details {
                    detail.kill_tick = local(detail.kill_tick as i32, *shift) as u32;
                }
                output
                    .collection_details
                    .insert(col.collection_num, details);
            }
            output.collections.push(col);
        }
        result.extend(by_path.into_values());
        if !untouched.collections.is_empty() {
            result.push(untouched);
        }
    }
    for asset in assets.values_mut().flatten() {
        if asset.format != AssetFormat::Dem {
            continue;
        }
        let (_, _, end) = lookup
            .get(&key(&asset.folder, &asset.demo_name, asset.collection_num))
            .context("missing clip timeline")?;
        asset.format_version = LOCAL_DEM_VERSION;
        // These are clip-local playable bounds, not a mapping back to the source.
        asset.logical_start_tick = Some(if skip_buy_time { 2 } else { 1 });
        asset.logical_end_tick = Some(*end);
        asset.checkpoint_tick = None;
        asset.source_path = None;
        asset.source_bytes = None;
    }
    Ok(result)
}

#[derive(Clone, Debug)]
struct CatalogClip {
    path: std::path::PathBuf,
    demo: String,
    number: i32,
    total: i32,
    round: i32,
    start: i32,
    end: i32,
    freeze: i32,
    bytes: u64,
    kind: String,
    source_bytes: i64,
    modified_ns: i64,
}
fn path_id(path: &std::path::Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .trim_start_matches("//?/")
        .to_lowercase()
}
#[derive(Default)]
struct CatalogIndex {
    clips: Vec<CatalogClip>,
    by_path: HashMap<String, Vec<usize>>,
    by_source: HashMap<(String, String), Vec<usize>>,
    identities: std::sync::Mutex<CatalogIdentities>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CollectionIdentity {
    number: i32,
    kind: String,
}

#[derive(Default)]
struct CatalogIdentities {
    // All catalog types, including rows which do not yet have a DEM/S2R asset.
    by_player_round: HashMap<(String, i32, String), Vec<CollectionIdentity>>,
    high_water: HashMap<String, i32>,
}

impl CatalogIdentities {
    fn remember(&mut self, demo: &str, round: i32, killer: String, number: i32, total: i32, kind: String) {
        let demo = name(demo).to_lowercase();
        let high_water = self.high_water.entry(demo.clone()).or_default();
        *high_water = (*high_water).max(number).max(total);
        let identities = self.by_player_round.entry((demo, round, killer)).or_default();
        let identity = CollectionIdentity { number, kind };
        if !identities.contains(&identity) {
            identities.push(identity);
        }
    }
}
fn catalog_clips(root: &std::path::Path, parent: &std::path::Path) -> std::sync::Arc<CatalogIndex> {
    // One bounded catalog read per parser process/root, shared across round jobs.
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<HashMap<std::path::PathBuf, std::sync::Arc<CatalogIndex>>>,
    > = std::sync::OnceLock::new();
    let mut cache = CACHE.get_or_init(Default::default).lock().unwrap();
    let folder = config::storage::source_directory(root, parent);
    cache.entry(root.join(&folder)).or_insert_with(|| {
        let mut clips = Vec::new();
        let mut identities = CatalogIdentities::default();
        for kind in ["ACE", "QUAD", "TRIPLE", "MULTI", "DOUBLE", "SINGLE"] {
            let database = config::storage::database_path(root, kind, &folder);
            let Ok(flags) = duckdb::Config::default().access_mode(duckdb::AccessMode::ReadOnly) else { continue; };
            let Ok(conn) = duckdb::Connection::open_with_flags(&database, flags) else { continue; };
            if let Ok(mut query) = conn.prepare("SELECT demo_name, round, steam_id, collection_num, col_total, type FROM kill_collections") {
                if let Ok(rows) = query.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i32>(1)?, r.get::<_, String>(2)?, r.get::<_, i32>(3)?, r.get::<_, i32>(4)?, r.get::<_, String>(5)?))) {
                    for (demo, round, killer, number, total, kind) in rows.flatten() {
                        identities.remember(&demo, round, killer, number, total, kind);
                    }
                }
            }
            let Ok(mut query) = conn.prepare("SELECT a.path, c.demo_name, c.collection_num, c.col_total, c.round, c.steam_id, a.logical_start_tick, a.logical_end_tick, c.round_freeze_end, a.size_bytes, c.type, s.size_bytes, s.modified_ns FROM kill_collections c JOIN tick_assets a ON c.demo_name=a.demo_name AND c.collection_num=a.collection_num LEFT JOIN demo_sources s ON s.demo_name=c.demo_name WHERE a.format='DEM' AND a.status='complete' AND a.format_version=5") else { continue; };
            let Ok(rows) = query.query_map([], |r| Ok(CatalogClip {
                path: config::storage::resolve_asset(root, &database, &r.get::<_, String>(0)?), demo: r.get(1)?, number: r.get(2)?, total: r.get(3)?, round: r.get(4)?, start: r.get(6)?, end: r.get(7)?, freeze: r.get(8)?, bytes: r.get::<_, i64>(9)? as u64, kind: r.get(10)?, source_bytes: r.get::<_, Option<i64>>(11)?.unwrap_or(-1), modified_ns: r.get::<_, Option<i64>>(12)?.unwrap_or(-1),
            })) else { continue; };
            clips.extend(rows.flatten());
        }
        let mut index = CatalogIndex { clips, identities: std::sync::Mutex::new(identities), ..Default::default() };
        for (i, clip) in index.clips.iter().enumerate() {
            index.by_path.entry(path_id(&clip.path)).or_default().push(i);
            index.by_source.entry((path_id(clip.path.parent().unwrap_or(root)), name(&clip.demo).to_lowercase())).or_default().push(i);
        }
        std::sync::Arc::new(index)
    }).clone()
}

pub fn is_registered_clip(root: &std::path::Path, path: &std::path::Path) -> bool {
    catalog_clips(root, path.parent().unwrap_or(root))
        .by_path
        .contains_key(&path_id(path))
}

/// Re-parsing a saved round retains its catalog IDs even though a standalone
/// discovery naturally numbers that round's collections from one again.
pub fn restore_clip_identity(
    root: &std::path::Path,
    path: &std::path::Path,
    collections: &mut [KillCollection],
) -> Result<()> {
    let index = catalog_clips(root, path.parent().unwrap_or(root));
    let rows: Vec<_> = index
        .by_path
        .get(&path_id(path))
        .into_iter()
        .flatten()
        .map(|i| &index.clips[*i])
        .collect();
    if rows.is_empty() {
        return Ok(());
    }
    let anchor = rows[0];
    anyhow::ensure!(rows.iter().all(|r| r.demo == anchor.demo && r.round == anchor.round),
        "clip has conflicting demo/round registrations: {}", path.display());
    anyhow::ensure!(collections.iter().map(|c| c.round).collect::<std::collections::HashSet<_>>().len() <= 1,
        "registered round clip contains collections from multiple rounds: {}", path.display());
    let demo_key = name(&anchor.demo).to_lowercase();
    // Reserve numbers across concurrent round jobs. Catalog identities influence numbering,
    // never discovery: only collections actually found in this input clip enter this loop.
    let mut identities = index.identities.lock().unwrap();
    for col in collections.iter_mut() {
        let key = (demo_key.clone(), anchor.round, col.killer_steamid.clone());
        let number = match identities.by_player_round.get(&key) {
            Some(matches) => {
                anyhow::ensure!(matches.len() == 1 && matches[0].kind == col.collection_type,
                    "clip collection identity is ambiguous or changed type for {} in {}", col.killer_steamid, path.display());
                matches[0].number
            }
            None => {
                let high_water = identities.high_water.entry(demo_key.clone()).or_insert(anchor.total);
                *high_water = high_water.checked_add(1).context("collection number overflow")?;
                let number = *high_water;
                identities.by_player_round.insert(key, vec![CollectionIdentity { number, kind: col.collection_type.clone() }]);
                number
            }
        };
        col.demo_name = anchor.demo.clone();
        col.collection_num = number;
        col.round = anchor.round;
        col.round_start_tick = col.round_start_tick.max(1);
        col.round_freeze_end = anchor.freeze;
        for kill in &mut col.kills {
            kill.round = anchor.round;
        }
    }
    let total = identities.high_water.get(&demo_key).copied().unwrap_or(anchor.total);
    for col in collections { col.col_total = total; }
    Ok(())
}

/// Identify a self-contained rebased round from its parsed round count and
/// actual packet structure. No original demo or JSON report is consulted.
fn saved_round_index(path: &std::path::Path, rounds: &[RoundInfo]) -> Option<usize> {
    if rounds.len() == 1 { return Some(0); }
    // A legacy checkpoint can retain multiple earlier rounds (observed: r25
    // contains the tail of r23 and all of r24). Require an ordered, contiguous
    // prefix ending in the named round; rebasing is checked below. For longer
    // prefixes also require a partial opening round, not an entire match.
    let stem = path.file_stem()?.to_str()?;
    let (_, suffix) = stem.rsplit_once("_r")?;
    let named_round = suffix.parse::<u32>().ok()?;
    if rounds.len() >= 2 && rounds.last()?.round == named_round
        && rounds.windows(2).all(|pair| pair[0].round.checked_add(1) == Some(pair[1].round)
            && pair[0].start_tick < pair[1].start_tick
            && pair[0].end_tick <= pair[1].start_tick
            && pair[0].next_start_tick == Some(pair[1].start_tick))
        && (rounds.len() == 2 || (rounds[0].round > 1
            && rounds[0].start_tick == 0 && rounds[0].round_freeze_end == 0)) {
        return Some(rounds.len() - 1);
    }
    None
}

pub fn standalone_clip(
    path: &std::path::Path,
    data: &mut KillCollectionData,
) -> Result<Option<demo_writer::VerifiedClipInspection>> {
    let named_clip = path.file_stem().and_then(|s| s.to_str())
        .and_then(|s| s.rsplit_once("_r"))
        .is_some_and(|(_, suffix)| !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()));
    let Some(index) = saved_round_index(path, &data.rounds) else {
        anyhow::ensure!(!named_clip,
            "Refusing to trim round-named input {} again: its round structure is not a recognized saved clip", path.display());
        return Ok(None);
    };
    let inspection = demo_writer::inspect_verified_clip(path, false)?;
    if !(0..=1).contains(&inspection.first_full_packet_tick) {
        anyhow::ensure!(!named_clip,
            "Refusing to trim round-named input {} again: its first checkpoint is not rebased", path.display());
        return Ok(None);
    }
    let selected = data.rounds[index].clone();
    data.collections.retain(|c| c.round == selected.round);
    data.collection_details.retain(|number, _| data.collections.iter().any(|c| c.collection_num == *number));
    data.rounds = vec![selected];
    data.demo_info.total_ticks = inspection.last_packet_tick as u32;
    data.demo_info.game_start_offset = 0.0;
    let round = data
        .collections
        .first()
        .map(|c| c.round)
        .unwrap_or(data.rounds[0].round);
    data.rounds[0].round = round;
    data.rounds[0].start_tick = data.rounds[0].start_tick.max(1);
    data.rounds[0].next_start_tick = Some(inspection.last_packet_tick as u32 + 1);
    for col in &mut data.collections {
        col.round_start_tick = data.rounds[0].start_tick;
        col.round_freeze_end = col.round_freeze_end.max(2);
    }
    Ok(Some(inspection))
}

pub fn existing_assets(
    root: &std::path::Path,
    collections: &mut [Vec<KillCollection>],
    inputs: &mut [KillCollectionData],
    skip: bool,
) -> Result<Assets> {
    let mut result = Assets::new();
    for input in inputs {
        let path = std::path::PathBuf::from(&input.demo_info.demo_path);
        let Some(inspection) = standalone_clip(&path, input)? else {
            continue;
        };
        for col in collections
            .iter_mut()
            .flatten()
            .filter(|c| path_id(std::path::Path::new(&c.demo_path)) == path_id(&path))
        {
            col.round_start_tick = input.rounds[0].start_tick as i32;
            col.round_freeze_end = col.round_freeze_end.max(2);
            result
                .entry((col.collection_type.clone(), col.folder.clone()))
                .or_default()
                .push(TickAsset {
                    demo_name: col.demo_name.clone(),
                    collection_num: col.collection_num,
                    collection_type: col.collection_type.clone(),
                    folder: col.folder.clone(),
                    format: AssetFormat::Dem,
                    format_version: LOCAL_DEM_VERSION,
                    path: path.to_string_lossy().into(),
                    size_bytes: inspection.output_bytes as i64,
                    checksum: inspection.checksum.clone(),
                    status: interface::models::tick_asset::AssetStatus::Complete,
                    grenade_traj: 0,
                    authority_bytes: 0,
                    agent_life_count: 0,
                    weapon_lifetime_count: 0,
                    inventory_delta_count: 0,
                    world_weapon_delta_count: 0,
                    checkpoint_tick: None,
                    logical_start_tick: Some(if skip { col.round_freeze_end.max(2) } else { col.round_start_tick }),
                    logical_end_tick: Some(inspection.last_packet_tick),
                    source_path: None,
                    source_bytes: None,
                });
        }
    }
    let _ = root;
    Ok(result)
}

pub fn prefer_saved_rounds(
    entries: Vec<std::path::PathBuf>,
    config: &config::AppConfig,
    filter: &mut Option<HashMap<std::path::PathBuf, Vec<u32>>>,
) -> Vec<std::path::PathBuf> {
    let mut result = Vec::new();
    for source in entries {
        let index = catalog_clips(
            &config.paths.parser_output,
            source.parent().unwrap_or(&config.paths.parser_output),
        );
        if index.by_path.contains_key(&path_id(&source)) {
            result.push(source);
            continue;
        }
        let source_key = (
            path_id(source.parent().unwrap_or(&config.paths.parser_output)),
            name(&source.file_name().unwrap_or_default().to_string_lossy()).to_lowercase(),
        );
        let rows: Vec<_> = index
            .by_source
            .get(&source_key)
            .into_iter()
            .flatten()
            .map(|i| &index.clips[*i])
            .collect();
        let metadata = std::fs::metadata(&source).ok();
        let valid_source = rows.first().is_some_and(|r| {
            metadata.as_ref().is_some_and(|m| {
                m.len() as i64 == r.source_bytes
                    && m.modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_nanos() as i64)
                        == Some(r.modified_ns)
            })
        });
        let wanted = filter
            .as_ref()
            .and_then(|f| crate::selection::collections_for(f, &source.to_string_lossy()));
        let required: Vec<_> = rows
            .iter()
            .copied()
            .filter(|r| {
                wanted
                    .as_ref()
                    .map_or(true, |w| w.contains(&(r.number as u32)))
            })
            .collect();
        let complete = if let Some(wanted) = &wanted {
            wanted
                .iter()
                .all(|n| required.iter().any(|r| r.number as u32 == *n))
        } else {
            rows.iter().map(|r| r.number).max() == rows.first().map(|r| r.total)
        };
        let usable = valid_source
            && complete
            && !required.is_empty()
            && required.iter().all(|r| {
                (r.start
                    == if config.parser.skip_buy_time {
                        r.freeze.max(1)
                    } else {
                        1
                    })
                    && r.end >= r.start
                    && std::fs::metadata(&r.path)
                        .ok()
                        .is_some_and(|m| m.len() == r.bytes)
            });
        if !usable {
            result.push(source);
            continue;
        }
        for row in required {
            if config.parser.process_tick_data && !config.is_collection_type_enabled(&row.kind) {
                continue;
            }
            if let Some(filter) = filter.as_mut() {
                filter
                    .entry(row.path.clone())
                    .or_default()
                    .push(row.number as u32);
            }
            result.push(row.path.clone());
        }
    }
    result.sort();
    result.dedup();
    result
}

/// Packet ticks are already local. Embedded server-tick fields are converted
/// using the demo's own standard header, so no external offset is required.
pub fn normalize_embedded_ticks(output: &mut parser::parse_demo::DemoOutput) {
    use parser::second_pass::variants::Variant;
    let server_tick_offset = output
        .header
        .as_ref()
        .and_then(|h| h.get("server_start_tick"))
        .and_then(|s| s.parse::<i32>().ok());
    output.utility.server_tick_offset = server_tick_offset;
    let shift = server_tick_offset.unwrap_or(0);
    if shift == 0 {
        return;
    }
    for track in &mut output.smoke_voxels {
        for frame in &mut track.frames {
            if frame.effect_tick_begin > 0 {
                frame.effect_tick_begin = frame.effect_tick_begin.saturating_sub(shift);
            }
        }
    }
    for event in &mut output.game_events {
        for field in &mut event.fields {
            if !["message_tick", "attack_tick_count", "render_tick_count"]
                .contains(&field.name.as_str())
            {
                continue;
            }
            if let Some(value) = field.data.as_mut() {
                match value {
                    Variant::I32(tick) if *tick >= 0 => *tick = tick.saturating_sub(shift),
                    Variant::U32(tick) => {
                        *value = Variant::I32((*tick as i32).saturating_sub(shift))
                    }
                    _ => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_round25_accepts_two_checkpoint_prefix_rounds() {
        let mut rounds = vec![
            RoundInfo { round:23, start_tick:0, end_tick:248, round_freeze_end:0, next_start_tick:Some(696) },
            RoundInfo { round:24, start_tick:696, end_tick:3102, round_freeze_end:1976, next_start_tick:Some(3550) },
            RoundInfo { round:25, start_tick:3550, end_tick:8938, round_freeze_end:4830, next_start_tick:None },
        ];
        let path = std::path::Path::new("match_r25.dem");
        assert_eq!(saved_round_index(path, &rounds), Some(2));
        assert_eq!(saved_round_index(std::path::Path::new("match.dem"), &rounds), None);
        assert_eq!(saved_round_index(std::path::Path::new("match_r24.dem"), &rounds), None);
        rounds[0].next_start_tick = Some(700);
        assert_eq!(saved_round_index(path, &rounds), None);
        rounds[0].next_start_tick = Some(696);
        rounds[0].round_freeze_end = 100;
        assert_eq!(saved_round_index(path, &rounds), None);
    }
    #[test]
    fn legacy_round_scope_excludes_only_a_named_checkpoint_prefix() {
        let rounds = vec![
            RoundInfo { round:13, start_tick:0, end_tick:456, round_freeze_end:0, next_start_tick:Some(1000) },
            RoundInfo { round:14, start_tick:1000, end_tick:3996, round_freeze_end:2696, next_start_tick:None },
        ];
        assert_eq!(saved_round_index(std::path::Path::new("match_r14.dem"), &rounds), Some(1));
        assert_eq!(saved_round_index(std::path::Path::new("match.dem"), &rounds), None);
        assert_eq!(saved_round_index(std::path::Path::new("match_r13.dem"), &rounds), None);
        let mut full = rounds.clone(); full[0].round_freeze_end=100;
        assert_eq!(saved_round_index(std::path::Path::new("match_r14.dem"), &full), Some(1));
        assert_eq!(saved_round_index(std::path::Path::new("match.dem"), &full), None);
        full.push(rounds[1].clone());
        assert_eq!(saved_round_index(std::path::Path::new("match_r14.dem"), &full), None);
    }
    fn identity_fixture() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("output");
        let clip = dir.path().join("inputs/match_r17.dem");
        for kind in ["ACE", "TRIPLE"] {
            let db = config::storage::database_path(&output, kind, "inputs");
            std::fs::create_dir_all(db.parent().unwrap()).unwrap();
            let conn = duckdb::Connection::open(db).unwrap();
            kill_collection_master::duckdb::schema::initialize_tables(&conn).unwrap();
            if kind == "ACE" {
                conn.execute("INSERT INTO kill_collections (steam_id,demo_name,round,collection_num,col_total,type,round_freeze_end) VALUES ('ace','match',17,8,100,'ACE',2)", []).unwrap();
                conn.execute("INSERT INTO tick_assets (demo_name,collection_num,format,format_version,path,status,logical_start_tick,logical_end_tick,size_bytes) VALUES ('match',8,'DEM',5,?,'complete',2,800,100)", [clip.to_string_lossy().as_ref()]).unwrap();
            } else {
                // Known identity without any replay/trim registration, plus an unrelated round.
                conn.execute("INSERT INTO kill_collections (steam_id,demo_name,round,collection_num,col_total,type) VALUES ('triple','match',17,9,100,'TRIPLE'),('outside','match',18,92,100,'TRIPLE')", []).unwrap();
            }
        }
        (dir, output, clip)
    }
    fn discovered(killer: &str, kind: &str, number: i32) -> KillCollection {
        KillCollection { demo_name: "match_r17".into(), round: 1, killer_steamid: killer.into(), collection_type: kind.into(), collection_num: number, ..Default::default() }
    }
    #[test]
    fn additional_types_use_only_discovered_clip_collections_and_keep_known_ids() {
        let (_dir, output, clip) = identity_fixture();
        let mut collections = vec![discovered("ace", "ACE", 1), discovered("triple", "TRIPLE", 2), discovered("single", "SINGLE", 3)];
        restore_clip_identity(&output, &clip, &mut collections).unwrap();
        assert_eq!(collections.iter().map(|c| c.collection_num).collect::<Vec<_>>(), [8, 9, 101]);
        assert_eq!(collections.len(), 3); // Catalog round 18 was not imported.
        assert!(collections.iter().all(|c| c.demo_name == "match" && c.round == 17 && c.col_total == 101));
        let mut repeated = vec![discovered("single", "SINGLE", 1), discovered("ace", "ACE", 2)];
        restore_clip_identity(&output, &clip, &mut repeated).unwrap();
        assert_eq!(repeated.iter().map(|c| c.collection_num).collect::<Vec<_>>(), [101, 8]);
    }
    #[test]
    fn full_source_and_unregistered_trim_discovery_are_not_expanded_or_renumbered() {
        let (_dir, output, clip) = identity_fixture();
        for path in [clip.with_file_name("match.dem"), clip.with_file_name("standalone.dem")] {
            let mut collections = vec![discovered("single", "SINGLE", 1)];
            restore_clip_identity(&output, &path, &mut collections).unwrap();
            assert_eq!(collections.len(), 1);
            assert_eq!(collections[0].collection_num, 1);
            assert_eq!(collections[0].round, 1);
        }
    }
    #[test]
    fn conflicting_type_does_not_silently_reuse_a_collection_number() {
        let (_dir, output, clip) = identity_fixture();
        let mut collections = vec![discovered("ace", "TRIPLE", 1)];
        assert!(restore_clip_identity(&output, &clip, &mut collections).is_err());
    }
    #[test]
    fn parallel_saved_rounds_reserve_distinct_new_collection_numbers() {
        let (_dir, output, clip) = identity_fixture();
        let other_clip = clip.with_file_name("match_r18.dem");
        let db = config::storage::database_path(&output, "ACE", "inputs");
        let conn = duckdb::Connection::open(db).unwrap();
        conn.execute("INSERT INTO kill_collections (steam_id,demo_name,round,collection_num,col_total,type,round_freeze_end) VALUES ('ace2','match',18,93,100,'ACE',2)", []).unwrap();
        conn.execute("INSERT INTO tick_assets (demo_name,collection_num,format,format_version,path,status,logical_start_tick,logical_end_tick,size_bytes) VALUES ('match',93,'DEM',5,?,'complete',2,800,100)", [other_clip.to_string_lossy().as_ref()]).unwrap();
        drop(conn);
        let numbers = std::thread::scope(|scope| {
            let first = scope.spawn(|| { let mut cols = vec![discovered("new", "SINGLE", 1)]; restore_clip_identity(&output, &clip, &mut cols).unwrap(); cols[0].collection_num });
            let second = scope.spawn(|| { let mut cols = vec![discovered("new", "SINGLE", 1)]; restore_clip_identity(&output, &other_clip, &mut cols).unwrap(); cols[0].collection_num });
            [first.join().unwrap(), second.join().unwrap()]
        });
        assert_ne!(numbers[0], numbers[1]);
        assert!(numbers.iter().all(|&n| n == 101 || n == 102));
    }
    fn asset(kind: &str, number: i32) -> TickAsset {
        serde_json::from_value(serde_json::json!({
            "demo_name":"match", "collection_num":number, "collection_type":kind, "folder":"month",
            "format":"Dem", "format_version":4, "path":"match_r17.dem", "size_bytes":100, "checksum":"abc", "status":"Complete", "grenade_traj":0,
            "checkpoint_tick":101, "logical_start_tick":102, "logical_end_tick":900, "source_path":"original.dem", "source_bytes":10000
        })).unwrap()
    }
    #[test]
    fn round_collections_share_local_clock_without_source_mapping() {
        let mut collections = vec![vec![
            KillCollection {
                demo_name: "match".into(),
                demo_path: "original.dem".into(),
                folder: "month".into(),
                collection_type: "ACE".into(),
                collection_num: 8,
                col_total: 9,
                round: 17,
                start_kill_tick: 200,
                end_kill_tick: 300,
                round_start_tick: 10,
                round_freeze_end: 102,
                round_end_tick: 500,
                kill_ticks: vec![200, 300],
                util_thrown_ticks: "[210;220]".into(),
                ..Default::default()
            },
            KillCollection {
                demo_name: "match".into(),
                demo_path: "original.dem".into(),
                folder: "month".into(),
                collection_type: "QUAD".into(),
                collection_num: 9,
                col_total: 9,
                round: 17,
                start_kill_tick: 250,
                end_kill_tick: 350,
                round_start_tick: 10,
                round_freeze_end: 102,
                round_end_tick: 500,
                kill_ticks: vec![250, 350],
                ..Default::default()
            },
        ]];
        let mut assets = HashMap::from([
            (("ACE".into(), "month".into()), vec![asset("ACE", 8)]),
            (("QUAD".into(), "month".into()), vec![asset("QUAD", 9)]),
        ]);
        prepare(&mut collections, vec![], &mut assets, true).unwrap();
        assert_eq!(collections[0][0].kill_ticks, [100, 200]);
        assert_eq!(collections[0][1].kill_ticks, [150, 250]);
        assert_eq!(collections[0][0].util_thrown_ticks, "[110;120]");
        assert_eq!(
            collections[0]
                .iter()
                .map(|c| c.collection_num)
                .collect::<Vec<_>>(),
            [8, 9]
        );
        for col in &collections[0] {
            assert_eq!(col.round, 17);
            assert_eq!(col.demo_path, "match_r17.dem");
            assert_eq!(col.round_freeze_end, 2);
        }
        for a in assets.values().flatten() {
            assert_eq!(a.format_version, LOCAL_DEM_VERSION);
            assert_eq!(
                (a.logical_start_tick, a.logical_end_tick),
                (Some(2), Some(800))
            );
            assert_eq!(
                (a.checkpoint_tick, a.source_path.as_ref(), a.source_bytes),
                (None, None, None)
            );
        }
    }
    #[test]
    fn selected_saved_clip_validates_against_original_catalog_identity() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("inputs/match_r17.dem");
        let output = dir.path().join("output");
        let db = config::storage::database_path(&output, "ACE", "inputs");
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let conn = duckdb::Connection::open(db).unwrap();
        kill_collection_master::duckdb::schema::initialize_tables(&conn).unwrap();
        conn.execute("INSERT INTO kill_collections (steam_id,demo_name,round,collection_num,col_total,type) VALUES ('1','match',17,8,8,'ACE')", []).unwrap();
        drop(conn);
        let col = KillCollection { demo_name: "match".into(), demo_path: source.to_string_lossy().into(), folder: "inputs".into(), collection_type: "ACE".into(), collection_num: 8, round: 17, ..Default::default() };
        let filter = HashMap::from([(source.clone(), vec![8])]);
        crate::validate_asset_refresh_targets(&[vec![col]], &[(source.clone(), source)], &filter, &output).unwrap();
    }

    #[test]
    fn timeline_refresh_preserves_annotations_and_rolls_back_on_missing_target() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ACE_month.duckdb");
        let conn = duckdb::Connection::open(&path).unwrap();
        kill_collection_master::duckdb::schema::initialize_tables(&conn).unwrap();
        conn.execute("INSERT INTO kill_collections (steam_id,demo_name,round,collection_num,col_total,type,tag,start_kill_tick) VALUES ('1','match',17,8,8,'ACE','keep me',900)",[]).unwrap();
        drop(conn);
        let writer = kill_collection_master::master_writer::MasterWriter::new(
            &path.to_string_lossy(),
            "ACE",
            "month",
        )
        .unwrap();
        let col = KillCollection {
            demo_name: "match".into(),
            collection_num: 8,
            demo_path: "match_r17.dem".into(),
            start_kill_tick: 100,
            end_kill_tick: 200,
            round_start_tick: 1,
            round_end_tick: 400,
            round_freeze_end: 2,
            kill_ticks: vec![100, 200],
            ..Default::default()
        };
        writer
            .repair_catalog_assets_with_timeline(&[], &[], &[col.clone()], None)
            .unwrap();
        let conn = duckdb::Connection::open(&path).unwrap();
        let before: (String, i32, String) = conn
            .query_row(
                "SELECT tag,start_kill_tick,kill_ticks FROM kill_collections",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(before, ("keep me".into(), 100, "100;200".into()));
        drop(conn);
        let mut changed = col.clone();
        changed.start_kill_tick = 123;
        let mut missing = col;
        missing.collection_num = 999;
        assert!(writer
            .repair_catalog_assets_with_timeline(&[], &[], &[changed, missing], None)
            .is_err());
        let conn = duckdb::Connection::open(&path).unwrap();
        let after: i32 = conn
            .query_row("SELECT start_kill_tick FROM kill_collections", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(after, 100);
    }
}
