//! Shared catalog/replay paths. Legacy CLI output remains readable; the app uses
//! one cache directory per registered source folder beneath its local ANOMALOUS root.
use std::{collections::HashMap, fs, path::{Path, PathBuf}, sync::{Mutex, OnceLock}};

pub fn folder_layout(root: &Path) -> bool {
    root.file_name().is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("ANOMALOUS"))
        || root.join(".s2dvr-folder-layout").is_file()
        || root.join(".anomalous-folder-layout").is_file()
}

pub fn safe_name(name: &str) -> String {
    let value: String = name.chars().map(|c| if c.is_control() || "<>:\"/\\|?*".contains(c) { '_' } else { c }).collect();
    let value = value.trim().trim_end_matches(['.', ' ']).to_lowercase();
    if value.is_empty() || value == "." || value == ".." { "source".into() } else {
        let stem = value.split('.').next().unwrap_or("");
        if ["con", "prn", "aux", "nul"].contains(&stem) ||
           (stem.len() == 4 && (stem.starts_with("com") || stem.starts_with("lpt")) && stem.as_bytes()[3].is_ascii_digit()) {
            format!("_{value}")
        } else { value }
    }
}

fn identity(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/").trim_end_matches('/').trim_start_matches("//?/").to_ascii_lowercase()
}

pub fn source_folder(root: &Path, source: &Path) -> String {
    let parent = source.parent().unwrap_or(source);
    source_directory(root, parent)
}

pub fn source_directory(root: &Path, directory: &Path) -> String {
    if !folder_layout(root) {
        return directory.file_name().map(|s| s.to_string_lossy().into_owned()).filter(|s| !s.is_empty()).unwrap_or_else(|| "unknown".into());
    }
    // The host finishes registrations before launching a parser process. Load
    // this small manifest map once per output root, not once per source demo.
    static REGISTRIES: OnceLock<Mutex<HashMap<PathBuf, HashMap<String, String>>>> = OnceLock::new();
    let mut registries = REGISTRIES.get_or_init(Default::default).lock().unwrap();
    let registry = registries.entry(root.to_path_buf()).or_insert_with(|| {
        let mut result = HashMap::new();
        if let Ok(entries) = fs::read_dir(root) {
            for entry in entries.flatten() {
                let folder = entry.path();
                if let Ok(bytes) = fs::read(folder.join("source.json")) {
                    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                        if let (Some(source), Some(key)) = (value["SourcePath"].as_str(), folder.file_name()) {
                            result.insert(identity(Path::new(source)), key.to_string_lossy().into_owned());
                        }
                    }
                }
            }
        }
        result
    });
    registry.get(&identity(directory)).cloned().unwrap_or_else(|| {
        // Standalone CLI inputs have no host registration. Include a stable path
        // suffix so unrelated same-name folders cannot overwrite each other.
        let key = identity(directory);
        let hash = key.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3));
        format!("{}-{:08x}", safe_name(&directory.file_name().unwrap_or_default().to_string_lossy()), hash as u32)
    })
}

pub fn database_path(root: &Path, kind: &str, folder: &str) -> PathBuf {
    if folder_layout(root) {
        let folder = partition_name(folder);
        root.join(&folder).join(format!("{}_{folder}.duckdb", kind.to_ascii_uppercase()))
    } else {
        root.join("KillCollectionMaster").join(format!("{}_{folder}.duckdb", kind.to_ascii_uppercase()))
    }
}

pub fn replay_path(root: &Path, kind: &str, folder: &str, demo: &str, _round: u32, collection: u32) -> PathBuf {
    if folder_layout(root) {
        root.join(partition_name(folder)).join(safe_name(kind)).join(format!("{}_{demo}_{collection}.s2r", kind.to_ascii_uppercase()))
    } else {
        root.join("TickByTick").join(folder).join(kind).join(format!("{kind}_{demo}_{collection}.s2r"))
    }
}

/// Collections of one type share full-round authority, independent of collection number.
pub fn round_replay_path(root: &Path, kind: &str, folder: &str, demo: &str, round: u32) -> PathBuf {
    if folder_layout(root) {
        root.join(partition_name(folder)).join(safe_name(kind)).join(format!("{demo}_r{round}.s2r"))
    } else {
        root.join("TickByTick").join(folder).join(demo).join(format!("_r{round}")).join("round.s2r")
    }
}

pub fn stored_replay_path(root: &Path, path: &Path, folder: &str) -> String {
    if folder_layout(root) {
        if let Ok(relative) = path.strip_prefix(root.join(partition_name(folder))) {
            return relative.to_string_lossy().replace('\\', "/");
        }
    }
    path.to_string_lossy().into_owned()
}

pub fn resolve_asset(root: &Path, database: &Path, stored: &str) -> PathBuf {
    let path = Path::new(stored);
    if path.is_absolute() {
        if let Some(cache) = cache_folder(database) {
            if let Ok(bytes) = fs::read(cache.join("source.json")) {
                if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                    if let Some(current) = value["SourcePath"].as_str() {
                        if let Some(previous) = value["PreviousPaths"].as_array() {
                            for old in previous.iter().filter_map(|v| v.as_str()) {
                                let old_normal = old.replace('\\', "/").trim_start_matches("//?/").trim_end_matches('/').to_string() + "/";
                                let stored_normal = stored.replace('\\', "/").trim_start_matches("//?/").to_string();
                                if stored_normal.to_ascii_lowercase().starts_with(&old_normal.to_ascii_lowercase()) {
                                    return Path::new(current).join(&stored_normal[old_normal.len()..]);
                                }
                            }
                        }
                    }
                }
            }
        }
        path.to_path_buf()
    }
    else if cache_folder(database).is_some() || stored.starts_with("../") || stored.starts_with("..\\") {
        database.parent().unwrap_or(root).join(path)
    } else { root.join(path) }
}

pub fn database_identity(path: &Path) -> Option<(String, String)> {
    let stem = path.file_stem()?.to_str()?;
    let parent = path.parent()?;
    if parent.parent().and_then(Path::file_name).is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("database")) {
        Some((parent.file_name()?.to_string_lossy().to_ascii_uppercase(), stem.strip_prefix('_')?.to_string()))
    } else {
        let (kind, folder) = stem.split_once('_')?;
        Some((kind.to_ascii_uppercase(), folder.to_string()))
    }
}

pub fn databases(root: &Path) -> Vec<PathBuf> {
    let mut result = Vec::new();
    if folder_layout(root) {
        if let Ok(folders) = fs::read_dir(root) {
            for folder in folders.flatten() {
                collect_databases(&folder.path(), &mut result);
            }
        }
    } else { collect_databases(&root.join("KillCollectionMaster"), &mut result); }
    result.sort(); result
}

fn cache_folder(database: &Path) -> Option<&Path> {
    let parent = database.parent()?;
    if parent.join("source.json").is_file() || parent.parent().is_some_and(folder_layout) {
        Some(parent)
    } else if parent.parent()?.file_name()?.eq_ignore_ascii_case("database") {
        parent.parent()?.parent()
    } else { None }
}

pub fn partition_name(name: &str) -> String {
    let safe = safe_name(name);
    for month in ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"] {
        if let Some(suffix) = safe.strip_prefix(&month.to_ascii_lowercase()) {
            if suffix.len() == 2 && suffix.bytes().all(|c| c.is_ascii_digit()) { return format!("{month}{suffix}"); }
        }
    }
    safe
}

fn collect_databases(directory: &Path, result: &mut Vec<PathBuf>) {
    if let Ok(entries) = fs::read_dir(directory) {
        result.extend(entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("duckdb"))));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_round_paths_keep_demo_and_round_separate() {
        let root = Path::new("C:/cache/ANOMALOUS");
        let round = round_replay_path(root, "ACE", "december25", "match", 17);
        assert_eq!(stored_replay_path(root, &round, "december25"), "ace/match_r17.s2r");
        assert_ne!(round, round_replay_path(root, "ACE", "december25", "match", 18));
        assert_ne!(round, round_replay_path(root, "ACE", "other", "match", 17));
    }
    #[test]
    fn relative_replays_and_partition_identity_survive_folder_rename() {
        let root = Path::new("C:/cache/ANOMALOUS");
        let path = replay_path(root, "ACE", "december25", "match.1", 17, 2);
        let stored = stored_replay_path(root, &path, "december25");
        assert_eq!(stored, "ace/ACE_match.1_2.s2r");
        let db = database_path(root, "ACE", "renamed");
        assert_eq!(database_identity(&db), Some(("ACE".into(), "renamed".into())));
        assert_eq!(resolve_asset(root, &db, &stored), db.parent().unwrap().join(stored));
        assert_ne!(path, replay_path(root, "QUAD", "december25", "match.1", 17, 3));
    }
    #[test]
    fn unregistered_same_name_sources_do_not_share_a_partition() {
        let root = Path::new("C:/unused-storage-test/ANOMALOUS");
        assert_ne!(source_directory(root, Path::new("C:/first/month")), source_directory(root, Path::new("D:/other/month")));
        assert_eq!(safe_name("CON"), "_con");
        assert_eq!(safe_name("December25"), "december25");
    }
    #[test]
    fn month_catalogs_are_flat_and_replay_types_are_separate() {
        let root = Path::new("C:/cache/ANOMALOUS");
        assert_eq!(database_path(root, "ACE", "april26"), root.join("April26/ACE_April26.duckdb"));
        assert_eq!(round_replay_path(root, "QUAD", "April26", "match", 17), root.join("April26/quad/match_r17.s2r"));
        assert_ne!(round_replay_path(root, "ACE", "April26", "match", 17), round_replay_path(root, "QUAD", "April26", "match", 17));
    }
    #[test]
    fn legacy_paths_are_preserved() {
        let root = Path::new("C:/output");
        assert_eq!(database_path(root, "ACE", "December25"), root.join("KillCollectionMaster/ACE_December25.duckdb"));
        assert_eq!(replay_path(root, "ACE", "December25", "demo", 17, 2), root.join("TickByTick/December25/ACE/ACE_demo_2.s2r"));
    }
}
