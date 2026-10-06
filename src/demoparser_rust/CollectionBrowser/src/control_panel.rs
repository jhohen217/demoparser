use crate::message::Message;
use crate::models::DemoDirectory;
use crate::style;
use crate::theme::Theme;
use iced::alignment::{Horizontal, Vertical};
use iced::widget::{
    button, canvas, column, container, mouse_area, row, scrollable, text, text_input, tooltip,
    vertical_space, Space,
};
use iced::{mouse, Color, Element, Length, Padding, Point, Rectangle, Size};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmMode {
    Clear,
}

fn centered_button<'a>(
    label: &str,
    msg: Message,
) -> iced::widget::Button<'a, Message, Theme, iced::Renderer> {
    button(
        container(text(label).size(12))
            .width(Length::Fill)
            .center_x()
            .style(style::Container::Transparent),
    )
    .on_press(msg)
    .style(style::Button::Embossed)
    .width(Length::Fill)
    .padding(6.0)
}

struct LoadingBar {
    label: String,
    progress: f32,
}

impl canvas::Program<Message, Theme> for LoadingBar {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &iced::Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());

        // Draw segments first so they are behind text
        let target_block_width = 8.0;
        let spacing = 2.0;

        // N * width + (N-1) * spacing <= bounds.width
        let total_segments = ((bounds.width + spacing) / (target_block_width + spacing))
            .floor()
            .max(1.0) as usize;

        let filled_segments = (self.progress * total_segments as f32).round() as usize;

        // Calculate total width actually used to center the blocks
        let used_width =
            total_segments as f32 * target_block_width + (total_segments as f32 - 1.0) * spacing;
        let x_offset = (bounds.width - used_width) / 2.0;

        // Vertically center the blocks within the canvas bounds
        let height = bounds.height * 0.8;
        let y_offset = (bounds.height - height) / 2.0;

        for i in 0..filled_segments {
            let x = x_offset + i as f32 * (target_block_width + spacing);
            let rect = Rectangle::new(
                Point::new(x, y_offset),
                Size::new(target_block_width, height),
            );
            frame.fill(
                &canvas::Path::rectangle(rect.position(), rect.size()),
                Color::WHITE,
            );
        }

        // Draw text aligned right, vertically centered
        let padding_right = 4.0;
        let text_size = 10.0;

        frame.fill_text(canvas::Text {
            content: self.label.clone(),
            position: Point::new(bounds.width - padding_right, bounds.height / 2.0),
            color: theme.button_border_light,
            size: text_size.into(),
            font: iced::Font {
                weight: iced::font::Weight::Bold,
                ..Default::default()
            },
            horizontal_alignment: Horizontal::Right,
            vertical_alignment: Vertical::Center,
            ..canvas::Text::default()
        });

        vec![frame.into_geometry()]
    }
}

fn view_loading_bar<'a>(label: &str, progress: f32) -> Element<'a, Message, Theme, iced::Renderer> {
    container(
        canvas(LoadingBar {
            label: label.to_string(),
            progress,
        })
        .width(Length::Fill)
        .height(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fixed(28.0)) // Match button height
    .style(style::Container::LoadingBarBg)
    .padding(2)
    .into()
}

fn create_checkbox<'a>(
    label: &str,
    is_checked: bool,
    msg: Message,
    hovered_control: Option<&str>,
) -> Element<'a, Message, Theme, iced::Renderer> {
    let id = label;
    let is_hovered = hovered_control == Some(id);

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

    mouse_area(
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
                .style(style::Button::ControlPanelCheckbox(false, is_hovered)) // Always False to behave as a container frame, but pass hover state
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
                .style(style::Button::ControlPanelLabel(is_hovered))
                .padding(0)
                .height(Length::Fixed(16.0)),
            ]
            .spacing(5)
            .align_items(iced::Alignment::Center),
        )
        .width(Length::Fixed(110.0))
        .style(style::Container::Transparent),
    )
    .on_enter(Message::ControlHovered(id.to_string()))
    .on_exit(Message::ControlUnhovered)
    .into()
}

pub fn view<'a>(
    directories: &[DemoDirectory],
    selected_directory_index: Option<usize>,
    selected_count: usize,
    tag_input: &str,
    confirm_mode: Option<ConfirmMode>,
    unzip_dir_path: &str,
    ram_unzip: bool,
    parser_aces: bool,
    parser_quads: bool,
    parser_triples: bool,
    parser_multi: bool,
    parser_singles: bool,
    parser_doubles: bool,
    parser_grenade_trajectory: bool,
    parser_overwrite: bool,
    parser_output_npz: bool,
    parser_output_s2r: bool,
    parser_threads_input: &str,
    hovered_control: Option<&str>,
    collection_progress: f32,
    tick_progress: f32,
    batch_progress: f32,
    collection_label: String,
    tick_label: String,
    batch_label: String,
    is_parsing: bool,
) -> Element<'a, Message, Theme, iced::Renderer> {
    let has_directory_selected = selected_directory_index.is_some();
    let has_selection = selected_count > 0;

    // Control buttons below table, aligned right
    let dir_buttons = row![
        tooltip(
            button(
                container(text("+").size(9))
                    .center_x()
                    .center_y()
                    .style(style::Container::Transparent)
            )
            .on_press(Message::AddDemoDirectory)
            .style(style::Button::Embossed)
            .width(Length::Fixed(20.0))
            .height(Length::Fixed(20.0))
            .padding(0),
            "Add directory",
            tooltip::Position::Top
        )
        .style(style::Container::Tooltip),
        tooltip(
            button(
                container(text("-").size(9))
                    .center_x()
                    .center_y()
                    .style(style::Container::Transparent)
            )
            .on_press(Message::RemoveDemoDirectory)
            .style(style::Button::Embossed)
            .width(Length::Fixed(20.0))
            .height(Length::Fixed(20.0))
            .padding(0),
            "Remove selected directory",
            tooltip::Position::Top
        )
        .style(style::Container::Tooltip),
        tooltip(
            button(
                container(text("^").size(9))
                    .center_x()
                    .center_y()
                    .style(style::Container::Transparent)
            )
            .on_press(Message::OpenDirectoryInExplorer)
            .style(style::Button::Embossed)
            .width(Length::Fixed(20.0))
            .height(Length::Fixed(20.0))
            .padding(0),
            "Open in file explorer",
            tooltip::Position::Top
        )
        .style(style::Container::Tooltip),
    ]
    .spacing(2);

    // Directory table with zebra striping and selection
    let dir_rows_content = if directories.is_empty() {
        scrollable(
            mouse_area(
                container(Space::new(Length::Fill, Length::Fill))
                    .height(Length::Fill)
                    .style(style::Container::Transparent),
            )
            .on_press(Message::DeselectAllDirectories),
        )
        .height(Length::Fixed(120.0))
        .style(style::Scrollable)
    } else {
        scrollable(column(
            directories
                .iter()
                .enumerate()
                .map(|(idx, dir)| {
                    let is_selected = selected_directory_index == Some(idx);
                    let is_enabled = dir.enabled;
                    let is_alt = idx % 2 != 0;
                    let drive_letter = dir.drive_letter();
                    let folder_name = dir.folder_name();

                    // Enabled checkbox
                    let enabled_checkbox_content =
                        container(Space::new(Length::Fixed(6.0), Length::Fixed(6.0)))
                            .width(Length::Fixed(6.0))
                            .height(Length::Fixed(6.0))
                            .style(if is_enabled {
                                style::Container::CheckboxSquare(true)
                            } else {
                                style::Container::Transparent
                            });

                    let enabled_checkbox = button(
                        container(enabled_checkbox_content)
                            .width(Length::Fill)
                            .height(Length::Fill)
                            .center_x()
                            .center_y(),
                    )
                    .on_press(Message::ToggleDirectoryEnabled(idx, !is_enabled))
                    .style(style::Button::Checkbox(is_enabled, is_selected, is_alt))
                    .width(Length::Fixed(11.0))
                    .height(Length::Fixed(11.0))
                    .padding(0);

                    // File count and processed demo count
                    let count_text =
                        format!("{}/{}", dir.valid_file_count, dir.processed_demo_count);

                    // TickData count - show "..." if not loaded yet
                    let tick_data_text =
                        match (dir.tick_data_enabled_count, dir.total_collections_count) {
                            (Some(enabled), Some(total)) => format!("{}/{}", enabled, total),
                            _ => "...".to_string(),
                        };

                    let row_content = row![
                        container(enabled_checkbox)
                            .width(Length::Fixed(11.0))
                            .center_x(),
                        dir_text_cell(&drive_letter, Length::Fixed(25.0), is_selected),
                        dir_text_cell(&folder_name, Length::Fill, is_selected),
                        dir_text_cell(&count_text, Length::Fixed(50.0), is_selected),
                        dir_text_cell(&tick_data_text, Length::Fixed(50.0), is_selected),
                    ]
                    .spacing(3)
                    .align_items(iced::Alignment::Center)
                    .padding(Padding::from([2.0, 3.0, 2.0, 3.0]));

                    button(row_content)
                        .on_press(Message::SelectDirectoryRow(idx))
                        .style(style::Button::TableRow(is_selected, is_alt))
                        .width(Length::Fill)
                        .padding(0)
                        .into()
                })
                .collect::<Vec<_>>(),
        ))
        .height(Length::Fixed(120.0))
        .style(style::Scrollable)
    };

    // Column header row - "Drive" with enough space
    let column_header = container(
        row![
            text("Drive").size(10).font(iced::Font {
                weight: iced::font::Weight::Bold,
                ..Default::default()
            }),
            Space::new(Length::Fixed(3.0), Length::Fixed(0.0)),
            container(text("Folder").size(10).font(iced::Font {
                weight: iced::font::Weight::Bold,
                ..Default::default()
            }))
            .width(Length::Fill)
            .height(Length::Fixed(14.0))
            .center_y()
            .style(style::Container::Transparent),
            container(text("Demos").size(10).font(iced::Font {
                weight: iced::font::Weight::Bold,
                ..Default::default()
            }))
            .width(Length::Fixed(50.0))
            .height(Length::Fixed(14.0))
            .center_y()
            .style(style::Container::Transparent),
            container(text("Tick").size(10).font(iced::Font {
                weight: iced::font::Weight::Bold,
                ..Default::default()
            }))
            .width(Length::Fixed(50.0))
            .height(Length::Fixed(14.0))
            .center_y()
            .style(style::Container::Transparent),
        ]
        .spacing(3)
        .align_items(iced::Alignment::Center),
    )
    .padding(Padding::from([2.0, 3.0, 2.0, 3.0]))
    .style(style::Container::Transparent);

    let dir_section = column![
        column_header,
        container(dir_rows_content)
            .height(Length::Fixed(120.0))
            .width(Length::Fill)
            .style(style::Container::Table)
            .padding(2),
        row![Space::new(Length::Fill, Length::Fixed(0.0)), dir_buttons,]
            .align_items(iced::Alignment::Center),
    ]
    .spacing(5);

    // Unzip directory section
    let unzip_dir_section = tooltip(
        row![
            button(
                container(text("Browse").size(12))
                    .center_x()
                    .style(style::Container::Transparent)
            )
            .on_press(Message::BrowseUnzipDir)
            .style(style::Button::Embossed)
            .width(Length::Fixed(60.0))
            .padding(6.0),
            text_input("Unzip To Directory", unzip_dir_path)
                .on_input(Message::UnzipDirChanged)
                .style(style::TextInput)
                .width(Length::Fill),
        ]
        .spacing(0)
        .align_items(iced::Alignment::Center),
        "Optional directory for decompressing demos. Recommend an SSD for faster processing. Useful when using an HDD for demo storage.",
        tooltip::Position::Top
    )
    .style(style::Container::Tooltip);

    // RAM Unzip checkbox
    let ram_unzip_checkbox = tooltip(
        create_checkbox(
            "RAM Unzip",
            ram_unzip,
            Message::ToggleRamUnzip(!ram_unzip),
            hovered_control,
        ),
        "Decompress directly to memory (faster, requires more RAM)",
        tooltip::Position::Top,
    )
    .style(style::Container::Tooltip);

    let tag_buttons = if let Some(mode) = confirm_mode {
        let msg = match mode {
            ConfirmMode::Clear => "Clear all?",
        };
        column![
            text(msg).size(14),
            iced::widget::row![
                centered_button("Yes", Message::ConfirmTagAction),
                centered_button("No", Message::CancelTagAction),
            ]
            .spacing(10)
        ]
        .spacing(5)
    } else {
        column![
            iced::widget::row![
                tooltip(
                    centered_button("Apply Tag", Message::ApplyTag),
                    "Apply the tag text to all selected collections. Tags are used to organize and identify collections.",
                    tooltip::Position::Top
                )
                .style(style::Container::Tooltip),
                tooltip(
                    centered_button("Clear Tag", Message::ClearTag),
                    "Remove tags from all selected collections. Click twice to confirm.",
                    tooltip::Position::Top
                )
                .style(style::Container::Tooltip),
            ].spacing(10)
        ].spacing(0)
    };

    // Parser Configuration section
    let parser_config_section = column![
        text("Tick Data Config").size(12).font(iced::Font {
            weight: iced::font::Weight::Bold,
            ..Default::default()
        }),
        vertical_space().height(Length::Fixed(5.0)),
        // Collection type checkboxes - 2 per row, 4 rows
        row![
            tooltip(
                create_checkbox("Ace", parser_aces, Message::ToggleParserAces(!parser_aces), hovered_control),
                "Generate tick-by-tick for 5 kills",
                tooltip::Position::Top
            )
            .style(style::Container::Tooltip),
            Space::new(Length::Fill, Length::Fixed(0.0)),
            tooltip(
                create_checkbox("Quad", parser_quads, Message::ToggleParserQuads(!parser_quads), hovered_control),
                "Generate tick-by-tick for 4 kills",
                tooltip::Position::Top
            )
            .style(style::Container::Tooltip),
        ],
        vertical_space().height(Length::Fixed(3.0)),
        row![
            tooltip(
                create_checkbox("Multi", parser_multi, Message::ToggleParserMulti(!parser_multi), hovered_control),
                "Generate tick-by-tick for 3 kills on the same tick",
                tooltip::Position::Top
            )
            .style(style::Container::Tooltip),
            Space::new(Length::Fill, Length::Fixed(0.0)),
            tooltip(
                create_checkbox("Triple", parser_triples, Message::ToggleParserTriples(!parser_triples), hovered_control),
                "Generate tick-by-tick for 3 kills",
                tooltip::Position::Top
            )
            .style(style::Container::Tooltip),
        ],
        vertical_space().height(Length::Fixed(3.0)),
        row![
            tooltip(
                create_checkbox("Double", parser_doubles, Message::ToggleParserDoubles(!parser_doubles), hovered_control),
                "Generate tick-by-tick for 2 kills on the same tick",
                tooltip::Position::Top
            )
            .style(style::Container::Tooltip),
            Space::new(Length::Fill, Length::Fixed(0.0)),
            tooltip(
                create_checkbox("Single", parser_singles, Message::ToggleParserSingles(!parser_singles), hovered_control),
                "Generate tick-by-tick for 1 kill",
                tooltip::Position::Top
            )
            .style(style::Container::Tooltip),
        ],
        vertical_space().height(Length::Fixed(3.0)),
        row![
            tooltip(
                create_checkbox("Util Trajectory", parser_grenade_trajectory, Message::ToggleParserGrenadeTrajectory(!parser_grenade_trajectory), hovered_control),
                "Parse grenade trajectories for killer's grenades (mode 1)",
                tooltip::Position::Top
            )
            .style(style::Container::Tooltip),
            Space::new(Length::Fill, Length::Fixed(0.0)),
            tooltip(
                create_checkbox("Overwrite", parser_overwrite, Message::ToggleParserOverwrite(!parser_overwrite), hovered_control),
                "Overwrite existing output files",
                tooltip::Position::Top
            )
            .style(style::Container::Tooltip),
        ],
        vertical_space().height(Length::Fixed(3.0)),
        row![
            tooltip(
                create_checkbox("Output NPZ", parser_output_npz, Message::ToggleOutputNpz(!parser_output_npz), hovered_control),
                "Write .npz files (numpy archive) for each collection",
                tooltip::Position::Top
            )
            .style(style::Container::Tooltip),
            Space::new(Length::Fill, Length::Fixed(0.0)),
            tooltip(
                create_checkbox("Output S2R", parser_output_s2r, Message::ToggleOutputS2r(!parser_output_s2r), hovered_control),
                "Write .s2r binary replay files for each collection",
                tooltip::Position::Top
            )
            .style(style::Container::Tooltip),
        ],
        vertical_space().height(Length::Fixed(5.0)),
        // Threads text input - aligned under checkboxes
        row![
            tooltip(
                text_input("auto", parser_threads_input)
                    .on_input(Message::ParserThreadsInputChanged)
                    .style(style::TextInput)
                    .width(Length::Fixed(40.0)),
                "Number of threads for parallel processing (leave blank or 0 for auto-detect using 75% of cores)",
                tooltip::Position::Top
            )
            .style(style::Container::Tooltip),
            Space::new(Length::Fill, Length::Fixed(0.0)),
            container(text("Threads").size(12))
                .width(Length::Fixed(110.0))
                .style(style::Container::Transparent),
        ].align_items(iced::Alignment::Center),
    ];

    // Parse button - dynamic based on selection
    let parse_button: Element<'a, Message, Theme, iced::Renderer> = if is_parsing {
        tooltip(
            button(
                container(text("Cancel Parsing").size(12))
                    .width(Length::Fill)
                    .center_x()
                    .style(style::Container::Transparent),
            )
            .on_press(Message::CancelParsing)
            .style(style::Button::Embossed)
            .width(Length::Fill)
            .padding(6.0),
            "Cancel currently running parse operation",
            tooltip::Position::Top,
        )
        .style(style::Container::Tooltip)
        .into()
    } else if has_selection {
        tooltip(
            button(
                container(text("Parse Collections").size(12))
                    .width(Length::Fill)
                    .center_x()
                    .style(style::Container::Transparent),
            )
            .on_press(Message::ParseCollections)
            .style(style::Button::Embossed)
            .width(Length::Fill)
            .padding(6.0),
            "Parse selected collection entries",
            tooltip::Position::Top,
        )
        .style(style::Container::Tooltip)
        .into()
    } else if has_directory_selected {
        tooltip(
            button(
                container(text("Parse Demos").size(12))
                    .width(Length::Fill)
                    .center_x()
                    .style(style::Container::Transparent),
            )
            .on_press(Message::ParseDirectory)
            .style(style::Button::Embossed)
            .width(Length::Fill)
            .padding(6.0),
            "Parse all demos in selected directory",
            tooltip::Position::Top,
        )
        .style(style::Container::Tooltip)
        .into()
    } else {
        tooltip(
            button(
                container(text("Parse Demos").size(12))
                    .width(Length::Fill)
                    .center_x()
                    .style(style::Container::Transparent),
            )
            .style(style::Button::Disabled)
            .width(Length::Fill)
            .padding(6.0),
            "Select a directory or collections to parse",
            tooltip::Position::Top,
        )
        .style(style::Container::Tooltip)
        .into()
    };

    column![
        tooltip(
            text_input("Enter tag...", tag_input)
                .on_input(Message::TagInputChanged)
                .style(style::TextInput),
            "Enter a tag name to organize selected collections. Tags help you categorize and filter collections (e.g., 'best_kills', 'training', 'competitive').",
            tooltip::Position::Top
        )
        .style(style::Container::Tooltip),
        vertical_space().height(Length::Fixed(2.0)),
        tag_buttons,
        vertical_space().height(Length::Fixed(3.0)),
        dir_section,
        vertical_space().height(Length::Fixed(3.0)),
        unzip_dir_section,
        vertical_space().height(Length::Fixed(3.0)),
        ram_unzip_checkbox,
        vertical_space().height(Length::Fixed(3.0)),
        parser_config_section,
        vertical_space().height(Length::Fill),
        parse_button,
        centered_button("Import Frags", Message::ExportSelections),
        vertical_space().height(Length::Fixed(10.0)),
        column![
            view_loading_bar(&collection_label, collection_progress),
            view_loading_bar(&tick_label, tick_progress),
            view_loading_bar(&batch_label, batch_progress),
        ].spacing(5)
    ]
    .width(Length::Fixed(250.0))
    .padding(8)
    .spacing(3)
    .into()
}

fn dir_text_cell<'a>(
    content: &str,
    width: Length,
    is_selected: bool,
) -> Element<'a, Message, Theme, iced::Renderer> {
    container(
        text(content)
            .shaping(text::Shaping::Advanced)
            .size(10)
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
    .height(Length::Fixed(14.0))
    .center_y()
    .style(style::Container::Transparent)
    .clip(true)
    .into()
}
