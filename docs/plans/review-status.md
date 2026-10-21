# Review status (v2)

Status of every Critical finding in the three pre-v2 reviews, read against the
code on `feat/multi-provider-usage` (2026-10-21) rather than against any
summary. Line numbers are for this branch; the review files keep their own.

- **fixed before v2** — already fixed in the scaffold commit history, recorded
  here with the line that shows the fix.
- **fixed in v2** — fixed by v2 itself (AC-12 for the IPC pair).
- **deferred** — not fixed, with a reason.

| Finding | Review claim | Status | Evidence |
| --- | --- | --- | --- |
| C1 | `row_to_usage_record` swallowed every column error with `unwrap_or_default`/`unwrap_or(0)`, so a schema shift produced wrong data silently | fixed in v2 | `crates/claude-daemon/src/storage.rs:1341` returns `rusqlite::Result<UsageRecord>` and reads every numeric and timestamp column with `?`; `row_to_session` does the same at `storage.rs:1462`. Tests: `code_review_c1_corrupt_row_fails_the_query` (`storage.rs:1708`), `code_review_c1_unknown_labels_read_as_unknown` (`storage.rs:1732`) |
| C2 | `insert_usage_batch` used `unchecked_transaction()` | fixed before v2 | `storage.rs:279` takes `&mut self`; `storage.rs:282` uses `conn.transaction()`, so the borrow checker enforces one writer and a mid-batch error rolls back through `Drop` |
| C3 | `db_path.to_str().unwrap_or(":memory:")` silently lost all data on a non-UTF-8 path | fixed before v2 | `crates/claude-daemon/src/main.rs:37` matches on `to_str()`; the `None` arm logs the path and exits 1 instead of opening a throwaway database |
| C4 | `DaemonClient::is_connected()` always returned `true` | fixed before v2 | `crates/claude-tui/src/client.rs:68` reports whether a live stream is held (`try_lock` + `is_some`). Test: `reconnect_after_connection_loss` (`client.rs:260`) asserts both the `false` and `true` states |
| V1 | SIGINT/SIGTERM handler called `std::process::exit(0)`, racing an in-flight SQLite write | fixed before v2 | `main.rs:59` creates a `CancellationToken`; the handler at `main.rs:81-99` only cancels it. `main` then awaits the collector (`main.rs:107`) and drops `Storage` on return (`main.rs:114`), so the connection is closed and the WAL checkpointed |
| V2 | Same as C2: `unchecked_transaction` for batch inserts | fixed before v2 | `storage.rs:282` — see C2 |
| V3 | Socket file readable by other local users, with a window between bind and chmod | fixed in v2 | `crates/claude-daemon/src/ipc.rs:87` (`bind_private`) binds under a `0o077` umask and restores it at `ipc.rs:99`, then chmods to 0600 with the error propagated (`ipc.rs:102-104`). Test: `ipc_hardening_socket_is_owner_only` (`ipc.rs:470`) |
| V4 | Unbounded `lines()` read: a gigabyte line from a local client OOMs the daemon | fixed in v2 | `ipc.rs:38` (`read_request_line`) reads through `take(MAX_MESSAGE_LEN + 1)`; an unterminated line over the cap becomes `RequestLine::TooLong` and is answered with an invalid-request error before the connection closes (`ipc.rs:215-235`), without buffering the rest. Tests: `ipc_hardening_reads_lines_up_to_the_cap` (`ipc.rs:495`), `ipc_hardening_oversized_line_buffering_is_bounded` (`ipc.rs:517`), `ipc_hardening_rejects_a_100_kib_request` (`ipc.rs:538`) |
| V5 | Client `read_line` had no timeout, so the TUI could hang forever | fixed before v2 | `client.rs:105` wraps the read in `timeout(self.read_timeout, …)` and drops the stream on expiry, returning `IpcError::Timeout` (`client.rs:118-124`); the default is 10s (`client.rs:15`, `client.rs:52`). The same budget covers the response cap (`client.rs:103-105`), tested by `ipc_hardening_rejects_an_oversized_response` (`client.rs:233`) |
| U1 | ABBA deadlock: `handle_request` took `storage` then `collector_status`, the collector took them in the opposite order | fixed before v2 | `ipc.rs:269-272` reads `collector_status` before locking `storage`; the collector never holds both at once (`crates/claude-daemon/src/collectors/mod.rs:496` publishes status, `:511` then locks storage) |
| U2 | `env::set_var` in tests is unsound without serialisation | fixed before v2 | `crates/claude-common/src/paths.rs:67` imports `serial_test::serial`; the env-reading and env-writing tests are `#[serial]` (`paths.rs:70`, `:77`, `:84`). Dev-dependency at `crates/claude-common/Cargo.toml:16` |
| W8 | No request size limit on the IPC server's line read | fixed in v2 | `ipc.rs:38-62` — see V4; the client applies the same cap in the other direction (`client.rs:103-105`) |

## Residue named by AC-16

C1 also asked for errors where errors are meaningful. The row reader now
propagates them, with one deliberate exception: **label** columns (`provider`,
`subscription`, `cost_source`, `source`) are read as an explicit unknown variant
when the string is unrecognised (`storage.rs:1371-1384`), because a label from a
newer release is not corruption and must not fail a whole query. Numeric and
timestamp columns are strict. Covered by
`code_review_c1_unknown_labels_read_as_unknown`.

The `unwrap_or(ModelType::Sonnet)` fallbacks (review M2, memory-safety W4) are
gone: `UsageRecord.model` is a plain `String` now, so an unpriced or unknown
model keeps its own name and only its cost becomes unknown (AC-6). The one
remaining `ModelType` parse, `row_to_session` (`storage.rs:1462-1477`), propagates
the parse error as a `rusqlite` conversion failure instead of substituting
Sonnet.

## Not covered here

The reviews' Major and Minor findings stay out of scope for v2 (AC-16 covers the
Critical list only). Two are visible in the code and worth a follow-up: the
`daily_aggregates` rollup is still never scheduled (M8, D3), and `budget.set`
still stores an unvalidated limit (E9).
