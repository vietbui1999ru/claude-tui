use ratatui::style::{Color, Modifier, Style};

// -- Primary UI Colors --
pub const COLOR_PRIMARY: Color = Color::Rgb(130, 170, 255);
pub const COLOR_SECONDARY: Color = Color::Rgb(140, 140, 160);
pub const COLOR_TEXT_DIM: Color = Color::Rgb(100, 100, 120);

// -- Status Colors --
pub const COLOR_SUCCESS: Color = Color::Rgb(80, 200, 120);
pub const COLOR_WARNING: Color = Color::Rgb(240, 180, 50);
pub const COLOR_ERROR: Color = Color::Rgb(240, 80, 80);

// -- Model Colors --
pub const COLOR_OPUS: Color = Color::Rgb(200, 120, 255);
pub const COLOR_SONNET: Color = Color::Rgb(80, 180, 240);
pub const COLOR_HAIKU: Color = Color::Rgb(120, 220, 180);

// -- Token Type Colors --
pub const COLOR_INPUT_TOKENS: Color = Color::Rgb(80, 180, 240);
pub const COLOR_OUTPUT_TOKENS: Color = Color::Rgb(240, 160, 80);
pub const COLOR_CACHE_READ_TOKENS: Color = Color::Rgb(140, 200, 100);
#[allow(dead_code)]
pub const COLOR_CACHE_WRITE_TOKENS: Color = Color::Rgb(200, 140, 200);

// -- Budget Bar Gradient --
pub const COLOR_BUDGET_LOW: Color = Color::Rgb(80, 200, 120);
pub const COLOR_BUDGET_MED: Color = Color::Rgb(240, 180, 50);
pub const COLOR_BUDGET_HIGH: Color = Color::Rgb(240, 130, 50);
pub const COLOR_BUDGET_CRIT: Color = Color::Rgb(240, 80, 80);

// -- Tab Styles --
pub const STYLE_TAB_ACTIVE: Style = Style::new()
    .fg(Color::Rgb(255, 255, 255))
    .bg(Color::Rgb(80, 100, 160))
    .add_modifier(Modifier::BOLD);

pub const STYLE_TAB_INACTIVE: Style = Style::new().fg(Color::Rgb(140, 140, 160));

pub fn budget_color(pct: f64) -> Color {
    if pct < 0.50 {
        COLOR_BUDGET_LOW
    } else if pct < 0.75 {
        COLOR_BUDGET_MED
    } else if pct < 0.90 {
        COLOR_BUDGET_HIGH
    } else {
        COLOR_BUDGET_CRIT
    }
}

pub fn model_color(model: &claude_common::ModelType) -> Color {
    match model {
        claude_common::ModelType::Opus => COLOR_OPUS,
        claude_common::ModelType::Sonnet => COLOR_SONNET,
        claude_common::ModelType::Haiku => COLOR_HAIKU,
    }
}
