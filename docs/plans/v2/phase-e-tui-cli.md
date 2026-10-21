# Phase E: CLI and TUI Overview (AC-10, AC-18, AC-11)

## Current state

- `crates/claude-tui/src/main.rs` has no argument parsing. If the daemon connection fails it prints "starting in mock mode".
- `app.rs`: every field starts as mock data (~lines 95-101), and mock data replaces real data on any failure (~203-224). `Tab {Tokens, Costs, Models, Live}`.
- `client.rs`: `DaemonClient` with a 10s timeout. `reconnect` is dead code.

## Do

1. **Argument parsing:** hand-rolled `std::env::args`, with no new dependency. Accepted forms:
   - `claude-tui` and `claude-tui --demo`;
   - `claude-tui status --line` and `claude-tui status --json`.
   An unknown argument prints usage and exits with code 2.
2. **AC-10, `status_line`:**
   - Add a new module `status_line.rs` with a pure `fn format_line(&StatusResponse, now) -> String`. Example: `C 5h 34% 7d 12% · X 5h 3% 7d 8% · G $3.20 ⚠limit 14:05`.
   - `C` is claude, `X` is chatgpt (its primary and secondary windows), and `G` is opencode-go (today's `≈` cost, plus `⚠limit HH:MM` when the last hit is under 24h old). Mark stale values with `?` (e.g. `5h 34%?`).
   - With the daemon down, print `usage: daemon down`. Always exit 0.
   - Use a 2s connect and read timeout for this path.
3. **AC-18, `status_json_schema`:**
   - `status --json` prints `{"v":1,"subscriptions":{"claude":{...},"chatgpt":{...},"opencode-go":{...}},"generated_at":"..."}`, built from the AC-9 data. With the daemon down it prints `{"v":1,"daemon":"down"}`. Always exit 0.
   - Tests deserialize the output and check the keys.
   - Document the schema in `docs/status-json.md`. It is the contract for the Pi extensions.
4. **AC-11, `no_mock_without_demo`:**
   - The default tab becomes a new `Overview`, with one card per subscription: quota gauges with reset times, today and 7d tokens and cost (`≈ API $` when the cost source is Computed), last limit hit, and freshness or stale markers.
   - Mock data is used only with `--demo`. Without the daemon, the TUI shows a "daemon unreachable — start `claude-daemon`" panel and retries with `reconnect` on each refresh tick.
   - The test builds `App` without demo and with a failing client, and asserts that no mock values are present.
   - Keep the existing tabs working on real data where the daemon provides it. Where it doesn't, show "no data" rather than mock values.
