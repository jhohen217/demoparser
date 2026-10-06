//! Directory management handlers

use crate::app::BrowserApp;
use crate::database::DatabaseManager;
use crate::message::Message;
use crate::models::DemoDirectory;
use iced::Command;
use std::path::PathBuf;

/// Normalize parser output path - removes KillCollections from end if present
fn normalize_parser_output_path(selected_path: PathBuf) -> PathBuf {
    // If path ends with "KillCollections", go up one level
    if selected_path.file_name().and_then(|n| n.to_str()) == Some("KillCollections") {
        if let Some(parent) = selected_path.parent() {
            return parent.to_path_buf();
        }
    }

    // If path contains a KillCollections subdirectory, return the selected path as-is
    // Otherwise return as-is (parser will create KillCollections)
    selected_path
}

/// Handle BrowseParserOutput message - opens folder picker
pub fn handle_browse_parser_output(_app: &mut BrowserApp) -> Command<Message> {
    Command::perform(
        async {
            rfd::AsyncFileDialog::new()
                .set_title("Select Parser Output Directory")
                .pick_folder()
                .await
                .map(|handle| handle.path().to_path_buf())
        },
        Message::ParserOutputSelected,
    )
}

/// Handle ParserOutputSelected message - validates and updates config
pub fn handle_parser_output_selected(
    app: &mut BrowserApp,
    path_opt: Option<PathBuf>,
) -> Command<Message> {
    if let Some(path) = path_opt {
        // Normalize the path (remove KillCollections if at end)
        let normalized_path = normalize_parser_output_path(path);

        // Update config
        app.config.parser_output = normalized_path.clone();
        app.parser_output_path = normalized_path.to_string_lossy().to_string();

        // Save config
        if let Err(e) = app.config.save() {
            return app.log(format!("ERROR: Failed to save config: {}", e));
        }

        // Update database manager to use new path
        let master_dir = app.config.kill_collection_master_dir();
        app.db_manager = DatabaseManager::new(master_dir.clone());

        // Log the change
        let log_cmd = app.log(format!(
            "Parser output path updated to: {}",
            app.parser_output_path
        ));

        // Trigger refresh to scan new location
        Command::batch(vec![
            log_cmd,
            Command::perform(async {}, |_| Message::RefreshDatabases),
        ])
    } else {
        Command::none()
    }
}

/// Handle ParserOutputChanged message - manual text field edit
pub fn handle_parser_output_changed(app: &mut BrowserApp, value: String) -> Command<Message> {
    app.parser_output_path = value.clone();

    // Update config with the new path
    app.config.parser_output = PathBuf::from(value);

    // Save config
    if let Err(e) = app.config.save() {
        return app.log(format!("ERROR: Failed to save config: {}", e));
    }

    // Update database manager
    let master_dir = app.config.kill_collection_master_dir();
    app.db_manager = DatabaseManager::new(master_dir);

    Command::none()
}

/// Handle BrowseUnzipDir message - opens folder picker
pub fn handle_browse_unzip_dir(_app: &mut BrowserApp) -> Command<Message> {
    Command::perform(
        async {
            rfd::AsyncFileDialog::new()
                .set_title("Select Unzip Directory (Optional SSD location)")
                .pick_folder()
                .await
                .map(|handle| handle.path().to_path_buf())
        },
        Message::UnzipDirSelected,
    )
}

/// Handle UnzipDirSelected message - validates and updates config
pub fn handle_unzip_dir_selected(
    app: &mut BrowserApp,
    path_opt: Option<PathBuf>,
) -> Command<Message> {
    if let Some(path) = path_opt {
        // Update config
        app.config.unzip_dir = Some(path.clone());
        app.unzip_dir_path = path.to_string_lossy().to_string();

        // Save config
        if let Err(e) = app.config.save() {
            return app.log(format!("ERROR: Failed to save config: {}", e));
        }

        // Log the change
        app.log(format!("Unzip directory set to: {}", app.unzip_dir_path))
    } else {
        Command::none()
    }
}

/// Handle UnzipDirChanged message - manual text field edit
pub fn handle_unzip_dir_changed(app: &mut BrowserApp, value: String) -> Command<Message> {
    app.unzip_dir_path = value.clone();

    // Update config with the new path (empty string = None)
    app.config.unzip_dir = if value.is_empty() {
        None
    } else {
        Some(PathBuf::from(value))
    };

    // Save config
    if let Err(e) = app.config.save() {
        return app.log(format!("ERROR: Failed to save config: {}", e));
    }

    Command::none()
}

/// Handle ToggleRamUnzip message - toggle RAM decompression mode
pub fn handle_toggle_ram_unzip(app: &mut BrowserApp, enabled: bool) -> Command<Message> {
    app.ram_unzip = enabled;
    app.config.ram_unzip = enabled;

    // Save config
    if let Err(e) = app.config.save() {
        return app.log(format!("ERROR: Failed to save config: {}", e));
    }

    // Log the change
    let status = if enabled { "enabled" } else { "disabled" };
    app.log(format!("RAM Unzip {}", status))
}

/// Handle AddDemoDirectory message - opens file dialog
pub fn handle_add(_app: &mut BrowserApp) -> Command<Message> {
    Command::perform(
        async {
            rfd::AsyncFileDialog::new()
                .set_title("Select Demo Directory")
                .pick_folder()
                .await
                .map(|handle| handle.path().to_path_buf())
        },
        Message::DirectorySelected,
    )
}

/// Handle DirectorySelected message - adds the selected directory
pub fn handle_selected(app: &mut BrowserApp, path_opt: Option<PathBuf>) -> Command<Message> {
    if let Some(path) = path_opt {
        // Add the main directory
        let new_dir = DemoDirectory::new(path.clone());

        // Skip if already exists
        if !app.demo_directories.iter().any(|d| d.path == path) {
            app.demo_directories.push(new_dir);

            // Save config
            app.config.demo_directories = app.demo_directories.clone();
            if let Err(e) = app.config.save() {
                eprintln!("Failed to save config: {}", e);
            }

            // Trigger RefreshDatabases which serializes all database operations
            return Command::perform(async {}, |_| Message::RefreshDatabases);
        }
    }
    Command::none()
}

/// Handle RemoveDemoDirectory message
pub fn handle_remove(app: &mut BrowserApp) -> Command<Message> {
    if let Some(idx) = app.selected_directory_index {
        if idx < app.demo_directories.len() {
            app.demo_directories.remove(idx);
            app.selected_directory_index = None;

            // Save config
            app.config.demo_directories = app.demo_directories.clone();
            if let Err(e) = app.config.save() {
                eprintln!("Failed to save config: {}", e);
            }

            // Auto-refresh after removal
            return Command::perform(async {}, |_| Message::RefreshDatabases);
        }
    }
    Command::none()
}

/// Handle DeselectAllDirectories message
pub fn handle_deselect_all_directories(app: &mut BrowserApp) -> Command<Message> {
    app.selected_directory_index = None;
    app.last_selected_directory_index = None;
    Command::none()
}

/// Handle SelectDirectoryRow message with modifier key support
pub fn handle_select_row(app: &mut BrowserApp, index: usize) -> Command<Message> {
    // Clear collection selections when selecting a directory (mutual exclusivity)
    app.select_all = false;
    for collection in &mut app.loaded_collections {
        collection.selected = false;
    }
    app.last_selected_index = None;

    if app.alt_pressed {
        // Alt + Click: Range deselect
        if let Some(last_idx) = app.last_selected_directory_index {
            let start = std::cmp::min(last_idx, index);
            let end = std::cmp::max(last_idx, index);

            for i in start..=end {
                if i == index {
                    app.selected_directory_index = None;
                }
            }
        } else {
            app.selected_directory_index = None;
        }
    } else if app.shift_pressed {
        // Shift + Click: Range select (just select the clicked one for directories)
        app.selected_directory_index = Some(index);
    } else if app.ctrl_pressed {
        // Ctrl + Click: Toggle selection
        if app.selected_directory_index == Some(index) {
            app.selected_directory_index = None;
        } else {
            app.selected_directory_index = Some(index);
        }
    } else {
        // Normal click: Single select
        app.selected_directory_index = Some(index);
    }

    app.last_selected_directory_index = Some(index);
    Command::none()
}

/// Handle ToggleDirectoryEnabled message
pub fn handle_toggle_enabled(app: &mut BrowserApp, idx: usize, enabled: bool) -> Command<Message> {
    if let Some(dir) = app.demo_directories.get_mut(idx) {
        dir.enabled = enabled;

        // Save config
        app.config.demo_directories = app.demo_directories.clone();
        if let Err(e) = app.config.save() {
            eprintln!("Failed to save config: {}", e);
        }

        // Immediately filter and reload collections without full database rescan
        let filtered_dbs = app.get_filtered_databases();
        let manager = app.db_manager.clone();
        let filter_tick_data = app.filter_tick_data;
        return Command::perform(
            async move {
                manager
                    .load_collections(&filtered_dbs, filter_tick_data)
                    .map_err(|e| e.to_string())
            },
            |result| Message::CollectionsLoaded(result, false),
        );
    }
    Command::none()
}

/// Handle OpenDirectoryInExplorer message
pub fn handle_open_in_explorer(app: &mut BrowserApp) -> Command<Message> {
    // Open the first enabled directory in file explorer
    if let Some(dir) = app.demo_directories.iter().find(|d| d.enabled) {
        let path = dir.path.clone();
        return Command::perform(
            async move {
                #[cfg(target_os = "windows")]
                {
                    std::process::Command::new("explorer")
                        .arg(path)
                        .spawn()
                        .ok();
                }
                #[cfg(target_os = "macos")]
                {
                    std::process::Command::new("open").arg(path).spawn().ok();
                }
                #[cfg(target_os = "linux")]
                {
                    std::process::Command::new("xdg-open")
                        .arg(path)
                        .spawn()
                        .ok();
                }
            },
            |_| Message::RefreshDatabases,
        );
    }
    Command::none()
}

/// Handle DirectoryScanned message
pub fn handle_scanned(
    app: &mut BrowserApp,
    idx: usize,
    result: Result<(usize, usize, usize), String>,
) -> Command<Message> {
    match result {
        Ok((count, _archived, _unarchived)) => {
            if let Some(dir) = app.demo_directories.get_mut(idx) {
                dir.valid_file_count = count;
            }
            // Don't log individual directory scans to reduce console clutter
            // The information is shown in the UI directory list
            Command::none()
        }
        Err(e) => app.log(format!("ERROR: Failed to scan directory: {}", e)),
    }
}

/// Handle AllDirectoryStatsCounted message
pub fn handle_all_stats_counted(
    app: &mut BrowserApp,
    result: Result<Vec<(String, usize, usize, usize)>, String>,
) -> Command<Message> {
    match result {
        Ok(stats) => {
            // Update each directory with its stats
            for (folder, processed_demos, tick_enabled, total_collections) in stats {
                // Find the directory by folder name
                for dir in &mut app.demo_directories {
                    if dir.folder_name() == folder {
                        dir.processed_demo_count = processed_demos;
                        dir.tick_data_enabled_count = Some(tick_enabled);
                        dir.total_collections_count = Some(total_collections);
                        break;
                    }
                }
            }
            app.log("Directory statistics counted".to_string())
        }
        Err(e) => app.log(format!("ERROR: Failed to count directory stats: {}", e)),
    }
}

/// Handle ParseDirectory message - launch the parser for the selected directory
pub fn handle_parse_directory(app: &mut BrowserApp) -> Command<Message> {
    // Get the selected directory
    let selected_idx = match app.selected_directory_index {
        Some(idx) => idx,
        None => {
            return app.log("ERROR: No directory selected".to_string());
        }
    };

    let selected_dir = match app.demo_directories.get(selected_idx) {
        Some(dir) => dir.clone(),
        None => {
            return app.log("ERROR: Selected directory not found".to_string());
        }
    };

    let folder_name = selected_dir.folder_name();
    let dir_path = selected_dir.path.clone();

    // Set parsing state
    app.is_parsing = true;
    app.parsing_directory = Some(dir_path);
    app.parsing_progress = 0.0;
    app.parsing_status = format!("Starting parse for directory: {}...", folder_name);

    // Initial log message
    app.log(app.parsing_status.clone())
}
