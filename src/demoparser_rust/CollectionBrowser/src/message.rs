use crate::database::DatabaseInfo;
use crate::models::{CollectionEntry, CollectionType, TickFilterState};
use crate::sorting::SortColumn;
use demoparser::ProgressEvent;
use iced::Event;

/// Application messages
#[derive(Debug, Clone)]
pub enum Message {
    Fetch(crate::fetch::FetchMessage),
    // Parser output management
    BrowseParserOutput,
    ParserOutputSelected(Option<std::path::PathBuf>),
    ParserOutputChanged(String),

    // Unzip directory management
    BrowseUnzipDir,
    UnzipDirSelected(Option<std::path::PathBuf>),
    UnzipDirChanged(String),
    ToggleRamUnzip(bool),

    // Directory management
    AddDemoDirectory,
    DirectorySelected(Option<std::path::PathBuf>),
    RemoveDemoDirectory,
    SelectDirectoryRow(usize),
    ToggleDirectoryEnabled(usize, bool),
    OpenDirectoryInExplorer,
    DirectoryScanned(usize, Result<(usize, usize, usize), String>),
    AllDirectoryStatsCounted(Result<Vec<(String, usize, usize, usize)>, String>),

    // Database management
    RefreshDatabases,
    DatabasesScanned(Result<Vec<DatabaseInfo>, String>),
    CollectionsLoaded(Result<Vec<CollectionEntry>, String>, bool),
    ToggleFilter(CollectionType, bool),
    ToggleTickDataFilter(TickFilterState),
    Sort(SortColumn),

    // Selection management
    ToggleSelectAll(bool),
    ToggleSelect(usize, bool),
    SelectRow(usize),
    NavigateUp,
    NavigateDown,
    NavigateLeft,
    NavigateRight,
    DeselectAllCollections,
    DeselectAllDirectories,
    DetailsLoaded(usize, Result<CollectionEntry, String>),

    // Tag management
    TagInputChanged(String),
    ApplyTag,
    ClearTag,
    ConfirmTagAction,
    CancelTagAction,
    TagsUpdated(Result<(), String>),

    // Search filter
    SearchInputChanged(String),

    // Demo name filter
    ToggleDemoNameFilter,

    // Numerical range filter
    NumFilterMinChanged(String),
    NumFilterMaxChanged(String),
    NumFilterColumnChanged(String),
    NumFilterExcludeToggled(bool),

    // Export and validation
    ExportSelections,

    // Parser configuration
    ToggleParserAces(bool),
    ToggleParserQuads(bool),
    ToggleParserTriples(bool),
    ToggleParserMulti(bool),
    ToggleParserSingles(bool),
    ToggleParserDoubles(bool),
    ToggleParserGrenadeTrajectory(bool),
    ToggleParserOverwrite(bool),
    ToggleOutputNpz(bool),
    ToggleOutputS2r(bool),
    ParserThreadsInputChanged(String),
    ParserConfigSaved(Result<(), String>),

    // Hover state
    ControlHovered(String),
    ControlUnhovered,

    // Separator drag
    SeparatorPressed,
    SeparatorHovered,
    SeparatorUnhovered,

    // Parser execution
    ParseDirectory,
    ParseCollections,
    CancelParsing,
    ParsingEvent(ProgressEvent),

    // Radar and playback
    TogglePlayback,
    ToggleLoopKillRegion(bool),
    SetPlaybackSpeed(f32),
    ScrubTimeline(usize),
    PlaybackTick,
    LoadNpzData(usize), // Load NPZ for selected collection index
    NpzDataLoaded(
        usize,
        std::sync::Arc<crate::npz_loader::NpzData>,
        Option<crate::radar_config::RadarConfig>,
        Option<iced::widget::image::Handle>,
    ),
    NpzLoadError(String),

    // Misc
    Event(Event),
    Noop, // No-op message for read-only text inputs
}
