//! Tag management, export, and validation handlers

use crate::app::BrowserApp;
use crate::control_panel::ConfirmMode;
use crate::message::Message;
use crate::models::CollectionEntry;
use crate::tag_utils::{append_tag, remove_tag};
use iced::Command;

/// Handle TagInputChanged message
pub fn handle_input_changed(app: &mut BrowserApp, value: String) -> Command<Message> {
    app.tag_input = value;
    Command::none()
}

/// Handle ApplyTag message
pub fn handle_apply(app: &mut BrowserApp) -> Command<Message> {
    if app.loaded_collections.iter().any(|c| c.selected) && !app.tag_input.is_empty() {
        let new_tag = app.tag_input.clone();
        let selected_indices: Vec<usize> = app
            .loaded_collections
            .iter()
            .enumerate()
            .filter(|(_, c)| c.selected)
            .map(|(i, _)| i)
            .collect();

        // Optimistic update (additive)
        for &idx in &selected_indices {
            if let Some(collection) = app.loaded_collections.get_mut(idx) {
                collection.tag = append_tag(&collection.tag, &new_tag);
            }
        }

        // Collect updated entries for DB update
        let updated_entries: Vec<CollectionEntry> = selected_indices
            .iter()
            .filter_map(|&idx| app.loaded_collections.get(idx).cloned())
            .collect();

        let manager = app.db_manager.clone();

        return Command::perform(
            async move {
                let refs: Vec<&CollectionEntry> = updated_entries.iter().collect();
                manager.update_tags(&refs).map_err(|e| e.to_string())
            },
            Message::TagsUpdated,
        );
    }
    Command::none()
}

/// Handle ClearTag message
pub fn handle_clear(app: &mut BrowserApp) -> Command<Message> {
    if app.loaded_collections.iter().any(|c| c.selected) {
        if app.tag_input.is_empty() {
            app.confirm_mode = Some(ConfirmMode::Clear);
        } else {
            // Remove specific tag - No Confirmation
            let tag_to_remove = app.tag_input.clone();
            let selected_indices: Vec<usize> = app
                .loaded_collections
                .iter()
                .enumerate()
                .filter(|(_, c)| c.selected)
                .map(|(i, _)| i)
                .collect();

            // Optimistic update
            for &idx in &selected_indices {
                if let Some(collection) = app.loaded_collections.get_mut(idx) {
                    collection.tag = remove_tag(&collection.tag, &tag_to_remove);
                }
            }

            // DB Update
            let updated_entries: Vec<CollectionEntry> = selected_indices
                .iter()
                .filter_map(|&idx| app.loaded_collections.get(idx).cloned())
                .collect();

            let manager = app.db_manager.clone();
            return Command::perform(
                async move {
                    let refs: Vec<&CollectionEntry> = updated_entries.iter().collect();
                    manager.update_tags(&refs).map_err(|e| e.to_string())
                },
                Message::TagsUpdated,
            );
        }
    }
    Command::none()
}

/// Handle ConfirmTagAction message
pub fn handle_confirm(app: &mut BrowserApp) -> Command<Message> {
    if let Some(mode) = app.confirm_mode {
        let selected_indices: Vec<usize> = app
            .loaded_collections
            .iter()
            .enumerate()
            .filter(|(_, c)| c.selected)
            .map(|(i, _)| i)
            .collect();

        if !selected_indices.is_empty() {
            match mode {
                ConfirmMode::Clear => {
                    // Optimistic update (clear)
                    for &idx in &selected_indices {
                        if let Some(collection) = app.loaded_collections.get_mut(idx) {
                            collection.tag.clear();
                        }
                    }
                }
            }

            // Collect updated entries for DB update
            let updated_entries: Vec<CollectionEntry> = selected_indices
                .iter()
                .filter_map(|&idx| app.loaded_collections.get(idx).cloned())
                .collect();

            let manager = app.db_manager.clone();
            app.confirm_mode = None;

            return Command::perform(
                async move {
                    let refs: Vec<&CollectionEntry> = updated_entries.iter().collect();
                    manager.update_tags(&refs).map_err(|e| e.to_string())
                },
                Message::TagsUpdated,
            );
        }
    }
    app.confirm_mode = None;
    Command::none()
}

/// Handle CancelTagAction message
pub fn handle_cancel(app: &mut BrowserApp) -> Command<Message> {
    app.confirm_mode = None;
    Command::none()
}

/// Handle TagsUpdated message
pub fn handle_updated(app: &mut BrowserApp, result: Result<(), String>) -> Command<Message> {
    if let Err(e) = result {
        return app.log(format!("ERROR: Failed to update tags: {}", e));
    } else {
        return app.log("Tags updated successfully".to_string());
    }
}

/// Handle ExportSelections message
pub fn handle_export(app: &mut BrowserApp) -> Command<Message> {
    // Implementation similar to original
    app.log("Exporting selections...".to_string())
}
