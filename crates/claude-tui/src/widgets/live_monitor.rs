use crate::format::{format_duration_short, format_tokens_comma};
use crate::theme;
use claude_common::{ActiveSession, SessionStatus};
use chrono::Utc;
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Cell, Paragraph, Row, Table},
};

const SPINNER_FRAMES: &[&str] = &[".", "..", "...", "..", ".", " "];

pub fn render_live_monitor(
    frame: &mut Frame,
    area: Rect,
    sessions: &[ActiveSession],
    selected: usize,
    spinner_tick: usize,
) {
    if sessions.is_empty() {
        render_empty_state(frame, area);
        return;
    }

    let chunks = Layout::vertical([
        Constraint::Percentage(45), // session list
        Constraint::Percentage(55), // session detail
    ])
    .split(area);

    render_session_list(frame, chunks[0], sessions, selected, spinner_tick);
    render_session_detail(frame, chunks[1], sessions, selected, spinner_tick);
}

fn render_empty_state(frame: &mut Frame, area: Rect) {
    let block = Block::default()
        .title(" Live Sessions                                         0 active ")
        .borders(Borders::ALL)
        .border_style(Style::new().fg(theme::COLOR_SECONDARY));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let center_v = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .split(inner);

    frame.render_widget(
        Paragraph::new("No active sessions")
            .style(Style::new().fg(theme::COLOR_PRIMARY).bold())
            .alignment(Alignment::Center),
        center_v[1],
    );
    frame.render_widget(
        Paragraph::new("Waiting for Claude activity...")
            .style(Style::new().fg(theme::COLOR_TEXT_DIM))
            .alignment(Alignment::Center),
        center_v[2],
    );
}

fn render_session_list(
    frame: &mut Frame,
    area: Rect,
    sessions: &[ActiveSession],
    selected: usize,
    spinner_tick: usize,
) {
    let title = format!(
        " Live Sessions                                         {} active ",
        sessions
            .iter()
            .filter(|s| s.status == SessionStatus::Streaming)
            .count()
    );
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::new().fg(theme::COLOR_SECONDARY));

    let header = Row::new(vec!["#", "Session", "Model", "Status", "Tokens", "Duration"])
        .style(Style::new().fg(theme::COLOR_PRIMARY).bold())
        .bottom_margin(1);

    let now = Utc::now();
    let rows: Vec<Row> = sessions
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let duration = (now - s.started_at).num_seconds();
            let status_text = match s.status {
                SessionStatus::Streaming => {
                    let frame = SPINNER_FRAMES[spinner_tick % SPINNER_FRAMES.len()];
                    format!("Streaming {}", frame)
                }
                SessionStatus::Idle => "Idle".to_string(),
                SessionStatus::Completed => "Completed".to_string(),
            };
            let status_color = match s.status {
                SessionStatus::Streaming => theme::COLOR_SUCCESS,
                SessionStatus::Idle => theme::COLOR_WARNING,
                SessionStatus::Completed => theme::COLOR_TEXT_DIM,
            };
            let model_color = theme::model_color(&s.model);

            let style = if i == selected {
                Style::new().bg(Color::Rgb(50, 60, 80))
            } else {
                Style::new()
            };

            Row::new(vec![
                Cell::from(format!("{}", i + 1)),
                Cell::from(s.session_id.clone()),
                Cell::from(s.model.to_string()).style(Style::new().fg(model_color)),
                Cell::from(status_text).style(Style::new().fg(status_color)),
                Cell::from(format_tokens_comma(
                    s.total_input_tokens + s.total_output_tokens,
                )),
                Cell::from(format_duration_short(duration)),
            ])
            .style(style)
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(3),
            Constraint::Percentage(30),
            Constraint::Percentage(12),
            Constraint::Percentage(18),
            Constraint::Percentage(15),
            Constraint::Percentage(15),
        ],
    )
    .header(header)
    .block(block);

    frame.render_widget(table, area);
}

fn render_session_detail(
    frame: &mut Frame,
    area: Rect,
    sessions: &[ActiveSession],
    selected: usize,
    spinner_tick: usize,
) {
    let session = match sessions.get(selected) {
        Some(s) => s,
        None => return,
    };

    let title = format!(" Session Detail: {} ", session.session_id);
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::new().fg(theme::COLOR_SECONDARY));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let now = Utc::now();
    let duration = (now - session.started_at).num_seconds();
    let status_str = match session.status {
        SessionStatus::Streaming => {
            let frame = SPINNER_FRAMES[spinner_tick % SPINNER_FRAMES.len()];
            format!("Streaming {}", frame)
        }
        SessionStatus::Idle => "Idle".to_string(),
        SessionStatus::Completed => "Completed".to_string(),
    };

    let key_style = Style::new().fg(theme::COLOR_SECONDARY).bold();
    let val_style = Style::new();

    let lines = vec![
        Line::from(vec![
            Span::styled("  Model:    ", key_style),
            Span::styled(
                session.model.to_string(),
                Style::new().fg(theme::model_color(&session.model)),
            ),
        ]),
        Line::from(vec![
            Span::styled("  Status:   ", key_style),
            Span::styled(status_str, val_style),
        ]),
        Line::from(vec![
            Span::styled("  Started:  ", key_style),
            Span::styled(
                session.started_at.format("%H:%M:%S UTC").to_string(),
                val_style,
            ),
        ]),
        Line::from(vec![
            Span::styled("  Duration: ", key_style),
            Span::styled(format_duration_short(duration), val_style),
        ]),
        Line::from(vec![
            Span::styled("  Input:    ", key_style),
            Span::styled(
                format!("{} tokens", format_tokens_comma(session.total_input_tokens)),
                val_style,
            ),
        ]),
        Line::from(vec![
            Span::styled("  Output:   ", key_style),
            Span::styled(
                format!(
                    "{} tokens{}",
                    format_tokens_comma(session.total_output_tokens),
                    if session.status == SessionStatus::Streaming {
                        " (streaming...)"
                    } else {
                        ""
                    }
                ),
                val_style,
            ),
        ]),
        Line::from(vec![
            Span::styled("  Est Cost: ", key_style),
            Span::styled(format!("${:.3}", session.cost_usd), val_style),
        ]),
        Line::from(vec![
            Span::styled("  Requests: ", key_style),
            Span::styled(format!("{}", session.request_count), val_style),
        ]),
    ];

    if let Some(ref proj) = session.project {
        let mut all_lines = vec![Line::from(vec![
            Span::styled("  Project:  ", key_style),
            Span::styled(proj.clone(), val_style),
        ])];
        all_lines.extend(lines);
        frame.render_widget(Paragraph::new(all_lines), inner);
    } else {
        frame.render_widget(Paragraph::new(lines), inner);
    }
}
