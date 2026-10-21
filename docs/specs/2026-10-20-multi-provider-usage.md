---
kind: spec
work_type: spec
status: approved
branch: feat/multi-provider-usage
goal: "One local dashboard for context, usage, quota and approximate cost across the Claude, ChatGPT (Codex) and opencode-go subscriptions"
task: "Re-spec claude-tui from a Claude-only usage TUI into a multi-provider, subscription-grouped dashboard with a sketchybar line"
tags: [rust, ratatui, usage, dashboard, sketchybar]
---

# SPEC: Multi-provider usage dashboard (claude-tui v2)

## Objective

- **Who:** one person (this machine's user), running three AI subscriptions: Claude Max through Claude Code; a ChatGPT plan used by the Codex CLI and by Pi's `openai-codex` provider; and the opencode-go plan used by Pi's `opencode-go` provider and by OpenCode.
- **Problem:** usage is scattered. Claude's 5h and 7d windows show only in the current session's statusline and the session-band band. Codex rate limits are buried in rollout files. Pi cost shows only in Pi's own side panel. The opencode-go quota is invisible until a request fails with `429 GoUsageLimitError`. No single place answers "which subscription am I burning, and how fast?".
- **What success looks like:**
  - `claude-tui` opens on an Overview with one card per subscription. Each card shows its quota windows (where the provider exposes them), tokens and approximate cost for today and the last 7 days, and how fresh each number is.
  - A one-line `claude-tui status --line` feeds a sketchybar item, so the same numbers are always on screen.
- Everything stays local: the daemon only reads files that the tools already write. No network calls.
- This builds on the existing workspace (`claude-common`, `claude-daemon`, `claude-tui`), which today handles Claude Code logs only, with a hardcoded price table and mostly mock TUI data.

## Tech Stack

- Rust 2024 edition, MSRV 1.85 (existing workspace).
- TUI: ratatui 0.29 and crossterm 0.28 (existing).
- Daemon: tokio 1.43, rusqlite 0.32 (bundled), notify 7 (macos_fsevent), notify-debouncer-mini 0.5 (existing).
- New dependencies, Ask First: `toml` and `serde` for the config file, if they are not already transitive.
- Reading OpenCode's SQLite database uses the existing rusqlite, opened read-only.

## Commands

- Build: `cargo build --workspace`
- Test: `cargo test --workspace`
- Lint: `cargo clippy --workspace --all-targets -- -D warnings`
- Format: `cargo fmt --all --check`
- Run: `cargo run -p claude-daemon`, then `cargo run -p claude-tui` (demo mode: `cargo run -p claude-tui -- --demo`)

## Project Structure

- `crates/claude-common/`: shared models (`UsageRecord`, pricing, the new `Provider` and `Subscription`), the JSON-RPC protocol, and paths.
- `crates/claude-daemon/`: collectors (the existing Claude log collector, plus the new Codex, Pi, OpenCode and Claude-quota collectors), SQLite storage and migrations, and the IPC server.
- `crates/claude-tui/`: the ratatui app, widgets, and the `status --line` subcommand.
- `scripts/sketchybar/claude_plugin.sh`: the existing sketchybar plugin, which moves to `claude-tui status --line`.
- `docs/plans/`: the existing design and review docs. This SPEC supersedes their Claude-only scope.
- Test fixtures under `crates/claude-daemon/tests/fixtures/<provider>/`: trimmed, anonymised samples of each source format. They contain no real prompts or paths.

## Code Style

- Follow the existing crate style: `thiserror` errors per crate, `tracing` for logs, and no `unwrap()` outside tests.
- One collector per source, behind a common trait:

```rust
pub trait SourceCollector {
    /// Stable id, e.g. "codex-rollouts".
    fn id(&self) -> &'static str;
    /// New records since the stored cursor. Read-only: never writes to the source.
    fn collect(&mut self, cursor: &Cursor) -> Result<(Vec<UsageRecord>, Cursor), CollectorError>;
}
```

- Provider-reported cost always wins over computed cost. A computed cost is labelled `≈` in the UI.

## Git Workflow

- Branch: `feat/multi-provider-usage` (create it from `main` before approval; `ac-run` requires the spec's branch to be checked out).
- Commit per acceptance criterion, as conventional commits (`feat(daemon): codex rollout collector`).
- Open a draft PR to `main`. CI (`.github/workflows/rust.yml`) gains the fmt and clippy steps (AC-13).

## Boundaries

### Always

- Open every provider data source read-only. Open SQLite with `?mode=ro`. Never write to `~/.claude`, `~/.codex`, `~/.pi` or `~/.local/share/opencode`.
- Keep the daemon's own state in its existing paths (`$XDG_DATA_HOME/claude-daemon/usage.db`, `~/.config/claude-daemon/config.toml`).
- Show freshness next to every number. A source with no update in 24 hours is marked stale, never shown as current.
- Keep counting tokens for a model with no configured price. Its cost shows as unknown.
- Run `cargo test --workspace` before each commit.

### Ask First

- Any network call, such as Anthropic's Admin Usage API or an OAuth usage endpoint.
- New dependencies beyond `toml` and `serde`.
- Schema changes beyond the v2 migration in AC-8.
- Changes in the dotfiles repo, such as the statusline snapshot line (AC-4) or the sketchybar item (AC-15). Those land as a separate PR there.
- Renaming the project or its binaries.

### Never

- Read, log or store credentials: `~/.codex/auth.json`, `~/.pi/agent/auth.json`, OpenCode's `credential`, `account` and `control_account` tables, `~/.local/share/opencode/auth.json`, or any token or API key value.
- Store prompt or response text from any transcript. Keep only usage metadata (timestamps, model, provider, token counts, cost, session id, error kind).
- Edit a `verify:` or `baseline:` line, or a test, to make a check pass.
- Hardcode prices in source code. Prices live in the config file (AC-7).

## Acceptance Criteria

- AC-1: The Codex collector reads `~/.codex/sessions/**/rollout-*.jsonl`.
  - Token usage per response comes from `token_usage_record` lines, de-duplicated by `response_id`. Older files have no such lines, so for them it uses `event_msg` `token_count` `info.last_token_usage`. Either way it records input, cached input, cache write, output and reasoning tokens, plus `model_context_window`.
  - It records `rate_limits.primary` and `rate_limits.secondary` (`used_percent`, `window_minutes`, `resets_at` in epoch seconds) and `rate_limits.plan_type`. `rate_limits` is null on some events; a null never overwrites the last known value.
  - The model comes from the latest `turn_context.payload.model`. When a file has none, the model is recorded as unknown.
  verify: `cargo test -p claude-daemon codex_rollout`
- AC-2: The Pi collector reads `~/.pi/agent/sessions/**/*.jsonl`. Slug directories may contain spaces.
  - For each assistant message it records provider, model, input, output, cacheRead, cacheWrite, reasoning (optional, absent counts as 0) and `cost.total`. It de-duplicates by entry `id` and timestamps from `message.timestamp` (epoch ms).
  - Assistant `errorMessage` values are recorded as quota-hit events with subscription, period (`5h`, `weekly`, `monthly` or unknown) and a reset time parsed from "Resets in …" or "Try again in ~…" text. The variants are:
    - `429 <period> usage limit reached. Resets in …`;
    - `429: {"type":"GoUsageLimitError","message":"…"}`, including `Go usage limit exceeded`, which has no period;
    - from `openai-codex`, `The usage limit has been reached` and `You have hit your ChatGPT usage limit (…). Try again in ~N min.`, which are recorded under `chatgpt`.
  - An error whose type is `rate_limit_error` is provider throttling, not a quota hit, and is not recorded as one.
  verify: `cargo test -p claude-daemon pi_session`
- AC-3: The OpenCode collector opens `~/.local/share/opencode/opencode.db` read-only (`?mode=ro`). It reads per-message usage from assistant rows of `session_message` (JSON `data`: `cost`, `tokens.{input,output,reasoning,cache.read,cache.write}`, `model.{id,providerID}`, `time.created` in ms), and from the legacy `message` table (JSON `data`: `cost`, `tokens`, `modelID`, `providerID`) for older rows, de-duplicated by message id.
  - The legacy `session` table is not used: it stopped recording cost and tokens around OpenCode 1.18.
  - `cost` may be an integer or a real number.
  - It never selects from the `credential`, `account` or `control_account` tables.
  verify: `cargo test -p claude-daemon opencode_db`
- AC-4: The Claude quota collector reads a JSON snapshot file (default `~/.local/state/claude-tui/claude-status.json`, configurable). The file has the shape Claude Code passes to its statusline: `rate_limits.five_hour.used_percentage`, `rate_limits.seven_day.used_percentage`, `context_window.used_percentage`, `session_id`.
  - When the file is missing or stale, the collector falls back to `cachedUsageUtilization` in `~/.claude.json`: `utilization.five_hour.utilization` and `utilization.seven_day.utilization` (integer percent) with their `resets_at` (ISO), marked stale with `fetchedAtMs`.
  - It reads no other key of that file. A window whose `resets_at` is in the past is shown as expired, not current.
  verify: `cargo test -p claude-daemon claude_quota`
- AC-5: Records are grouped by subscription:
  - `claude`: Claude Code logs and the Claude quota snapshot;
  - `chatgpt`: the Codex CLI and Pi's `openai-codex` provider;
  - `opencode-go`: Pi's `opencode-go` provider and OpenCode;
  - `other`: any other provider.
  The grouping is a pure function, with tests for each source.
  verify: `cargo test -p claude-common subscription_grouping`
- AC-6: A record whose model has no configured price is stored with its tokens and an unknown cost. It is no longer silently skipped, which is the current behaviour at `collector.rs:278-281`.
  verify: `cargo test -p claude-daemon unknown_model_kept`
- AC-7: Prices load from `[prices."<model-substring>"]` tables (input, output, cache_read, cache_write in USD per million tokens) in `~/.config/claude-daemon/config.toml`. The hardcoded table in `models.rs` is removed. A provider-reported cost (Pi `cost.total`, OpenCode message `cost`) takes precedence over a computed one, and the record says which it is.
  verify: `cargo test -p claude-common price_config`
- AC-8: Storage migration v2 adds the provider, subscription, cost_source and dedup_key columns, the quota tables (`quota_snapshots`, `quota_hits`) and a `collector_cursors` table, so collectors resume after a restart instead of rescanning. An existing v1 `usage.db` upgrades in place with no row loss.
  verify: `cargo test -p claude-daemon migrate_v2`
- AC-9: The `status` JSON-RPC method returns, per subscription: quota windows (used percentage, reset time, source and freshness), tokens and cost for today and the last 7 days, the latest quota-hit event, and stale flags.
  verify: `cargo test -p claude-daemon status_rpc_subscriptions`
- AC-10: `claude-tui status --line` prints one line for sketchybar, for example `C 5h 34% 7d 12% · X 7d 8% · G $3.20 ⚠limit 14:05`, and exits 0. With the daemon down it prints `usage: daemon down` and still exits 0.
  verify: `cargo test -p claude-tui status_line`
- AC-11: The TUI opens on an Overview tab with one card per subscription, built from daemon data. Mock data appears only with `--demo`. When the daemon is unreachable outside demo mode, the TUI says so instead of showing mock numbers.
  verify: `cargo test -p claude-tui no_mock_without_demo`
- AC-12: The IPC socket is created with mode 0600, with no window between bind and chmod (bind under a restrictive umask). A request larger than 64 KiB is rejected without unbounded reads. These are review findings V3 and V4 in `docs/plans/devils-advocate-review.md` and W8 in `memory-safety-review.md`.
  verify: `cargo test -p claude-daemon ipc_hardening`
- AC-13: Formatting and clippy are clean, and CI runs both.
  verify: `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`
- AC-14: The existing test suite keeps passing.
  verify: `cargo test --workspace`
  baseline: pass
- AC-15: On the person's machine, the Overview and the sketchybar item show plausible, current numbers for all three subscriptions after a day of normal use, and the opencode-go card shows the last limit hit.
  verify: manual - needs live data from the person's own subscriptions and a visual check of the bar
- AC-16: After the scaffold (AC-1 to AC-11), `docs/plans/review-status.md` lists every Critical finding in `docs/plans/code-review.md`, `devils-advocate-review.md` and `memory-safety-review.md` with its status.
  - On 2026-10-20, C2, C3, C4, V1, V2, V5, U1 and U2 were already fixed in code; each is recorded with the file and line that shows the fix.
  - The open ones are fixed with a test or deferred with a written reason. That covers C1's residue, the `unwrap_or(ModelType::Sonnet)` fallbacks in `storage.rs`, and any finding AC-12 does not cover.
  verify: manual - each finding's fix or deferral needs a human read against the review text
- AC-17: Claude Code log records are de-duplicated by `message.id` and `requestId`. Claude Code writes one line per content block with identical usage, and the current key (timestamp, model and tokens) counts these lines more than once. Subagent logs (`**/subagents/*.jsonl`) are included.
  verify: `cargo test -p claude-daemon claude_dedup_by_message_id`
- AC-18: `claude-tui status --json` prints the AC-9 data as one JSON document with a version field (`{"v":1,"subscriptions":{…}}`) and exits 0. With the daemon down it prints `{"v":1,"daemon":"down"}` and still exits 0. This is the contract the Pi extensions read (see Notes).
  verify: `cargo test -p claude-tui status_json_schema`

## Open Questions

All answered by the person on 2026-10-20:

- Claude 5h and 7d source: **yes**, the dotfiles statusline script (`claude/.claude/statusline-command.sh`) also writes its stdin JSON to the snapshot file. This lands as a separate dotfiles PR. Until it does, the `~/.claude.json` fallback is used and labelled stale.
- Cost on flat-rate plans: **yes**, show an API-equivalent cost labelled `≈ API $`, next to the quota windows, which remain the primary signal.
- opencode-go quota: **yes**, "limit hit at <time>" plus the 5h, weekly or monthly hint from the error text. No guessed percentage.
- Default prices: the builder fills the config defaults from the providers' **official pricing pages** at build time, with the date recorded in the config. No price values in this spec.
- Name: **keep** `claude-tui` and `claude-daemon`.
- Review findings: **fix after the scaffold.** AC-1 to AC-11 come first. Then AC-12 (V3, V4) and AC-16 (the other Critical findings) are a final phase.
- Pi status snapshots (`~/.pi/status/*.json`): **not used**; Pi session JSONL files are the live source. The person also wants stale snapshots cleared automatically. That is **out of scope for this repo**, because claude-tui stays read-only toward `~/.pi` (Boundaries). It goes to a dotfiles follow-up: the Pi extension that writes them prunes its own stale files.

## Notes

- Source map, surveyed 2026-10-20. Names only; no values were read from credential files.

  | Source | Path | Tokens | Cost | Quota | Freshness |
  |---|---|---|---|---|---|
  | Claude Code logs | `~/.claude/projects/**/*.jsonl` (`message.usage`, `message.model`) | yes | derive | no | live |
  | Claude quota | statusline stdin JSON (`rate_limits.*`, `context_window.*`) | n/a | n/a | 5h and 7d | live only while a session runs |
  | Claude quota fallback | `~/.claude.json` `cachedUsageUtilization` (`utilization.five_hour`, `utilization.seven_day`, `fetchedAtMs`) | n/a | n/a | yes | days stale |
  | Codex CLI | `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` (`token_usage_record`, `event_msg` `token_count`, `turn_context` for the model) | yes | derive | primary and secondary windows, `plan_type` (`rate_limits` sometimes null) | per session |
  | Pi | `~/.pi/agent/sessions/<cwd-slug>/*.jsonl` (`usage`, `cost.total`, `errorMessage`) | yes | reported | 429 events only (Go and ChatGPT) | live |
  | OpenCode | `~/.local/share/opencode/opencode.db` `session_message` and legacy `message` (JSON `data`); the `session` table is frozen | yes | reported | no | per message |

- Double counting: Pi's `openai-codex` usage and the Codex CLI draw on the same ChatGPT plan, and Pi's `opencode-go` usage and OpenCode draw on the same Go plan. The subscription grouping (AC-5) sums them; per-tool views stay available as a breakdown.
- `~/.claude/stats-cache.json` was last written 2026-10-04. Do not use it.
- Sketchybar: the current `sketchybarrc` (94 lines) has no usage item, so AC-15 needs a new one. The repo's `scripts/sketchybar/claude_plugin.sh` ignores `XDG_RUNTIME_DIR` when it finds the socket; `claude-tui status --line` resolves the path through `paths::socket_path()` instead. `.claude/settings.local.json` in this repo still has the stale prefix `~/repos/RustProjects/claude-tui`.
- Consolidation (decided 2026-10-20): the daemon becomes the single source of usage data, and the Pi extensions read `claude-tui status --json` (AC-18) instead of parsing on their own.
  - Ported to Rust: `goUsageLimitState` and `parseResetDuration` from `pi-side-panel/status-pane.mjs` (AC-2).
  - Rewired in a dotfiles follow-up PR, with their duplicate parsers removed:
    - the OPENCODE section of `status-pane.mjs` (`goUsageLimitState`, `piCostWindows`);
    - the limits segment of `pi-statusline.ts`;
    - `lib/openai-usage.ts`.
  - Those three compute cost windows from `.pi/status` snapshots, which put a whole session's cost into one window. Per-message JSONL parsing fixes that.
  - The `panel.state.json` ownership heartbeat stays.
  - The session-band band and its `hooks/pisession.ts` stay as they are.
- Dotfiles follow-up PR scope:
  - the statusline tee, which writes stdin to the snapshot file atomically, before the early exit taken when a session-band is alive;
  - the new sketchybar item;
  - the Pi extension rewiring;
  - pruning stale `.pi/status` files in `~/repos/pi-live-status`.
