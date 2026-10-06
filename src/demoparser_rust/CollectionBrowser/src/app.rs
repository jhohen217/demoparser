//! Main application module for the Demo Browser

use crate::bottom_panel;
use crate::config::BrowserConfig;
use crate::control_panel;
use crate::database::{DatabaseInfo, DatabaseManager};
use crate::handlers::parsing;
use crate::message::Message;
use crate::models::{CollectionEntry, CollectionType, DemoDirectory, TickFilterState};
use crate::npz_loader::NpzData;
use crate::playbar::PlaybarState;
use crate::radar_config::RadarConfigManager;
use crate::radar_view::RadarView;
use crate::sorting::{SortColumn, SortOrder};
use crate::style;
use crate::table;
use crate::theme::Theme;
use crate::top_panel;
use demoparser_config::{AppConfig, BatchConfig, DownloaderConfig, ParserConfig, PathsConfig};
use iced::widget::{column, container, mouse_area, row, scrollable, text};
use iced::{Application, Command, Element, Length, Subscription};
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

/// Main application state
pub struct BrowserApp {
    pub(crate) fetch: crate::fetch::State,
    pub(crate) config: BrowserConfig,
    pub(crate) db_manager: DatabaseManager,
    pub(crate) theme: Theme,
    pub(crate) terminal_scroll_id: scrollable::Id,
    pub(crate) table_scroll_id: scrollable::Id,

    // Parser output path
    pub(crate) parser_output_path: String,
    pub(crate) unzip_dir_path: String,
    pub(crate) ram_unzip: bool,

    // Parser configuration
    pub(crate) parser_aces: bool,
    pub(crate) parser_quads: bool,
    pub(crate) parser_triples: bool,
    pub(crate) parser_multi: bool,
    pub(crate) parser_singles: bool,
    pub(crate) parser_doubles: bool,
    pub(crate) parser_grenade_trajectory: bool,
    pub(crate) parser_overwrite: bool,
    pub(crate) parser_output_npz: bool,
    pub(crate) parser_output_s2r: bool,
    pub(crate) parser_threads: u32,
    pub(crate) parser_threads_input: String,

    // Directory state
    pub(crate) demo_directories: Vec<DemoDirectory>,
    pub(crate) selected_directory_index: Option<usize>,
    pub(crate) last_selected_directory_index: Option<usize>,

    // Database state
    pub(crate) all_databases: Vec<DatabaseInfo>,
    pub(crate) loaded_collections: Vec<CollectionEntry>,

    // Filter state
    pub(crate) type_filters: HashMap<CollectionType, bool>,
    pub(crate) filter_tick_data: TickFilterState,
    pub(crate) filter_demo_names: bool,
    pub(crate) filter_demo_names_list: Vec<String>, // Demo names to filter by when active
    pub(crate) search_input: String,
    pub(crate) num_filter_min: String,
    pub(crate) num_filter_max: String,
    pub(crate) num_filter_column: String,
    pub(crate) num_filter_exclude: bool,

    // UI state
    pub(crate) tag_input: String,
    pub(crate) available_tags: Vec<String>,
    pub(crate) terminal_logs: Vec<String>,

    // Selection state
    pub(crate) select_all: bool,
    pub(crate) last_selected_index: Option<usize>,
    pub(crate) shift_pressed: bool,
    pub(crate) alt_pressed: bool,
    pub(crate) ctrl_pressed: bool,

    // Sorting state
    pub(crate) sort_column: Option<SortColumn>,
    pub(crate) sort_order: SortOrder,

    // Tag confirmation state
    pub(crate) confirm_mode: Option<control_panel::ConfirmMode>,

    // Control panel hover state
    pub(crate) hovered_control: Option<String>,

    // Bottom panel resize state
    pub(crate) bottom_panel_height: f32,
    pub(crate) is_dragging_separator: bool,
    pub(crate) is_separator_hovered: bool,
    pub(crate) drag_start_y: Option<f32>,
    pub(crate) drag_start_height: Option<f32>,

    // Parsing state
    pub(crate) is_parsing: bool,
    pub(crate) parsing_progress: f32, // 0.0 to 1.0
    pub(crate) parsing_status: String,
    pub(crate) parsing_directory: Option<std::path::PathBuf>,
    pub(crate) parsing_inputs: Option<Vec<std::path::PathBuf>>,
    pub(crate) parsing_filter: Option<HashMap<std::path::PathBuf, Vec<u32>>>,
    pub(crate) parsing_cancel_token: Option<Arc<AtomicBool>>,

    // Granular progress state
    pub(crate) col_current: usize,
    pub(crate) col_total: usize,
    pub(crate) tick_current: usize,
    pub(crate) tick_total: usize,

    pub(crate) batch_progress: f32,
    pub(crate) total_demos: usize,
    pub(crate) processed_demos: usize,
    pub(crate) total_batches: usize,
    pub(crate) current_batch: usize,
    pub(crate) batch_unzipped_count: usize,

    // Radar and playback state
    pub(crate) radar_view: RadarView,
    pub(crate) playbar_state: PlaybarState,
    pub(crate) radar_config_manager: RadarConfigManager,
    pub(crate) npz_data: Option<Arc<NpzData>>,
}

impl Application for BrowserApp {
    type Executor = iced::executor::Default;
    type Message = Message;
    type Theme = Theme;
    type Flags = ();

    fn new(_flags: ()) -> (Self, Command<Message>) {
        let config = BrowserConfig::load().unwrap_or_else(|e| {
            eprintln!("Failed to load config: {}", e);
            BrowserConfig {
                parser_output: std::path::PathBuf::from("output"),
                unzip_dir: None,
                ram_unzip: false,
                demo_directories: Vec::new(),
                aces: true,
                quads: false,
                triples: false,
                multi: true,
                singles: true,
                doubles: true,
                grenade_trajectory_mode: 1,
                overwrite: false,
                threads: 0,
                output_npz: true,
                output_s2r: true,
                playback_speed: 1.0,
                loop_kill_region: false,
            }
        });

        let master_dir = config.kill_collection_master_dir();
        let db_manager = DatabaseManager::new(master_dir.clone());

        let mut type_filters = HashMap::new();
        for coll_type in CollectionType::all() {
            // Default: only Ace, Multi, and Double are enabled
            let enabled = matches!(
                coll_type,
                CollectionType::Ace | CollectionType::Multi | CollectionType::Double
            );
            type_filters.insert(coll_type, enabled);
        }

        let demo_directories = config.demo_directories.clone();
        let parser_output_path = config.parser_output.to_string_lossy().to_string();
        let unzip_dir_path = config
            .unzip_dir
            .as_ref()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();

        // Copy parser config state
        let parser_aces = config.aces;
        let parser_quads = config.quads;
        let parser_triples = config.triples;
        let parser_multi = config.multi;
        let parser_singles = config.singles;
        let parser_doubles = config.doubles;
        let parser_grenade_trajectory = config.grenade_trajectory_mode == 1;
        let parser_overwrite = config.overwrite;
        let parser_output_npz = config.output_npz;
        let parser_output_s2r = config.output_s2r;
        let parser_threads = config.threads;
        let parser_threads_input = if parser_threads == 0 {
            String::new()
        } else {
            parser_threads.to_string()
        };

        let ram_unzip = config.ram_unzip;

        // Save playback settings before moving config
        let playback_speed = config.playback_speed;
        let loop_kill_region = config.loop_kill_region;

        let app = Self {
            fetch: crate::fetch::State::default(),
            parser_output_path,
            unzip_dir_path,
            ram_unzip,
            parser_aces,
            parser_quads,
            parser_triples,
            parser_multi,
            parser_singles,
            parser_doubles,
            parser_grenade_trajectory,
            parser_overwrite,
            parser_output_npz,
            parser_output_s2r,
            parser_threads,
            parser_threads_input,
            config,
            db_manager,
            theme: Theme::new(),
            demo_directories,
            selected_directory_index: None,
            last_selected_directory_index: None,
            all_databases: Vec::new(),
            loaded_collections: Vec::new(),
            type_filters,
            filter_tick_data: TickFilterState::All,
            filter_demo_names: false,
            filter_demo_names_list: Vec::new(),
            search_input: String::new(),
            num_filter_min: String::new(),
            num_filter_max: String::new(),
            num_filter_column: String::from("Duration"),
            num_filter_exclude: false,
            tag_input: String::new(),
            available_tags: Vec::new(),
            terminal_logs: vec![format!(
                "Demo Browser v1.0.0 - Reading from: {}",
                master_dir.display()
            )],
            select_all: false,
            last_selected_index: None,
            shift_pressed: false,
            alt_pressed: false,
            ctrl_pressed: false,
            sort_column: None,
            sort_order: SortOrder::Ascending,
            confirm_mode: None,
            hovered_control: None, // Initialize hover state
            terminal_scroll_id: scrollable::Id::unique(),
            table_scroll_id: scrollable::Id::unique(),
            bottom_panel_height: 160.0,
            is_dragging_separator: false,
            is_separator_hovered: false,
            drag_start_y: None,
            drag_start_height: None,

            is_parsing: false,
            parsing_progress: 0.0,
            parsing_status: String::new(),
            parsing_directory: None,
            parsing_inputs: None,
            parsing_filter: None,
            parsing_cancel_token: None,
            col_current: 0,
            col_total: 0,
            tick_current: 0,
            tick_total: 0,
            batch_progress: 0.0,
            total_demos: 0,
            processed_demos: 0,
            total_batches: 0,
            current_batch: 0,
            batch_unzipped_count: 0,

            // Initialize radar and playback state with settings from config
            radar_view: RadarView::new(),
            playbar_state: {
                let mut state = PlaybarState::new();
                state.playback_speed = playback_speed;
                state.loop_kill_region = loop_kill_region;
                state
            },
            radar_config_manager: RadarConfigManager::new(
                std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(|p| p.join("radar")))
                    .unwrap_or_else(|| std::path::PathBuf::from("radar")),
            ),
            npz_data: None,
        };

        // RefreshDatabases now handles all directory scanning and counting
        (
            app,
            Command::perform(async {}, |_| Message::RefreshDatabases),
        )
    }

    fn title(&self) -> String {
        String::from("Demo Browser")
    }

    fn theme(&self) -> Theme {
        self.theme
    }

    fn subscription(&self) -> Subscription<Message> {
        let events = iced::event::listen().map(Message::Event);

        // Add playback ticker when playing
        let playback = if self.playbar_state.playing && self.playbar_state.total_ticks > 0 {
            let speed = self.playbar_state.playback_speed;
            // Calculate interval: base 64 ticks/sec, adjusted by speed
            let interval_ms = (1000.0 / (64.0 * speed)) as u64;
            iced::time::every(std::time::Duration::from_millis(interval_ms))
                .map(|_| Message::PlaybackTick)
        } else {
            Subscription::none()
        };

        if self.fetch.job.is_some() {
            return Subscription::batch(vec![events, playback, crate::fetch::subscription(self)]);
        }

        if self.is_parsing {
            // Check if we have parsing inputs explicitly set (for selected collection parsing)
            // OR if we have a directory set (for whole directory parsing)
            let inputs_opt = if let Some(inputs) = &self.parsing_inputs {
                Some(inputs.clone())
            } else if let Some(dir) = &self.parsing_directory {
                Some(vec![dir.clone()])
            } else {
                None
            };

            if let Some(inputs) = inputs_opt {
                // If we are parsing specific collections (filter is present), force enable all collection types
                // to ensure the desired collections are generated and not filtered out by global config settings.
                let force_all_types = self.parsing_filter.is_some();

                let saved_parser = AppConfig::load().ok().map(|config| config.parser);
                let config_clone = AppConfig {
                    paths: PathsConfig {
                        parser_output: self.config.parser_output.clone(),
                        unzip_dir: self.config.unzip_dir.clone(),
                        ram_unzip: self.config.ram_unzip,
                    },
                    parser: ParserConfig {
                        process_tick_data: true,
                        pad_ticks: 0,
                        skip_buy_time: true,
                        track_all_players: true,
                        aces: if force_all_types {
                            true
                        } else {
                            self.config.aces
                        },
                        quads: if force_all_types {
                            true
                        } else {
                            self.config.quads
                        },
                        triples: if force_all_types {
                            true
                        } else {
                            self.config.triples
                        },
                        multi: if force_all_types {
                            true
                        } else {
                            self.config.multi
                        },
                        singles: if force_all_types {
                            true
                        } else {
                            self.config.singles
                        },
                        doubles: if force_all_types {
                            true
                        } else {
                            self.config.doubles
                        },
                        overwrite: self.config.overwrite,
                        catalog_aces: saved_parser
                            .as_ref()
                            .map_or(true, |parser| parser.catalog_aces),
                        catalog_quads: saved_parser
                            .as_ref()
                            .map_or(true, |parser| parser.catalog_quads),
                        catalog_triples: saved_parser
                            .as_ref()
                            .map_or(true, |parser| parser.catalog_triples),
                        catalog_multi: saved_parser
                            .as_ref()
                            .map_or(true, |parser| parser.catalog_multi),
                        catalog_singles: saved_parser
                            .as_ref()
                            .map_or(true, |parser| parser.catalog_singles),
                        catalog_doubles: saved_parser
                            .as_ref()
                            .map_or(true, |parser| parser.catalog_doubles),
                        trim_aces: saved_parser
                            .as_ref()
                            .map_or(true, |parser| parser.trim_aces),
                        trim_quads: saved_parser
                            .as_ref()
                            .map_or(true, |parser| parser.trim_quads),
                        trim_triples: saved_parser
                            .as_ref()
                            .map_or(true, |parser| parser.trim_triples),
                        trim_multi: saved_parser
                            .as_ref()
                            .map_or(true, |parser| parser.trim_multi),
                        trim_singles: saved_parser
                            .as_ref()
                            .map_or(true, |parser| parser.trim_singles),
                        trim_doubles: saved_parser
                            .as_ref()
                            .map_or(true, |parser| parser.trim_doubles),
                        trim_collection_rounds: saved_parser
                            .as_ref()
                            .map_or(false, |parser| parser.trim_collection_rounds),
                        delete_source_after_trim: saved_parser
                            .as_ref()
                            .map_or(false, |parser| parser.delete_source_after_trim),
                    },
                    downloader: DownloaderConfig { batch_size: 32 },
                    batch: BatchConfig {
                        threads: if self.config.threads > 0 {
                            Some(self.config.threads as usize)
                        } else {
                            None
                        },
                        prefetch_depth: 1,
                        max_concurrent_files: 16,
                        max_retries: 3,
                        retry_delay: 1.0,
                        show_progress: true,
                        autoclose: true,
                    },
                };

                let filter_clone = self.parsing_filter.clone();
                let cancel_token = self.parsing_cancel_token.clone();

                // Create parsing subscription
                let parsing = iced::subscription::channel(
                    std::any::TypeId::of::<demoparser::ProgressEvent>(),
                    100,
                    move |output| {
                        let inputs_clone = inputs.clone();
                        let config_clone = config_clone.clone();
                        let filter_clone = filter_clone.clone();
                        let cancel_token_clone = cancel_token.clone();

                        // Spawn the parsing task
                        async move {
                            let output = Arc::new(Mutex::new(output));
                            let output_clone = output.clone();

                            let _ = demoparser::run_parsing(
                                inputs_clone,
                                config_clone,
                                move |event| {
                                    if let Ok(mut tx) = output_clone.lock() {
                                        let _ = tx.try_send(Message::ParsingEvent(event));
                                    }
                                },
                                filter_clone,
                                false,
                                demoparser::InputKind::All,
                                cancel_token_clone,
                            )
                            .await;

                            loop {
                                std::future::pending::<()>().await;
                            }
                        }
                    },
                );
                return Subscription::batch(vec![events, playback, parsing]);
            }
        }

        Subscription::batch(vec![events, playback])
    }

    fn update(&mut self, message: Message) -> Command<Message> {
        match message {
            Message::ParseCollections => parsing::handle_parse_collections(self),
            Message::ParseDirectory => parsing::handle_parse_directory(self),
            Message::ParsingEvent(event) => parsing::handle_parsing_event(self, event),
            Message::CancelParsing => parsing::handle_cancel_parsing(self),
            _ => self.handle_message(message),
        }
    }

    fn view(&self) -> Element<'_, Message, Theme, iced::Renderer> {
        // Count filtered results for display
        let filtered_count = if self.search_input.trim().is_empty() {
            self.loaded_collections.len()
        } else {
            self.loaded_collections
                .iter()
                .filter(|entry| Self::matches_search(entry, &self.search_input))
                .count()
        };

        let selected_count = self
            .loaded_collections
            .iter()
            .filter(|c| c.selected)
            .count();

        let top_panel = top_panel::view(
            &self.parser_output_path,
            &self.type_filters,
            self.filter_tick_data,
            self.filter_demo_names,
            selected_count,
            &self.search_input,
            &self.num_filter_min,
            &self.num_filter_max,
            &self.num_filter_column,
            self.num_filter_exclude,
            &self.loaded_collections,
            filtered_count,
            self.is_parsing,
        );

        // Main Table - pass search_input, demo name filter, and numerical filters for filtering
        let table = container(table::view(
            &self.loaded_collections,
            self.sort_column,
            self.sort_order,
            self.select_all,
            self.filter_demo_names,
            &self.filter_demo_names_list,
            &self.search_input,
            &self.num_filter_min,
            &self.num_filter_max,
            &self.num_filter_column,
            self.num_filter_exclude,
        ))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(style::Container::Table)
        .padding(2);

        // Bottom Area (Terminal + Details)
        // If parsing, show progress bar overlay or integration
        let bottom_content_area: Element<Message, Theme, iced::Renderer> = if self.is_parsing {
            column![
                text(&self.parsing_status).size(14),
                // Progress bar removed as requested, using Control Panel bars instead
                // Show terminal below progress
                bottom_panel::view(
                    &self.terminal_logs,
                    None, // No selection details during parsing ideally, or just current
                    &self.theme,
                    self.terminal_scroll_id.clone(),
                    self.bottom_panel_height - 30.0, // adjust height
                    &self.radar_view,
                    &self.playbar_state,
                )
            ]
            .spacing(5)
            .padding(5)
            .height(Length::Shrink)
            .into()
        } else {
            let selected_collection = self
                .last_selected_index
                .and_then(|idx| self.loaded_collections.get(idx))
                .filter(|c| c.selected)
                .or_else(|| self.loaded_collections.iter().find(|c| c.selected));

            bottom_panel::view(
                &self.terminal_logs,
                selected_collection,
                &self.theme,
                self.terminal_scroll_id.clone(),
                self.bottom_panel_height,
                &self.radar_view,
                &self.playbar_state,
            )
        };

        let bottom_area = container(bottom_content_area)
            .height(Length::Shrink)
            .width(Length::Fill);

        // Draggable separator
        let separator = mouse_area(
            container(text("").size(1))
                .width(Length::Fill)
                .height(Length::Fixed(2.0))
                .style(style::Container::Separator(self.is_separator_hovered)),
        )
        .on_press(Message::SeparatorPressed)
        .on_enter(Message::SeparatorHovered)
        .on_exit(Message::SeparatorUnhovered);

        // Right Panel (Control Panel)

        // Calculate progress values and labels
        let col_progress_val = if self.col_total > 0 {
            self.col_current as f32 / self.col_total as f32
        } else {
            0.0
        };
        // Change label to shows progress if active (total > 0)
        let col_label = if self.col_total > 0 {
            format!("Collection {}/{}", self.col_current, self.col_total)
        } else {
            "Collection".to_string()
        };

        let tick_progress_val = if self.tick_total > 0 {
            self.tick_current as f32 / self.tick_total as f32
        } else {
            0.0
        };
        let tick_label = if self.tick_total > 0 {
            format!("Tick {}/{}", self.tick_current, self.tick_total)
        } else {
            "Tick".to_string()
        };

        // Batch label: Demos X/Y (N/M Demos processed) - as requested: "Demos DemosProcessed#/totalDemos#" effectively
        // User asked for: "Demos DemosProcessed#/totalDemos#" (e.g. Demos 48/54 demo count)
        // But also said "progress bar tracks actual batch 5/12".
        // So Label tracks demos, Bar tracks batches.
        let batch_label = if self.total_demos > 0 {
            format!("Demos {}/{}", self.processed_demos, self.total_demos)
        } else {
            "Demos".to_string()
        };

        let selected_count = self
            .loaded_collections
            .iter()
            .filter(|c| c.selected)
            .count();

        let right_panel = control_panel::view(
            &self.demo_directories,
            self.selected_directory_index,
            selected_count,
            &self.tag_input,
            self.confirm_mode,
            &self.unzip_dir_path,
            self.ram_unzip,
            self.parser_aces,
            self.parser_quads,
            self.parser_triples,
            self.parser_multi,
            self.parser_singles,
            self.parser_doubles,
            self.parser_grenade_trajectory,
            self.parser_overwrite,
            self.parser_output_npz,
            self.parser_output_s2r,
            &self.parser_threads_input,
            self.hovered_control.as_deref(), // Pass hover state
            col_progress_val,
            tick_progress_val,
            self.batch_progress,
            col_label,
            tick_label,
            batch_label,
            self.is_parsing,
        );

        // Wrap right panel in container and disable interaction if parsing
        // We can just disable specific buttons in control_panel.view based on is_parsing flag passed down?
        // Modifying control_panel::view signature is cleaner.
        // For now, let's keep it simple.

        // Left side: top panel + table + separator + bottom area (terminal + details)
        let left_side = column![
            crate::fetch::view(self),
            container(top_panel)
                .style(style::Container::Panel)
                .width(Length::Fill),
            table,
            separator,
            bottom_area,
        ]
        .spacing(0)
        .width(Length::Fill)
        .height(Length::Fill);

        // Main Layout - row with left side and right panel
        let layout = row![
            left_side,
            container(right_panel)
                .style(style::Container::Panel)
                .height(Length::Fill),
        ]
        .height(Length::Fill)
        .width(Length::Fill);

        container(layout)
            .style(style::Container::Main)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }
}

impl BrowserApp {
    /// Log a message to the terminal and auto-scroll
    pub fn log(&mut self, message: String) -> Command<Message> {
        self.terminal_logs.push(message);
        if self.terminal_logs.len() > 1000 {
            self.terminal_logs.remove(0);
        }
        scrollable::snap_to(
            self.terminal_scroll_id.clone(),
            scrollable::RelativeOffset::END,
        )
    }

    /// Get databases filtered by both directory and type filters
    pub fn get_filtered_databases(&self) -> Vec<DatabaseInfo> {
        // Get enabled folder names
        let enabled_folders: Vec<String> = self
            .demo_directories
            .iter()
            .filter(|d| d.enabled)
            .map(|d| d.folder_name())
            .collect();

        // If no directories are configured OR none are enabled, return empty list
        if enabled_folders.is_empty() {
            return Vec::new();
        }

        self.all_databases
            .iter()
            .filter(|db| {
                // Check collection type filter
                let type_matches = CollectionType::all().iter().any(|coll_type| {
                    self.type_filters.get(coll_type).copied().unwrap_or(false)
                        && db.matches_type(*coll_type)
                });

                // Check directory filter - only allow if folder is in enabled list
                let dir_matches = enabled_folders.contains(&db.folder);

                type_matches && dir_matches
            })
            .cloned()
            .collect()
    }

    /// Save parser configuration to config.ini
    pub fn save_parser_config(&self) -> Command<Message> {
        let config_clone = self.config.clone();
        Command::perform(async move { config_clone.save() }, |result| {
            Message::ParserConfigSaved(result.map_err(|e| e.to_string()))
        })
    }

    /// Apply search filter to collections
    pub fn get_filtered_collections(&self) -> Vec<CollectionEntry> {
        if self.search_input.trim().is_empty() {
            return self.loaded_collections.clone();
        }

        self.loaded_collections
            .iter()
            .filter(|entry| Self::matches_search(entry, &self.search_input))
            .cloned()
            .collect()
    }

    /// Check if a collection entry matches the search criteria
    fn matches_search(entry: &CollectionEntry, search: &str) -> bool {
        if search.trim().is_empty() {
            return true;
        }

        // Build searchable text from non-numerical fields
        let searchable_text = format!(
            "{} {} {} {} {}",
            entry.killer_name,
            entry.map_name,
            entry.weapons_formatted,
            entry.util_thrown,
            entry.tag
        )
        .to_lowercase();

        // Split by comma for OR logic
        let terms: Vec<&str> = search.split(',').map(|s| s.trim()).collect();

        // Entry matches if ANY term matches
        terms.iter().any(|term| {
            if term.starts_with('!') {
                // Exclusion - must NOT contain
                let exclude_term = term[1..].trim().to_lowercase();
                if exclude_term.is_empty() {
                    return true; // Empty exclusion matches everything
                }
                !searchable_text.contains(&exclude_term)
            } else {
                // Inclusion - must contain
                let include_term = term.trim().to_lowercase();
                if include_term.is_empty() {
                    return true; // Empty term matches everything
                }
                searchable_text.contains(&include_term)
            }
        })
    }
}
