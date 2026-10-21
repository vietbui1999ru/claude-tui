use chrono::Utc;
use claude_common::{
    ActiveSession, BudgetConfig, CollectorStatus, CostSource, DailyAggregate, DataSource,
    ModelStats, ModelType, ModelsCompareResponse, PriceBook, QuotaHitStatus, QuotaWindow,
    SessionStatus, SourceStatus, StatusResponse, Subscription, SubscriptionStatus, TokenTotals,
    UsageSummaryResponse,
};

const DEMO_OPUS: &str = "claude-opus-5-5";
const DEMO_SONNET: &str = "claude-sonnet-5-5";
const DEMO_HAIKU: &str = "claude-haiku-5-5";

pub fn mock_status() -> StatusResponse {
    StatusResponse {
        daemon_uptime_secs: 3600,
        active_sessions: 2,
        current_model: Some(ModelType::Sonnet),
        cost_today_usd: 3.47,
        budget_pct: Some(0.67),
        collector_status: CollectorStatus::Log,
        subscriptions: mock_subscriptions(),
    }
}

/// Demo cards for the Overview tab. Only shown with `--demo` (AC-11).
pub fn mock_subscriptions() -> Vec<SubscriptionStatus> {
    let now = Utc::now();
    let stale_now = now - chrono::Duration::days(2);

    let claude = SubscriptionStatus {
        subscription: Subscription::Claude,
        quota: vec![
            QuotaWindow {
                window: "five_hour:live".to_string(),
                used_pct: Some(34.0),
                resets_at: Some(now + chrono::Duration::minutes(95)),
                source: DataSource::ClaudeQuota,
                observed_at: now,
                stale: false,
                expired: false,
            },
            QuotaWindow {
                window: "seven_day:live".to_string(),
                used_pct: Some(12.0),
                resets_at: Some(now + chrono::Duration::days(4)),
                source: DataSource::ClaudeQuota,
                observed_at: now,
                stale: false,
                expired: false,
            },
        ],
        today: mock_totals(1_240_000, 310_000, 9.10, CostSource::Computed),
        last_7d: mock_totals(8_400_000, 2_100_000, 61.40, CostSource::Computed),
        by_tool: Vec::new(),
        last_quota_hit: None,
        sources: vec![
            mock_source("claude-logs", Some(now), false),
            mock_source("claude-quota", Some(now), false),
        ],
    };

    let chatgpt = SubscriptionStatus {
        subscription: Subscription::ChatGpt,
        quota: vec![
            QuotaWindow {
                window: "primary".to_string(),
                used_pct: Some(3.0),
                resets_at: Some(now + chrono::Duration::hours(5)),
                source: DataSource::CodexRollout,
                observed_at: now,
                stale: false,
                expired: false,
            },
            QuotaWindow {
                window: "secondary".to_string(),
                used_pct: Some(8.0),
                resets_at: Some(now + chrono::Duration::days(5)),
                source: DataSource::CodexRollout,
                observed_at: now,
                stale: false,
                expired: false,
            },
        ],
        today: mock_totals(640_000, 155_000, 2.80, CostSource::Computed),
        last_7d: mock_totals(3_900_000, 980_000, 17.60, CostSource::Computed),
        by_tool: Vec::new(),
        last_quota_hit: None,
        sources: vec![
            mock_source("codex-rollouts", Some(now), false),
            mock_source(
                "pi-sessions",
                Some(now - chrono::Duration::minutes(12)),
                false,
            ),
        ],
    };

    let opencode_go = SubscriptionStatus {
        subscription: Subscription::OpencodeGo,
        quota: Vec::new(),
        today: mock_totals(410_000, 96_000, 3.20, CostSource::Reported),
        last_7d: mock_totals(2_100_000, 520_000, 13.40, CostSource::Reported),
        by_tool: Vec::new(),
        last_quota_hit: Some(QuotaHitStatus {
            period: Some("5h".to_string()),
            reset_at: Some(now + chrono::Duration::hours(2)),
            observed_at: now - chrono::Duration::minutes(40),
        }),
        sources: vec![
            mock_source("pi-sessions", Some(now), false),
            mock_source("opencode-db", Some(stale_now), true),
        ],
    };

    vec![claude, chatgpt, opencode_go]
}

fn mock_totals(input: u64, output: u64, cost_usd: f64, cost_source: CostSource) -> TokenTotals {
    TokenTotals {
        input,
        output,
        cache_read: input / 4,
        cache_write: input / 20,
        reasoning: 0,
        cost_usd: Some(cost_usd),
        cost_source,
    }
}

fn mock_source(
    collector_id: &str,
    last_update: Option<chrono::DateTime<Utc>>,
    stale: bool,
) -> SourceStatus {
    SourceStatus {
        collector_id: collector_id.to_string(),
        last_update,
        stale,
    }
}

pub fn mock_budget() -> BudgetConfig {
    BudgetConfig {
        daily_limit_usd: Some(5.00),
        weekly_limit_usd: None,
        monthly_limit_usd: Some(150.00),
        alert_threshold_pct: 0.80,
    }
}

pub fn mock_daily_aggregates() -> Vec<DailyAggregate> {
    let today = Utc::now().date_naive();
    let mut aggs = Vec::new();

    let data = [
        (-6, DEMO_SONNET, 45_000, 18_000, 12_000, 3_000),
        (-6, DEMO_OPUS, 12_000, 5_000, 4_000, 1_000),
        (-5, DEMO_SONNET, 82_000, 35_000, 25_000, 6_000),
        (-5, DEMO_HAIKU, 30_000, 10_000, 8_000, 2_000),
        (-4, DEMO_OPUS, 95_000, 42_000, 30_000, 8_000),
        (-4, DEMO_SONNET, 110_000, 48_000, 35_000, 9_000),
        (-3, DEMO_SONNET, 75_000, 32_000, 22_000, 5_000),
        (-3, DEMO_OPUS, 68_000, 28_000, 20_000, 5_000),
        (-2, DEMO_SONNET, 130_000, 55_000, 40_000, 10_000),
        (-2, DEMO_HAIKU, 45_000, 18_000, 12_000, 3_000),
        (-1, DEMO_SONNET, 98_000, 40_000, 30_000, 7_000),
        (-1, DEMO_OPUS, 55_000, 22_000, 16_000, 4_000),
        (0, DEMO_SONNET, 65_000, 28_000, 20_000, 5_000),
        (0, DEMO_OPUS, 35_000, 15_000, 10_000, 3_000),
        (0, DEMO_HAIKU, 20_000, 8_000, 6_000, 1_500),
    ];

    let price_book = PriceBook::embedded().unwrap_or_default();

    for (offset, model, input, output, cache_r, cache_w) in data {
        let date = today + chrono::Duration::days(offset);
        let cost = price_book
            .cost(model, input, output, cache_r, cache_w)
            .unwrap_or(0.0);
        aggs.push(DailyAggregate {
            date,
            model: model.to_string(),
            total_input_tokens: input,
            total_output_tokens: output,
            total_cache_read_tokens: cache_r,
            total_cache_write_tokens: cache_w,
            total_cost_usd: cost,
            request_count: (input / 5000) as u64,
            session_count: (input / 20000) as u64,
        });
    }

    aggs
}

pub fn mock_usage_summary() -> UsageSummaryResponse {
    let aggs = mock_daily_aggregates();
    let total_cost: f64 = aggs.iter().map(|a| a.total_cost_usd).sum();
    let total_input: u64 = aggs.iter().map(|a| a.total_input_tokens).sum();
    let total_output: u64 = aggs.iter().map(|a| a.total_output_tokens).sum();
    let total_requests: u64 = aggs.iter().map(|a| a.request_count).sum();

    UsageSummaryResponse {
        aggregates: aggs,
        total_cost_usd: total_cost,
        total_input_tokens: total_input,
        total_output_tokens: total_output,
        total_requests,
    }
}

pub fn mock_sessions() -> Vec<ActiveSession> {
    let now = Utc::now();
    vec![
        ActiveSession {
            session_id: "claude-tui#a3f2".to_string(),
            model: ModelType::Sonnet,
            started_at: now - chrono::Duration::seconds(154),
            last_activity: now - chrono::Duration::seconds(2),
            total_input_tokens: 8_200,
            total_output_tokens: 4_250,
            total_cache_read_tokens: 1_200,
            total_cache_write_tokens: 300,
            cost_usd: 0.032,
            request_count: 5,
            status: SessionStatus::Streaming,
            project: Some("claude-tui".to_string()),
        },
        ActiveSession {
            session_id: "ml-pipeline#8b1c".to_string(),
            model: ModelType::Opus,
            started_at: now - chrono::Duration::seconds(72),
            last_activity: now - chrono::Duration::seconds(5),
            total_input_tokens: 5_800,
            total_output_tokens: 2_400,
            total_cache_read_tokens: 800,
            total_cache_write_tokens: 200,
            cost_usd: 0.28,
            request_count: 3,
            status: SessionStatus::Streaming,
            project: Some("ml-pipeline".to_string()),
        },
        ActiveSession {
            session_id: "code-review#e4d9".to_string(),
            model: ModelType::Haiku,
            started_at: now - chrono::Duration::seconds(348),
            last_activity: now - chrono::Duration::seconds(120),
            total_input_tokens: 3_100,
            total_output_tokens: 1_200,
            total_cache_read_tokens: 500,
            total_cache_write_tokens: 100,
            cost_usd: 0.008,
            request_count: 2,
            status: SessionStatus::Idle,
            project: Some("code-review".to_string()),
        },
    ]
}

pub fn mock_model_stats() -> ModelsCompareResponse {
    ModelsCompareResponse {
        models: vec![
            ModelStats {
                model: DEMO_OPUS.to_string(),
                total_input_tokens: 1_245_000,
                total_output_tokens: 310_000,
                total_cost_usd: 67.20,
                request_count: 44,
                avg_input_per_request: 28_295.0,
                avg_output_per_request: 7_045.0,
                avg_cost_per_request: 1.527,
            },
            ModelStats {
                model: DEMO_SONNET.to_string(),
                total_input_tokens: 2_890_000,
                total_output_tokens: 640_000,
                total_cost_usd: 41.50,
                request_count: 239,
                avg_input_per_request: 12_092.0,
                avg_output_per_request: 2_678.0,
                avg_cost_per_request: 0.174,
            },
            ModelStats {
                model: DEMO_HAIKU.to_string(),
                total_input_tokens: 4_120_000,
                total_output_tokens: 980_000,
                total_cost_usd: 20.30,
                request_count: 503,
                avg_input_per_request: 8_191.0,
                avg_output_per_request: 1_948.0,
                avg_cost_per_request: 0.040,
            },
        ],
    }
}

pub struct MockProjectCost {
    pub project: String,
    pub model: String,
    pub cost: f64,
    pub pct: f64,
}

pub fn mock_project_costs() -> Vec<MockProjectCost> {
    vec![
        MockProjectCost {
            project: "ml-pipeline".to_string(),
            model: DEMO_OPUS.to_string(),
            cost: 45.20,
            pct: 0.35,
        },
        MockProjectCost {
            project: "claude-tui".to_string(),
            model: DEMO_SONNET.to_string(),
            cost: 18.40,
            pct: 0.14,
        },
        MockProjectCost {
            project: "docs-generator".to_string(),
            model: DEMO_SONNET.to_string(),
            cost: 12.80,
            pct: 0.10,
        },
        MockProjectCost {
            project: "code-review".to_string(),
            model: DEMO_HAIKU.to_string(),
            cost: 8.50,
            pct: 0.07,
        },
        MockProjectCost {
            project: "(other)".to_string(),
            model: DEMO_SONNET.to_string(),
            cost: 44.10,
            pct: 0.34,
        },
    ]
}
