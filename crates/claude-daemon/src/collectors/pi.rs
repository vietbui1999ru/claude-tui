//! Pi session files (`~/.pi/agent/sessions/<cwd-slug>/*.jsonl`).
//!
//! Each assistant message carries its usage and, when Pi reports one, the
//! provider cost (`cost.total`, recorded as reported). Assistant
//! `errorMessage` values that name a quota limit are recorded as quota hits.
//! Provider throttling (`rate_limit_error`) is not a quota hit.

use std::path::PathBuf;

use chrono::{DateTime, TimeZone, Utc};
use claude_common::{
    CollectorError, CostSource, DataSource, PriceBook, Provider, QuotaHit, Subscription,
    UsageRecord, subscription_for,
};
use serde_json::Value;
use tracing::warn;
use uuid::Uuid;

use super::reset_text::parse_reset_duration;
use super::{CollectOutput, Cursor, SourceCollector, read_jsonl_from, walk_files};

/// Stable collector id, also the cursor namespace.
pub const COLLECTOR_ID: &str = "pi-sessions";

/// Namespace for deterministic record ids. Distinct from the other collectors.
const PI_UUID_NAMESPACE: Uuid = Uuid::from_bytes([
    0x91, 0x0b, 0x44, 0xc2, 0x5e, 0x7a, 0x4d, 0x10, 0xa3, 0x6f, 0x28, 0xb9, 0x5c, 0x01, 0xe7, 0x42,
]);

pub struct PiSessionCollector {
    roots: Vec<PathBuf>,
    prices: PriceBook,
}

impl PiSessionCollector {
    pub fn new(roots: Vec<PathBuf>, prices: PriceBook) -> Self {
        Self { roots, prices }
    }

    fn files(&self) -> Vec<PathBuf> {
        let mut files = Vec::new();
        for root in &self.roots {
            files.extend(walk_files(root, &|p| {
                p.extension().and_then(|e| e.to_str()) == Some("jsonl")
            }));
        }
        files
    }
}

impl SourceCollector for PiSessionCollector {
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
                match parse_entry(line, &self.prices) {
                    Ok(Some(parsed)) => {
                        if let Some(record) = parsed.record {
                            push_unique_record(&mut out, record);
                        }
                        if let Some(hit) = parsed.hit {
                            out.hits.push(hit);
                        }
                    }
                    Ok(None) => {}
                    Err(e) => warn!("skipping unparseable Pi line in {}: {e}", path.display()),
                }
            }
            out.cursor.set_u64(key, chunk.next_offset);
        }
        Ok(out)
    }
}

/// What one Pi entry produced.
#[derive(Debug, Default)]
struct ParsedEntry {
    record: Option<UsageRecord>,
    hit: Option<QuotaHit>,
}

/// Parse one Pi JSONL entry. Only `message` entries from the assistant carry data.
fn parse_entry(line: &str, prices: &PriceBook) -> Result<Option<ParsedEntry>, CollectorError> {
    let line = line.trim();
    if line.is_empty() {
        return Ok(None);
    }
    let value: Value =
        serde_json::from_str(line).map_err(|e| CollectorError::LogParse(e.to_string()))?;
    if value.get("type").and_then(Value::as_str) != Some("message") {
        return Ok(None);
    }
    let Some(message) = value.get("message") else {
        return Ok(None);
    };
    if message.get("role").and_then(Value::as_str) != Some("assistant") {
        return Ok(None);
    }
    let Some(entry_id) = value.get("id").and_then(Value::as_str) else {
        return Ok(None);
    };
    let Some(timestamp) = message_time(&value, message) else {
        return Ok(None);
    };
    let provider_name = message
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or("");
    let provider: Provider = provider_name.parse().unwrap_or(Provider::Other);
    let model = message
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();

    let hit = message
        .get("errorMessage")
        .and_then(Value::as_str)
        .and_then(|text| quota_hit(text, entry_id, provider, timestamp));

    let record = usage_record(entry_id, message, provider, &model, timestamp, prices);

    if record.is_none() && hit.is_none() {
        return Ok(None);
    }
    Ok(Some(ParsedEntry { record, hit }))
}

/// The entry's time: `message.timestamp` (epoch ms), else the entry's RFC 3339 stamp.
fn message_time(entry: &Value, message: &Value) -> Option<DateTime<Utc>> {
    if let Some(ms) = message.get("timestamp").and_then(Value::as_i64) {
        return Utc.timestamp_millis_opt(ms).single();
    }
    entry
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
}

/// A usage record for one assistant message, or `None` when it has no tokens.
fn usage_record(
    entry_id: &str,
    message: &Value,
    provider: Provider,
    model: &str,
    timestamp: DateTime<Utc>,
    prices: &PriceBook,
) -> Option<UsageRecord> {
    let usage = message.get("usage")?;
    let field = |name: &str| usage.get(name).and_then(Value::as_u64).unwrap_or(0);
    let input = field("input");
    // Reasoning is absent on most entries; absent counts as 0.
    let output = field("output") + field("reasoning");
    let cache_read = field("cacheRead");
    let cache_write = field("cacheWrite");
    if input == 0 && output == 0 && cache_read == 0 && cache_write == 0 {
        return None;
    }

    let reported = usage
        .get("cost")
        .and_then(|c| c.get("total"))
        .and_then(Value::as_f64);
    let (cost_usd, cost_source) = match reported {
        Some(total) => (Some(total), CostSource::Reported),
        None => match prices.cost(model, input, output, cache_read, cache_write) {
            Some(c) => (Some(c), CostSource::Computed),
            None => (None, CostSource::Unknown),
        },
    };

    Some(UsageRecord {
        id: None,
        uuid: Uuid::new_v5(
            &PI_UUID_NAMESPACE,
            format!("{provider}:{entry_id}").as_bytes(),
        ),
        timestamp,
        model: model.to_string(),
        provider,
        subscription: subscription_for(DataSource::PiSession, provider),
        input_tokens: input,
        output_tokens: output,
        cache_read_tokens: cache_read,
        cache_write_tokens: cache_write,
        cost_usd,
        cost_source,
        dedup_key: entry_id.to_string(),
        session_id: None,
        project: None,
        source: DataSource::PiSession,
    })
}

/// Build a quota hit when an assistant `errorMessage` names a limit we track.
fn quota_hit(
    text: &str,
    entry_id: &str,
    provider: Provider,
    observed_at: DateTime<Utc>,
) -> Option<QuotaHit> {
    let (period, raw_kind, reset_text) = classify_limit(text)?;
    let reset_at = reset_text
        .as_deref()
        .and_then(parse_reset_duration)
        .map(|d| observed_at + d);
    let subscription = subscription_for(DataSource::PiSession, provider);
    // Only the subscriptions a limit can name are kept: a Go or ChatGPT plan.
    if subscription == Subscription::Claude || subscription == Subscription::Other {
        return None;
    }
    Some(QuotaHit {
        id: None,
        subscription,
        period,
        reset_at,
        observed_at,
        raw_kind: raw_kind.to_string(),
        dedup_key: format!("pi:{entry_id}"),
    })
}

/// Classify a limit error. Returns (period, raw kind, reset text) for the
/// variants listed in the spec, and `None` for everything else, including
/// `rate_limit_error`.
fn classify_limit(text: &str) -> Option<(Option<String>, &'static str, Option<String>)> {
    // Provider throttling is not a quota limit.
    if text.contains("rate_limit_error") {
        return None;
    }

    // `429 <period> usage limit reached. Resets in …`
    let lower = text.to_ascii_lowercase();
    for period in ["5-hour", "weekly", "monthly"] {
        let marker = format!("{period} usage limit reached");
        if let Some(at) = lower.find(&marker) {
            let after = &text[at + marker.len()..];
            let reset = after
                .find("Resets in")
                .map(|i| after[i + "Resets in".len()..].trim())
                .map(|r| r.trim_end_matches('.').trim().to_string());
            return Some((Some(normalise_period(period)), "go_usage_limit", reset));
        }
    }

    // `429: {"type":"GoUsageLimitError", …}` and `Go usage limit exceeded`: no period.
    if text.contains("GoUsageLimitError") || text.contains("Go usage limit exceeded") {
        return Some((None, "go_usage_limit", None));
    }

    // `openai-codex`: `The usage limit has been reached`, and
    // `You have hit your ChatGPT usage limit (…). Try again in ~N min.`
    if text.contains("The usage limit has been reached") {
        return Some((None, "chatgpt_usage_limit", None));
    }
    if let Some(at) = text.find("hit your ChatGPT usage limit") {
        let reset = text[at..]
            .find("Try again in")
            .map(|i| &text[at + i + "Try again in".len()..])
            .map(|r| {
                r.trim()
                    .trim_start_matches('~')
                    .trim_end_matches('.')
                    .trim()
                    .to_string()
            });
        return Some((None, "chatgpt_usage_limit", reset));
    }

    None
}

fn normalise_period(raw: &str) -> String {
    match raw.to_ascii_lowercase().as_str() {
        "5-hour" => "5h".to_string(),
        other => other.to_string(),
    }
}

/// Insert a record unless this collector already produced its dedup key in
/// this pass. Storage ignores repeats across passes.
fn push_unique_record(out: &mut CollectOutput, record: UsageRecord) {
    if out
        .records
        .iter()
        .any(|r| r.provider == record.provider && r.dedup_key == record.dedup_key)
    {
        return;
    }
    out.records.push(record);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prices() -> PriceBook {
        PriceBook::embedded().expect("embedded default prices must parse")
    }

    fn fixture_root() -> PathBuf {
        let src =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pi/sessions");
        let dst = std::env::temp_dir().join(format!("claude-tui-pi-{}", Uuid::new_v4()));
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

    fn collect_all() -> CollectOutput {
        let mut collector = PiSessionCollector::new(vec![fixture_root()], prices());
        collector.collect(&Cursor::new()).unwrap()
    }

    #[test]
    fn pi_session_records_assistant_usage_with_reported_cost() {
        let out = collect_all();
        let go1 = out
            .records
            .iter()
            .find(|r| r.dedup_key == "m-go-1")
            .expect("m-go-1 stored");
        assert_eq!(go1.provider, Provider::OpencodeGo);
        assert_eq!(go1.subscription, Subscription::OpencodeGo);
        assert_eq!(go1.input_tokens, 1200);
        assert_eq!(go1.output_tokens, 340, "output + reasoning");
        assert_eq!(go1.cache_read_tokens, 5000);
        assert_eq!(go1.cost_source, CostSource::Reported);
        assert_eq!(go1.cost_usd, Some(0.0123));
    }

    #[test]
    fn pi_session_dedups_by_entry_id_and_skips_user_messages() {
        let out = collect_all();
        let keys: Vec<_> = out.records.iter().map(|r| r.dedup_key.clone()).collect();
        assert!(!keys.iter().any(|k| k == "m-user-1"));
        let go_ids = keys.iter().filter(|k| *k == "m-go-1").count();
        assert_eq!(go_ids, 1);
    }

    #[test]
    fn pi_session_codex_and_anthropic_grouping() {
        let out = collect_all();
        let cx = out
            .records
            .iter()
            .find(|r| r.dedup_key == "m-cx-1")
            .unwrap();
        assert_eq!(cx.subscription, Subscription::ChatGpt);
        let anth = out
            .records
            .iter()
            .find(|r| r.dedup_key == "m-anth-1")
            .unwrap();
        assert_eq!(anth.subscription, Subscription::Other);
    }

    #[test]
    fn pi_session_quota_hit_variants_recorded() {
        let out = collect_all();
        let kinds: Vec<_> = out.hits.iter().map(|h| h.dedup_key.clone()).collect();
        for id in [
            "pi:m-go-limit-5h",
            "pi:m-go-limit-generic",
            "pi:m-codex-limit",
            "pi:m-codex-limit-generic",
        ] {
            assert!(kinds.iter().any(|k| k == id), "missing {id}: {kinds:?}");
        }
    }

    #[test]
    fn pi_session_rate_limit_error_is_not_a_hit() {
        let out = collect_all();
        assert!(!out.hits.iter().any(|h| h.dedup_key == "pi:m-throttle"));
    }

    #[test]
    fn pi_session_period_and_reset_from_text() {
        let out = collect_all();
        let five = out
            .hits
            .iter()
            .find(|h| h.dedup_key == "pi:m-go-limit-5h")
            .unwrap();
        assert_eq!(five.period.as_deref(), Some("5h"));
        // 10:03:00 + 2h15m = 12:18:00 UTC on the fixture day.
        let observed = five.observed_at;
        assert_eq!(
            five.reset_at,
            Some(observed + chrono::Duration::minutes(135))
        );

        let codex = out
            .hits
            .iter()
            .find(|h| h.dedup_key == "pi:m-codex-limit")
            .unwrap();
        assert_eq!(codex.subscription, Subscription::ChatGpt);
        assert_eq!(
            codex.reset_at,
            Some(codex.observed_at + chrono::Duration::minutes(14))
        );

        let monthly_root = std::env::temp_dir().join(format!("claude-tui-pi-m-{}", Uuid::new_v4()));
        let slug = monthly_root.join("--tmp--");
        std::fs::create_dir_all(&slug).unwrap();
        std::fs::copy(
            fixture_root().join("--tmp-my project--/2026-03-07T08-00-00-000Z_pi-c.jsonl"),
            slug.join("c.jsonl"),
        )
        .unwrap();
        let mut collector = PiSessionCollector::new(vec![monthly_root], prices());
        let monthly = collector.collect(&Cursor::new()).unwrap();
        let hit = &monthly.hits[0];
        assert_eq!(hit.period.as_deref(), Some("monthly"));
        assert_eq!(
            hit.reset_at,
            Some(hit.observed_at + chrono::Duration::minutes(3 * 24 * 60 + 4 * 60))
        );
    }

    #[test]
    fn pi_session_slug_directory_with_spaces_is_read() {
        let out = collect_all();
        assert!(out.records.iter().any(|r| r.dedup_key == "m-cx-1"));
    }

    #[test]
    fn pi_session_cursor_resumes_without_rescanning() {
        let mut collector = PiSessionCollector::new(vec![fixture_root()], prices());
        let first = collector.collect(&Cursor::new()).unwrap();
        assert!(!first.records.is_empty());
        let second = collector.collect(&first.cursor).unwrap();
        assert!(second.records.is_empty());
        assert!(second.hits.is_empty());
    }
}
