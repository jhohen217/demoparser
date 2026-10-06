//! Database and collection management handlers

use crate::app::BrowserApp;
use crate::database::DatabaseInfo;
use crate::message::Message;
use crate::models::{CollectionType, TickFilterState};
use crate::sorting::{sort_collections, SortColumn};
use iced::Command;

/// Handle RefreshDatabases message
pub fn handle_refresh(app: &mut BrowserApp) -> Command<Message> {
    // First, only scan for demo files in directories (no database access)
    let mut commands: Vec<Command<Message>> = Vec::new();

    for (idx, dir) in app.demo_directories.iter().enumerate() {
        let dir_path = dir.path.clone();
        let recursive = dir.recursive;

        // Scan for demo files only (no database access, so safe to run in parallel)
        let manager_scan = app.db_manager.clone();
        commands.push(Command::perform(
            async move {
                manager_scan
                    .scan_directory_for_demos(&dir_path, recursive)
                    .map(|files| {
                        let total = files.len();
                        let mut archived = 0;
                        let mut unarchived = 0;
                        for f in &files {
                            if let Some(ext) = f.extension().and_then(|s| s.to_str()) {
                                if ext.eq_ignore_ascii_case("dem") {
                                    unarchived += 1;
                                } else if ext.eq_ignore_ascii_case("gz")
                                    || ext.eq_ignore_ascii_case("zst")
                                {
                                    archived += 1;
                                }
                            }
                        }
                        (total, archived, unarchived)
                    })
                    .map_err(|e| e.to_string())
            },
            move |result| Message::DirectoryScanned(idx, result),
        ));
    }

    // Then trigger database scan (which will chain to collection loading and then directory stats)
    let manager = app.db_manager.clone();
    commands.push(Command::perform(
        async move { manager.scan_databases().map_err(|e| e.to_string()) },
        Message::DatabasesScanned,
    ));

    Command::batch(commands)
}

/// Handle DatabasesScanned message
pub fn handle_scanned(
    app: &mut BrowserApp,
    result: Result<Vec<DatabaseInfo>, String>,
) -> Command<Message> {
    let cmd = match result {
        Ok(databases) => {
            let log_cmd = app.log(format!(
                "Found {} database files (before filtering)",
                databases.len()
            ));
            app.all_databases = databases;

            // Use get_filtered_databases() to apply BOTH directory AND type filters
            let filtered_dbs = app.get_filtered_databases();

            // Load tags from filtered databases
            if let Ok(tags) = app.db_manager.get_all_tags(&filtered_dbs) {
                app.available_tags = tags;
            }

            // Trigger collection load with filtered databases
            // Capture tick data filter state
            let filter_tick_data = app.filter_tick_data;
            let manager = app.db_manager.clone();
            Command::batch(vec![
                log_cmd,
                Command::perform(
                    async move {
                        manager
                            .load_collections(&filtered_dbs, filter_tick_data)
                            .map_err(|e| e.to_string())
                    },
                    |result| Message::CollectionsLoaded(result, true),
                ),
            ])
        }
        Err(e) => app.log(format!("ERROR: Failed to scan databases: {}", e)),
    };
    cmd
}

/// Handle CollectionsLoaded message
pub fn handle_collections_loaded(
    app: &mut BrowserApp,
    result: Result<Vec<crate::models::CollectionEntry>, String>,
    should_recalculate_directory_stats: bool,
) -> Command<Message> {
    match result {
        Ok(mut collections) => {
            // Preserve selection state: collect (demo_name, collection_num) of selected entries
            let selected_entries: std::collections::HashSet<(String, i32)> = app
                .loaded_collections
                .iter()
                .filter(|e| e.selected)
                .map(|e| (e.demo_name.clone(), e.collection_num))
                .collect();

            // Restore selection state in newly loaded collections
            for entry in &mut collections {
                if selected_entries.contains(&(entry.demo_name.clone(), entry.collection_num)) {
                    entry.selected = true;
                }
            }

            sort_collections(&mut collections, app.sort_column, app.sort_order);
            app.loaded_collections = collections;
            let log_cmd = app.log(format!(
                "Loaded {} collections",
                app.loaded_collections.len()
            ));

            // Only recalculate directory statistics if requested (e.g., on full refresh, not on filter changes)
            if should_recalculate_directory_stats {
                // Trigger directory statistics counting in a SINGLE synchronous call
                // This prevents race conditions by ensuring all database accesses are sequential
                // Also performs automatic cleanup of orphaned DuckDB entries when NPZ mismatches are detected
                let folders: Vec<String> = app
                    .demo_directories
                    .iter()
                    .map(|d| d.folder_name())
                    .collect();

                let manager = app.db_manager.clone();
                let parser_output = app.config.parser_output.clone();

                return Command::batch(vec![
                    log_cmd,
                    Command::perform(
                        async move {
                            manager
                                .count_all_directory_stats(&folders, &parser_output)
                                .map_err(|e| e.to_string())
                        },
                        Message::AllDirectoryStatsCounted,
                    ),
                ]);
            } else {
                return log_cmd;
            }
        }
        Err(e) => return app.log(format!("ERROR: Failed to load collections: {}", e)),
    }
}

/// Handle ToggleFilter message
pub fn handle_toggle_filter(
    app: &mut BrowserApp,
    coll_type: CollectionType,
    enabled: bool,
) -> Command<Message> {
    app.type_filters.insert(coll_type, enabled);

    // Reload collections without recalculating directory stats (type filters don't affect directory stats)
    let filtered_dbs = app.get_filtered_databases();
    let manager = app.db_manager.clone();
    let filter_tick_data = app.filter_tick_data;

    Command::perform(
        async move {
            manager
                .load_collections(&filtered_dbs, filter_tick_data)
                .map_err(|e| e.to_string())
        },
        |result| Message::CollectionsLoaded(result, false),
    )
}

/// Handle ToggleTickDataFilter message
pub fn handle_toggle_tick_data_filter(
    app: &mut BrowserApp,
    state: TickFilterState,
) -> Command<Message> {
    app.filter_tick_data = state;

    // Reload collections with new filter
    let filtered_dbs = app.get_filtered_databases();
    let manager = app.db_manager.clone();
    let filter_tick_data = app.filter_tick_data;

    Command::perform(
        async move {
            manager
                .load_collections(&filtered_dbs, filter_tick_data)
                .map_err(|e| e.to_string())
        },
        |result| Message::CollectionsLoaded(result, false),
    )
}

/// Handle Sort message
pub fn handle_sort(app: &mut BrowserApp, column: SortColumn) -> Command<Message> {
    if app.sort_column == Some(column) {
        app.sort_order = app.sort_order.toggle();
    } else {
        app.sort_column = Some(column);
        app.sort_order = crate::sorting::SortOrder::Descending;
    }
    sort_collections(&mut app.loaded_collections, app.sort_column, app.sort_order);
    Command::none()
}
