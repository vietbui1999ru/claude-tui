//! Price configuration.
//!
//! Prices live in `~/.config/claude-daemon/config.toml` as
//! `[prices."<model-substring>"]` tables, in USD per million tokens. The
//! repository ships [`EMBEDDED_DEFAULT_PRICES`] as a fallback for a user
//! config with no `[prices]` table. A model that matches no key has no known
//! price: its tokens are still counted and its cost is reported as unknown.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::errors::ConfigError;

/// Default prices shipped with the binary and used when the user config has no
/// `[prices]` table.
pub const EMBEDDED_DEFAULT_PRICES: &str = include_str!("../../../config/default-prices.toml");

/// USD prices per million tokens for one model family.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct Prices {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

impl Prices {
    /// Cost in USD for a token breakdown.
    pub fn cost(
        &self,
        input_tokens: u64,
        output_tokens: u64,
        cache_read_tokens: u64,
        cache_write_tokens: u64,
    ) -> f64 {
        let input = input_tokens as f64 * self.input;
        let output = output_tokens as f64 * self.output;
        let cache_read = cache_read_tokens as f64 * self.cache_read;
        let cache_write = cache_write_tokens as f64 * self.cache_write;
        (input + output + cache_read + cache_write) / 1_000_000.0
    }
}

/// The `[prices]` table of the config file.
#[derive(Debug, Default, Deserialize)]
struct PriceTables {
    #[serde(default)]
    prices: BTreeMap<String, Prices>,
}

/// Model substring to price lookup.
///
/// Matching is a case-insensitive substring test against the model id; the
/// longest matching key wins, so a specific model key overrides a broader one.
#[derive(Debug, Clone, Default)]
pub struct PriceBook {
    prices: BTreeMap<String, Prices>,
}

impl PriceBook {
    /// Load the user config at [`crate::paths::config_path`].
    ///
    /// A missing file, or a file without a `[prices]` table, falls back to the
    /// embedded defaults.
    pub fn load() -> Result<Self, ConfigError> {
        Self::load_from(&crate::paths::config_path())
    }

    /// Load the config at `path`, with the same fallback as [`Self::load`].
    pub fn load_from(path: &Path) -> Result<Self, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::from_toml_str(&text, &path.display().to_string()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::embedded(),
            Err(e) => Err(ConfigError::Read {
                path: path.display().to_string(),
                reason: e.to_string(),
            }),
        }
    }

    /// Parse a config document. An empty `[prices]` table means "no configured
    /// prices", so the embedded defaults apply.
    pub fn from_toml_str(text: &str, origin: &str) -> Result<Self, ConfigError> {
        let prices = parse_price_tables(text, origin)?;
        if prices.is_empty() {
            return Self::embedded();
        }
        Ok(Self { prices })
    }

    /// The embedded defaults from `config/default-prices.toml`.
    pub fn embedded() -> Result<Self, ConfigError> {
        Ok(Self {
            prices: parse_price_tables(EMBEDDED_DEFAULT_PRICES, "embedded default prices")?,
        })
    }

    /// Price for a model id, or `None` when no configured key matches.
    pub fn lookup(&self, model: &str) -> Option<&Prices> {
        let model = model.to_lowercase();
        self.prices
            .iter()
            .filter(|(key, _)| model.contains(&key.to_lowercase()))
            .max_by_key(|(key, _)| key.len())
            .map(|(_, prices)| prices)
    }

    /// Computed cost in USD, or `None` when the model has no configured price.
    pub fn cost(
        &self,
        model: &str,
        input_tokens: u64,
        output_tokens: u64,
        cache_read_tokens: u64,
        cache_write_tokens: u64,
    ) -> Option<f64> {
        self.lookup(model).map(|prices| {
            prices.cost(
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_write_tokens,
            )
        })
    }
}

fn parse_price_tables(text: &str, origin: &str) -> Result<BTreeMap<String, Prices>, ConfigError> {
    let parsed: PriceTables = toml::from_str(text).map_err(|e| ConfigError::Parse {
        path: origin.to_string(),
        reason: e.to_string(),
    })?;
    Ok(parsed.prices)
}

/// The non-`[prices]` keys of the daemon config file that this crate reads.
#[derive(Debug, Default, Deserialize)]
struct DaemonKeys {
    #[serde(default)]
    claude_status_path: Option<PathBuf>,
}

/// The `claude_status_path` set in [`crate::paths::config_path`], if any.
pub fn claude_status_path_override() -> Option<PathBuf> {
    claude_status_path_override_from(&crate::paths::config_path())
}

/// The `claude_status_path` set in the config file at `path`, if any.
///
/// A missing, unreadable or malformed file means no override, so the caller
/// falls back to the default snapshot path. A config error is already reported
/// when the prices are loaded; this key is optional by design.
pub fn claude_status_path_override_from(path: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(path).ok()?;
    let keys: DaemonKeys = toml::from_str(&text).ok()?;
    keys.claude_status_path
        .filter(|path| !path.as_os_str().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_prices() -> PriceBook {
        PriceBook::embedded().expect("embedded default prices must parse")
    }

    fn write_temp_config(contents: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "claude-tui-price-config-{}.toml",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&path, contents).expect("write temp config");
        path
    }

    #[test]
    fn price_config_longest_model_key_wins() {
        let book = PriceBook::from_toml_str(
            r#"
            [prices."claude"]
            input = 1.0
            output = 2.0
            cache_read = 0.1
            cache_write = 1.0

            [prices."claude-opus-5-5"]
            input = 4.0
            output = 20.0
            cache_read = 0.20
            cache_write = 5.0
            "#,
            "test",
        )
        .expect("config must parse");

        let prices = book
            .lookup("claude-opus-5-5-20260901")
            .expect("specific key");
        assert!((prices.input - 4.0).abs() < 1e-10);
        let generic = book.lookup("claude-haiku-5-5").expect("generic key");
        assert!((generic.input - 1.0).abs() < 1e-10);
    }

    #[test]
    fn price_config_unknown_model_has_no_price() {
        let book = default_prices();
        assert!(book.lookup("some-unlisted-model").is_none());
        assert!(
            book.cost("some-unlisted-model", 1_000, 1_000, 0, 0)
                .is_none()
        );
    }

    #[test]
    fn price_config_model_match_is_case_insensitive_substring() {
        let book = default_prices();
        assert!(book.lookup("OPENAI-CODEX/GPT-6.1-SOL").is_some());
        assert!(book.lookup("claude-sonnet-5-5-20260901").is_some());
        assert!(book.lookup("opencode-go/minimax-m3").is_some());
    }

    #[test]
    fn price_config_embedded_defaults_cover_required_models() {
        let book = default_prices();
        for model in [
            "claude-opus-5-5",
            "claude-sonnet-5-5",
            "claude-haiku-5-5",
            "gpt-6.1-sol",
            "gpt-6-sol",
            "gpt-6-luna",
            "gpt-6-astra",
            "gpt-5.6-terra",
            "kimi-k3",
            "glm-5.3",
            "qwen3.8-max",
            "minimax-m3",
        ] {
            assert!(book.lookup(model).is_some(), "missing price for {model}");
        }
    }

    #[test]
    fn price_config_computes_cost_for_opus_defaults() {
        let book = default_prices();
        // input 1M * 4.0, output 500k * 20/1M = 10.0,
        // cache read 200k * 0.20/1M = 0.04, cache write 100k * 5/1M = 0.5
        let cost = book
            .cost("claude-opus-5-5", 1_000_000, 500_000, 200_000, 100_000)
            .expect("opus 5.5 must be priced");
        assert!((cost - 14.54).abs() < 1e-10, "got {cost}");
    }

    #[test]
    fn price_config_computes_cost_for_sonnet_defaults() {
        let book = default_prices();
        // input 1M * 2.0 + output 1M * 10.0
        let cost = book
            .cost("claude-sonnet-5-5", 1_000_000, 1_000_000, 0, 0)
            .expect("sonnet 5.5 must be priced");
        assert!((cost - 12.0).abs() < 1e-10, "got {cost}");
    }

    #[test]
    fn price_config_computes_cost_for_haiku_defaults() {
        let book = default_prices();
        // input 500k * 0.10/1M = 0.05, output 250k * 0.50/1M = 0.125,
        // cache read 100k * 0.01/1M = 0.001, cache write 50k * 0.125/1M = 0.00625
        let cost = book
            .cost("claude-haiku-5-5", 500_000, 250_000, 100_000, 50_000)
            .expect("haiku 5.5 must be priced");
        assert!((cost - 0.18225).abs() < 1e-10, "got {cost}");
    }

    #[test]
    fn price_config_zero_tokens_cost_nothing() {
        let book = default_prices();
        let cost = book
            .cost("claude-sonnet-5-5", 0, 0, 0, 0)
            .expect("sonnet 5.5 must be priced");
        assert!(cost.abs() < 1e-10);
    }

    #[test]
    fn price_config_user_prices_replace_defaults() {
        let path = write_temp_config(
            r#"
            [prices."only-model"]
            input = 1.0
            output = 1.0
            cache_read = 1.0
            cache_write = 1.0
            "#,
        );
        let book = PriceBook::load_from(&path).expect("config must load");
        std::fs::remove_file(&path).ok();

        assert!(book.lookup("only-model").is_some());
        // The user table replaces the defaults rather than merging into them.
        assert!(book.lookup("claude-opus-5-5").is_none());
    }

    #[test]
    fn price_config_missing_prices_table_uses_defaults() {
        let path = write_temp_config("# no prices here\n");
        let book = PriceBook::load_from(&path).expect("config must load");
        std::fs::remove_file(&path).ok();

        assert!(book.lookup("claude-opus-5-5").is_some());
    }

    #[test]
    fn price_config_missing_file_uses_defaults() {
        let path = std::env::temp_dir().join(format!(
            "claude-tui-price-config-absent-{}.toml",
            uuid::Uuid::new_v4()
        ));
        let book = PriceBook::load_from(&path).expect("missing file falls back to defaults");
        assert!(book.lookup("claude-haiku-5-5").is_some());
    }

    #[test]
    fn price_config_malformed_toml_is_an_error() {
        let err = PriceBook::from_toml_str("[prices.\"broken\"\ninput = 1.0", "test")
            .expect_err("malformed TOML must error");
        assert!(err.to_string().contains("config"));
    }

    #[test]
    fn config_claude_status_path_override_is_read() {
        let path = write_temp_config(
            r#"
            claude_status_path = "/tmp/claude-status-from-config.json"

            [prices."claude"]
            input = 1.0
            output = 2.0
            cache_read = 0.1
            cache_write = 1.0
            "#,
        );
        let override_path = claude_status_path_override_from(&path);
        std::fs::remove_file(&path).ok();

        assert_eq!(
            override_path,
            Some(PathBuf::from("/tmp/claude-status-from-config.json"))
        );
    }

    #[test]
    fn config_claude_status_path_absent_is_no_override() {
        let path = write_temp_config(
            r#"
            [prices."claude"]
            input = 1.0
            output = 2.0
            cache_read = 0.1
            cache_write = 1.0
            "#,
        );
        let override_path = claude_status_path_override_from(&path);
        std::fs::remove_file(&path).ok();

        assert_eq!(override_path, None);
    }

    #[test]
    fn config_claude_status_path_missing_file_is_no_override() {
        let path = std::env::temp_dir().join(format!(
            "claude-tui-config-absent-{}.toml",
            uuid::Uuid::new_v4()
        ));
        assert_eq!(claude_status_path_override_from(&path), None);
    }
}
