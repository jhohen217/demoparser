//! Selection and row interaction handlers

use crate::app::BrowserApp;
use crate::message::Message;
use crate::models::CollectionEntry;
use iced::{keyboard, mouse, Command, Event};

/// Handle ToggleSelectAll message
pub fn handle_toggle_select_all(app: &mut BrowserApp, selected: bool) -> Command<Message> {
    // Clear directory selection when selecting collections
    app.selected_directory_index = None;

    app.select_all = selected;
    for collection in &mut app.loaded_collections {
        collection.selected = selected;
    }
    Command::none()
}

/// Handle ToggleSelect message (checkbox toggle)
pub fn handle_toggle_select(
    app: &mut BrowserApp,
    index: usize,
    selected: bool,
) -> Command<Message> {
    if let Some(collection) = app.loaded_collections.get_mut(index) {
        collection.selected = selected;

        // Trigger detail loading if selected and details missing
        if selected && !collection.details_loaded {
            // If TickData is 1, we know details are available, so load them
            // If TickData is -1 (unknown/legacy), also try to load
            // If TickData is 0, there are no details, so mark as loaded but empty
            if collection.tick_data == 0 {
                collection.details_loaded = true;
            } else {
                // Load details if TickData is 1 (available) or -1 (unknown/legacy)
                let manager = app.db_manager.clone();
                let entry = collection.clone();
                return Command::perform(
                    async move { manager.load_details(entry).map_err(|e| e.to_string()) },
                    move |res| Message::DetailsLoaded(index, res),
                );
            }
        }
    }
    app.last_selected_index = Some(index);
    Command::none()
}

/// Handle DeselectAllCollections message
pub fn handle_deselect_all_collections(app: &mut BrowserApp) -> Command<Message> {
    app.select_all = false;
    for collection in &mut app.loaded_collections {
        collection.selected = false;
    }
    app.last_selected_index = None;
    Command::none()
}

/// Handle SelectRow message (row click with modifier key support)
pub fn handle_select_row(app: &mut BrowserApp, index: usize) -> Command<Message> {
    // Clear directory selection when selecting collections (mutual exclusivity)
    app.selected_directory_index = None;

    // Only clear radar if we're doing a single-select (not ctrl/shift/alt) and selecting a different entry
    let should_clear_radar = !app.ctrl_pressed
        && !app.shift_pressed
        && !app.alt_pressed
        && app.last_selected_index != Some(index);

    if should_clear_radar {
        // Clear radar view immediately to prevent showing stale data from previous selection
        app.radar_view.set_npz_data(None);
        app.radar_view.set_radar_config(None);
        app.radar_view.set_radar_image(None);
        app.playbar_state.total_ticks = 0;
        app.playbar_state.current_tick_idx = 0;
        app.playbar_state.playing = false;
    }

    if app.alt_pressed {
        // Alt + Click: Range deselect
        if let Some(last_idx) = app.last_selected_index {
            let start = std::cmp::min(last_idx, index);
            let end = std::cmp::max(last_idx, index);

            for i in start..=end {
                if let Some(collection) = app.loaded_collections.get_mut(i) {
                    collection.selected = false;
                }
            }
        } else {
            if let Some(collection) = app.loaded_collections.get_mut(index) {
                collection.selected = false;
            }
        }
    } else if app.shift_pressed {
        // Shift + Click: Range select
        if let Some(last_idx) = app.last_selected_index {
            let start = std::cmp::min(last_idx, index);
            let end = std::cmp::max(last_idx, index);

            for i in start..=end {
                if let Some(collection) = app.loaded_collections.get_mut(i) {
                    collection.selected = true;
                }
            }
        } else {
            if let Some(collection) = app.loaded_collections.get_mut(index) {
                collection.selected = true;
            }
        }
    } else if app.ctrl_pressed {
        // Ctrl + Click: Toggle selection without clearing others
        if let Some(collection) = app.loaded_collections.get_mut(index) {
            collection.selected = !collection.selected;
        }
    } else {
        // Single select - clear others
        for collection in &mut app.loaded_collections {
            collection.selected = false;
        }

        if let Some(collection) = app.loaded_collections.get_mut(index) {
            collection.selected = true;
        }
    }

    // Trigger detail loading if selected and details missing
    if let Some(collection) = app.loaded_collections.get(index) {
        if collection.selected && !collection.details_loaded {
            // If TickData is 0, we know there are no details, so mark as loaded but empty
            if collection.tick_data == 0 {
                // We need to get a mutable reference to update details_loaded
                if let Some(collection) = app.loaded_collections.get_mut(index) {
                    collection.details_loaded = true;
                }
            } else {
                // Load details if TickData is 1 (available) or -1 (unknown/legacy)
                let manager = app.db_manager.clone();
                let entry = collection.clone();
                app.last_selected_index = Some(index);
                return Command::perform(
                    async move { manager.load_details(entry).map_err(|e| e.to_string()) },
                    move |res| Message::DetailsLoaded(index, res),
                );
            }
        } else if collection.selected && collection.details_loaded && collection.tick_data == 1 {
            // Details already loaded, but we still need to (re)load NPZ data for radar
            app.last_selected_index = Some(index);
            return Command::perform(async {}, move |_| Message::LoadNpzData(index));
        }
    }

    app.last_selected_index = Some(index);
    Command::none()
}

/// Handle NavigateUp message (arrow up key)
pub fn handle_navigate_up(app: &mut BrowserApp) -> Command<Message> {
    if app.loaded_collections.is_empty() {
        return Command::none();
    }

    // Find the first selected item
    let current_index = app
        .loaded_collections
        .iter()
        .position(|c| c.selected)
        .or(app.last_selected_index);

    if let Some(idx) = current_index {
        if idx > 0 {
            return handle_select_row(app, idx - 1);
        }
    } else if !app.loaded_collections.is_empty() {
        // Select the last item if nothing is selected
        return handle_select_row(app, app.loaded_collections.len() - 1);
    }

    Command::none()
}

/// Handle NavigateDown message (arrow down key)
pub fn handle_navigate_down(app: &mut BrowserApp) -> Command<Message> {
    if app.loaded_collections.is_empty() {
        return Command::none();
    }

    // Find the first selected item
    let current_index = app
        .loaded_collections
        .iter()
        .position(|c| c.selected)
        .or(app.last_selected_index);

    if let Some(idx) = current_index {
        if idx < app.loaded_collections.len() - 1 {
            return handle_select_row(app, idx + 1);
        }
    } else {
        // Select the first item if nothing is selected
        return handle_select_row(app, 0);
    }

    Command::none()
}

/// Handle NavigateLeft message (arrow left key) - navigate to previous collection_num for same demo
pub fn handle_navigate_left(app: &mut BrowserApp) -> Command<Message> {
    if app.loaded_collections.is_empty() {
        return Command::none();
    }

    // Find the currently selected collection
    let current_index = app
        .loaded_collections
        .iter()
        .position(|c| c.selected)
        .or(app.last_selected_index);

    if let Some(current_idx) = current_index {
        let current = &app.loaded_collections[current_idx];
        let current_demo_name = &current.demo_name;
        let current_collection_num = current.collection_num;

        // Find all collections with same demo_name, with lower collection_num
        let mut candidates: Vec<(usize, i32)> = app
            .loaded_collections
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                c.demo_name == *current_demo_name && c.collection_num < current_collection_num
            })
            .map(|(idx, c)| (idx, c.collection_num))
            .collect();

        // Sort by collection_num descending to get the closest lower number
        candidates.sort_by(|a, b| b.1.cmp(&a.1));

        // Select the highest collection_num that's lower than current
        if let Some((target_idx, _)) = candidates.first() {
            return handle_select_row(app, *target_idx);
        }
    }

    Command::none()
}

/// Handle NavigateRight message (arrow right key) - navigate to next collection_num for same demo
pub fn handle_navigate_right(app: &mut BrowserApp) -> Command<Message> {
    if app.loaded_collections.is_empty() {
        return Command::none();
    }

    // Find the currently selected collection
    let current_index = app
        .loaded_collections
        .iter()
        .position(|c| c.selected)
        .or(app.last_selected_index);

    if let Some(current_idx) = current_index {
        let current = &app.loaded_collections[current_idx];
        let current_demo_name = &current.demo_name;
        let current_collection_num = current.collection_num;

        // Find all collections with same demo_name, with higher collection_num
        let mut candidates: Vec<(usize, i32)> = app
            .loaded_collections
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                c.demo_name == *current_demo_name && c.collection_num > current_collection_num
            })
            .map(|(idx, c)| (idx, c.collection_num))
            .collect();

        // Sort by collection_num ascending to get the closest higher number
        candidates.sort_by(|a, b| a.1.cmp(&b.1));

        // Select the lowest collection_num that's higher than current
        if let Some((target_idx, _)) = candidates.first() {
            return handle_select_row(app, *target_idx);
        }
    }

    Command::none()
}

/// Handle DetailsLoaded message
pub fn handle_details_loaded(
    app: &mut BrowserApp,
    index: usize,
    result: Result<CollectionEntry, String>,
) -> Command<Message> {
    match result {
        Ok(updated_entry) => {
            // Verify identity before updating to avoid race conditions with sorting
            if let Some(collection) = app.loaded_collections.get_mut(index) {
                if collection.steam_id == updated_entry.steam_id
                    && collection.demo_name == updated_entry.demo_name
                    && collection.round == updated_entry.round
                {
                    *collection = updated_entry;
                    // Restore selection state as it might have been lost in the clone/update
                    collection.selected = true;

                    // Trigger NPZ loading for radar view if tick data is available
                    if collection.tick_data == 1 {
                        return Command::perform(async {}, move |_| Message::LoadNpzData(index));
                    }
                }
            }
        }
        Err(e) => return app.log(format!("Failed to load details: {}", e)),
    }
    Command::none()
}

/// Handle keyboard and mouse Event messages
pub fn handle_event(app: &mut BrowserApp, event: Event) -> Command<Message> {
    match event {
        Event::Keyboard(keyboard::Event::KeyPressed { key, .. }) => match key {
            keyboard::Key::Named(keyboard::key::Named::Shift) => app.shift_pressed = true,
            keyboard::Key::Named(keyboard::key::Named::Alt) => app.alt_pressed = true,
            keyboard::Key::Named(keyboard::key::Named::Control) => app.ctrl_pressed = true,
            keyboard::Key::Named(keyboard::key::Named::ArrowUp) => {
                return Command::perform(async {}, |_| Message::NavigateUp);
            }
            keyboard::Key::Named(keyboard::key::Named::ArrowDown) => {
                return Command::perform(async {}, |_| Message::NavigateDown);
            }
            keyboard::Key::Named(keyboard::key::Named::ArrowLeft) => {
                return Command::perform(async {}, |_| Message::NavigateLeft);
            }
            keyboard::Key::Named(keyboard::key::Named::ArrowRight) => {
                return Command::perform(async {}, |_| Message::NavigateRight);
            }
            _ => {}
        },
        Event::Keyboard(keyboard::Event::KeyReleased { key, .. }) => match key {
            keyboard::Key::Named(keyboard::key::Named::Shift) => app.shift_pressed = false,
            keyboard::Key::Named(keyboard::key::Named::Alt) => app.alt_pressed = false,
            keyboard::Key::Named(keyboard::key::Named::Control) => app.ctrl_pressed = false,
            _ => {}
        },
        Event::Mouse(mouse::Event::CursorMoved { position }) => {
            if app.is_dragging_separator {
                if let (Some(start_y), Some(start_height)) =
                    (app.drag_start_y, app.drag_start_height)
                {
                    let delta = start_y - position.y;
                    let new_height = (start_height + delta).max(50.0).min(800.0);
                    app.bottom_panel_height = new_height;
                } else {
                    app.drag_start_y = Some(position.y);
                    app.drag_start_height = Some(app.bottom_panel_height);
                }
            }
        }
        Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
            if app.is_dragging_separator {
                app.is_dragging_separator = false;
                app.drag_start_y = None;
                app.drag_start_height = None;
            }
        }
        _ => {}
    }
    Command::none()
}
