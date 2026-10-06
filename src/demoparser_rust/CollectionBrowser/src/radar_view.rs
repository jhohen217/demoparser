//! Radar view canvas widget for displaying player positions on map

use crate::message::Message;
use crate::npz_loader::{NpzData, PlayerBounds};
use crate::radar_config::RadarConfig;
use crate::theme::Theme;
use iced::advanced::widget::Tree;
use iced::advanced::{layout, mouse, overlay, renderer, widget, Layout, Widget};
use iced::mouse::Cursor;
use iced::widget::canvas::{self, Cache, Canvas, Frame, Geometry, Path, Stroke};
use iced::{event, Color, Element, Length, Point, Rectangle, Renderer, Size, Vector};
use std::sync::Arc;

/// Represents the shooting state for player marker border styling
#[derive(Debug, Clone, Copy, PartialEq)]
enum ShotState {
    None, // Not shooting - default white border, normal thickness
    Miss, // Shot but missed - white border, thicker
    Hit,  // Shot and hit but not kill - dim red border, thicker
    Kill, // Shot and killed - full red border, thickest
}

pub struct RadarView {
    cache: Cache,
    npz_data: Option<Arc<NpzData>>,
    radar_config: Option<RadarConfig>,
    current_tick_idx: usize,
    pub radar_image: Option<iced::widget::image::Handle>,
    player_bounds: Option<PlayerBounds>,
}

impl RadarView {
    pub fn new() -> Self {
        Self {
            cache: Cache::new(),
            npz_data: None,
            radar_config: None,
            current_tick_idx: 0,
            radar_image: None,
            player_bounds: None,
        }
    }

    /// Set NPZ data for rendering
    pub fn set_npz_data(&mut self, data: Option<Arc<NpzData>>) {
        // Calculate player bounds when loading new data
        self.player_bounds = data.as_ref().and_then(|d| d.calculate_player_bounds());
        self.npz_data = data;
        self.cache.clear();
    }

    /// Set radar configuration
    pub fn set_radar_config(&mut self, config: Option<RadarConfig>) {
        self.radar_config = config;
        self.cache.clear();
    }

    /// Set current tick index
    pub fn set_current_tick(&mut self, tick_idx: usize) {
        if self.current_tick_idx != tick_idx {
            self.current_tick_idx = tick_idx;
            self.cache.clear();
        }
    }

    /// Set radar background image
    pub fn set_radar_image(&mut self, handle: Option<iced::widget::image::Handle>) {
        self.radar_image = handle;
        self.cache.clear();
    }

    /// Create view element with optional image background
    pub fn view(&self) -> Element<Message, Theme, Renderer> {
        if let Some(ref img) = self.radar_image {
            use crate::style::Container as ContainerStyle;
            use iced::widget::{container, image};
            // Use RadarContainer to force Canvas (overlay) on top of Image (content)
            RadarContainer::new(
                container(
                    image(img.clone())
                        .width(Length::Fill)
                        .height(Length::Fill)
                        .content_fit(iced::ContentFit::Contain),
                )
                .width(Length::Fill)
                .height(Length::Fill)
                .center_x()
                .center_y()
                .style(ContainerStyle::Table), // Dark background for radar
                Canvas::new(self).width(Length::Fill).height(Length::Fill),
            )
            .into()
        } else {
            Canvas::new(self)
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        }
    }
}

impl canvas::Program<Message, Theme, Renderer> for RadarView {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<Geometry> {
        let geometry = self.cache.draw(renderer, bounds.size(), |frame| {
            // Use square canvas (smallest dimension)
            let size = bounds.width.min(bounds.height);

            // Center the radar view
            let offset_x = (bounds.width - size) / 2.0;
            let offset_y = (bounds.height - size) / 2.0;
            frame.translate(Vector::new(offset_x, offset_y));

            // Draw background image if available, otherwise draw dark background + grid
            if let Some(ref _img) = self.radar_image {
                // Draw the radar image as background
                // Managed by RadarContainer (Image)
            } else {
                // Draw background using theme's darkest color (table_background)
                let background = Path::rectangle(Point::ORIGIN, Size::new(size, size));
                frame.fill(&background, theme.table_background);

                // Draw placeholder grid
                self.draw_grid(frame, size, theme);
            }

            // Draw player markers
            if let Some(npz_data) = &self.npz_data {
                // Draw shoot lines first (so they appear under players)
                self.draw_shoot_lines(frame, npz_data, self.radar_config.as_ref(), size);

                self.draw_players(frame, npz_data, self.radar_config.as_ref(), size, theme);

                // Draw tick info
                if self.current_tick_idx < npz_data.frames.len() {
                    let tick = npz_data.frames[self.current_tick_idx];
                    self.draw_tick_info(
                        frame,
                        tick,
                        self.current_tick_idx,
                        npz_data.frames.len(),
                        size,
                        theme,
                    );
                }
            }
        });

        vec![geometry]
    }
}

impl RadarView {
    /// Draw a placeholder grid
    fn draw_grid(&self, frame: &mut Frame, size: f32, theme: &Theme) {
        let grid_color = Color {
            r: theme.text.r,
            g: theme.text.g,
            b: theme.text.b,
            a: 0.1,
        };

        // Draw grid lines every 100 pixels
        let grid_spacing = 100.0;
        let num_lines = (size / grid_spacing) as usize;

        for i in 0..=num_lines {
            let pos = i as f32 * grid_spacing;

            // Vertical line
            let v_line = Path::line(Point::new(pos, 0.0), Point::new(pos, size));
            frame.stroke(
                &v_line,
                Stroke::default().with_color(grid_color).with_width(1.0),
            );

            // Horizontal line
            let h_line = Path::line(Point::new(0.0, pos), Point::new(size, pos));
            frame.stroke(
                &h_line,
                Stroke::default().with_color(grid_color).with_width(1.0),
            );
        }
    }

    /// Dynamic world to radar coordinate transformation
    fn world_to_radar_dynamic(&self, world_x: f32, world_y: f32, canvas_size: f32) -> (f32, f32) {
        if let Some(bounds) = &self.player_bounds {
            let (center_x, center_y) = bounds.center();
            let (width, height) = bounds.dimensions();

            // Add padding (15% on each side)
            let padding_factor = 1.3;
            let padded_width = width * padding_factor;
            let padded_height = height * padding_factor;

            // Calculate scale to fit the largest dimension
            let max_dimension = padded_width.max(padded_height);
            let scale = if max_dimension > 0.0 {
                canvas_size / max_dimension
            } else {
                1.0
            };

            // Transform: translate to center, then scale to canvas
            let radar_x = ((world_x - center_x) * scale) + (canvas_size / 2.0);
            let radar_y = ((center_y - world_y) * scale) + (canvas_size / 2.0); // Y is inverted

            (radar_x, radar_y)
        } else {
            // Fallback
            let default_scale = canvas_size / 5000.0;
            let radar_x = (world_x * default_scale) + (canvas_size / 2.0);
            let radar_y = (-world_y * default_scale) + (canvas_size / 2.0);
            (radar_x, radar_y)
        }
    }

    /// Draw player markers (triangles with look direction, or X for dead players)
    fn draw_players(
        &self,
        frame: &mut Frame,
        npz_data: &NpzData,
        radar_config: Option<&RadarConfig>,
        size: f32,
        _theme: &Theme,
    ) {
        let num_players = npz_data.num_players();

        for player_idx in 0..num_players {
            // Check if player is alive by attempting to get position
            let player_alive = npz_data
                .get_player_position(self.current_tick_idx, player_idx)
                .is_some();

            if player_alive {
                // Draw alive player as triangle
                if let Some((x, y, _z)) =
                    npz_data.get_player_position(self.current_tick_idx, player_idx)
                {
                    // Transform world coordinates to radar coordinates
                    let (radar_x, radar_y) = if let Some(config) = radar_config {
                        config.world_to_radar(x, y, size)
                    } else {
                        self.world_to_radar_dynamic(x, y, size)
                    };

                    // Skip if outside bounds (with small margin)
                    if radar_x < -10.0
                        || radar_x > size + 10.0
                        || radar_y < -10.0
                        || radar_y > size + 10.0
                    {
                        continue;
                    }

                    // Get player metadata and check if killer using steamid
                    let player_meta = npz_data.get_player_meta(player_idx);
                    let is_killer = player_meta
                        .map(|m| m.steamid.parse::<u64>().ok() == Some(npz_data.killer_steamid))
                        .unwrap_or(false);

                    // Get killer's team to determine teammates vs enemies
                    let killer_team = npz_data
                        .player_meta
                        .iter()
                        .find(|m| m.steamid.parse::<u64>().ok() == Some(npz_data.killer_steamid))
                        .map(|m| m.team.as_str());

                    // DEBUG: Single line for killer verification
                    if is_killer {
                        eprintln!(
                            "KILLER: idx={}, steamid={}, name={}",
                            player_idx,
                            player_meta.map(|m| m.steamid.as_str()).unwrap_or("?"),
                            player_meta.map(|m| m.name.as_str()).unwrap_or("?")
                        );
                    }

                    let mut color = if let Some(meta) = player_meta {
                        self.get_team_color(&meta.team, is_killer, false)
                    } else {
                        Color::from_rgb(0.5, 0.5, 0.5)
                    };

                    // Apply dimness based on team relationship
                    if !is_killer {
                        let is_teammate = player_meta
                            .as_ref()
                            .and_then(|m| killer_team.map(|kt| m.team == kt))
                            .unwrap_or(false);

                        if is_teammate {
                            // Teammates: 50% dimness = 0.5 opacity
                            color.a = 0.5;
                        } else {
                            // Enemies: 75% dimness = 0.25 opacity
                            color.a = 0.25;
                        }
                    }

                    // Get player view angle for triangle rotation
                    let view_yaw = npz_data
                        .get_player_angles(self.current_tick_idx, player_idx)
                        .map(|(_, yaw)| yaw)
                        .unwrap_or(0.0);

                    // DEBUG: Log yaw for first few players to verify
                    if player_idx < 3 && self.current_tick_idx % 32 == 0 {
                        eprintln!(
                            "P{}: yaw={:.1}°, world=({:.0},{:.0}), radar=({:.0},{:.0})",
                            player_idx, view_yaw, x, y, radar_x, radar_y
                        );
                    }

                    // Draw triangle marker - scale with canvas size for consistency
                    let marker_size = size * 0.012; // 1.2% of canvas size

                    // Check if killer is shooting at current frame (for border styling)
                    let shot_state = if is_killer {
                        self.get_killer_shot_state(npz_data, self.current_tick_idx, player_idx)
                    } else {
                        ShotState::None
                    };

                    self.draw_triangle_marker(
                        frame,
                        radar_x,
                        radar_y,
                        marker_size,
                        view_yaw,
                        color,
                        is_killer,
                        shot_state,
                    );
                }
            } else {
                // Player is dead - try to get last known position (subtract 1 from current to avoid showing at death location)
                // Search backwards from current tick - 1 to find last valid position before death
                // Ignore (0,0,0) positions which indicate invalid/uninitialized data
                let search_start = if self.current_tick_idx > 0 {
                    self.current_tick_idx - 1
                } else {
                    0
                };
                let mut last_pos = None;
                for tick_idx in (0..=search_start).rev() {
                    if let Some(pos) = npz_data.get_player_position(tick_idx, player_idx) {
                        // Ignore positions at origin (0,0,0) - likely invalid data
                        if pos.0.abs() > 1.0 || pos.1.abs() > 1.0 {
                            last_pos = Some(pos);
                            break;
                        }
                    }
                }

                if let Some((x, y, _z)) = last_pos {
                    // Transform world coordinates to radar coordinates
                    let (radar_x, radar_y) = if let Some(config) = radar_config {
                        config.world_to_radar(x, y, size)
                    } else {
                        self.world_to_radar_dynamic(x, y, size)
                    };

                    // Skip if outside bounds
                    if radar_x < -10.0
                        || radar_x > size + 10.0
                        || radar_y < -10.0
                        || radar_y > size + 10.0
                    {
                        continue;
                    }

                    // Get player metadata
                    let player_meta = npz_data.get_player_meta(player_idx);
                    let is_killer = player_meta
                        .map(|m| m.steamid.parse::<u64>().ok() == Some(npz_data.killer_steamid))
                        .unwrap_or(false);

                    // Get killer's team to determine teammates vs enemies
                    let killer_team = npz_data
                        .player_meta
                        .iter()
                        .find(|m| m.steamid.parse::<u64>().ok() == Some(npz_data.killer_steamid))
                        .map(|m| m.team.as_str());

                    let mut color = if let Some(meta) = player_meta {
                        self.get_team_color(&meta.team, is_killer, true) // true = dead
                    } else {
                        Color::from_rgba(0.5, 0.5, 0.5, 0.5) // Dimmed default
                    };

                    // Apply dimness based on team relationship and who killed them
                    if is_killer {
                        // Killer's own kill markers: 85% dimness = 0.15 opacity
                        color.a = 0.15;
                    } else {
                        let is_enemy_of_killer = player_meta
                            .as_ref()
                            .and_then(|m| killer_team.map(|kt| m.team != kt))
                            .unwrap_or(false);

                        if is_enemy_of_killer {
                            // Dead enemies (non-killer kills): 60% dimness = 0.40 opacity
                            color.a = 0.40;
                        } else {
                            // Dead teammates: keep base opacity from get_team_color (0.4)
                            // This is already set, so no change needed
                        }
                    }

                    // Draw X marker for dead player - scale with canvas size
                    let marker_size = size * 0.008; // 0.8% of canvas size
                    self.draw_x_marker(frame, radar_x, radar_y, marker_size, color);
                }
            }
        }
    }

    /// Draw a triangle marker representing player with look direction
    fn draw_triangle_marker(
        &self,
        frame: &mut Frame,
        x: f32,
        y: f32,
        size: f32,
        yaw: f32,
        color: Color,
        is_killer: bool,
        shot_state: ShotState,
    ) {
        // CS View Angles: 0 = East, 90 = North, increases Counter-Clockwise (CCW).
        // Screen Space: Y is inverted (Down), so positive rotation is Clockwise (CW).
        // Triangle starts pointing RIGHT (East).
        // To map CCW World Angle to CW Screen Rotation, we using negative yaw:
        // Yaw 0 (East) -> Rot 0 -> Right.
        // Yaw 90 (North) -> Rot -90 -> Up.
        let angle = (-yaw).to_radians();
        let cos_a = angle.cos();
        let sin_a = angle.sin();

        // Border based on shot state - determine early for size calculation
        let (border_color, border_width) = match shot_state {
            ShotState::None => {
                // Default border: brighten the fill color by 30%
                let bright_r = (color.r * 1.3).min(1.0);
                let bright_g = (color.g * 1.3).min(1.0);
                let bright_b = (color.b * 1.3).min(1.0);
                (Color::from_rgb(bright_r, bright_g, bright_b), 1.2)
            }
            ShotState::Miss => (Color::from_rgb(0.9, 0.9, 0.9), 2.0), // White, thicker
            ShotState::Hit => (Color::from_rgb(0.7, 0.0, 0.0), 2.0),  // Dim red, thicker
            ShotState::Kill => (Color::from_rgb(1.0, 0.0, 0.0), 2.5), // Full red, thickest
        };

        // Scale triangle size to expand outwards with thicker borders
        // Add half the border width to make it expand outwards
        let border_expansion = (border_width - 1.2) * 0.5; // Relative to default 1.2px border
        let effective_size = size + border_expansion;

        // Create triangle with tip pointing RIGHT initially (this is our 0° reference)
        // We'll rotate it to match the yaw
        let tip_dist = effective_size * 1.2; // Distance to tip (longest vertex)
        let base_dist = effective_size * 0.6; // Distance to base vertices
        let base_width = effective_size * 0.7; // Half-width of base

        // Define triangle points relative to center, pointing RIGHT (0°)
        // p1 = tip (right)
        // p2 = base top
        // p3 = base bottom
        let p1_local = (tip_dist, 0.0); // Tip points right
        let p2_local = (-base_dist, -base_width); // Base top
        let p3_local = (-base_dist, base_width); // Base bottom

        // Rotate all points by angle and translate to position
        // Standard 2D rotation matrix
        let p1_x = x + (p1_local.0 * cos_a - p1_local.1 * sin_a);
        let p1_y = y + (p1_local.0 * sin_a + p1_local.1 * cos_a);

        let p2_x = x + (p2_local.0 * cos_a - p2_local.1 * sin_a);
        let p2_y = y + (p2_local.0 * sin_a + p2_local.1 * cos_a);

        let p3_x = x + (p3_local.0 * cos_a - p3_local.1 * sin_a);
        let p3_y = y + (p3_local.0 * sin_a + p3_local.1 * cos_a);

        // Build triangle path
        let mut path_builder = canvas::path::Builder::new();
        path_builder.move_to(Point::new(p1_x, p1_y));
        path_builder.line_to(Point::new(p2_x, p2_y));
        path_builder.line_to(Point::new(p3_x, p3_y));
        path_builder.close();
        let triangle = path_builder.build();

        // Fill triangle
        frame.fill(&triangle, color);

        // Add glow for killer
        if is_killer {
            let glow_color = Color {
                r: color.r,
                g: color.g,
                b: color.b,
                a: 0.4,
            };
            frame.stroke(
                &triangle,
                Stroke::default().with_color(glow_color).with_width(2.5),
            );
        }

        // Draw border (already calculated above based on shot state)
        frame.stroke(
            &triangle,
            Stroke::default()
                .with_color(border_color)
                .with_width(border_width),
        );

        // Add halo for killer - a larger triangle outline with gap
        if is_killer {
            // Halo color based on shot state
            let halo_color = match shot_state {
                ShotState::None => Color::from_rgb(190.0 / 255.0, 190.0 / 255.0, 190.0 / 255.0), // Light gray
                ShotState::Miss => Color::from_rgb(1.0, 1.0, 1.0), // White
                ShotState::Hit => Color::from_rgb(0.7, 0.0, 0.0),  // Dim red
                ShotState::Kill => Color::from_rgb(1.0, 0.0, 0.0), // Full red
            };

            // Create gap between border and halo: add border width + gap (3px) to size
            let gap_size = 3.0;
            let halo_expansion = border_width + gap_size;
            let halo_size = effective_size + halo_expansion;

            // Create larger triangle for halo using same proportions
            let halo_tip_dist = halo_size * 1.2;
            let halo_base_dist = halo_size * 0.6;
            let halo_base_width = halo_size * 0.7;

            let h1_local = (halo_tip_dist, 0.0);
            let h2_local = (-halo_base_dist, -halo_base_width);
            let h3_local = (-halo_base_dist, halo_base_width);

            // Rotate and translate halo points
            let h1_x = x + (h1_local.0 * cos_a - h1_local.1 * sin_a);
            let h1_y = y + (h1_local.0 * sin_a + h1_local.1 * cos_a);

            let h2_x = x + (h2_local.0 * cos_a - h2_local.1 * sin_a);
            let h2_y = y + (h2_local.0 * sin_a + h2_local.1 * cos_a);

            let h3_x = x + (h3_local.0 * cos_a - h3_local.1 * sin_a);
            let h3_y = y + (h3_local.0 * sin_a + h3_local.1 * cos_a);

            // Build halo triangle path
            let mut halo_builder = canvas::path::Builder::new();
            halo_builder.move_to(Point::new(h1_x, h1_y));
            halo_builder.line_to(Point::new(h2_x, h2_y));
            halo_builder.line_to(Point::new(h3_x, h3_y));
            halo_builder.close();
            let halo = halo_builder.build();

            // Draw halo outline - 1px width
            frame.stroke(
                &halo,
                Stroke::default().with_color(halo_color).with_width(1.0),
            );
        }
    }

    /// Draw an X marker for dead players
    fn draw_x_marker(&self, frame: &mut Frame, x: f32, y: f32, size: f32, color: Color) {
        let half_size = size * 1.2;

        // Draw X shape with two diagonal lines
        let line1 = Path::line(
            Point::new(x - half_size, y - half_size),
            Point::new(x + half_size, y + half_size),
        );
        let line2 = Path::line(
            Point::new(x - half_size, y + half_size),
            Point::new(x + half_size, y - half_size),
        );

        frame.stroke(&line1, Stroke::default().with_color(color).with_width(2.0));
        frame.stroke(&line2, Stroke::default().with_color(color).with_width(2.0));
    }

    fn get_team_color(&self, team: &str, is_killer: bool, is_dead: bool) -> Color {
        let (r, g, b) = match team {
            "CT" => (0.3, 0.5, 0.9),
            "T" => (0.9, 0.5, 0.2),
            _ => (0.5, 0.5, 0.5),
        };

        let brightness = if is_killer { 1.3_f32 } else { 1.0_f32 };
        let opacity = if is_dead { 0.4 } else { 1.0 };

        Color::from_rgba(
            (r * brightness).min(1.0),
            (g * brightness).min(1.0),
            (b * brightness).min(1.0),
            opacity,
        )
    }

    /// Get the shooting state for the killer at the current tick
    fn get_killer_shot_state(
        &self,
        npz_data: &NpzData,
        tick_idx: usize,
        player_idx: usize,
    ) -> ShotState {
        if let Some(wf) = &npz_data.weapon_fire {
            if tick_idx >= npz_data.frames.len() {
                return ShotState::None;
            }

            let current_tick = npz_data.frames[tick_idx];

            // Check all shots for the killer at current frame
            for (i, &shot_tick) in wf.tick.iter().enumerate() {
                let attacker_idx = wf.attacker[i] as usize;

                // Only check killer's shots
                if attacker_idx != player_idx {
                    continue;
                }

                // Display shot 1 frame earlier
                let display_tick = shot_tick - 1;

                if current_tick == display_tick {
                    // Check if kill
                    let is_kill = wf.kill.get(i).map(|&k| k > 0).unwrap_or(false);
                    if is_kill {
                        return ShotState::Kill;
                    }

                    // Check if hit (has victims)
                    let start_offset = wf.offsets[i] as usize;
                    let end_offset = wf.offsets[i + 1] as usize;
                    if end_offset > start_offset {
                        return ShotState::Hit;
                    }

                    // Otherwise it's a miss
                    return ShotState::Miss;
                }
            }
        }

        ShotState::None
    }

    fn draw_tick_info(
        &self,
        frame: &mut Frame,
        tick: i32,
        tick_idx: usize,
        total_ticks: usize,
        _size: f32,
        theme: &Theme,
    ) {
        let info_bg = Path::rectangle(Point::new(5.0, 5.0), Size::new(180.0, 45.0));
        let bg_color = Color {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.7,
        };
        frame.fill(&info_bg, bg_color);

        let text_color = theme.text;
        frame.fill_text(canvas::Text {
            content: format!("Tick: {} / {}", tick_idx + 1, total_ticks),
            position: Point::new(10.0, 15.0),
            color: text_color,
            size: 12.0.into(),
            ..canvas::Text::default()
        });
        frame.fill_text(canvas::Text {
            content: format!("Time: {:.2}s", tick as f32 / 64.0),
            position: Point::new(10.0, 30.0),
            color: text_color,
            size: 12.0.into(),
            ..canvas::Text::default()
        });
    }

    /// Draw shoot lines for hits and misses using weapon fire data
    /// Only shows killer's shots, displayed 1 frame earlier than the fire event tick
    fn draw_shoot_lines(
        &self,
        frame: &mut Frame,
        npz_data: &NpzData,
        radar_config: Option<&RadarConfig>,
        size: f32,
    ) {
        // Number of frames to show hit lines with decay
        const DECAY_FRAMES: i32 = 8;

        if let Some(wf) = &npz_data.weapon_fire {
            // Ensure we have valid current tick
            if self.current_tick_idx >= npz_data.frames.len() {
                return;
            }

            let current_tick = npz_data.frames[self.current_tick_idx];

            // Find the killer's player index
            let killer_idx = npz_data
                .player_meta
                .iter()
                .position(|meta| meta.steamid.parse::<u64>().ok() == Some(npz_data.killer_steamid));

            if killer_idx.is_none() {
                return; // No killer found, don't draw any lines
            }
            let killer_player_idx = killer_idx.unwrap();

            // Iterate over all shots
            for (i, &shot_tick) in wf.tick.iter().enumerate() {
                let attacker_idx = wf.attacker[i] as usize;

                // Only draw lines for the killer's shots
                if attacker_idx != killer_player_idx {
                    continue;
                }

                // Display shot 1 frame earlier to accommodate timing (when fire = 1)
                let display_tick = shot_tick - 1;

                // Check if current tick is within decay range
                let frames_since_shot = current_tick - display_tick;
                if frames_since_shot < 0 || frames_since_shot >= DECAY_FRAMES {
                    continue; // Outside decay window
                }

                // Calculate opacity multiplier based on frame offset
                // Frame 0: 1.0, Frame 1: 0.67, Frame 2: 0.33, Frame 3: 0.0 (for DECAY_FRAMES = 4)
                let opacity_multiplier = if DECAY_FRAMES > 1 {
                    1.0 - (frames_since_shot as f32 / (DECAY_FRAMES - 1) as f32)
                } else {
                    1.0 // No decay if only 1 frame
                };

                // Get Attacker Pos at the display tick (1 frame before shot_tick)
                let shot_tick_idx = if let Some(idx) = npz_data.get_tick_index(display_tick) {
                    idx
                } else {
                    continue;
                };

                // Get attacker pos
                let attacker_pos =
                    if let Some(pos) = npz_data.get_player_position(shot_tick_idx, attacker_idx) {
                        pos
                    } else {
                        continue;
                    };

                let (ax, ay) = if let Some(config) = radar_config {
                    config.world_to_radar(attacker_pos.0, attacker_pos.1, size)
                } else {
                    self.world_to_radar_dynamic(attacker_pos.0, attacker_pos.1, size)
                };

                // ALWAYS draw yellow muzzle flash for every shot
                // Calculate muzzle flash end point for use by hit lines
                let (muzzle_end_x, muzzle_end_y) = if let Some((_, yaw)) =
                    npz_data.get_player_angles(shot_tick_idx, attacker_idx)
                {
                    // Correct Coordinate Conversion for Yaw
                    // Yaw 0 = East. Screen Rotation = -Yaw.
                    let angle = (-yaw).to_radians();

                    // Short muzzle flash (2% of map size - shorter to avoid overlap)
                    let flash_length = size * 0.02;

                    let flash_end_x = ax + angle.cos() * flash_length;
                    let flash_end_y = ay + angle.sin() * flash_length;

                    // Draw yellow muzzle flash (short, bright line) with decay
                    let flash_line =
                        Path::line(Point::new(ax, ay), Point::new(flash_end_x, flash_end_y));
                    let flash_color = Color {
                        r: 1.0,
                        g: 1.0,
                        b: 0.0,
                        a: 0.9 * opacity_multiplier,
                    };
                    frame.stroke(
                        &flash_line,
                        Stroke::default().with_color(flash_color).with_width(2.0),
                    );

                    (flash_end_x, flash_end_y)
                } else {
                    (ax, ay) // Fallback to player position if no angle
                };

                // Determine if Hit or Miss
                let start_offset = wf.offsets[i] as usize;
                let end_offset = wf.offsets[i + 1] as usize;

                if end_offset > start_offset {
                    // HIT(s) - Draw line from muzzle flash end to victim
                    // Check if this shot resulted in a kill
                    let is_kill = wf.kill.get(i).map(|&k| k > 0).unwrap_or(false);

                    for v_ptr in start_offset..end_offset {
                        let victim_idx = wf.victim_idx[v_ptr] as usize;
                        let impact_tick = wf.impact_tick[i];

                        // Get victim pos at impact (subtract 1 frame for display timing)
                        let impact_display_tick = impact_tick - 1;
                        let impact_tick_idx = npz_data
                            .get_tick_index(impact_display_tick)
                            .unwrap_or(shot_tick_idx);

                        if let Some(victim_pos) =
                            npz_data.get_player_position(impact_tick_idx, victim_idx)
                        {
                            let (vx, vy) = if let Some(config) = radar_config {
                                config.world_to_radar(victim_pos.0, victim_pos.1, size)
                            } else {
                                self.world_to_radar_dynamic(victim_pos.0, victim_pos.1, size)
                            };

                            // Draw HIT line starting from muzzle flash end (no overlap) with decay
                            // Kill: Full bright red (255, 0, 0), thick (2.5px)
                            // Hit: Dimmer red, thin (1.5px)
                            let line = Path::line(
                                Point::new(muzzle_end_x, muzzle_end_y),
                                Point::new(vx, vy),
                            );
                            let (base_color, width) = if is_kill {
                                (
                                    Color {
                                        r: 1.0,
                                        g: 0.0,
                                        b: 0.0,
                                        a: 1.0,
                                    },
                                    2.5,
                                )
                            } else {
                                (
                                    Color {
                                        r: 0.7,
                                        g: 0.0,
                                        b: 0.0,
                                        a: 0.8,
                                    },
                                    1.5,
                                )
                            };
                            // Apply opacity decay
                            let color = Color {
                                r: base_color.r,
                                g: base_color.g,
                                b: base_color.b,
                                a: base_color.a * opacity_multiplier,
                            };
                            frame.stroke(
                                &line,
                                Stroke::default().with_color(color).with_width(width),
                            );
                        }
                    }
                }
                // No else needed - misses only show muzzle flash (yellow line above)
            }
        }
    }
}

impl Default for RadarView {
    fn default() -> Self {
        Self::new()
    }
}

/// A widget that overlays one element on top of another using Iced's Overlay system.
/// This guarantees the overlay element is drawn on top of the content element.
struct RadarContainer<'a, Message, Theme, Renderer> {
    content: Element<'a, Message, Theme, Renderer>,
    overlay: Element<'a, Message, Theme, Renderer>,
}

impl<'a, Message, Theme, Renderer> RadarContainer<'a, Message, Theme, Renderer> {
    pub fn new(
        content: impl Into<Element<'a, Message, Theme, Renderer>>,
        overlay: impl Into<Element<'a, Message, Theme, Renderer>>,
    ) -> Self {
        Self {
            content: content.into(),
            overlay: overlay.into(),
        }
    }
}

impl<'a, Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for RadarContainer<'a, Message, Theme, Renderer>
where
    Renderer: renderer::Renderer,
{
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn layout(
        &self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        // Layout the content
        self.content
            .as_widget()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        // Draw the content (Image)
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }

    // Create the overlay for the Canvas
    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'_>,
        _renderer: &Renderer,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        // Position of the content
        let position = layout.bounds().position() + translation;

        // Return an overlay element that wraps our overlay element (Canvas)
        Some(overlay::Element::new(Box::new(OverlayWrapper {
            element: &mut self.overlay,
            tree: &mut tree.children[1],
            size: layout.bounds().size(),
            position,
        })))
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content), Tree::new(&self.overlay)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&[&self.content, &self.overlay]);
    }

    fn operate(
        &self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn widget::Operation<Message>,
    ) {
        self.content
            .as_widget()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn on_event(
        &mut self,
        tree: &mut Tree,
        event: iced::Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn iced::advanced::clipboard::Clipboard,
        shell: &mut iced::advanced::Shell<'_, Message>,
        viewport: &Rectangle,
    ) -> event::Status {
        // Only content handles main events
        self.content.as_widget_mut().on_event(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        )
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }
}

impl<'a, Message, Theme, Renderer> From<RadarContainer<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: 'a,
    Renderer: renderer::Renderer + 'a,
{
    fn from(container: RadarContainer<'a, Message, Theme, Renderer>) -> Self {
        Element::new(container)
    }
}

// Wrapper to adapt an Element into an Overlay trait implementation
struct OverlayWrapper<'a, 'b, Message, Theme, Renderer> {
    element: &'b mut Element<'a, Message, Theme, Renderer>,
    tree: &'b mut Tree,
    size: Size,
    position: Point,
}

impl<'a, 'b, Message, Theme, Renderer> overlay::Overlay<Message, Theme, Renderer>
    for OverlayWrapper<'a, 'b, Message, Theme, Renderer>
where
    Renderer: renderer::Renderer,
{
    fn layout(
        &mut self, // Changed to &mut self
        renderer: &Renderer,
        _bounds: Size,
    ) -> layout::Node {
        // We want to layout our overlay (Canvas) to match the size of the container content.
        let limits = layout::Limits::new(Size::ZERO, self.size);
        let node = self
            .element
            .as_widget()
            .layout(self.tree, renderer, &limits);
        node.move_to(self.position)
    }

    fn draw(
        &self,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
    ) {
        self.element.as_widget().draw(
            self.tree,
            renderer,
            theme,
            style,
            layout,
            cursor,
            &layout.bounds(),
        );
    }

    fn operate(
        &mut self,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn widget::Operation<Message>,
    ) {
        self.element
            .as_widget()
            .operate(self.tree, layout, renderer, operation);
    }

    fn on_event(
        &mut self,
        event: iced::Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn iced::advanced::clipboard::Clipboard,
        shell: &mut iced::advanced::Shell<'_, Message>,
    ) -> event::Status {
        self.element.as_widget_mut().on_event(
            self.tree,
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            &layout.bounds(),
        )
    }

    fn mouse_interaction(
        &self,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.element
            .as_widget()
            .mouse_interaction(self.tree, layout, cursor, viewport, renderer)
    }
}
