//! One line of usage for sketchybar (AC-10).
//!
//! The formatting is a pure function of a [`StatusResponse`] and the current
//! time, so the daemon-down path and every marker can be tested without a
//! socket.

use chrono::{DateTime, Local, Utc};
use claude_common::{
    CostSource, QuotaWindow, StatusResponse, Subscription, SubscriptionStatus, TokenTotals,
};

use crate::windows::labelled_windows;

/// Printed when the daemon cannot be reached. The command still exits 0.
pub const DAEMON_DOWN_LINE: &str = "usage: daemon down";

/// Printed when the daemon answers but has collected nothing yet.
pub const NO_DATA_LINE: &str = "usage: no data";

/// A limit hit older than this is no longer shown.
const LIMIT_HIT_MAX_AGE_HOURS: i64 = 24;

/// Format the sketchybar line for a status response.
///
/// One segment per subscription, separated by ` · `. Claude and ChatGPT show
/// their quota windows; opencode-go exposes no percentage, so it shows today's
/// cost and its last limit hit instead. A subscription with nothing to say
/// contributes nothing, and a line with no data at all is empty.
pub fn format_line(status: &StatusResponse, now: DateTime<Utc>) -> String {
    let mut segments = Vec::new();
    for subscription in &status.subscriptions {
        let tag = match subscription.subscription {
            Subscription::Claude => "C",
            Subscription::ChatGpt => "X",
            Subscription::OpencodeGo => "G",
            Subscription::Other => continue,
        };
        let parts = segment_parts(tag, subscription, now);
        if parts.len() > 1 {
            segments.push(parts.join(" "));
        }
    }
    segments.join(" · ")
}

/// The line the `status --line` subcommand prints.
///
/// Falls back to [`DAEMON_DOWN_LINE`] and never fails.
pub async fn line(socket: &std::path::Path, now: DateTime<Utc>) -> String {
    match crate::fetch_status(socket).await {
        Some(status) => {
            let formatted = format_line(&status, now);
            if formatted.is_empty() {
                NO_DATA_LINE.to_string()
            } else {
                formatted
            }
        }
        None => DAEMON_DOWN_LINE.to_string(),
    }
}

fn segment_parts(tag: &str, subscription: &SubscriptionStatus, now: DateTime<Utc>) -> Vec<String> {
    let mut parts = vec![tag.to_string()];

    for (label, window) in labelled_windows(&subscription.quota) {
        parts.push(format_window(label, window));
    }

    // Claude and ChatGPT report quota percentages, which are the primary
    // signal on a flat-rate plan. opencode-go reports no percentage, so the
    // line shows its cost instead (AC-10's example line).
    if subscription.subscription == Subscription::OpencodeGo
        && let Some(cost) = cost_text(&subscription.today, all_sources_stale(subscription))
    {
        parts.push(cost);
    }

    if let Some(at) = recent_limit_hit(subscription, now) {
        let at = at.with_timezone(&Local).format("%H:%M");
        parts.push(format!("⚠limit {at}"));
    }

    parts
}

fn format_window(label: &str, window: &QuotaWindow) -> String {
    let pct = match window.used_pct {
        Some(pct) => format!("{}%", pct.round() as i64),
        None => "--".to_string(),
    };
    let stale = if window.stale { "?" } else { "" };
    format!("{label} {pct}{stale}")
}

/// `$3.20`, `≈$3.20` for a computed cost, or nothing when the cost is unknown.
fn cost_text(totals: &TokenTotals, stale: bool) -> Option<String> {
    let cost = totals.cost_usd?;
    let approximate = if matches!(totals.cost_source, CostSource::Computed) {
        "≈"
    } else {
        ""
    };
    let stale = if stale { "?" } else { "" };
    Some(format!("{approximate}${cost:.2}{stale}"))
}

/// A subscription is only as fresh as its sources: all of them stale means the
/// cost is stale too.
fn all_sources_stale(subscription: &SubscriptionStatus) -> bool {
    !subscription.sources.is_empty() && subscription.sources.iter().all(|source| source.stale)
}

/// The last limit hit, when it was observed within the last 24 hours.
/// Returns the time to show: the reset time when known, else when it was hit.
fn recent_limit_hit(
    subscription: &SubscriptionStatus,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    let hit = subscription.last_quota_hit.as_ref()?;
    let age = now.signed_duration_since(hit.observed_at);
    if age >= chrono::Duration::hours(LIMIT_HIT_MAX_AGE_HOURS) {
        return None;
    }
    Some(hit.reset_at.unwrap_or(hit.observed_at))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use claude_common::{DataSource, QuotaHitStatus, SourceStatus};

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 20, 12, 0, 0)
            .single()
            .expect("valid test timestamp")
    }

    fn window(name: &str, used_pct: f64, stale: bool) -> QuotaWindow {
        QuotaWindow {
            window: name.to_string(),
            used_pct: Some(used_pct),
            resets_at: Some(now() + chrono::Duration::hours(3)),
            source: DataSource::ClaudeQuota,
            observed_at: now(),
            stale,
            expired: false,
        }
    }

    fn totals(cost: Option<f64>, cost_source: CostSource) -> TokenTotals {
        TokenTotals {
            input: 100,
            output: 50,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
            cost_usd: cost,
            cost_source,
        }
    }

    fn fresh_source() -> SourceStatus {
        SourceStatus {
            collector_id: "claude-logs".to_string(),
            last_update: Some(now()),
            stale: false,
        }
    }

    fn subscription(
        subscription: Subscription,
        quota: Vec<QuotaWindow>,
        today: TokenTotals,
    ) -> SubscriptionStatus {
        SubscriptionStatus {
            subscription,
            quota,
            today,
            last_7d: totals(Some(12.4), CostSource::Reported),
            by_tool: Vec::new(),
            last_quota_hit: None,
            sources: vec![fresh_source()],
        }
    }

    fn status_response(subscriptions: Vec<SubscriptionStatus>) -> StatusResponse {
        StatusResponse {
            daemon_uptime_secs: 60,
            active_sessions: 1,
            current_model: None,
            cost_today_usd: 3.20,
            budget_pct: None,
            collector_status: claude_common::CollectorStatus::Log,
            subscriptions,
        }
    }

    /// The AC-10 example line: all three subscriptions, an opencode-go cost and
    /// a fresh limit hit.
    fn example_response() -> StatusResponse {
        let mut go = subscription(
            Subscription::OpencodeGo,
            Vec::new(),
            totals(Some(3.20), CostSource::Reported),
        );
        go.last_quota_hit = Some(QuotaHitStatus {
            period: Some("5h".to_string()),
            reset_at: Some(
                Utc.with_ymd_and_hms(2026, 10, 20, 14, 5, 0)
                    .single()
                    .expect("valid test timestamp"),
            ),
            observed_at: now(),
        });

        status_response(vec![
            subscription(
                Subscription::Claude,
                vec![
                    window("five_hour", 34.4, false),
                    window("seven_day", 12.0, false),
                ],
                totals(Some(9.10), CostSource::Computed),
            ),
            subscription(
                Subscription::ChatGpt,
                vec![
                    window("primary", 3.0, false),
                    window("secondary", 8.0, false),
                ],
                totals(Some(1.10), CostSource::Computed),
            ),
            go,
        ])
    }

    #[test]
    fn status_line_matches_the_ac_example() {
        let hit_at = Utc
            .with_ymd_and_hms(2026, 10, 20, 14, 5, 0)
            .single()
            .expect("valid test timestamp")
            .with_timezone(&Local)
            .format("%H:%M");
        let expected = format!("C 5h 34% 7d 12% · X 5h 3% 7d 8% · G $3.20 ⚠limit {hit_at}");
        assert_eq!(format_line(&example_response(), now()), expected);
    }

    #[test]
    fn status_line_marks_stale_windows() {
        let claude = subscription(
            Subscription::Claude,
            vec![window("five_hour", 34.0, true)],
            totals(Some(3.20), CostSource::Reported),
        );

        assert_eq!(
            format_line(&status_response(vec![claude]), now()),
            "C 5h 34%?"
        );
    }

    #[test]
    fn status_line_marks_stale_cost() {
        let mut go = subscription(
            Subscription::OpencodeGo,
            Vec::new(),
            totals(Some(3.20), CostSource::Reported),
        );
        go.sources = vec![SourceStatus {
            collector_id: "opencode-db".to_string(),
            last_update: Some(now() - chrono::Duration::days(3)),
            stale: true,
        }];

        assert_eq!(format_line(&status_response(vec![go]), now()), "G $3.20?");
    }

    #[test]
    fn status_line_marks_computed_cost_as_approximate() {
        let go = subscription(
            Subscription::OpencodeGo,
            Vec::new(),
            totals(Some(3.20), CostSource::Computed),
        );

        assert_eq!(format_line(&status_response(vec![go]), now()), "G ≈$3.20");
    }

    #[test]
    fn status_line_hides_limit_hits_older_than_a_day() {
        let mut go = subscription(
            Subscription::OpencodeGo,
            Vec::new(),
            totals(Some(3.20), CostSource::Reported),
        );
        go.last_quota_hit = Some(QuotaHitStatus {
            period: Some("weekly".to_string()),
            reset_at: None,
            observed_at: now() - chrono::Duration::hours(30),
        });

        assert_eq!(format_line(&status_response(vec![go]), now()), "G $3.20");
    }

    #[test]
    fn status_line_omits_subscriptions_without_data() {
        let empty = subscription(Subscription::Claude, Vec::new(), TokenTotals::empty());
        let status = status_response(vec![
            empty,
            subscription(
                Subscription::OpencodeGo,
                Vec::new(),
                totals(Some(0.5), CostSource::Reported),
            ),
        ]);

        assert_eq!(format_line(&status, now()), "G $0.50");
        assert_eq!(format_line(&status_response(Vec::new()), now()), "");
    }

    #[test]
    fn status_line_prefers_the_newest_window_per_label() {
        let mut older = window("five_hour", 90.0, true);
        older.observed_at = now() - chrono::Duration::days(2);
        let claude = subscription(
            Subscription::Claude,
            vec![older, window("five_hour", 34.0, false)],
            totals(Some(1.0), CostSource::Reported),
        );

        assert_eq!(
            format_line(&status_response(vec![claude]), now()),
            "C 5h 34%"
        );
    }

    #[tokio::test]
    async fn status_line_daemon_down() {
        let missing = std::env::temp_dir()
            .join("claude-tui-status-line-absent")
            .join("no-daemon.sock");

        assert_eq!(line(&missing, now()).await, DAEMON_DOWN_LINE);
        assert_eq!(line(&missing, now()).await, "usage: daemon down");
    }
}
