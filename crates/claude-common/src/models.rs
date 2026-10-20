use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelType {
    Opus,
    Sonnet,
    Haiku,
}

impl ModelType {
    /// Price per million input tokens in USD.
    pub fn input_price_per_m(&self) -> f64 {
        match self {
            ModelType::Opus => 15.0,
            ModelType::Sonnet => 3.0,
            ModelType::Haiku => 0.80,
        }
    }

    /// Price per million output tokens in USD.
    pub fn output_price_per_m(&self) -> f64 {
        match self {
            ModelType::Opus => 75.0,
            ModelType::Sonnet => 15.0,
            ModelType::Haiku => 4.0,
        }
    }

    /// Price per million cache read tokens in USD.
    pub fn cache_read_price_per_m(&self) -> f64 {
        match self {
            ModelType::Opus => 1.50,
            ModelType::Sonnet => 0.30,
            ModelType::Haiku => 0.08,
        }
    }

    /// Price per million cache write tokens in USD.
    pub fn cache_write_price_per_m(&self) -> f64 {
        match self {
            ModelType::Opus => 18.75,
            ModelType::Sonnet => 3.75,
            ModelType::Haiku => 1.0,
        }
    }

    /// Compute cost for a given token breakdown.
    pub fn compute_cost(
        &self,
        input_tokens: u64,
        output_tokens: u64,
        cache_read_tokens: u64,
        cache_write_tokens: u64,
    ) -> f64 {
        let input = input_tokens as f64 * self.input_price_per_m() / 1_000_000.0;
        let output = output_tokens as f64 * self.output_price_per_m() / 1_000_000.0;
        let cache_read = cache_read_tokens as f64 * self.cache_read_price_per_m() / 1_000_000.0;
        let cache_write = cache_write_tokens as f64 * self.cache_write_price_per_m() / 1_000_000.0;
        input + output + cache_read + cache_write
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            ModelType::Opus => "Opus",
            ModelType::Sonnet => "Sonnet",
            ModelType::Haiku => "Haiku",
        }
    }
}

impl fmt::Display for ModelType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Parse from full model ID strings like "claude-opus-4-6" or "claude-sonnet-4-6".
impl std::str::FromStr for ModelType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let lower = s.to_lowercase();
        if lower.contains("opus") {
            Ok(ModelType::Opus)
        } else if lower.contains("sonnet") {
            Ok(ModelType::Sonnet)
        } else if lower.contains("haiku") {
            Ok(ModelType::Haiku)
        } else {
            Err(format!("unknown model: {s}"))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionStatus {
    Streaming,
    Idle,
    Completed,
}

impl fmt::Display for SessionStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionStatus::Streaming => f.write_str("Streaming"),
            SessionStatus::Idle => f.write_str("Idle"),
            SessionStatus::Completed => f.write_str("Completed"),
        }
    }
}

/// Where a usage record was collected from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DataSource {
    /// Claude Code conversation logs (`~/.claude/projects/**/*.jsonl`).
    Log,
    /// Claude Code quota snapshot (statusline stdin JSON or `~/.claude.json`).
    ClaudeQuota,
    /// Codex CLI rollout files (`~/.codex/sessions/**/rollout-*.jsonl`).
    CodexRollout,
    /// Pi session files (`~/.pi/agent/sessions/**/*.jsonl`).
    PiSession,
    /// OpenCode's SQLite database.
    OpencodeDb,
}

/// The upstream model provider that billed a record's tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Provider {
    Anthropic,
    OpenAiCodex,
    OpencodeGo,
    /// Any provider this dashboard does not know about.
    Other,
}

impl Provider {
    pub fn as_str(&self) -> &'static str {
        match self {
            Provider::Anthropic => "anthropic",
            Provider::OpenAiCodex => "openai-codex",
            Provider::OpencodeGo => "opencode-go",
            Provider::Other => "other",
        }
    }
}

impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The subscription a record's usage counts against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Subscription {
    Claude,
    #[serde(rename = "chatgpt")]
    ChatGpt,
    OpencodeGo,
    Other,
}

impl Subscription {
    pub fn as_str(&self) -> &'static str {
        match self {
            Subscription::Claude => "claude",
            Subscription::ChatGpt => "chatgpt",
            Subscription::OpencodeGo => "opencode-go",
            Subscription::Other => "other",
        }
    }
}

impl fmt::Display for Subscription {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Group a collected record by the subscription it draws on.
///
/// Several tools share one plan: the Codex CLI and Pi's `openai-codex` provider
/// both draw on the ChatGPT plan, and Pi's `opencode-go` provider and OpenCode
/// both draw on the opencode-go plan. This is a pure function so the grouping
/// stays identical wherever it is applied.
pub fn subscription_for(source: DataSource, provider: Provider) -> Subscription {
    match source {
        DataSource::Log | DataSource::ClaudeQuota => Subscription::Claude,
        DataSource::CodexRollout => Subscription::ChatGpt,
        DataSource::PiSession => match provider {
            Provider::OpenAiCodex => Subscription::ChatGpt,
            Provider::OpencodeGo => Subscription::OpencodeGo,
            Provider::Anthropic | Provider::Other => Subscription::Other,
        },
        DataSource::OpencodeDb => Subscription::OpencodeGo,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageRecord {
    /// Auto-incremented database ID. None before insertion.
    pub id: Option<i64>,
    /// Unique identifier for deduplication.
    pub uuid: Uuid,
    /// When this usage event occurred.
    pub timestamp: DateTime<Utc>,
    /// Which model was used.
    pub model: ModelType,
    /// Number of input tokens (non-cached).
    pub input_tokens: u64,
    /// Number of output tokens.
    pub output_tokens: u64,
    /// Number of cache read tokens.
    pub cache_read_tokens: u64,
    /// Number of cache write tokens.
    pub cache_write_tokens: u64,
    /// Computed cost in USD.
    pub cost_usd: f64,
    /// Session this request belongs to, if known.
    pub session_id: Option<String>,
    /// Project/directory context, if known.
    pub project: Option<String>,
    /// Data source: "api" or "log".
    pub source: DataSource,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveSession {
    pub session_id: String,
    pub model: ModelType,
    pub started_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cache_read_tokens: u64,
    pub total_cache_write_tokens: u64,
    pub cost_usd: f64,
    pub request_count: u32,
    pub status: SessionStatus,
    pub project: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetConfig {
    /// Maximum daily spend in USD. None means no limit.
    pub daily_limit_usd: Option<f64>,
    /// Maximum weekly spend in USD. None means no limit.
    pub weekly_limit_usd: Option<f64>,
    /// Maximum monthly spend in USD. None means no limit.
    pub monthly_limit_usd: Option<f64>,
    /// Percentage (0.0 - 1.0) at which to trigger an alert.
    pub alert_threshold_pct: f64,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            daily_limit_usd: None,
            weekly_limit_usd: None,
            monthly_limit_usd: None,
            alert_threshold_pct: 0.80,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyAggregate {
    pub date: NaiveDate,
    pub model: ModelType,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cache_read_tokens: u64,
    pub total_cache_write_tokens: u64,
    pub total_cost_usd: f64,
    pub request_count: u64,
    pub session_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeRange {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TimeWindow {
    Day,
    Week,
    Month,
    Quarter,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_cost_opus() {
        let cost = ModelType::Opus.compute_cost(1_000_000, 500_000, 200_000, 100_000);
        // input: 1M * 15/1M = 15.0
        // output: 500K * 75/1M = 37.5
        // cache_read: 200K * 1.5/1M = 0.3
        // cache_write: 100K * 18.75/1M = 1.875
        let expected = 15.0 + 37.5 + 0.3 + 1.875;
        assert!(
            (cost - expected).abs() < 1e-10,
            "got {cost}, expected {expected}"
        );
    }

    #[test]
    fn test_compute_cost_sonnet() {
        let cost = ModelType::Sonnet.compute_cost(1_000_000, 1_000_000, 0, 0);
        // input: 1M * 3/1M = 3.0
        // output: 1M * 15/1M = 15.0
        let expected = 18.0;
        assert!(
            (cost - expected).abs() < 1e-10,
            "got {cost}, expected {expected}"
        );
    }

    #[test]
    fn test_compute_cost_haiku() {
        let cost = ModelType::Haiku.compute_cost(500_000, 250_000, 100_000, 50_000);
        // input: 500K * 0.8/1M = 0.4
        // output: 250K * 4.0/1M = 1.0
        // cache_read: 100K * 0.08/1M = 0.008
        // cache_write: 50K * 1.0/1M = 0.05
        let expected = 0.4 + 1.0 + 0.008 + 0.05;
        assert!(
            (cost - expected).abs() < 1e-10,
            "got {cost}, expected {expected}"
        );
    }

    #[test]
    fn test_compute_cost_zero() {
        let cost = ModelType::Sonnet.compute_cost(0, 0, 0, 0);
        assert!((cost - 0.0).abs() < 1e-10);
    }

    #[test]
    fn test_from_str_full_model_ids() {
        assert_eq!(
            "claude-opus-4-6".parse::<ModelType>().unwrap(),
            ModelType::Opus
        );
        assert_eq!(
            "claude-sonnet-4-6".parse::<ModelType>().unwrap(),
            ModelType::Sonnet
        );
        assert_eq!(
            "claude-haiku-4-5-20251001".parse::<ModelType>().unwrap(),
            ModelType::Haiku
        );
    }

    #[test]
    fn test_from_str_short_names() {
        assert_eq!("Opus".parse::<ModelType>().unwrap(), ModelType::Opus);
        assert_eq!("sonnet".parse::<ModelType>().unwrap(), ModelType::Sonnet);
        assert_eq!("HAIKU".parse::<ModelType>().unwrap(), ModelType::Haiku);
    }

    #[test]
    fn test_from_str_unknown() {
        assert!("gpt-4".parse::<ModelType>().is_err());
        assert!("unknown".parse::<ModelType>().is_err());
    }

    #[test]
    fn test_display() {
        assert_eq!(ModelType::Opus.to_string(), "Opus");
        assert_eq!(ModelType::Sonnet.to_string(), "Sonnet");
        assert_eq!(ModelType::Haiku.to_string(), "Haiku");
    }

    #[test]
    fn test_session_status_display() {
        assert_eq!(SessionStatus::Streaming.to_string(), "Streaming");
        assert_eq!(SessionStatus::Idle.to_string(), "Idle");
        assert_eq!(SessionStatus::Completed.to_string(), "Completed");
    }

    #[test]
    fn test_budget_config_default() {
        let config = BudgetConfig::default();
        assert_eq!(config.daily_limit_usd, None);
        assert_eq!(config.weekly_limit_usd, None);
        assert_eq!(config.monthly_limit_usd, None);
        assert!((config.alert_threshold_pct - 0.80).abs() < 1e-10);
    }

    #[test]
    fn test_model_type_serde_roundtrip() {
        let model = ModelType::Sonnet;
        let json = serde_json::to_string(&model).unwrap();
        assert_eq!(json, r#""sonnet""#);
        let parsed: ModelType = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, model);
    }

    #[test]
    fn test_data_source_serde_roundtrip() {
        let source = DataSource::Log;
        let json = serde_json::to_string(&source).unwrap();
        assert_eq!(json, r#""log""#);
        let parsed: DataSource = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, source);
    }

    #[test]
    fn subscription_grouping_claude_logs() {
        assert_eq!(
            subscription_for(DataSource::Log, Provider::Anthropic),
            Subscription::Claude
        );
    }

    #[test]
    fn subscription_grouping_claude_quota() {
        assert_eq!(
            subscription_for(DataSource::ClaudeQuota, Provider::Anthropic),
            Subscription::Claude
        );
    }

    #[test]
    fn subscription_grouping_codex_rollout() {
        assert_eq!(
            subscription_for(DataSource::CodexRollout, Provider::OpenAiCodex),
            Subscription::ChatGpt
        );
    }

    #[test]
    fn subscription_grouping_pi_openai_codex() {
        assert_eq!(
            subscription_for(DataSource::PiSession, Provider::OpenAiCodex),
            Subscription::ChatGpt
        );
    }

    #[test]
    fn subscription_grouping_pi_opencode_go() {
        assert_eq!(
            subscription_for(DataSource::PiSession, Provider::OpencodeGo),
            Subscription::OpencodeGo
        );
    }

    #[test]
    fn subscription_grouping_opencode_db() {
        assert_eq!(
            subscription_for(DataSource::OpencodeDb, Provider::Other),
            Subscription::OpencodeGo
        );
    }

    #[test]
    fn subscription_grouping_other_provider() {
        assert_eq!(
            subscription_for(DataSource::PiSession, Provider::Other),
            Subscription::Other
        );
        assert_eq!(
            subscription_for(DataSource::PiSession, Provider::Anthropic),
            Subscription::Other
        );
    }

    #[test]
    fn test_subscription_serde_names() {
        let json = serde_json::to_string(&Subscription::ChatGpt).unwrap();
        assert_eq!(json, r#""chatgpt""#);
        let json = serde_json::to_string(&Subscription::OpencodeGo).unwrap();
        assert_eq!(json, r#""opencode-go""#);
        assert_eq!(Provider::OpenAiCodex.to_string(), "openai-codex");
    }
}
