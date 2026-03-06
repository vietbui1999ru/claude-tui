use crate::client::DaemonClient;
use crate::format::format_tokens_short;
use crate::mock;
use crate::theme;
use crate::widgets::{cost_breakdown, live_monitor, model_compare, token_chart};
use chrono::Utc;
use claude_common::{
    ActiveSession, BudgetConfig, DailyAggregate, ModelType,
    ModelsCompareResponse, SessionsListParams, StatusResponse, UsageSummaryParams,
    UsageSummaryResponse,
};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Gauge, Paragraph, Tabs},
};
use std::time::{Duration, Instant};
use tracing::warn;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Tokens,
    Costs,
    Models,
    Live,
}

impl Tab {
    const ALL: &[Tab] = &[Tab::Tokens, Tab::Costs, Tab::Models, Tab::Live];

    fn title(&self) -> &'static str {
        match self {
            Tab::Tokens => "Tokens",
            Tab::Costs => "Costs",
            Tab::Models => "Models",
            Tab::Live => "Live",
        }
    }

    fn index(&self) -> usize {
        match self {
            Tab::Tokens => 0,
            Tab::Costs => 1,
            Tab::Models => 2,
            Tab::Live => 3,
        }
    }

    fn from_index(i: usize) -> Self {
        Tab::ALL[i % Tab::ALL.len()]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectionStatus {
    Connected,
    Disconnected,
}

pub struct App {
    client: Option<DaemonClient>,
    current_tab: Tab,
    should_quit: bool,
    connection_status: ConnectionStatus,

    // Data
    status: StatusResponse,
    budget: BudgetConfig,
    daily_aggregates: Vec<DailyAggregate>,
    usage_summary: UsageSummaryResponse,
    sessions: Vec<ActiveSession>,
    model_stats: ModelsCompareResponse,
    project_costs: Vec<mock::MockProjectCost>,

    // UI state
    time_range: token_chart::TimeRangeSelection,
    project_scroll: usize,
    session_selected: usize,
    spinner_tick: usize,
    last_data_refresh: Instant,
}

impl App {
    pub async fn new(client: Option<DaemonClient>) -> Self {
        let connected = client.is_some();
        let mut app = Self {
            client,
            current_tab: Tab::Tokens,
            should_quit: false,
            connection_status: if connected {
                ConnectionStatus::Connected
            } else {
                ConnectionStatus::Disconnected
            },
            status: mock::mock_status(),
            budget: mock::mock_budget(),
            daily_aggregates: mock::mock_daily_aggregates(),
            usage_summary: mock::mock_usage_summary(),
            sessions: mock::mock_sessions(),
            model_stats: mock::mock_model_stats(),
            project_costs: mock::mock_project_costs(),
            time_range: token_chart::TimeRangeSelection::SevenDays,
            project_scroll: 0,
            session_selected: 0,
            spinner_tick: 0,
            last_data_refresh: Instant::now(),
        };

        // If connected, try fetching real data
        if connected {
            app.refresh_data_from_daemon().await;
        }

        app
    }

    pub async fn run(&mut self, terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>) -> Result<(), Box<dyn std::error::Error>> {
        let tick_rate = Duration::from_millis(100); // 100ms for spinner animation
        let data_refresh_interval = Duration::from_secs(30);

        loop {
            terminal.draw(|frame| self.render(frame))?;

            let timeout = tick_rate;
            if event::poll(timeout)? {
                if let Event::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press {
                        self.handle_key(key.code).await;
                    }
                }
            }

            // Spinner tick (every 100ms)
            self.spinner_tick = self.spinner_tick.wrapping_add(1);

            // Periodic data refresh
            if self.last_data_refresh.elapsed() >= data_refresh_interval {
                self.refresh_data_from_daemon().await;
            }

            if self.should_quit {
                return Ok(());
            }
        }
    }

    async fn handle_key(&mut self, key: KeyCode) {
        match key {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('r') => {
                self.refresh_data_from_daemon().await;
            }
            KeyCode::Left => {
                let idx = self.current_tab.index();
                self.current_tab = if idx == 0 {
                    Tab::from_index(Tab::ALL.len() - 1)
                } else {
                    Tab::from_index(idx - 1)
                };
            }
            KeyCode::Right => {
                let idx = self.current_tab.index();
                self.current_tab = Tab::from_index(idx + 1);
            }
            KeyCode::Char('1') if self.current_tab == Tab::Tokens => {
                self.time_range = token_chart::TimeRangeSelection::SevenDays;
            }
            KeyCode::Char('2') if self.current_tab == Tab::Tokens => {
                self.time_range = token_chart::TimeRangeSelection::ThirtyDays;
            }
            KeyCode::Char('3') if self.current_tab == Tab::Tokens => {
                self.time_range = token_chart::TimeRangeSelection::NinetyDays;
            }
            KeyCode::Up => match self.current_tab {
                Tab::Costs => {
                    self.project_scroll = self.project_scroll.saturating_sub(1);
                }
                Tab::Live => {
                    self.session_selected = self.session_selected.saturating_sub(1);
                }
                _ => {}
            },
            KeyCode::Down => match self.current_tab {
                Tab::Costs => {
                    if self.project_scroll + 1 < self.project_costs.len() {
                        self.project_scroll += 1;
                    }
                }
                Tab::Live => {
                    if self.session_selected + 1 < self.sessions.len() {
                        self.session_selected += 1;
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }

    async fn refresh_data_from_daemon(&mut self) {
        self.last_data_refresh = Instant::now();

        let client = match self.client.as_ref() {
            Some(c) if c.is_connected() => c,
            _ => {
                // No client or not connected — use mock data
                self.connection_status = ConnectionStatus::Disconnected;
                self.refresh_mock_data();
                return;
            }
        };

        // Try fetching real data; fall back to mock on any failure
        match client.status().await {
            Ok(status) => {
                self.status = status;
                self.connection_status = ConnectionStatus::Connected;
            }
            Err(e) => {
                warn!("Failed to fetch status from daemon: {e}");
                self.connection_status = ConnectionStatus::Disconnected;
                self.refresh_mock_data();
                return;
            }
        }

        if let Ok(budget) = client.get_budget().await {
            self.budget = budget;
        }

        let summary_params = UsageSummaryParams {
            time_range: None,
            window: claude_common::TimeWindow::Day,
            model: None,
        };
        if let Ok(summary) = client.get_summary(summary_params).await {
            self.daily_aggregates = summary.aggregates.clone();
            self.usage_summary = summary;
        }

        let session_params = SessionsListParams {
            status: None,
            limit: 50,
            offset: 0,
        };
        if let Ok(sessions_resp) = client.list_sessions(session_params).await {
            self.sessions = sessions_resp.sessions;
        }

        let compare_params = claude_common::ModelsCompareParams {
            time_range: None,
        };
        if let Ok(model_stats) = client.compare_models(compare_params).await {
            self.model_stats = model_stats;
        }

        // Project costs are not yet available from daemon — keep mock
    }

    fn refresh_mock_data(&mut self) {
        self.status = mock::mock_status();
        self.budget = mock::mock_budget();
        self.daily_aggregates = mock::mock_daily_aggregates();
        self.usage_summary = mock::mock_usage_summary();
        self.sessions = mock::mock_sessions();
        self.model_stats = mock::mock_model_stats();
        self.project_costs = mock::mock_project_costs();
    }

    fn render(&self, frame: &mut Frame) {
        let size = frame.area();

        // Check minimum terminal size
        if size.width < 80 || size.height < 20 {
            let msg = Paragraph::new("Terminal too small.\nMinimum: 80x20")
                .style(Style::new().fg(theme::COLOR_ERROR).bold())
                .alignment(Alignment::Center);
            frame.render_widget(msg, size);
            return;
        }

        // Main vertical layout: Header(1), TabBar(1), Content(Min(20)), Footer(1)
        let main_chunks = Layout::vertical([
            Constraint::Length(1), // header
            Constraint::Length(1), // tab bar
            Constraint::Min(20),  // content
            Constraint::Length(1), // footer
        ])
        .split(size);

        self.render_header(frame, main_chunks[0]);
        self.render_tab_bar(frame, main_chunks[1]);
        self.render_content(frame, main_chunks[2]);
        self.render_footer(frame, main_chunks[3]);
    }

    fn render_header(&self, frame: &mut Frame, area: Rect) {
        let now = Utc::now();
        let time_str = now.format("%Y-%m-%d %H:%M").to_string();

        let (status_text, status_color) = match self.connection_status {
            ConnectionStatus::Connected => ("[Connected]", theme::COLOR_SUCCESS),
            ConnectionStatus::Disconnected => ("[Disconnected]", theme::COLOR_ERROR),
        };

        let left = Span::styled(
            " Claude Usage Monitor",
            Style::new().fg(theme::COLOR_PRIMARY).bold(),
        );
        let right = Line::from(vec![
            Span::styled(status_text, Style::new().fg(status_color)),
            Span::raw("  "),
            Span::styled(time_str, Style::new().fg(theme::COLOR_SECONDARY)),
            Span::raw(" "),
        ]);

        // Two-column layout for header
        let header_layout = Layout::horizontal([
            Constraint::Fill(1),
            Constraint::Length(right.width() as u16),
        ])
        .split(area);

        frame.render_widget(Paragraph::new(left), header_layout[0]);
        frame.render_widget(
            Paragraph::new(right).alignment(Alignment::Right),
            header_layout[1],
        );
    }

    fn render_tab_bar(&self, frame: &mut Frame, area: Rect) {
        let titles: Vec<&str> = Tab::ALL.iter().map(|t| t.title()).collect();
        let tabs = Tabs::new(titles)
            .select(self.current_tab.index())
            .highlight_style(theme::STYLE_TAB_ACTIVE)
            .style(theme::STYLE_TAB_INACTIVE)
            .divider("  ");
        frame.render_widget(tabs, area);
    }

    fn render_content(&self, frame: &mut Frame, area: Rect) {
        let hide_sidebar = area.width < 100;

        if hide_sidebar {
            // Full width content only
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::new().fg(theme::COLOR_SECONDARY));
            let inner = block.inner(area);
            frame.render_widget(block, area);
            self.render_tab_content(frame, inner);
        } else {
            // 70/30 split
            let content_chunks = Layout::horizontal([
                Constraint::Percentage(70),
                Constraint::Percentage(30),
            ])
            .split(area);

            // Main content
            let main_block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::new().fg(theme::COLOR_SECONDARY));
            let main_inner = main_block.inner(content_chunks[0]);
            frame.render_widget(main_block, content_chunks[0]);
            self.render_tab_content(frame, main_inner);

            // Sidebar
            self.render_sidebar(frame, content_chunks[1]);
        }
    }

    fn render_tab_content(&self, frame: &mut Frame, area: Rect) {
        match self.current_tab {
            Tab::Tokens => {
                token_chart::render_token_chart(
                    frame,
                    area,
                    &self.daily_aggregates,
                    self.time_range,
                );
            }
            Tab::Costs => {
                let cost_data = cost_breakdown::CostData {
                    model_stats: &self.model_stats.models,
                    project_costs: &self.project_costs,
                    summary: &self.usage_summary,
                    budget: &self.budget,
                    monthly_spend: self.usage_summary.total_cost_usd,
                };
                cost_breakdown::render_cost_breakdown(
                    frame,
                    area,
                    &cost_data,
                    self.project_scroll,
                );
            }
            Tab::Models => {
                model_compare::render_model_compare(frame, area, &self.model_stats);
            }
            Tab::Live => {
                live_monitor::render_live_monitor(
                    frame,
                    area,
                    &self.sessions,
                    self.session_selected,
                    self.spinner_tick,
                );
            }
        }
    }

    fn render_sidebar(&self, frame: &mut Frame, area: Rect) {
        let block = Block::default()
            .title(" Today's Summary ")
            .borders(Borders::ALL)
            .border_style(Style::new().fg(theme::COLOR_SECONDARY));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let chunks = Layout::vertical([
            Constraint::Length(1), // Model
            Constraint::Length(1), // Sessions
            Constraint::Length(1), // Tokens
            Constraint::Length(1), // Cost
            Constraint::Length(1), // spacer
            Constraint::Length(1), // Budget (Daily) label
            Constraint::Length(1), // daily gauge
            Constraint::Length(1), // daily detail
            Constraint::Length(1), // spacer
            Constraint::Length(1), // Budget (Monthly) label
            Constraint::Length(1), // monthly gauge
            Constraint::Length(1), // monthly detail
            Constraint::Length(1), // spacer
            Constraint::Length(1), // Top Model Today label
            Constraint::Length(1), // model 1
            Constraint::Length(1), // model 2
            Constraint::Length(1), // model 3
            Constraint::Min(0),   // fill
        ])
        .split(inner);

        let key_style = Style::new().fg(theme::COLOR_SECONDARY).bold();

        // Model
        let model_name = self
            .status
            .current_model
            .map(|m| m.to_string())
            .unwrap_or_else(|| "--".to_string());
        let model_color = self
            .status
            .current_model
            .map(|m| theme::model_color(&m))
            .unwrap_or(theme::COLOR_TEXT_DIM);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" Model: ", key_style),
                Span::styled(model_name, Style::new().fg(model_color)),
            ])),
            chunks[0],
        );

        // Sessions
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" Sessions: ", key_style),
                Span::raw(format!("{}", self.status.active_sessions)),
            ])),
            chunks[1],
        );

        // Tokens (sum today's aggregates)
        let today = Utc::now().date_naive();
        let today_tokens: u64 = self
            .daily_aggregates
            .iter()
            .filter(|a| a.date == today)
            .map(|a| a.total_input_tokens + a.total_output_tokens)
            .sum();
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" Tokens: ", key_style),
                Span::raw(format_tokens_short(today_tokens)),
            ])),
            chunks[2],
        );

        // Cost
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" Cost: ", key_style),
                Span::raw(format!("${:.2}", self.status.cost_today_usd)),
            ])),
            chunks[3],
        );

        // Daily Budget
        frame.render_widget(
            Paragraph::new(Span::styled(" Budget (Daily)", key_style.bold())),
            chunks[5],
        );

        let daily_limit = self.budget.daily_limit_usd.unwrap_or(0.0);
        let daily_ratio = if daily_limit > 0.0 {
            (self.status.cost_today_usd / daily_limit).min(1.0)
        } else {
            0.0
        };
        let daily_color = theme::budget_color(daily_ratio);
        frame.render_widget(
            Gauge::default()
                .ratio(daily_ratio)
                .label(format!("{}%", (daily_ratio * 100.0) as u32))
                .gauge_style(Style::new().fg(daily_color).bg(Color::Rgb(40, 40, 50))),
            chunks[6],
        );
        frame.render_widget(
            Paragraph::new(format!(
                " ${:.2} / ${:.2}",
                self.status.cost_today_usd, daily_limit
            ))
            .style(Style::new().fg(theme::COLOR_TEXT_DIM)),
            chunks[7],
        );

        // Monthly Budget
        frame.render_widget(
            Paragraph::new(Span::styled(" Budget (Monthly)", key_style.bold())),
            chunks[9],
        );

        let monthly_limit = self.budget.monthly_limit_usd.unwrap_or(0.0);
        let monthly_spend = self.usage_summary.total_cost_usd;
        let monthly_ratio = if monthly_limit > 0.0 {
            (monthly_spend / monthly_limit).min(1.0)
        } else {
            0.0
        };
        let monthly_color = theme::budget_color(monthly_ratio);
        frame.render_widget(
            Gauge::default()
                .ratio(monthly_ratio)
                .label(format!("{}%", (monthly_ratio * 100.0) as u32))
                .gauge_style(Style::new().fg(monthly_color).bg(Color::Rgb(40, 40, 50))),
            chunks[10],
        );
        frame.render_widget(
            Paragraph::new(format!(
                " ${:.2} / ${:.2}",
                monthly_spend, monthly_limit
            ))
            .style(Style::new().fg(theme::COLOR_TEXT_DIM)),
            chunks[11],
        );

        // Top Model Today
        frame.render_widget(
            Paragraph::new(Span::styled(" Top Model Today", key_style.bold())),
            chunks[13],
        );

        // Compute model percentages from today's aggregates
        let today_by_model = compute_today_model_pcts(&self.daily_aggregates);
        for (i, (model, pct)) in today_by_model.iter().enumerate().take(3) {
            if 14 + i < chunks.len() {
                let color = theme::model_color(model);
                frame.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::styled(format!(" {}  ", model), Style::new().fg(color)),
                        Span::raw(format!("{}%", pct)),
                    ])),
                    chunks[14 + i],
                );
            }
        }
    }

    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        let hints = Line::from(vec![
            Span::styled(" q", Style::new().bold()),
            Span::styled(": Quit  ", Style::new().fg(theme::COLOR_TEXT_DIM)),
            Span::styled("r", Style::new().bold()),
            Span::styled(": Refresh  ", Style::new().fg(theme::COLOR_TEXT_DIM)),
            Span::styled("<-/->", Style::new().bold()),
            Span::styled(": Tabs  ", Style::new().fg(theme::COLOR_TEXT_DIM)),
            Span::styled("Up/Down", Style::new().bold()),
            Span::styled(": Scroll  ", Style::new().fg(theme::COLOR_TEXT_DIM)),
            Span::styled("?", Style::new().bold()),
            Span::styled(": Help", Style::new().fg(theme::COLOR_TEXT_DIM)),
        ]);
        frame.render_widget(Paragraph::new(hints), area);
    }
}

fn compute_today_model_pcts(aggregates: &[DailyAggregate]) -> Vec<(ModelType, u32)> {
    let today = Utc::now().date_naive();
    let today_aggs: Vec<&DailyAggregate> = aggregates.iter().filter(|a| a.date == today).collect();

    let total_tokens: u64 = today_aggs
        .iter()
        .map(|a| a.total_input_tokens + a.total_output_tokens)
        .sum();

    if total_tokens == 0 {
        return vec![
            (ModelType::Sonnet, 0),
            (ModelType::Opus, 0),
            (ModelType::Haiku, 0),
        ];
    }

    let mut model_pcts: Vec<(ModelType, u32)> = Vec::new();
    for model in &[ModelType::Opus, ModelType::Sonnet, ModelType::Haiku] {
        let tokens: u64 = today_aggs
            .iter()
            .filter(|a| a.model == *model)
            .map(|a| a.total_input_tokens + a.total_output_tokens)
            .sum();
        let pct = (tokens as f64 / total_tokens as f64 * 100.0) as u32;
        model_pcts.push((*model, pct));
    }

    model_pcts.sort_by(|a, b| b.1.cmp(&a.1));
    model_pcts
}
