//! Utility functions for CLI interaction and file discovery
//!
//! Provides helper functions for user prompts, file discovery,
//! and argument parsing.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

static PUBLISHED_CLIP_STEMS: OnceLock<Mutex<HashMap<PathBuf, HashSet<String>>>> = OnceLock::new();

/// Prompt user to resume previous job (CLI mode)
#[allow(dead_code)]
pub fn prompt_resume_job() -> bool {
    print!("Previous job found. Resume? [Y/n]: ");
    io::stdout().flush().unwrap();

    let mut input = String::new();
    io::stdin().read_line(&mut input).unwrap();

    let input = input.trim().to_lowercase();
    input.is_empty() || input == "y" || input == "yes"
}

/// Parse command line arguments into a map and list of input paths
pub fn parse_arguments(args: &[String]) -> (HashMap<String, String>, Vec<PathBuf>) {
    let mut arg_map = HashMap::new();
    let mut input_paths = Vec::new();

    let mut i = 1;
    while i < args.len() {
        if args[i].starts_with("--") {
            let key = args[i].trim_start_matches("--").to_string();
            // Only value-taking options may consume a following positional path.
            // In particular --ndjson D:\\April25 must not silently drop April25.
            if matches!(key.as_str(), "padding" | "select" | "source-kind")
                && i + 1 < args.len() && !args[i + 1].starts_with("--") {
                arg_map.insert(key, args[i + 1].clone());
                i += 2;
            } else {
                arg_map.insert(key, "true".to_string());
                i += 1;
            }
        } else {
            // Positional argument (treat as a demo file or directory)
            input_paths.push(PathBuf::from(&args[i]));
            i += 1;
        }
    }

    (arg_map, input_paths)
}

/// Find all demo files from provided paths
pub fn find_demo_files(input_paths: &[PathBuf]) -> anyhow::Result<Vec<PathBuf>> {
    find_demo_files_excluding(input_paths, &HashSet::new())
}

/// Discover demos while treating DuckDB's complete DEM asset rows as authoritative derived-file
/// provenance. JSON remains only a compatibility fallback for clips made by older builds.
pub fn find_demo_files_with_catalog(
    input_paths: &[PathBuf],
    parser_output: &Path,
) -> anyhow::Result<Vec<PathBuf>> {
    // Explicit files are an opt-in escape hatch even for derived clips. They never consult
    // `derived` below, so avoid scanning an entire historical catalog for a selected reparse.
    if !input_paths.iter().any(|path| path.is_dir()) {
        return find_demo_files(input_paths);
    }
    let derived = catalogued_dem_assets(parser_output);
    find_demo_files_excluding(input_paths, &derived)
}

fn find_demo_files_excluding(
    input_paths: &[PathBuf],
    derived: &HashSet<String>,
) -> anyhow::Result<Vec<PathBuf>> {
    let mut entries = Vec::new();

    for path in input_paths {
        if !path.exists() {
            eprintln!("Warning: Path not found, skipping: {}", path.display());
            continue;
        }

        if path.is_dir() {
            let dir_entries = fs::read_dir(path)?
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.path())
                .filter(|p| {
                    matches!(
                        p.extension().and_then(|s| s.to_str()),
                        Some("dem") | Some("gz") | Some("zst")
                    )
                })
                // Round clips live beside their source by design. A matching DemoWriter report
                // is authoritative provenance that this is derived output, not a new source demo.
                // Explicitly passing the clip path still parses it when that is genuinely wanted.
                .filter(|p| {
                    !derived.contains(&path_identity(p))
                        && !has_source_sibling(p)
                        && !is_generated_round_clip(p)
                });
            entries.extend(dir_entries);
        } else if path.is_file() {
            if matches!(
                path.extension().and_then(|s| s.to_str()),
                Some("dem") | Some("gz") | Some("zst")
            ) {
                entries.push(path.to_path_buf());
            } else {
                eprintln!(
                    "Warning: Unsupported file type, skipping: {}",
                    path.display()
                );
            }
        }
    }

    Ok(entries)
}

fn catalogued_dem_assets(parser_output: &Path) -> HashSet<String> {
    let mut paths = HashSet::new();
    for database in config::storage::databases(parser_output)
    {
        let Ok(config) = duckdb::Config::default().access_mode(duckdb::AccessMode::ReadOnly) else {
            continue;
        };
        let Ok(conn) = duckdb::Connection::open_with_flags(&database, config) else {
            continue;
        };
        let Ok(mut stmt) = conn.prepare(
            "SELECT DISTINCT path FROM tick_assets
             WHERE format = 'DEM' AND status = 'complete' AND coalesce(format_version, 0) < 5 AND path IS NOT NULL",
        ) else {
            continue;
        };
        let Ok(rows) = stmt.query_map([], |row| row.get::<_, String>(0)) else {
            continue;
        };
        for stored in rows.flatten() {
            let resolved = config::storage::resolve_asset(parser_output, &database, &stored);
            paths.insert(path_identity(&resolved));
        }
    }
    paths
}

fn path_identity(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase()
}

fn is_generated_round_clip(path: &Path) -> bool {
    if !has_round_clip_name(path) {
        return false;
    }

    let report_path = path.with_extension("json");
    let Ok(report_text) = fs::read_to_string(&report_path) else {
        return false;
    };
    let Ok(report) = serde_json::from_str::<serde_json::Value>(&report_text) else {
        return false;
    };
    let Some(source) = report.get("source").and_then(|value| value.as_str()) else {
        return false;
    };
    let Some(output) = report.get("output").and_then(|value| value.as_str()) else {
        return false;
    };
    if source.is_empty() || output.is_empty() || report.get("checkpoint_tick").is_none() {
        return false;
    }

    let reported = PathBuf::from(output);
    let reported = if reported.is_absolute() {
        reported
    } else {
        report_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(reported)
    };

    paths_equal(path, &reported)
}

pub fn has_round_clip_name(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let lower = name.to_ascii_lowercase();
    let Some(stem) = lower.strip_suffix(".dem") else {
        return false;
    };
    let Some((_, round)) = stem.rsplit_once("_r") else {
        return false;
    };
    !round.is_empty() && round.bytes().all(|byte| byte.is_ascii_digit())
}

/// Integrated clips deliberately have no JSON report. While their source is retained, the
/// deterministic sibling name is enough to keep an interrupted pre-registration batch from being
/// re-imported as new source data. After source deletion, the DuckDB asset row is authoritative.
fn has_source_sibling(path: &Path) -> bool {
    if !has_round_clip_name(path) {
        return false;
    }
    let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
        return false;
    };
    let Some((source_stem, _)) = stem.rsplit_once("_r") else {
        return false;
    };
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    [".dem", ".dem.gz", ".dem.zst"]
        .iter()
        .any(|suffix| parent.join(format!("{source_stem}{suffix}")).is_file())
}

/// A clip beside a full source is already a published result. Integrated clips intentionally
/// have no JSON sidecar, so a source-mode job leaves that source for recovery/audit rather than
/// risking an overwrite of a previously published clip.
pub fn has_published_round_clip_sibling(path: &Path) -> bool {
    let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    let mut source_stem = file_name.to_ascii_lowercase();
    for suffix in [".gz", ".zst", ".dem"] {
        if source_stem.ends_with(suffix) {
            source_stem.truncate(source_stem.len() - suffix.len());
        }
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let cache = PUBLISHED_CLIP_STEMS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut cache = cache.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let stems = cache.entry(parent.to_path_buf()).or_insert_with(|| {
        fs::read_dir(parent)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                let stem = name.strip_suffix(".dem")?;
                let (source_stem, round) = stem.rsplit_once("_r")?;
                ( !source_stem.is_empty()
                    && !round.is_empty()
                    && round.bytes().all(|byte| byte.is_ascii_digit())
                )
                    .then(|| source_stem.to_string())
            })
            .collect()
    });
    stems.contains(&source_stem)
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left
            .to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy()),
    }
}

/// Print usage information
pub fn print_usage(program_name: &str) {
    eprintln!(
        "Usage: {} <demo_paths...> [--padding <number>]",
        program_name
    );
    eprintln!();
    eprintln!("Arguments:");
    eprintln!("  demo_paths...   One or more paths to demo files (.dem, .gz, .zst) or directories containing them.");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --padding <number>   Number of ticks to pad before and after the collection.");
    eprintln!(
        "  --trim-collections   Write verified per-round DEM clips and register them in DuckDB."
    );
    eprintln!("  --delete-source-after-trim");
    eprintln!(
        "                       Delete verified raw source .dem files after registration; archives and published round clips are kept."
    );
    eprintln!("  trim <demo> --round <n> --output <clip.dem>");
    eprintln!(
        "                       Write one round as a smaller playable demo (use trim --help)."
    );
    eprintln!();
    eprintln!("Master files will be created in the configured ParserOutput directory.");
    eprintln!();
    eprintln!("FACEIT downloads (built into this executable):");
    eprintln!("  fetch <match_ids_or_room_urls...> --mode download|trim|parse");
    eprintln!("  fetch --matches-file <queue.txt> --output <demo_folder> --mode parse");
    eprintln!("  fetch --help   See all download options; credentials come from a FACEIT INI beside the executable.");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boolean_flags_never_consume_input_folders() {
        let args = ["Demoparser", "--trim-collections", "--delete-source-after-trim", "--ndjson",
            "D:\\April25", "D:\\December24", "--padding", "all", "--select", "chosen.json"]
            .map(str::to_string);
        let (options, inputs) = parse_arguments(&args);
        assert_eq!(inputs, vec![PathBuf::from("D:\\April25"), PathBuf::from("D:\\December24")]);
        assert_eq!(options["padding"], "all");
        assert_eq!(options["select"], "chosen.json");
        assert_eq!(options["ndjson"], "true");
    }

    #[test]
    fn directory_discovery_skips_only_report_verified_round_clips() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("match.dem.gz");
        let clip = dir.path().join("match_r15.dem");
        let unverified = dir.path().join("other_r3.dem");
        fs::write(&source, b"source").unwrap();
        fs::write(&clip, b"clip").unwrap();
        fs::write(&unverified, b"ordinary demo").unwrap();
        fs::write(
            clip.with_extension("json"),
            serde_json::json!({
                "source": source,
                "output": clip,
                "round": 15,
                "checkpoint_tick": 100
            })
            .to_string(),
        )
        .unwrap();

        let found = find_demo_files(&[dir.path().to_path_buf()]).unwrap();
        assert!(found.contains(&source));
        assert!(found.contains(&unverified));
        assert!(!found.contains(&clip));

        // An explicit clip path remains an opt-in escape hatch for advanced workflows.
        assert_eq!(find_demo_files(&[clip.clone()]).unwrap(), vec![clip]);
    }

    #[test]
    fn mismatched_report_does_not_hide_a_demo() {
        let dir = tempfile::tempdir().unwrap();
        let clip = dir.path().join("match_r2.dem");
        fs::write(&clip, b"demo").unwrap();
        fs::write(
            clip.with_extension("json"),
            serde_json::json!({
                "source": "source.dem",
                "output": dir.path().join("something_else.dem"),
                "checkpoint_tick": 10
            })
            .to_string(),
        )
        .unwrap();

        assert_eq!(
            find_demo_files(&[dir.path().to_path_buf()]).unwrap(),
            vec![clip]
        );
    }

    #[test]
    fn source_sibling_hides_integrated_clip_without_json() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("match.dem.zst");
        let clip = dir.path().join("match_r4.dem");
        fs::write(&source, b"source").unwrap();
        fs::write(&clip, b"clip").unwrap();

        assert_eq!(
            find_demo_files(&[dir.path().to_path_buf()]).unwrap(),
            vec![source]
        );
    }

    #[test]
    fn duckdb_asset_hides_clip_after_source_deletion() {
        let dir = tempfile::tempdir().unwrap();
        let demos = dir.path().join("demos");
        let masters = dir.path().join("output").join("KillCollectionMaster");
        fs::create_dir_all(&demos).unwrap();
        fs::create_dir_all(&masters).unwrap();
        let clip = demos.join("match_r4.dem");
        fs::write(&clip, b"clip").unwrap();

        let conn = duckdb::Connection::open(masters.join("ACE_folder.duckdb")).unwrap();
        kill_collection_master::duckdb::schema::initialize_tables(&conn).unwrap();
        conn.execute(
            "INSERT INTO tick_assets (
                demo_name, collection_num, format, path, status
             ) VALUES ('match', 1, 'DEM', ?, 'complete')",
            duckdb::params![clip.to_string_lossy().to_string()],
        )
        .unwrap();
        drop(conn);

        assert!(
            find_demo_files_with_catalog(&[demos], &dir.path().join("output"))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            find_demo_files_with_catalog(&[clip.clone()], &dir.path().join("output")).unwrap(),
            vec![clip]
        );
    }
}
