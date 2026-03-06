# Rust Memory Safety Review

Reviewed all `.rs` files across `claude-common`, `claude-daemon`, and `claude-tui` crates.

---

## Critical (UB or data race potential)

### [U1] Holding tokio::Mutex across nested .await (potential deadlock)
- **File:** `crates/claude-daemon/src/ipc.rs:123-150`
- **Pattern:** `handle_request()` acquires `storage.lock().await` at line 123 and holds the `MutexGuard` for the entire match block. Inside `RpcMethod::Status` (line 150), it acquires a second lock: `collector_status.lock().await`. Both `storage` and `collector_status` are `Arc<Mutex<T>>`. Meanwhile, `collector.rs` acquires locks in the opposite order: `collector_status.lock().await` (line 96, 122, 136) then `storage.lock().await` (line 113, 151). This creates an **ABBA lock ordering** -- thread 1 holds storage and waits for collector_status, thread 2 holds collector_status and waits for storage.
- **Risk:** Deadlock under concurrent access (IPC request arrives while collector is updating).
- **Fix:** Either (a) drop the storage lock before acquiring collector_status in `handle_request`, (b) acquire collector_status first in `handle_request`, or (c) combine them into a single struct behind one Mutex. The simplest fix: read collector_status *before* locking storage in `handle_request`.

### [U2] `std::env::set_var`/`remove_var` are unsafe in Rust 2024 edition
- **File:** `crates/claude-common/src/paths.rs:96, 108-109`
- **Pattern:** `unsafe { std::env::set_var(key, value) }` and `unsafe { std::env::remove_var(&self.key) }` in test code. While correctly marked `unsafe`, these are unsound in multi-threaded test contexts since `cargo test` runs tests in parallel within the same process by default. Even with the comment "only used in serial unit tests", there is no enforcement (no `#[serial]` attribute).
- **Risk:** Data race if another test reads env vars concurrently. Low practical risk since the test only modifies an unusual env var, but technically UB.
- **Fix:** Use `serial_test` crate with `#[serial]` attribute, or use a `temp_env`-style crate, or run these tests with `-- --test-threads=1`.

---

## Warnings (not UB but problematic)

### [W1] Unbounded log file reads in collector
- **File:** `crates/claude-daemon/src/collector.rs:201-228` (`scan_log_directory`)
- **Pattern:** `scan_log_directory` reads every `.jsonl` and `.log` file completely into memory via `read_to_string`. There is no size limit. A large or malicious log file could cause OOM.
- **Risk:** Memory exhaustion if log files grow large (e.g., gigabytes of accumulated logs).
- **Fix:** Use `BufReader::lines()` for streaming, or add a max file size check, or track last-read position.

### [W2] Re-scanning all log files every poll cycle (no cursor tracking)
- **File:** `crates/claude-daemon/src/collector.rs:144-163`
- **Pattern:** Every poll interval (60s), `scan_log_directory` reads and parses ALL log files from scratch. Only UUID dedup in SQLite prevents duplicate inserts, but the parsing work grows linearly with log history.
- **Risk:** CPU waste and I/O amplification over time. Not a memory safety issue per se, but the unbounded Vec of records returned grows with log history.
- **Fix:** Track per-file offsets (seek position) to only read new data, or use inotify/kqueue for change detection.

### [W3] `unwrap_or_default()` silently swallows corrupt DB data
- **File:** `crates/claude-daemon/src/storage.rs:661-721` (`row_to_usage_record`, `row_to_session`)
- **Pattern:** These functions use `unwrap_or_default()` for nearly every column, including critical fields like UUID and timestamp. If the database is corrupt, the code silently returns records with invented UUIDs (new random v4) and timestamps (current time) that do not correspond to real data.
- **Risk:** Silent data corruption -- users would see fabricated data without any warning.
- **Fix:** Return `Result<T, StorageError>` from these functions and propagate errors. At minimum, log a warning when a fallback is used.

### [W4] `unwrap_or(ModelType::Sonnet)` default on unknown model strings
- **File:** `crates/claude-daemon/src/storage.rs:332, 390, 643, 675, 705`
- **Pattern:** Several places parse model strings with `.parse().unwrap_or(ModelType::Sonnet)`, silently converting any unknown model to Sonnet.
- **Risk:** If a new model type is added, historical data for that model would be misattributed to Sonnet in all aggregations and queries.
- **Fix:** Either return an error or use a dedicated `Unknown` variant.

### [W5] `std::process::exit(0)` in signal handler prevents Drop execution
- **File:** `crates/claude-daemon/src/main.rs:91`
- **Pattern:** The SIGINT/SIGTERM handler calls `std::process::exit(0)` which immediately terminates without running destructors. The socket file is manually cleaned up before exit, but any in-flight database transactions, WAL checkpoints, or other Drop implementations are skipped.
- **Risk:** SQLite WAL file may not be checkpointed, leading to data loss. The `Storage` struct's `Connection` won't be dropped cleanly.
- **Fix:** Use a cancellation token (e.g., `tokio_util::sync::CancellationToken`) to signal graceful shutdown instead of `process::exit`.

### [W6] `db_path.to_str().unwrap_or(":memory:")` silently falls back to in-memory DB
- **File:** `crates/claude-daemon/src/main.rs:35`
- **Pattern:** If `db_path` contains non-UTF8 characters, the daemon silently uses an in-memory database, losing all persistent data.
- **Risk:** Silent data loss on systems with non-UTF8 paths (rare but possible).
- **Fix:** Use `db_path.to_string_lossy()` with a warning log, or fail explicitly.

### [W7] No timeout on IPC client read_line
- **File:** `crates/claude-tui/src/client.rs:73-77`
- **Pattern:** `stream.read_line(&mut line).await` has no timeout. If the daemon hangs or sends a partial response, the TUI client blocks indefinitely.
- **Risk:** TUI hangs permanently if daemon becomes unresponsive.
- **Fix:** Wrap with `tokio::time::timeout()`.

### [W8] No request size limit on IPC server line reading
- **File:** `crates/claude-daemon/src/ipc.rs:86-88`
- **Pattern:** `reader.lines()` reads an unbounded line from the Unix socket. A malicious or buggy client could send a multi-gigabyte line.
- **Risk:** OOM from a single malicious client connection.
- **Fix:** Use `read_line` with a max buffer size check, or `take()` to limit bytes read.

---

## Suggestions (optimization/best practice)

### [S1] Unnecessary `.clone()` on `req.params` in `RpcMethod::from_request`
- **File:** `crates/claude-common/src/protocol.rs:206, 214, 222, 230, 239, 247`
- **Pattern:** `serde_json::from_value(req.params.clone())` clones the `serde_json::Value` each time. Since `from_request` takes `&RpcRequest`, the clone is necessary. However, the method could take ownership of `RpcRequest` instead (it is only called once per request in the dispatch).
- **Fix:** Change `from_request` to take `RpcRequest` by value: `pub fn from_request(req: RpcRequest)` and use `req.params` directly.

### [S2] `"2.0".to_string()` repeated in every RPC response
- **File:** `crates/claude-common/src/protocol.rs:30, 40`
- **Pattern:** `"2.0".to_string()` allocates a new String on every response construction.
- **Fix:** Minor -- consider using `Cow<'static, str>` or a const if this is hot path. Low priority.

### [S3] `format_tokens_short` duplicated between `app.rs` and `token_chart.rs`
- **File:** `crates/claude-tui/src/app.rs:535-543`, `crates/claude-tui/src/widgets/token_chart.rs:34-42`
- **Pattern:** Same function defined in two places.
- **Fix:** Extract to a shared utility module. Not a safety issue but a maintenance concern.

### [S4] `AtomicU64` with `Ordering::Relaxed` for RPC request IDs
- **File:** `crates/claude-tui/src/client.rs:49`
- **Pattern:** `self.next_id.fetch_add(1, Ordering::Relaxed)` -- this is fine for a monotonic counter where uniqueness matters but ordering does not. No issue here.

### [S5] `i64 as u64` casts in storage could silently truncate negative values
- **File:** `crates/claude-daemon/src/storage.rs:333-339, 676-678, 712-717`
- **Pattern:** `row.get::<_, i64>(N)? as u64` -- if SQLite stores a negative number, this wraps to a very large u64.
- **Risk:** Unlikely since token counts should never be negative, but a corrupted DB or future bug could cause silent wraparound.
- **Fix:** Use `u64::try_from(val).unwrap_or(0)` or clamp with `.max(0) as u64`.

### [S6] `compute_today_model_pcts` allocates intermediate Vec unnecessarily
- **File:** `crates/claude-tui/src/app.rs:545-575`
- **Pattern:** Creates an intermediate `Vec<&DailyAggregate>` via `collect()` then iterates it again. Could use iterators directly.
- **Fix:** Minor allocation optimization, not a safety issue.

---

## Clean Patterns (well done)

- **Panic hook for terminal restoration**: `crates/claude-tui/src/main.rs:20-25` correctly installs a panic hook that restores terminal raw mode, preventing terminal corruption on panic. This is a critical pattern for TUI apps and is done correctly.

- **No `unsafe` in production code** (except the trivially safe `libc::getuid()` call in `paths.rs:17`). The codebase is effectively 100% safe Rust.

- **No `static mut` or `lazy_static` with mutable state**: The codebase uses `Arc<Mutex<T>>` properly for shared state.

- **`saturating_*` arithmetic for backoff**: `crates/claude-daemon/src/collector.rs:177-181` uses `saturating_mul`, `saturating_pow`, and `saturating_add` to prevent overflow.

- **`wrapping_add` for spinner tick**: `crates/claude-tui/src/app.rs:134` uses `wrapping_add(1)` for the spinner counter to avoid overflow panic.

- **Proper error propagation**: The error hierarchy (`AppError` -> `CollectorError`/`StorageError`/`IpcError`) uses `thiserror` with `#[from]` for clean error conversion.

- **UUID-based dedup for log ingestion**: `crates/claude-daemon/src/collector.rs:322-344` generates deterministic UUIDs from log data, and storage uses `INSERT OR IGNORE` for dedup. This prevents duplicate records without needing cursor tracking (though tracking would be more efficient -- see W2).

- **tokio::Mutex (not std::Mutex) for async code**: The daemon correctly uses `tokio::sync::Mutex` rather than `std::sync::Mutex`, avoiding blocking the tokio runtime.

- **Input validation in log parser**: `crates/claude-daemon/src/collector.rs:278-286` rejects zero-token records and future timestamps, preventing garbage data ingestion.

- **Test code properly isolated**: All `unwrap()` and `expect()` calls in the codebase are within `#[cfg(test)]` modules, not in production paths. Production code uses `unwrap_or_default()`, `unwrap_or()`, or proper error handling.

- **`BudgetConfig` singleton pattern**: The budget table uses `CHECK (id = 1)` to enforce exactly one config row, preventing accidental duplication.

- **No circular `Arc`/`Rc` references**: All `Arc` usage is tree-structured (main -> storage/collector_status -> individual handlers), no cycles.
