//! Integrated, DuckDB-backed round trimming.

use anyhow::{Context, Result};
use dashmap::DashMap;
use demo_writer::{
    trim_rounds_verified_with_source_data, VerifiedRoundTrimOutcome, VerifiedRoundTrimRequest,
    VerifiedTrimOptions, VERIFIED_TRIM_FORMAT_VERSION,
};
use interface::models::collection::KillCollection;
use interface::models::demo_source::DemoSource;
use interface::models::tick_asset::{AssetFormat, AssetStatus, TickAsset};
use kill_collection_master::master_writer::MasterWriter;
use rayon::prelude::*;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

/// Trimming is I/O- and CPU-heavy, so keep it off the parsing pool and avoid saturating the
/// source/output drives. The pool is constructed once for the parser run and reused by batches.
pub const DEFAULT_TRIM_CONCURRENCY: usize = 3;

/// Cleanup is allowed only for a complete, unselected trim import. A verified DEM clip and its
/// durable DuckDB row form the commit barrier; S2R generation is optional for trim-only archive
/// reduction, so it must not be a prerequisite for reclaiming an explicitly requested raw DEM.
pub fn source_cleanup_enabled(requested: bool, _process_replays: bool, selected: bool) -> bool {
    requested && !selected
}

#[derive(Debug, Clone)]
pub struct TrimTarget {
    pub collection_type: String,
    pub folder: String,
    pub demo_name: String,
    pub collection_num: i32,
}

#[derive(Debug, Clone)]
pub struct SourceTrimPlan {
    pub original_path: PathBuf,
    pub materialized_path: PathBuf,
    pub rounds: BTreeMap<i32, Vec<TrimTarget>>,
}

type GroupedAssets = HashMap<(String, String), Vec<TickAsset>>;

fn path_key(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase()
}

/// Build only from collections that survived selection and catalog filtering.
pub fn build_trim_plans(
    collections: &[Vec<KillCollection>],
    source_paths: &[(PathBuf, PathBuf)],
) -> Result<Vec<SourceTrimPlan>> {
    let origins: HashMap<String, (PathBuf, PathBuf)> = source_paths
        .iter()
        .map(|(materialized, original)| {
            let materialized = materialized
                .canonicalize()
                .unwrap_or_else(|_| materialized.clone());
            let original = original.canonicalize().unwrap_or_else(|_| original.clone());
            (path_key(&materialized), (materialized, original))
        })
        .collect();
    let mut plans: HashMap<String, SourceTrimPlan> = HashMap::new();

    for collection in collections.iter().flatten() {
        let materialized_key = path_key(Path::new(&collection.demo_path));
        let (materialized, original) =
            origins.get(&materialized_key).cloned().with_context(|| {
                format!(
                    "collection {} round {} lost its source provenance for {}",
                    collection.demo_name, collection.round, collection.demo_path
                )
            })?;
        let original_key = path_key(&original);
        plans
            .entry(original_key)
            .or_insert_with(|| SourceTrimPlan {
                original_path: original,
                materialized_path: materialized,
                rounds: BTreeMap::new(),
            })
            .rounds
            .entry(collection.round)
            .or_default()
            .push(TrimTarget {
                collection_type: collection.collection_type.clone(),
                folder: collection.folder.clone(),
                demo_name: collection.demo_name.clone(),
                collection_num: collection.collection_num,
            });
    }

    let mut plans: Vec<_> = plans.into_values().collect();
    plans.sort_by(|left, right| left.original_path.cmp(&right.original_path));
    Ok(plans)
}

/// Keep only rounds containing at least one trim-enabled collection type. Targets are deliberately
/// retained as a whole: one physical clip serves the entire round, so every catalogued collection
/// in a selected round should receive the same DEM asset without creating another file.
pub fn retain_trim_enabled_rounds(
    plans: &mut Vec<SourceTrimPlan>,
    is_enabled: impl Fn(&str) -> bool,
) {
    for plan in plans.iter_mut() {
        plan.rounds.retain(|_, targets| {
            targets
                .iter()
                .any(|target| is_enabled(&target.collection_type))
        });
    }
    plans.retain(|plan| !plan.rounds.is_empty());
}

pub fn grouped_demo_sources(
    plans: &[SourceTrimPlan],
) -> Result<HashMap<(String, String), Vec<DemoSource>>> {
    let mut grouped = HashMap::<(String, String), HashMap<String, DemoSource>>::new();
    for plan in plans {
        let metadata = std::fs::metadata(&plan.original_path).with_context(|| {
            format!(
                "could not read source metadata for {}",
                plan.original_path.display()
            )
        })?;
        let size_bytes = i64::try_from(metadata.len()).with_context(|| {
            format!(
                "source is too large to catalogue: {}",
                plan.original_path.display()
            )
        })?;
        let modified_ns = metadata
            .modified()
            .with_context(|| format!("could not read mtime for {}", plan.original_path.display()))?
            .duration_since(UNIX_EPOCH)
            .with_context(|| {
                format!(
                    "source mtime predates the Unix epoch: {}",
                    plan.original_path.display()
                )
            })?
            .as_nanos()
            .min(i64::MAX as u128) as i64;
        for target in plan.rounds.values().flatten() {
            grouped
                .entry((target.collection_type.clone(), target.folder.clone()))
                .or_default()
                .entry(target.demo_name.clone())
                .or_insert_with(|| DemoSource {
                    demo_name: target.demo_name.clone(),
                    source_path: plan.original_path.to_string_lossy().to_string(),
                    size_bytes,
                    modified_ns,
                });
        }
    }
    Ok(grouped
        .into_iter()
        .map(|(key, values)| (key, values.into_values().collect()))
        .collect())
}

fn source_stem(path: &Path) -> Result<String> {
    let mut name = path
        .file_name()
        .and_then(|value| value.to_str())
        .context("source demo has no valid file name")?
        .to_string();
    for suffix in [".gz", ".zst", ".dem"] {
        if name.to_ascii_lowercase().ends_with(suffix) {
            name.truncate(name.len() - suffix.len());
        }
    }
    Ok(name)
}

fn output_for(plan: &SourceTrimPlan, round: i32) -> Result<PathBuf> {
    let directory = plan
        .original_path
        .parent()
        .unwrap_or_else(|| Path::new("."));
    Ok(directory.join(format!(
        "{}_r{round}.dem",
        source_stem(&plan.original_path)?
    )))
}

fn fan_out_outcome(
    plan: &SourceTrimPlan,
    outcome: &VerifiedRoundTrimOutcome,
    grouped_assets: &mut GroupedAssets,
) -> Result<()> {
    let source_bytes = i64::try_from(
        std::fs::metadata(&plan.original_path)
            .with_context(|| {
                format!(
                    "could not read source metadata for DEM asset {}",
                    plan.original_path.display()
                )
            })?
            .len(),
    )
    .context("source is too large to describe in a DEM asset row")?;
    let source_path = plan.original_path.to_string_lossy().to_string();
    let targets = plan
        .rounds
        .get(&outcome.round)
        .with_context(|| format!("trim returned unexpected round {}", outcome.round))?;
    for target in targets {
        grouped_assets
            .entry((target.collection_type.clone(), target.folder.clone()))
            .or_default()
            .push(TickAsset {
                demo_name: target.demo_name.clone(),
                collection_num: target.collection_num,
                collection_type: target.collection_type.clone(),
                folder: target.folder.clone(),
                format: AssetFormat::Dem,
                format_version: VERIFIED_TRIM_FORMAT_VERSION,
                path: outcome.destination.to_string_lossy().to_string(),
                size_bytes: outcome.output_bytes as i64,
                checksum: outcome.checksum.clone(),
                status: AssetStatus::Complete,
                grenade_traj: 0,
                authority_bytes: 0,
                agent_life_count: 0,
                weapon_lifetime_count: 0,
                inventory_delta_count: 0,
                world_weapon_delta_count: 0,
                checkpoint_tick: Some(outcome.checkpoint_tick),
                logical_start_tick: Some(outcome.logical_start_tick),
                logical_end_tick: Some(outcome.logical_end_tick),
                source_path: Some(source_path.clone()),
                source_bytes: Some(source_bytes),
            });
    }
    Ok(())
}

/// A cleanup target is an original source that has just passed trim verification and database
/// registration. Both raw DEMs and compressed DEM archives are eligible. `_rN.dem` files are
/// already-published round clips and must never be reclaimed as if they were raw sources.
/// Be deliberately conservative about a full source whose name happens to resemble a round
/// clip: retaining one rare source is safer than deleting a clip.
fn is_cleanup_eligible_source(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".dem.gz") || lower.ends_with(".dem.zst") {
        return true;
    }
    if !lower.ends_with(".dem") {
        return false;
    }
    let stem = &lower[..lower.len() - ".dem".len()];
    !stem.rsplit_once("_r").is_some_and(|(_, round)| {
        !round.is_empty() && round.bytes().all(|byte| byte.is_ascii_digit())
    })
}

fn register_assets_then_cleanup<F>(
    plans: &[SourceTrimPlan],
    grouped_assets: GroupedAssets,
    delete_sources: bool,
    mut register: F,
) -> Result<()>
where
    F: FnMut(&str, &str, &[TickAsset]) -> Result<()>,
{
    if !plans.is_empty() && grouped_assets.is_empty() {
        anyhow::bail!("refusing source cleanup because no DEM assets were produced");
    }

    // Registration is deliberately complete before deletion begins. Any error returns through
    // `?` with every original still present.
    for ((collection_type, folder), assets) in grouped_assets {
        register(&collection_type, &folder, &assets)?;
    }

    cleanup_sources(plans, delete_sources)
}

fn cleanup_sources(plans: &[SourceTrimPlan], delete_sources: bool) -> Result<()> {
    if delete_sources {
        let mut deleted = HashSet::new();
        for plan in plans {
            if !deleted.insert(path_key(&plan.original_path)) || !plan.original_path.exists() {
                continue;
            }
            if is_cleanup_eligible_source(&plan.original_path) {
                std::fs::remove_file(&plan.original_path).with_context(|| {
                    format!(
                        "could not delete verified source {}",
                        plan.original_path.display()
                    )
                })?;
            } else {
                println!(
                    "Keeping source {}: it is a published round clip or unsupported source type",
                    plan.original_path.display()
                );
            }
        }
    }
    Ok(())
}

/// Verified clip publication is a prerequisite of replay parsing and catalog writes. The
/// callback must finish every requested replay and database commit before source cleanup.
/// A failed trim never enters the callback; a failed callback never deletes a source.
pub fn with_verified_trims<T>(
    plans: &[SourceTrimPlan],
    trim_pool: &rayon::ThreadPool,
    delete_sources: bool,
    skip_buy_time: bool,
    process_and_commit: impl FnOnce(GroupedAssets) -> Result<T>,
) -> Result<T> {
    with_trim_operation(
        plans,
        trim_pool,
        delete_sources,
        |plan| trim_source_plan(plan, skip_buy_time),
        process_and_commit,
    )
}

fn with_trim_operation<T>(
    plans: &[SourceTrimPlan],
    trim_pool: &rayon::ThreadPool,
    delete_sources: bool,
    trim: impl Fn(&SourceTrimPlan) -> Result<Vec<VerifiedRoundTrimOutcome>> + Send + Sync,
    process_and_commit: impl FnOnce(GroupedAssets) -> Result<T>,
) -> Result<T> {
    let outcomes = preflight_and_trim_all(plans, trim_pool, trim)?;
    let assets = merge_trim_outcomes(plans, outcomes)?;
    if !plans.is_empty() && assets.is_empty() {
        anyhow::bail!("no verified DEM assets were produced");
    }
    let result = process_and_commit(assets)?;
    cleanup_sources(plans, delete_sources)?;
    Ok(result)
}

fn trim_source_plan(plan: &SourceTrimPlan, skip_buy_time: bool) -> Result<Vec<VerifiedRoundTrimOutcome>> {
    let requests = plan
        .rounds
        .keys()
        .map(|round| {
            Ok(VerifiedRoundTrimRequest {
                round: *round,
                destination: output_for(plan, *round)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let cached = interface::demo_cache::get_cached_demo(&plan.materialized_path.to_string_lossy());
    let rounds =
        interface::demo_cache::get_cached_round_events(&plan.materialized_path.to_string_lossy());
    trim_rounds_verified_with_source_data(
        &plan.materialized_path,
        &requests,
        VerifiedTrimOptions {
            force: true,
            skip_buy_time,
            // Keep this explicit: verified output is the trim pipeline's contract, rather than
            // an accidental consequence of the current DemoWriter default.
            parse_check: true,
            ..Default::default()
        },
        cached.as_deref().map(Vec::as_slice),
        rounds.as_deref().map(Vec::as_slice),
    )
    .with_context(|| format!("could not trim {}", plan.original_path.display()))
}

fn preflight_trim_destinations(plans: &[SourceTrimPlan]) -> Result<()> {
    let inputs: HashSet<_> = plans.iter().flat_map(|plan| [
        path_key(&plan.original_path), path_key(&plan.materialized_path),
    ]).collect();
    let mut destinations = HashMap::<String, (PathBuf, i32, PathBuf)>::new();
    for plan in plans {
        for round in plan.rounds.keys() {
            let destination = output_for(plan, *round)?;
            let key = path_key(&destination);
            if inputs.contains(&key) {
                anyhow::bail!("trim output would overwrite an input demo: {}", destination.display());
            }
            if let Some((previous_source, previous_round, previous_destination)) = destinations
                .insert(
                    key,
                    (plan.original_path.clone(), *round, destination.clone()),
                )
            {
                anyhow::bail!(
                    "trim output collision at {}: {} round {} and {} round {} both target {}",
                    destination.display(),
                    previous_source.display(),
                    previous_round,
                    plan.original_path.display(),
                    round,
                    previous_destination.display(),
                );
            }
        }
    }
    Ok(())
}

fn trim_all_plans<F>(
    plans: &[SourceTrimPlan],
    trim_pool: &rayon::ThreadPool,
    trim: F,
) -> Result<Vec<Vec<VerifiedRoundTrimOutcome>>>
where
    F: Fn(&SourceTrimPlan) -> Result<Vec<VerifiedRoundTrimOutcome>> + Send + Sync,
{
    // `collect` on this indexed parallel iterator preserves the input order. That lets the
    // sequential fan-out below preserve catalog/source ordering while all physical clip work
    // happens on the dedicated pool.
    trim_pool.install(|| plans.par_iter().map(trim).collect())
}

fn preflight_and_trim_all<F>(
    plans: &[SourceTrimPlan],
    trim_pool: &rayon::ThreadPool,
    trim: F,
) -> Result<Vec<Vec<VerifiedRoundTrimOutcome>>>
where
    F: Fn(&SourceTrimPlan) -> Result<Vec<VerifiedRoundTrimOutcome>> + Send + Sync,
{
    // Detect output collisions before scheduling any worker. We must never let two source
    // variants race to overwrite the same clip, even when the later catalog barrier holds.
    preflight_trim_destinations(plans)?;
    trim_all_plans(plans, trim_pool, trim)
}

fn merge_trim_outcomes(
    plans: &[SourceTrimPlan],
    outcomes_by_plan: Vec<Vec<VerifiedRoundTrimOutcome>>,
) -> Result<GroupedAssets> {
    if plans.len() != outcomes_by_plan.len() {
        anyhow::bail!("trim result count did not match source plan count");
    }

    let mut grouped_assets = GroupedAssets::new();
    for (plan, outcomes) in plans.iter().zip(outcomes_by_plan) {
        let returned: HashSet<_> = outcomes.iter().map(|outcome| outcome.round).collect();
        if outcomes.len() != plan.rounds.len()
            || returned.len() != outcomes.len()
            || !plan.rounds.keys().all(|round| returned.contains(round))
        {
            anyhow::bail!("verified trim outcomes do not cover every planned round for {}", plan.original_path.display());
        }
        for outcome in outcomes {
            if path_key(&outcome.destination) != path_key(&output_for(plan, outcome.round)?) {
                anyhow::bail!("verified trim destination does not match its plan: {}", outcome.destination.display());
            }
            fan_out_outcome(plan, &outcome, &mut grouped_assets)?;
        }
    }
    Ok(grouped_assets)
}

/// Generate every clip first, register all fan-out rows second, and delete no sources until every
/// registration succeeds. This makes deletion a batch-level commit consequence, not a subprocess
/// exit-code consequence.
pub fn execute_trim_plans(
    plans: &[SourceTrimPlan],
    parser_output: &Path,
    master_file_lock: &Arc<DashMap<PathBuf, Arc<Mutex<()>>>>,
    trim_pool: &rayon::ThreadPool,
    delete_sources: bool,
) -> Result<()> {
    if delete_sources {
        anyhow::bail!("source cleanup requires the integrated replay-and-catalog import, not trim-only execution");
    }
    if plans.is_empty() {
        return Ok(());
    }

    // Do not merge, register, or delete until every source plan has succeeded. Individual
    // workers may have published clips before a peer fails; those remain retryable artifacts,
    // but the catalog and source-cleanup commit barrier is preserved.
    let outcomes_by_plan = preflight_and_trim_all(plans, trim_pool, |plan| trim_source_plan(plan, true))?;
    let grouped_assets = merge_trim_outcomes(plans, outcomes_by_plan)?;

    register_assets_then_cleanup(
        plans,
        grouped_assets,
        delete_sources,
        |collection_type, folder, assets| {
            let master_path = config::storage::database_path(parser_output, &collection_type, &folder);
            let writer =
                MasterWriter::new(&master_path.to_string_lossy(), &collection_type, &folder)?;
            let entry = master_file_lock
                .entry(master_path)
                .or_insert_with(|| Arc::new(Mutex::new(())));
            let lock = Arc::clone(entry.value());
            writer.write_tick_assets(assets, Some(&lock))?;
            Ok(())
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    #[test]
    #[ignore = "requires upstream src/parser/test_demo.dem; run explicitly with --ignored"]
    fn discovery_round_reuse_matches_standalone_verified_trimming() {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../parser/test_demo.dem");
        let directory = tempfile::tempdir().unwrap();
        let _processor =
            interface::core::demo_processor::DemoProcessor::new(source.to_str().unwrap()).unwrap();
        let events =
            interface::demo_cache::get_cached_round_events(&source.to_string_lossy()).unwrap();
        let requests = |prefix: &str| [2, 3].map(|round| VerifiedRoundTrimRequest {
            round, destination: directory.path().join(format!("{prefix}-{round}.dem")),
        });
        let originals = requests("original");
        let reused = requests("reused");
        demo_writer::trim_rounds_verified(&source, &originals, VerifiedTrimOptions::default()).unwrap();
        let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build().unwrap();
        let outcomes = pool.install(|| demo_writer::trim_rounds_verified_with_source_data(
            &source, &reused, VerifiedTrimOptions::default(), None, Some(&events),
        )).unwrap();
        assert_eq!(outcomes.iter().map(|outcome| outcome.round).collect::<Vec<_>>(), [2, 3]);
        for (original, reused) in originals.iter().zip(&reused) {
            assert_eq!(std::fs::read(&original.destination).unwrap(), std::fs::read(&reused.destination).unwrap());
        }
    }

    #[test]
    fn trim_is_a_prerequisite_and_cleanup_is_a_commit_consequence() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("match.dem");
        std::fs::write(&source, b"source").unwrap();
        let plans = vec![plan(source.clone())];
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        let trimmed = AtomicUsize::new(0);
        let trim = |_: &SourceTrimPlan| {
            trimmed.fetch_add(1, Ordering::SeqCst);
            Ok(vec![outcome(&dir.path().join("match_r4.dem"))])
        };
        let result: Result<()> = with_trim_operation(&plans, &pool, true, trim, |assets| {
            assert_eq!(trimmed.load(Ordering::SeqCst), 1);
            assert!(!assets.is_empty());
            assert!(source.exists());
            anyhow::bail!("replay or database failed");
        });
        assert!(result.is_err());
        assert!(source.exists());
        with_trim_operation(&plans, &pool, true, trim, |_| {
            assert!(source.exists());
            Ok(())
        })
        .unwrap();
        assert!(!source.exists());
    }

    #[test]
    fn failed_trim_never_starts_replay_or_database_work() {
        let plans = vec![plan(PathBuf::from("match.dem"))];
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        let result: Result<()> = with_trim_operation(
            &plans,
            &pool,
            false,
            |_| anyhow::bail!("trim failed"),
            |_| panic!("replay/database must not start"),
        );
        assert!(result.is_err());
    }

    #[test]
    fn incomplete_duplicate_or_misdirected_trims_never_commit_or_delete() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("match.dem");
        std::fs::write(&source, b"source").unwrap();
        let mut source_plan = plan(source.clone());
        source_plan.rounds.insert(5, vec![target("QUAD", 2)]);
        let pool = rayon::ThreadPoolBuilder::new().num_threads(1).build().unwrap();
        for mode in 0..4 {
            let result: Result<()> = with_trim_operation(&[source_plan.clone()], &pool, true, |_| {
                let first = outcome(&dir.path().join("match_r4.dem"));
                let mut second = outcome(&dir.path().join("match_r5.dem"));
                second.round = 5;
                Ok(match mode {
                    0 => vec![],
                    1 => vec![first],
                    2 => vec![outcome(&dir.path().join("match_r4.dem")), first],
                    _ => { second.destination = source.clone(); vec![first, second] }
                })
            }, |_| panic!("invalid trims must not enter replay or catalog commit"));
            assert!(result.is_err());
            assert!(source.exists());
        }
    }

    #[test]
    fn trim_destination_cannot_overwrite_another_input() {
        let plans = [plan(PathBuf::from("match.dem")), plan(PathBuf::from("match_r4.dem"))];
        assert!(preflight_trim_destinations(&plans).unwrap_err().to_string().contains("overwrite an input"));
    }

    #[test]
    fn cleanup_requires_explicit_unselected_import() {
        assert!(source_cleanup_enabled(true, true, false));
        assert!(source_cleanup_enabled(true, false, false));
        assert!(!source_cleanup_enabled(false, true, false));
        assert!(!source_cleanup_enabled(true, true, true));
    }

    fn target(collection_type: &str, collection_num: i32) -> TrimTarget {
        TrimTarget {
            collection_type: collection_type.into(),
            folder: "folder".into(),
            demo_name: "match".into(),
            collection_num,
        }
    }

    fn outcome(path: &Path) -> VerifiedRoundTrimOutcome {
        VerifiedRoundTrimOutcome {
            round: 4,
            destination: path.to_path_buf(),
            output_bytes: 123,
            checksum: "abc".into(),
            checkpoint_tick: 10,
            logical_start_tick: 20,
            logical_end_tick: 30,
            file_info_offset: 20,
            spawn_groups_offset: 30,
            playback_ticks: 40,
            playback_frames: 50,
            playback_time: 1.0,
        }
    }

    fn plan(source: PathBuf) -> SourceTrimPlan {
        SourceTrimPlan {
            original_path: source.clone(),
            materialized_path: source.clone(),
            rounds: BTreeMap::from([(4, vec![target("ACE", 1)])]),
        }
    }

    #[test]
    fn source_stem_handles_compressed_demo_suffixes() {
        assert_eq!(source_stem(Path::new("match.dem.gz")).unwrap(), "match");
        assert_eq!(source_stem(Path::new("match.dem.zst")).unwrap(), "match");
        assert_eq!(source_stem(Path::new("match.dem")).unwrap(), "match");
    }

    #[test]
    fn one_round_fans_out_to_every_collection_and_partition() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("match.dem");
        std::fs::write(&source, b"source demo").unwrap();
        let plan = SourceTrimPlan {
            original_path: source.clone(),
            materialized_path: source.clone(),
            rounds: BTreeMap::from([(
                4,
                vec![target("ACE", 1), target("ACE", 2), target("TRIPLE", 3)],
            )]),
        };
        let clip = PathBuf::from("match_r4.dem");
        let mut grouped = GroupedAssets::new();

        fan_out_outcome(&plan, &outcome(&clip), &mut grouped).unwrap();

        assert_eq!(grouped.values().map(Vec::len).sum::<usize>(), 3);
        assert_eq!(grouped.len(), 2);
        assert!(grouped
            .values()
            .flatten()
            .all(|asset| asset.path == clip.to_string_lossy()));
        assert!(grouped.values().flatten().all(|asset| {
            asset.checkpoint_tick == Some(10)
                && asset.logical_start_tick == Some(20)
                && asset.logical_end_tick == Some(30)
                && asset.source_path.as_deref() == Some(source.to_string_lossy().as_ref())
                && asset.source_bytes == Some(11)
        }));
    }

    #[test]
    fn provenance_miss_is_an_error_instead_of_a_temp_path_guess() {
        let collection = KillCollection {
            demo_name: "match".into(),
            demo_path: "missing-materialized.dem".into(),
            round: 4,
            ..Default::default()
        };

        assert!(build_trim_plans(&[vec![collection]], &[]).is_err());
    }

    #[test]
    fn unreadable_source_metadata_is_an_error() {
        let missing = PathBuf::from("definitely-missing-source.dem");
        assert!(grouped_demo_sources(&[plan(missing)]).is_err());
    }

    #[test]
    fn same_source_and_round_build_one_plan_with_all_targets() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("match.dem");
        std::fs::write(&source, b"demo").unwrap();
        let collections = vec![
            KillCollection {
                collection_type: "ACE".into(),
                folder: "folder".into(),
                demo_name: "match".into(),
                demo_path: source.to_string_lossy().into(),
                collection_num: 1,
                round: 4,
                ..Default::default()
            },
            KillCollection {
                collection_type: "TRIPLE".into(),
                folder: "folder".into(),
                demo_name: "match".into(),
                demo_path: source.to_string_lossy().into(),
                collection_num: 2,
                round: 4,
                ..Default::default()
            },
        ];

        let plans = build_trim_plans(&[collections], &[(source.clone(), source)]).unwrap();

        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].rounds.len(), 1);
        assert_eq!(plans[0].rounds[&4].len(), 2);
    }

    #[test]
    fn trim_filter_drops_unwanted_only_rounds_and_empty_sources() {
        let source = PathBuf::from("match.dem");
        let mut plans = vec![SourceTrimPlan {
            original_path: source.clone(),
            materialized_path: source,
            rounds: BTreeMap::from([(4, vec![target("TRIPLE", 1)])]),
        }];

        retain_trim_enabled_rounds(&mut plans, |kind| matches!(kind, "ACE" | "QUAD" | "MULTI"));

        assert!(plans.is_empty());
    }

    #[test]
    fn trim_filter_keeps_all_targets_when_any_type_selects_the_round() {
        let source = PathBuf::from("match.dem");
        let mut plans = vec![SourceTrimPlan {
            original_path: source.clone(),
            materialized_path: source,
            rounds: BTreeMap::from([
                (4, vec![target("TRIPLE", 1)]),
                (5, vec![target("QUAD", 2), target("TRIPLE", 3)]),
            ]),
        }];

        retain_trim_enabled_rounds(&mut plans, |kind| matches!(kind, "ACE" | "QUAD" | "MULTI"));

        assert_eq!(plans.len(), 1);
        assert!(!plans[0].rounds.contains_key(&4));
        assert_eq!(plans[0].rounds[&5].len(), 2);
    }

    #[test]
    fn registration_failure_keeps_an_unarchived_raw_source() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("match.dem");
        std::fs::write(&source, b"raw").unwrap();
        let plan = plan(source.clone());
        let mut assets = GroupedAssets::new();
        fan_out_outcome(
            &plan,
            &outcome(&directory.path().join("match_r4.dem")),
            &mut assets,
        )
        .unwrap();

        let result = register_assets_then_cleanup(&[plan], assets, true, |_, _, _| {
            anyhow::bail!("induced registration failure")
        });

        assert!(result.is_err());
        assert!(source.exists());
    }

    #[test]
    fn verified_registration_deletes_an_unarchived_raw_demo() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("match.dem");
        std::fs::write(&source, b"raw").unwrap();
        let plan = plan(source.clone());
        let mut assets = GroupedAssets::new();
        fan_out_outcome(
            &plan,
            &outcome(&directory.path().join("match_r4.dem")),
            &mut assets,
        )
        .unwrap();

        register_assets_then_cleanup(&[plan], assets, true, |_, _, _| Ok(())).unwrap();

        assert!(!source.exists());
    }

    #[test]
    fn archive_siblings_are_retained_but_not_required_for_raw_cleanup() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("match.dem");
        let archive = directory.path().join("match.dem.gz");
        std::fs::write(&source, b"raw").unwrap();
        std::fs::write(&archive, b"corrupt").unwrap();
        cleanup_sources(&[plan(source.clone())], true).unwrap();
        assert!(!source.exists());
        assert!(archive.exists());
    }

    #[test]
    fn disabled_cleanup_keeps_raw_demo() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("match.dem");
        std::fs::write(&source, b"raw").unwrap();
        let plan = plan(source.clone());
        let mut assets = GroupedAssets::new();
        fan_out_outcome(
            &plan,
            &outcome(&directory.path().join("match_r4.dem")),
            &mut assets,
        )
        .unwrap();

        register_assets_then_cleanup(&[plan], assets, false, |_, _, _| Ok(())).unwrap();

        assert!(source.exists());
    }

    #[test]
    fn verified_archive_source_is_deleted() {
        let directory = tempfile::tempdir().unwrap();
        let archive = directory.path().join("match.dem.zst");
        std::fs::write(&archive, b"archive").unwrap();
        let plan = plan(archive.clone());
        let mut assets = GroupedAssets::new();
        fan_out_outcome(
            &plan,
            &outcome(&directory.path().join("match_r4.dem")),
            &mut assets,
        )
        .unwrap();

        register_assets_then_cleanup(&[plan], assets, true, |_, _, _| Ok(())).unwrap();

        assert!(!archive.exists());
    }

    #[test]
    fn published_round_clip_is_never_deleted() {
        let directory = tempfile::tempdir().unwrap();
        let clip = directory.path().join("match_r12.dem");
        std::fs::write(&clip, b"clip").unwrap();
        cleanup_sources(&[plan(clip.clone())], true).unwrap();
        assert!(clip.exists());
    }

    #[test]
    fn double_named_published_round_clip_is_never_deleted() {
        let directory = tempfile::tempdir().unwrap();
        let clip = directory.path().join("match_r25_r25.dem");
        std::fs::write(&clip, b"clip").unwrap();
        cleanup_sources(&[plan(clip.clone())], true).unwrap();
        assert!(clip.exists());
    }

    #[test]
    fn dedicated_pool_runs_source_plans_concurrently_and_keeps_source_order() {
        let plans = (0..4)
            .map(|index| plan(PathBuf::from(format!("source-{index}.dem"))))
            .collect::<Vec<_>>();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        let active = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);

        let outcomes = trim_all_plans(&plans, &pool, |source_plan| {
            let current = active.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(current, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(20));
            active.fetch_sub(1, Ordering::SeqCst);
            Ok(vec![outcome(&source_plan.original_path)])
        })
        .unwrap();

        assert_eq!(peak.load(Ordering::SeqCst), 2);
        assert_eq!(outcomes.len(), plans.len());
        for (plan, outcomes) in plans.iter().zip(outcomes) {
            assert_eq!(outcomes[0].destination, plan.original_path);
        }
    }

    #[test]
    fn a_trim_failure_returns_before_assets_can_be_merged_or_registered() {
        let plans = vec![
            plan(PathBuf::from("good.dem")),
            plan(PathBuf::from("bad.dem")),
        ];
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();

        let result = trim_all_plans(&plans, &pool, |source_plan| {
            if source_plan.original_path == Path::new("bad.dem") {
                anyhow::bail!("induced trim failure");
            }
            Ok(vec![outcome(&source_plan.original_path)])
        });

        assert!(result.is_err());
        // `execute_trim_plans` calls merge/register only after this `Result` is unwrapped, so a
        // failed worker leaves the catalog and source-cleanup barrier untouched.
    }

    #[test]
    fn colliding_compressed_source_stems_fail_before_any_trim_worker_can_write() {
        let directory = tempfile::tempdir().unwrap();
        let plans = vec![
            plan(directory.path().join("match.dem.gz")),
            plan(directory.path().join("match.dem.zst")),
        ];
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        let worker_runs = AtomicUsize::new(0);
        let destination = directory.path().join("match_r4.dem");

        let error = preflight_and_trim_all(&plans, &pool, |source_plan| {
            worker_runs.fetch_add(1, Ordering::SeqCst);
            std::fs::write(output_for(source_plan, 4)?, b"worker output")?;
            Ok(vec![outcome(&source_plan.original_path)])
        })
        .unwrap_err();

        assert!(error.to_string().contains("trim output collision"));
        assert!(error.to_string().contains("match.dem.gz"));
        assert!(error.to_string().contains("match.dem.zst"));
        assert_eq!(worker_runs.load(Ordering::SeqCst), 0);
        assert!(!destination.exists());
    }
}
