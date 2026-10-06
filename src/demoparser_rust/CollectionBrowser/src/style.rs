#[derive(Clone, Copy, Default)]
pub enum Button {
    #[default]
    Embossed,
    FilterActive,                     // Active filter with orange highlight
    FilterInactive,                   // Inactive filter (same as embossed but square)
    Header(bool),                     // (is_selected)
    TableRow(bool, bool),             // (is_selected, is_alt_row)
    Checkbox(bool, bool, bool),       // (is_checked, is_selected, is_alt_row)
    ControlPanelCheckbox(bool, bool), // (is_checked, is_hovered)
    ControlPanelLabel(bool),          // (is_hovered)
    Disabled,                         // Greyed out, non-interactive button
}

#[derive(Clone, Copy, Default)]
pub enum Container {
    #[default]
    Main,
    Panel,
    Table,
    Transparent,
    CheckboxSquare(bool), // is_white
    Separator(bool),      // Separator container style (is_hovered)
    Tooltip,
    LoadingBarBg,
}

#[derive(Clone, Copy, Default)]
pub struct Scrollable;

#[derive(Clone, Copy, Default)]
pub struct TextInput;

#[derive(Clone, Copy, Default)]
pub enum Checkbox {
    #[default]
    Primary,
}
