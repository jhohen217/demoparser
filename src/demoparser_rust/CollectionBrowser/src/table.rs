use crate::message::Message;
use crate::models::CollectionEntry;
use crate::sorting::{SortColumn, SortOrder};
use crate::style;
use crate::theme::Theme;
use iced::widget::{button, column, container, mouse_area, row, scrollable, text, Space};
use iced::{Color, Element, Length, Padding};

/// Clean map name (remove de_, cs_ prefixes)
fn clean_map_name(map: &str) -> String {
    map.replace("de_", "").replace("cs_", "")
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
        entry.display_weapons(),
        entry.util_thrown,
        entry.tag
    )
    .to_lowercase();

    // Split by comma for AND logic
    let terms: Vec<&str> = search.split(',').map(|s| s.trim()).collect();

    // Entry matches if ALL terms match
    terms.iter().all(|term| {
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

pub fn view<'a>(
    collections: &'a [CollectionEntry],
    sort_column: Option<SortColumn>,
    sort_order: SortOrder,
    select_all: bool,
    filter_demo_names: bool,
    filter_demo_names_list: &[String],
    search_input: &str,
    num_filter_min: &str,
    num_filter_max: &str,
    num_filter_column: &str,
    num_filter_exclude: bool,
) -> Element<'a, Message, Theme, iced::Renderer> {
    // Apply demo name filter first (overrides other filters when active)
    // Track (original_index, entry_ref) to preserve correct indices for selection
    let mut filtered_collections: Vec<(usize, &CollectionEntry)> = if filter_demo_names {
        // Use the provided demo names list
        if filter_demo_names_list.is_empty() {
            // No demo names to filter, show nothing
            Vec::new()
        } else {
            collections
                .iter()
                .enumerate()
                .filter(|(_, entry)| filter_demo_names_list.contains(&entry.demo_name))
                .collect()
        }
    } else if search_input.trim().is_empty() {
        collections.iter().enumerate().collect()
    } else {
        collections
            .iter()
            .enumerate()
            .filter(|(_, entry)| matches_search(entry, search_input))
            .collect()
    };

    // Apply numerical range filter if min or max is set (unless demo name filter is active)
    if !filter_demo_names
        && (!num_filter_min.trim().is_empty() || !num_filter_max.trim().is_empty())
    {
        let min_val = num_filter_min.trim().parse::<f32>().ok();
        let max_val = num_filter_max.trim().parse::<f32>().ok();

        filtered_collections.retain(|(_, entry)| {
            let value = match num_filter_column {
                "Duration" => entry.tick_duration as f32 / 128.0, // Convert ticks to seconds
                "Killer R" => entry.killer_radius as f32,
                "Victim R" => entry.victims_radius as f32,
                "Moved" => entry.killer_move_distance as f32,
                "GameVer" => entry.game_version as f32,
                "Hits" => entry.hits as f32,
                "Misses" => entry.misses as f32,
                "Hit %" => (entry.hit_rate * 100.0) as f32,
                _ => return true, // Unknown column, don't filter
            };

            let in_range = match (min_val, max_val) {
                (Some(min), Some(max)) => value >= min && value <= max,
                (Some(min), None) => value >= min,
                (None, Some(max)) => value <= max,
                (None, None) => true,
            };

            // Apply exclude logic
            if num_filter_exclude {
                !in_range
            } else {
                in_range
            }
        });
    }
    // Header
    let header_checkbox_content = container(Space::new(Length::Fixed(6.0), Length::Fixed(6.0)))
        .width(Length::Fixed(6.0))
        .height(Length::Fixed(6.0))
        .style(if select_all {
            style::Container::CheckboxSquare(true)
        } else {
            style::Container::Transparent
        });

    let header_checkbox = button(
        container(header_checkbox_content)
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x()
            .center_y(),
    )
    .on_press(Message::ToggleSelectAll(!select_all))
    .style(style::Button::Checkbox(select_all, false, false))
    .width(Length::Fixed(11.0))
    .height(Length::Fixed(11.0))
    .padding(0);

    let header = row![
        container(header_checkbox)
            .width(Length::Fixed(11.0))
            .center_x()
            .padding(0), // Padding handled by row
        header_cell(
            SortColumn::Type,
            "TYPE",
            sort_column,
            sort_order,
            Length::Fixed(60.0)
        ),
        header_cell(
            SortColumn::Frag,
            "FRAG",
            sort_column,
            sort_order,
            Length::Fixed(50.0)
        ),
        header_cell(
            SortColumn::Player,
            "PLAYER",
            sort_column,
            sort_order,
            Length::Fixed(100.0)
        ),
        header_cell(
            SortColumn::Map,
            "MAP",
            sort_column,
            sort_order,
            Length::Fixed(60.0)
        ),
        header_cell(
            SortColumn::Duration,
            "DURATION",
            sort_column,
            sort_order,
            Length::Fixed(60.0)
        ),
        header_cell(
            SortColumn::KillerRadius,
            "KILLER R",
            sort_column,
            sort_order,
            Length::Fixed(60.0)
        ),
        header_cell(
            SortColumn::VictimRadius,
            "VICTIM R",
            sort_column,
            sort_order,
            Length::Fixed(60.0)
        ),
        header_cell(
            SortColumn::Moved,
            "MOVED",
            sort_column,
            sort_order,
            Length::Fixed(60.0)
        ),
        header_cell(
            SortColumn::GameVersion,
            "GAMEVER",
            sort_column,
            sort_order,
            Length::Fixed(60.0)
        ),
        header_cell(
            SortColumn::Hits,
            "HITS",
            sort_column,
            sort_order,
            Length::Fixed(40.0)
        ),
        header_cell(
            SortColumn::Misses,
            "MISS",
            sort_column,
            sort_order,
            Length::Fixed(40.0)
        ),
        header_cell(
            SortColumn::HitRate,
            "HIT %",
            sort_column,
            sort_order,
            Length::Fixed(60.0)
        ),
        header_cell(
            SortColumn::Weapons,
            "WEAPONS",
            sort_column,
            sort_order,
            Length::FillPortion(1)
        ),
        header_cell(
            SortColumn::Util,
            "UTIL",
            sort_column,
            sort_order,
            Length::FillPortion(1)
        ),
        header_cell(
            SortColumn::Tag,
            "TAG",
            sort_column,
            sort_order,
            Length::FillPortion(1)
        ),
    ]
    .spacing(0)
    .padding(Padding::from([0.0, 0.0, 0.0, 2.0]))
    .align_items(iced::Alignment::Center);

    // Rows content
    let rows_content: Element<'a, Message, Theme, iced::Renderer> = if filtered_collections
        .is_empty()
    {
        // Empty table - clickable area to deselect
        mouse_area(
            container(Space::new(Length::Fill, Length::Fill))
                .width(Length::Fill)
                .height(Length::Fill)
                .style(style::Container::Transparent),
        )
        .on_press(Message::DeselectAllCollections)
        .into()
    } else {
        column(
            filtered_collections
                .iter()
                .enumerate()
                .map(|(display_index, (original_index, c))| {
                    let is_selected = c.selected;
                    let is_alt = display_index % 2 != 0;
                    let checkbox_content =
                        container(Space::new(Length::Fixed(6.0), Length::Fixed(6.0)))
                            .width(Length::Fixed(6.0))
                            .height(Length::Fixed(6.0))
                            .style(if is_selected {
                                style::Container::CheckboxSquare(true)
                            } else {
                                style::Container::Transparent
                            });

                    let row_checkbox = button(
                        container(checkbox_content)
                            .width(Length::Fill)
                            .height(Length::Fill)
                            .center_x()
                            .center_y(),
                    )
                    .on_press(Message::ToggleSelect(*original_index, !is_selected))
                    .style(style::Button::Checkbox(is_selected, is_selected, is_alt))
                    .width(Length::Fixed(11.0))
                    .height(Length::Fixed(11.0))
                    .padding(0);

                    // Format frag display: "collection_num/col_total" or "collection_num/?"
                    let frag_display = c
                        .col_total
                        .filter(|&t| t > 0)
                        .map(|t| format!("{}/{}", c.collection_num, t))
                        .unwrap_or_else(|| format!("{}/?", c.collection_num));

                    let row_content = row![
                        container(row_checkbox)
                            .width(Length::Fixed(11.0))
                            .center_x(),
                        text_cell(&c.collection_type, Length::Fixed(60.0), is_selected),
                        text_cell(&frag_display, Length::Fixed(50.0), is_selected),
                        text_cell(&c.killer_name, Length::Fixed(100.0), is_selected),
                        text_cell(
                            &clean_map_name(&c.map_name),
                            Length::Fixed(60.0),
                            is_selected
                        ),
                        text_cell(&c.duration_display, Length::Fixed(60.0), is_selected),
                        text_cell(&c.killer_radius_display, Length::Fixed(60.0), is_selected),
                        text_cell(&c.victims_radius_display, Length::Fixed(60.0), is_selected),
                        text_cell(&c.move_distance_display, Length::Fixed(60.0), is_selected),
                        text_cell(
                            &c.game_version.to_string(),
                            Length::Fixed(60.0),
                            is_selected
                        ),
                        text_cell(&c.hits.to_string(), Length::Fixed(40.0), is_selected),
                        text_cell(&c.misses.to_string(), Length::Fixed(40.0), is_selected),
                        text_cell(&c.hit_rate_display, Length::Fixed(60.0), is_selected),
                        text_cell_left(c.display_weapons(), Length::FillPortion(1), is_selected),
                        text_cell_right(&c.util_thrown, Length::FillPortion(1), is_selected),
                        text_cell(&c.tag, Length::FillPortion(1), is_selected),
                    ]
                    .spacing(0)
                    .align_items(iced::Alignment::Center)
                    .padding(Padding::from([2.0, 0.0, 2.0, 3.0]));

                    button(row_content)
                        .on_press(Message::SelectRow(*original_index))
                        .style(style::Button::TableRow(is_selected, is_alt))
                        .width(Length::Fill)
                        .padding(0)
                        .into()
                })
                .collect::<Vec<_>>(),
        )
        .width(Length::Fill)
        .into()
    };

    column![
        container(header)
            .style(style::Container::Panel)
            .width(Length::Fill),
        scrollable(rows_content)
            .style(style::Scrollable)
            .height(Length::Fill)
            .width(Length::Fill),
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

fn header_cell<'a>(
    column: SortColumn,
    label: &str,
    sort_column: Option<SortColumn>,
    _sort_order: SortOrder,
    width: Length,
) -> Element<'a, Message, Theme, iced::Renderer> {
    let is_selected = sort_column == Some(column);

    let content = container(text(label).size(11))
        .width(Length::Fill)
        .height(Length::Fixed(18.0))
        .center_y()
        .center_x()
        .style(style::Container::Transparent)
        .clip(true);

    button(content)
        .on_press(Message::Sort(column))
        .style(style::Button::Header(is_selected))
        .width(width)
        .padding(0)
        .into()
}

fn text_cell<'a>(
    content: &str,
    width: Length,
    is_selected: bool,
) -> Element<'a, Message, Theme, iced::Renderer> {
    container(
        text(content)
            .shaping(text::Shaping::Advanced)
            .size(11)
            .style(if is_selected {
                Some(Color::BLACK)
            } else {
                None
            })
            .font(iced::Font {
                weight: if is_selected {
                    iced::font::Weight::Bold
                } else {
                    iced::font::Weight::Normal
                },
                ..Default::default()
            }),
    )
    .width(width)
    .height(Length::Fixed(16.0))
    .center_y()
    .center_x()
    .style(style::Container::Transparent)
    .clip(true)
    .into()
}

fn text_cell_left<'a>(
    content: &str,
    width: Length,
    is_selected: bool,
) -> Element<'a, Message, Theme, iced::Renderer> {
    container(
        text(content)
            .shaping(text::Shaping::Advanced)
            .size(11)
            .style(if is_selected {
                Some(Color::BLACK)
            } else {
                None
            })
            .font(iced::Font {
                weight: if is_selected {
                    iced::font::Weight::Bold
                } else {
                    iced::font::Weight::Normal
                },
                ..Default::default()
            }),
    )
    .width(width)
    .height(Length::Fixed(16.0))
    .center_y()
    .padding(Padding::from([0.0, 0.0, 0.0, 5.0]))
    .style(style::Container::Transparent)
    .clip(true)
    .into()
}

fn text_cell_right<'a>(
    content: &str,
    width: Length,
    is_selected: bool,
) -> Element<'a, Message, Theme, iced::Renderer> {
    container(
        text(content)
            .shaping(text::Shaping::Advanced)
            .size(11)
            .style(if is_selected {
                Some(Color::BLACK)
            } else {
                None
            })
            .font(iced::Font {
                weight: if is_selected {
                    iced::font::Weight::Bold
                } else {
                    iced::font::Weight::Normal
                },
                ..Default::default()
            }),
    )
    .width(width)
    .height(Length::Fixed(16.0))
    .center_y()
    .padding(Padding::from([0.0, 5.0, 0.0, 0.0]))
    .align_x(iced::alignment::Horizontal::Right)
    .style(style::Container::Transparent)
    .clip(true)
    .into()
}
