use crate::style;
use iced::application;
use iced::overlay::menu;
use iced::widget::{button, checkbox, container, pick_list, scrollable, slider, text, text_input};
use iced::{Background, Border, Color, Vector};

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    // Base colors
    pub background: Color,
    pub panel_background: Color,
    pub table_background: Color,
    pub text: Color,
    pub text_bright: Color,
    pub accent: Color,
    pub error_red: Color,

    // Button colors
    pub button_hover: Color,
    pub button_active: Color,
    pub button_border_light: Color,

    // Table colors
    pub table_header_bg: Color,
    pub table_row_bg_alt: Color,
    pub table_border: Color,

    // Scrollbar colors
    pub scroll_bg: Color,
    pub scroll_handle: Color,
    pub scroll_handle_hover: Color,
    pub scroll_handle_active: Color,

    // General border
    pub border: Color,
}

impl Theme {
    pub fn new() -> Self {
        Self {
            background: Color::from_rgb8(76, 76, 76),
            panel_background: Color::from_rgb8(62, 62, 62),
            table_background: Color::from_rgb8(30, 30, 30),
            text: Color::from_rgb8(222, 222, 222),
            text_bright: Color::from_rgb8(255, 255, 255),
            accent: Color::from_rgb8(255, 155, 0),
            error_red: Color::from_rgb8(255, 80, 80),

            button_hover: Color::from_rgb8(93, 92, 92),
            button_active: Color::from_rgb8(42, 41, 41),
            button_border_light: Color::from_rgb8(110, 110, 110),

            table_header_bg: Color::from_rgb8(62, 62, 62),
            table_row_bg_alt: Color::from_rgb8(35, 35, 35),
            table_border: Color::from_rgb8(83, 83, 83),

            scroll_bg: Color::from_rgb8(20, 20, 20),
            scroll_handle: Color::from_rgb8(160, 160, 160),
            scroll_handle_hover: Color::from_rgb8(180, 180, 180),
            scroll_handle_active: Color::from_rgb8(140, 140, 140),

            border: Color::from_rgb8(83, 83, 83),
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::new()
    }
}

impl application::StyleSheet for Theme {
    type Style = ();

    fn appearance(&self, _style: &Self::Style) -> application::Appearance {
        application::Appearance {
            background_color: self.background,
            text_color: self.text,
        }
    }
}

impl text::StyleSheet for Theme {
    type Style = Option<Color>;

    fn appearance(&self, style: Self::Style) -> text::Appearance {
        // Return None to allow inheriting color from parent (e.g. Button)
        // or falling back to Application default.
        text::Appearance { color: style }
    }
}

impl button::StyleSheet for Theme {
    type Style = style::Button;

    fn active(&self, style: &Self::Style) -> button::Appearance {
        match style {
            style::Button::Embossed => button::Appearance {
                background: None,
                text_color: self.text,
                border: Border {
                    color: Color::from_rgb8(180, 180, 180),
                    width: 1.0,
                    radius: 2.0.into(),
                },
                shadow_offset: Vector::ZERO,
                ..Default::default()
            },
            style::Button::FilterActive => button::Appearance {
                background: Some(Background::Color(self.accent)),
                text_color: Color::from_rgb8(40, 40, 40),
                border: Border {
                    color: self.accent,
                    width: 1.0,
                    radius: 0.0.into(),
                },
                shadow_offset: Vector::ZERO,
                ..Default::default()
            },
            style::Button::FilterInactive => button::Appearance {
                background: None,
                text_color: self.text,
                border: Border {
                    color: Color::from_rgb8(180, 180, 180),
                    width: 1.0,
                    radius: 0.0.into(),
                },
                shadow_offset: Vector::ZERO,
                ..Default::default()
            },
            style::Button::Header(is_selected) => button::Appearance {
                background: if *is_selected {
                    Some(Background::Color(self.accent))
                } else {
                    Some(Background::Color(self.table_header_bg))
                },
                text_color: if *is_selected {
                    Color::BLACK
                } else {
                    self.text
                },
                border: Border {
                    color: self.border,
                    width: 1.0,
                    radius: 0.0.into(),
                },
                ..Default::default()
            },
            style::Button::TableRow(is_selected, is_alt) => button::Appearance {
                background: if *is_selected {
                    Some(Background::Color(self.accent))
                } else if *is_alt {
                    Some(Background::Color(self.table_row_bg_alt))
                } else {
                    Some(Background::Color(self.table_background))
                },
                text_color: if *is_selected {
                    Color::BLACK
                } else {
                    self.text
                },
                border: Border {
                    color: Color::TRANSPARENT,
                    width: 0.0,
                    radius: 0.0.into(),
                },
                shadow_offset: Vector::ZERO,
                ..Default::default()
            },
            style::Button::Checkbox(_is_checked, is_selected, _is_alt) => button::Appearance {
                background: if *is_selected {
                    Some(Background::Color(self.table_background))
                } else {
                    None
                },
                border: Border {
                    color: Color::TRANSPARENT,
                    width: 0.0,
                    radius: 0.0.into(),
                },
                shadow_offset: Vector::ZERO,
                ..Default::default()
            },
            style::Button::ControlPanelCheckbox(_is_checked, is_hovered) => button::Appearance {
                background: None,
                border: Border {
                    color: if *is_hovered {
                        self.accent
                    } else {
                        Color::from_rgb8(180, 180, 180)
                    },
                    width: 1.0,
                    radius: 0.0.into(),
                },
                shadow_offset: Vector::ZERO,
                ..Default::default()
            },
            style::Button::ControlPanelLabel(is_hovered) => button::Appearance {
                background: None,
                text_color: if *is_hovered { self.accent } else { self.text },
                border: Border {
                    color: Color::TRANSPARENT,
                    width: 0.0,
                    radius: 0.0.into(),
                },
                shadow_offset: Vector::ZERO,
                ..Default::default()
            },
            style::Button::Disabled => button::Appearance {
                background: None,
                text_color: Color::from_rgb8(100, 100, 100),
                border: Border {
                    color: Color::from_rgb8(80, 80, 80),
                    width: 1.0,
                    radius: 2.0.into(),
                },
                shadow_offset: Vector::ZERO,
                ..Default::default()
            },
        }
    }

    fn hovered(&self, style: &Self::Style) -> button::Appearance {
        match style {
            style::Button::Embossed => button::Appearance {
                background: Some(Background::Color(Color::from_rgba8(255, 255, 255, 0.1))),
                text_color: self.text_bright,
                border: Border {
                    color: self.text_bright,
                    width: 1.0,
                    radius: 2.0.into(),
                },
                shadow_offset: Vector::ZERO,
                ..Default::default()
            },
            style::Button::FilterActive => button::Appearance {
                background: Some(Background::Color(self.accent)),
                text_color: Color::BLACK,
                border: Border {
                    color: self.accent,
                    width: 1.0,
                    radius: 0.0.into(),
                },
                shadow_offset: Vector::ZERO,
                ..Default::default()
            },
            style::Button::FilterInactive => button::Appearance {
                background: Some(Background::Color(Color::from_rgba8(255, 255, 255, 0.1))),
                text_color: self.text_bright,
                border: Border {
                    color: self.text_bright,
                    width: 1.0,
                    radius: 0.0.into(),
                },
                shadow_offset: Vector::ZERO,
                ..Default::default()
            },
            style::Button::Header(is_selected) => button::Appearance {
                background: if *is_selected {
                    Some(Background::Color(self.accent))
                } else {
                    Some(Background::Color(self.button_hover))
                },
                text_color: if *is_selected {
                    Color::BLACK
                } else {
                    self.text_bright
                },
                border: Border {
                    color: self.border,
                    width: 1.0,
                    radius: 0.0.into(),
                },
                ..Default::default()
            },
            style::Button::TableRow(is_selected, _is_alt) => button::Appearance {
                background: if *is_selected {
                    Some(Background::Color(self.accent))
                } else {
                    Some(Background::Color(self.button_hover))
                },
                text_color: if *is_selected {
                    Color::BLACK
                } else {
                    self.text_bright
                },
                border: Border {
                    color: Color::TRANSPARENT,
                    width: 0.0,
                    radius: 0.0.into(),
                },
                ..Default::default()
            },
            style::Button::Checkbox(_is_checked, is_selected, _is_alt) => button::Appearance {
                background: if *is_selected {
                    Some(Background::Color(self.table_background))
                } else {
                    None
                },
                border: Border {
                    color: Color::TRANSPARENT,
                    width: 0.0,
                    radius: 0.0.into(),
                },
                ..Default::default()
            },
            style::Button::ControlPanelCheckbox(_is_checked, _is_hovered) => button::Appearance {
                background: None,
                border: Border {
                    color: self.accent,
                    width: 1.0,
                    radius: 0.0.into(),
                },
                ..Default::default()
            },
            style::Button::ControlPanelLabel(_is_hovered) => button::Appearance {
                background: None,
                text_color: self.accent,
                border: Border {
                    color: Color::TRANSPARENT,
                    width: 0.0,
                    radius: 0.0.into(),
                },
                ..Default::default()
            },
            style::Button::Disabled => button::Appearance {
                background: None,
                text_color: Color::from_rgb8(100, 100, 100),
                border: Border {
                    color: Color::from_rgb8(80, 80, 80),
                    width: 1.0,
                    radius: 2.0.into(),
                },
                shadow_offset: Vector::ZERO,
                ..Default::default()
            },
        }
    }

    fn pressed(&self, style: &Self::Style) -> button::Appearance {
        match style {
            style::Button::Embossed => button::Appearance {
                background: Some(Background::Color(Color::from_rgba8(0, 0, 0, 0.2))),
                text_color: self.text_bright,
                border: Border {
                    color: self.text_bright,
                    width: 1.0,
                    radius: 2.0.into(),
                },
                shadow_offset: Vector::ZERO,
                ..Default::default()
            },
            style::Button::FilterActive => button::Appearance {
                background: Some(Background::Color(Color::from_rgb8(230, 140, 0))),
                text_color: Color::BLACK,
                border: Border {
                    color: self.accent,
                    width: 1.0,
                    radius: 0.0.into(),
                },
                shadow_offset: Vector::ZERO,
                ..Default::default()
            },
            style::Button::FilterInactive => button::Appearance {
                background: Some(Background::Color(Color::from_rgba8(0, 0, 0, 0.2))),
                text_color: self.text_bright,
                border: Border {
                    color: self.text_bright,
                    width: 1.0,
                    radius: 0.0.into(),
                },
                shadow_offset: Vector::ZERO,
                ..Default::default()
            },
            style::Button::Header(is_selected) => button::Appearance {
                background: if *is_selected {
                    Some(Background::Color(self.accent))
                } else {
                    Some(Background::Color(self.button_active))
                },
                text_color: if *is_selected {
                    Color::BLACK
                } else {
                    self.text_bright
                },
                border: Border {
                    color: self.border,
                    width: 1.0,
                    radius: 0.0.into(),
                },
                ..Default::default()
            },
            style::Button::TableRow(is_selected, _is_alt) => button::Appearance {
                background: if *is_selected {
                    Some(Background::Color(self.accent))
                } else {
                    Some(Background::Color(self.button_active))
                },
                text_color: if *is_selected {
                    Color::BLACK
                } else {
                    self.text_bright
                },
                border: Border {
                    color: Color::TRANSPARENT,
                    width: 0.0,
                    radius: 0.0.into(),
                },
                ..Default::default()
            },
            style::Button::Checkbox(_is_checked, is_selected, _is_alt) => button::Appearance {
                background: if *is_selected {
                    Some(Background::Color(self.table_background))
                } else {
                    None
                },
                border: Border {
                    color: Color::TRANSPARENT,
                    width: 0.0,
                    radius: 0.0.into(),
                },
                ..Default::default()
            },
            style::Button::ControlPanelCheckbox(_is_checked, _is_hovered) => button::Appearance {
                background: None,
                border: Border {
                    color: self.accent,
                    width: 1.0,
                    radius: 0.0.into(),
                },
                ..Default::default()
            },
            style::Button::ControlPanelLabel(_is_hovered) => button::Appearance {
                background: None,
                text_color: self.accent,
                border: Border {
                    color: Color::TRANSPARENT,
                    width: 0.0,
                    radius: 0.0.into(),
                },
                ..Default::default()
            },
            style::Button::Disabled => button::Appearance {
                background: None,
                text_color: Color::from_rgb8(100, 100, 100),
                border: Border {
                    color: Color::from_rgb8(80, 80, 80),
                    width: 1.0,
                    radius: 2.0.into(),
                },
                shadow_offset: Vector::ZERO,
                ..Default::default()
            },
        }
    }
}

impl container::StyleSheet for Theme {
    type Style = style::Container;

    fn appearance(&self, style: &Self::Style) -> container::Appearance {
        match style {
            style::Container::Main => container::Appearance {
                background: Some(Background::Color(self.background)),
                text_color: Some(self.text),
                ..Default::default()
            },
            style::Container::Panel => container::Appearance {
                background: Some(Background::Color(self.panel_background)),
                border: Border {
                    color: self.border,
                    width: 2.0,
                    radius: 0.0.into(),
                },
                ..Default::default()
            },
            style::Container::Table => container::Appearance {
                background: Some(Background::Color(self.table_background)),
                border: Border {
                    color: self.table_border,
                    width: 2.0,
                    radius: 0.0.into(),
                },
                ..Default::default()
            },
            style::Container::Transparent => container::Appearance {
                background: None,
                text_color: None,
                ..Default::default()
            },
            style::Container::CheckboxSquare(is_white) => container::Appearance {
                background: if *is_white {
                    Some(Background::Color(Color::from_rgb8(180, 180, 180)))
                } else {
                    None
                },
                ..Default::default()
            },
            style::Container::Separator(is_hovered) => container::Appearance {
                background: Some(Background::Color(if *is_hovered {
                    self.accent
                } else {
                    self.border
                })),
                border: Border {
                    color: Color::TRANSPARENT,
                    width: 0.0,
                    radius: 0.0.into(),
                },
                ..Default::default()
            },
            style::Container::Tooltip => container::Appearance {
                background: Some(Background::Color(Color::from_rgb8(25, 25, 25))),
                border: Border {
                    color: Color::from_rgb8(180, 180, 180),
                    width: 1.0,
                    radius: 2.0.into(),
                },
                text_color: Some(self.text_bright),
                ..Default::default()
            },
            style::Container::LoadingBarBg => container::Appearance {
                background: Some(Background::Color(Color::from_rgb8(40, 40, 40))),
                border: Border {
                    color: Color::from_rgb8(100, 100, 100),
                    width: 2.0,
                    radius: 0.0.into(),
                },
                ..Default::default()
            },
        }
    }
}

impl scrollable::StyleSheet for Theme {
    type Style = style::Scrollable;

    fn active(&self, _style: &Self::Style) -> scrollable::Appearance {
        scrollable::Appearance {
            container: container::Appearance {
                background: None,
                border: Border {
                    color: Color::TRANSPARENT,
                    width: 0.0,
                    radius: 0.0.into(),
                },
                ..Default::default()
            },
            scrollbar: scrollable::Scrollbar {
                background: Some(Background::Color(self.scroll_bg)),
                border: Border {
                    color: self.border,
                    width: 0.0,
                    radius: 0.0.into(),
                },
                scroller: scrollable::Scroller {
                    color: self.scroll_handle,
                    border: Border {
                        color: Color::TRANSPARENT,
                        width: 0.0,
                        radius: 0.0.into(),
                    },
                },
            },
            gap: None,
        }
    }

    fn hovered(
        &self,
        style: &Self::Style,
        is_mouse_over_scrollbar: bool,
    ) -> scrollable::Appearance {
        let active = self.active(style);
        if is_mouse_over_scrollbar {
            scrollable::Appearance {
                scrollbar: scrollable::Scrollbar {
                    scroller: scrollable::Scroller {
                        color: self.scroll_handle_hover,
                        ..active.scrollbar.scroller
                    },
                    ..active.scrollbar
                },
                ..active
            }
        } else {
            active
        }
    }

    fn dragging(&self, style: &Self::Style) -> scrollable::Appearance {
        let active = self.active(style);
        scrollable::Appearance {
            scrollbar: scrollable::Scrollbar {
                scroller: scrollable::Scroller {
                    color: self.scroll_handle_active,
                    ..active.scrollbar.scroller
                },
                ..active.scrollbar
            },
            ..active
        }
    }
}

impl text_input::StyleSheet for Theme {
    type Style = style::TextInput;

    fn active(&self, _style: &Self::Style) -> text_input::Appearance {
        text_input::Appearance {
            background: Background::Color(self.panel_background),
            border: Border {
                color: self.border,
                width: 2.0,
                radius: 0.0.into(),
            },
            icon_color: self.text,
        }
    }

    fn focused(&self, _style: &Self::Style) -> text_input::Appearance {
        text_input::Appearance {
            background: Background::Color(self.panel_background),
            border: Border {
                color: self.accent,
                width: 2.0,
                radius: 0.0.into(),
            },
            icon_color: self.text,
        }
    }

    fn placeholder_color(&self, _style: &Self::Style) -> Color {
        let mut color = self.text;
        color.a = 0.5;
        color
    }

    fn value_color(&self, _style: &Self::Style) -> Color {
        self.text
    }

    fn disabled_color(&self, _style: &Self::Style) -> Color {
        let mut color = self.text;
        color.a = 0.3;
        color
    }

    fn selection_color(&self, _style: &Self::Style) -> Color {
        self.accent
    }

    fn disabled(&self, style: &Self::Style) -> text_input::Appearance {
        self.active(style)
    }
}

impl checkbox::StyleSheet for Theme {
    type Style = style::Checkbox;

    fn active(&self, style: &Self::Style, _is_checked: bool) -> checkbox::Appearance {
        match style {
            style::Checkbox::Primary => checkbox::Appearance {
                background: Background::Color(Color::TRANSPARENT),
                icon_color: self.accent,
                border: Border {
                    color: Color::WHITE,
                    width: 1.0,
                    radius: 0.0.into(),
                },
                text_color: Some(self.text),
            },
        }
    }

    fn hovered(&self, style: &Self::Style, _is_checked: bool) -> checkbox::Appearance {
        match style {
            style::Checkbox::Primary => checkbox::Appearance {
                background: Background::Color(Color::TRANSPARENT),
                icon_color: self.accent,
                border: Border {
                    color: self.accent,
                    width: 1.0,
                    radius: 0.0.into(),
                },
                text_color: Some(self.text_bright),
            },
        }
    }
}

impl pick_list::StyleSheet for Theme {
    type Style = ();

    fn active(&self, _style: &Self::Style) -> pick_list::Appearance {
        pick_list::Appearance {
            text_color: self.text,
            placeholder_color: self.text,
            handle_color: self.text,
            background: Background::Color(self.panel_background),
            border: Border {
                color: self.border,
                width: 2.0,
                radius: 0.0.into(),
            },
        }
    }

    fn hovered(&self, _style: &Self::Style) -> pick_list::Appearance {
        pick_list::Appearance {
            text_color: self.text_bright,
            placeholder_color: self.text_bright,
            handle_color: self.text_bright,
            background: Background::Color(self.panel_background),
            border: Border {
                color: self.accent,
                width: 2.0,
                radius: 0.0.into(),
            },
        }
    }
}

impl menu::StyleSheet for Theme {
    type Style = ();

    fn appearance(&self, _style: &Self::Style) -> menu::Appearance {
        menu::Appearance {
            text_color: self.text,
            background: Background::Color(self.panel_background),
            border: Border {
                color: self.border,
                width: 2.0,
                radius: 0.0.into(),
            },
            selected_text_color: Color::BLACK,
            selected_background: Background::Color(self.accent),
        }
    }
}

impl slider::StyleSheet for Theme {
    type Style = ();

    fn active(&self, _style: &Self::Style) -> slider::Appearance {
        slider::Appearance {
            rail: slider::Rail {
                colors: (self.border, self.panel_background),
                width: 4.0,
                border_radius: 0.0.into(),
            },
            handle: slider::Handle {
                shape: slider::HandleShape::Circle { radius: 8.0 },
                color: self.accent,
                border_color: self.text_bright,
                border_width: 1.0,
            },
        }
    }

    fn hovered(&self, _style: &Self::Style) -> slider::Appearance {
        slider::Appearance {
            rail: slider::Rail {
                colors: (self.accent, self.panel_background),
                width: 4.0,
                border_radius: 0.0.into(),
            },
            handle: slider::Handle {
                shape: slider::HandleShape::Circle { radius: 9.0 },
                color: self.accent,
                border_color: self.text_bright,
                border_width: 2.0,
            },
        }
    }

    fn dragging(&self, _style: &Self::Style) -> slider::Appearance {
        slider::Appearance {
            rail: slider::Rail {
                colors: (self.accent, self.panel_background),
                width: 4.0,
                border_radius: 0.0.into(),
            },
            handle: slider::Handle {
                shape: slider::HandleShape::Circle { radius: 9.0 },
                color: Color::from_rgb8(230, 140, 0),
                border_color: self.text_bright,
                border_width: 2.0,
            },
        }
    }
}
