use crate::format::format_tokens_short;
use crate::theme;
use claude_common::DailyAggregate;
use ratatui::{
    prelude::*,
    widgets::{Bar, BarChart, BarGroup, Block, Borders, Paragraph, Tabs},
};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeRangeSelection {
    SevenDays,
    ThirtyDays,
    NinetyDays,
}

impl TimeRangeSelection {
    pub fn days(&self) -> i64 {
        match self {
            TimeRangeSelection::SevenDays => 7,
            TimeRangeSelection::ThirtyDays => 30,
            TimeRangeSelection::NinetyDays => 90,
        }
    }

    pub fn index(&self) -> usize {
        match self {
            TimeRangeSelection::SevenDays => 0,
            TimeRangeSelection::ThirtyDays => 1,
            TimeRangeSelection::NinetyDays => 2,
        }
    }
}

pub fn render_token_chart(
    frame: &mut Frame,
    area: Rect,
    data: &[DailyAggregate],
    time_range: TimeRangeSelection,
) {
    let chunks = Layout::vertical([
        Constraint::Length(1), // time range selector
        Constraint::Min(6),   // chart
        Constraint::Length(1), // legend
    ])
    .split(area);

    // Time range selector
    let range_tabs = Tabs::new(vec!["7d", "30d", "90d"])
        .select(time_range.index())
        .highlight_style(theme::STYLE_TAB_ACTIVE)
        .style(theme::STYLE_TAB_INACTIVE)
        .divider(" ");
    let range_line = Layout::horizontal([Constraint::Fill(1), Constraint::Length(20)])
        .split(chunks[0]);
    frame.render_widget(
        Paragraph::new("  Token Usage").style(Style::new().fg(theme::COLOR_PRIMARY).bold()),
        range_line[0],
    );
    frame.render_widget(range_tabs, range_line[1]);

    if data.is_empty() {
        let empty = Paragraph::new("No data available")
            .style(Style::new().fg(theme::COLOR_TEXT_DIM))
            .alignment(Alignment::Center);
        frame.render_widget(empty, chunks[1]);
        return;
    }

    // Aggregate per-date: sum across models
    let today = chrono::Utc::now().date_naive();
    let cutoff = today - chrono::Duration::days(time_range.days());

    let mut daily: BTreeMap<chrono::NaiveDate, (u64, u64, u64)> = BTreeMap::new();
    for agg in data {
        if agg.date < cutoff {
            continue;
        }
        let entry = daily.entry(agg.date).or_default();
        entry.0 += agg.total_input_tokens;
        entry.1 += agg.total_output_tokens;
        entry.2 += agg.total_cache_read_tokens + agg.total_cache_write_tokens;
    }

    let max_val = daily
        .values()
        .map(|(i, o, c)| i + o + c)
        .max()
        .unwrap_or(1);

    // Build bars: 3 bars per date (input, output, cache), grouped visually
    let all_bars: Vec<Bar> = daily
        .iter()
        .flat_map(|(date, (input, output, cache))| {
            let label = date.format("%b %d").to_string();
            vec![
                Bar::default()
                    .value(*input)
                    .style(Style::new().fg(theme::COLOR_INPUT_TOKENS))
                    .text_value(format_tokens_short(*input))
                    .label(Line::from(label)),
                Bar::default()
                    .value(*output)
                    .style(Style::new().fg(theme::COLOR_OUTPUT_TOKENS))
                    .text_value(format_tokens_short(*output)),
                Bar::default()
                    .value(*cache)
                    .style(Style::new().fg(theme::COLOR_CACHE_READ_TOKENS))
                    .text_value(format_tokens_short(*cache)),
            ]
        })
        .collect();

    let chart = BarChart::default()
        .block(Block::default().borders(Borders::NONE))
        .data(BarGroup::default().bars(&all_bars))
        .bar_width(3)
        .bar_gap(0)
        .group_gap(2)
        .max(max_val);

    frame.render_widget(chart, chunks[1]);

    // Legend
    let legend = Line::from(vec![
        Span::styled("  ## ", Style::new().fg(theme::COLOR_INPUT_TOKENS)),
        Span::styled("Input  ", Style::new().fg(theme::COLOR_TEXT_DIM)),
        Span::styled("++ ", Style::new().fg(theme::COLOR_OUTPUT_TOKENS)),
        Span::styled("Output  ", Style::new().fg(theme::COLOR_TEXT_DIM)),
        Span::styled(".. ", Style::new().fg(theme::COLOR_CACHE_READ_TOKENS)),
        Span::styled("Cache", Style::new().fg(theme::COLOR_TEXT_DIM)),
    ]);
    frame.render_widget(Paragraph::new(legend), chunks[2]);
}
