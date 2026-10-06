use crate::app::BrowserApp;
use crate::message::Message;
use crate::models::CollectionEntry;
use demoparser::ProgressEvent;
use iced::Command;
use std::collections::{HashMap, HashSet};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

/// Handle request to parse selected collections
pub fn handle_parse_collections(app: &mut BrowserApp) -> Command<Message> {
    if app.is_parsing {
        return Command::none();
    }

    let selected_collections: Vec<&CollectionEntry> = app
        .loaded_collections
        .iter()
        .filter(|c| c.selected)
        .collect();

    if selected_collections.is_empty() {
        return app.log("No collections selected".to_string());
    }

    let mut filter_map: HashMap<std::path::PathBuf, Vec<u32>> = HashMap::new();
    let mut process_paths_set: HashSet<std::path::PathBuf> = HashSet::new();

    let mut skipped_count = 0;
    let mut added_count = 0;

    for entry in selected_collections {
        // If TickData == 1 and not overwrite, skip
        if entry.tick_data == 1 && !app.parser_overwrite {
            skipped_count += 1;
            continue;
        }

        let folder_name = entry.folder();
        let dir_opt = app
            .demo_directories
            .iter()
            .find(|d| d.folder_name() == folder_name);

        if let Some(dir) = dir_opt {
            let base_path = dir.path.join(&entry.demo_name);
            let mut full_path = base_path.clone();

            // Check if file exists, if not try common extensions
            if !full_path.exists() {
                // Try .dem
                let dem_path = dir.path.join(format!("{}.dem", entry.demo_name));
                if dem_path.exists() {
                    full_path = dem_path;
                } else {
                    // Try .dem.gz
                    let gz_path = dir.path.join(format!("{}.dem.gz", entry.demo_name));
                    if gz_path.exists() {
                        full_path = gz_path;
                    } else {
                        // Try .dem.zst
                        let zst_path = dir.path.join(format!("{}.dem.zst", entry.demo_name));
                        if zst_path.exists() {
                            full_path = zst_path;
                        }
                    }
                }
            }

            if full_path.exists() {
                process_paths_set.insert(full_path.clone());
                filter_map
                    .entry(full_path)
                    .or_default()
                    .push(entry.collection_num as u32);
                added_count += 1;
            } else {
                println!("Warning: Path not found, skipping: {}", base_path.display());
            }
        } else {
            // Cannot modify app AND log safely in loop if using simple logging
            // But log puts into a queue.
            // Since app is mutable ref, we can't call self.log multiple times if we iterate?
            // Actually log returns a Command.
            // We'll just print to stdout for warnings or accumulate messages.
            println!(
                "Warning: Could not find configured directory for folder: {}",
                folder_name
            );
        }
    }

    if added_count == 0 {
        return app.log(format!(
            "No collections to process (skipped {} already parsed)",
            skipped_count
        ));
    }

    let start_msg = app.log(format!(
        "Starting processing of {} collections (skipped {})",
        added_count, skipped_count
    ));

    app.is_parsing = true;
    app.parsing_directory = None;
    app.parsing_inputs = Some(process_paths_set.into_iter().collect());
    app.parsing_filter = Some(filter_map);
    app.parsing_cancel_token = Some(Arc::new(AtomicBool::new(false)));

    start_msg
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
    app.parsing_cancel_token = Some(Arc::new(AtomicBool::new(false)));

    // Initial log message
    app.log(app.parsing_status.clone())
}

/// Handle CancelParsing message
pub fn handle_cancel_parsing(app: &mut BrowserApp) -> Command<Message> {
    if let Some(token) = &app.parsing_cancel_token {
        token.store(true, Ordering::Relaxed);
        // Append *CANCELING* to current status instead of replacing
        app.parsing_status = format!("{} *CANCELING*", app.parsing_status);
        app.log("Parsing canceled. Finishing current batch...".to_string())
    } else {
        Command::none()
    }
}

/// Handle parsing progress events
pub fn handle_parsing_event(app: &mut BrowserApp, event: ProgressEvent) -> Command<Message> {
    match event {
        ProgressEvent::Started {
            total_files,
            total_batches,
        } => {
            app.parsing_progress = 0.0;
            app.parsing_status = format!(
                "Starting processing of {} files ({} batches)...",
                total_files, total_batches
            );
            app.total_demos = total_files;
            app.total_batches = total_batches;
            app.processed_demos = 0;
            app.current_batch = 0;
            app.batch_progress = 0.0;

            app.col_current = 0;
            app.col_total = 0;
            app.tick_current = 0;
            app.tick_total = 0;

            app.log(app.parsing_status.clone())
        }
        ProgressEvent::BatchStarted {
            batch_num,
            total_batches,
            file_count: _,
        } => {
            app.parsing_status = format!(
                "Processing Demos {}/{} - Batch {}/{}...",
                app.processed_demos, app.total_demos, batch_num, total_batches
            );
            app.current_batch = batch_num;
            // Reset batch unzip counter
            app.batch_unzipped_count = 0;
            app.log(app.parsing_status.clone())
        }
        ProgressEvent::BatchCompleted {
            batch_num,
            total_batches,
            demos_count,
            elapsed,
            ..
        } => {
            let mut msg = format!("Processed {} demos in {:.2} seconds", demos_count, elapsed);

            // Add unzip summary if any demos were unzipped
            if app.batch_unzipped_count > 0 {
                let dest = if app.unzip_dir_path.is_empty() {
                    "source folder".to_string()
                } else {
                    app.unzip_dir_path.clone()
                };
                msg = format!(
                    "{}. Unzipped {} demos to {}",
                    msg, app.batch_unzipped_count, dest
                );
            }

            if total_batches > 0 {
                app.batch_progress = batch_num as f32 / total_batches as f32;
            }

            app.col_current = 0;
            app.col_total = 0;
            app.tick_current = 0;
            app.tick_total = 0;

            let log_cmd = app.log(msg);

            Command::batch(vec![
                log_cmd,
                Command::perform(async {}, |_| Message::RefreshDatabases),
            ])
        }
        ProgressEvent::DemoProcessed { filename: _ } => {
            app.processed_demos += 1;
            Command::none()
        }
        ProgressEvent::PipelineMetrics {
            completed_demos,
            skipped_demos,
            elapsed,
            final_sample,
            ..
        } => {
            let label = if final_sample {
                "Full pipeline"
            } else {
                "Pipeline so far"
            };
            let rate = demoparser::pipeline_metrics::seconds_per_demo(elapsed, completed_demos)
                .map(|value| format!("{:.3}", value))
                .unwrap_or_else(|| "n/a".into());
            app.log(format!(
                "{}: {} seconds/demo ({} completed, {} skipped)",
                label, rate, completed_demos, skipped_demos
            ))
        }
        ProgressEvent::PhaseCompleted { .. } => Command::none(),
        ProgressEvent::Error { path, error } => {
            let msg = format!("Error processing {}: {}", path.display(), error);
            app.log(msg)
        }
        ProgressEvent::Finished {
            total_successful,
            total_failed,
            elapsed,
        } => {
            app.is_parsing = false;
            app.parsing_directory = None;
            app.parsing_inputs = None;
            app.parsing_filter = None;
            app.parsing_cancel_token = None;

            app.batch_progress = 0.0;
            app.processed_demos = 0;
            app.total_demos = 0;
            app.col_current = 0;
            app.col_total = 0;
            app.tick_current = 0;
            app.tick_total = 0;

            let msg = format!(
                "Parsing completed in {:.2}s. Success: {}, Failed: {}",
                elapsed, total_successful, total_failed
            );
            let log_cmd = app.log(msg);

            Command::batch(vec![
                log_cmd,
                Command::perform(async {}, |_| Message::RefreshDatabases),
            ])
        }
        ProgressEvent::UnzippedFile { filename: _ } => {
            // Just count, don't log individual files - summary shown in BatchCompleted
            app.batch_unzipped_count += 1;
            Command::none()
        }
        ProgressEvent::CollectionWritingProgress { completed, total } => {
            if completed > app.col_current {
                app.col_current = completed;
            }
            app.col_total = total;
            Command::none()
        }
        ProgressEvent::TickWritingProgress { completed, total } => {
            if completed > app.tick_current {
                app.tick_current = completed;
            }
            app.tick_total = total;
            Command::none()
        }
    }
}
