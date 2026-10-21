use std::path::{Path, PathBuf};
use std::sync::Arc;

use claude_common::protocol::{
    BudgetSetResponse, CollectorStatus, INTERNAL_ERROR, NOT_FOUND, PARSE_ERROR, RpcError,
    RpcMethod, RpcRequest, RpcResponse, StatusResponse,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::storage::Storage;

/// Maximum allowed line length for IPC requests (1 MB).
const MAX_REQUEST_LINE_LEN: usize = 1_048_576;

pub struct IpcServer {
    socket_path: PathBuf,
    storage: Arc<Mutex<Storage>>,
    start_time: std::time::Instant,
    collector_status: Arc<Mutex<CollectorStatus>>,
    cancel: CancellationToken,
}

impl IpcServer {
    pub fn new(
        socket_path: PathBuf,
        storage: Arc<Mutex<Storage>>,
        collector_status: Arc<Mutex<CollectorStatus>>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            socket_path,
            storage,
            start_time: std::time::Instant::now(),
            collector_status,
            cancel,
        }
    }

    pub async fn run(&self) -> Result<(), claude_common::IpcError> {
        // Clean up stale socket file
        if Path::new(&self.socket_path).exists() {
            std::fs::remove_file(&self.socket_path).ok();
        }

        // Ensure parent directory exists
        if let Some(parent) = self.socket_path.parent() {
            std::fs::create_dir_all(parent).ok();
        }

        let listener = UnixListener::bind(&self.socket_path).map_err(|e| {
            claude_common::IpcError::SocketBind {
                path: self.socket_path.display().to_string(),
                reason: e.to_string(),
            }
        })?;

        // Set socket file permissions to owner-only (0o600)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.socket_path, std::fs::Permissions::from_mode(0o600))
                .ok();
        }

        info!(path = %self.socket_path.display(), "IPC server listening");

        loop {
            tokio::select! {
                _ = self.cancel.cancelled() => {
                    info!("IPC server shutting down");
                    break;
                }
                result = listener.accept() => {
                    match result {
                        Ok((stream, _addr)) => {
                            let storage = Arc::clone(&self.storage);
                            let start_time = self.start_time;
                            let collector_status = Arc::clone(&self.collector_status);
                            let cancel = self.cancel.clone();
                            tokio::spawn(async move {
                                if let Err(e) =
                                    handle_client(stream, storage, start_time, collector_status, cancel).await
                                {
                                    warn!("client handler error: {e}");
                                }
                            });
                        }
                        Err(e) => {
                            error!("accept error: {e}");
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

async fn handle_client(
    stream: tokio::net::UnixStream,
    storage: Arc<Mutex<Storage>>,
    start_time: std::time::Instant,
    collector_status: Arc<Mutex<CollectorStatus>>,
    cancel: CancellationToken,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (reader, mut writer) = stream.into_split();
    let reader = BufReader::new(reader);
    let mut lines = reader.lines();

    loop {
        let line = tokio::select! {
            _ = cancel.cancelled() => break,
            result = lines.next_line() => {
                match result? {
                    Some(line) => line,
                    None => break,
                }
            }
        };

        // Reject oversized requests to prevent OOM
        if line.len() > MAX_REQUEST_LINE_LEN {
            warn!(
                "dropping connection: request line too large ({} bytes)",
                line.len()
            );
            break;
        }

        let response = match serde_json::from_str::<RpcRequest>(&line) {
            Ok(request) => handle_request(&storage, &request, start_time, &collector_status).await,
            Err(e) => RpcResponse::error(
                0,
                RpcError {
                    code: PARSE_ERROR,
                    message: e.to_string(),
                    data: None,
                },
            ),
        };

        let mut response_json = serde_json::to_string(&response)?;
        response_json.push('\n');
        writer.write_all(response_json.as_bytes()).await?;
        writer.flush().await?;
    }

    Ok(())
}

async fn handle_request(
    storage: &Arc<Mutex<Storage>>,
    request: &RpcRequest,
    start_time: std::time::Instant,
    collector_status: &Arc<Mutex<CollectorStatus>>,
) -> RpcResponse {
    let method = match RpcMethod::from_request(request) {
        Ok(m) => m,
        Err(e) => return RpcResponse::error(request.id, e),
    };

    match method {
        RpcMethod::Status => {
            // [U1 fix] Acquire collector_status FIRST to avoid ABBA deadlock with collector.
            let cs = *collector_status.lock().await;

            let storage = storage.lock().await;
            let uptime = start_time.elapsed().as_secs();
            let cost_today = storage.get_cost_today().unwrap_or(0.0);
            let budget = storage.get_budget().unwrap_or_default();
            let sessions = storage
                .list_sessions(&claude_common::protocol::SessionsListParams {
                    status: Some(claude_common::SessionStatus::Streaming),
                    limit: 100,
                    offset: 0,
                })
                .unwrap_or(claude_common::protocol::SessionsListResponse {
                    sessions: vec![],
                    total_count: 0,
                });

            let current_model = sessions.sessions.first().map(|s| s.model);
            let budget_pct = budget
                .daily_limit_usd
                .map(|limit| if limit > 0.0 { cost_today / limit } else { 0.0 });

            // Per-subscription usage, quota and freshness (AC-9). A failed
            // aggregate leaves the list empty rather than failing `status`, so
            // an older client keeps its fields and ignores this one.
            let subscriptions = match storage.subscription_statuses(chrono::Utc::now()) {
                Ok(subscriptions) => subscriptions,
                Err(e) => {
                    warn!("status: subscription aggregates failed: {e}");
                    Vec::new()
                }
            };

            let resp = StatusResponse {
                daemon_uptime_secs: uptime,
                active_sessions: sessions.total_count as u32,
                current_model,
                cost_today_usd: cost_today,
                budget_pct,
                collector_status: cs,
                subscriptions,
            };
            RpcResponse::success(request.id, serde_json::to_value(&resp).unwrap_or_default())
        }
        RpcMethod::UsageQuery(params) => {
            let storage = storage.lock().await;
            match storage.query_usage(&params) {
                Ok(result) => RpcResponse::success(
                    request.id,
                    serde_json::to_value(&result).unwrap_or_default(),
                ),
                Err(e) => RpcResponse::error(
                    request.id,
                    RpcError {
                        code: INTERNAL_ERROR,
                        message: e.to_string(),
                        data: None,
                    },
                ),
            }
        }
        RpcMethod::UsageSummary(params) => {
            let storage = storage.lock().await;
            match storage.get_summary(&params) {
                Ok(result) => RpcResponse::success(
                    request.id,
                    serde_json::to_value(&result).unwrap_or_default(),
                ),
                Err(e) => RpcResponse::error(
                    request.id,
                    RpcError {
                        code: INTERNAL_ERROR,
                        message: e.to_string(),
                        data: None,
                    },
                ),
            }
        }
        RpcMethod::SessionsList(params) => {
            let storage = storage.lock().await;
            match storage.list_sessions(&params) {
                Ok(result) => RpcResponse::success(
                    request.id,
                    serde_json::to_value(&result).unwrap_or_default(),
                ),
                Err(e) => RpcResponse::error(
                    request.id,
                    RpcError {
                        code: INTERNAL_ERROR,
                        message: e.to_string(),
                        data: None,
                    },
                ),
            }
        }
        RpcMethod::SessionsGet(params) => {
            let storage = storage.lock().await;
            match storage.get_session(&params.session_id) {
                Ok(Some(session)) => RpcResponse::success(
                    request.id,
                    serde_json::to_value(&session).unwrap_or_default(),
                ),
                Ok(None) => RpcResponse::error(
                    request.id,
                    RpcError {
                        code: NOT_FOUND,
                        message: format!("session not found: {}", params.session_id),
                        data: None,
                    },
                ),
                Err(e) => RpcResponse::error(
                    request.id,
                    RpcError {
                        code: INTERNAL_ERROR,
                        message: e.to_string(),
                        data: None,
                    },
                ),
            }
        }
        RpcMethod::BudgetGet => {
            let storage = storage.lock().await;
            match storage.get_budget() {
                Ok(config) => RpcResponse::success(
                    request.id,
                    serde_json::to_value(&config).unwrap_or_default(),
                ),
                Err(e) => RpcResponse::error(
                    request.id,
                    RpcError {
                        code: INTERNAL_ERROR,
                        message: e.to_string(),
                        data: None,
                    },
                ),
            }
        }
        RpcMethod::BudgetSet(config) => {
            let storage = storage.lock().await;
            match storage.set_budget(&config) {
                Ok(()) => RpcResponse::success(
                    request.id,
                    serde_json::to_value(&BudgetSetResponse { success: true }).unwrap_or_default(),
                ),
                Err(e) => RpcResponse::error(
                    request.id,
                    RpcError {
                        code: INTERNAL_ERROR,
                        message: e.to_string(),
                        data: None,
                    },
                ),
            }
        }
        RpcMethod::ModelsCompare(params) => {
            let storage = storage.lock().await;
            match storage.get_model_stats(&params) {
                Ok(result) => RpcResponse::success(
                    request.id,
                    serde_json::to_value(&result).unwrap_or_default(),
                ),
                Err(e) => RpcResponse::error(
                    request.id,
                    RpcError {
                        code: INTERNAL_ERROR,
                        message: e.to_string(),
                        data: None,
                    },
                ),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};
    use claude_common::protocol::RpcRequest;
    use claude_common::{
        CostSource, DataSource, Provider, QuotaHit, QuotaSnapshot, Subscription, UsageRecord,
    };

    fn make_request(method: &str, params: serde_json::Value) -> RpcRequest {
        RpcRequest {
            jsonrpc: "2.0".to_string(),
            id: 1,
            method: method.to_string(),
            params,
        }
    }

    /// One usage row, as a collector would have stored it.
    fn usage_record(
        subscription: Subscription,
        provider: Provider,
        source: DataSource,
        timestamp: chrono::DateTime<Utc>,
        input: u64,
        output: u64,
        cost: Option<(f64, CostSource)>,
    ) -> UsageRecord {
        let uuid = uuid::Uuid::new_v4();
        UsageRecord {
            id: None,
            uuid,
            timestamp,
            model: "test-model".to_string(),
            provider,
            subscription,
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cost_usd: cost.map(|(usd, _)| usd),
            cost_source: cost
                .map(|(_, source)| source)
                .unwrap_or(CostSource::Unknown),
            dedup_key: uuid.to_string(),
            session_id: None,
            project: None,
            source,
        }
    }

    #[tokio::test]
    async fn status_rpc_subscriptions_group_sums_and_flags() {
        let storage = Arc::new(Mutex::new(Storage::new(":memory:").unwrap()));
        let start_time = std::time::Instant::now();
        let collector_status = Arc::new(Mutex::new(CollectorStatus::Log));
        let now = Utc::now();

        {
            let mut storage = storage.lock().await;
            storage
                .insert_usage_batch(&[
                    // Claude Code, today.
                    usage_record(
                        Subscription::Claude,
                        Provider::Anthropic,
                        DataSource::Log,
                        now,
                        1_000,
                        500,
                        Some((1.5, CostSource::Computed)),
                    ),
                    // Codex CLI, today, and an old row that only the 7d
                    // window sees.
                    usage_record(
                        Subscription::ChatGpt,
                        Provider::OpenAiCodex,
                        DataSource::CodexRollout,
                        now,
                        2_000,
                        300,
                        Some((0.25, CostSource::Computed)),
                    ),
                    usage_record(
                        Subscription::ChatGpt,
                        Provider::OpenAiCodex,
                        DataSource::CodexRollout,
                        now - Duration::days(3),
                        400,
                        100,
                        Some((0.05, CostSource::Reported)),
                    ),
                    // opencode-go through Pi today, through OpenCode three
                    // days ago: the OpenCode source is stale.
                    usage_record(
                        Subscription::OpencodeGo,
                        Provider::OpencodeGo,
                        DataSource::PiSession,
                        now,
                        100,
                        50,
                        Some((0.01, CostSource::Reported)),
                    ),
                    usage_record(
                        Subscription::OpencodeGo,
                        Provider::OpencodeGo,
                        DataSource::OpencodeDb,
                        now - Duration::days(3),
                        10,
                        5,
                        Some((0.02, CostSource::Reported)),
                    ),
                    // A provider the dashboard does not know: `other`.
                    usage_record(
                        Subscription::Other,
                        Provider::Other,
                        DataSource::PiSession,
                        now - Duration::days(2),
                        7,
                        1,
                        Some((0.0, CostSource::Reported)),
                    ),
                ])
                .unwrap();
            storage
                .insert_quota_snapshot(&QuotaSnapshot {
                    id: None,
                    subscription: Subscription::Claude,
                    window: "five_hour".to_string(),
                    used_pct: Some(34.0),
                    resets_at: Some(now + Duration::hours(4)),
                    source: DataSource::ClaudeQuota,
                    observed_at: now - Duration::minutes(5),
                    plan_type: None,
                })
                .unwrap();
            // A quota window nobody has refreshed in three days.
            storage
                .insert_quota_snapshot(&QuotaSnapshot {
                    id: None,
                    subscription: Subscription::ChatGpt,
                    window: "primary".to_string(),
                    used_pct: Some(80.0),
                    resets_at: Some(now - Duration::minutes(1)),
                    source: DataSource::CodexRollout,
                    observed_at: now - Duration::days(3),
                    plan_type: Some("plus".to_string()),
                })
                .unwrap();
            storage
                .insert_quota_hit(&QuotaHit {
                    id: None,
                    subscription: Subscription::OpencodeGo,
                    period: Some("5h".to_string()),
                    reset_at: Some(now + Duration::hours(2)),
                    observed_at: now - Duration::minutes(30),
                    raw_kind: "go_usage_limit".to_string(),
                    dedup_key: "hit-1".to_string(),
                })
                .unwrap();
        }

        let req = make_request("status", serde_json::Value::Null);
        let resp = handle_request(&storage, &req, start_time, &collector_status).await;
        let status: claude_common::protocol::StatusResponse =
            serde_json::from_value(resp.result.expect("status result")).unwrap();

        let ids: Vec<&str> = status
            .subscriptions
            .iter()
            .map(|s| s.subscription.as_str())
            .collect();
        assert_eq!(ids, vec!["claude", "chatgpt", "opencode-go", "other"]);

        let claude = &status.subscriptions[0];
        assert_eq!(claude.today.input, 1_000);
        assert_eq!(claude.today.output, 500);
        assert!((claude.today.cost_usd.expect("claude cost") - 1.5).abs() < 1e-10);
        assert_eq!(claude.today.cost_source, CostSource::Computed);
        assert_eq!(claude.quota.len(), 1);
        assert_eq!(claude.quota[0].window, "five_hour");
        assert_eq!(claude.quota[0].used_pct, Some(34.0));
        assert_eq!(claude.quota[0].source, DataSource::ClaudeQuota);
        assert!(!claude.quota[0].stale, "fresh snapshot");
        assert!(!claude.quota[0].expired, "reset is in the future");
        assert!(claude.last_quota_hit.is_none());
        let claude_sources: Vec<&str> = claude
            .sources
            .iter()
            .map(|s| s.collector_id.as_str())
            .collect();
        assert_eq!(claude_sources, vec!["claude-logs", "claude-quota"]);
        assert!(claude.sources.iter().all(|s| !s.stale));

        let chatgpt = &status.subscriptions[1];
        assert_eq!(chatgpt.today.input, 2_000, "today excludes the old row");
        assert_eq!(chatgpt.last_7d.input, 2_400, "7d includes it");
        assert_eq!(chatgpt.quota.len(), 1);
        assert!(chatgpt.quota[0].stale, "no update in 24h");
        assert!(chatgpt.quota[0].expired, "reset time is in the past");
        // The codex rollout collector is live, even though the quota window it
        // last reported is three days old.
        assert!(chatgpt.sources.iter().all(|s| !s.stale));

        let opencode = &status.subscriptions[2];
        assert_eq!(opencode.today.input, 100);
        assert_eq!(opencode.last_7d.input, 110);
        let tools: Vec<(&str, &str)> = opencode
            .by_tool
            .iter()
            .map(|t| (t.provider.as_str(), t.source.as_str()))
            .collect();
        assert_eq!(
            tools,
            vec![
                ("opencode-go", "opencode-db"),
                ("opencode-go", "pi-session")
            ]
        );
        let pi_tool = opencode
            .by_tool
            .iter()
            .find(|t| t.source == DataSource::PiSession)
            .expect("pi breakdown");
        assert_eq!(pi_tool.totals.input, 100);
        let hit = opencode.last_quota_hit.as_ref().expect("last quota hit");
        assert_eq!(hit.period.as_deref(), Some("5h"));
        let sources: Vec<(&str, bool)> = opencode
            .sources
            .iter()
            .map(|s| (s.collector_id.as_str(), s.stale))
            .collect();
        assert_eq!(sources, vec![("opencode-db", true), ("pi-sessions", false)]);

        let other = &status.subscriptions[3];
        assert_eq!(other.today.input, 0);
        assert_eq!(other.last_7d.input, 7);
        assert_eq!(other.today.cost_usd, Some(0.0));
        assert_eq!(other.today.cost_source, CostSource::Reported);
    }

    #[tokio::test]
    async fn test_handle_request_status() {
        let storage = Arc::new(Mutex::new(Storage::new(":memory:").unwrap()));
        let start_time = std::time::Instant::now();
        let collector_status = Arc::new(Mutex::new(CollectorStatus::Offline));
        let req = make_request("status", serde_json::Value::Null);
        let resp = handle_request(&storage, &req, start_time, &collector_status).await;
        assert!(resp.result.is_some());
        assert!(resp.error.is_none());
    }

    #[tokio::test]
    async fn test_handle_request_budget_get() {
        let storage = Arc::new(Mutex::new(Storage::new(":memory:").unwrap()));
        let start_time = std::time::Instant::now();
        let collector_status = Arc::new(Mutex::new(CollectorStatus::Offline));
        let req = make_request("budget.get", serde_json::Value::Null);
        let resp = handle_request(&storage, &req, start_time, &collector_status).await;
        assert!(resp.result.is_some());
    }

    #[tokio::test]
    async fn test_handle_request_unknown_method() {
        let storage = Arc::new(Mutex::new(Storage::new(":memory:").unwrap()));
        let start_time = std::time::Instant::now();
        let collector_status = Arc::new(Mutex::new(CollectorStatus::Offline));
        let req = make_request("nonexistent", serde_json::Value::Null);
        let resp = handle_request(&storage, &req, start_time, &collector_status).await;
        assert!(resp.error.is_some());
        assert_eq!(
            resp.error.unwrap().code,
            claude_common::protocol::METHOD_NOT_FOUND
        );
    }

    #[tokio::test]
    async fn test_handle_request_usage_query_empty() {
        let storage = Arc::new(Mutex::new(Storage::new(":memory:").unwrap()));
        let start_time = std::time::Instant::now();
        let collector_status = Arc::new(Mutex::new(CollectorStatus::Offline));
        let req = make_request("usage.query", serde_json::json!({}));
        let resp = handle_request(&storage, &req, start_time, &collector_status).await;
        assert!(resp.result.is_some());

        let result: claude_common::protocol::UsageQueryResponse =
            serde_json::from_value(resp.result.unwrap()).unwrap();
        assert_eq!(result.total_count, 0);
        assert!(result.records.is_empty());
    }

    #[tokio::test]
    async fn test_handle_request_budget_roundtrip() {
        let storage = Arc::new(Mutex::new(Storage::new(":memory:").unwrap()));
        let start_time = std::time::Instant::now();
        let collector_status = Arc::new(Mutex::new(CollectorStatus::Offline));

        // Set budget
        let set_req = make_request(
            "budget.set",
            serde_json::json!({
                "daily_limit_usd": 10.0,
                "alert_threshold_pct": 0.75
            }),
        );
        let set_resp = handle_request(&storage, &set_req, start_time, &collector_status).await;
        assert!(set_resp.result.is_some());

        // Get budget
        let get_req = make_request("budget.get", serde_json::Value::Null);
        let get_resp = handle_request(&storage, &get_req, start_time, &collector_status).await;
        let config: claude_common::BudgetConfig =
            serde_json::from_value(get_resp.result.unwrap()).unwrap();
        assert_eq!(config.daily_limit_usd, Some(10.0));
    }

    #[tokio::test]
    async fn test_handle_request_session_not_found() {
        let storage = Arc::new(Mutex::new(Storage::new(":memory:").unwrap()));
        let start_time = std::time::Instant::now();
        let collector_status = Arc::new(Mutex::new(CollectorStatus::Offline));
        let req = make_request(
            "sessions.get",
            serde_json::json!({"session_id": "nonexistent"}),
        );
        let resp = handle_request(&storage, &req, start_time, &collector_status).await;
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, NOT_FOUND);
    }
}
