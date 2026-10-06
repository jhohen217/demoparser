//! Playbar widget for scrubbing through round replay

use crate::message::Message;
use crate::style;
use crate::theme::Theme;
use iced::widget::{button, checkbox, container, row, slider, text, Column};
use iced::{Element, Length};

pub struct PlaybarState {
    pub playing: bool,
    pub current_tick_idx: usize,
    pub total_ticks: usize,
    pub playback_speed: f32,
    pub kill_ticks: Vec<i32>,
    pub loop_kill_region: bool,
}

impl PlaybarState {
    pub fn new() -> Self {
        Self {
            playing: false,
            current_tick_idx: 0,
            total_ticks: 0,
            playback_speed: 1.0,
            kill_ticks: Vec::new(),
            loop_kill_region: false,
        }
    }

    pub fn set_total_ticks(&mut self, total: usize) {
        self.total_ticks = total;
        if self.current_tick_idx >= total && total > 0 {
            self.current_tick_idx = total - 1;
        }
    }

    pub fn set_kill_ticks(&mut self, ticks: Vec<i32>) {
        self.kill_ticks = ticks;
    }

    pub fn advance_tick(&mut self) {
        if self.playing && self.total_ticks > 0 {
            self.current_tick_idx = (self.current_tick_idx + 1) % self.total_ticks;
        }
    }

    pub fn reset(&mut self) {
        self.playing = false;
        self.current_tick_idx = 0;
        self.total_ticks = 0;
        self.kill_ticks.clear();
    }
}

impl Default for PlaybarState {
    fn default() -> Self {
        Self::new()
    }
}

/// Create playbar view
pub fn view<'a>(
    state: &PlaybarState,
    theme: &Theme,
) -> Element<'a, Message, Theme, iced::Renderer> {
    if state.total_ticks == 0 {
        // No data loaded - show placeholder
        return container(
            text("No replay data loaded. Select a collection to view round replay.")
                .size(13)
                .style(Some(theme.text)),
        )
        .padding(10)
        .width(Length::Fill)
        .style(style::Container::Panel)
        .into();
    }

    // Play/Pause button
    let play_pause_btn = button(text(if state.playing { "⏸" } else { "▶" }).size(14))
        .on_press(Message::TogglePlayback)
        .padding([4, 8])
        .style(style::Button::Embossed);

    // Speed buttons
    let speed_05x = button(text("0.5x").size(11))
        .on_press(Message::SetPlaybackSpeed(0.5))
        .padding([4, 6])
        .style(if (state.playback_speed - 0.5).abs() < 0.01 {
            style::Button::FilterActive
        } else {
            style::Button::Embossed
        });

    let speed_1x = button(text("1x").size(11))
        .on_press(Message::SetPlaybackSpeed(1.0))
        .padding([4, 6])
        .style(if (state.playback_speed - 1.0).abs() < 0.01 {
            style::Button::FilterActive
        } else {
            style::Button::Embossed
        });

    let speed_2x = button(text("2x").size(11))
        .on_press(Message::SetPlaybackSpeed(2.0))
        .padding([4, 6])
        .style(if (state.playback_speed - 2.0).abs() < 0.01 {
            style::Button::FilterActive
        } else {
            style::Button::Embossed
        });

    let speed_4x = button(text("4x").size(11))
        .on_press(Message::SetPlaybackSpeed(4.0))
        .padding([4, 6])
        .style(if (state.playback_speed - 4.0).abs() < 0.01 {
            style::Button::FilterActive
        } else {
            style::Button::Embossed
        });

    // Tick info
    let tick_info = text(format!(
        "Tick: {} / {} ({:.1}s / {:.1}s)",
        state.current_tick_idx + 1,
        state.total_ticks,
        (state.current_tick_idx as f32) / 64.0,
        (state.total_ticks as f32) / 64.0
    ))
    .size(12)
    .style(Some(theme.text));

    // Loop Kill-Region checkbox
    let loop_checkbox = checkbox("Loop Kill-Region", state.loop_kill_region)
        .on_toggle(Message::ToggleLoopKillRegion)
        .size(12)
        .text_size(12)
        .style(style::Checkbox::Primary);

    // Timeline slider - convert usize to f64 for slider
    let slider_widget = if state.total_ticks > 0 {
        slider(
            0.0..=(state.total_ticks.saturating_sub(1) as f64),
            state.current_tick_idx as f64,
            |value| Message::ScrubTimeline(value as usize),
        )
        .step(1.0)
        .width(Length::Fill)
    } else {
        slider(0.0..=0.0, 0.0, |_| Message::Noop)
            .step(1.0)
            .width(Length::Fill)
    };

    // Controls row
    let controls = row![
        play_pause_btn,
        speed_05x,
        speed_1x,
        speed_2x,
        speed_4x,
        tick_info,
        loop_checkbox,
    ]
    .spacing(8)
    .padding([5, 10]);

    // Timeline row with kill markers (visual representation)
    let timeline_row = container(slider_widget)
        .padding([0, 10])
        .width(Length::Fill);

    // Combine into column
    let content = Column::new()
        .push(controls)
        .push(timeline_row)
        .width(Length::Fill)
        .spacing(0);

    container(content)
        .width(Length::Fill)
        .style(style::Container::Panel)
        .padding(5)
        .into()
}
