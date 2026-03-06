use std::collections::HashMap;
use std::io::{BufRead, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use claude_common::protocol::CollectorStatus;
use claude_common::{CollectorError, DataSource, ModelType, UsageRecord};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::storage::Storage;

/// Configuration for the collector subsystem.
#[derive(Debug, Clone)]
pub struct CollectorConfig {
    pub poll_interval_secs: u64,
    pub log_paths: Vec<PathBuf>,
}

impl Default for CollectorConfig {
    fn default() -> Self {
        Self {
            poll_interval_secs: 60,
            log_paths: default_log_paths(),
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
        *self.collector_status.lock().await = CollectorStatus::Log;
        info!("collector starting — reading local Claude Code logs");

        // Track per-file byte offsets so only new lines are read each cycle.
        let mut file_cursors: HashMap<PathBuf, u64> = HashMap::new();

        let mut interval =
            tokio::time::interval(tokio::time::Duration::from_secs(self.config.poll_interval_secs));

        // First scan fires immediately (no wait).
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
                let storage = self.storage.lock().await;
                if let Err(e) = storage.rebuild_sessions() {
                    error!("failed to rebuild sessions: {e}");
                }
            }
        }

        Ok(())
    }
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
