use crate::client::DaemonClient;
use crate::format::format_tokens_short;
use crate::mock;
use crate::overview;
use crate::theme;
use crate::widgets::{cost_breakdown, live_monitor, model_compare, token_chart};
use chrono::Utc;
use claude_common::{
    ActiveSession, BudgetConfig, CollectorStatus, DailyAggregate, ModelsCompareResponse,
    SessionsListParams, StatusResponse, UsageSummaryParams, UsageSummaryResponse,
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
    Overview,
    Tokens,
    Costs,
    Models,
    Live,
}

impl Tab {
    const ALL: &[Tab] = &[
        Tab::Overview,
        Tab::Tokens,
        Tab::Costs,
        Tab::Models,
        Tab::Live,
    ];

    fn title(&self) -> &'static str {
        match self {
            Tab::Overview => "Overview",
            Tab::Tokens => "Tokens",
            Tab::Costs => "Costs",
            Tab::Models => "Models",
            Tab::Live => "Live",
        }
    }

    fn index(&self) -> usize {
        match self {
            Tab::Overview => 0,
            Tab::Tokens => 1,
            Tab::Costs => 2,
            Tab::Models => 3,
            Tab::Live => 4,
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
    Demo,
}

pub struct App {
    client: DaemonClient,
    /// Mock data is only ever shown with `--demo` (AC-11).
    demo: bool,
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
    /// Build the app.
    ///
    /// Without `demo`, every field starts empty and only daemon data fills it:
    /// a down daemon leaves the app with nothing to show, never with mock
    /// numbers.
    pub async fn new(client: DaemonClient, demo: bool) -> Self {
        let mut app = Self {
            client,
            demo,
            current_tab: Tab::Overview,
            should_quit: false,
            connection_status: ConnectionStatus::Disconnected,
            status: empty_status(),
            budget: BudgetConfig::default(),
            daily_aggregates: Vec::new(),
            usage_summary: empty_summary(),
            sessions: Vec::new(),
            model_stats: ModelsCompareResponse { models: Vec::new() },
            project_costs: Vec::new(),
            time_range: token_chart::TimeRangeSelection::Week,
            project_scroll: 0,
            session_selected: 0,
            spinner_tick: 0,
            last_data_refresh: Instant::now(),
        };

        app.refresh_data().await;
        app
    }

    pub async fn run(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let tick_rate = Duration::from_millis(100); // 100ms for spinner animation
        let data_refresh_interval = Duration::from_secs(30);

        loop {
            terminal.draw(|frame| self.render(frame))?;

            let timeout = tick_rate;
            if event::poll(timeout)?
                && let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
            {
                self.handle_key(key.code).await;
            }

            // Spinner tick (every 100ms)
            self.spinner_tick = self.spinner_tick.wrapping_add(1);

            // Periodic data refresh
            if self.last_data_refresh.elapsed() >= data_refresh_interval {
                self.refresh_data().await;
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
                self.refresh_data().await;
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
                self.time_range = token_chart::TimeRangeSelection::Week;
            }
            KeyCode::Char('2') if self.current_tab == Tab::Tokens => {
                self.time_range = token_chart::TimeRangeSelection::Month;
            }
            KeyCode::Char('3') if self.current_tab == Tab::Tokens => {
                self.time_range = token_chart::TimeRangeSelection::Quarter;
            }
            KeyCode::Up if self.current_tab == Tab::Costs => {
                self.project_scroll = self.project_scroll.saturating_sub(1);
            }
            KeyCode::Up if self.current_tab == Tab::Live => {
                self.session_selected = self.session_selected.saturating_sub(1);
            }
            KeyCode::Down
                if self.current_tab == Tab::Costs
                    && self.project_scroll + 1 < self.project_costs.len() =>
            {
                self.project_scroll += 1;
            }
            KeyCode::Down
                if self.current_tab == Tab::Live
                    && self.session_selected + 1 < self.sessions.len() =>
            {
                self.session_selected += 1;
            }
            _ => {}
        }
    }

    /// Pull daemon data, or mock data in demo mode.
    ///
    /// A failure never substitutes mock data (AC-11): it sets the disconnected
    /// state, which the UI renders as the unreachable panel, and the next tick
    /// retries the connection.
    async fn refresh_data(&mut self) {
        self.last_data_refresh = Instant::now();

        if self.demo {
            self.connection_status = ConnectionStatus::Demo;
            self.refresh_mock_data();
            return;
        }

        if !self.client.is_connected() && !self.client.reconnect().await {
            self.connection_status = ConnectionStatus::Disconnected;
            return;
        }

        match self.client.status().await {
            Ok(status) => {
                self.status = status;
                self.connection_status = ConnectionStatus::Connected;
            }
            Err(e) => {
                warn!("Failed to fetch status from daemon: {e}");
                self.connection_status = ConnectionStatus::Disconnected;
                return;
            }
        }

        if let Ok(budget) = self.client.get_budget().await {
            self.budget = budget;
        }

        let summary_params = UsageSummaryParams {
            time_range: None,
            window: claude_common::TimeWindow::Day,
            model: None,
        };
        if let Ok(summary) = self.client.get_summary(summary_params).await {
            self.daily_aggregates = summary.aggregates.clone();
            self.usage_summary = summary;
        }

        let session_params = SessionsListParams {
            status: None,
            limit: 50,
            offset: 0,
        };
        if let Ok(sessions_resp) = self.client.list_sessions(session_params).await {
            self.sessions = sessions_resp.sessions;
        }

        let compare_params = claude_common::ModelsCompareParams { time_range: None };
        if let Ok(model_stats) = self.client.compare_models(compare_params).await {
            self.model_stats = model_stats;
        }

        // Project costs have no daemon source yet: the Costs tab shows the empty
        // section rather than demo numbers.
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
            Constraint::Min(20),   // content
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
            ConnectionStatus::Demo => ("[Demo]", theme::COLOR_WARNING),
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
        if self.connection_status == ConnectionStatus::Disconnected {
            self.render_daemon_unreachable(frame, area);
            return;
        }

        // The Overview cards need the full width; the sidebar duplicates
        // numbers for the other tabs.
        let hide_sidebar = area.width < 100 || self.current_tab == Tab::Overview;

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
            let content_chunks =
                Layout::horizontal([Constraint::Percentage(70), Constraint::Percentage(30)])
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
            Tab::Overview => {
                overview::render_overview(frame, area, &self.status, Utc::now());
            }
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
                cost_breakdown::render_cost_breakdown(frame, area, &cost_data, self.project_scroll);
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
            Constraint::Min(0),    // fill
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
            .map(|m| theme::model_color(m.as_str()))
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
            Paragraph::new(format!(" ${:.2} / ${:.2}", monthly_spend, monthly_limit))
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

    fn render_daemon_unreachable(&self, frame: &mut Frame, area: Rect) {
        let block = Block::default()
            .title(" Daemon ")
            .borders(Borders::ALL)
            .border_style(Style::new().fg(theme::COLOR_ERROR));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let message = vec![
            Line::from(""),
            Line::from(Span::styled(
                "daemon unreachable — start `claude-daemon`",
                Style::new().fg(theme::COLOR_ERROR).bold(),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "retrying on every refresh tick · press r to retry now · q to quit",
                Style::new().fg(theme::COLOR_TEXT_DIM),
            )),
        ];
        frame.render_widget(Paragraph::new(message).alignment(Alignment::Center), inner);
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

fn compute_today_model_pcts(aggregates: &[DailyAggregate]) -> Vec<(String, u32)> {
    let today = Utc::now().date_naive();
    let today_aggs: Vec<&DailyAggregate> = aggregates.iter().filter(|a| a.date == today).collect();

    let total_tokens: u64 = today_aggs
        .iter()
        .map(|a| a.total_input_tokens + a.total_output_tokens)
        .sum();

    if total_tokens == 0 {
        return Vec::new();
    }

    let mut tokens_per_model: std::collections::BTreeMap<&str, u64> =
        std::collections::BTreeMap::new();
    for aggregate in &today_aggs {
        *tokens_per_model
            .entry(aggregate.model.as_str())
            .or_default() += aggregate.total_input_tokens + aggregate.total_output_tokens;
    }

    let mut model_pcts: Vec<(String, u32)> = tokens_per_model
        .into_iter()
        .map(|(model, tokens)| {
            let pct = (tokens as f64 / total_tokens as f64 * 100.0) as u32;
            (model.to_string(), pct)
        })
        .collect();

    model_pcts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    model_pcts
}

/// The no-data status, used before the first daemon answer and after the daemon
/// goes away. Deliberately empty: it holds no numbers a person could mistake for
/// real usage.
fn empty_status() -> StatusResponse {
    StatusResponse {
        daemon_uptime_secs: 0,
        active_sessions: 0,
        current_model: None,
        cost_today_usd: 0.0,
        budget_pct: None,
        collector_status: CollectorStatus::Offline,
        subscriptions: Vec::new(),
    }
}

fn empty_summary() -> UsageSummaryResponse {
    UsageSummaryResponse {
        aggregates: Vec::new(),
        total_cost_usd: 0.0,
        total_input_tokens: 0,
        total_output_tokens: 0,
        total_requests: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    use std::path::PathBuf;

    /// A socket path that cannot exist, so every connect attempt fails.
    fn absent_socket() -> PathBuf {
        std::env::temp_dir()
            .join(format!("claude-tui-app-{}", std::process::id()))
            .join("absent.sock")
    }

    fn disconnected_client() -> DaemonClient {
        DaemonClient::disconnected(&absent_socket())
    }

    fn buffer_text(app: &mut App) -> String {
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| app.render(frame))
            .expect("test render");
        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..buffer.area().height {
            for x in 0..buffer.area().width {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        text
    }

    #[tokio::test]
    async fn no_mock_without_demo() {
        let mut app = App::new(disconnected_client(), false).await;
        app.refresh_data().await;

        assert_eq!(app.connection_status, ConnectionStatus::Disconnected);
        assert_eq!(app.current_tab, Tab::Overview);

        // No field holds a mock value: every one is empty or neutral.
        let mock_status = mock::mock_status();
        assert!(app.status.subscriptions.is_empty());
        assert_ne!(app.status.cost_today_usd, mock_status.cost_today_usd);
        assert_eq!(app.status.cost_today_usd, 0.0);
        assert_eq!(app.status.active_sessions, 0);
        assert!(matches!(
            app.status.collector_status,
            CollectorStatus::Offline
        ));

        assert!(app.daily_aggregates.is_empty());
        assert!(app.sessions.is_empty());
        assert!(app.model_stats.models.is_empty());
        assert!(app.project_costs.is_empty());
        assert_eq!(app.usage_summary.total_cost_usd, 0.0);

        let mock_budget = mock::mock_budget();
        assert_ne!(app.budget.daily_limit_usd, mock_budget.daily_limit_usd);
        assert_eq!(app.budget.daily_limit_usd, None);
        assert_eq!(app.budget.monthly_limit_usd, None);
    }

    #[tokio::test]
    async fn no_mock_without_demo_renders_the_unreachable_panel() {
        let mut app = App::new(disconnected_client(), false).await;
        app.refresh_data().await;

        let text = buffer_text(&mut app);
        assert!(text.contains("daemon unreachable — start `claude-daemon`"));
        assert!(!text.contains("claude-opus-5-5"));
        assert!(!text.contains("opencode-go"));
    }

    #[tokio::test]
    async fn no_mock_without_demo_demo_mode_still_uses_mock_data() {
        let mut app = App::new(disconnected_client(), true).await;
        app.refresh_data().await;

        assert_eq!(app.connection_status, ConnectionStatus::Demo);
        assert_eq!(app.status.subscriptions.len(), 3);
        assert_eq!(
            app.status.cost_today_usd,
            mock::mock_status().cost_today_usd
        );

        let text = buffer_text(&mut app);
        assert!(text.contains("opencode-go"), "{text}");
        assert!(!text.contains("daemon unreachable"));
    }
}
