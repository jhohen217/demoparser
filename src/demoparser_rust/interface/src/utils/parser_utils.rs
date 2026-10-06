//! Parser utils module for the interface crate
//!
//! This module contains utility functions for parsing demo files.

use std::path::Path;

/// Stable catalogue identity for one demo, independent of its transport container.
///
/// Only the known transport suffixes are removed. Dots inside the match name remain part of the
/// identity, and ASCII case is folded so moving the same source between `.dem`, `.dem.gz`, and
/// `.dem.zst` cannot create parallel catalogue rows.
pub fn canonical_demo_name(value: &str) -> String {
    let file_name = Path::new(value)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(value);
    let lower = file_name.to_ascii_lowercase();
    let without_archive = [".zst", ".gz"]
        .iter()
        .find_map(|suffix| lower.strip_suffix(*suffix))
        .unwrap_or(&lower);
    without_archive
        .strip_suffix(".dem")
        .unwrap_or(without_archive)
        .to_string()
}

/// Get the demo name from a demo path
pub fn get_demo_name(demo_path: &str) -> String {
    let identity = canonical_demo_name(demo_path);
    if identity.is_empty() {
        "unknown".to_string()
    } else {
        identity
    }
}

/// Get the output path for a demo file
pub fn get_output_path(demo_path: &str, suffix: &str, extension: &str) -> String {
    let path = Path::new(demo_path);

    if let Some(parent) = path.parent() {
        if let Some(file_stem) = path.file_stem() {
            if let Some(file_stem_str) = file_stem.to_str() {
                if let Some(parent_str) = parent.to_str() {
                    return format!("{}/{}{}.{}", parent_str, file_stem_str, suffix, extension);
                }
            }
        }
    }

    format!("{}{}.{}", demo_path, suffix, extension)
}

/// Get the folder from a demo path
pub fn get_folder_from_demo_path(demo_path: &str) -> String {
    // Extract the month from the parent folder name
    let path = std::path::Path::new(demo_path);

    // Get the parent directory
    if let Some(parent) = path.parent() {
        // Get the directory name
        if let Some(dir_name) = parent.file_name() {
            if let Some(dir_str) = dir_name.to_str() {
                return dir_str.to_string();
            }
        }
    }

    // Default value if we can't extract the month
    "test".to_string()
}

/// Check if a file is a demo file
pub fn is_demo_file(file_path: &str) -> bool {
    let path = Path::new(file_path);

    if let Some(extension) = path.extension() {
        if let Some(extension_str) = extension.to_str() {
            return extension_str.to_lowercase() == "dem";
        }
    }

    false
}

// The following functions with mock implementations have been removed to prevent dummy data:
// - get_map_name_from_demo_path
// - get_tick_rate_from_demo
// - get_playback_ticks_from_demo
// - get_playback_time_from_demo
// - get_demo_protocol_from_demo
// - get_network_protocol_from_demo
// - get_server_name_from_demo
// - get_client_name_from_demo
// - get_game_directory_from_demo
//
// These functions should be implemented with real demo parsing logic when needed.
// Any code relying on these functions will need to be updated to handle the missing implementations.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_identity_is_transport_independent() {
        let expected = "match.one";
        assert_eq!(canonical_demo_name(r"D:\demos\Match.One.dem"), expected);
        assert_eq!(canonical_demo_name(r"D:\demos\Match.One.dem.gz"), expected);
        assert_eq!(canonical_demo_name(r"D:\demos\Match.One.dem.zst"), expected);
        assert_eq!(canonical_demo_name("match.one"), expected);
    }
}
