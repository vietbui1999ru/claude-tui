# Phase C: collectors (AC-17, AC-1, AC-2, AC-3, AC-4)

## Current state

- `crates/claude-daemon/src/collector.rs` is one concrete `Collector` struct with Claude-only parsing (`scan_log_directory`, `read_new_lines`, `parse_log_line`, `generate_log_uuid`). Cursors are an in-memory `HashMap<PathBuf, u64>`, and the cursor is set to the file length rather than the bytes consumed.
- `main.rs` spawns the collector on a `tokio::time::interval` with a `CancellationToken`. Keep the polling loop; do not wire `notify`.
- After Phase B, storage has `collector_cursors`, `quota_snapshots`, `quota_hits`, and `(provider, dedup_key)` uniqueness.

## Structure

Create `crates/claude-daemon/src/collectors/`:
- `mod.rs`: the `SourceCollector` trait from SPEC.md "Code Style", a `Cursor` type (a string map persisted via `get_cursor`/`set_cursor`), and a shared JSONL tail reader. The reader advances the cursor only past the last complete `\n`-terminated line, resets when a file shrinks, and caps each line at 1 MiB (skip longer lines with a `warn!`).
- `claude.rs`: the old `collector.rs`, moved behind the trait.
- `codex.rs`, `pi.rs`, `opencode.rs`, `claude_quota.rs`.
- `reset_text.rs`: a port of `parseResetDuration` and the matching logic of `goUsageLimitState` from `~/dotfiles/pi/.pi/agent/extensions/pi-side-panel/status-pane.mjs` (read it). It sums `(\d+)\s*(days?|hours?|hrs?|hr|minutes?|mins?|min)` (day = 24h, h* = 1h, otherwise 1 min).

Collectors return quota snapshots and quota hits as well as usage records. Extend the trait's return value with a small struct, and update SPEC's trait shape only in code, not in SPEC.md. The run loop calls every collector each tick. A collector error is logged and recorded as that source's status, and never stops the others.

## Per AC (fixtures in `crates/claude-daemon/tests/fixtures/<provider>/`, written by hand from the shapes below)

- **AC-17, `claude_dedup_by_message_id`:**
  - `dedup_key = "{message.id}:{requestId}"`, falling back to the old UUID when either is missing.
  - The fixture has 3 lines with the same id and requestId and identical usage, and expects 1 record.
  - Include `**/subagents/*.jsonl`.
- **AC-1, `codex_rollout`:** each line is `{timestamp, type, payload}`.
  - Preferred: `token_usage_record` with `payload.response_id` and `payload.usage{input_tokens, cached_input_tokens, cache_write_input_tokens, output_tokens, reasoning_output_tokens}`.
  - For files that have no `token_usage_record`, use `event_msg` with `payload.type=="token_count"` and `info.last_token_usage`, deduped by `file:line-offset`.
  - `payload.rate_limits` may be null; null never overwrites. Otherwise record `{primary, secondary}` with `{used_percent, window_minutes, resets_at (epoch s)}` and `plan_type`.
  - The model comes from the latest `turn_context.payload.model`, else `"unknown"`.
  - Fixtures cover a new-format file, an old-format file and a null `rate_limits`.
- **AC-2, `pi_session`:**
  - Lines `{type:"message", id, timestamp, message{role:"assistant", provider, model, usage{input, output, cacheRead, cacheWrite, reasoning?, cost{total}}, stopReason, errorMessage?, timestamp(ms)}}`. `dedup_key` = entry id. The cost is Reported.
  - Quota hits use every variant listed in SPEC AC-2; a fixture line for each variant, plus a `rate_limit_error` line that must NOT become a hit.
  - The period comes from `5-hour|weekly|monthly`, and the reset is the message time plus the parsed duration.
  - Slug directories may contain spaces.
- **AC-3, `opencode_db`:**
  - Open the database with the `file:...?mode=ro` URI flag (`SQLITE_OPEN_READ_ONLY | SQLITE_OPEN_URI`).
  - Read assistant rows from `session_message` (`id, session_id, type, data`), where `data` is JSON with `cost`, `tokens{input, output, reasoning, cache{read, write}}`, `model{id, providerID}` and `time{created}` in ms.
  - Read the legacy `message(id, session_id, time_created, data)`, where `data` has `role`, `cost`, `tokens`, `modelID`, `providerID` and `time.created`.
  - Dedupe by message id. `cost` may be an integer. The cursor is the max `time_created` per table.
  - The fixture is a temp SQLite database created in the test with those tables plus a `credential` table. The test asserts the collector's SQL never mentions `credential` or `account`.
- **AC-4, `claude_quota`:**
  - The snapshot path comes from the config key `claude_status_path`, default `~/.local/state/claude-tui/claude-status.json`. Read `rate_limits.{five_hour,seven_day}.used_percentage` and `context_window.used_percentage`.
  - When the snapshot is missing or older than 24h, fall back to `~/.claude.json`. Parse it with a struct that has only `cachedUsageUtilization{fetchedAtMs, utilization{five_hour{utilization, resets_at}, seven_day{...}}}`, so serde ignores every other key. Mark the result stale.
  - A `resets_at` in the past means expired.
  - Tests cover fresh, missing and stale cases, using temp files.

Paths are configurable through the collector constructors so tests never touch `$HOME`. Commit one AC at a time, in the order above.
