# claude-tui v2: rules for every phase

You are implementing one phase of an approved spec in a Rust workspace. You are already in the repository, on branch `feat/multi-provider-usage`. Do not switch branches.

## Read first

1. `SPEC.md` at the repository root: the whole file, especially Boundaries and the ACs named in your phase.
2. Your phase section below, which names the files to touch.
3. The existing code you are about to change. Match its style: `thiserror` errors per crate, `tracing` for logs, inline `#[cfg(test)]` modules, and no `unwrap()` or `expect()` outside tests.

## Hard rules

- Work only toward the AC ids your phase names. Do not start the next phase.
- Every collector opens provider data read-only. Never write under `~/.claude`, `~/.codex`, `~/.pi` or `~/.local/share/opencode`.
- Never read credential files (`~/.codex/auth.json`, `~/.pi/agent/auth.json`, `~/.local/share/opencode/auth.json`) or OpenCode's `credential`, `account` and `control_account` tables.
- Never store prompt or response text, only usage metadata.
- Tests must not touch the real home directory. Use fixtures under `crates/claude-daemon/tests/fixtures/<provider>/` (trimmed, anonymised, no real prompts or paths) or temp dirs, and `:memory:` SQLite.
- Test names must contain the AC's `verify:` filter string (for example a test named `codex_rollout_reads_rate_limits` for the filter `codex_rollout`), so `cargo test -p <crate> <filter>` runs at least one test.
- Never edit a `verify:` or `baseline:` line in SPEC.md, and never weaken an existing test to make it pass.
- Never run `ac-run approve` or `agent-review approve`.
- New dependencies: only `toml` is pre-approved. Anything else, stop and say so in your final message instead of adding it.

## Each AC

1. Write the failing test first, then the code.
2. Run `cargo test --workspace` and `cargo fmt --all` until both are clean. Run `cargo clippy --workspace --all-targets` and add no new warnings in code you write; the existing warnings are AC-13's job (Phase F), so leave them unless your phase is F.
3. Commit only the files you changed, staged by path (never `git add -A`). Use a conventional commit naming the AC, e.g. `feat(daemon): codex rollout collector (AC-1)`. End the message with `Co-Authored-By: Pi <noreply@pi.dev>`.
4. Do not push.

## Finish

End with a short report: each AC id with its commit hash, test names added, anything deferred and why. If a check fails after three fix attempts, stop and report the exact failing command and its output.
