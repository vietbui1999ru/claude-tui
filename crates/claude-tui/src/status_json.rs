//! The versioned status document the Pi extensions read (AC-18).
//!
//! `claude-tui status --json` prints exactly one of two shapes:
//!
//! ```json
//! {"v":1,"subscriptions":{"claude":{…},"chatgpt":{…},"opencode-go":{…}},"generated_at":"…"}
//! {"v":1,"daemon":"down"}
//! ```
//!
//! The schema is documented in `docs/status-json.md`; bump [`STATUS_JSON_VERSION`]
//! when a consumer would have to change.

use chrono::{DateTime, Utc};
use claude_common::{StatusResponse, Subscription};
use serde_json::{Map, Value, json};

/// Schema version. Consumers must reject a document with a version they do not
/// know.
pub const STATUS_JSON_VERSION: u32 = 1;

/// The subscriptions a v1 document always carries a key for.
const KNOWN_SUBSCRIPTIONS: [&str; 3] = ["claude", "chatgpt", "opencode-go"];

/// Build the document from the daemon's AC-9 data.
///
/// A subscription the daemon reported nothing for is `null`, never absent, so a
/// consumer can tell "no data" from "field missing".
pub fn status_json(status: &StatusResponse, generated_at: DateTime<Utc>) -> Value {
    let mut subscriptions = Map::new();
    for name in KNOWN_SUBSCRIPTIONS {
        let value = status
            .subscriptions
            .iter()
            .find(|subscription| subscription.subscription.as_str() == name)
            .and_then(|subscription| serde_json::to_value(subscription).ok())
            .unwrap_or(Value::Null);
        subscriptions.insert(name.to_string(), value);
    }

    // `other` has no fixed key: it exists only when the daemon recorded
    // something for it.
    if let Some(other) = status
        .subscriptions
        .iter()
        .find(|subscription| subscription.subscription == Subscription::Other)
        && let Ok(value) = serde_json::to_value(other)
    {
        subscriptions.insert("other".to_string(), value);
    }

    json!({
        "v": STATUS_JSON_VERSION,
        "subscriptions": subscriptions,
        "generated_at": generated_at.to_rfc3339(),
    })
}

/// The document printed when the daemon cannot be reached.
pub fn daemon_down_json() -> Value {
    json!({
        "v": STATUS_JSON_VERSION,
        "daemon": "down",
    })
}

/// The single line `status --json` prints. Never fails.
pub async fn document(socket: &std::path::Path, now: DateTime<Utc>) -> String {
    match crate::fetch_status(socket).await {
        Some(status) => status_json(&status, now),
        None => daemon_down_json(),
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use claude_common::{CostSource, DataSource, QuotaWindow, SubscriptionStatus, TokenTotals};

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 20, 12, 0, 0)
            .single()
            .expect("valid test timestamp")
    }

    fn totals(cost: Option<f64>, cost_source: CostSource) -> TokenTotals {
        TokenTotals {
            input: 1_000,
            output: 500,
            cache_read: 100,
            cache_write: 50,
            reasoning: 0,
            cost_usd: cost,
            cost_source,
        }
    }

    fn subscription(subscription: Subscription) -> SubscriptionStatus {
        SubscriptionStatus {
            subscription,
            quota: vec![QuotaWindow {
                window: "five_hour".to_string(),
                used_pct: Some(34.0),
                resets_at: Some(now() + chrono::Duration::hours(2)),
                source: DataSource::ClaudeQuota,
                observed_at: now(),
                stale: false,
                expired: false,
            }],
            today: totals(Some(3.2), CostSource::Computed),
            last_7d: totals(Some(12.4), CostSource::Reported),
            by_tool: Vec::new(),
            last_quota_hit: None,
            sources: Vec::new(),
        }
    }

    fn status_response() -> StatusResponse {
        StatusResponse {
            daemon_uptime_secs: 60,
            active_sessions: 1,
            current_model: None,
            cost_today_usd: 3.2,
            budget_pct: None,
            collector_status: claude_common::CollectorStatus::Log,
            subscriptions: vec![
                subscription(Subscription::Claude),
                subscription(Subscription::ChatGpt),
                subscription(Subscription::OpencodeGo),
            ],
        }
    }

    /// Round-trip through the printed form, the way a consumer sees it.
    fn printed(status: &StatusResponse) -> Value {
        let text = status_json(status, now()).to_string();
        serde_json::from_str(&text).expect("the document is valid JSON")
    }

    #[test]
    fn status_json_schema_has_version_subscriptions_and_timestamp() {
        let doc = printed(&status_response());

        assert_eq!(doc["v"], json!(1));
        let subscriptions = doc["subscriptions"]
            .as_object()
            .expect("subscriptions is an object");
        for name in KNOWN_SUBSCRIPTIONS {
            assert!(subscriptions.contains_key(name), "missing {name}");
        }
        assert!(!subscriptions.contains_key("other"));

        let claude = &subscriptions["claude"];
        assert_eq!(claude["subscription"], json!("claude"));
        assert_eq!(claude["quota"][0]["window"], json!("five_hour"));
        assert_eq!(claude["quota"][0]["used_pct"], json!(34.0));
        assert_eq!(claude["today"]["cost_source"], json!("computed"));
        assert_eq!(claude["last_7d"]["cost_usd"], json!(12.4));
        assert!(claude["last_quota_hit"].is_null());

        let generated_at = doc["generated_at"]
            .as_str()
            .expect("generated_at is a string");
        let parsed = DateTime::parse_from_rfc3339(generated_at).expect("generated_at is RFC 3339");
        assert_eq!(parsed.with_timezone(&Utc), now());
    }

    #[test]
    fn status_json_schema_nulls_missing_subscriptions() {
        let mut status = status_response();
        status
            .subscriptions
            .retain(|s| s.subscription != Subscription::ChatGpt);

        let doc = printed(&status);

        assert!(doc["subscriptions"]["chatgpt"].is_null());
        assert!(doc["subscriptions"]["claude"].is_object());
    }

    #[test]
    fn status_json_schema_includes_other_only_when_present() {
        let mut status = status_response();
        status.subscriptions.push(subscription(Subscription::Other));

        let doc = printed(&status);

        assert!(doc["subscriptions"]["other"].is_object());
    }

    #[test]
    fn status_json_schema_daemon_down_has_only_version_and_state() {
        let text = daemon_down_json().to_string();
        let doc: Value = serde_json::from_str(&text).expect("the document is valid JSON");

        assert_eq!(doc["v"], json!(1));
        assert_eq!(doc["daemon"], json!("down"));
        assert!(doc.get("subscriptions").is_none());
        assert!(doc.get("generated_at").is_none());
    }

    #[tokio::test]
    async fn status_json_schema_daemon_down_from_the_cli() {
        let missing = std::env::temp_dir()
            .join("claude-tui-status-json-absent")
            .join("no-daemon.sock");
        let text = document(&missing, now()).await;

        assert_eq!(text, r#"{"daemon":"down","v":1}"#);
    }
}
