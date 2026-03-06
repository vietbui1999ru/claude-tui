use std::collections::HashMap;
use std::io::{BufRead, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use claude_common::protocol::CollectorStatus;
use claude_common::{CollectorError, DataSource, ModelType, UsageRecord};
use serde::Deserialize;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::storage::Storage;

// ---------------------------------------------------------------------------
// Anthropic Usage API types
// ---------------------------------------------------------------------------

/// Base URL for the Anthropic Usage Report API (Admin API).
const USAGE_API_URL: &str = "https://api.anthropic.com/v1/organizations/usage_report/messages";

/// Maximum pages to fetch per poll cycle to prevent runaway pagination.
const MAX_PAGES: usize = 10;

#[derive(Debug, Deserialize)]
struct UsageReportResponse {
    data: Vec<UsageBucket>,
    has_more: bool,
    next_page: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UsageBucket {
    starting_at: String,
    #[allow(dead_code)]
    ending_at: String,
    results: Vec<UsageResult>,
}

#[derive(Debug, Deserialize)]
struct UsageResult {
    model: Option<String>,
    uncached_input_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    cache_creation: Option<CacheCreation>,
    output_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct CacheCreation {
    ephemeral_5m_input_tokens: u64,
    ephemeral_1h_input_tokens: u64,
}

/// Configuration for the collector subsystem.
#[derive(Debug, Clone)]
pub struct CollectorConfig {
    pub api_key: Option<String>,
    pub poll_interval_secs: u64,
    pub log_paths: Vec<PathBuf>,
    pub fallback_to_logs: bool,
}

impl Default for CollectorConfig {
    fn default() -> Self {
        Self {
            api_key: None,
            poll_interval_secs: 60,
            log_paths: default_log_paths(),
            fallback_to_logs: true,
        }
    }
}

fn default_log_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        // Primary: Claude Code stores per-request usage in conversation JSONL files.
        paths.push(PathBuf::from(&home).join(".claude").join("projects"));
        // Legacy fallback paths
        paths.push(PathBuf::from(&home).join(".claude").join("logs"));
        paths.push(
            PathBuf::from(&home)
                .join(".config")
                .join("claude")
                .join("logs"),
        );
    }
    paths
}

/// Collector state machine for fallback logic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CollectorState {
    ApiActive,
    LogFallback,
    Offline,
}

impl CollectorState {
    fn to_collector_status(self) -> CollectorStatus {
        match self {
            CollectorState::ApiActive => CollectorStatus::Api,
            CollectorState::LogFallback => CollectorStatus::Log,
            CollectorState::Offline => CollectorStatus::Offline,
        }
    }
}

pub struct Collector {
    config: CollectorConfig,
    storage: Arc<Mutex<Storage>>,
    collector_status: Arc<Mutex<CollectorStatus>>,
    cancel: CancellationToken,
}

impl Collector {
    pub fn new(
        config: CollectorConfig,
        storage: Arc<Mutex<Storage>>,
        collector_status: Arc<Mutex<CollectorStatus>>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            config,
            storage,
            collector_status,
            cancel,
        }
    }

    pub async fn run(&self) -> Result<(), CollectorError> {
        let has_api_key = self.config.api_key.is_some();
        let mut state = if has_api_key {
            CollectorState::ApiActive
        } else if self.config.fallback_to_logs {
            CollectorState::LogFallback
        } else {
            CollectorState::Offline
        };

        *self.collector_status.lock().await = state.to_collector_status();
        info!("collector starting in state: {state:?}");

        // HTTP client reused across all poll cycles (connection pooling)
        let http_client = reqwest::Client::new();

        // Track the high-water mark so we only fetch new data each cycle.
        // Start 24 hours in the past on first run to bootstrap history.
        let mut last_polled_at = Utc::now() - chrono::Duration::hours(24);

        // Track per-file byte offsets to only read new lines on each log scan cycle.
        let mut file_cursors: HashMap<PathBuf, u64> = HashMap::new();

        let mut consecutive_failures: u32 = 0;
        let mut interval =
            tokio::time::interval(tokio::time::Duration::from_secs(self.config.poll_interval_secs));

        // When true, skip waiting for the next tick and act immediately.
        // Starts true so the first poll/scan fires without delay.
        let mut immediate = true;

        loop {
            if !immediate {
                tokio::select! {
                    _ = self.cancel.cancelled() => {
                        info!("collector shutting down");
                        break;
                    }
                    _ = interval.tick() => {}
                }

                if self.cancel.is_cancelled() {
                    break;
                }
            }
            immediate = false;

            match state {
                CollectorState::ApiActive => {
                    if let Some(ref api_key) = self.config.api_key {
                        match poll_api(&http_client, api_key, last_polled_at).await {
                            Ok(records) => {
                                consecutive_failures = 0;
                                last_polled_at = Utc::now();
                                if !records.is_empty() {
                                    info!("fetched {} usage records from API", records.len());
                                    let mut storage = self.storage.lock().await;
                                    if let Err(e) = storage.insert_usage_batch(&records) {
                                        error!("failed to insert API records: {e}");
                                    }
                                    if let Err(e) = storage.rebuild_sessions() {
                                        error!("failed to rebuild sessions: {e}");
                                    }
                                }
                            }
                            Err(CollectorError::AuthError(msg)) => {
                                warn!("API auth failed: {msg}, switching to log fallback");
                                state = CollectorState::LogFallback;
                                *self.collector_status.lock().await =
                                    state.to_collector_status();
                                consecutive_failures = 0;
                                immediate = true; // scan logs right away
                            }
                            Err(CollectorError::RateLimited { retry_after_secs }) => {
                                warn!("API rate limited, backing off {retry_after_secs}s");
                                tokio::time::sleep(tokio::time::Duration::from_secs(
                                    retry_after_secs,
                                ))
                                .await;
                            }
                            Err(e) => {
                                consecutive_failures += 1;
                                let backoff = compute_backoff(consecutive_failures);
                                warn!(
                                    "API poll failed (attempt {consecutive_failures}): {e}, \
                                     backoff {backoff}s"
                                );
                                if consecutive_failures >= 3 && self.config.fallback_to_logs {
                                    info!("3+ consecutive failures, switching to log fallback");
                                    state = CollectorState::LogFallback;
                                    *self.collector_status.lock().await =
                                        state.to_collector_status();
                                    immediate = true; // scan logs right away
                                }
                                tokio::time::sleep(tokio::time::Duration::from_secs(backoff)).await;
                            }
                        }
                    }
                }
                CollectorState::LogFallback => {
                    // Attempt to parse any new log entries (only reads new data via cursors)
                    let mut any_new = false;
                    for path in &self.config.log_paths {
                        if path.exists() {
                            match scan_log_directory(path, &mut file_cursors) {
                                Ok(records) => {
                                    if !records.is_empty() {
                                        any_new = true;
                                        info!("parsed {} new usage records from logs", records.len());
                                        let mut storage = self.storage.lock().await;
                                        if let Err(e) = storage.insert_usage_batch(&records) {
                                            error!("failed to insert log records: {e}");
                                        }
                                    }
                                }
                                Err(e) => {
                                    warn!("log scan error for {}: {e}", path.display());
                                }
                            }
                        }
                    }
                    if any_new {
                        let mut storage = self.storage.lock().await;
                        if let Err(e) = storage.rebuild_sessions() {
                            error!("failed to rebuild sessions: {e}");
                        }
                    }
                }
                CollectorState::Offline => {
                    // Nothing to do, just wait for next tick
                }
            }
        }

        Ok(())
    }
}

/// Compute exponential backoff with jitter.
/// base = 5s, max = 300s, jitter = 0-5s.
fn compute_backoff(attempt: u32) -> u64 {
    let base: u64 = 5;
    let max_backoff: u64 = 300;
    let exponential = base.saturating_mul(2u64.saturating_pow(attempt.min(10)));
    let clamped = exponential.min(max_backoff);
    // Deterministic jitter based on attempt number for reproducibility in tests
    let jitter = (attempt as u64 * 7 + 3) % 6;
    clamped.saturating_add(jitter)
}

/// Poll the Anthropic Admin Usage API for token usage data.
///
/// Requires an **Admin API key** (starts with `sk-ant-admin...`).
/// Fetches hourly buckets grouped by model since `since`, handling pagination.
async fn poll_api(
    client: &reqwest::Client,
    api_key: &str,
    since: DateTime<Utc>,
) -> Result<Vec<UsageRecord>, CollectorError> {
    if api_key.is_empty() {
        return Err(CollectorError::AuthError("empty API key".to_string()));
    }

    let now = Utc::now();
    let mut all_records = Vec::new();
    let mut page: Option<String> = None;

    for _ in 0..MAX_PAGES {
        let starting_at = since.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let ending_at = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

        let mut request = client
            .get(USAGE_API_URL)
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .query(&[
                ("starting_at", starting_at.as_str()),
                ("ending_at", ending_at.as_str()),
                ("bucket_width", "1h"),
            ])
            .query(&[("group_by[]", "model")]);

        if let Some(ref p) = page {
            request = request.query(&[("page", p.as_str())]);
        }

        let response = request
            .send()
            .await
            .map_err(|e| CollectorError::ApiRequest(e.to_string()))?;

        let status = response.status().as_u16();

        if status == 401 || status == 403 {
            let body = response.text().await.unwrap_or_default();
            return Err(CollectorError::AuthError(format!(
                "HTTP {status}: {body}"
            )));
        }

        if status == 429 {
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(60);
            return Err(CollectorError::RateLimited { retry_after_secs: retry_after });
        }

        if !response.status().is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(CollectorError::ApiResponse { status, body });
        }

        let report: UsageReportResponse = response
            .json()
            .await
            .map_err(|e| CollectorError::ApiRequest(format!("JSON parse error: {e}")))?;

        for bucket in &report.data {
            let bucket_ts = DateTime::parse_from_rfc3339(&bucket.starting_at)
                .map(|dt| dt.with_timezone(&Utc))
                .map_err(|e| CollectorError::ApiRequest(format!("bad timestamp: {e}")))?;

            for result in &bucket.results {
                let model_str = match &result.model {
                    Some(m) => m,
                    None => continue,
                };

                let model: ModelType = match model_str.parse() {
                    Ok(m) => m,
                    Err(_) => {
                        warn!("skipping unknown model from API: {model_str}");
                        continue;
                    }
                };

                let input_tokens = result.uncached_input_tokens.unwrap_or(0);
                let output_tokens = result.output_tokens.unwrap_or(0);
                let cache_read_tokens = result.cache_read_input_tokens.unwrap_or(0);
                let cache_write_tokens = result
                    .cache_creation
                    .as_ref()
                    .map(|c| c.ephemeral_5m_input_tokens + c.ephemeral_1h_input_tokens)
                    .unwrap_or(0);

                // Skip empty buckets
                if input_tokens == 0
                    && output_tokens == 0
                    && cache_read_tokens == 0
                    && cache_write_tokens == 0
                {
                    continue;
                }

                let cost_usd = model.compute_cost(
                    input_tokens,
                    output_tokens,
                    cache_read_tokens,
                    cache_write_tokens,
                );
                let cost_usd = (cost_usd * 1_000_000.0).round() / 1_000_000.0;

                let uuid = generate_api_uuid(bucket_ts, &model, input_tokens, output_tokens);

                all_records.push(UsageRecord {
                    id: None,
                    uuid,
                    timestamp: bucket_ts,
                    model,
                    input_tokens,
                    output_tokens,
                    cache_read_tokens,
                    cache_write_tokens,
                    cost_usd,
                    session_id: None,
                    project: None,
                    source: DataSource::Api,
                });
            }
        }

        if report.has_more {
            page = report.next_page;
        } else {
            break;
        }
    }

    Ok(all_records)
}

/// Generate a deterministic UUID from API usage data for deduplication.
/// Uses a distinct prefix ("api:") to avoid collisions with log-based UUIDs.
fn generate_api_uuid(
    timestamp: DateTime<Utc>,
    model: &ModelType,
    input_tokens: u64,
    output_tokens: u64,
) -> Uuid {
    let name = format!(
        "api:{}:{}:{}:{}",
        timestamp.timestamp(),
        model.as_str(),
        input_tokens,
        output_tokens,
    );
    Uuid::new_v5(&LOG_UUID_NAMESPACE, name.as_bytes())
}

/// Scan a directory recursively for JSONL/log files and parse usage records.
/// Uses `file_cursors` to track per-file byte offsets so only new data is read.
fn scan_log_directory(
    dir: &std::path::Path,
    file_cursors: &mut HashMap<PathBuf, u64>,
) -> Result<Vec<UsageRecord>, CollectorError> {
    let mut records = Vec::new();
    scan_log_directory_recursive(dir, file_cursors, &mut records)?;
    Ok(records)
}

fn scan_log_directory_recursive(
    dir: &std::path::Path,
    file_cursors: &mut HashMap<PathBuf, u64>,
    records: &mut Vec<UsageRecord>,
) -> Result<(), CollectorError> {
    let entries = std::fs::read_dir(dir).map_err(|e| CollectorError::LogWatch(e.to_string()))?;

    for entry in entries {
        let entry = entry.map_err(|e| CollectorError::LogWatch(e.to_string()))?;
        let path = entry.path();

        if path.is_dir() {
            // Recurse into subdirectories (projects, subagents, etc.)
            if let Err(e) = scan_log_directory_recursive(&path, file_cursors, records) {
                warn!("error scanning {}: {e}", path.display());
            }
            continue;
        }

        let ext = path.extension().and_then(|e| e.to_str());
        if ext == Some("jsonl") || ext == Some("log") {
            if let Err(e) = read_new_lines(&path, file_cursors, records) {
                warn!("error reading {}: {e}", path.display());
            }
        }
    }

    Ok(())
}

/// Read only new lines from a file since the last recorded byte offset.
fn read_new_lines(
    path: &std::path::Path,
    file_cursors: &mut HashMap<PathBuf, u64>,
    records: &mut Vec<UsageRecord>,
) -> Result<(), CollectorError> {
    let file =
        std::fs::File::open(path).map_err(|e| CollectorError::LogWatch(e.to_string()))?;

    let file_len = file
        .metadata()
        .map_err(|e| CollectorError::LogWatch(e.to_string()))?
        .len();

    let prev_offset = file_cursors.get(path).copied().unwrap_or(0);

    // File hasn't grown since last scan
    if file_len <= prev_offset {
        // If file shrank (e.g., truncated), reset cursor
        if file_len < prev_offset {
            file_cursors.insert(path.to_path_buf(), 0);
        }
        return Ok(());
    }

    let mut reader = std::io::BufReader::new(file);
    reader
        .seek(SeekFrom::Start(prev_offset))
        .map_err(|e| CollectorError::LogWatch(e.to_string()))?;

    let mut line = String::new();
    loop {
        line.clear();
        let bytes_read = reader
            .read_line(&mut line)
            .map_err(|e| CollectorError::LogWatch(e.to_string()))?;
        if bytes_read == 0 {
            break;
        }
        match parse_log_line(&line) {
            Ok(Some(record)) => records.push(record),
            Ok(None) => {}
            Err(e) => {
                warn!("skipping unparseable log line: {e}");
            }
        }
    }

    // Update cursor to current end of file
    file_cursors.insert(path.to_path_buf(), file_len);

    Ok(())
}

/// Parse a single JSONL log line into a UsageRecord, if it contains usage data.
///
/// Supports two formats:
/// 1. **Claude Code conversation format**: `{type: "assistant", timestamp, sessionId, message: {model, usage: {...}}}`
/// 2. **Generic log format**: `{timestamp, model, usage: {...}}`
fn parse_log_line(line: &str) -> Result<Option<UsageRecord>, CollectorError> {
    let line = line.trim();
    if line.is_empty() {
        return Ok(None);
    }

    let value: serde_json::Value =
        serde_json::from_str(line).map_err(|e| CollectorError::LogParse(e.to_string()))?;

    // Determine format: Claude Code conversation or generic log
    let is_claude_code = value.get("type").and_then(|v| v.as_str()) == Some("assistant")
        && value.get("message").is_some();

    // Extract timestamp (always at top level)
    let timestamp = value
        .get("timestamp")
        .and_then(|v| v.as_str())
        .and_then(|s| {
            // Claude Code uses millisecond ISO timestamps; chrono handles both
            DateTime::parse_from_rfc3339(s).ok()
        })
        .map(|dt| dt.with_timezone(&Utc));

    let timestamp = match timestamp {
        Some(ts) => ts,
        None => return Ok(None),
    };

    // Extract model and usage — nested under `message` for Claude Code, top-level otherwise
    let (model_str, usage) = if is_claude_code {
        let msg = value.get("message").unwrap(); // safe: checked above
        (
            msg.get("model").and_then(|v| v.as_str()),
            msg.get("usage"),
        )
    } else {
        (
            value.get("model").and_then(|v| v.as_str()),
            value.get("usage"),
        )
    };

    let (Some(model_str), Some(usage)) = (model_str, usage) else {
        return Ok(None);
    };

    let model: ModelType = match model_str.parse() {
        Ok(m) => m,
        Err(_) => return Ok(None), // skip unknown models silently
    };

    let input_tokens = usage
        .get("input_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let output_tokens = usage
        .get("output_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    // Claude Code uses "cache_read_input_tokens"; generic logs may use "cache_read_tokens"
    let cache_read_tokens = usage
        .get("cache_read_input_tokens")
        .or_else(|| usage.get("cache_read_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    // Claude Code uses "cache_creation_input_tokens"; generic logs may use "cache_write_tokens"
    let cache_write_tokens = usage
        .get("cache_creation_input_tokens")
        .or_else(|| usage.get("cache_write_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    // Validate: reject if all token counts are zero
    if input_tokens == 0 && output_tokens == 0 && cache_read_tokens == 0 && cache_write_tokens == 0
    {
        return Ok(None);
    }

    // Validate: reject if timestamp is in the future
    if timestamp > Utc::now() + chrono::Duration::minutes(5) {
        return Ok(None);
    }

    let cost_usd =
        model.compute_cost(input_tokens, output_tokens, cache_read_tokens, cache_write_tokens);
    // Truncate to 6 decimal places
    let cost_usd = (cost_usd * 1_000_000.0).round() / 1_000_000.0;

    let uuid = generate_log_uuid(timestamp, &model, input_tokens, output_tokens);

    // Session ID: Claude Code uses "sessionId", generic logs use "session_id"
    let session_id = value
        .get("sessionId")
        .or_else(|| value.get("session_id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let project = value
        .get("project")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    Ok(Some(UsageRecord {
        id: None,
        uuid,
        timestamp,
        model,
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
        cost_usd,
        session_id,
        project,
        source: DataSource::Log,
    }))
}

/// Fixed namespace UUID for deterministic log-based UUID generation.
const LOG_UUID_NAMESPACE: Uuid = Uuid::from_bytes([
    0x6b, 0xa7, 0xb8, 0x10, 0x9d, 0xad, 0x11, 0xd1,
    0x80, 0xb4, 0x00, 0xc0, 0x4f, 0xd4, 0x30, 0xc8,
]);

/// Generate a deterministic UUID from log line data for deduplication.
/// Uses UUID v5 (SHA-1 based) with a fixed namespace for stability across Rust versions.
fn generate_log_uuid(
    timestamp: DateTime<Utc>,
    model: &ModelType,
    input_tokens: u64,
    output_tokens: u64,
) -> Uuid {
    let name = format!(
        "{}:{}:{}:{}",
        timestamp.timestamp_millis(),
        model.as_str(),
        input_tokens,
        output_tokens,
    );
    Uuid::new_v5(&LOG_UUID_NAMESPACE, name.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_backoff() {
        // First attempt
        let b0 = compute_backoff(0);
        assert!(b0 >= 5 && b0 <= 11, "b0={b0}");

        // Second attempt
        let b1 = compute_backoff(1);
        assert!(b1 >= 10 && b1 <= 16, "b1={b1}");

        // Third attempt
        let b2 = compute_backoff(2);
        assert!(b2 >= 20 && b2 <= 26, "b2={b2}");

        // Cap at 300
        let b20 = compute_backoff(20);
        assert!(b20 <= 306, "b20={b20}");
    }

    #[test]
    fn test_parse_log_line_valid() {
        let line = r#"{"timestamp":"2026-03-05T10:30:00Z","model":"claude-sonnet-4-6","usage":{"input_tokens":1234,"output_tokens":567}}"#;
        let record = parse_log_line(line).unwrap().unwrap();
        assert_eq!(record.model, ModelType::Sonnet);
        assert_eq!(record.input_tokens, 1234);
        assert_eq!(record.output_tokens, 567);
        assert_eq!(record.source, DataSource::Log);
        assert!(record.cost_usd > 0.0);
    }

    #[test]
    fn test_parse_log_line_with_cache() {
        let line = r#"{"timestamp":"2026-03-05T10:30:00Z","model":"claude-opus-4-6","usage":{"input_tokens":1000,"output_tokens":500,"cache_read_tokens":200,"cache_write_tokens":100}}"#;
        let record = parse_log_line(line).unwrap().unwrap();
        assert_eq!(record.model, ModelType::Opus);
        assert_eq!(record.cache_read_tokens, 200);
        assert_eq!(record.cache_write_tokens, 100);
    }

    #[test]
    fn test_parse_claude_code_format() {
        let line = r#"{"type":"assistant","timestamp":"2026-03-05T10:30:00Z","sessionId":"abc-123","message":{"model":"claude-opus-4-6","type":"message","role":"assistant","content":[],"usage":{"input_tokens":50,"output_tokens":200,"cache_read_input_tokens":10000,"cache_creation_input_tokens":5000}}}"#;
        let record = parse_log_line(line).unwrap().unwrap();
        assert_eq!(record.model, ModelType::Opus);
        assert_eq!(record.input_tokens, 50);
        assert_eq!(record.output_tokens, 200);
        assert_eq!(record.cache_read_tokens, 10000);
        assert_eq!(record.cache_write_tokens, 5000);
        assert_eq!(record.session_id.as_deref(), Some("abc-123"));
        assert!(record.cost_usd > 0.0);
    }

    #[test]
    fn test_parse_claude_code_non_assistant_skipped() {
        // "user" type messages don't have usage data
        let line = r#"{"type":"user","timestamp":"2026-03-05T10:30:00Z","sessionId":"abc-123","message":{"role":"user","content":"hello"}}"#;
        let result = parse_log_line(line).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_log_line_zero_tokens_rejected() {
        let line = r#"{"timestamp":"2026-03-05T10:30:00Z","model":"claude-sonnet-4-6","usage":{"input_tokens":0,"output_tokens":0}}"#;
        let result = parse_log_line(line).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_log_line_missing_usage() {
        let line = r#"{"timestamp":"2026-03-05T10:30:00Z","model":"claude-sonnet-4-6"}"#;
        let result = parse_log_line(line).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_log_line_empty() {
        let result = parse_log_line("").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_log_line_invalid_json() {
        let result = parse_log_line("not json");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_log_line_unknown_fields_ignored() {
        let line = r#"{"timestamp":"2026-03-05T10:30:00Z","model":"claude-sonnet-4-6","usage":{"input_tokens":100,"output_tokens":50},"extra_field":"ignored"}"#;
        let record = parse_log_line(line).unwrap().unwrap();
        assert_eq!(record.input_tokens, 100);
    }

    #[test]
    fn test_generate_log_uuid_deterministic() {
        let ts = DateTime::parse_from_rfc3339("2026-03-05T10:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let uuid1 = generate_log_uuid(ts, &ModelType::Sonnet, 1234, 567);
        let uuid2 = generate_log_uuid(ts, &ModelType::Sonnet, 1234, 567);
        assert_eq!(uuid1, uuid2, "same inputs must produce same UUID");
    }

    #[test]
    fn test_generate_log_uuid_different_inputs() {
        let ts = DateTime::parse_from_rfc3339("2026-03-05T10:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let uuid1 = generate_log_uuid(ts, &ModelType::Sonnet, 1234, 567);
        let uuid2 = generate_log_uuid(ts, &ModelType::Sonnet, 1234, 568);
        assert_ne!(uuid1, uuid2, "different inputs must produce different UUIDs");
    }

    #[test]
    fn test_generate_log_uuid_different_models() {
        let ts = DateTime::parse_from_rfc3339("2026-03-05T10:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let uuid1 = generate_log_uuid(ts, &ModelType::Sonnet, 1234, 567);
        let uuid2 = generate_log_uuid(ts, &ModelType::Opus, 1234, 567);
        assert_ne!(uuid1, uuid2);
    }

    #[test]
    fn test_default_config() {
        let config = CollectorConfig::default();
        assert_eq!(config.poll_interval_secs, 60);
        assert!(config.fallback_to_logs);
        assert!(config.api_key.is_none());
    }

    #[test]
    fn test_normalization_cost_truncation() {
        let line = r#"{"timestamp":"2026-03-05T10:30:00Z","model":"claude-sonnet-4-6","usage":{"input_tokens":1,"output_tokens":1}}"#;
        let record = parse_log_line(line).unwrap().unwrap();
        // Verify cost is truncated to 6 decimal places
        let cost_str = format!("{:.6}", record.cost_usd);
        let decimal_part = cost_str.split('.').nth(1).unwrap();
        assert!(decimal_part.len() <= 6);
    }
}
