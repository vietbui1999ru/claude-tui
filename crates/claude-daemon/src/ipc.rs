use std::path::{Path, PathBuf};
use std::sync::Arc;

use claude_common::protocol::{
    BudgetSetResponse, CollectorStatus, RpcError, RpcMethod, RpcRequest, RpcResponse,
    StatusResponse, INTERNAL_ERROR, NOT_FOUND, PARSE_ERROR,
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
            std::fs::set_permissions(
                &self.socket_path,
                std::fs::Permissions::from_mode(0o600),
            )
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
            warn!("dropping connection: request line too large ({} bytes)", line.len());
            break;
        }

        let response = match serde_json::from_str::<RpcRequest>(&line) {
            Ok(request) => {
                handle_request(&storage, &request, start_time, &collector_status).await
            }
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
            let budget_pct = budget.daily_limit_usd.map(|limit| {
                if limit > 0.0 {
                    cost_today / limit
                } else {
                    0.0
                }
            });

            let resp = StatusResponse {
                daemon_uptime_secs: uptime,
                active_sessions: sessions.total_count as u32,
                current_model,
                cost_today_usd: cost_today,
                budget_pct,
                collector_status: cs,
            };
            RpcResponse::success(request.id, serde_json::to_value(&resp).unwrap_or_default())
        }
        RpcMethod::UsageQuery(params) => {
            let storage = storage.lock().await;
            match storage.query_usage(&params) {
                Ok(result) => {
                    RpcResponse::success(request.id, serde_json::to_value(&result).unwrap_or_default())
                }
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
                Ok(result) => {
                    RpcResponse::success(request.id, serde_json::to_value(&result).unwrap_or_default())
                }
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
                Ok(result) => {
                    RpcResponse::success(request.id, serde_json::to_value(&result).unwrap_or_default())
                }
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
                Ok(result) => {
                    RpcResponse::success(request.id, serde_json::to_value(&result).unwrap_or_default())
                }
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
    use claude_common::protocol::RpcRequest;

    fn make_request(method: &str, params: serde_json::Value) -> RpcRequest {
        RpcRequest {
            jsonrpc: "2.0".to_string(),
            id: 1,
            method: method.to_string(),
            params,
        }
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
