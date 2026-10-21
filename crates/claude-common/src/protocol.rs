use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::models::{
    ActiveSession, BudgetConfig, CostSource, DailyAggregate, DataSource, ModelType, Provider,
    SessionStatus, Subscription, TimeRange, TimeWindow, UsageRecord,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcRequest {
    pub jsonrpc: String,
    pub id: u64,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcResponse {
    pub jsonrpc: String,
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl RpcResponse {
    pub fn success(id: u64, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: u64, error: RpcError) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(error),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

// --- Error codes ---
pub const PARSE_ERROR: i32 = -32700;
pub const INVALID_REQUEST: i32 = -32600;
pub const METHOD_NOT_FOUND: i32 = -32601;
pub const INVALID_PARAMS: i32 = -32602;
pub const INTERNAL_ERROR: i32 = -32603;
pub const COLLECTOR_UNAVAILABLE: i32 = -1;
pub const STORAGE_ERROR: i32 = -2;
pub const NOT_FOUND: i32 = -3;

// --- Status ---

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CollectorStatus {
    Log,
    Offline,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StatusResponse {
    pub daemon_uptime_secs: u64,
    pub active_sessions: u32,
    pub current_model: Option<ModelType>,
    pub cost_today_usd: f64,
    pub budget_pct: Option<f64>,
    pub collector_status: CollectorStatus,
    /// Per-subscription usage, quota and freshness (AC-9). Empty when the
    /// storage query fails, so an old client still gets a usable answer.
    #[serde(default)]
    pub subscriptions: Vec<SubscriptionStatus>,
}

/// Token and cost totals for one subscription over one window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenTotals {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// Reasoning tokens. The collectors fold reasoning into `output`, so this
    /// is 0 until a storage column separates them.
    pub reasoning: u64,
    /// USD over the records whose cost is known. `None` when every record in
    /// the window has an unknown cost.
    pub cost_usd: Option<f64>,
    /// The rules are evaluated in the order SPEC AC-9 states them: `Computed`
    /// if any part was computed, then `Unknown` if any part is unknown, else
    /// `Reported`.
    pub cost_source: CostSource,
}

impl TokenTotals {
    /// Totals for a window with no records: no tokens, no cost, nothing
    /// computed and nothing unknown.
    pub fn empty() -> Self {
        Self {
            input: 0,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
            cost_usd: Some(0.0),
            cost_source: CostSource::Reported,
        }
    }
}

/// One quota window as the daemon last observed it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuotaWindow {
    /// Window name as the provider reports it, e.g. `five_hour` or `primary`.
    pub window: String,
    pub used_pct: Option<f64>,
    pub resets_at: Option<DateTime<Utc>>,
    pub source: DataSource,
    pub observed_at: DateTime<Utc>,
    /// No observation of this window in the last 24 hours.
    pub stale: bool,
    /// The window's reset time is in the past.
    pub expired: bool,
}

/// Usage from one provider/source pair, so the two tools sharing a plan stay
/// distinguishable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolBreakdown {
    pub provider: Provider,
    pub source: DataSource,
    pub totals: TokenTotals,
}

/// The latest provider quota-limit event for a subscription.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuotaHitStatus {
    pub period: Option<String>,
    pub reset_at: Option<DateTime<Utc>>,
    pub observed_at: DateTime<Utc>,
}

/// Freshness of one collector feeding a subscription.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceStatus {
    pub collector_id: String,
    /// The newest record or snapshot this collector stored. `None` when it has
    /// stored nothing yet.
    pub last_update: Option<DateTime<Utc>>,
    pub stale: bool,
}

/// Everything the dashboard shows for one subscription.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscriptionStatus {
    pub subscription: Subscription,
    /// Newest snapshot per window, in window-name order.
    pub quota: Vec<QuotaWindow>,
    /// The local calendar day.
    pub today: TokenTotals,
    /// The local calendar days from six days before today through today.
    pub last_7d: TokenTotals,
    /// Provider/source breakdown over the same seven days.
    pub by_tool: Vec<ToolBreakdown>,
    pub last_quota_hit: Option<QuotaHitStatus>,
    pub sources: Vec<SourceStatus>,
}

// --- usage.query ---

fn default_limit() -> u32 {
    100
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UsageQueryParams {
    #[serde(default)]
    pub time_range: Option<TimeRange>,
    #[serde(default)]
    pub model: Option<ModelType>,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: u32,
    #[serde(default)]
    pub offset: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UsageQueryResponse {
    pub records: Vec<UsageRecord>,
    pub total_count: u64,
}

// --- usage.summary ---

#[derive(Debug, Serialize, Deserialize)]
pub struct UsageSummaryParams {
    pub window: TimeWindow,
    #[serde(default)]
    pub time_range: Option<TimeRange>,
    #[serde(default)]
    pub model: Option<ModelType>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UsageSummaryResponse {
    pub aggregates: Vec<DailyAggregate>,
    pub total_cost_usd: f64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_requests: u64,
}

// --- sessions.list ---

#[derive(Debug, Serialize, Deserialize)]
pub struct SessionsListParams {
    #[serde(default)]
    pub status: Option<SessionStatus>,
    #[serde(default = "default_limit")]
    pub limit: u32,
    #[serde(default)]
    pub offset: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SessionsListResponse {
    pub sessions: Vec<ActiveSession>,
    pub total_count: u64,
}

// --- sessions.get ---

#[derive(Debug, Serialize, Deserialize)]
pub struct SessionsGetParams {
    pub session_id: String,
}

// --- budget.set response ---

#[derive(Debug, Serialize, Deserialize)]
pub struct BudgetSetResponse {
    pub success: bool,
}

// --- models.compare ---

#[derive(Debug, Serialize, Deserialize)]
pub struct ModelsCompareParams {
    #[serde(default)]
    pub time_range: Option<TimeRange>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ModelsCompareResponse {
    pub models: Vec<ModelStats>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ModelStats {
    /// Model id as the source reports it, e.g. `claude-opus-5-5`.
    pub model: String,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cost_usd: f64,
    pub request_count: u64,
    pub avg_input_per_request: f64,
    pub avg_output_per_request: f64,
    pub avg_cost_per_request: f64,
}

// --- Typed dispatch ---

#[derive(Debug)]
pub enum RpcMethod {
    Status,
    UsageQuery(UsageQueryParams),
    UsageSummary(UsageSummaryParams),
    SessionsList(SessionsListParams),
    SessionsGet(SessionsGetParams),
    BudgetGet,
    BudgetSet(BudgetConfig),
    ModelsCompare(ModelsCompareParams),
}

impl RpcMethod {
    pub fn from_request(req: &RpcRequest) -> Result<Self, RpcError> {
        match req.method.as_str() {
            "status" => Ok(RpcMethod::Status),
            "usage.query" => {
                let params = serde_json::from_value(req.params.clone()).map_err(|e| RpcError {
                    code: INVALID_PARAMS,
                    message: e.to_string(),
                    data: None,
                })?;
                Ok(RpcMethod::UsageQuery(params))
            }
            "usage.summary" => {
                let params = serde_json::from_value(req.params.clone()).map_err(|e| RpcError {
                    code: INVALID_PARAMS,
                    message: e.to_string(),
                    data: None,
                })?;
                Ok(RpcMethod::UsageSummary(params))
            }
            "sessions.list" => {
                let params = serde_json::from_value(req.params.clone()).map_err(|e| RpcError {
                    code: INVALID_PARAMS,
                    message: e.to_string(),
                    data: None,
                })?;
                Ok(RpcMethod::SessionsList(params))
            }
            "sessions.get" => {
                let params = serde_json::from_value(req.params.clone()).map_err(|e| RpcError {
                    code: INVALID_PARAMS,
                    message: e.to_string(),
                    data: None,
                })?;
                Ok(RpcMethod::SessionsGet(params))
            }
            "budget.get" => Ok(RpcMethod::BudgetGet),
            "budget.set" => {
                let params = serde_json::from_value(req.params.clone()).map_err(|e| RpcError {
                    code: INVALID_PARAMS,
                    message: e.to_string(),
                    data: None,
                })?;
                Ok(RpcMethod::BudgetSet(params))
            }
            "models.compare" => {
                let params = serde_json::from_value(req.params.clone()).map_err(|e| RpcError {
                    code: INVALID_PARAMS,
                    message: e.to_string(),
                    data: None,
                })?;
                Ok(RpcMethod::ModelsCompare(params))
            }
            _ => Err(RpcError {
                code: METHOD_NOT_FOUND,
                message: format!("unknown method: {}", req.method),
                data: None,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rpc_request_roundtrip() {
        let req = RpcRequest {
            jsonrpc: "2.0".to_string(),
            id: 1,
            method: "status".to_string(),
            params: serde_json::Value::Null,
        };
        let json = serde_json::to_string(&req).unwrap();
        let parsed: RpcRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, 1);
        assert_eq!(parsed.method, "status");
    }

    #[test]
    fn test_rpc_response_success_roundtrip() {
        let resp = RpcResponse::success(1, serde_json::json!({"ok": true}));
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: RpcResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, 1);
        assert!(parsed.result.is_some());
        assert!(parsed.error.is_none());
    }

    #[test]
    fn test_rpc_response_error_roundtrip() {
        let resp = RpcResponse::error(
            2,
            RpcError {
                code: METHOD_NOT_FOUND,
                message: "not found".to_string(),
                data: None,
            },
        );
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: RpcResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, 2);
        assert!(parsed.result.is_none());
        assert_eq!(parsed.error.as_ref().unwrap().code, METHOD_NOT_FOUND);
    }

    #[test]
    fn test_status_response_roundtrip() {
        let resp = StatusResponse {
            daemon_uptime_secs: 3600,
            active_sessions: 2,
            current_model: Some(ModelType::Sonnet),
            cost_today_usd: 3.47,
            budget_pct: Some(0.67),
            collector_status: CollectorStatus::Log,
            subscriptions: vec![],
        };
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: StatusResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.active_sessions, 2);
        assert_eq!(parsed.current_model, Some(ModelType::Sonnet));
    }

    #[test]
    fn test_status_response_parses_without_subscriptions() {
        // An older daemon sends the fields it had then; the new client must
        // still parse it and see an empty subscription list.
        let json = r#"{"daemon_uptime_secs":10,"active_sessions":0,
            "current_model":null,"cost_today_usd":0.0,"budget_pct":null,
            "collector_status":"log"}"#;
        let parsed: StatusResponse = serde_json::from_str(json).unwrap();
        assert!(parsed.subscriptions.is_empty());
        assert!(matches!(parsed.collector_status, CollectorStatus::Log));
    }

    #[test]
    fn test_subscription_status_roundtrip() {
        let now = chrono::Utc::now();
        let status = SubscriptionStatus {
            subscription: Subscription::Claude,
            quota: vec![QuotaWindow {
                window: "five_hour".to_string(),
                used_pct: Some(34.0),
                resets_at: Some(now),
                source: DataSource::ClaudeQuota,
                observed_at: now,
                stale: false,
                expired: false,
            }],
            today: TokenTotals {
                input: 10,
                output: 5,
                cache_read: 2,
                cache_write: 1,
                reasoning: 0,
                cost_usd: Some(0.5),
                cost_source: CostSource::Computed,
            },
            last_7d: TokenTotals::empty(),
            by_tool: vec![ToolBreakdown {
                provider: Provider::Anthropic,
                source: DataSource::Log,
                totals: TokenTotals::empty(),
            }],
            last_quota_hit: Some(QuotaHitStatus {
                period: Some("5h".to_string()),
                reset_at: Some(now),
                observed_at: now,
            }),
            sources: vec![SourceStatus {
                collector_id: "claude-logs".to_string(),
                last_update: Some(now),
                stale: false,
            }],
        };
        let json = serde_json::to_string(&status).unwrap();
        let parsed: SubscriptionStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.subscription, Subscription::Claude);
        assert_eq!(parsed.quota[0].window, "five_hour");
        assert_eq!(parsed.today.input, 10);
        assert_eq!(parsed.today.cost_source, CostSource::Computed);
        assert_eq!(parsed.by_tool[0].provider, Provider::Anthropic);
        assert_eq!(
            parsed
                .last_quota_hit
                .as_ref()
                .and_then(|h| h.period.as_deref()),
            Some("5h")
        );
        assert_eq!(parsed.sources[0].collector_id, "claude-logs");
    }

    #[test]
    fn test_usage_query_params_defaults() {
        let params: UsageQueryParams = serde_json::from_str("{}").unwrap();
        assert_eq!(params.limit, 100);
        assert_eq!(params.offset, 0);
        assert!(params.time_range.is_none());
        assert!(params.model.is_none());
        assert!(params.project.is_none());
    }

    #[test]
    fn test_sessions_list_params_defaults() {
        let params: SessionsListParams = serde_json::from_str("{}").unwrap();
        assert_eq!(params.limit, 100);
        assert_eq!(params.offset, 0);
        assert!(params.status.is_none());
    }

    #[test]
    fn test_rpc_method_from_request_status() {
        let req = RpcRequest {
            jsonrpc: "2.0".to_string(),
            id: 1,
            method: "status".to_string(),
            params: serde_json::Value::Null,
        };
        let method = RpcMethod::from_request(&req).unwrap();
        assert!(matches!(method, RpcMethod::Status));
    }

    #[test]
    fn test_rpc_method_from_request_unknown() {
        let req = RpcRequest {
            jsonrpc: "2.0".to_string(),
            id: 1,
            method: "foo.bar".to_string(),
            params: serde_json::Value::Null,
        };
        let err = RpcMethod::from_request(&req).unwrap_err();
        assert_eq!(err.code, METHOD_NOT_FOUND);
    }

    #[test]
    fn test_rpc_method_from_request_usage_query() {
        let req = RpcRequest {
            jsonrpc: "2.0".to_string(),
            id: 2,
            method: "usage.query".to_string(),
            params: serde_json::json!({"limit": 50}),
        };
        let method = RpcMethod::from_request(&req).unwrap();
        match method {
            RpcMethod::UsageQuery(params) => assert_eq!(params.limit, 50),
            _ => panic!("expected UsageQuery"),
        }
    }

    #[test]
    fn test_rpc_method_from_request_budget_set() {
        let req = RpcRequest {
            jsonrpc: "2.0".to_string(),
            id: 3,
            method: "budget.set".to_string(),
            params: serde_json::json!({
                "daily_limit_usd": 5.0,
                "alert_threshold_pct": 0.9
            }),
        };
        let method = RpcMethod::from_request(&req).unwrap();
        match method {
            RpcMethod::BudgetSet(config) => {
                assert_eq!(config.daily_limit_usd, Some(5.0));
                assert!((config.alert_threshold_pct - 0.9).abs() < 1e-10);
            }
            _ => panic!("expected BudgetSet"),
        }
    }

    #[test]
    fn test_collector_status_roundtrip() {
        let status = CollectorStatus::Log;
        let json = serde_json::to_string(&status).unwrap();
        assert_eq!(json, r#""log""#);
        let parsed: CollectorStatus = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, CollectorStatus::Log));
    }

    #[test]
    fn test_models_compare_response_roundtrip() {
        let resp = ModelsCompareResponse {
            models: vec![ModelStats {
                model: "claude-opus-5-5".to_string(),
                total_input_tokens: 1_000_000,
                total_output_tokens: 500_000,
                total_cost_usd: 52.5,
                request_count: 100,
                avg_input_per_request: 10_000.0,
                avg_output_per_request: 5_000.0,
                avg_cost_per_request: 0.525,
            }],
        };
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: ModelsCompareResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.models.len(), 1);
        assert_eq!(parsed.models[0].model, "claude-opus-5-5");
    }

    #[test]
    fn test_budget_set_response_roundtrip() {
        let resp = BudgetSetResponse { success: true };
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: BudgetSetResponse = serde_json::from_str(&json).unwrap();
        assert!(parsed.success);
    }

    #[test]
    fn test_usage_summary_params_roundtrip() {
        let params = UsageSummaryParams {
            window: TimeWindow::Week,
            time_range: None,
            model: Some(ModelType::Sonnet),
        };
        let json = serde_json::to_string(&params).unwrap();
        let parsed: UsageSummaryParams = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed.window, TimeWindow::Week));
        assert_eq!(parsed.model, Some(ModelType::Sonnet));
    }
}
