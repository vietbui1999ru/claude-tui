//! Codex CLI rollout files (`~/.codex/sessions/**/rollout-*.jsonl`).
//!
//! Each file is one session. Usage comes from `token_usage_record` lines when
//! the file has them, de-duplicated by `response_id`. Older files have none,
//! so for those the `event_msg` `token_count` `info.last_token_usage` is used
//! instead, keyed by `file:offset`. Rate-limit windows come from `token_count`
//! `rate_limits`, where a null never overwrites the last known value.

use std::path::PathBuf;

use chrono::{DateTime, TimeZone, Utc};
use claude_common::{
    CollectorError, CostSource, DataSource, PriceBook, Provider, QuotaSnapshot, Subscription,
    UsageRecord, subscription_for,
};
use serde_json::Value;
use tracing::warn;
use uuid::Uuid;

use super::{CollectOutput, Cursor, SourceCollector, read_jsonl_from, walk_files};

/// Stable collector id, also the cursor namespace.
pub const COLLECTOR_ID: &str = "codex-rollouts";

/// Namespace for deterministic record ids. Distinct from the Claude namespace.
const CODEX_UUID_NAMESPACE: Uuid = Uuid::from_bytes([
    0x3f, 0x1c, 0x9a, 0x42, 0x7d, 0x0e, 0x4b, 0x88, 0x91, 0x2a, 0x6c, 0x5e, 0xd0, 0x17, 0x8b, 0x03,
]);

/// Model used when a file never names one.
const UNKNOWN_MODEL: &str = "unknown";

pub struct CodexRolloutCollector {
    roots: Vec<PathBuf>,
    prices: PriceBook,
}

impl CodexRolloutCollector {
    pub fn new(roots: Vec<PathBuf>, prices: PriceBook) -> Self {
        Self { roots, prices }
    }

    fn files(&self) -> Vec<PathBuf> {
        let mut files = Vec::new();
        for root in &self.roots {
            files.extend(walk_files(root, &|p| {
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                name.starts_with("rollout-") && name.ends_with(".jsonl")
            }));
        }
        files
    }
}

impl SourceCollector for CodexRolloutCollector {
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
            parse_rollout_lines(&key, offset, &chunk.lines, &self.prices, &mut out);
            out.cursor.set_u64(key, chunk.next_offset);
        }
        Ok(out)
    }
}

/// Parse the new lines of one rollout file into `out`.
///
/// `file` names the file for `file:offset` keys; `base_offset` is the byte
/// offset of the first line, so keys are stable across passes.
fn parse_rollout_lines(
    file: &str,
    base_offset: u64,
    lines: &[String],
    prices: &PriceBook,
    out: &mut CollectOutput,
) {
    // Usage comes from `token_usage_record` only when the file has any, so a
    // file with both formats does not count the same response twice.
    let has_usage_records = lines
        .iter()
        .any(|l| line_type(l) == Some("token_usage_record"));

    let mut model = UNKNOWN_MODEL.to_string();
    let mut offset = base_offset;
    for line in lines {
        let line_offset = offset;
        offset += line.len() as u64 + 1; // +1 for the newline the reader stripped

        let value: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                warn!("skipping unparseable rollout line in {file}: {e}");
                continue;
            }
        };
        let Some(timestamp) = timestamp_of(&value) else {
            continue;
        };
        let payload = value.get("payload").unwrap_or(&Value::Null);

        match value.get("type").and_then(Value::as_str) {
            Some("turn_context") => {
                if let Some(m) = payload.get("model").and_then(Value::as_str) {
                    model = m.to_string();
                }
            }
            Some("token_usage_record") if has_usage_records => {
                let response_id = payload.get("response_id").and_then(Value::as_str);
                if let (Some(response_id), Some(usage)) = (response_id, payload.get("usage")) {
                    push_usage(
                        out,
                        prices,
                        &model,
                        timestamp,
                        usage,
                        format!("response:{response_id}"),
                    );
                }
            }
            Some("event_msg")
                if payload.get("type").and_then(Value::as_str) == Some("token_count") =>
            {
                let last = payload.get("info").and_then(|i| i.get("last_token_usage"));
                if let (false, Some(usage)) = (has_usage_records, last.filter(|u| !u.is_null())) {
                    push_usage(
                        out,
                        prices,
                        &model,
                        timestamp,
                        usage,
                        format!("{file}:{line_offset}"),
                    );
                }
                if let Some(rate_limits) = payload.get("rate_limits").filter(|r| !r.is_null()) {
                    push_rate_limits(out, rate_limits, timestamp);
                }
            }
            _ => {}
        }
    }
}

fn line_type(line: &str) -> Option<&str> {
    // Cheap pre-check before a full parse: rollout lines carry `"type":"…"`.
    let start = line.find("\"type\":\"")? + 8;
    let rest = &line[start..];
    let end = rest.find('"')?;
    Some(&rest[..end])
}

fn timestamp_of(value: &Value) -> Option<DateTime<Utc>> {
    value
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
}

/// Build one usage record from a token breakdown and add it to `out`.
fn push_usage(
    out: &mut CollectOutput,
    prices: &PriceBook,
    model: &str,
    timestamp: DateTime<Utc>,
    usage: &Value,
    dedup_key: String,
) {
    let field = |name: &str| usage.get(name).and_then(Value::as_u64).unwrap_or(0);
    let input_total = field("input_tokens");
    let cached = field("cached_input_tokens");
    let cache_write = field("cache_write_input_tokens");
    let output = field("output_tokens");
    let reasoning = field("reasoning_output_tokens");

    // `input_tokens` includes the cached part; bill only the uncached remainder.
    let input = input_total.saturating_sub(cached);
    // Reasoning tokens are billed as output.
    let output_total = output + reasoning;

    if input == 0 && output_total == 0 && cached == 0 && cache_write == 0 {
        return;
    }
    // A response id can appear more than once in a file; keep the first.
    if out.records.iter().any(|r| r.dedup_key == dedup_key) {
        return;
    }

    let provider = Provider::OpenAiCodex;
    let cost = prices
        .cost(model, input, output_total, cached, cache_write)
        .map(|c| (c * 1_000_000.0).round() / 1_000_000.0);
    let cost_source = if cost.is_some() {
        CostSource::Computed
    } else {
        CostSource::Unknown
    };

    out.records.push(UsageRecord {
        id: None,
        uuid: Uuid::new_v5(
            &CODEX_UUID_NAMESPACE,
            format!("{provider}:{dedup_key}").as_bytes(),
        ),
        timestamp,
        model: model.to_string(),
        provider,
        subscription: subscription_for(DataSource::CodexRollout, provider),
        input_tokens: input,
        output_tokens: output_total,
        cache_read_tokens: cached,
        cache_write_tokens: cache_write,
        cost_usd: cost,
        cost_source,
        dedup_key,
        session_id: None,
        project: None,
        source: DataSource::CodexRollout,
    });
}

/// Record the primary and secondary windows from a `rate_limits` object.
fn push_rate_limits(out: &mut CollectOutput, rate_limits: &Value, observed_at: DateTime<Utc>) {
    let plan_type = rate_limits
        .get("plan_type")
        .and_then(Value::as_str)
        .map(str::to_string);
    for window in ["primary", "secondary"] {
        let Some(w) = rate_limits.get(window).filter(|w| !w.is_null()) else {
            continue;
        };
        let used_pct = w.get("used_percent").and_then(Value::as_f64);
        let resets_at = w
            .get("resets_at")
            .and_then(Value::as_i64)
            .and_then(|s| Utc.timestamp_opt(s, 0).single());
        out.snapshots.push(QuotaSnapshot {
            id: None,
            subscription: Subscription::ChatGpt,
            window: window.to_string(),
            used_pct,
            resets_at,
            source: DataSource::CodexRollout,
            observed_at,
            plan_type: plan_type.clone(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prices() -> PriceBook {
        PriceBook::embedded().expect("embedded default prices must parse")
    }

    fn fixture_root() -> PathBuf {
        let src =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex/sessions");
        let dst = std::env::temp_dir().join(format!("claude-tui-codex-{}", Uuid::new_v4()));
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

    fn collect_all(root: PathBuf) -> CollectOutput {
        let mut collector = CodexRolloutCollector::new(vec![root], prices());
        collector.collect(&Cursor::new()).unwrap()
    }

    #[test]
    fn codex_rollout_reads_usage_and_dedups_response_id() {
        let out = collect_all(fixture_root());
        let new_format: Vec<_> = out
            .records
            .iter()
            .filter(|r| r.dedup_key.starts_with("response:"))
            .collect();
        // resp_aaa appears twice in the new-format file and counts once; resp_bbb
        // is the second response there; resp_ccc is in the null-limits file.
        assert_eq!(
            new_format.len(),
            3,
            "aaa, bbb (new file) and ccc (null-limits)"
        );
        assert_eq!(
            new_format
                .iter()
                .filter(|r| r.dedup_key == "response:resp_aaa")
                .count(),
            1,
            "duplicate response_id must collapse to one record"
        );
        let aaa = new_format
            .iter()
            .find(|r| r.dedup_key == "response:resp_aaa")
            .expect("resp_aaa stored");
        assert_eq!(aaa.model, "gpt-5.5-codex");
        assert_eq!(aaa.cache_read_tokens, 400);
        assert_eq!(aaa.input_tokens, 600, "uncached input = input - cached");
        assert_eq!(aaa.output_tokens, 250, "output + reasoning");
        assert_eq!(aaa.provider, Provider::OpenAiCodex);
        assert_eq!(aaa.subscription, claude_common::Subscription::ChatGpt);
    }

    #[test]
    fn codex_rollout_old_format_uses_token_count() {
        let out = collect_all(fixture_root());
        let old: Vec<_> = out
            .records
            .iter()
            .filter(|r| r.dedup_key.contains("rollout-old-format.jsonl:"))
            .collect();
        assert_eq!(old.len(), 2, "two token_count events with usage");
        assert!(old.iter().all(|r| r.model == "gpt-5-codex"));
    }

    #[test]
    fn codex_rollout_reads_rate_limits_and_null_never_overwrites() {
        let out = collect_all(fixture_root());
        // new-format file: primary + secondary once (the null event adds nothing).
        let new_primary = out
            .snapshots
            .iter()
            .filter(|s| s.window == "primary" && s.used_pct == Some(12.5))
            .count();
        assert_eq!(new_primary, 1);
        // old-format file: the second event's rate_limits is null, so only the
        // first event's primary (30%) is recorded for that file.
        let old_primary = out
            .snapshots
            .iter()
            .filter(|s| s.window == "primary" && s.used_pct == Some(30.0))
            .count();
        assert_eq!(old_primary, 1);
        let snap = out
            .snapshots
            .iter()
            .find(|s| s.used_pct == Some(12.5))
            .unwrap();
        assert_eq!(snap.plan_type.as_deref(), Some("plus"));
        assert_eq!(snap.resets_at.map(|t| t.timestamp()), Some(1_772_707_200));
    }

    #[test]
    fn codex_rollout_unknown_model_when_no_turn_context() {
        // The null-limits file has a turn_context, so use a file with none.
        let mut out = CollectOutput::default();
        let line = r#"{"timestamp":"2026-03-07T00:00:00Z","type":"token_usage_record","payload":{"response_id":"r1","usage":{"input_tokens":10,"output_tokens":5}}}"#;
        parse_rollout_lines("f", 0, &[line.to_string()], &prices(), &mut out);
        assert_eq!(out.records.len(), 1);
        assert_eq!(out.records[0].model, UNKNOWN_MODEL);
        assert_eq!(out.records[0].cost_source, CostSource::Unknown);
    }

    #[test]
    fn codex_rollout_cursor_resumes_without_rescanning() {
        let root = fixture_root();
        let mut collector = CodexRolloutCollector::new(vec![root], prices());
        let first = collector.collect(&Cursor::new()).unwrap();
        assert!(!first.records.is_empty());
        let second = collector.collect(&first.cursor).unwrap();
        assert!(second.records.is_empty(), "nothing new on the second pass");
    }
}
