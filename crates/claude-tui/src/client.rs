use claude_common::protocol::MAX_MESSAGE_LEN;
use claude_common::{
    ActiveSession, BudgetConfig, BudgetSetResponse, IpcError, ModelsCompareParams,
    ModelsCompareResponse, RpcRequest, RpcResponse, SessionsGetParams, SessionsListParams,
    SessionsListResponse, StatusResponse, UsageQueryParams, UsageQueryResponse, UsageSummaryParams,
    UsageSummaryResponse,
};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::Mutex;
use tokio::time::{Duration, timeout};

const READ_TIMEOUT_SECS: u64 = 10;

pub struct DaemonClient {
    stream: Mutex<Option<BufReader<UnixStream>>>,
    next_id: AtomicU64,
    socket_path: String,
    read_timeout: Duration,
}

impl DaemonClient {
    /// Connect with an explicit connect and read timeout.
    ///
    /// `claude-tui status` uses a 2s budget so a dead daemon cannot stall a
    /// sketchybar refresh (AC-10).
    pub async fn connect_with_timeout(socket_path: &Path, limit: Duration) -> Option<Self> {
        let path_str = socket_path.to_string_lossy().to_string();
        let stream = timeout(limit, UnixStream::connect(socket_path))
            .await
            .ok()?
            .ok()?;
        Some(Self {
            stream: Mutex::new(Some(BufReader::new(stream))),
            next_id: AtomicU64::new(1),
            socket_path: path_str,
            read_timeout: limit,
        })
    }

    /// A client with no stream, remembering where to connect.
    ///
    /// The TUI keeps one of these when the daemon was down at startup, so it can
    /// retry with [`Self::reconnect`] on every refresh tick (AC-11).
    pub fn disconnected(socket_path: &Path) -> Self {
        Self {
            stream: Mutex::new(None),
            next_id: AtomicU64::new(1),
            socket_path: socket_path.to_string_lossy().to_string(),
            read_timeout: Duration::from_secs(READ_TIMEOUT_SECS),
        }
    }

    pub async fn reconnect(&self) -> bool {
        let path = Path::new(&self.socket_path);
        match timeout(self.read_timeout, UnixStream::connect(path)).await {
            Ok(Ok(stream)) => {
                let mut guard = self.stream.lock().await;
                *guard = Some(BufReader::new(stream));
                true
            }
            _ => false,
        }
    }

    pub fn is_connected(&self) -> bool {
        self.stream.try_lock().is_ok_and(|guard| guard.is_some())
    }

    async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, IpcError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let req = RpcRequest {
            jsonrpc: "2.0".to_string(),
            id,
            method: method.to_string(),
            params,
        };

        let mut req_bytes =
            serde_json::to_vec(&req).map_err(|e| IpcError::Serialization(e.to_string()))?;
        req_bytes.push(b'\n');

        let mut guard = self.stream.lock().await;
        let stream = guard.as_mut().ok_or_else(|| IpcError::DaemonNotRunning {
            path: self.socket_path.clone(),
        })?;

        if let Err(e) = stream.get_mut().write_all(&req_bytes).await {
            // A dead socket must not be reused: the next refresh tick reconnects.
            *guard = None;
            return Err(IpcError::Connection(e.to_string()));
        }

        // A response is read through a limited reader, so a runaway daemon
        // cannot buffer more than the cap here either (AC-12).
        let mut buf = Vec::new();
        let outcome = {
            let mut limited = stream.take(MAX_MESSAGE_LEN as u64 + 1);
            timeout(self.read_timeout, limited.read_until(b'\n', &mut buf)).await
        };
        let read = match outcome {
            Ok(Ok(0)) => {
                // Connection closed
                *guard = None;
                return Err(IpcError::Connection("server closed connection".into()));
            }
            Ok(Ok(read)) => read,
            Ok(Err(e)) => {
                *guard = None;
                return Err(IpcError::Connection(e.to_string()));
            }
            Err(_) => {
                // A late answer would be read as this request's answer, so drop
                // the stream instead of keeping it.
                *guard = None;
                return Err(IpcError::Timeout {
                    timeout_ms: self.read_timeout.as_millis() as u64,
                });
            }
        };

        if buf.last() != Some(&b'\n') && read > MAX_MESSAGE_LEN {
            // The other end kept writing past the cap: drop it.
            *guard = None;
            return Err(IpcError::Protocol(format!(
                "response line exceeds {MAX_MESSAGE_LEN} bytes"
            )));
        }

        let line = String::from_utf8(buf).map_err(|_| {
            *guard = None;
            IpcError::Deserialization("response is not valid UTF-8".to_string())
        })?;

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

#[cfg(test)]
mod tests {
    use super::*;

    /// A socket in a temporary directory, so the test never touches the real
    /// daemon socket or the home directory.
    fn temp_socket(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("claude-tui-client-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir.join(format!("{name}.sock"))
    }

    #[tokio::test]
    async fn ipc_hardening_rejects_an_oversized_response() {
        let path = temp_socket("oversized-response");
        let _ = std::fs::remove_file(&path);
        let listener = tokio::net::UnixListener::bind(&path).expect("bind temp socket");

        let client = DaemonClient::connect_with_timeout(&path, Duration::from_secs(5))
            .await
            .expect("connect");
        let (mut server, _) = listener.accept().await.expect("accept");

        // 100 KiB and no newline: over the cap, so the client rejects it rather
        // than buffering the whole line.
        tokio::spawn(async move {
            let _ = server.write_all(&vec![b'x'; 100 * 1024]).await;
        });

        let error = client
            .status()
            .await
            .expect_err("an oversized response is rejected");
        assert!(matches!(error, IpcError::Protocol(_)), "got {error:?}");
        assert!(!client.is_connected(), "the stream is dropped");

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn reconnect_after_connection_loss() {
        let path = temp_socket("daemon");
        let _ = std::fs::remove_file(&path);
        let listener = tokio::net::UnixListener::bind(&path).expect("bind temp socket");

        let client = DaemonClient::disconnected(&path);
        assert!(!client.is_connected());
        assert!(client.reconnect().await);
        assert!(client.is_connected());

        // The server hangs up without answering: the call fails and the client
        // drops the dead stream so the next refresh tick reconnects.
        let (stream, _) = listener.accept().await.expect("accept");
        drop(stream);
        assert!(client.status().await.is_err());
        assert!(!client.is_connected());

        let _ = std::fs::remove_file(&path);
    }
}
