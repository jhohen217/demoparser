use crate::message::Message;
use crate::models::CollectionEntry;
use crate::playbar;
use crate::playbar::PlaybarState;
use crate::radar_view::RadarView;
use crate::style;
use crate::theme::Theme;
use iced::widget::{column, container, image, mouse_area, row, scrollable, text};
use iced::{Element, Length};

/// Convert weapon ID to weapon name
fn weapon_id_to_name(weapon_id: &str) -> &str {
    match weapon_id {
        "1" => "deagle",
        "2" => "elite",
        "3" => "fiveseven",
        "4" => "glock",
        "7" => "ak47",
        "8" => "aug",
        "9" => "awp",
        "10" => "famas",
        "11" => "g3sg1",
        "13" => "galilar",
        "14" => "m249",
        "16" => "m4a1",
        "17" => "mac10",
        "19" => "p90",
        "23" => "mp5sd",
        "24" => "ump45",
        "25" => "xm1014",
        "26" => "bizon",
        "27" => "mag7",
        "28" => "negev",
        "29" => "sawedoff",
        "30" => "tec9",
        "31" => "taser",
        "32" => "hkp2000",
        "33" => "mp7",
        "34" => "mp9",
        "35" => "nova",
        "36" => "p250",
        "38" => "scar20",
        "39" => "sg556",
        "40" => "ssg08",
        "60" => "m4a1_silencer",
        "61" => "usp_silencer",
        "63" => "cz75a",
        "64" => "revolver",
        "42" => "knife",
        "43" => "flashbang",
        "44" => "hegrenade",
        "45" => "smokegrenade",
        "46" => "molotov",
        "47" => "decoy",
        "48" => "incgrenade",
        "49" => "c4",
        _ => weapon_id,
    }
}

pub fn view<'a>(
    terminal_logs: &[String],
    selected_collection: Option<&CollectionEntry>,
    theme: &Theme,
    terminal_scroll_id: scrollable::Id,
    height: f32,
    radar_view: &'a RadarView,
    playbar_state: &PlaybarState,
) -> Element<'a, Message, Theme, iced::Renderer> {
    // Terminal
    let terminal = container(
        scrollable(
            column(
                terminal_logs
                    .iter()
                    .map(|log| {
                        let color = if log.starts_with("ERROR:") {
                            theme.error_red
                        } else {
                            theme.text
                        };
                        text(log).style(Some(color)).into()
                    })
                    .collect::<Vec<_>>(),
            )
            .width(Length::Fill)
            .padding(5),
        )
        .id(terminal_scroll_id)
        .height(Length::Fixed(height))
        .width(Length::Fill)
        .style(style::Scrollable),
    )
    .style(style::Container::Table)
    .width(Length::Fill)
    .padding(2);

    // Details Panel
    let details_panel = if let Some(selected) = selected_collection {
        let mut dim_text = theme.text;
        dim_text.a = 0.7;

        // Format date better (YYYY-MM-DD HH:MM:SS)
        let formatted_date = selected
            .created_at
            .replace('T', " ")
            .split('.')
            .next()
            .unwrap_or(&selected.created_at)
            .to_string();

        // Title: TYPE - Demoname - Frag X/Y - folder - Date
        // col_total is loaded from database on initial load (not dependent on tick data)
        // Show "?" if None or 0 (0 is not valid - means unpopulated in older databases)
        let col_total_display = selected
            .col_total
            .filter(|&t| t > 0)
            .map(|t| t.to_string())
            .unwrap_or_else(|| "?".to_string());

        let header = text(format!(
            "{} - {} - Frag {}/{} - {} - {}",
            selected.collection_type,
            selected.demo_name,
            selected.collection_num,
            col_total_display,
            selected.folder(),
            formatted_date
        ))
        .size(14)
        .style(Some(theme.text))
        .font(iced::Font {
            weight: iced::font::Weight::Bold,
            ..Default::default()
        });

        // Map and Killer line (bold)
        let map_killer_line = text(format!("{} - {}", selected.map_name, selected.killer_name))
            .size(13)
            .style(Some(theme.text))
            .font(iced::Font {
                weight: iced::font::Weight::Bold,
                ..Default::default()
            });

        let mut content = column![header, map_killer_line, text("").size(1)]
            .spacing(5)
            .padding(5);

        if selected.details_loaded {
            // Parse all available kill detail arrays
            let kill_ticks: Vec<&str> = selected.kill_ticks.split(';').collect();
            let victim_names: Vec<&str> = if !selected.victims_names.is_empty() {
                selected.victims_names.split(';').collect()
            } else {
                // Fallback to indices if names not available
                selected.victims_index.split(';').collect()
            };
            let ticks_between = selected
                .ticks_between_kills
                .as_ref()
                .map(|s| s.split(';').collect::<Vec<&str>>());
            let victim_distances = selected
                .victim_distance
                .as_ref()
                .map(|s| s.split(';').collect::<Vec<&str>>());

            // For weapons: use kill_weapon_ids if available (TickData=1), otherwise fallback to weapons_id (always available)
            let kill_weapons = if let Some(ref kw) = selected.kill_weapon_ids {
                if !kw.is_empty() {
                    Some(kw.split(';').collect::<Vec<&str>>())
                } else {
                    Some(selected.weapons_id.split(';').collect::<Vec<&str>>())
                }
            } else {
                Some(selected.weapons_id.split(';').collect::<Vec<&str>>())
            };

            let killer_pos_x = selected
                .killer_pos_x
                .as_ref()
                .map(|s| s.split(';').collect::<Vec<&str>>());
            let killer_pos_y = selected
                .killer_pos_y
                .as_ref()
                .map(|s| s.split(';').collect::<Vec<&str>>());
            let killer_pos_z = selected
                .killer_pos_z
                .as_ref()
                .map(|s| s.split(';').collect::<Vec<&str>>());
            let victim_pos_x = selected
                .victim_pos_x
                .as_ref()
                .map(|s| s.split(';').collect::<Vec<&str>>());
            let victim_pos_y = selected
                .victim_pos_y
                .as_ref()
                .map(|s| s.split(';').collect::<Vec<&str>>());
            let victim_pos_z = selected
                .victim_pos_z
                .as_ref()
                .map(|s| s.split(';').collect::<Vec<&str>>());
            let movement_between = if !selected.movement_between_kills.is_empty() {
                Some(
                    selected
                        .movement_between_kills
                        .split(';')
                        .collect::<Vec<&str>>(),
                )
            } else {
                None
            };
            let killer_pitch = selected
                .killer_view_pitch
                .as_ref()
                .map(|s| s.split(';').collect::<Vec<&str>>());
            let killer_yaw = selected
                .killer_view_yaw
                .as_ref()
                .map(|s| s.split(';').collect::<Vec<&str>>());

            // TickData status (line 2 if unavailable)
            if selected.tick_data == 0 {
                let tick_data_text = text("** MISSING TICK DATA **")
                    .size(12)
                    .style(Some(dim_text));
                content = content.push(tick_data_text);
            }

            // Summary line (third line if multikill)
            if kill_ticks.len() > 1 {
                let mut info_color = theme.text;
                info_color.a = 0.8;
                let summary_line = text(format!(
                    "Total: {} kills | Duration: {:.1}s | Average Spacing: {:.1}s",
                    kill_ticks.len(),
                    selected.tick_duration as f64 / 64.0,
                    selected.tick_duration as f64 / 64.0 / (kill_ticks.len() - 1) as f64
                ))
                .size(11)
                .style(Some(info_color));
                content = content.push(summary_line);
            }

            // Display each kill's data
            for i in 0..kill_ticks.len() {
                let tick = kill_ticks.get(i).unwrap_or(&"?");
                // Display actual victim name if available
                let victim_label = victim_names
                    .get(i)
                    .map(|name| name.to_string())
                    .unwrap_or_else(|| "Unknown".to_string());

                // Time between kills
                let time_info = if let Some(ref tb) = ticks_between {
                    if let Some(ticks) = tb.get(i) {
                        let time_diff = ticks.parse::<f64>().unwrap_or(0.0) / 64.0;
                        format!(" (+{:.1}s)", time_diff)
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                };

                // Weapon info - convert ID to name
                let weapon_info = if let Some(ref w) = kill_weapons {
                    weapon_id_to_name(w.get(i).unwrap_or(&"?"))
                } else {
                    "?"
                };

                // Distance to victim
                let dist_info = if let Some(ref d) = victim_distances {
                    if let Some(dist) = d.get(i) {
                        if let Ok(dist_val) = dist.parse::<f64>() {
                            format!(" | Dist: {:.1}", dist_val)
                        } else {
                            String::new()
                        }
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                };

                // Movement between kills
                let move_info = if i > 0 {
                    if let Some(ref m) = movement_between {
                        if let Some(movement) = m.get(i) {
                            if let Ok(move_val) = movement.parse::<f64>() {
                                format!(" | Moved: {:.1}", move_val)
                            } else {
                                String::new()
                            }
                        } else {
                            String::new()
                        }
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                };

                // Main kill line: [TICK X] victim | weapon | Dist: X | Moved: X (+seconds)
                let kill_line = text(format!(
                    "[TICK {}] {}{} | {}{}{}",
                    tick, victim_label, time_info, weapon_info, dist_info, move_info
                ))
                .size(12)
                .style(Some(theme.text));
                content = content.push(kill_line);

                // Position and view angles combined (if available)
                let has_positions = killer_pos_x.is_some() && victim_pos_x.is_some();
                let has_angles = killer_pitch.is_some() && killer_yaw.is_some();

                if has_positions {
                    let k_x = killer_pos_x
                        .as_ref()
                        .and_then(|v| v.get(i))
                        .and_then(|s| s.parse::<f64>().ok());
                    let k_y = killer_pos_y
                        .as_ref()
                        .and_then(|v| v.get(i))
                        .and_then(|s| s.parse::<f64>().ok());
                    let k_z = killer_pos_z
                        .as_ref()
                        .and_then(|v| v.get(i))
                        .and_then(|s| s.parse::<f64>().ok());
                    let v_x = victim_pos_x
                        .as_ref()
                        .and_then(|v| v.get(i))
                        .and_then(|s| s.parse::<f64>().ok());
                    let v_y = victim_pos_y
                        .as_ref()
                        .and_then(|v| v.get(i))
                        .and_then(|s| s.parse::<f64>().ok());
                    let v_z = victim_pos_z
                        .as_ref()
                        .and_then(|v| v.get(i))
                        .and_then(|s| s.parse::<f64>().ok());

                    if let (Some(kx), Some(ky), Some(kz), Some(vx), Some(vy), Some(vz)) =
                        (k_x, k_y, k_z, v_x, v_y, v_z)
                    {
                        // Get view angles for killer
                        let view_str = if has_angles {
                            let pitch = killer_pitch
                                .as_ref()
                                .and_then(|v| v.get(i))
                                .and_then(|s| s.parse::<f64>().ok());
                            let yaw = killer_yaw
                                .as_ref()
                                .and_then(|v| v.get(i))
                                .and_then(|s| s.parse::<f64>().ok());

                            if let (Some(p), Some(y)) = (pitch, yaw) {
                                format!("({:.1}, {:.1})", p, y)
                            } else {
                                String::new()
                            }
                        } else {
                            String::new()
                        };

                        let pos_line = text(format!(
                            "     Killer: ({:.1}, {:.1}, {:.1}){} | Victim: ({:.1}, {:.1}, {:.1})",
                            kx, ky, kz, view_str, vx, vy, vz
                        ))
                        .size(11)
                        .style(Some(dim_text));
                        content = content.push(pos_line);
                    }
                }
            }
        } else {
            content = content.push(text("Loading details...").size(12));
        }

        container(
            scrollable(content.width(Length::Fill))
                .height(Length::Fixed(height))
                .width(Length::Fill)
                .style(style::Scrollable),
        )
        .width(Length::Fill)
        .style(style::Container::Table)
        .padding(2)
    } else {
        container(
            scrollable(
                column![text("Select a collection to view details").size(14)]
                    .width(Length::Fill)
                    .padding(5),
            )
            .height(Length::Fixed(height))
            .width(Length::Fill)
            .style(style::Scrollable),
        )
        .width(Length::Fill)
        .style(style::Container::Table)
        .padding(2)
    };

    // Radar view (square area) - constrain to 1:1 aspect ratio
    // Width should equal height to maintain square shape
    let radar_container = container(radar_view.view())
        .width(Length::Fixed(height)) // Match height to maintain 1:1 ratio
        .height(Length::Fixed(height))
        .style(style::Container::Table)
        .padding(2);

    // Playbar (full width above bottom panel)
    let playbar_widget = playbar::view(playbar_state, theme);

    // Bottom row: Terminal + Radar + Details
    let bottom_row = row![terminal, radar_container, details_panel]
        .spacing(2)
        .height(Length::Fixed(height));

    // Complete bottom area with playbar on top
    column![playbar_widget, bottom_row].spacing(2).into()
}
