# Phase A: model and price config (AC-5, AC-6, AC-7)

## Current state

- `crates/claude-common/src/models.rs`: `ModelType` is a closed enum {Opus, Sonnet, Haiku} with prices hardcoded in match arms (lines ~16-49) and `compute_cost` (~52-64). `UsageRecord` (~124-149) has `model: ModelType` and `cost_usd: f64`.
- `crates/claude-daemon/src/collector.rs:278-281` drops any record whose model does not parse into `ModelType`.
- `claude_common::paths::config_path()` exists but nothing loads a config file. `toml` is not a dependency yet.

## Progress from the first run (stopped by provider timeouts)

- AC-5 is done and committed (`feat(common): provider and subscription grouping (AC-5)`). Do not redo it. Fix one bug in it first, as part of AC-7's commit: `Provider` uses `#[serde(rename_all = "kebab-case")]`, which serialises `OpenAiCodex` as `open-ai-codex`, but `as_str()` says `openai-codex`. Add `#[serde(rename = "openai-codex")]` and a round-trip test.
- The workspace was reformatted with `cargo fmt` and committed. Start from AC-7.

## Do

1. **AC-5:** add `Provider` and `Subscription` (`Claude`, `ChatGpt`, `OpencodeGo`, `Other`) to claude-common. Add a pure function `subscription_for(source, provider) -> Subscription` that implements the AC-5 grouping:
   - Claude Code logs and the Claude quota snapshot map to `claude`;
   - the Codex CLI and Pi `openai-codex` map to `chatgpt`;
   - Pi `opencode-go` and OpenCode map to `opencode-go`;
   - anything else maps to `other`.
   Test every source in tests whose names contain `subscription_grouping`.
2. **AC-7:**
   - Add `toml` to the workspace and to claude-common.
   - Add `claude-common/src/config.rs` to load `[prices."<model-substring>"]` tables (input, output, cache_read, cache_write in USD per million tokens) from `config_path()`. When a model matches several keys, the longest key wins.
   - Add `CostSource {Reported, Computed, Unknown}`. Price lookup returns `Option`.
   - Remove the hardcoded price match arms.
   - Ship default prices in a data file `config/default-prices.toml` at the repository root, embedded with `include_str!` and used when the user config has no `[prices]` table. Fill it from the providers' **official pricing pages**, with a `# fetched: YYYY-MM-DD` comment and the source URL per provider. Cover:
     - the Claude 5.5 family (opus, sonnet, haiku);
     - the OpenAI models in `~/.pi/agent/settings.json` `enabledModels` (gpt-6.1-sol, gpt-6-sol, gpt-6-luna, gpt-6-astra, gpt-5.6-terra);
     - the opencode-go models (deepseek-v4-pro, deepseek-v4.1-flash, kimi-k3, glm-5.3, qwen3.8-max, minimax-m3).
   - If a price cannot be found on an official page, leave that model out (its cost becomes unknown) and list it in your report. Never guess a price.
   - Tests use names containing `price_config`.
3. **AC-6:**
   - Change `UsageRecord.model` to `String`.
   - Add `provider`, `subscription`, `cost_usd: Option<f64>`, `cost_source` and `dedup_key: String`.
   - A model with no configured price keeps its tokens and gets `cost_usd: None` with `CostSource::Unknown`. Remove the skip at `collector.rs:278-281`.
   - Keep `ModelType` only if the TUI still needs it for colours; otherwise delete it.
   - Update the existing call sites in claude-daemon, claude-tui and the tests so the workspace compiles. Storage columns for the new fields come in Phase B. Until then, map them so existing tests still pass.
   - Test name contains `unknown_model_kept` (in claude-daemon).

Commit order: AC-5, AC-7, AC-6.
