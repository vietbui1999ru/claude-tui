# Code Review Report

**Reviewer:** Reviewer Agent (Sonnet)
**Date:** 2026-03-05
**Scope:** All source files in `crates/` and `scripts/`

---

## Critical Issues (must fix)

### [C1] `unwrap()` in production path: `storage.rs` row deserializer
- **File:** `crates/claude-daemon/src/storage.rs:662-688` (`row_to_usage_record`)
- **Issue:** Every field extraction uses `.unwrap_or_default()` or `.unwrap_or(0)` silently, masking data corruption. If column indices shift after a schema change, the code silently produces wrong data instead of returning an error. The closure signature returns `UsageRecord` (not `Result<UsageRecord>`), so errors cannot propagate.
- **Fix:** Change the closure to return `rusqlite::Result<UsageRecord>` and use `?` on each `row.get()` call, which is the rusqlite idiom. Same applies to `row_to_session` at line 691.

### [C2] `unchecked_transaction()` usage in `insert_usage_batch`
- **File:** `crates/claude-daemon/src/storage.rs:184`
- **Issue:** `unchecked_transaction()` bypasses rusqlite's borrow-checker safety guarantees. If the connection is shared across threads (it's behind `Arc<Mutex<Storage>>`, so this is safe at runtime), but it sets a poor precedent. Also, if an error occurs mid-batch, the transaction is dropped without explicit rollback, relying on implicit rollback. This is fine in practice but is fragile.
- **Fix:** Use `self.conn.transaction()` (the safe variant) by taking a `&mut self` receiver, or restructure so `insert_usage_batch` takes `&mut self`. If mutability cannot be changed, document clearly why `unchecked_transaction` is safe here.

### [C3] Silent data loss on database path resolution failure
- **File:** `crates/claude-daemon/src/main.rs:35`
- **Issue:** `db_path.to_str().unwrap_or(":memory:")` silently falls back to an in-memory database if the path contains invalid UTF-8. All data written during the session would be lost on exit with no error message to the user.
- **Fix:** Check for UTF-8 validity explicitly and exit with a clear error if conversion fails:
  ```rust
  let db_path_str = db_path.to_str().unwrap_or_else(|| {
      error!("database path contains invalid UTF-8: {}", db_path.display());
      std::process::exit(1);
  });
  ```

### [C4] `DaemonClient::is_connected()` always returns `true`
- **File:** `crates/claude-tui/src/client.rs:43-46`
- **Issue:** `is_connected()` always returns `true` regardless of actual connection state. This is explicitly noted as "best-effort", but the method name misleads callers into thinking it actually checks connectivity. If `App` ever calls this to decide whether to show real vs mock data, it will always show mock data as "connected".
- **Fix:** Remove the method (it's behind `#[allow(dead_code)]` anyway) or implement it properly by checking if `self.stream.try_lock().map_or(false, |g| g.is_some())`.

---

## Major Issues (should fix)

### [M1] `glob::use_glob` star import in `app.rs` pollutes namespace
- **File:** `crates/claude-tui/src/app.rs:6`
- **Issue:** `use claude_common::*;` imports everything from claude_common, including potentially conflicting names. It also makes it non-obvious which types come from where when reading the file.
- **Fix:** Replace with explicit imports: `use claude_common::{DailyAggregate, ModelType, ...}`. The same glob is used in `mock.rs:2`.

### [M2] `unwrap_or(ModelType::Sonnet)` as silent default is misleading
- **File:** `crates/claude-daemon/src/storage.rs:332, 390, 643, 675, 705`
- **Issue:** When a model string from the database fails to parse, the code silently substitutes `ModelType::Sonnet`. This means corrupt data or a future model type (e.g., a new Claude model) produces wrong cost calculations and model attribution without any warning log.
- **Fix:** Either log a warning before substituting, or propagate the parse error:
  ```rust
  model_str.parse().map_err(|e| StorageError::Query(format!("bad model '{model_str}': {e}")))?
  ```

### [M3] `DefaultHasher` for UUID generation is not stable across Rust versions
- **File:** `crates/claude-daemon/src/collector.rs:328-343` (`generate_log_uuid`)
- **Issue:** `std::collections::hash_map::DefaultHasher` is documented as not guaranteed to be stable across Rust releases or platforms. Using it for deterministic deduplication UUIDs means the same log line could generate different UUIDs after a Rust upgrade, breaking deduplication. The test at line 423 passes today but may fail after a toolchain update.
- **Fix:** Use a stable hash algorithm like `std::hash::SipHasher` (deprecated but stable) or preferably the `fnv` or `ahash` crate, or use `uuid::Uuid::new_v5()` with a namespace UUID to generate deterministic UUIDs.

### [M4] `compute_today_model_pcts` always returns 3 entries even with no data
- **File:** `crates/claude-tui/src/app.rs:545-575`
- **Issue:** When `total_tokens == 0`, the function returns a hardcoded `vec![(Sonnet, 0), (Opus, 0), (Haiku, 0)]`. This is harmless today but means the sidebar always shows all three models even when no data exists. More importantly, the early-return branch doesn't sort by percentage, so it always returns Sonnet first regardless of usage.
- **Fix:** Return an empty `Vec` when there's no data, and handle the empty case in the caller. Or document the intentional behavior.

### [M5] `SessionsGet` uses `INTERNAL_ERROR` code for "not found"
- **File:** `crates/claude-daemon/src/ipc.rs:207-213`
- **Issue:** When a session is not found, the server returns error code `-32603` (INTERNAL_ERROR). The correct code for a "not found" condition in the protocol should be a defined application error code (e.g., the existing `-1` COLLECTOR_UNAVAILABLE or a new `-3` NOT_FOUND), not a generic internal error.
- **Fix:** Define a `NOT_FOUND: i32 = -3` constant in `protocol.rs` and use it for the "session not found" case.

### [M6] Hardcoded magic numbers in `render_sidebar` layout
- **File:** `crates/claude-tui/src/app.rs:360-515`
- **Issue:** The sidebar layout uses raw numeric indices (e.g., `chunks[5]`, `chunks[9]`, `chunks[13]`, `14 + i`) that are fragile. Adding or removing a row requires updating every downstream index. This violates the visual design's intent of maintainability.
- **Fix:** Define named constants or a struct for layout slot indices. Or use an iterator-based approach to allocate areas.

### [M7] Shell scripts: `build_budget_bar` duplicated verbatim across three files
- **File:** `scripts/menubar.sh:56-70`, `scripts/sketchybar/claude_plugin.sh:56-70`, `scripts/aerospace/claude_workspace.sh` (partially)
- **Issue:** The `build_budget_bar` function is copy-pasted identically in two scripts. A bug fix or enhancement to the bar rendering requires updating multiple places.
- **Fix:** Extract to a shared `scripts/lib/claude_common.sh` sourced by each script.

### [M8] Shell scripts: `$?` check after pipeline may not reflect socat failure
- **File:** `scripts/menubar.sh:13`, `scripts/sketchybar/claude_plugin.sh:17`
- **Issue:** `RESPONSE=$(... | socat ...) ; if [ $? -ne 0 ]` — in bash, `$?` after a command substitution captures the exit status of the last command in the pipeline **inside** the substitution, which is `socat`. However, the correct pattern is to use `pipefail` or check `PIPESTATUS`. Currently works by coincidence but is fragile if the pipeline is modified.
- **Fix:** Add `set -o pipefail` at the top of each script, or restructure to run socat separately and capture both output and exit code.

---

## Minor Issues (nice to fix)

### [m1] `format_tokens_short` is duplicated in `app.rs` and `token_chart.rs`
- **File:** `crates/claude-tui/src/app.rs:535-543`, `crates/claude-tui/src/widgets/token_chart.rs:34-42`
- **Issue:** The same formatting function exists in two places with slightly different implementations (app.rs uses `{:.1}K`, token_chart.rs uses `{:.0}K`). This inconsistency may cause different displays of the same value in different areas of the UI.
- **Fix:** Move to a shared location (e.g., `crates/claude-tui/src/format.rs`) and use consistently.

### [m2] `format_tokens` and `format_tokens_comma` are also duplicated
- **File:** `crates/claude-tui/src/widgets/model_compare.rs:8-16`, `crates/claude-tui/src/widgets/live_monitor.rs:24-37`
- **Issue:** Two more formatting functions with the same logic are duplicated. `format_tokens` (model_compare) and `format_tokens_comma` (live_monitor) are identical.
- **Fix:** Same as [m1] — consolidate into a shared formatting module.

### [m3] `#[allow(dead_code)]` on `DaemonClient` struct and impl block
- **File:** `crates/claude-tui/src/client.rs:13, 20`
- **Issue:** The entire `DaemonClient` struct and impl block are annotated with `#[allow(dead_code)]`. This suggests the client is not yet wired up to the `App`. The `client` field in `App` is also annotated `#[allow(dead_code)]` at `app.rs:57`. This is acceptable for an MVP scaffold, but should be tracked.
- **Note:** This is intentional (mock-first approach) but should be addressed before a real release. Consider adding a `// TODO: wire up real client` comment.

### [m4] `Reconnecting` variant is dead code in `ConnectionStatus`
- **File:** `crates/claude-tui/src/app.rs:52`
- **Issue:** `ConnectionStatus::Reconnecting` is annotated `#[allow(dead_code)]` but is never set anywhere. The `reconnect()` method on `DaemonClient` exists but is never called.
- **Fix:** Either implement reconnection logic or remove the variant until needed.

### [m5] `get_daily_aggregates` in storage is unused from the IPC layer
- **File:** `crates/claude-daemon/src/storage.rs:360-405`
- **Issue:** `Storage::get_daily_aggregates` is not called from `ipc.rs`. It may be intended for future use, but it's dead code at the IPC level.
- **Fix:** Add `#[allow(dead_code)]` if intentionally kept, or remove it.

### [m6] `TimeWindow` parameter in `UsageSummaryParams` is ignored in `get_summary`
- **File:** `crates/claude-daemon/src/storage.rs:278`
- **Issue:** `UsageSummaryParams` has a `window: TimeWindow` field, but `Storage::get_summary()` never uses it. The query always groups by date and model regardless of whether the window is Day, Week, Month, or Quarter.
- **Fix:** Either implement windowed aggregation (e.g., group by week for `TimeWindow::Week`) or document that `window` is reserved for future use and `time_range` is the effective filter.

### [m7] `last_session_refresh` field is never read
- **File:** `crates/claude-tui/src/app.rs:79`
- **Issue:** `last_session_refresh: Instant` is stored in `App` but only set on initialization. It's never read (hence `#[allow(dead_code)]`). This suggests session refresh logic was planned but not implemented.
- **Fix:** Remove if unused, or implement session refresh using this field.

### [m8] `recompute_daily_aggregate` is never called in production code
- **File:** `crates/claude-daemon/src/storage.rs:407-431`
- **Issue:** This method is implemented but never invoked. The architecture doc describes end-of-day rollup logic (section 7.5), but no scheduler or periodic task calls this.
- **Fix:** Add a scheduler in `main.rs` that calls `recompute_daily_aggregate` at midnight UTC, as specified in the architecture doc.

### [m9] Shell scripts: missing `#!/usr/bin/env bash` portability
- **File:** All three shell scripts use `#!/bin/bash`
- **Issue:** `#!/bin/bash` is less portable than `#!/usr/bin/env bash` on systems where bash is not at `/bin/bash` (e.g., some macOS configurations with custom bash paths).
- **Fix:** Change to `#!/usr/bin/env bash` for better portability.

### [m10] `libc` dependency used in `paths.rs` without explicit declaration
- **File:** `crates/claude-common/src/paths.rs:17`
- **Issue:** `unsafe { libc::getuid() }` uses the `libc` crate, but `libc` does not appear in `claude-common/Cargo.toml`. This only compiles because it's a transitive dependency; adding `libc` explicitly as a direct dependency would be more correct and explicit.
- **Fix:** Add `libc = "0.2"` to `crates/claude-common/Cargo.toml`.

### [m11] `NaiveDate::parse_from_str` fallback to `Utc::now().date_naive()` is misleading
- **File:** `crates/claude-daemon/src/storage.rs:329, 389`
- **Issue:** If a date string from the database fails to parse, the code uses today's date as a fallback. This is worse than an error: it could attribute historical data to today, distorting charts. The `unwrap_or_else` is completely silent.
- **Fix:** Return an error or log a warning before substituting. This is closely related to [C1].

### [m12] Minimum terminal size is inconsistent between code and design doc
- **File:** `crates/claude-tui/src/app.rs:212`, `docs/plans/visual-design.md:17`
- **Issue:** The code enforces a minimum of 80x20 (`if size.width < 80 || size.height < 20`), but the visual design doc specifies "Minimum terminal size: 100 columns x 30 rows". The code allows a terminal that would clip the 70/30 sidebar split (sidebar only appears at width >= 100, per `app.rs:281`).
- **Fix:** Align the hard minimum to match the design doc (100x30), or update the design doc to reflect the actual behavior (80x20 shows full-width content, 100+ shows sidebar).

### [m13] `env::set_var` in tests without `#[serial]` attribute
- **File:** `crates/claude-common/src/paths.rs:96`
- **Issue:** The test uses `unsafe { std::env::set_var(...) }` which is documented as unsound in multi-threaded tests (which is the Rust test runner's default). The comment acknowledges this but the `EnvGuard` RAII struct alone doesn't prevent concurrent tests from reading the wrong env var value.
- **Fix:** Add the `serial_test` crate and mark these tests `#[serial]` to ensure they don't run concurrently with other tests that touch env vars.

---

## Positive Observations

- **Excellent test coverage for claude-common:** Both `models.rs` and `protocol.rs` have thorough unit tests covering serde round-trips, edge cases, and error paths. The test count is impressive (20+ tests in models, 15+ in protocol).
- **Strong storage tests:** `storage.rs` tests cover the full CRUD lifecycle including deduplication, batch inserts, filtering, and budget management. Testing against `:memory:` SQLite is the correct approach.
- **Well-structured error hierarchy:** The `errors.rs` design cleanly separates concerns (Collector/Storage/IPC) and uses `thiserror` idiomatically. String wrapping of external error types in `claude-common` keeps it as a leaf crate.
- **Good IPC test coverage:** `ipc.rs` has async tests covering the request dispatch, which is non-trivial to test correctly.
- **Proper terminal cleanup on panic:** The panic hook in `main.rs:20-25` correctly restores terminal state before printing the panic message.
- **Idiomatic Rust usage:** The codebase uses `?` consistently for error propagation in all production paths, avoids `unwrap()` in most places, and uses appropriate types (`Arc<Mutex<>>`, `AtomicU64`).
- **Collector fallback logic is well-designed:** The state machine in `collector.rs` with exponential backoff is clean and testable. The deterministic jitter implementation avoids randomness in tests.
- **Log parsing is defensive:** `parse_log_line` correctly rejects future timestamps and all-zero token counts, handles missing fields gracefully, and ignores unknown fields.
- **Shell scripts are functional:** The menu bar and SketchyBar scripts are readable, handle daemon offline gracefully, and produce correct output formats.
- **Design conformance is high:** The implemented layout matches the visual design spec closely (header/tabbar/content/footer, 70/30 split, sidebar summary, budget gauges, live session spinner).

---

## Test Coverage Assessment

### Well-tested
- `claude-common/models.rs`: Full coverage of `ModelType` (pricing, parsing, display, serde), `SessionStatus`, `BudgetConfig` defaults, `DataSource`.
- `claude-common/protocol.rs`: All RPC types serialized and deserialized; `RpcMethod::from_request` tested for all methods.
- `claude-common/errors.rs`: All `IpcError::to_rpc_error()` mappings tested; `From` implementations tested.
- `claude-common/paths.rs`: Env var override for socket path tested.
- `claude-daemon/storage.rs`: Insert/query, batch, dedup, filter, budget CRUD, sessions CRUD, model stats, daily cost — solid coverage.
- `claude-daemon/ipc.rs`: Async request dispatch for status, budget, usage query, and error cases.
- `claude-daemon/collector.rs`: Backoff function, log line parsing (valid, invalid, edge cases), UUID determinism.

### Missing / Insufficient Coverage
- **`claude-tui/app.rs`:** No tests at all. Key logic like `handle_key`, `compute_today_model_pcts`, `format_tokens_short`, tab navigation, and the data refresh flow are completely untested.
- **`claude-tui/client.rs`:** No tests. The IPC client's `call()` method, connection handling, and all typed methods are untested.
- **Widget rendering (`token_chart`, `cost_breakdown`, `model_compare`, `live_monitor`):** No tests. This is partially acceptable for rendering code (harder to unit test), but the helper functions (`format_tokens`, `format_duration_short`, etc.) could be tested independently.
- **`claude-daemon/collector.rs`:** `scan_log_directory` and `poll_api` are not tested. The full `Collector::run` loop is not tested (only helpers).
- **`claude-common/paths.rs`:** Only the `CLAUDE_DAEMON_SOCKET` override is tested. `XDG_RUNTIME_DIR`, `XDG_DATA_HOME`, `HOME`, `XDG_CONFIG_HOME` paths are not tested.
- **Integration tests:** No integration tests between the daemon and TUI via the socket protocol.

---

## Summary

| Severity | Count |
|----------|-------|
| Critical | 4     |
| Major    | 8     |
| Minor    | 13    |

The codebase is well-structured and shows strong fundamentals. The main concerns are around silent data corruption in row deserialization (C1), the misleading `is_connected()` stub (C4), and missing integration/unit tests for the TUI layer. The duplicate formatting utilities and dead code should be cleaned up before the project grows larger.
