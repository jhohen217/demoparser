//! Message handlers for the BrowserApp
//!
//! This module contains all the message handling logic, organized by domain.

pub mod database;
pub mod directory;
pub mod parsing;
pub mod selection;
pub mod tags;

use crate::app::BrowserApp;
use crate::message::Message;
use iced::Command;

impl BrowserApp {
    /// Main message dispatcher that routes messages to appropriate handlers
    pub fn handle_message(&mut self, message: Message) -> Command<Message> {
        match message {
            Message::Fetch(message) => crate::fetch::handle(self, message),
            // Parser output management
            Message::BrowseParserOutput => directory::handle_browse_parser_output(self),
            Message::ParserOutputSelected(path_opt) => {
                directory::handle_parser_output_selected(self, path_opt)
            }
            Message::ParserOutputChanged(value) => {
                directory::handle_parser_output_changed(self, value)
            }

            // Unzip directory management
            Message::BrowseUnzipDir => directory::handle_browse_unzip_dir(self),
            Message::UnzipDirSelected(path_opt) => {
                directory::handle_unzip_dir_selected(self, path_opt)
            }
            Message::UnzipDirChanged(value) => directory::handle_unzip_dir_changed(self, value),
            Message::ToggleRamUnzip(enabled) => directory::handle_toggle_ram_unzip(self, enabled),

            // Directory management
            Message::AddDemoDirectory => directory::handle_add(self),
            Message::DirectorySelected(path_opt) => directory::handle_selected(self, path_opt),
            Message::RemoveDemoDirectory => directory::handle_remove(self),
            Message::SelectDirectoryRow(index) => directory::handle_select_row(self, index),
            Message::ToggleDirectoryEnabled(idx, enabled) => {
                directory::handle_toggle_enabled(self, idx, enabled)
            }
            Message::OpenDirectoryInExplorer => directory::handle_open_in_explorer(self),
            Message::DirectoryScanned(idx, result) => directory::handle_scanned(self, idx, result),
            Message::AllDirectoryStatsCounted(result) => {
                directory::handle_all_stats_counted(self, result)
            }

            // Database management
            Message::RefreshDatabases => database::handle_refresh(self),
            Message::DatabasesScanned(result) => database::handle_scanned(self, result),
            Message::CollectionsLoaded(result, should_recalculate) => {
                database::handle_collections_loaded(self, result, should_recalculate)
            }
            Message::ToggleFilter(coll_type, enabled) => {
                database::handle_toggle_filter(self, coll_type, enabled)
            }
            Message::ToggleTickDataFilter(state) => {
                database::handle_toggle_tick_data_filter(self, state)
            }
            Message::Sort(column) => database::handle_sort(self, column),

            // Selection management
            Message::ToggleSelectAll(selected) => {
                selection::handle_toggle_select_all(self, selected)
            }
            Message::ToggleSelect(index, selected) => {
                selection::handle_toggle_select(self, index, selected)
            }
            Message::SelectRow(index) => selection::handle_select_row(self, index),
            Message::NavigateUp => selection::handle_navigate_up(self),
            Message::NavigateDown => selection::handle_navigate_down(self),
            Message::NavigateLeft => selection::handle_navigate_left(self),
            Message::NavigateRight => selection::handle_navigate_right(self),
            Message::DeselectAllCollections => selection::handle_deselect_all_collections(self),
            Message::DeselectAllDirectories => directory::handle_deselect_all_directories(self),
            Message::DetailsLoaded(index, result) => {
                selection::handle_details_loaded(self, index, result)
            }

            // Tag management
            Message::TagInputChanged(value) => tags::handle_input_changed(self, value),
            Message::ApplyTag => tags::handle_apply(self),
            Message::ClearTag => tags::handle_clear(self),
            Message::ConfirmTagAction => tags::handle_confirm(self),
            Message::CancelTagAction => tags::handle_cancel(self),
            Message::TagsUpdated(result) => tags::handle_updated(self, result),

            // Search filter
            Message::SearchInputChanged(value) => {
                self.search_input = value;
                Command::none()
            }

            // Demo name filter
            Message::ToggleDemoNameFilter => {
                if !self.filter_demo_names {
                    // ACTIVATING: Capture demo names from selected entries
                    let demo_names: Vec<String> = self
                        .loaded_collections
                        .iter()
                        .filter(|c| c.selected)
                        .map(|c| c.demo_name.clone())
                        .collect();

                    if demo_names.is_empty() {
                        return Command::none();
                    }

                    self.filter_demo_names_list = demo_names;
                    self.filter_demo_names = true;

                    // Reload ALL collections (all types, all tick states)
                    let enabled_folders: Vec<String> = self
                        .demo_directories
                        .iter()
                        .filter(|d| d.enabled)
                        .map(|d| d.folder_name())
                        .collect();

                    if enabled_folders.is_empty() {
                        return Command::none();
                    }

                    let all_dbs: Vec<crate::database::DatabaseInfo> = self
                        .all_databases
                        .iter()
                        .filter(|db| enabled_folders.contains(&db.folder))
                        .cloned()
                        .collect();

                    let manager = self.db_manager.clone();
                    return Command::perform(
                        async move {
                            manager
                                .load_collections(&all_dbs, crate::models::TickFilterState::All)
                                .map_err(|e| e.to_string())
                        },
                        |result| Message::CollectionsLoaded(result, false),
                    );
                } else {
                    // DEACTIVATING: Clear list and reload with normal filters
                    self.filter_demo_names = false;
                    self.filter_demo_names_list.clear();

                    let filtered_dbs = self.get_filtered_databases();
                    let manager = self.db_manager.clone();
                    let filter_tick_data = self.filter_tick_data;

                    return Command::perform(
                        async move {
                            manager
                                .load_collections(&filtered_dbs, filter_tick_data)
                                .map_err(|e| e.to_string())
                        },
                        |result| Message::CollectionsLoaded(result, false),
                    );
                }
            }

            Message::NumFilterMinChanged(value) => {
                self.num_filter_min = value;
                Command::none()
            }

            Message::NumFilterMaxChanged(value) => {
                self.num_filter_max = value;
                Command::none()
            }

            Message::NumFilterColumnChanged(value) => {
                self.num_filter_column = value;
                Command::none()
            }

            Message::NumFilterExcludeToggled(enabled) => {
                self.num_filter_exclude = enabled;
                Command::none()
            }

            // Export and validation
            Message::ExportSelections => tags::handle_export(self),

            // Parser execution
            Message::ParseDirectory => directory::handle_parse_directory(self),
            Message::ParseCollections => Command::none(), // Handled in update loop directly
            Message::CancelParsing => parsing::handle_cancel_parsing(self), // Also handled in update, but must be exhaustive
            Message::ParsingEvent(_) => Command::none(), // Handled in update loop directly

            // Separator drag
            Message::SeparatorPressed => {
                self.is_dragging_separator = true;
                Command::none()
            }
            Message::SeparatorHovered => {
                self.is_separator_hovered = true;
                Command::none()
            }
            Message::SeparatorUnhovered => {
                self.is_separator_hovered = false;
                Command::none()
            }

            // Control panel hover states
            Message::ControlHovered(id) => {
                self.hovered_control = Some(id);
                Command::none()
            }
            Message::ControlUnhovered => {
                self.hovered_control = None;
                Command::none()
            }

            // Parser configuration - auto-save on each change
            Message::ToggleParserAces(enabled) => {
                self.parser_aces = enabled;
                self.config.aces = enabled;
                self.save_parser_config()
            }
            Message::ToggleParserQuads(enabled) => {
                self.parser_quads = enabled;
                self.config.quads = enabled;
                self.save_parser_config()
            }
            Message::ToggleParserTriples(enabled) => {
                self.parser_triples = enabled;
                self.config.triples = enabled;
                self.save_parser_config()
            }
            Message::ToggleParserMulti(enabled) => {
                self.parser_multi = enabled;
                self.config.multi = enabled;
                self.save_parser_config()
            }
            Message::ToggleParserSingles(enabled) => {
                self.parser_singles = enabled;
                self.config.singles = enabled;
                self.save_parser_config()
            }
            Message::ToggleParserDoubles(enabled) => {
                self.parser_doubles = enabled;
                self.config.doubles = enabled;
                self.save_parser_config()
            }
            Message::ToggleParserGrenadeTrajectory(enabled) => {
                self.parser_grenade_trajectory = enabled;
                self.config.grenade_trajectory_mode = if enabled { 1 } else { 0 };
                self.save_parser_config()
            }
            Message::ToggleParserOverwrite(enabled) => {
                self.parser_overwrite = enabled;
                self.config.overwrite = enabled;
                self.save_parser_config()
            }
            Message::ToggleOutputNpz(enabled) => {
                self.parser_output_npz = enabled;
                self.config.output_npz = enabled;
                self.save_parser_config()
            }
            Message::ToggleOutputS2r(enabled) => {
                self.parser_output_s2r = enabled;
                self.config.output_s2r = enabled;
                self.save_parser_config()
            }
            Message::ParserThreadsInputChanged(input) => {
                self.parser_threads_input = input.clone();

                // Parse the input - if empty or invalid, treat as 0 (auto)
                let threads = if input.trim().is_empty() {
                    0
                } else {
                    input.trim().parse::<u32>().unwrap_or(0)
                };

                self.parser_threads = threads;
                self.config.threads = threads;
                self.save_parser_config()
            }
            Message::ParserConfigSaved(result) => {
                match result {
                    Ok(()) => Command::none(), // Silent save
                    Err(e) => self.log(format!("Failed to save parser configuration: {}", e)),
                }
            }

            // Radar and playback
            Message::TogglePlayback => {
                self.playbar_state.playing = !self.playbar_state.playing;
                Command::none()
            }
            Message::ToggleLoopKillRegion(enabled) => {
                self.playbar_state.loop_kill_region = enabled;
                self.config.loop_kill_region = enabled;

                // When enabling loop mode, immediately jump to the loop start position
                // This provides immediate feedback to the user
                if enabled && !self.playbar_state.kill_ticks.is_empty() {
                    if let Some(npz_data) = &self.npz_data {
                        const LOOP_PADDING_TICKS: i32 = 64; // 1 second padding

                        let first_kill_tick = self.playbar_state.kill_ticks[0];
                        let loop_start_tick = first_kill_tick - LOOP_PADDING_TICKS;

                        // Find the tick index for the loop start
                        let loop_start_idx = npz_data
                            .frames
                            .iter()
                            .position(|&tick| tick >= loop_start_tick)
                            .unwrap_or(0);

                        self.playbar_state.current_tick_idx = loop_start_idx;
                        self.radar_view.set_current_tick(loop_start_idx);

                        // Ensure playback is active for immediate feedback
                        self.playbar_state.playing = true;
                    }
                }

                self.save_parser_config()
            }
            Message::SetPlaybackSpeed(speed) => {
                self.playbar_state.playback_speed = speed;
                self.config.playback_speed = speed;
                self.save_parser_config()
            }
            Message::ScrubTimeline(tick_idx) => {
                self.playbar_state.current_tick_idx = tick_idx;
                self.radar_view.set_current_tick(tick_idx);
                Command::none()
            }
            Message::PlaybackTick => {
                if self.playbar_state.playing {
                    if self.playbar_state.loop_kill_region
                        && !self.playbar_state.kill_ticks.is_empty()
                    {
                        // Loop Kill-Region mode: loop from 64 ticks before first kill to enough ticks after last kill
                        if let Some(npz_data) = &self.npz_data {
                            // Constants for timing offsets
                            const DISPLAY_FRAME_OFFSET: i32 = 1; // Shots display 1 frame earlier
                            const DECAY_FRAMES: i32 = 8; // Shot lines decay over 8 frames
                            const LOOP_PADDING_TICKS: i32 = 64; // 1 second padding

                            let first_kill_tick = self.playbar_state.kill_ticks[0];
                            let last_kill_tick = *self.playbar_state.kill_ticks.last().unwrap();

                            // Calculate loop region boundaries
                            // Start: 64 ticks before first kill
                            let loop_start_tick = first_kill_tick - LOOP_PADDING_TICKS;

                            // End: After last kill + padding + decay frames + display offset
                            // This ensures we show the complete decay animation of the last shot
                            let loop_end_tick = last_kill_tick + LOOP_PADDING_TICKS + DECAY_FRAMES
                                - DISPLAY_FRAME_OFFSET;

                            // Find tick indices for loop boundaries
                            let loop_start_idx = npz_data
                                .frames
                                .iter()
                                .position(|&tick| tick >= loop_start_tick)
                                .unwrap_or(0);
                            // For loop_end: find the LAST frame that's within or just past the target region
                            let loop_end_idx = npz_data
                                .frames
                                .iter()
                                .rposition(|&tick| tick <= loop_end_tick)
                                .unwrap_or(npz_data.frames.len().saturating_sub(1))
                                .min(npz_data.frames.len().saturating_sub(1));

                            // Advance tick
                            self.playbar_state.current_tick_idx += 1;

                            // Check if we've passed the end of the loop region
                            // Loop back after displaying the frame at loop_end_idx
                            if self.playbar_state.current_tick_idx > loop_end_idx {
                                // Loop back to the start of the kill region
                                self.playbar_state.current_tick_idx = loop_start_idx;
                            }
                        } else {
                            // Fallback to normal advance if no NPZ data
                            self.playbar_state.advance_tick();
                        }
                    } else {
                        // Normal playback: advance and wrap around at end
                        self.playbar_state.advance_tick();
                    }
                    self.radar_view
                        .set_current_tick(self.playbar_state.current_tick_idx);
                }
                Command::none()
            }
            Message::NpzDataLoaded(index, npz_data, radar_config, image_handle) => {
                // Set NPZ data and radar config on the radar view
                self.radar_view.set_npz_data(Some(npz_data.clone()));
                if let Some(config) = radar_config {
                    self.radar_view.set_radar_config(Some(config));
                }

                // Set radar background image
                self.radar_view.set_radar_image(image_handle);

                // Update playbar with total ticks and kill ticks
                self.playbar_state.total_ticks = npz_data.frames.len();
                self.playbar_state
                    .set_kill_ticks(npz_data.kill_ticks.clone());

                // Store NPZ data for loop region calculation
                self.npz_data = Some(npz_data.clone());

                // Start playback 64 ticks before the first kill (1 second at 64 tick rate)
                let start_tick_idx = if !npz_data.kill_ticks.is_empty() {
                    // Get the first kill tick from the NPZ data
                    let first_kill_tick = npz_data.kill_ticks[0];

                    // Find the tick index that's 64 ticks before the first kill
                    let target_tick = first_kill_tick - 64;

                    // Find the closest tick index to the target
                    npz_data
                        .frames
                        .iter()
                        .position(|&tick| tick >= target_tick)
                        .unwrap_or(0)
                } else {
                    0 // Fallback to start if no kill ticks available
                };

                self.playbar_state.current_tick_idx = start_tick_idx;
                self.radar_view.set_current_tick(start_tick_idx);
                self.playbar_state.playing = true; // Auto-play on selection

                self.log(format!(
                    "Loaded NPZ data: {} ticks for collection {} (starting at tick {})",
                    npz_data.frames.len(),
                    index,
                    start_tick_idx
                ))
            }
            Message::NpzLoadError(error) => self.log(format!("ERROR: {}", error)),
            Message::LoadNpzData(index) => {
                // Load NPZ file for the selected collection
                if let Some(collection) = self.loaded_collections.get(index) {
                    if collection.tick_data == 1 {
                        // Build NPZ path using correct format from scanner
                        // Path: parser_output/TickByTick/folder/collection_type/{TYPE}_{DEMONAME}_{COLLECTIONNUM}.npz
                        let parser_output = self.config.parser_output.clone();
                        let folder = collection.folder();
                        let collection_type = &collection.collection_type;
                        let collection_num = collection.collection_num;

                        // NPZ filename format: {TYPE}_{DEMONAME}_{COLLECTIONNUM}.npz
                        // Strip ALL extensions from demo_name (e.g., "file.dem.gz" -> "file")
                        let mut demo_name_base = collection.demo_name.as_str();
                        while let Some(stem) = std::path::Path::new(demo_name_base)
                            .file_stem()
                            .and_then(|s| s.to_str())
                        {
                            if stem == demo_name_base {
                                break; // No more extensions
                            }
                            demo_name_base = stem;
                        }

                        let npz_filename = format!(
                            "{}_{}_{}.npz",
                            collection_type, demo_name_base, collection_num
                        );

                        // Path: parser_output/TickByTick/folder/collection_type/filename
                        let npz_path = parser_output
                            .join("TickByTick")
                            .join(&folder)
                            .join(collection_type)
                            .join(&npz_filename);

                        // Log the path for debugging (S2R fallback is tried automatically)
                        self.terminal_logs.push(format!(
                            "Loading replay: {}",
                            npz_path.with_extension("").display()
                        ));

                        let map_name = collection.map_name.clone();
                        let radar_dir = std::env::current_exe()
                            .ok()
                            .and_then(|p| p.parent().map(|p| p.join("radar")))
                            .unwrap_or_else(|| std::path::PathBuf::from("radar"));

                        return Command::perform(
                            async move {
                                // Load NPZ file
                                use crate::npz_loader::NpzData;
                                use crate::radar_config::RadarConfig;
                                use std::sync::Arc;

                                // Try NPZ first; if absent, fall back to the .s2r file.
                                let s2r_path = npz_path.with_extension("s2r");
                                let npz_result = if npz_path.exists() {
                                    NpzData::load_from_file(&npz_path)
                                } else if s2r_path.exists() {
                                    NpzData::load_from_s2r_file(&s2r_path)
                                } else {
                                    Err(anyhow::anyhow!(
                                        "No replay file found — tried:\n  {}\n  {}",
                                        npz_path.display(),
                                        s2r_path.display()
                                    ))
                                };

                                match npz_result {
                                    Ok(npz_data) => {
                                        // Use map name from NPZ data (more reliable)
                                        let actual_map_name = &npz_data.map_name;

                                        // Try to load radar config using actual map name
                                        let config_path =
                                            radar_dir.join(format!("{}.txt", actual_map_name));
                                        let radar_config_result =
                                            RadarConfig::from_file(&config_path);

                                        let (radar_config, config_error) = match radar_config_result
                                        {
                                            Ok(config) => (Some(config), None),
                                            Err(e) => (
                                                None,
                                                Some(format!(
                                                    "Radar config not found: {} ({})",
                                                    config_path.display(),
                                                    e
                                                )),
                                            ),
                                        };

                                        // Determine which radar image to load based on median altitude
                                        let (image_handle, section_name) = if let Some(ref config) =
                                            radar_config
                                        {
                                            let median_altitude =
                                                npz_data.calculate_median_altitude();
                                            let section = median_altitude
                                                .and_then(|alt| {
                                                    config.get_section_for_altitude(alt)
                                                })
                                                .map(|s| s.name.as_str())
                                                .unwrap_or("default");

                                            // Build image path: {map_name}_{section}_radar_psd.png or {map_name}_radar_psd.png for default
                                            let image_path = if section == "default" {
                                                radar_dir.join(format!(
                                                    "{}_radar_psd.png",
                                                    actual_map_name
                                                ))
                                            } else {
                                                radar_dir.join(format!(
                                                    "{}_{}_radar_psd.png",
                                                    actual_map_name, section
                                                ))
                                            };

                                            // Try to load the image
                                            let handle = if image_path.exists() {
                                                Some(iced::widget::image::Handle::from_path(
                                                    &image_path,
                                                ))
                                            } else {
                                                // Try fallback to default if specific image doesn't exist
                                                let fallback_path = radar_dir.join(format!(
                                                    "{}_radar_psd.png",
                                                    actual_map_name
                                                ));
                                                if fallback_path.exists() {
                                                    Some(iced::widget::image::Handle::from_path(
                                                        &fallback_path,
                                                    ))
                                                } else {
                                                    None
                                                }
                                            };

                                            (handle, section.to_string())
                                        } else {
                                            (None, "none".to_string())
                                        };

                                        Ok((
                                            Arc::new(npz_data),
                                            radar_config,
                                            image_handle,
                                            section_name,
                                            config_error,
                                        ))
                                    }
                                    Err(e) => Err(format!("Failed to load NPZ: {}", e)),
                                }
                            },
                            move |result| {
                                match result {
                                    Ok((
                                        npz_data,
                                        radar_config,
                                        image_handle,
                                        section_name,
                                        config_error,
                                    )) => {
                                        // Log any radar config errors
                                        if let Some(err) = config_error {
                                            eprintln!("WARNING: {}", err);
                                        }

                                        // Log success info
                                        if radar_config.is_some() {
                                            eprintln!(
                                                "Loaded radar config for map: {} (section: {})",
                                                npz_data.map_name, section_name
                                            );
                                        } else {
                                            eprintln!("Using dynamic bounds for map: {} (no radar config)", npz_data.map_name);
                                        }

                                        // This will be handled by a new message
                                        Message::NpzDataLoaded(
                                            index,
                                            npz_data,
                                            radar_config,
                                            image_handle,
                                        )
                                    }
                                    Err(e) => {
                                        // Log error
                                        Message::NpzLoadError(e)
                                    }
                                }
                            },
                        );
                    }
                }
                Command::none()
            }

            // Misc
            Message::Event(event) => selection::handle_event(self, event),
            Message::Noop => Command::none(), // Do nothing for read-only text inputs
        }
    }
}
