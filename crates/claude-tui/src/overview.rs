//! The Overview tab: one card per subscription, built from daemon data (AC-11).
//!
//! Every card shows whatever the daemon could observe. Missing data renders as
//! `--` or `no data`, never as a made-up number: mock values exist only in
//! `--demo` mode, and that decision is made in [`crate::app`].

use chrono::{DateTime, Local, Utc};
use claude_common::{CostSource, QuotaWindow, StatusResponse, Subscription, SubscriptionStatus};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Gauge, Paragraph, Wrap},
};

use crate::format::format_tokens_short;
use crate::theme;
use crate::windows::labelled_windows;

/// Render the Overview into `area`.
pub fn render_overview(frame: &mut Frame, area: Rect, status: &StatusResponse, now: DateTime<Utc>) {
    if status.subscriptions.is_empty() {
        let block = panel_block(" Overview ");
        let inner = block.inner(area);
        frame.render_widget(block, area);
        frame.render_widget(
            Paragraph::new("no data — the daemon has not collected anything yet")
                .style(Style::new().fg(theme::COLOR_TEXT_DIM))
                .alignment(Alignment::Center)
                .wrap(Wrap { trim: true }),
            inner,
        );
        return;
    }

    let count = status.subscriptions.len() as u32;
    let constraints = vec![Constraint::Ratio(1, count); status.subscriptions.len()];
    let columns = Layout::horizontal(constraints).split(area);

    for (subscription, column) in status.subscriptions.iter().zip(columns.iter()) {
        render_card(frame, *column, subscription, now);
    }
}

fn panel_block(title: &str) -> Block<'_> {
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::new().fg(theme::COLOR_SECONDARY))
        .title_style(Style::new().fg(theme::COLOR_PRIMARY).bold())
}

fn render_card(
    frame: &mut Frame,
    area: Rect,
    subscription: &SubscriptionStatus,
    now: DateTime<Utc>,
) {
    let title = format!(" {} ", subscription_name(subscription.subscription));
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::new().fg(theme::COLOR_SECONDARY))
        .title_style(Style::new().fg(theme::COLOR_PRIMARY).bold());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let windows = labelled_windows(&subscription.quota);

    let mut constraints = Vec::with_capacity(windows.len() * 2 + 6);
    for _ in &windows {
        constraints.push(Constraint::Length(1)); // gauge
        constraints.push(Constraint::Length(1)); // reset time
    }
    constraints.push(Constraint::Length(1)); // today
    constraints.push(Constraint::Length(1)); // last 7d
    constraints.push(Constraint::Length(1)); // last limit hit
    constraints.push(Constraint::Length(2)); // sources
    constraints.push(Constraint::Min(0)); // fill
    let rows = Layout::vertical(constraints).split(inner);

    let mut row = 0;
    for (label, window) in windows {
        render_gauge(frame, rows[row], label, window);
        row += 1;
        frame.render_widget(
            Paragraph::new(reset_line(window, now)).style(Style::new().fg(theme::COLOR_TEXT_DIM)),
            rows[row],
        );
        row += 1;
    }

    frame.render_widget(
        Paragraph::new(totals_line("today", &subscription.today)),
        rows[row],
    );
    row += 1;
    frame.render_widget(
        Paragraph::new(totals_line("7d", &subscription.last_7d)),
        rows[row],
    );
    row += 1;
    frame.render_widget(Paragraph::new(limit_line(subscription)), rows[row]);
    row += 1;
    frame.render_widget(
        Paragraph::new(sources_line(subscription, now))
            .style(Style::new().fg(theme::COLOR_TEXT_DIM))
            .wrap(Wrap { trim: true }),
        rows[row],
    );
}

fn render_gauge(frame: &mut Frame, area: Rect, label: &str, window: &QuotaWindow) {
    let ratio = (window.used_pct.unwrap_or(0.0) / 100.0).clamp(0.0, 1.0);
    let pct = match window.used_pct {
        Some(pct) => format!("{}%", pct.round() as i64),
        None => "--".to_string(),
    };
    let stale = if window.stale { "?" } else { "" };
    let color = if window.expired {
        theme::COLOR_TEXT_DIM
    } else {
        theme::budget_color(ratio)
    };
    frame.render_widget(
        Gauge::default()
            .ratio(ratio)
            .label(format!("{label} {pct}{stale}"))
            .gauge_style(Style::new().fg(color).bg(Color::Rgb(40, 40, 50))),
        area,
    );
}

fn reset_line(window: &QuotaWindow, now: DateTime<Utc>) -> String {
    if window.expired {
        return "  expired".to_string();
    }
    let mut line = match window.resets_at {
        Some(resets_at) => format!("  resets {}", format_reset(resets_at, now)),
        None => "  resets unknown".to_string(),
    };
    if window.stale {
        line.push_str(" · stale");
    }
    line
}

/// `14:05` inside a day, `Mon 14:05` further out.
fn format_reset(resets_at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let local = resets_at.with_timezone(&Local);
    let within_a_day = resets_at.signed_duration_since(now) < chrono::Duration::hours(24);
    if within_a_day {
        local.format("%H:%M").to_string()
    } else {
        local.format("%a %H:%M").to_string()
    }
}

fn totals_line(label: &str, totals: &claude_common::TokenTotals) -> String {
    let tokens = totals.input + totals.output;
    format!(
        "  {label} {} {}",
        format_tokens_short(tokens),
        cost_text(totals)
    )
}

/// Computed costs are the API-equivalent price of the tokens, so they carry the
/// `≈` marker (SPEC Code Style).
fn cost_text(totals: &claude_common::TokenTotals) -> String {
    match (totals.cost_usd, totals.cost_source) {
        (Some(cost), CostSource::Computed) => format!("≈API ${cost:.2}"),
        (Some(cost), _) => format!("${cost:.2}"),
        (None, _) => "cost unknown".to_string(),
    }
}

fn limit_line(subscription: &SubscriptionStatus) -> String {
    match &subscription.last_quota_hit {
        Some(hit) => {
            let at = hit
                .reset_at
                .unwrap_or(hit.observed_at)
                .with_timezone(&Local);
            let period = hit.period.as_deref().unwrap_or("limit");
            format!("  hit {period} {}", at.format("%m-%d %H:%M"))
        }
        None => "  hit --".to_string(),
    }
}

/// Collector ids with the age of their newest record. A stale source carries
/// the same `?` marker the status line uses.
fn sources_line(subscription: &SubscriptionStatus, now: DateTime<Utc>) -> String {
    if subscription.sources.is_empty() {
        return "  src --".to_string();
    }
    let parts: Vec<String> = subscription
        .sources
        .iter()
        .map(|source| match source.last_update {
            Some(last_update) => {
                let age = format_age(now.signed_duration_since(last_update));
                if source.stale {
                    format!("{} {age}?", source.collector_id)
                } else {
                    format!("{} {age}", source.collector_id)
                }
            }
            None => format!("{} never", source.collector_id),
        })
        .collect();
    format!("  src {}", parts.join(" · "))
}

fn format_age(age: chrono::Duration) -> String {
    let secs = age.num_seconds().max(0);
    if secs < 60 {
        "now".to_string()
    } else if secs < 3_600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3_600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

fn subscription_name(subscription: Subscription) -> &'static str {
    match subscription {
        Subscription::Claude => "claude",
        Subscription::ChatGpt => "chatgpt",
        Subscription::OpencodeGo => "opencode-go",
        Subscription::Other => "other",
    }
}
