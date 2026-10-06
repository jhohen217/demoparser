use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct CliArgs {
    pub track_all_players: Option<bool>,
    pub padding: Option<String>,
    pub collection_nums: Option<Vec<u32>>,
    pub collection_types: CollectionTypeFilter,
    pub output_dir: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct CollectionTypeFilter {
    pub ace: Option<bool>,
    pub quad: Option<bool>,
    pub triple: Option<bool>,
    pub multi: Option<bool>,
    pub double: Option<bool>,
    pub single: Option<bool>,
    pub all_types: bool,
}

impl Default for CollectionTypeFilter {
    fn default() -> Self {
        Self {
            ace: None,
            quad: None,
            triple: None,
            multi: None,
            double: None,
            single: None,
            all_types: false,
        }
    }
}
