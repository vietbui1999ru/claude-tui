use crate::mock::MockProjectCost;
use crate::theme;
use claude_common::{BudgetConfig, ModelStats, UsageSummaryResponse};
use ratatui::{
    prelude::*,
    widgets::{
        Bar, BarChart, BarGroup, Block, Borders, Cell, Gauge, Paragraph, Row, Sparkline, Table,
    },
};

pub struct CostData<'a> {
    pub model_stats: &'a [ModelStats],
    pub project_costs: &'a [MockProjectCost],
    pub summary: &'a UsageSummaryResponse,
    pub budget: &'a BudgetConfig,
    pub monthly_spend: f64,
}

pub fn render_cost_breakdown(
    frame: &mut Frame,
    area: Rect,
    data: &CostData,
    project_scroll: usize,
) {
    let chunks = Layout::vertical([
        Constraint::Percentage(35), // top row: model breakdown + sparkline
        Constraint::Min(6),         // project table
        Constraint::Length(3),      // budget bar
    ])
    .split(area);

    // Top row: model breakdown (left) + cumulative sparkline (right)
    let top_row = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(chunks[0]);

    render_model_bars(frame, top_row[0], data.model_stats);
    render_cumulative_sparkline(frame, top_row[1], data.summary);
    render_project_table(frame, chunks[1], data.project_costs, project_scroll);
    render_budget_bar(frame, chunks[2], data.budget, data.monthly_spend);
}

fn render_model_bars(frame: &mut Frame, area: Rect, stats: &[ModelStats]) {
    let block = Block::default()
        .title(" By Model (Last 30d) ")
        .borders(Borders::ALL)
        .border_style(Style::new().fg(theme::COLOR_SECONDARY));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if stats.is_empty() {
        frame.render_widget(
            Paragraph::new("No data").style(Style::new().fg(theme::COLOR_TEXT_DIM)),
            inner,
        );
        return;
    }

    let total_cost: f64 = stats.iter().map(|s| s.total_cost_usd).sum();

    let bars: Vec<Bar> = stats
        .iter()
        .map(|s| {
            let pct = if total_cost > 0.0 {
                (s.total_cost_usd / total_cost * 100.0) as u64
            } else {
                0
            };
            let color = theme::model_color(&s.model);
            Bar::default()
                .value(pct)
                .label(Line::from(format!("{}", s.model)))
                .style(Style::new().fg(color))
                .text_value(format!("${:.2} ({}%)", s.total_cost_usd, pct))
        })
        .collect();

    let chart = BarChart::default()
        .data(BarGroup::default().bars(&bars))
        .bar_width(1)
        .bar_gap(1)
        .max(100)
        .direction(Direction::Horizontal);

    frame.render_widget(chart, inner);
}

fn render_cumulative_sparkline(frame: &mut Frame, area: Rect, summary: &UsageSummaryResponse) {
    let block = Block::default()
        .title(" Cumulative Spend ")
        .borders(Borders::ALL)
        .border_style(Style::new().fg(theme::COLOR_SECONDARY));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if summary.aggregates.is_empty() {
        frame.render_widget(
            Paragraph::new("No data").style(Style::new().fg(theme::COLOR_TEXT_DIM)),
            inner,
        );
        return;
    }

    // Build cumulative cost per date
    let mut daily_costs: Vec<f64> = Vec::new();
    let mut dates: Vec<chrono::NaiveDate> = summary.aggregates.iter().map(|a| a.date).collect();
    dates.sort();
    dates.dedup();

    let mut cumulative = 0.0;
    for date in &dates {
        let day_cost: f64 = summary
            .aggregates
            .iter()
            .filter(|a| a.date == *date)
            .map(|a| a.total_cost_usd)
            .sum();
        cumulative += day_cost;
        daily_costs.push(cumulative);
    }

    // Scale to u64 for sparkline (multiply by 100 for 2-decimal precision)
    let spark_data: Vec<u64> = daily_costs.iter().map(|c| (*c * 100.0) as u64).collect();

    let sparkline = Sparkline::default()
        .data(&spark_data)
        .style(Style::new().fg(theme::COLOR_PRIMARY));

    // Show total at top
    let label = Paragraph::new(format!("Total: ${:.2}", cumulative))
        .style(Style::new().fg(theme::COLOR_PRIMARY));

    let spark_layout =
        Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(inner);
    frame.render_widget(label, spark_layout[0]);
    frame.render_widget(sparkline, spark_layout[1]);
}

fn render_project_table(
    frame: &mut Frame,
    area: Rect,
    projects: &[MockProjectCost],
    scroll: usize,
) {
    let block = Block::default()
        .title(" By Project (Last 30d) ")
        .borders(Borders::ALL)
        .border_style(Style::new().fg(theme::COLOR_SECONDARY));

    let header = Row::new(vec!["Project", "Model", "Cost", "Pct"])
        .style(Style::new().fg(theme::COLOR_PRIMARY).bold())
        .bottom_margin(1);

    let rows: Vec<Row> = projects
        .iter()
        .skip(scroll)
        .map(|p| {
            let color = theme::model_color(&p.model);
            Row::new(vec![
                Cell::from(p.project.clone()),
                Cell::from(p.model.to_string()).style(Style::new().fg(color)),
                Cell::from(format!("${:.2}", p.cost)),
                Cell::from(format!("{}%", (p.pct * 100.0) as u32)),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Percentage(35),
            Constraint::Percentage(20),
            Constraint::Percentage(25),
            Constraint::Percentage(20),
        ],
    )
    .header(header)
    .block(block);

    frame.render_widget(table, area);
}

fn render_budget_bar(frame: &mut Frame, area: Rect, budget: &BudgetConfig, monthly_spend: f64) {
    let limit = budget.monthly_limit_usd.unwrap_or(0.0);
    let ratio = if limit > 0.0 {
        (monthly_spend / limit).min(1.0)
    } else {
        0.0
    };

    let color = theme::budget_color(ratio);
    let label = if limit > 0.0 {
        format!(
            "Budget: ${:.2} / ${:.2}  ({}%)",
            monthly_spend,
            limit,
            (ratio * 100.0) as u32
        )
    } else {
        format!("Budget: ${:.2} (no limit set)", monthly_spend)
    };

    let gauge = Gauge::default()
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::new().fg(theme::COLOR_SECONDARY)),
        )
        .gauge_style(Style::new().fg(color).bg(Color::Rgb(40, 40, 50)))
        .ratio(ratio)
        .label(Span::styled(label, Style::new().fg(Color::White).bold()));

    frame.render_widget(gauge, area);
}
