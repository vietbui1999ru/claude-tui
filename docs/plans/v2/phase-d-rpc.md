# Phase D: per-subscription status RPC (AC-9)

## Current state

- `crates/claude-common/src/protocol.rs`: `RpcMethod::from_request` dispatch, and `StatusResponse {daemon_uptime_secs, active_sessions, current_model, cost_today_usd, budget_pct, collector_status}`.
- `crates/claude-daemon/src/ipc.rs` handles `Status` (it reads `collector_status` before locking storage; keep that order, which is the U1 fix).

## Do

Extend `StatusResponse`. Add fields; keep the existing ones so old clients still parse. The new field is `subscriptions: Vec<SubscriptionStatus>`, one entry each for claude, chatgpt and opencode-go, plus other when it has data:

- `quota: Vec<QuotaWindow {window, used_pct: Option<f64>, resets_at, source, observed_at, stale: bool, expired: bool}>`;
- `today` and `last_7d`: `{input, output, cache_read, cache_write, reasoning, cost_usd: Option<f64>, cost_source}`. `cost_source` is Computed if any part was computed, Unknown if any part is unknown, else Reported;
- `by_tool`: a breakdown by provider/source;
- `last_quota_hit: Option<{period, reset_at, observed_at}>`;
- `sources: Vec<{collector_id, last_update, stale}>`. A source is stale when it has had no update in 24h, per Boundaries.

"Today" is the local calendar day. Add storage queries for these aggregates and do not load full rows into memory.

Carried over from Phase C: wire the `claude_status_path` key from `config.toml` (loaded by `claude-common/src/config.rs`) into the Claude quota collector's constructor in `default_collectors`. The default stays `~/.local/state/claude-tui/claude-status.json`. Add a test whose name contains `claude_quota` for the configured path.

Tests (names contain `status_rpc_subscriptions`) run against `:memory:` storage seeded with records from all three subscriptions, a quota snapshot, a stale source and a quota hit. They assert the grouping, the sums, the stale flags and the last hit.
