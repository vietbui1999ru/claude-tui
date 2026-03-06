# Claude TUI - Implementation Plan

**Date:** 2026-03-05
**Author:** Planner (Sonnet)
**Status:** Complete
**Source Documents:** architecture.md, visual-design.md, 2026-03-05-claude-tui-design.md

---

## A. Ordered Task List

### Phase 0: Workspace Foundation

#### Task 1 - Cargo Workspace + claude-common Crate Skeleton
**Assignment:** Coder A
**Files to create:**
- `Cargo.toml` (workspace root)
- `crates/claude-common/Cargo.toml`
- `crates/claude-common/src/lib.rs`
- `crates/claude-daemon/Cargo.toml`
- `crates/claude-daemon/src/main.rs` (stub: `fn main() {}`)
- `crates/claude-tui/Cargo.toml`
- `crates/claude-tui/src/main.rs` (stub: `fn main() {}`)

**Acceptance criteria:**
- `cargo build --workspace` succeeds with zero errors
- All three crates exist with correct workspace dependency wiring
- Workspace Cargo.toml matches architecture.md section 1 (resolver, workspace.dependencies)

**Complexity:** Low
**Dependencies:** None

---

#### Task 2 - Data Models in claude-common
**Assignment:** Coder A
**Files to create/modify:**
- `crates/claude-common/src/models.rs`
- `crates/claude-common/src/lib.rs` (add `pub mod models; pub use models::*;`)

**Acceptance criteria:**
- All types implemented per architecture.md section 2: `ModelType`, `SessionStatus`, `DataSource`, `UsageRecord`, `ActiveSession`, `BudgetConfig`, `DailyAggregate`, `TimeRange`, `TimeWindow`
- All types derive `Debug, Clone, Serialize, Deserialize` as specified
- `ModelType` has `input_price_per_m()`, `output_price_per_m()`, `cache_read_price_per_m()`, `cache_write_price_per_m()`, `compute_cost()`, `as_str()`, `Display`, `FromStr`
- `BudgetConfig` has `Default` impl
- Unit tests for `ModelType::compute_cost()` and `FromStr`
- `cargo test -p claude-common` passes

**Complexity:** Medium
**Dependencies:** Task 1

---

#### Task 3 - IPC Protocol Types in claude-common
**Assignment:** Coder A
**Files to create/modify:**
- `crates/claude-common/src/protocol.rs`
- `crates/claude-common/src/lib.rs` (add `pub mod protocol; pub use protocol::*;`)

**Acceptance criteria:**
- All protocol types from architecture.md section 3: `RpcRequest`, `RpcResponse`, `RpcError`, `RpcMethod` enum, `StatusResponse`, `CollectorStatus`, `UsageQueryParams`, `UsageQueryResponse`, `UsageSummaryParams`, `UsageSummaryResponse`, `SessionsListParams`, `SessionsListResponse`, `SessionsGetParams`, `BudgetSetResponse`, `ModelsCompareParams`, `ModelsCompareResponse`, `ModelStats`
- `RpcMethod::from_request()` parses all method names correctly
- `default_limit()` returns 100
- Serialization round-trip tests for all request/response types
- `cargo test -p claude-common` passes

**Complexity:** Medium
**Dependencies:** Task 2

---

#### Task 4 - Error Types in claude-common
**Assignment:** Coder A
**Files to create/modify:**
- `crates/claude-common/src/errors.rs`
- `crates/claude-common/src/lib.rs` (add `pub mod errors; pub use errors::*;`)

**Acceptance criteria:**
- All error types from architecture.md section 5: `AppError`, `CollectorError`, `StorageError`, `IpcError`
- `IpcError::to_rpc_error()` maps to correct JSON-RPC error codes
- All `#[from]` conversions compile
- Note: `CollectorError::ApiRequest` wraps `reqwest::Error` and `CollectorError::LogWatch` wraps `notify::Error` -- these types are only available in `claude-daemon`. For `claude-common`, define the error variants that use `String` wrappers instead, or gate behind feature flags. **Decision: Use `String` wrappers in `claude-common` since it's a leaf crate that should not depend on reqwest/notify. The daemon will convert concrete errors to these string-based variants.**
- `cargo test -p claude-common` passes

**Complexity:** Low
**Dependencies:** Task 3

---

### Phase 1A: Daemon Implementation (Coder A)

#### Task 5 - SQLite Storage Layer
**Assignment:** Coder A
**Files to create/modify:**
- `crates/claude-daemon/src/storage.rs`
- `crates/claude-daemon/src/main.rs` (import storage module)

**Acceptance criteria:**
- `Storage::new(db_path)` creates the database file and parent directories
- `Storage::migrate()` runs all SQL from architecture.md section 4: creates `usage_records`, `daily_aggregates`, `sessions`, `budget_config`, `schema_version` tables with all indexes
- Seeds `budget_config` default row
- Implements all Storage methods from architecture.md section 6:
  - `insert_usage`, `insert_usage_batch`
  - `query_usage` (with time_range, model, project, limit, offset filters)
  - `get_summary` (aggregated by TimeWindow)
  - `get_daily_aggregates`
  - `recompute_daily_aggregate`
  - `upsert_session`, `list_sessions`, `get_session`
  - `get_budget`, `set_budget`
  - `get_cost_today`
  - `get_model_stats`
- Unit tests: insert + query round-trip, deduplication via UUID, budget get/set, session upsert, daily aggregate computation
- `cargo test -p claude-daemon` passes (tests use in-memory SQLite `:memory:`)

**Complexity:** High
**Dependencies:** Task 4

---

#### Task 6 - IPC Server (Unix Domain Socket)
**Assignment:** Coder A
**Files to create/modify:**
- `crates/claude-daemon/src/ipc.rs`

**Acceptance criteria:**
- `IpcServer::new(socket_path, storage)` creates the server
- `IpcServer::run()` binds a Unix domain socket, accepts connections, spawns per-client tokio tasks
- Each client connection reads newline-delimited JSON, parses `RpcRequest`, dispatches via `RpcMethod::from_request()`
- `handle_request()` routes each `RpcMethod` variant to the corresponding `Storage` method, wraps result in `RpcResponse`
- Handles malformed JSON gracefully (returns JSON-RPC parse error)
- Handles unknown methods (returns method not found error)
- Cleans up stale socket file on startup
- Socket path resolution follows architecture.md Appendix B (env var, XDG, fallback)
- Integration test: spawn server in background, connect via `tokio::net::UnixStream`, send a `status` request, receive valid response
- `cargo test -p claude-daemon` passes

**Complexity:** High
**Dependencies:** Task 5

---

#### Task 7 - Collector Module (API Poller + Log Watcher)
**Assignment:** Coder A
**Files to create/modify:**
- `crates/claude-daemon/src/collector.rs`

**Acceptance criteria:**
- `CollectorConfig` struct with `api_key`, `poll_interval_secs`, `log_paths`, `fallback_to_logs` fields
- `Collector::new(config, storage_handle)` creates the collector
- `Collector::run()` spawns `ApiPoller` and `LogWatcher` as tokio tasks
- `ApiPoller`:
  - Polls at configured interval (default 60s)
  - Follows pagination cursors
  - Deduplicates via UUID
  - Exponential backoff on failure (base 5s, max 300s, jitter 0-5s)
  - Auth errors trigger fallback to log mode
  - Sends `UsageRecord` batches to storage
- `LogWatcher`:
  - Uses `notify` crate with FSEvents backend on macOS
  - Watches `~/.claude/logs/`, `~/.config/claude/logs/`, and custom paths
  - Parses JSONL log lines, normalizes to `UsageRecord`
  - Generates deterministic UUID from (timestamp_epoch_ms, model, input_tokens, output_tokens)
- Fallback state machine: `ApiActive` -> `LogFallback` -> `Offline` per architecture.md section 7.3
- Data normalization pipeline: Parse -> Validate -> Enrich -> Normalize per section 7.4
- Unit tests: log line parsing, UUID generation determinism, normalization pipeline, backoff calculation
- `cargo test -p claude-daemon` passes

**Complexity:** High
**Dependencies:** Task 5

---

#### Task 8 - Daemon main.rs Wiring
**Assignment:** Coder A
**Files to modify:**
- `crates/claude-daemon/src/main.rs`

**Acceptance criteria:**
- Tokio async entry point with `#[tokio::main]`
- Initializes tracing-subscriber with env-filter
- Reads config from `~/.config/claude-daemon/config.toml` (if exists) using TOML parsing
- Determines socket path, database path via `directories` crate
- Creates `Storage`, runs migrations
- Spawns `Collector::run()` as a background task
- Spawns `IpcServer::run()` as the main server loop
- Handles SIGINT/SIGTERM for graceful shutdown (cleanup socket file)
- `cargo build -p claude-daemon` succeeds
- Running `claude-daemon` starts and listens on socket (manual verification)

**Complexity:** Medium
**Dependencies:** Tasks 6, 7

---

### Phase 1B: TUI Implementation (Coder B)

#### Task 9 - TUI Skeleton (App + Event Loop + Terminal Setup)
**Assignment:** Coder B
**Files to create/modify:**
- `crates/claude-tui/src/main.rs`
- `crates/claude-tui/src/app.rs`
- `crates/claude-tui/src/lib.rs` (optional, for module declarations)

**Acceptance criteria:**
- `main()` initializes terminal (crossterm raw mode, alternate screen), creates `App`, runs event loop, restores terminal on exit
- `App` struct holds: `current_tab: Tab`, `should_quit: bool`, `tick_rate`, connection status
- `Tab` enum: `Tokens`, `Costs`, `Models`, `Live`
- Event loop handles: crossterm key events, tick timer
- Pressing `q` sets `should_quit = true` and exits cleanly
- Left/right arrows cycle through tabs
- Renders placeholder text showing current tab name
- Terminal restore works even on panic (install panic hook)
- `cargo build -p claude-tui` succeeds
- Running `claude-tui` shows a bordered terminal with tab names and quit works

**Complexity:** Medium
**Dependencies:** Task 4 (needs error types from claude-common)

---

#### Task 10 - IPC Client
**Assignment:** Coder B
**Files to create/modify:**
- `crates/claude-tui/src/client.rs`

**Acceptance criteria:**
- `DaemonClient::connect(socket_path)` connects to Unix domain socket
- Implements all client methods from architecture.md section 6: `status()`, `query_usage()`, `get_summary()`, `list_sessions()`, `get_session()`, `get_budget()`, `set_budget()`, `compare_models()`
- Each method: constructs `RpcRequest`, serializes to JSON + newline, sends over socket, reads response line, deserializes `RpcResponse`, extracts result or maps error to `IpcError`
- Auto-incrementing request ID
- Handles connection failures gracefully (returns `IpcError::DaemonNotRunning` or `IpcError::Connection`)
- Socket path resolution matches daemon (shared function in claude-common or duplicated)
- `cargo build -p claude-tui` succeeds

**Complexity:** Medium
**Dependencies:** Task 4

---

#### Task 11 - Main Layout Frame + Sidebar
**Assignment:** Coder B
**Files to create/modify:**
- `crates/claude-tui/src/app.rs` (rendering logic)
- `crates/claude-tui/src/widgets/mod.rs`
- `crates/claude-tui/src/widgets/sidebar.rs` (optional, or inline in app.rs)

**Acceptance criteria:**
- Implements the full layout from visual-design.md section 1:
  - Header: app title (left), connection status + datetime (right)
  - Tab bar: `Tabs` widget with active/inactive styling per visual-design.md section 3 color scheme
  - Content area: 70/30 horizontal split (main + sidebar)
  - Footer: key hints in styled format
- Sidebar renders "Today's Summary" block with:
  - Model, Sessions, Tokens, Cost fields
  - Daily budget `Gauge` with color gradient (green/yellow/orange/red per thresholds)
  - Monthly budget `Gauge`
  - Top Model Today percentages
- Header connection status: `[Connected]` green, `[Reconnecting...]` yellow, `[Disconnected]` red
- Color constants defined per visual-design.md section 3
- Responsive behavior: sidebar hidden when width < 100 cols, warning overlay when < 80 cols per visual-design.md section 8
- `cargo build -p claude-tui` succeeds

**Complexity:** Medium
**Dependencies:** Task 9

---

#### Task 12 - Token Chart Widget
**Assignment:** Coder B
**Files to create/modify:**
- `crates/claude-tui/src/widgets/token_chart.rs`

**Acceptance criteria:**
- `TokenChartWidget` renders a bar chart of token usage over time
- Uses ratatui `BarChart` with `BarGroup` per date
- Stacked bars: input (blue), output (orange), cache read (green) per visual-design.md section 3
- Time range selector at top: `[7d] 30d 90d`, toggled by `1`/`2`/`3` keys
- Y-axis: token counts with K/M suffixes per visual-design.md section 10
- X-axis: dates formatted as `MMM DD`
- Legend bar at bottom with colored labels
- `render()` takes `area: Rect`, `buf: &mut Buffer`, `data: &[DailyAggregate]`
- Works with empty data (shows "No data available" centered)
- `cargo build -p claude-tui` succeeds

**Complexity:** Medium
**Dependencies:** Task 11

---

#### Task 13 - Cost Breakdown Widget
**Assignment:** Coder B
**Files to create/modify:**
- `crates/claude-tui/src/widgets/cost_breakdown.rs`

**Acceptance criteria:**
- Layout per visual-design.md section 2.2:
  - Top-left: horizontal bar chart per model with cost + percentage
  - Top-right: cumulative spend sparkline
  - Middle: scrollable project cost table (Project, Model, Cost, Pct columns)
  - Bottom: full-width budget progress bar with threshold marker
- Budget bar color shifts at thresholds: green (<50%), yellow (50-75%), orange (75-90%), red (>90%)
- Table scrollable with up/down keys (scroll state tracked in App)
- Numbers formatted per visual-design.md section 10 (costs to 2 decimal places, percentages as integers)
- Works with empty data
- `cargo build -p claude-tui` succeeds

**Complexity:** High
**Dependencies:** Task 11

---

#### Task 14 - Model Comparison Widget
**Assignment:** Coder B
**Files to create/modify:**
- `crates/claude-tui/src/widgets/model_compare.rs`

**Acceptance criteria:**
- Layout per visual-design.md section 2.3:
  - Top: comparison table (Model, Total Tokens, Avg/Session, Total Cost, Avg Latency)
  - Middle: usage distribution with `Gauge` per model, colored by model color
  - Bottom: token breakdown table (Model, Input, Output, Cache Read, Cache Write)
- Rows styled with model colors (Opus: purple, Sonnet: blue, Haiku: teal)
- Numbers right-aligned, names left-aligned
- Column sorting with `s` key (cycle) and `S` (reverse) -- sort indicator shown in header
- Works with empty data
- `cargo build -p claude-tui` succeeds

**Complexity:** Medium
**Dependencies:** Task 11

---

#### Task 15 - Live Session Monitor Widget
**Assignment:** Coder B
**Files to create/modify:**
- `crates/claude-tui/src/widgets/live_monitor.rs`

**Acceptance criteria:**
- Layout per visual-design.md section 2.4:
  - Top: session list table (#, Session, Model, Status, Tokens, Duration)
  - Bottom: selected session detail panel
- Status column uses styled text: "Streaming" + spinner animation (green), "Idle" (yellow), "Completed" (dim)
- Dot animation for streaming: cycles `. -> .. -> ... -> .. -> . -> (empty)` per visual-design.md section 9
- Session detail shows: Model, Status, Started, Duration, Input tokens, Output tokens, Est Cost
- Empty state: centered "No active sessions" with dim "Waiting for Claude activity..." subtitle
- Session selection with up/down keys (when Live tab is active)
- Duration computed from `started_at` to now, formatted as `Xm Ys`
- `cargo build -p claude-tui` succeeds

**Complexity:** Medium
**Dependencies:** Task 11

---

#### Task 16 - Keyboard Handling + Tab Navigation
**Assignment:** Coder B
**Files to modify:**
- `crates/claude-tui/src/app.rs`

**Acceptance criteria:**
- Full keybinding implementation per design doc:
  - `q`: quit
  - `r`: manual refresh (re-fetch data from daemon)
  - Left/Right arrows: cycle tabs
  - Up/Down arrows: scroll within current tab (project table in Costs, session list in Live)
  - `1`/`2`/`3`: time range selector in Tokens tab
  - `s`/`S`: sort toggle in Models tab
  - `?`: toggle help overlay (optional, stretch goal)
- Tab-specific key handling: only process scroll keys when relevant tab is active
- Refresh triggers IPC calls to update displayed data
- `cargo build -p claude-tui` succeeds

**Complexity:** Low
**Dependencies:** Tasks 12, 13, 14, 15

---

#### Task 17 - Data Integration (App <-> Client <-> Widgets)
**Assignment:** Coder B
**Files to modify:**
- `crates/claude-tui/src/app.rs`
- `crates/claude-tui/src/main.rs`

**Acceptance criteria:**
- `App` holds a `DaemonClient` instance (connected on startup, with graceful handling if daemon is not running)
- On startup, fetches: `status`, `usage.summary` (for Tokens tab), `sessions.list` (for Live tab), `budget.get`
- Data stored in App state, passed to widgets during rendering
- Periodic refresh: budget/status every 30s, live sessions every 2s, charts on manual refresh
- Connection status in header reflects actual daemon connectivity
- Handles daemon disconnection gracefully: shows `[Disconnected]`, retries connection periodically
- Clock in header updates every 1s per visual-design.md section 9
- `cargo build -p claude-tui` succeeds
- With daemon running: TUI shows real data from daemon

**Complexity:** High
**Dependencies:** Tasks 10, 16

---

### Phase 1C: Shell Scripts (Coder B)

#### Task 18 - SketchyBar Plugin Script
**Assignment:** Coder B
**Files to create:**
- `scripts/sketchybar/claude_plugin.sh`

**Acceptance criteria:**
- Queries daemon via `socat` on Unix domain socket
- Sends JSON-RPC `status` request per architecture.md Appendix C
- Parses response with `jq`: extracts `active_sessions`, `current_model`, `cost_today_usd`, `budget_pct`
- Formats output per visual-design.md section 4:
  - Icon: `*` (active), `-` (idle), `!` (over budget), `x` (disconnected)
  - Budget bar: 6-char Unicode block bar
  - Color coding by budget percentage thresholds
- Update interval: 5s when active, 60s when idle per visual-design.md section 4
- Handles daemon offline gracefully (outputs "offline")
- Script is executable (`chmod +x`)

**Complexity:** Low
**Dependencies:** Task 6 (needs daemon socket to query)

---

#### Task 19 - Menu Bar Script
**Assignment:** Coder B
**Files to create:**
- `scripts/menubar.sh`

**Acceptance criteria:**
- Queries daemon same as SketchyBar plugin
- Supports three format levels per visual-design.md section 5:
  - Full: `[icon] [model_abbrev] | [budget_bar] [pct]% | [$cost]`
  - Compact: `[icon] [model_abbrev] [pct]% $[cost]`
  - Minimal: `[model_abbrev] $[cost]`
- Model abbreviations: O (Opus), S (Sonnet), H (Haiku), M (Mixed), - (None)
- Active/idle indicators per visual-design.md section 5
- Truncation rules: drop budget bar first, then percentage, then model
- Script is executable

**Complexity:** Low
**Dependencies:** Task 6

---

#### Task 20 - AeroSpace Workspace Indicator
**Assignment:** Coder B
**Files to create:**
- `scripts/aerospace/claude_workspace.sh`

**Acceptance criteria:**
- Queries daemon for active model
- Outputs per visual-design.md section 6: `[model_abbrev][status_dot]`
  - `S*` (Sonnet active), `O*` (Opus active), `H` (Haiku idle), `-` (no activity)
- Status dots: `*` (active/streaming), nothing (idle), `!` (over budget)
- Handles daemon offline (outputs `-`)
- Script is executable

**Complexity:** Low
**Dependencies:** Task 6

---

### Phase 2: Integration + Polish

#### Task 21 - Integration Test (Daemon + TUI Client)
**Assignment:** Coder A
**Files to create:**
- `tests/integration_test.rs` (workspace-level integration test, or `crates/claude-daemon/tests/integration.rs`)

**Acceptance criteria:**
- Test spawns daemon in background with a temp socket path and in-memory/temp DB
- Test creates a `DaemonClient`, connects to the temp socket
- Verifies: `status()` returns valid response, `query_usage()` with empty DB returns empty vec, `budget.get` returns defaults, `budget.set` + `budget.get` round-trips
- Test inserts usage records via a second connection, then queries and verifies they appear
- Test cleans up socket file and DB on completion
- `cargo test` passes

**Complexity:** Medium
**Dependencies:** Tasks 8, 10

---

#### Task 22 - Socket Path Helper in claude-common
**Assignment:** Coder A
**Files to create/modify:**
- `crates/claude-common/src/paths.rs`
- `crates/claude-common/src/lib.rs`

**Acceptance criteria:**
- `socket_path() -> PathBuf` implements logic from architecture.md Appendix B
- `db_path() -> PathBuf` resolves database location per architecture.md section 4
- `config_path() -> PathBuf` resolves config file per architecture.md Appendix A
- Used by both daemon and TUI client (single source of truth)
- `cargo test -p claude-common` passes

**Complexity:** Low
**Dependencies:** Task 1

---

---

## B. Work Stream Split

### Coder A: claude-daemon + claude-common

| Order | Task | Title | Complexity | Dependencies |
|-------|------|-------|------------|--------------|
| 1 | T1 | Cargo workspace + crate skeletons | Low | None |
| 2 | T2 | Data models in claude-common | Medium | T1 |
| 3 | T3 | IPC protocol types in claude-common | Medium | T2 |
| 4 | T4 | Error types in claude-common | Low | T3 |
| 5 | T22 | Socket/DB/config path helpers | Low | T1 |
| 6 | T5 | SQLite storage layer | High | T4 |
| 7 | T6 | IPC server (Unix domain socket) | High | T5, T22 |
| 8 | T7 | Collector module (API poller + log watcher) | High | T5 |
| 9 | T8 | Daemon main.rs wiring | Medium | T6, T7 |
| 10 | T21 | Integration test (daemon + client) | Medium | T8, T10 |

### Coder B: claude-tui + shell scripts

| Order | Task | Title | Complexity | Dependencies |
|-------|------|-------|------------|--------------|
| 1 | T9 | TUI skeleton (app, event loop, terminal) | Medium | T4 |
| 2 | T10 | IPC client | Medium | T4 |
| 3 | T11 | Main layout frame + sidebar | Medium | T9 |
| 4 | T12 | Token chart widget | Medium | T11 |
| 5 | T13 | Cost breakdown widget | High | T11 |
| 6 | T14 | Model comparison widget | Medium | T11 |
| 7 | T15 | Live session monitor widget | Medium | T11 |
| 8 | T16 | Keyboard handling + tab navigation | Low | T12-T15 |
| 9 | T17 | Data integration (App <-> Client <-> Widgets) | High | T10, T16 |
| 10 | T18 | SketchyBar plugin script | Low | T6 |
| 11 | T19 | Menu bar script | Low | T6 |
| 12 | T20 | AeroSpace workspace indicator | Low | T6 |

---

## C. Integration Points

### Sync Point 1: claude-common Types Finalized (after Tasks 1-4, 22)
**What:** All shared data models, protocol types, error types, and path helpers must be stable before heavy work begins on both sides.
**Who waits:** Both Coder A (storage, IPC server) and Coder B (IPC client, widgets) depend on these types.
**Resolution:** Coder A owns Tasks 1-4 and T22. Coder B can begin T9 (TUI skeleton) as soon as T4 is done, since it only needs error types. Coder B should NOT start T10 (IPC client) until T3 (protocol types) is complete.

### Sync Point 2: IPC Server Running (after Task 6)
**What:** Shell scripts (T18-T20) need a running daemon with a functional socket to test against.
**Who waits:** Coder B's shell scripts.
**Resolution:** Coder B can write the scripts at any time but can only test them after Coder A's IPC server is functional. Coder B should prioritize TUI widgets first and defer scripts.

### Sync Point 3: IPC Client + Server Compatibility (Tasks 6 + 10)
**What:** The IPC client (Coder B) and IPC server (Coder A) must agree on protocol framing (newline-delimited JSON-RPC).
**Who waits:** Integration test (T21) and data integration (T17) need both sides working.
**Resolution:** Both sides implement against the shared protocol types in `claude-common`. Protocol framing rule: one JSON object per line (`\n` delimiter). Both sides must handle this identically.

### Sync Point 4: Integration Testing (Task 21)
**What:** Full round-trip test of daemon + TUI client.
**Who waits:** Both coders should have completed their core work.
**Resolution:** Coder A writes the integration test after T8 is done and T10 is available. If Coder B finishes T10 early, they can share it for integration testing.

---

## D. Build Sequence

The project must compile at every step. Here is the guaranteed build order:

```
Step 1:  T1  - Workspace + stubs           -> cargo build --workspace OK
Step 2:  T2  - Data models                 -> cargo build --workspace OK
Step 3:  T3  - Protocol types              -> cargo build --workspace OK
Step 4:  T4  - Error types                 -> cargo build --workspace OK
Step 5:  T22 - Path helpers                -> cargo build --workspace OK

--- Parallel streams begin ---

Coder A stream:                    Coder B stream:
Step 6A: T5  - Storage layer       Step 6B: T9  - TUI skeleton
Step 7A: T6  - IPC server          Step 7B: T10 - IPC client
Step 8A: T7  - Collector           Step 8B: T11 - Layout + sidebar
Step 9A: T8  - Daemon main.rs      Step 9B: T12 - Token chart
                                   Step 10B: T13 - Cost breakdown
                                   Step 11B: T14 - Model comparison
                                   Step 12B: T15 - Live monitor
                                   Step 13B: T16 - Keyboard handling
                                   Step 14B: T17 - Data integration

--- Scripts (after T6 is done) ---
Step 15B: T18 - SketchyBar plugin
Step 16B: T19 - Menu bar script
Step 17B: T20 - AeroSpace indicator

--- Integration ---
Step 18: T21 - Integration test (needs T8 + T10)
```

At every step, `cargo build --workspace` must succeed. No task introduces code that breaks compilation of any other crate.

---

## E. Testing Strategy

### Unit Tests (per module)

| Module | Test Focus | Test Location |
|--------|-----------|---------------|
| `claude-common/models` | `compute_cost()` accuracy, `FromStr` parsing, `Default` for `BudgetConfig` | `crates/claude-common/src/models.rs` (inline `#[cfg(test)]`) |
| `claude-common/protocol` | Serialization round-trips for all request/response types, `RpcMethod::from_request()` dispatch | `crates/claude-common/src/protocol.rs` (inline) |
| `claude-common/errors` | `IpcError::to_rpc_error()` code mapping | `crates/claude-common/src/errors.rs` (inline) |
| `claude-daemon/storage` | CRUD operations on in-memory SQLite, dedup via UUID, aggregation queries, budget get/set | `crates/claude-daemon/src/storage.rs` (inline) |
| `claude-daemon/ipc` | Request parsing, response formatting, error responses for bad input | `crates/claude-daemon/src/ipc.rs` (inline) |
| `claude-daemon/collector` | Log line parsing, deterministic UUID generation, normalization pipeline, backoff calculation | `crates/claude-daemon/src/collector.rs` (inline) |

### Integration Tests

| Test | Description | Location |
|------|------------|----------|
| Daemon + Client round-trip | Spawn daemon, connect client, exercise all RPC methods | `tests/integration_test.rs` or `crates/claude-daemon/tests/` |
| Storage migrations | Test migration from empty DB to current schema version | Included in `storage.rs` unit tests |

### Manual Testing Checklist

- [ ] `cargo build --workspace` compiles clean (zero warnings with `#![warn(clippy::all)]`)
- [ ] `claude-daemon` starts, creates socket, logs startup message
- [ ] `claude-tui` starts, shows empty dashboard with `[Disconnected]` status
- [ ] Start daemon, then TUI: status changes to `[Connected]`, sidebar populates
- [ ] Tab navigation: left/right arrows switch tabs, correct content renders
- [ ] Tokens tab: bar chart renders with mock/real data, time range selector works
- [ ] Costs tab: model breakdown, project table, budget bar all render
- [ ] Models tab: comparison table, usage distribution gauges render
- [ ] Live tab: shows active sessions or empty state correctly
- [ ] `q` key quits TUI cleanly, terminal restored to normal
- [ ] Terminal resize: layout adapts, sidebar hides at narrow widths
- [ ] SketchyBar plugin: outputs correct format when daemon running
- [ ] Menu bar script: outputs correct format at all truncation levels
- [ ] AeroSpace script: outputs model abbreviation + status dot
- [ ] All three scripts output fallback when daemon is offline

---

## Summary

**Total tasks:** 22
**Coder A tasks:** 10 (T1-T8, T21, T22)
**Coder B tasks:** 12 (T9-T20)
**Critical path:** T1 -> T2 -> T3 -> T4 -> T5 -> T6 -> T8 (daemon ready) and T4 -> T9 -> T11 -> T16 -> T17 (TUI ready), then T21 (integration)
**Estimated parallelism:** After T4 completes, both coders work independently until integration testing.
