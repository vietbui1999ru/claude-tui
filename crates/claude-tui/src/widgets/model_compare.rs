use crate::format::format_tokens_comma;
use crate::theme;
use claude_common::{ModelStats, ModelsCompareResponse};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Cell, Gauge, Paragraph, Row, Table},
};

pub fn render_model_compare(frame: &mut Frame, area: Rect, data: &ModelsCompareResponse) {
    let chunks = Layout::vertical([
        Constraint::Percentage(35), // comparison table
        Constraint::Percentage(25), // usage distribution
        Constraint::Percentage(40), // token breakdown
    ])
    .split(area);

    render_comparison_table(frame, chunks[0], &data.models);
    render_usage_distribution(frame, chunks[1], &data.models);
    render_token_breakdown(frame, chunks[2], &data.models);
}

fn render_comparison_table(frame: &mut Frame, area: Rect, models: &[ModelStats]) {
    let block = Block::default()
        .title(" Model Comparison ")
        .borders(Borders::ALL)
        .border_style(Style::new().fg(theme::COLOR_SECONDARY));

    let header = Row::new(vec![
        "Model",
        "Total Tokens",
        "Avg/Session",
        "Total Cost",
        "Avg Cost/Req",
    ])
    .style(Style::new().fg(theme::COLOR_PRIMARY).bold())
    .bottom_margin(1);

    let rows: Vec<Row> = models
        .iter()
        .map(|s| {
            let color = theme::model_color(&s.model);
            let total_tokens = s.total_input_tokens + s.total_output_tokens;
            Row::new(vec![
                Cell::from(s.model.to_string()).style(Style::new().fg(color)),
                Cell::from(format_tokens_comma(total_tokens)),
                Cell::from(format!(
                    "{}",
                    format_tokens_comma(s.avg_input_per_request as u64 + s.avg_output_per_request as u64)
                )),
                Cell::from(format!("${:.2}", s.total_cost_usd)),
                Cell::from(format!("${:.3}", s.avg_cost_per_request)),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Percentage(15),
            Constraint::Percentage(20),
            Constraint::Percentage(20),
            Constraint::Percentage(20),
            Constraint::Percentage(25),
        ],
    )
    .header(header)
    .block(block);

    frame.render_widget(table, area);
}

fn render_usage_distribution(frame: &mut Frame, area: Rect, models: &[ModelStats]) {
    let block = Block::default()
        .title(" Usage Distribution ")
        .borders(Borders::ALL)
        .border_style(Style::new().fg(theme::COLOR_SECONDARY));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if models.is_empty() {
        frame.render_widget(
            Paragraph::new("No data").style(Style::new().fg(theme::COLOR_TEXT_DIM)),
            inner,
        );
        return;
    }

    let total_cost: f64 = models.iter().map(|s| s.total_cost_usd).sum();

    let constraints: Vec<Constraint> = models.iter().map(|_| Constraint::Length(1)).collect();
    let gauge_areas = Layout::vertical(constraints).split(inner);

    for (i, stat) in models.iter().enumerate() {
        if i >= gauge_areas.len() {
            break;
        }
        let pct = if total_cost > 0.0 {
            stat.total_cost_usd / total_cost
        } else {
            0.0
        };
        let color = theme::model_color(&stat.model);
        let gauge = Gauge::default()
            .ratio(pct.min(1.0))
            .label(Span::styled(
                format!("{}: {}%", stat.model, (pct * 100.0) as u32),
                Style::new().fg(Color::White),
            ))
            .gauge_style(Style::new().fg(color).bg(Color::Rgb(40, 40, 50)));
        frame.render_widget(gauge, gauge_areas[i]);
    }
}

fn render_token_breakdown(frame: &mut Frame, area: Rect, models: &[ModelStats]) {
    let block = Block::default()
        .title(" Token Breakdown per Model ")
        .borders(Borders::ALL)
        .border_style(Style::new().fg(theme::COLOR_SECONDARY));

    let header = Row::new(vec!["Model", "Input", "Output", "Total"])
        .style(Style::new().fg(theme::COLOR_PRIMARY).bold())
        .bottom_margin(1);

    let rows: Vec<Row> = models
        .iter()
        .map(|s| {
            let color = theme::model_color(&s.model);
            Row::new(vec![
                Cell::from(s.model.to_string()).style(Style::new().fg(color)),
                Cell::from(format_tokens_comma(s.total_input_tokens)),
                Cell::from(format_tokens_comma(s.total_output_tokens)),
                Cell::from(format_tokens_comma(s.total_input_tokens + s.total_output_tokens)),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Percentage(15),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(35),
        ],
    )
    .header(header)
    .block(block);

    frame.render_widget(table, area);
}
