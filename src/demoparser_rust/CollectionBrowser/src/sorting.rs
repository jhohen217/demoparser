use crate::models::CollectionEntry;

/// Column that can be sorted
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortColumn {
    Type,
    Frag,
    Player,
    Map,
    Duration,
    KillerRadius,
    VictimRadius,
    Moved,
    GameVersion,
    Hits,
    Misses,
    HitRate,
    Weapons,
    Util,
    Tag,
}

/// Sort order
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortOrder {
    Ascending,
    Descending,
}

impl SortOrder {
    pub fn toggle(&self) -> Self {
        match self {
            Self::Ascending => Self::Descending,
            Self::Descending => Self::Ascending,
        }
    }
}

/// Sort collections based on current sort settings
pub fn sort_collections(
    collections: &mut [CollectionEntry],
    column: Option<SortColumn>,
    order: SortOrder,
) {
    if let Some(column) = column {
        collections.sort_by(|a, b| {
            let cmp = match column {
                SortColumn::Type => a.collection_type.cmp(&b.collection_type),
                SortColumn::Frag => a.collection_num.cmp(&b.collection_num),
                SortColumn::Player => a
                    .killer_name
                    .to_lowercase()
                    .cmp(&b.killer_name.to_lowercase()),
                SortColumn::Map => a.map_name.cmp(&b.map_name),
                SortColumn::Duration => a.tick_duration.cmp(&b.tick_duration),
                SortColumn::KillerRadius => a
                    .killer_radius
                    .partial_cmp(&b.killer_radius)
                    .unwrap_or(std::cmp::Ordering::Equal),
                SortColumn::VictimRadius => a
                    .victims_radius
                    .partial_cmp(&b.victims_radius)
                    .unwrap_or(std::cmp::Ordering::Equal),
                SortColumn::Moved => a
                    .killer_move_distance
                    .partial_cmp(&b.killer_move_distance)
                    .unwrap_or(std::cmp::Ordering::Equal),
                SortColumn::GameVersion => a.game_version.cmp(&b.game_version),
                SortColumn::Hits => a.hits.cmp(&b.hits),
                SortColumn::Misses => a.misses.cmp(&b.misses),
                SortColumn::HitRate => a
                    .hit_rate
                    .partial_cmp(&b.hit_rate)
                    .unwrap_or(std::cmp::Ordering::Equal),
                SortColumn::Weapons => a.display_weapons().cmp(b.display_weapons()),
                SortColumn::Util => a.util_thrown.cmp(&b.util_thrown),
                SortColumn::Tag => a.tag.to_lowercase().cmp(&b.tag.to_lowercase()),
            };

            match order {
                SortOrder::Ascending => cmp,
                SortOrder::Descending => cmp.reverse(),
            }
        });
    }
}
