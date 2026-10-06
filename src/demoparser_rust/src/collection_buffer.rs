//! Collection buffer for RAM-based batch processing
//!
//! This module provides a thread-safe in-memory buffer for storing KillCollections
//! during batch processing. Collections are stored in RAM, updated with tickbytick
//! data, and then written to DuckDB in a single transaction at batch completion.

use dashmap::DashMap;
use interface::models::collection::KillCollection;
use interface::models::tick_asset::TickAsset;
use std::collections::HashMap;

/// Type alias for collection key: (collection_type, folder, demo_name, collection_num)
///
/// Keyed on `collection_num`, not `round`. Collections are grouped per (killer, round),
/// so two players can each produce a collection of the same type in the same round - under
/// a round-based key those collide and one is silently dropped on insert. `collection_num`
/// is assigned sequentially across a demo and is unique within it.
///
/// `demo_name` is normalised (see [`normalize_demo_name`]) so callers holding either
/// "foo.dem" or "foo" build the same key.
pub type CollectionKey = (String, String, String, u32);

/// Strip demo extensions so a key is stable regardless of which form the caller holds.
fn normalize_demo_name(demo_name: &str) -> String {
    demo_name
        .trim_end_matches(".gz")
        .trim_end_matches(".zst")
        .trim_end_matches(".dem")
        .to_string()
}

/// Thread-safe buffer for storing collections during batch processing
pub struct CollectionBuffer {
    /// Key: (collection_type, folder, demo_name, collection_num)
    /// Value: BufferedCollection with tickbytick completion status
    collections: DashMap<CollectionKey, BufferedCollection>,
    /// Tick-data assets written during this batch, keyed the same way as collections so a
    /// re-run of one collection replaces its own rows and nothing else.
    assets: DashMap<CollectionKey, Vec<TickAsset>>,
}

/// A buffered collection with metadata about tickbytick processing
#[derive(Clone)]
pub struct BufferedCollection {
    /// The actual kill collection data
    pub collection: KillCollection,
    /// Whether tickbytick data has been successfully added
    pub tickbytick_complete: bool,
}

impl CollectionBuffer {
    /// Create a new empty collection buffer
    pub fn new() -> Self {
        Self {
            collections: DashMap::new(),
            assets: DashMap::new(),
        }
    }

    /// Record the tick-data assets written for one collection, replacing any previously
    /// recorded for it.
    pub fn record_assets(&self, assets: Vec<TickAsset>) {
        let Some(first) = assets.first() else {
            return;
        };
        let key = (
            first.collection_type.clone(),
            first.folder.clone(),
            normalize_demo_name(&first.demo_name),
            first.collection_num as u32,
        );
        self.assets.insert(key, assets);
    }

    /// Merge verified DEM assets after replay processing, preserving the S2R row for the
    /// same collection. Each (collection, format) still has exactly one current asset.
    pub fn merge_assets(&self, assets: impl IntoIterator<Item = TickAsset>) {
        for asset in assets {
            let key = (
                asset.collection_type.clone(),
                asset.folder.clone(),
                normalize_demo_name(&asset.demo_name),
                asset.collection_num as u32,
            );
            let mut entry = self.assets.entry(key).or_default();
            entry.retain(|existing| existing.format != asset.format);
            entry.push(asset);
        }
    }

    /// Verify all requested replay references before catalog commit and source cleanup.
    pub fn validate_required_replays(&self, required: impl Fn(&str) -> bool) -> anyhow::Result<()> {
        use interface::models::tick_asset::{AssetFormat, AssetStatus};
        for entry in self.collections.iter() {
            if !required(&entry.collection.collection_type) { continue; }
            let complete = self.assets.get(entry.key()).is_some_and(|assets| {
                assets.iter().any(|asset| asset.format == AssetFormat::S2r
                    && asset.status == AssetStatus::Complete
                    && asset.size_bytes > 0 && !asset.checksum.is_empty())
            });
            if !complete {
                anyhow::bail!("required replay is missing or unverified for {:?}; source cleanup is prohibited", entry.key());
            }
        }
        Ok(())
    }

    /// All recorded assets grouped by (collection_type, folder), matching the grouping used
    /// to write collections so both can be flushed to the same database file.
    pub fn get_grouped_assets(&self) -> HashMap<(String, String), Vec<TickAsset>> {
        let mut grouped: HashMap<(String, String), Vec<TickAsset>> = HashMap::new();
        for entry in self.assets.iter() {
            let (collection_type, folder, _demo, _num) = entry.key();
            grouped
                .entry((collection_type.clone(), folder.clone()))
                .or_default()
                .extend(entry.value().iter().cloned());
        }
        grouped
    }

    /// Add a collection to the buffer
    /// If a collection with the same key exists, it will be replaced
    pub fn add_collection(&self, collection: KillCollection) {
        let key = Self::make_key(&collection);
        let buffered = BufferedCollection {
            collection,
            tickbytick_complete: false,
        };
        self.collections.insert(key, buffered);
    }

    /// Add multiple collections to the buffer
    pub fn add_collections(&self, collections: Vec<KillCollection>) {
        for collection in collections {
            self.add_collection(collection);
        }
    }

    /// Update a collection with tickbytick data
    /// Returns true if the collection was found and updated, false otherwise
    #[allow(clippy::too_many_arguments)]
    pub fn update_with_tickbytick(
        &self,
        collection_type: &str,
        folder: &str,
        demo_name: &str,
        collection_num: u32,
        util_thrown: &str,
        traj_mode: u8,
        hits: u32,
        misses: u32,
        hit_rate: f32,
        weapons_formatted: &str,
    ) -> bool {
        let key = (
            collection_type.to_string(),
            folder.to_string(),
            normalize_demo_name(demo_name),
            collection_num,
        );

        match self.collections.get_mut(&key) {
            Some(mut entry) => {
                let buffered = entry.value_mut();
                buffered.collection.parsed = 1; // TickData
                buffered.collection.util_thrown = util_thrown.to_string();
                buffered.collection.hits = hits;
                buffered.collection.misses = misses;
                buffered.collection.hit_rate = hit_rate;
                buffered.collection.grenade_traj = traj_mode as u32;
                buffered.collection.weapons_damaged_hits_formatted = weapons_formatted.to_string();
                buffered.tickbytick_complete = true;
                true
            }
            None => false,
        }
    }

    /// Get all collections grouped by (collection_type, folder)
    /// Returns a HashMap mapping (type, folder) to a vector of collections
    pub fn get_grouped_collections(&self) -> HashMap<(String, String), Vec<KillCollection>> {
        let mut grouped: HashMap<(String, String), Vec<KillCollection>> = HashMap::new();

        for entry in self.collections.iter() {
            let (collection_type, folder, _demo_name, _collection_num) = entry.key();
            let buffered = entry.value();

            let key = (collection_type.clone(), folder.clone());
            grouped
                .entry(key)
                .or_insert_with(Vec::new)
                .push(buffered.collection.clone());
        }

        grouped
    }

    /// Get the total number of collections in the buffer
    pub fn len(&self) -> usize {
        self.collections.len()
    }

    /// Check if the buffer is empty
    pub fn is_empty(&self) -> bool {
        self.collections.is_empty()
    }

    /// Clear all collections from the buffer
    pub fn clear(&self) {
        self.collections.clear();
        self.assets.clear();
    }

    /// Get statistics about tickbytick completion
    pub fn get_completion_stats(&self) -> (usize, usize) {
        let total = self.collections.len();
        let completed = self
            .collections
            .iter()
            .filter(|entry| entry.value().tickbytick_complete)
            .count();
        (completed, total)
    }

    /// Helper function to create a collection key
    fn make_key(collection: &KillCollection) -> CollectionKey {
        (
            collection.collection_type.clone(),
            collection.folder.clone(),
            normalize_demo_name(&collection.demo_name),
            collection.collection_num as u32,
        )
    }
}

impl Default for CollectionBuffer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use interface::models::tick_asset::AssetFormat;

    fn create_test_collection(demo_name: &str, round: i32, collection_num: i32) -> KillCollection {
        KillCollection {
            collection_type: "ACE".to_string(),
            folder: "test".to_string(),
            demo_name: demo_name.to_string(),
            round,
            collection_num,
            parsed: 0,
            util_thrown: String::new(),
            hits: 0,
            misses: 0,
            hit_rate: 0.0,
            ..Default::default()
        }
    }

    #[test]
    fn test_add_and_retrieve() {
        let buffer = CollectionBuffer::new();
        let collection = create_test_collection("demo1.dem", 5, 1);

        buffer.add_collection(collection.clone());
        assert_eq!(buffer.len(), 1);

        let grouped = buffer.get_grouped_collections();
        assert_eq!(grouped.len(), 1);
    }

    #[test]
    fn test_update_with_tickbytick() {
        let buffer = CollectionBuffer::new();
        let collection = create_test_collection("demo1.dem", 5, 1);

        buffer.add_collection(collection);

        let updated = buffer.update_with_tickbytick(
            "ACE",
            "test",
            "demo1",
            1,
            "[util]",
            1,
            10,
            2,
            0.833,
            "[ak47(5)]",
        );

        assert!(updated);

        let (completed, total) = buffer.get_completion_stats();
        assert_eq!(completed, 1);
        assert_eq!(total, 1);
    }

    /// `traj_mode` and `weapons_formatted` were accepted and then thrown away, which is why
    /// every row in the database reported GrenadeTraj = 0 regardless of configuration.
    #[test]
    fn tickbytick_update_persists_traj_mode_and_weapons() {
        let buffer = CollectionBuffer::new();
        buffer.add_collection(create_test_collection("demo1.dem", 5, 1));

        assert!(buffer.update_with_tickbytick(
            "ACE",
            "test",
            "demo1",
            1,
            "[util]",
            2,
            10,
            2,
            0.833,
            "[ak47(5) - glock(2)]",
        ));

        let stored = buffer
            .get_grouped_collections()
            .remove(&("ACE".to_string(), "test".to_string()))
            .unwrap()
            .remove(0);

        assert_eq!(stored.grenade_traj, 2);
        assert_eq!(
            stored.weapons_damaged_hits_formatted,
            "[ak47(5) - glock(2)]"
        );
        assert_eq!(stored.util_thrown, "[util]");
        assert_eq!(stored.parsed, 1);
    }

    /// Two players can each produce a collection of the same type in the same round.
    /// The key used to be (type, folder, demo, round), so the second insert evicted the
    /// first and one collection vanished from the batch.
    #[test]
    fn same_type_and_round_for_different_killers_do_not_collide() {
        let buffer = CollectionBuffer::new();

        let mut first = create_test_collection("demo1.dem", 5, 1);
        first.killer_steamid = "76561198000000001".to_string();
        let mut second = create_test_collection("demo1.dem", 5, 2);
        second.killer_steamid = "76561198000000002".to_string();

        buffer.add_collection(first);
        buffer.add_collection(second);

        assert_eq!(buffer.len(), 2, "both collections must survive");
    }

    /// The key normalises the demo name, so callers holding "foo.dem" and callers holding
    /// "foo" address the same entry.
    #[test]
    fn demo_name_extension_does_not_affect_lookup() {
        let buffer = CollectionBuffer::new();
        buffer.add_collection(create_test_collection("demo1.dem", 5, 1));

        assert!(buffer.update_with_tickbytick("ACE", "test", "demo1.dem", 1, "", 0, 0, 0, 0.0, "",));
    }

    fn test_asset(collection_num: i32, format: AssetFormat) -> TickAsset {
        TickAsset {
            demo_name: "demo1.dem".to_string(),
            collection_num,
            collection_type: "ACE".to_string(),
            folder: "test".to_string(),
            format,
            format_version: 5,
            path: "out.s2r".to_string(),
            size_bytes: 128,
            checksum: "0123456789abcdef".to_string(),
            status: interface::models::tick_asset::AssetStatus::Complete,
            grenade_traj: 1,
            authority_bytes: 0,
            agent_life_count: 0,
            weapon_lifetime_count: 0,
            inventory_delta_count: 0,
            world_weapon_delta_count: 0,
            checkpoint_tick: None,
            logical_start_tick: None,
            logical_end_tick: None,
            source_path: None,
            source_bytes: None,
        }
    }

    #[test]
    fn required_replays_reject_missing_failed_empty_and_wrong_collection_assets() {
        use interface::models::tick_asset::AssetStatus;
        let buffer = CollectionBuffer::new();
        buffer.add_collection(create_test_collection("demo1.dem", 5, 1));
        assert!(buffer.validate_required_replays(|_| true).is_err());
        assert!(buffer.validate_required_replays(|_| false).is_ok());
        buffer.record_assets(vec![test_asset(2, AssetFormat::S2r)]);
        assert!(buffer.validate_required_replays(|_| true).is_err());
        let mut asset = test_asset(1, AssetFormat::S2r);
        asset.status = AssetStatus::Failed;
        buffer.record_assets(vec![asset.clone()]);
        assert!(buffer.validate_required_replays(|_| true).is_err());
        asset.status = AssetStatus::Complete;
        asset.size_bytes = 0;
        buffer.record_assets(vec![asset]);
        assert!(buffer.validate_required_replays(|_| true).is_err());
        buffer.record_assets(vec![test_asset(1, AssetFormat::S2r)]);
        assert!(buffer.validate_required_replays(|_| true).is_ok());
    }

    #[test]
    fn assets_group_alongside_their_collections() {
        let buffer = CollectionBuffer::new();
        buffer.record_assets(vec![
            test_asset(1, AssetFormat::Npz),
            test_asset(1, AssetFormat::S2r),
        ]);
        buffer.record_assets(vec![test_asset(2, AssetFormat::S2r)]);

        let grouped = buffer.get_grouped_assets();
        let group = grouped
            .get(&("ACE".to_string(), "test".to_string()))
            .expect("assets group by type and folder like collections do");
        assert_eq!(group.len(), 3);
    }

    /// Re-running one collection replaces its own asset rows and leaves others alone.
    #[test]
    fn re_recording_a_collection_replaces_only_its_own_assets() {
        let buffer = CollectionBuffer::new();
        buffer.record_assets(vec![
            test_asset(1, AssetFormat::Npz),
            test_asset(1, AssetFormat::S2r),
        ]);
        buffer.record_assets(vec![test_asset(2, AssetFormat::S2r)]);

        // collection 1 re-run, this time producing only S2R
        buffer.record_assets(vec![test_asset(1, AssetFormat::S2r)]);

        let grouped = buffer.get_grouped_assets();
        let group = &grouped[&("ACE".to_string(), "test".to_string())];
        assert_eq!(
            group.len(),
            2,
            "1's stale NPZ row must be gone, 2 untouched"
        );
        assert!(group.iter().all(|a| a.format == AssetFormat::S2r));
    }

    #[test]
    fn clearing_the_buffer_also_clears_assets() {
        let buffer = CollectionBuffer::new();
        buffer.add_collection(create_test_collection("demo1.dem", 5, 1));
        buffer.record_assets(vec![test_asset(1, AssetFormat::S2r)]);

        buffer.clear();

        assert_eq!(buffer.len(), 0);
        assert!(buffer.get_grouped_assets().is_empty());
    }

    #[test]
    fn test_clear() {
        let buffer = CollectionBuffer::new();
        buffer.add_collection(create_test_collection("demo1.dem", 5, 1));
        buffer.add_collection(create_test_collection("demo2.dem", 3, 2));

        assert_eq!(buffer.len(), 2);
        buffer.clear();
        assert_eq!(buffer.len(), 0);
    }
}
