//! Provider window names, shared by the status line and the Overview cards.
//!
//! Claude reports `five_hour`/`seven_day`. Codex reports the same two windows
//! as `primary` (5-hour) and `secondary` (weekly). Freshness and expiry are the
//! window's `stale` and `expired` flags, never part of its name.

use claude_common::QuotaWindow;

/// The newest window per label, in `5h` then `7d` order.
///
/// Several rows can map to one label (e.g. a live Claude snapshot and the
/// `~/.claude.json` fallback). The most recent observation wins. Windows this dashboard
/// has no label for are dropped.
pub(crate) fn labelled_windows(quota: &[QuotaWindow]) -> Vec<(&'static str, &QuotaWindow)> {
    let mut best: Vec<(&'static str, &QuotaWindow)> = Vec::new();
    for window in quota {
        let label = if window.window.starts_with("five_hour") || window.window == "primary" {
            "5h"
        } else if window.window.starts_with("seven_day") || window.window == "secondary" {
            "7d"
        } else {
            continue;
        };
        match best.iter_mut().find(|(existing, _)| *existing == label) {
            Some(slot) => {
                if window.observed_at > slot.1.observed_at {
                    slot.1 = window;
                }
            }
            None => best.push((label, window)),
        }
    }
    best.sort_by_key(|(label, _)| if *label == "5h" { 0 } else { 1 });
    best
}
