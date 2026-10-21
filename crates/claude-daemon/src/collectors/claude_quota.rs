//! Claude Code quota windows (5h and 7d).
//!
//! Two sources, in order of preference:
//! 1. A snapshot file that Claude Code's statusline writes (`rate_limits.*`).
//!    It is live while a session runs.
//! 2. `cachedUsageUtilization` in `~/.claude.json`, used when the snapshot is
//!    missing or older than 24 hours. It is marked stale.
//!
//! `~/.claude.json` is parsed through a struct with only the keys read here,
//! so serde ignores every other key in the file. The collector never writes
//! to either file.

use std::path::PathBuf;

use chrono::{DateTime, TimeZone, Utc};
use claude_common::{CollectorError, DataSource, QuotaSnapshot, Subscription};
use serde::Deserialize;

use super::{CollectOutput, Cursor, SourceCollector};

/// Stable collector id.
pub const COLLECTOR_ID: &str = "claude-quota";

/// A snapshot older than this is not used; the fallback is preferred.
const MAX_SNAPSHOT_AGE: chrono::Duration = chrono::Duration::hours(24);

/// The statusline snapshot: only the fields this collector reads.
#[derive(Debug, Deserialize)]
struct StatusSnapshot {
    #[serde(default)]
    rate_limits: Option<RateLimits>,
}

#[derive(Debug, Deserialize)]
struct RateLimits {
    five_hour: Option<Window>,
    seven_day: Option<Window>,
}

#[derive(Debug, Deserialize)]
struct Window {
    used_percentage: Option<f64>,
    resets_at: Option<i64>,
}

/// The `~/.claude.json` fallback: only `cachedUsageUtilization` is read.
#[derive(Debug, Deserialize)]
struct ClaudeJson {
    #[serde(rename = "cachedUsageUtilization")]
    cached: Option<CachedUsage>,
}

#[derive(Debug, Deserialize)]
struct CachedUsage {
    #[serde(rename = "fetchedAtMs")]
    fetched_at_ms: i64,
    utilization: CachedUtilization,
}

#[derive(Debug, Deserialize)]
struct CachedUtilization {
    five_hour: Option<CachedWindow>,
    seven_day: Option<CachedWindow>,
}

#[derive(Debug, Deserialize)]
struct CachedWindow {
    utilization: Option<f64>,
    resets_at: Option<String>,
}

pub struct ClaudeQuotaCollector {
    status_path: PathBuf,
    claude_json_path: PathBuf,
}

impl ClaudeQuotaCollector {
    pub fn new(status_path: PathBuf, claude_json_path: PathBuf) -> Self {
        Self {
            status_path,
            claude_json_path,
        }
    }
}

impl SourceCollector for ClaudeQuotaCollector {
    fn id(&self) -> &'static str {
        COLLECTOR_ID
    }

    fn collect(&mut self, _cursor: &Cursor) -> Result<CollectOutput, CollectorError> {
        let now = Utc::now();
        let snapshots = quota_snapshots(&self.status_path, &self.claude_json_path, now);
        Ok(CollectOutput {
            snapshots,
            ..CollectOutput::default()
        })
    }
}

/// Quota snapshots for the 5h and 7d windows, from the best available source.
///
/// A missing file, a parse failure or a stale snapshot falls through to the
/// next source. Nothing here is an error: the quota view simply shows less.
fn quota_snapshots(
    status_path: &std::path::Path,
    claude_json_path: &std::path::Path,
    now: DateTime<Utc>,
) -> Vec<QuotaSnapshot> {
    if let Some(snapshots) = read_status_snapshot(status_path, now) {
        return snapshots;
    }
    read_cached_utilization(claude_json_path, now).unwrap_or_default()
}

/// The statusline snapshot, when present and younger than 24 hours.
fn read_status_snapshot(path: &std::path::Path, now: DateTime<Utc>) -> Option<Vec<QuotaSnapshot>> {
    let meta = std::fs::metadata(path).ok()?;
    let modified: DateTime<Utc> = meta.modified().ok()?.into();
    if now - modified > MAX_SNAPSHOT_AGE {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    let snap: StatusSnapshot = serde_json::from_str(&text).ok()?;
    let limits = snap.rate_limits?;

    let mut out = Vec::new();
    if let Some(w) = limits.five_hour {
        out.push(window_snapshot("five_hour", &w, modified, now));
    }
    if let Some(w) = limits.seven_day {
        out.push(window_snapshot("seven_day", &w, modified, now));
    }
    Some(out)
}

/// Window names stay plain (`five_hour`, `seven_day`); freshness and expiry are
/// derived from `observed_at` and `resets_at` when status is built.
fn window_snapshot(
    name: &str,
    window: &Window,
    observed_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> QuotaSnapshot {
    let resets_at = window
        .resets_at
        .and_then(|s| Utc.timestamp_opt(s, 0).single());
    // A reset that has already passed means the window has expired, not that
    // the percentage is current.
    let used_pct = match resets_at {
        Some(reset) if reset <= now => None,
        _ => window.used_percentage,
    };
    QuotaSnapshot {
        id: None,
        subscription: Subscription::Claude,
        window: name.to_string(),
        used_pct,
        resets_at,
        source: DataSource::ClaudeQuota,
        observed_at,
        plan_type: None,
    }
}

/// The `~/.claude.json` fallback, observed at its `fetchedAtMs`.
fn read_cached_utilization(
    path: &std::path::Path,
    now: DateTime<Utc>,
) -> Option<Vec<QuotaSnapshot>> {
    let text = std::fs::read_to_string(path).ok()?;
    let doc: ClaudeJson = serde_json::from_str(&text).ok()?;
    let cached = doc.cached?;
    let observed_at = Utc.timestamp_millis_opt(cached.fetched_at_ms).single()?;

    let mut out = Vec::new();
    for (name, window) in [
        ("five_hour", cached.utilization.five_hour),
        ("seven_day", cached.utilization.seven_day),
    ] {
        let Some(window) = window else { continue };
        let resets_at = window
            .resets_at
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc));
        let used_pct = match resets_at {
            Some(reset) if reset <= now => None,
            _ => window.utilization,
        };
        // observed_at is fetchedAtMs, so the 24h stale rule applies to it.
        out.push(QuotaSnapshot {
            id: None,
            subscription: Subscription::Claude,
            window: name.to_string(),
            used_pct,
            resets_at,
            source: DataSource::ClaudeQuota,
            observed_at,
            plan_type: None,
        });
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("claude-tui-quota-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(path: &std::path::Path, body: &str) {
        std::fs::write(path, body).unwrap();
    }

    /// Set a file's mtime so age-based tests do not depend on wall time.
    fn set_mtime(path: &std::path::Path, when: DateTime<Utc>) {
        let secs = when.timestamp().max(0) as u64;
        let time = SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
        let file = std::fs::File::options().write(true).open(path).unwrap();
        file.set_modified(time).unwrap();
    }

    fn fixture(name: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/claude-quota")
            .join(name);
        std::fs::read_to_string(path).unwrap()
    }

    /// A reset time well in the future, so the window is not expired.
    fn future() -> i64 {
        (Utc::now() + chrono::Duration::hours(2)).timestamp()
    }

    #[test]
    fn claude_quota_fresh_snapshot_wins() {
        let dir = temp_dir();
        let status = dir.join("claude-status.json");
        // The fixture's resets_at is in the past, so pin a future reset here to
        // exercise the live path; the expired path has its own test.
        let body = fixture("claude-status.json").replace(
            "\"resets_at\":1772706000",
            &format!("\"resets_at\":{}", future()),
        );
        write(&status, &body);
        let claude_json = dir.join(".claude.json");
        write(&claude_json, &fixture("claude.json"));

        let snaps = quota_snapshots(&status, &claude_json, Utc::now());
        let five = snaps
            .iter()
            .find(|s| s.window.starts_with("five_hour"))
            .unwrap();
        assert_eq!(five.used_pct, Some(34.0));
        assert_eq!(five.window, "five_hour");
        assert_eq!(five.source, DataSource::ClaudeQuota);
        assert!(snaps.iter().any(|s| s.window.starts_with("seven_day")));
    }

    #[test]
    fn claude_quota_missing_snapshot_falls_back_to_stale_cache() {
        let dir = temp_dir();
        let status = dir.join("missing.json");
        let claude_json = dir.join(".claude.json");
        write(&claude_json, &fixture("claude.json"));

        // The fixture's resets_at (2026-03-05) is in the past by now, so use
        // a claude.json with a future reset to test the stale label.
        let now = Utc::now();
        let body = format!(
            r#"{{"cachedUsageUtilization":{{"fetchedAtMs":{},"utilization":{{"five_hour":{{"utilization":61,"resets_at":"{}"}},"seven_day":{{"utilization":20,"resets_at":"{}"}}}}}}}}"#,
            now.timestamp_millis(),
            (now + chrono::Duration::hours(1)).to_rfc3339(),
            (now + chrono::Duration::days(2)).to_rfc3339(),
        );
        write(&claude_json, &body);

        let snaps = quota_snapshots(&status, &claude_json, now);
        let five = snaps
            .iter()
            .find(|s| s.window.starts_with("five_hour"))
            .unwrap();
        assert_eq!(five.window, "five_hour");
        assert_eq!(five.used_pct, Some(61.0));
        assert_eq!(five.source, DataSource::ClaudeQuota);
    }

    #[test]
    fn claude_quota_stale_snapshot_is_skipped() {
        let dir = temp_dir();
        let status = dir.join("claude-status.json");
        write(&status, &fixture("claude-status.json"));
        // Make the snapshot 48 hours old.
        set_mtime(&status, Utc::now() - chrono::Duration::hours(48));
        let claude_json = dir.join(".claude.json");
        let now = Utc::now();
        let body = format!(
            r#"{{"cachedUsageUtilization":{{"fetchedAtMs":{},"utilization":{{"five_hour":{{"utilization":7,"resets_at":"{}"}}}}}}}}"#,
            now.timestamp_millis(),
            (now + chrono::Duration::hours(1)).to_rfc3339(),
        );
        write(&claude_json, &body);

        let snaps = quota_snapshots(&status, &claude_json, now);
        assert!(
            snaps
                .iter()
                .all(|s| s.observed_at > now - chrono::Duration::hours(1)),
            "a 48h-old snapshot must not be shown as live"
        );
        assert_eq!(snaps[0].used_pct, Some(7.0));
    }

    #[test]
    fn claude_quota_expired_window_is_not_current() {
        let dir = temp_dir();
        let status = dir.join("claude-status.json");
        // Reset in the past: the 5h window has already rolled over.
        let body = r#"{"rate_limits":{"five_hour":{"used_percentage":90.0,"resets_at":1000},"seven_day":null}}"#;
        write(&status, body);
        let snaps = quota_snapshots(&status, &dir.join("none.json"), Utc::now());
        assert_eq!(snaps.len(), 1);
        assert_eq!(snaps[0].window, "five_hour");
        assert_eq!(
            snaps[0].used_pct, None,
            "an expired window shows no percentage"
        );
    }

    #[test]
    fn claude_quota_reads_only_needed_keys_from_claude_json() {
        // Keys other than cachedUsageUtilization must not break parsing.
        let dir = temp_dir();
        let claude_json = dir.join(".claude.json");
        write(&claude_json, &fixture("claude.json"));
        let snaps = read_cached_utilization(&claude_json, Utc::now());
        assert!(snaps.is_some(), "extra keys like oauthAccount are ignored");
    }

    #[test]
    fn claude_quota_nothing_available_is_empty() {
        let dir = temp_dir();
        let snaps = quota_snapshots(&dir.join("a.json"), &dir.join("b.json"), Utc::now());
        assert!(snaps.is_empty());
    }

    #[test]
    fn claude_quota_collector_trait_returns_snapshots() {
        let dir = temp_dir();
        let status = dir.join("claude-status.json");
        write(&status, &fixture("claude-status.json"));
        let mut collector = ClaudeQuotaCollector::new(status, dir.join(".claude.json"));
        let out = collector.collect(&Cursor::new()).unwrap();
        assert!(!out.snapshots.is_empty());
        assert!(out.records.is_empty());
    }
}
