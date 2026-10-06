//! Tick-data asset records.
//!
//! A collection's heavy tick stream lives on disk as an NPZ and/or an S2R file, not in the
//! database. Previously the only trace of that in DuckDB was a single `TickData` integer on
//! the collection row, which could not express *which* formats exist, what version they were
//! written at, or whether the file on disk is still the one that was written. That is how a
//! database ended up reporting 55 collections complete with only 3 NPZ files present.
//!
//! One row per (collection, format) records enough to answer those questions without
//! reading the demo again.

use serde::{Deserialize, Serialize};

/// Output format of a tick-data asset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AssetFormat {
    Npz,
    S2r,
    /// A round-sized `.dem` trimmed out of the source demo.
    ///
    /// Unlike NPZ and S2R, this file is inherently per *round* rather than per collection, so
    /// several collections from one round all point at a single physical clip and each still
    /// gets its own row. The duplication is deliberate: the databases are split one per
    /// collection type, so no table reachable from a collection row can hold one shared
    /// record. `path` is what identifies the physical file.
    Dem,
}

impl AssetFormat {
    pub fn as_str(&self) -> &'static str {
        match self {
            AssetFormat::Npz => "NPZ",
            AssetFormat::S2r => "S2R",
            AssetFormat::Dem => "DEM",
        }
    }
}

/// Whether the asset on disk is usable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AssetStatus {
    /// Written and verified present at the recorded size.
    Complete,
    /// Generation was attempted and failed; the file is absent or unusable.
    Failed,
}

impl AssetStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            AssetStatus::Complete => "complete",
            AssetStatus::Failed => "failed",
        }
    }
}

/// A single tick-data file belonging to one collection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TickAsset {
    pub demo_name: String,
    pub collection_num: i32,
    pub collection_type: String,
    pub folder: String,
    pub format: AssetFormat,
    /// Writer format revision (e.g. S2R_VERSION), so a stale asset is detectable in SQL
    /// rather than only by reading the file header.
    pub format_version: i32,
    pub path: String,
    pub size_bytes: i64,
    /// FNV-1a 64 over the file contents. Cheap, dependency-free, and sufficient to catch
    /// truncation or a file being replaced out from under the database.
    pub checksum: String,
    pub status: AssetStatus,
    /// Grenade trajectory mode the asset was produced with, so assets generated before a
    /// config change are distinguishable.
    pub grenade_traj: i32,
    /// Size of the additive S2EX authority block. Zero for NPZ and pre-v9 S2R assets.
    #[serde(default)]
    pub authority_bytes: i64,
    #[serde(default)]
    pub agent_life_count: i32,
    #[serde(default)]
    pub weapon_lifetime_count: i32,
    #[serde(default)]
    pub inventory_delta_count: i32,
    #[serde(default)]
    pub world_weapon_delta_count: i32,
    /// Legacy DEM source checkpoint. Null for clip-local DEM registrations (v5+).
    #[serde(default)]
    pub checkpoint_tick: Option<i32>,
    /// Playable bounds: clip-local for DEM registrations v5+, original timeline for legacy rows.
    #[serde(default)]
    pub logical_start_tick: Option<i32>,
    #[serde(default)]
    pub logical_end_tick: Option<i32>,
    /// Physical source container used to create the clip. Its transport suffix is provenance,
    /// not part of `demo_name` identity. DEM-only and allowed to point at a retained/missing file.
    #[serde(default)]
    pub source_path: Option<String>,
    #[serde(default)]
    pub source_bytes: Option<i64>,
}

/// FNV-1a 64-bit over a byte slice.
pub fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Hash a file on disk, returning its size and checksum.
pub fn checksum_file(path: &std::path::Path) -> std::io::Result<(i64, String)> {
    let bytes = std::fs::read(path)?;
    let size = bytes.len() as i64;
    Ok((size, format!("{:016x}", fnv1a_64(&bytes))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv1a_matches_known_vectors() {
        // Reference values for FNV-1a 64.
        assert_eq!(fnv1a_64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a_64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a_64(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn different_content_hashes_differently() {
        assert_ne!(fnv1a_64(b"payload"), fnv1a_64(b"payloae"));
        // Truncation, the case this is meant to catch.
        assert_ne!(fnv1a_64(b"payload"), fnv1a_64(b"paylo"));
    }

    #[test]
    fn checksum_file_reports_size_and_hash() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.bin");
        std::fs::write(&path, b"foobar").unwrap();

        let (size, checksum) = checksum_file(&path).unwrap();
        assert_eq!(size, 6);
        assert_eq!(checksum, format!("{:016x}", fnv1a_64(b"foobar")));
    }

    #[test]
    fn format_and_status_render_as_stable_strings() {
        assert_eq!(AssetFormat::Npz.as_str(), "NPZ");
        assert_eq!(AssetFormat::S2r.as_str(), "S2R");
        assert_eq!(AssetFormat::Dem.as_str(), "DEM");
        assert_eq!(AssetStatus::Complete.as_str(), "complete");
        assert_eq!(AssetStatus::Failed.as_str(), "failed");
    }
}
