use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use claude_common::protocol::{
    BudgetSetResponse, CollectorStatus, INTERNAL_ERROR, INVALID_REQUEST, MAX_MESSAGE_LEN,
    NOT_FOUND, PARSE_ERROR, RpcError, RpcMethod, RpcRequest, RpcResponse, StatusResponse,
};
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
};
use tokio::net::UnixListener;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::storage::Storage;

/// Socket file mode: owner read and write only (V3).
const SOCKET_MODE: u32 = 0o600;

/// One line read from a client, capped at [`MAX_MESSAGE_LEN`].
enum RequestLine {
    /// A complete line, without its terminating newline.
    Line(String),
    /// The line was not valid UTF-8.
    Invalid,
    /// The line exceeded the cap; only `buffered` bytes were held.
    TooLong { buffered: usize },
    /// The client closed the connection.
    Closed,
}

/// Read one newline-terminated request line.
///
/// Reading at most one byte past the cap is what keeps the buffered amount
/// bounded: the rest of an over-long line stays unread in the socket.
async fn read_request_line<R>(reader: &mut R) -> std::io::Result<RequestLine>
where
    R: AsyncBufRead + Unpin,
{
    let mut buf = Vec::new();
    reader
        .take(MAX_MESSAGE_LEN as u64 + 1)
        .read_until(b'\n', &mut buf)
        .await?;

    if buf.is_empty() {
        return Ok(RequestLine::Closed);
    }

    let terminated = buf.last() == Some(&b'\n');
    if !terminated && buf.len() > MAX_MESSAGE_LEN {
        return Ok(RequestLine::TooLong {
            buffered: buf.len(),
        });
    }
    if terminated {
        buf.pop();
    }

    Ok(match String::from_utf8(buf) {
        Ok(line) => RequestLine::Line(line),
        Err(_) => RequestLine::Invalid,
    })
}

/// Write one JSON-RPC response followed by a newline.
async fn send<W>(
    writer: &mut W,
    response: &RpcResponse,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    W: AsyncWrite + Unpin,
{
    let mut json = serde_json::to_string(response)?;
    json.push('\n');
    writer.write_all(json.as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}

/// Bind the IPC socket, owner-only from the moment it exists.
///
/// The socket file is created under a `0o077` umask and then chmod-ed, so there
/// is no window in which another local user could connect (V3).
fn bind_private(path: &Path) -> Result<UnixListener, claude_common::IpcError> {
    let bind_error = |e: std::io::Error| claude_common::IpcError::SocketBind {
        path: path.display().to_string(),
        reason: e.to_string(),
    };

    // SAFETY: `umask` has no memory-safety preconditions. It is process-wide, so
    // the mask captured here is restored immediately after `bind` returns, before
    // this task creates any other file.
    let previous = unsafe { libc::umask(0o077) };
    let bound = UnixListener::bind(path);
    // SAFETY: restores the mask captured above; same reasoning.
    unsafe { libc::umask(previous) };

    let listener = bound.map_err(bind_error)?;
    // A path that already existed keeps its old mode, so set it explicitly and
    // propagate a failure rather than racing on a permissive socket.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(SOCKET_MODE))
        .map_err(bind_error)?;
    Ok(listener)
}

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

        let listener = bind_private(&self.socket_path)?;

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
    let mut reader = BufReader::new(reader);

    loop {
        let line = tokio::select! {
            _ = cancel.cancelled() => break,
            result = read_request_line(&mut reader) => result?,
        };

        let line = match line {
            RequestLine::Closed => break,
            RequestLine::Invalid => {
                // The line boundary is intact, so the connection can continue.
                send(
                    &mut writer,
                    &RpcResponse::error(
                        0,
                        RpcError {
                            code: PARSE_ERROR,
                            message: "request line is not valid UTF-8".to_string(),
                            data: None,
                        },
                    ),
                )
                .await?;
                continue;
            }
            RequestLine::TooLong { buffered } => {
                // Reject and close: the rest of the oversized line is never read.
                warn!(
                    "dropping connection: request line exceeds {MAX_MESSAGE_LEN} bytes \
                     ({buffered} bytes buffered)"
                );
                send(
                    &mut writer,
                    &RpcResponse::error(
                        0,
                        RpcError {
                            code: INVALID_REQUEST,
                            message: format!("request line exceeds {MAX_MESSAGE_LEN} bytes"),
                            data: None,
                        },
                    ),
                )
                .await?;
                break;
            }
            RequestLine::Line(line) => line,
        };

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

        send(&mut writer, &response).await?;
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

    /// A per-test directory under the system temp dir, never the home directory.
    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("claude-daemon-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    /// The permission bits of a file, e.g. `0o600`.
    fn file_mode(path: &Path) -> u32 {
        std::fs::metadata(path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777
    }

    #[tokio::test]
    async fn ipc_hardening_socket_is_owner_only() {
        let dir = temp_dir("ipc-hardening-socket");

        // Baseline: the mode a file gets from the process umask today.
        let probe = dir.join("umask-before");
        std::fs::write(&probe, b"x").expect("write probe");
        let mode_before = file_mode(&probe);

        let path = dir.join("daemon.sock");
        let _ = std::fs::remove_file(&path);
        let listener = bind_private(&path).expect("bind the socket");

        assert_eq!(file_mode(&path), SOCKET_MODE, "socket mode is 0600");

        // The umask is restored, so a file created after the bind has the mode
        // it would have had before it rather than the bind-time 0600.
        let probe = dir.join("umask-after");
        std::fs::write(&probe, b"x").expect("write probe");
        assert_eq!(file_mode(&probe), mode_before, "umask restored after bind");

        drop(listener);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn ipc_hardening_reads_lines_up_to_the_cap() {
        // A line of exactly the cap is accepted; the sentinel after it is still
        // framed correctly.
        let at_cap = "a".repeat(MAX_MESSAGE_LEN);
        let stream = format!("{{\"id\":1}}\n{at_cap}\n");
        let mut reader = BufReader::new(stream.as_bytes());

        match read_request_line(&mut reader).await.expect("first line") {
            RequestLine::Line(line) => assert_eq!(line, "{\"id\":1}"),
            _ => panic!("a short request is a line"),
        }
        match read_request_line(&mut reader).await.expect("second line") {
            RequestLine::Line(line) => assert_eq!(line.len(), MAX_MESSAGE_LEN),
            _ => panic!("a line of exactly the cap is accepted"),
        }
        assert!(matches!(
            read_request_line(&mut reader).await.expect("end"),
            RequestLine::Closed
        ));
    }

    #[tokio::test]
    async fn ipc_hardening_oversized_line_buffering_is_bounded() {
        // 100 KiB with no newline: over the cap.
        let payload = vec![b'x'; 100 * 1024];
        let mut reader = BufReader::new(&payload[..]);

        match read_request_line(&mut reader).await.expect("read a line") {
            RequestLine::TooLong { buffered } => assert_eq!(
                buffered,
                MAX_MESSAGE_LEN + 1,
                "the reader holds at most the cap plus one byte"
            ),
            _ => panic!("an over-long line is rejected"),
        }

        // Nothing past the cap was buffered: the tail is still in the stream.
        let mut rest = Vec::new();
        reader.read_to_end(&mut rest).await.expect("read the tail");
        assert_eq!(rest.len(), payload.len() - (MAX_MESSAGE_LEN + 1));
    }

    #[tokio::test]
    async fn ipc_hardening_rejects_a_100_kib_request() {
        let storage = Arc::new(Mutex::new(Storage::new(":memory:").expect("in-memory")));
        let collector_status = Arc::new(Mutex::new(CollectorStatus::Offline));
        let (server, client) = tokio::net::UnixStream::pair().expect("socket pair");
        let handle = tokio::spawn(handle_client(
            server,
            storage,
            std::time::Instant::now(),
            collector_status,
            CancellationToken::new(),
        ));

        let (client_read, mut client_write) = client.into_split();
        // The write can block once the daemon stops reading at the cap; the
        // error that follows the close is expected and ignored.
        let writer = tokio::spawn(async move {
            let _ = client_write.write_all(&vec![b'x'; 100 * 1024]).await;
        });

        let mut reader = BufReader::new(client_read);
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .await
            .expect("read the error response");
        let response: RpcResponse = serde_json::from_str(&line).expect("json-rpc response");
        let error = response.error.expect("invalid-request error");
        assert_eq!(error.code, INVALID_REQUEST);
        assert!(
            error.message.contains(&MAX_MESSAGE_LEN.to_string()),
            "the message names the cap: {}",
            error.message
        );

        // The connection is closed instead of the rest being buffered. The
        // daemon closes with unread bytes still queued, which macOS reports as
        // EOF and Linux as ECONNRESET; both mean closed. The timeout fails the
        // test if the connection were left open.
        let mut rest = String::new();
        let closed = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            reader.read_line(&mut rest),
        )
        .await
        .expect("connection closed within 5s");
        match closed {
            Ok(n) => assert_eq!(n, 0, "nothing after the error response"),
            Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::ConnectionReset),
        }

        let _ = writer.await;
        handle.await.expect("handler task").expect("handler result");
    }

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
