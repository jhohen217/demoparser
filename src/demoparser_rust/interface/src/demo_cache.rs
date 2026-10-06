//! Thread-safe cache for decompressed demo files
//!
//! This module provides a global cache for storing decompressed demo data in memory,
//! allowing multiple parsing phases to reuse the same decompressed data without
//! redundant decompression operations.

use dashmap::DashMap;
use once_cell::sync::Lazy;
use std::sync::Arc;

/// Global cache for decompressed demo data
///
/// Uses DashMap for thread-safe concurrent access without locks.
/// The cache maps demo file paths to their decompressed data.
static DEMO_CACHE: Lazy<DemoCache> = Lazy::new(DemoCache::new);
static ROUND_EVENTS: Lazy<DashMap<String, Arc<Vec<parser::second_pass::game_events::GameEvent>>>> =
    Lazy::new(DashMap::new);

/// Share source-timeline round discovery with the verified trimmer. Only round events are
/// retained, not the large player dataframe. The batch boundary clears this for raw and
/// compressed inputs alike.
pub fn cache_round_events(path: String, events: &[parser::second_pass::game_events::GameEvent]) {
    ROUND_EVENTS.insert(
        path,
        Arc::new(
            events
                .iter()
                .filter(|event| {
                    matches!(
                        event.name.as_str(),
                        "round_start" | "round_freeze_end" | "round_end" | "round_officially_ended"
                    )
                })
                .cloned()
                .collect(),
        ),
    );
}

pub fn get_cached_round_events(
    path: &str,
) -> Option<Arc<Vec<parser::second_pass::game_events::GameEvent>>> {
    ROUND_EVENTS
        .get(path)
        .map(|entry| Arc::clone(entry.value()))
}

/// Cache entry containing decompressed demo data
#[derive(Clone)]
pub struct CacheEntry {
    /// The decompressed demo data
    pub data: Arc<Vec<u8>>,
    /// Original compressed file path
    pub original_path: String,
}

/// Thread-safe cache for decompressed demo files
pub struct DemoCache {
    cache: DashMap<String, CacheEntry>,
}

impl DemoCache {
    /// Create a new empty cache
    fn new() -> Self {
        DemoCache {
            cache: DashMap::new(),
        }
    }

    /// Insert decompressed demo data into the cache
    ///
    /// # Arguments
    /// * `path` - The file path (used as cache key)
    /// * `data` - The decompressed demo data
    pub fn insert(&self, path: String, data: Vec<u8>) -> Arc<Vec<u8>> {
        let arc_data = Arc::new(data);
        let entry = CacheEntry {
            data: Arc::clone(&arc_data),
            original_path: path.clone(),
        };

        self.cache.insert(path, entry);
        arc_data
    }

    /// Retrieve decompressed demo data from the cache
    ///
    /// # Arguments
    /// * `path` - The file path to look up
    ///
    /// # Returns
    /// * `Some(Arc<Vec<u8>>)` if found in cache
    /// * `None` if not in cache
    pub fn get(&self, path: &str) -> Option<Arc<Vec<u8>>> {
        self.cache.get(path).map(|entry| Arc::clone(&entry.data))
    }

    /// Check if a path exists in the cache
    pub fn contains(&self, path: &str) -> bool {
        self.cache.contains_key(path)
    }

    /// Remove an entry from the cache
    pub fn remove(&self, path: &str) -> Option<CacheEntry> {
        self.cache.remove(path).map(|(_, entry)| entry)
    }

    /// Clear all entries from the cache
    ///
    /// This is useful when switching between ram_unzip modes or
    /// when freeing memory after a batch processing job completes.
    pub fn clear(&self) {
        self.cache.clear();
    }

    /// Get the number of cached entries
    pub fn len(&self) -> usize {
        self.cache.len()
    }

    /// Check if the cache is empty
    pub fn is_empty(&self) -> bool {
        self.cache.is_empty()
    }

    /// Get the approximate memory usage of cached data in bytes
    pub fn memory_usage(&self) -> usize {
        self.cache
            .iter()
            .map(|entry| entry.value().data.len())
            .sum()
    }
}

/// Get a reference to the global demo cache
pub fn get_cache() -> &'static DemoCache {
    &DEMO_CACHE
}

/// Insert decompressed demo data into the global cache
///
/// # Arguments
/// * `path` - The file path (used as cache key)
/// * `data` - The decompressed demo data
///
/// # Returns
/// * `Arc<Vec<u8>>` - Arc-wrapped reference to the cached data
pub fn cache_demo(path: String, data: Vec<u8>) -> Arc<Vec<u8>> {
    get_cache().insert(path, data)
}

/// Retrieve decompressed demo data from the global cache
///
/// # Arguments
/// * `path` - The file path to look up
///
/// # Returns
/// * `Some(Arc<Vec<u8>>)` if found in cache
/// * `None` if not in cache
pub fn get_cached_demo(path: &str) -> Option<Arc<Vec<u8>>> {
    get_cache().get(path)
}

/// Clear the global demo cache
pub fn clear_cache() {
    get_cache().clear();
    ROUND_EVENTS.clear();
}

/// Get cache statistics
pub fn cache_stats() -> (usize, usize) {
    let cache = get_cache();
    (cache.len(), cache.memory_usage())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_insert_and_get() {
        let cache = DemoCache::new();
        let data = vec![1, 2, 3, 4, 5];
        let path = "test.dem".to_string();

        cache.insert(path.clone(), data.clone());

        let retrieved = cache.get(&path);
        assert!(retrieved.is_some());
        assert_eq!(*retrieved.unwrap(), data);
    }

    #[test]
    fn test_cache_contains() {
        let cache = DemoCache::new();
        let path = "test.dem".to_string();

        assert!(!cache.contains(&path));
        cache.insert(path.clone(), vec![1, 2, 3]);
        assert!(cache.contains(&path));
    }

    #[test]
    fn test_cache_remove() {
        let cache = DemoCache::new();
        let path = "test.dem".to_string();

        cache.insert(path.clone(), vec![1, 2, 3]);
        assert!(cache.contains(&path));

        cache.remove(&path);
        assert!(!cache.contains(&path));
    }

    #[test]
    fn test_cache_clear() {
        let cache = DemoCache::new();

        cache.insert("test1.dem".to_string(), vec![1, 2, 3]);
        cache.insert("test2.dem".to_string(), vec![4, 5, 6]);

        assert_eq!(cache.len(), 2);
        cache.clear();
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn test_memory_usage() {
        let cache = DemoCache::new();

        cache.insert("test1.dem".to_string(), vec![1, 2, 3]);
        cache.insert("test2.dem".to_string(), vec![4, 5, 6, 7]);

        assert_eq!(cache.memory_usage(), 7); // 3 + 4 bytes
    }
}
