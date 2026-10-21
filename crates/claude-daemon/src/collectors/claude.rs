//! Claude Code conversation logs (`~/.claude/projects/**/*.jsonl`).
//!
//! Claude Code writes one line per content block of an assistant message, and
//! every line repeats the same `usage`. A record is therefore keyed by
//! `message.id` and `requestId` (AC-17), so the repeats collapse to one.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use claude_common::{
    CollectorError, CostSource, DataSource, PriceBook, Provider, UsageRecord, subscription_for,
};
use tracing::warn;
use uuid::Uuid;

use super::{CollectOutput, Cursor, SourceCollector, read_jsonl_from, walk_files};

/// Stable collector id, also the cursor namespace.
pub const COLLECTOR_ID: &str = "claude-logs";

/// Fixed namespace UUID for deterministic log-based UUID generation.
const LOG_UUID_NAMESPACE: Uuid = Uuid::from_bytes([
    0x6b, 0xa7, 0xb8, 0x10, 0x9d, 0xad, 0x11, 0xd1, 0x80, 0xb4, 0x00, 0xc0, 0x4f, 0xd4, 0x30, 0xc8,
]);

pub struct ClaudeLogCollector {
    roots: Vec<PathBuf>,
    prices: PriceBook,
}

impl ClaudeLogCollector {
    pub fn new(roots: Vec<PathBuf>, prices: PriceBook) -> Self {
        Self { roots, prices }
    }

    fn files(&self) -> Vec<PathBuf> {
        let mut files = Vec::new();
        for root in &self.roots {
            files.extend(walk_files(root, &|p| {
                matches!(
                    p.extension().and_then(|e| e.to_str()),
                    Some("jsonl" | "log")
                )
            }));
        }
        files
    }
}

impl SourceCollector for ClaudeLogCollector {
    fn id(&self) -> &'static str {
        COLLECTOR_ID
    }

    fn collect(&mut self, cursor: &Cursor) -> Result<CollectOutput, CollectorError> {
        let mut out = CollectOutput::default();
        for path in self.files() {
            let key = path.display().to_string();
            let offset = cursor.get_u64(&key);
            let chunk = match read_jsonl_from(&path, offset) {
                Ok(chunk) => chunk,
                Err(e) => {
                    warn!("error reading {}: {e}", path.display());
                    continue;
                }
            };
            for line in &chunk.lines {
                match parse_log_line(line, &self.prices) {
                    Ok(Some(record)) => out.records.push(record),
                    Ok(None) => {}
                    Err(e) => warn!("skipping unparseable log line: {e}"),
                }
            }
            out.cursor.set_u64(key, chunk.next_offset);
        }
        Ok(out)
    }
}

/// Parse a single JSONL log line into a UsageRecord, if it contains usage data.
///
/// Supports two formats:
/// 1. **Claude Code conversation format**: `{type: "assistant", timestamp, sessionId, message: {id, model, usage: {...}}, requestId}`
/// 2. **Generic log format**: `{timestamp, model, usage: {...}}`
pub fn parse_log_line(
    line: &str,
    prices: &PriceBook,
) -> Result<Option<UsageRecord>, CollectorError> {
    let line = line.trim();
    if line.is_empty() {
        return Ok(None);
    }

    let value: serde_json::Value =
        serde_json::from_str(line).map_err(|e| CollectorError::LogParse(e.to_string()))?;

    // Determine format: Claude Code conversation or generic log
    let claude_message = match value.get("type").and_then(|v| v.as_str()) {
        Some("assistant") => value.get("message"),
        _ => None,
    };

    let timestamp = value
        .get("timestamp")
        .and_then(|v| v.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc));

    let Some(timestamp) = timestamp else {
        return Ok(None);
    };

    // Model and usage live under `message` for Claude Code, top-level otherwise.
    let (model_str, usage) = match claude_message {
        Some(msg) => (msg.get("model").and_then(|v| v.as_str()), msg.get("usage")),
        None => (
            value.get("model").and_then(|v| v.as_str()),
            value.get("usage"),
        ),
    };

    let (Some(model_str), Some(usage)) = (model_str, usage) else {
        return Ok(None);
    };

    // The model id is kept verbatim: prices resolve it later, by substring, and
    // a model we have never seen keeps its tokens with an unknown cost.
    let model = model_str.to_string();

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

    // Truncate to 6 decimal places. A model with no configured price keeps its
    // tokens and its cost is unknown.
    let cost_usd = prices
        .cost(
            &model,
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
        )
        .map(|cost| (cost * 1_000_000.0).round() / 1_000_000.0);
    let cost_source = match cost_usd {
        Some(_) => CostSource::Computed,
        None => CostSource::Unknown,
    };

    let provider = Provider::Anthropic;
    let dedup_key = claude_dedup_key(&value, claude_message).unwrap_or_else(|| {
        // Lines without ids (generic format) fall back to the content hash.
        generate_log_uuid(timestamp, &model, input_tokens, output_tokens).to_string()
    });
    let uuid = Uuid::new_v5(
        &LOG_UUID_NAMESPACE,
        format!("{provider}:{dedup_key}").as_bytes(),
    );

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
        provider,
        subscription: subscription_for(DataSource::Log, provider),
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
        cost_usd,
        cost_source,
        dedup_key,
        session_id,
        project,
        source: DataSource::Log,
    }))
}

/// `"{message.id}:{requestId}"`, the AC-17 key. `None` when either id is
/// missing, so the caller falls back to the content hash.
fn claude_dedup_key(
    value: &serde_json::Value,
    message: Option<&serde_json::Value>,
) -> Option<String> {
    let message_id = message?.get("id")?.as_str()?;
    let request_id = value.get("requestId")?.as_str()?;
    Some(format!("{message_id}:{request_id}"))
}

/// Generate a deterministic UUID from log line data for deduplication.
/// Uses UUID v5 (SHA-1 based) with a fixed namespace for stability across Rust versions.
fn generate_log_uuid(
    timestamp: DateTime<Utc>,
    model: &str,
    input_tokens: u64,
    output_tokens: u64,
) -> Uuid {
    let name = format!(
        "{}:{}:{}:{}",
        timestamp.timestamp_millis(),
        model,
        input_tokens,
        output_tokens,
    );
    Uuid::new_v5(&LOG_UUID_NAMESPACE, name.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use claude_common::Subscription;

    /// Embedded default prices: tests must never read the real home config.
    fn prices() -> PriceBook {
        PriceBook::embedded().expect("embedded default prices must parse")
    }

    #[test]
    fn unknown_model_kept_without_price() {
        let line = r#"{"timestamp":"2026-03-05T10:30:00Z","model":"mystery-model-9","usage":{"input_tokens":1234,"output_tokens":567,"cache_read_tokens":89,"cache_write_tokens":12}}"#;
        let record = parse_log_line(line, &prices()).unwrap().unwrap();

        assert_eq!(record.model, "mystery-model-9");
        assert_eq!(record.input_tokens, 1234);
        assert_eq!(record.output_tokens, 567);
        assert_eq!(record.cache_read_tokens, 89);
        assert_eq!(record.cache_write_tokens, 12);
        assert_eq!(record.cost_usd, None);
        assert_eq!(record.cost_source, CostSource::Unknown);
        assert_eq!(record.provider, Provider::Anthropic);
        assert_eq!(record.subscription, Subscription::Claude);
        assert!(!record.dedup_key.is_empty());
    }

    #[test]
    fn test_parse_log_line_valid() {
        let line = r#"{"timestamp":"2026-03-05T10:30:00Z","model":"claude-sonnet-5-5","usage":{"input_tokens":1234,"output_tokens":567}}"#;
        let record = parse_log_line(line, &prices()).unwrap().unwrap();
        assert_eq!(record.model, "claude-sonnet-5-5");
        assert_eq!(record.input_tokens, 1234);
        assert_eq!(record.output_tokens, 567);
        assert_eq!(record.source, DataSource::Log);
        assert!(record.cost_usd.expect("priced model") > 0.0);
    }

    #[test]
    fn test_parse_log_line_with_cache() {
        let line = r#"{"timestamp":"2026-03-05T10:30:00Z","model":"claude-opus-5-5","usage":{"input_tokens":1000,"output_tokens":500,"cache_read_tokens":200,"cache_write_tokens":100}}"#;
        let record = parse_log_line(line, &prices()).unwrap().unwrap();
        assert_eq!(record.model, "claude-opus-5-5");
        assert_eq!(record.cache_read_tokens, 200);
        assert_eq!(record.cache_write_tokens, 100);
    }

    #[test]
    fn test_parse_claude_code_format() {
        let line = r#"{"type":"assistant","timestamp":"2026-03-05T10:30:00Z","sessionId":"abc-123","message":{"model":"claude-opus-5-5","type":"message","role":"assistant","content":[],"usage":{"input_tokens":50,"output_tokens":200,"cache_read_input_tokens":10000,"cache_creation_input_tokens":5000}}}"#;
        let record = parse_log_line(line, &prices()).unwrap().unwrap();
        assert_eq!(record.model, "claude-opus-5-5");
        assert_eq!(record.input_tokens, 50);
        assert_eq!(record.output_tokens, 200);
        assert_eq!(record.cache_read_tokens, 10000);
        assert_eq!(record.cache_write_tokens, 5000);
        assert_eq!(record.session_id.as_deref(), Some("abc-123"));
        assert!(record.cost_usd.expect("priced model") > 0.0);
    }

    #[test]
    fn test_parse_claude_code_non_assistant_skipped() {
        // "user" type messages don't have usage data
        let line = r#"{"type":"user","timestamp":"2026-03-05T10:30:00Z","sessionId":"abc-123","message":{"role":"user","content":"hello"}}"#;
        let result = parse_log_line(line, &prices()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_log_line_zero_tokens_rejected() {
        let line = r#"{"timestamp":"2026-03-05T10:30:00Z","model":"claude-sonnet-5-5","usage":{"input_tokens":0,"output_tokens":0}}"#;
        let result = parse_log_line(line, &prices()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_log_line_missing_usage() {
        let line = r#"{"timestamp":"2026-03-05T10:30:00Z","model":"claude-sonnet-5-5"}"#;
        let result = parse_log_line(line, &prices()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_log_line_empty() {
        let result = parse_log_line("", &prices()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_log_line_invalid_json() {
        let result = parse_log_line("not json", &prices());
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_log_line_unknown_fields_ignored() {
        let line = r#"{"timestamp":"2026-03-05T10:30:00Z","model":"claude-sonnet-5-5","usage":{"input_tokens":100,"output_tokens":50},"extra_field":"ignored"}"#;
        let record = parse_log_line(line, &prices()).unwrap().unwrap();
        assert_eq!(record.input_tokens, 100);
    }

    #[test]
    fn test_generate_log_uuid_deterministic() {
        let ts = DateTime::parse_from_rfc3339("2026-03-05T10:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let uuid1 = generate_log_uuid(ts, "claude-sonnet-5-5", 1234, 567);
        let uuid2 = generate_log_uuid(ts, "claude-sonnet-5-5", 1234, 567);
        assert_eq!(uuid1, uuid2, "same inputs must produce same UUID");
    }

    #[test]
    fn test_generate_log_uuid_different_inputs() {
        let ts = DateTime::parse_from_rfc3339("2026-03-05T10:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let uuid1 = generate_log_uuid(ts, "claude-sonnet-5-5", 1234, 567);
        let uuid2 = generate_log_uuid(ts, "claude-sonnet-5-5", 1234, 568);
        assert_ne!(
            uuid1, uuid2,
            "different inputs must produce different UUIDs"
        );
    }

    #[test]
    fn test_generate_log_uuid_different_models() {
        let ts = DateTime::parse_from_rfc3339("2026-03-05T10:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let uuid1 = generate_log_uuid(ts, "claude-sonnet-5-5", 1234, 567);
        let uuid2 = generate_log_uuid(ts, "claude-opus-5-5", 1234, 567);
        assert_ne!(uuid1, uuid2);
    }

    /// The fixture tree, copied into a temp dir so the test never mutates the
    /// checked-in files and never reads `$HOME`.
    fn fixture_root() -> std::path::PathBuf {
        let src =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude/projects");
        let dst = std::env::temp_dir().join(format!("claude-tui-claude-{}", Uuid::new_v4()));
        copy_dir(&src, &dst);
        dst
    }

    fn copy_dir(src: &std::path::Path, dst: &std::path::Path) {
        std::fs::create_dir_all(dst).unwrap();
        for entry in std::fs::read_dir(src).unwrap() {
            let entry = entry.unwrap();
            let target = dst.join(entry.file_name());
            if entry.path().is_dir() {
                copy_dir(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), target).unwrap();
            }
        }
    }

    #[test]
    fn claude_dedup_by_message_id_end_to_end() {
        let root = fixture_root();
        let mut collector = ClaudeLogCollector::new(vec![root], prices());
        let mut storage = crate::storage::Storage::new(":memory:").unwrap();

        super::super::run_once(&mut collector, &mut storage).unwrap();

        // 3 lines for msg_01, 1 for msg_02, 1 from the subagent log: 3 records.
        let ids = storage.dedup_keys_for_test();
        assert_eq!(ids.len(), 3, "keys: {ids:?}");
        assert!(ids.iter().any(|k| k == "msg_01:req_01"));
        assert!(ids.iter().any(|k| k == "msg_sub:req_sub"));

        // A second pass reads nothing new: the stored cursors skip the files.
        let again = super::super::run_once(&mut collector, &mut storage).unwrap();
        assert_eq!(again, 0);
    }

    #[test]
    fn claude_dedup_by_message_id() {
        let line = r#"{"type":"assistant","timestamp":"2026-03-05T10:30:00Z","requestId":"req_1","sessionId":"s1","message":{"id":"msg_1","model":"claude-opus-5-5","usage":{"input_tokens":50,"output_tokens":200}}}"#;
        let p = prices();
        let a = parse_log_line(line, &p).unwrap().unwrap();
        let b = parse_log_line(line, &p).unwrap().unwrap();
        let c = parse_log_line(line, &p).unwrap().unwrap();
        assert_eq!(a.dedup_key, "msg_1:req_1");
        assert_eq!(a.dedup_key, b.dedup_key);
        assert_eq!(a.uuid, c.uuid);
    }

    #[test]
    fn test_normalization_cost_truncation() {
        let line = r#"{"timestamp":"2026-03-05T10:30:00Z","model":"claude-sonnet-5-5","usage":{"input_tokens":1,"output_tokens":1}}"#;
        let record = parse_log_line(line, &prices()).unwrap().unwrap();
        // Verify cost is truncated to 6 decimal places
        let cost_str = format!("{:.6}", record.cost_usd.expect("priced model"));
        let decimal_part = cost_str.split('.').nth(1).unwrap();
        assert!(decimal_part.len() <= 6);
    }
}
