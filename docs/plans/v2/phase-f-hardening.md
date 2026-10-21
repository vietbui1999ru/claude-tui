# Phase F: hardening, review status, CI (AC-12, AC-16, AC-13)

## Progress from the first run (stopped by a provider 429)

- AC-12 is done and committed (`fix(daemon): owner-only socket and bounded IPC reads (AC-12)`), with 4 `ipc_hardening` tests. Do not redo it.
- AC-13 has started: some clippy fixes in `storage.rs`, `app.rs` and `mock.rs` are committed. Run `cargo clippy --workspace --all-targets -- -D warnings` to see what remains.
- Start at step 2 (AC-16), then step 3 (AC-13) and step 4.

## Do

1. **AC-12, `ipc_hardening`, in `crates/claude-daemon/src/ipc.rs`:**
   - Bind the socket with no permissive window: set `umask(0o077)` around `bind` and restore it afterwards (wrapped in one small `unsafe` libc call with a `// SAFETY:` comment), then `set_permissions(0o600)` with the error propagated, not `.ok()`.
   - Replace `lines.next_line()` with a bounded read: read through `take(64 * 1024 + 1)` or `read_until` on a limited reader. If a line exceeds 64 KiB, reply with a JSON-RPC invalid-request error and close the connection, without buffering the rest.
   - Apply the same cap to `claude-tui/src/client.rs` `read_line`.
   - Tests check the socket mode is 0600, that a 100 KiB request is rejected, and that peak buffering stays bounded (the reader never holds more than the cap).
2. **AC-16:** write `docs/plans/review-status.md`. It has one row per Critical finding in `docs/plans/code-review.md` (C1-C4), `devils-advocate-review.md` (V1-V5) and `memory-safety-review.md` (U1, U2, plus W8). Each row gives the status (fixed before v2 / fixed in v2 / deferred), the evidence as a current `file:line` or test name, and a reason if deferred. Read each finding against the current code; do not trust any summary.
3. **AC-13:**
   - Add the steps `cargo fmt --all --check` and `cargo clippy --workspace --all-targets -- -D warnings` to `.github/workflows/rust.yml`, after checkout and before the tests.
   - Make `--workspace` explicit on the build and test steps.
   - Make sure fmt and clippy are clean locally.
4. **Repository tidy, part of AC-13's commit:**
   - Change `scripts/sketchybar/claude_plugin.sh` to call `claude-tui status --line` instead of socat, jq and bc.
   - Fix the stale `~/repos/RustProjects/claude-tui` prefix in `.claude/settings.local.json`.
