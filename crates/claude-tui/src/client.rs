use claude_common::{
    ActiveSession, BudgetConfig, BudgetSetResponse, IpcError, ModelsCompareParams,
    ModelsCompareResponse, RpcRequest, RpcResponse, SessionsGetParams, SessionsListParams,
    SessionsListResponse, StatusResponse, UsageQueryParams, UsageQueryResponse,
    UsageSummaryParams, UsageSummaryResponse,
};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::Mutex;
use tokio::time::{timeout, Duration};

const READ_TIMEOUT_SECS: u64 = 10;

pub struct DaemonClient {
    stream: Mutex<Option<BufReader<UnixStream>>>,
    next_id: AtomicU64,
    socket_path: String,
}

impl DaemonClient {
    pub async fn connect(socket_path: &Path) -> Option<Self> {
        let path_str = socket_path.to_string_lossy().to_string();
        let stream = UnixStream::connect(socket_path).await.ok()?;
        Some(Self {
            stream: Mutex::new(Some(BufReader::new(stream))),
            next_id: AtomicU64::new(1),
            socket_path: path_str,
        })
    }

    #[allow(dead_code)]
    pub async fn reconnect(&self) -> bool {
        let path = Path::new(&self.socket_path);
        if let Ok(stream) = UnixStream::connect(path).await {
            let mut guard = self.stream.lock().await;
            *guard = Some(BufReader::new(stream));
            true
        } else {
            false
        }
    }

    pub fn is_connected(&self) -> bool {
        self.stream
            .try_lock()
            .map_or(false, |guard| guard.is_some())
    }

    async fn call(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value, IpcError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let req = RpcRequest {
            jsonrpc: "2.0".to_string(),
            id,
            method: method.to_string(),
            params,
        };

        let mut req_bytes = serde_json::to_vec(&req).map_err(|e| IpcError::Serialization(e.to_string()))?;
        req_bytes.push(b'\n');

        let mut guard = self.stream.lock().await;
        let stream = guard
            .as_mut()
            .ok_or_else(|| IpcError::DaemonNotRunning {
                path: self.socket_path.clone(),
            })?;

        stream
            .get_mut()
            .write_all(&req_bytes)
            .await
            .map_err(|e| IpcError::Connection(e.to_string()))?;

        let mut line = String::new();
        match timeout(
            Duration::from_secs(READ_TIMEOUT_SECS),
            stream.read_line(&mut line),
        )
        .await
        {
            Ok(Ok(0)) => {
                // Connection closed
                *guard = None;
                return Err(IpcError::Connection("server closed connection".into()));
            }
            Ok(Ok(_)) => { /* line read successfully, continue below */ }
            Ok(Err(e)) => return Err(IpcError::Connection(e.to_string())),
            Err(_) => {
                return Err(IpcError::Timeout {
                    timeout_ms: READ_TIMEOUT_SECS * 1000,
                })
            }
        }

        let resp: RpcResponse =
            serde_json::from_str(&line).map_err(|e| IpcError::Deserialization(e.to_string()))?;

        if let Some(err) = resp.error {
            return Err(IpcError::Protocol(format!(
                "RPC error {}: {}",
                err.code, err.message
            )));
        }

        resp.result
            .ok_or_else(|| IpcError::Protocol("empty result".to_string()))
    }

    pub async fn status(&self) -> Result<StatusResponse, IpcError> {
        let val = self.call("status", serde_json::Value::Null).await?;
        serde_json::from_value(val).map_err(|e| IpcError::Deserialization(e.to_string()))
    }

    #[allow(dead_code)]
    pub async fn query_usage(
        &self,
        params: UsageQueryParams,
    ) -> Result<UsageQueryResponse, IpcError> {
        let p = serde_json::to_value(params).map_err(|e| IpcError::Serialization(e.to_string()))?;
        let val = self.call("usage.query", p).await?;
        serde_json::from_value(val).map_err(|e| IpcError::Deserialization(e.to_string()))
    }

    pub async fn get_summary(
        &self,
        params: UsageSummaryParams,
    ) -> Result<UsageSummaryResponse, IpcError> {
        let p = serde_json::to_value(params).map_err(|e| IpcError::Serialization(e.to_string()))?;
        let val = self.call("usage.summary", p).await?;
        serde_json::from_value(val).map_err(|e| IpcError::Deserialization(e.to_string()))
    }

    pub async fn list_sessions(
        &self,
        params: SessionsListParams,
    ) -> Result<SessionsListResponse, IpcError> {
        let p = serde_json::to_value(params).map_err(|e| IpcError::Serialization(e.to_string()))?;
        let val = self.call("sessions.list", p).await?;
        serde_json::from_value(val).map_err(|e| IpcError::Deserialization(e.to_string()))
    }

    #[allow(dead_code)]
    pub async fn get_session(&self, id: &str) -> Result<ActiveSession, IpcError> {
        let params = SessionsGetParams {
            session_id: id.to_string(),
        };
        let p = serde_json::to_value(params).map_err(|e| IpcError::Serialization(e.to_string()))?;
        let val = self.call("sessions.get", p).await?;
        serde_json::from_value(val).map_err(|e| IpcError::Deserialization(e.to_string()))
    }

    pub async fn get_budget(&self) -> Result<BudgetConfig, IpcError> {
        let val = self.call("budget.get", serde_json::Value::Null).await?;
        serde_json::from_value(val).map_err(|e| IpcError::Deserialization(e.to_string()))
    }

    #[allow(dead_code)]
    pub async fn set_budget(&self, config: BudgetConfig) -> Result<BudgetSetResponse, IpcError> {
        let p = serde_json::to_value(config).map_err(|e| IpcError::Serialization(e.to_string()))?;
        let val = self.call("budget.set", p).await?;
        serde_json::from_value(val).map_err(|e| IpcError::Deserialization(e.to_string()))
    }

    pub async fn compare_models(
        &self,
        params: ModelsCompareParams,
    ) -> Result<ModelsCompareResponse, IpcError> {
        let p = serde_json::to_value(params).map_err(|e| IpcError::Serialization(e.to_string()))?;
        let val = self.call("models.compare", p).await?;
        serde_json::from_value(val).map_err(|e| IpcError::Deserialization(e.to_string()))
    }
}
