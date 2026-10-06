//! Resolving a `--select` collection filter against the demos actually being processed.
//!
//! The filter is keyed by the path the caller named. By the time a demo is parsed it may be at a
//! different path — an archive is decompressed to a working directory first — so a lookup has to
//! tolerate that without becoming a guess.
//!
//! The previous approach fell back to comparing file names inside an unordered map, which is
//! ambiguous exactly when it matters: two selected demos both called `match.dem` would each match
//! the other's entry, and which one won depended on hash order. Here an exact path always wins,
//! a unique file-name match is accepted, and an ambiguous one is refused — because parsing the
//! wrong collections is worse than parsing none.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The collections wanted from `demo_path`, or `None` when the filter does not cover it.
///
/// `None` means "not selected" and the caller should skip the demo; it never means "take
/// everything", which is what an ambiguous match silently produced before.
pub fn collections_for(filter: &HashMap<PathBuf, Vec<u32>>, demo_path: &str) -> Option<Vec<u32>> {
    // An exact path is unambiguous by definition.
    if let Some(collections) = filter
        .iter()
        .find(|(key, _)| key.to_string_lossy() == demo_path)
        .map(|(_, value)| value.clone())
    {
        return Some(collections);
    }

    // Compared on the stem — the file name with every extension removed — because a demo named
    // in the selection as `a.dem.gz` is parsed from `a.dem` after decompression. Comparing file
    // names cannot bridge that; comparing stems can, and a demo's stem is its GUID in practice.
    let wanted = stem(demo_path)?;

    // Every entry with the same stem. More than one and there is no way to tell which was meant,
    // so nothing is returned.
    let mut matches = filter
        .iter()
        .filter(|(key, _)| stem(&key.to_string_lossy()).as_deref() == Some(wanted.as_str()))
        .map(|(_, value)| value);

    let first = matches.next()?;
    if matches.next().is_some() {
        return None;
    }

    Some(first.clone())
}

/// Whether the filter covers this demo at all.
pub fn is_selected(filter: &HashMap<PathBuf, Vec<u32>>, demo_path: &str) -> bool {
    collections_for(filter, demo_path).is_some()
}

/// A demo's identity: its file name with the compression and `.dem` suffixes removed.
///
/// Only those are removed. Stripping every dotted suffix would reduce `match.one.dem.gz` and
/// `match.two.dem.gz` to the same `match`, making two unrelated demos falsely ambiguous — and an
/// ambiguous identity is refused, so that turns a valid selection into no output at all. Dots
/// inside the name itself are part of the name.
fn stem(path: &str) -> Option<String> {
    let name = Path::new(path).file_name()?.to_string_lossy().to_string();

    let without_archive = [".gz", ".zst", ".bz2", ".xz"]
        .iter()
        .find_map(|suffix| name.strip_suffix(*suffix))
        .unwrap_or(&name);

    let identity = without_archive
        .strip_suffix(".dem")
        .unwrap_or(without_archive);

    (!identity.is_empty()).then(|| identity.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter(entries: &[(&str, &[u32])]) -> HashMap<PathBuf, Vec<u32>> {
        entries
            .iter()
            .map(|(path, nums)| (PathBuf::from(path), nums.to_vec()))
            .collect()
    }

    #[test]
    fn an_exact_path_matches() {
        let f = filter(&[("C:/demos/a.dem", &[1, 2])]);
        assert_eq!(collections_for(&f, "C:/demos/a.dem"), Some(vec![1, 2]));
    }

    #[test]
    fn a_decompressed_copy_matches_by_name_when_unique() {
        let f = filter(&[("C:/demos/a.dem.gz", &[3])]);
        assert_eq!(collections_for(&f, "C:/tmp/unzip/a.dem.gz"), Some(vec![3]));
    }

    #[test]
    fn duplicate_file_names_are_refused_rather_than_guessed() {
        let f = filter(&[("C:/one/match.dem", &[1]), ("C:/two/match.dem", &[2])]);

        // Neither answer is defensible, so there is no answer.
        assert_eq!(collections_for(&f, "C:/tmp/match.dem"), None);
    }

    #[test]
    fn an_exact_path_still_wins_when_names_collide() {
        let f = filter(&[("C:/one/match.dem", &[1]), ("C:/two/match.dem", &[2])]);
        assert_eq!(collections_for(&f, "C:/two/match.dem"), Some(vec![2]));
    }

    #[test]
    fn a_decompressed_archive_matches_its_archived_name() {
        // Selected as an archive, parsed from the extracted demo.
        let f = filter(&[("D:/demos/1-abc.dem.gz", &[4, 9])]);
        assert_eq!(
            collections_for(&f, "C:/tmp/unzip/1-abc.dem"),
            Some(vec![4, 9])
        );
    }

    #[test]
    fn dots_inside_a_demo_name_are_kept() {
        let f = filter(&[
            ("D:/demos/match.one.dem.gz", &[1]),
            ("D:/demos/match.two.dem.gz", &[2]),
        ]);

        // Distinct demos, so neither is ambiguous and each keeps its own collections.
        assert_eq!(collections_for(&f, "C:/tmp/match.two.dem"), Some(vec![2]));
        assert_eq!(collections_for(&f, "C:/tmp/match.one.dem"), Some(vec![1]));
    }

    #[test]
    fn an_unselected_demo_is_not_covered() {
        let f = filter(&[("C:/demos/a.dem", &[1])]);
        assert!(!is_selected(&f, "C:/demos/b.dem"));
    }
}
