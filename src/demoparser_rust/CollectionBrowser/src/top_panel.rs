use iced::widget::{button, container, pick_list, row, text, text_input, tooltip, Space};
use iced::{Element, Length};
use std::collections::HashMap;

use crate::message::Message;
use crate::models::{CollectionEntry, CollectionType, TickFilterState};
use crate::style;
use crate::theme::Theme;

// Numerical columns available for filtering
const NUM_COLUMNS: &[&str] = &[
    "Duration", "Killer R", "Victim R", "Moved", "GameVer", "Hits", "Misses", "Hit %",
];

fn create_checkbox<'a>(
    label: &str,
    is_checked: bool,
    msg: Message,
) -> Element<'a, Message, Theme, iced::Renderer> {
    let checkbox_content = if is_checked {
        container(Space::new(Length::Fixed(8.0), Length::Fixed(8.0)))
            .width(Length::Fixed(8.0))
            .height(Length::Fixed(8.0))
            .style(style::Container::CheckboxSquare(true))
    } else {
        container(Space::new(Length::Fixed(8.0), Length::Fixed(8.0)))
            .width(Length::Fixed(8.0))
            .height(Length::Fixed(8.0))
            .style(style::Container::Transparent)
    };

    container(
        row![
            button(
                container(checkbox_content)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .center_x()
                    .center_y()
            )
            .on_press(msg.clone())
            .style(style::Button::ControlPanelCheckbox(false, false))
            .width(Length::Fixed(16.0))
            .height(Length::Fixed(16.0))
            .padding(1.0),
            button(
                container(text(label).size(12))
                    .height(Length::Fixed(16.0))
                    .center_y()
                    .style(style::Container::Transparent)
            )
            .on_press(msg)
            .style(style::Button::ControlPanelLabel(false))
            .padding(0)
            .height(Length::Fixed(16.0)),
        ]
        .spacing(5)
        .align_items(iced::Alignment::Center),
    )
    .width(Length::Fixed(70.0))
    .style(style::Container::Transparent)
    .into()
}

/// View for the top panel of the application
pub fn view<'a>(
    parser_output_path: &'a str,
    type_filters: &'a HashMap<CollectionType, bool>,
    filter_tick_data: TickFilterState,
    filter_demo_names: bool,
    selected_count: usize,
    search_input: &'a str,
    num_filter_min: &'a str,
    num_filter_max: &'a str,
    num_filter_column: &'a str,
    num_filter_exclude: bool,
    loaded_collections: &'a [CollectionEntry],
    filtered_count: usize,
    is_parsing: bool,
) -> Element<'a, Message, Theme, iced::Renderer> {
    // Parser output section for top panel
    let parser_output_section = row![
        button(
            container(text("Browse").size(12))
                .center_x()
                .style(style::Container::Transparent)
        )
        .on_press(Message::BrowseParserOutput)
        .style(style::Button::Embossed)
        .width(Length::Fixed(60.0))
        .padding(6.0),
        text_input("", parser_output_path)
            .on_input(Message::ParserOutputChanged)
            .style(style::TextInput)
            .width(Length::Fixed(200.0)),
    ]
    .spacing(0)
    .align_items(iced::Alignment::Center);

    // Top Panel - spans full width with refresh button anchored right
    let filters = CollectionType::all()
        .iter()
        .fold(row![].spacing(2), |row, &coll_type| {
            let enabled = type_filters.get(&coll_type).copied().unwrap_or(false);

            let tooltip_text = match coll_type {
                CollectionType::Ace => "5 Kills",
                CollectionType::Quad => "4 Kills",
                CollectionType::Multi => "3 Kills on same tick",
                CollectionType::Triple => "3 Kills",
                CollectionType::Double => "2 Kills on same tick",
            };

            let filter_btn = button(
                container(text(coll_type.as_str()).size(12))
                    .center_x()
                    .style(style::Container::Transparent),
            )
            .on_press(Message::ToggleFilter(coll_type, !enabled))
            .style(if enabled {
                style::Button::FilterActive
            } else {
                style::Button::FilterInactive
            })
            .width(Length::Fixed(60.0))
            .padding(6.0);

            let filter_btn_with_tooltip =
                tooltip(filter_btn, tooltip_text, tooltip::Position::Bottom)
                    .style(style::Container::Tooltip);

            row.push(filter_btn_with_tooltip)
        });

    // Tick Data filter button
    let (tick_btn_text, tick_btn_style, tick_btn_tooltip) = match filter_tick_data {
        TickFilterState::All => (
            "ALL",
            style::Button::FilterInactive,
            "Show all collections (with and without tick data)",
        ),
        TickFilterState::TickOnly => (
            "TICK",
            style::Button::FilterActive,
            "Show only collections WITH tick data",
        ),
        TickFilterState::NoTick => (
            "NONE",
            style::Button::FilterActive,
            "Show only collections WITHOUT tick data",
        ),
    };

    let tick_data_btn = tooltip(
        button(
            container(text(tick_btn_text).size(12))
                .center_x()
                .style(style::Container::Transparent),
        )
        .on_press(Message::ToggleTickDataFilter(filter_tick_data.next()))
        .style(tick_btn_style)
        .width(Length::Fixed(70.0))
        .padding(6.0),
        tick_btn_tooltip,
        tooltip::Position::Bottom,
    )
    .style(style::Container::Tooltip);

    // DemoName filter button - enabled if filter is active OR selections exist
    let demo_name_btn = tooltip(
        button(
            container(text("DemoName").size(12))
                .center_x()
                .style(style::Container::Transparent),
        )
        .on_press(if filter_demo_names || selected_count > 0 {
            Message::ToggleDemoNameFilter
        } else {
            Message::Noop
        })
        .style(if !filter_demo_names && selected_count == 0 {
            style::Button::Disabled
        } else if filter_demo_names {
            style::Button::FilterActive
        } else {
            style::Button::FilterInactive
        })
        .width(Length::Fixed(90.0))
        .padding(6.0),
        "Show all entries from the same demo file(s) as selected entries.\nOverrides other filters when active.",
        tooltip::Position::Bottom
    )
    .style(style::Container::Tooltip);

    // Search filter input with tooltip
    let search_field = tooltip(
        text_input("Search", search_input)
            .on_input(Message::SearchInputChanged)
            .style(style::TextInput)
            .width(Length::Fixed(150.0)),
        "Filter collections by partial matches. Supports modifiers:\n\
         • Use ! prefix to exclude (e.g., !awp)\n\
         • Use , to separate multiple terms (AND logic)\n\
         • All matches are case-insensitive\n\n\
         Examples:\n\
         • dust2 - Show dust2 map\n\
         • awp,ak47 - Show kills with BOTH awp AND ak47\n\
         • !awp,!deagle - Exclude AWP and deagle kills",
        tooltip::Position::Bottom,
    )
    .style(style::Container::Tooltip);

    row![
        parser_output_section,
        container(
            row![
                filters,
                tick_data_btn,
                demo_name_btn,
                Space::new(Length::Fixed(4.0), Length::Fixed(0.0)),
                // Numerical filter controls grouped with 2px spacing, Exclude 4px away
                row![
                    pick_list(NUM_COLUMNS, Some(num_filter_column), |selected| {
                        Message::NumFilterColumnChanged(selected.to_string())
                    })
                    .width(Length::Fixed(85.0)),
                    text_input("Min", num_filter_min)
                        .on_input(Message::NumFilterMinChanged)
                        .style(style::TextInput)
                        .width(Length::Fixed(40.0)),
                    text_input("Max", num_filter_max)
                        .on_input(Message::NumFilterMaxChanged)
                        .style(style::TextInput)
                        .width(Length::Fixed(40.0)),
                    Space::new(Length::Fixed(4.0), Length::Fixed(0.0)),
                    create_checkbox(
                        "Exclude",
                        num_filter_exclude,
                        Message::NumFilterExcludeToggled(!num_filter_exclude)
                    ),
                ]
                .spacing(2)
                .align_items(iced::Alignment::Center),
            ]
            .spacing(2)
            .align_items(iced::Alignment::Center)
        )
        .width(Length::Fill)
        .clip(true)
        .style(style::Container::Transparent),
        container(
            row![
                container(text(format!("Results: {}", filtered_count)).size(14))
                    .clip(true)
                    .style(style::Container::Transparent),
                container(
                    text(format!(
                        "Selected: {}",
                        loaded_collections.iter().filter(|c| c.selected).count()
                    ))
                    .size(14)
                )
                .clip(true)
                .style(style::Container::Transparent),
            ]
            .spacing(10)
            .align_items(iced::Alignment::Center)
        )
        .style(style::Container::Transparent),
        Space::new(Length::Fixed(10.0), Length::Fixed(0.0)),
        search_field,
        button(
            container(text("Refresh").size(12))
                .center_x()
                .style(style::Container::Transparent)
        )
        .on_press(match is_parsing {
            true => Message::Noop,
            false => Message::RefreshDatabases,
        })
        .style(if is_parsing {
            style::Button::Disabled
        } else {
            style::Button::Embossed
        })
        .padding(6.0)
    ]
    .padding(5)
    .spacing(2)
    .align_items(iced::Alignment::Center)
    .into()
}
