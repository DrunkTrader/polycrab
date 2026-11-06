//! Configuration loading and types.
//!
//! Two-file split:
//! - `config.json` — public settings (committed)
//! - `config.yaml` — credentials (gitignored, must never be committed)

use alloy_primitives::Address;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::Path;
use std::str::FromStr;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub bot: BotConfig,
    pub site: SiteConfig,
    pub strategy: StrategyConfig,
    pub trading: TradingConfig,
    pub risk: RiskConfig,
    pub exchange: ExchangeConfig,

    /// Market eligibility filter (allowlist + blocklist by category, tag, slug).
    #[serde(default)]
    pub filters: FiltersConfig,

    /// Take-profit / stop-loss configuration.
    #[serde(default)]
    pub tp_sl: TpSlConfig,

    /// Loaded from `config.yaml`. Not serialised back out.
    #[serde(skip)]
    pub credentials: Credentials,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotConfig {
    pub wallets_to_track: Vec<String>,

    /// When `false`, decisions are computed but no orders are sent.
    #[serde(default)]
    pub enable_trading: bool,

    /// When `true`, every order path returns early with a log line.
    /// Independent of `enable_trading` — both must be permissive for live trades.
    #[serde(default = "default_true")]
    pub mock_trading: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SiteConfig {
    pub gamma_api_base: String,
    pub clob_api_base: String,
    pub polygon_ws_url: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum CopyStrategy {
    Percentage,
    Fixed,
    Adaptive,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyConfig {
    pub copy_strategy: CopyStrategy,

    /// For PERCENTAGE: percent of whale notional (e.g. 20.0 = 20%).
    /// For FIXED: USD per copy leg.
    /// For ADAPTIVE: ignored (uses adaptive_* below).
    pub copy_size: f64,

    pub trade_multiplier: f64,
    pub min_order_size_usd: f64,
    pub max_order_size_usd: f64,
    pub min_whale_shares_to_copy: f64,
    pub adaptive_threshold_usd: f64,
    pub adaptive_min_percent: f64,
    pub adaptive_max_percent: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradingConfig {
    /// Slippage tolerance applied to the order limit price (fractional, e.g. 0.005 = 0.5¢ at $1 scale).
    pub price_buffer: f64,

    pub fee_rate_bps: u32,
    pub order_expiration_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskConfig {
    pub large_trade_shares: f64,
    pub consecutive_trigger: u32,
    pub sequence_window_secs: u64,
    pub trip_duration_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExchangeConfig {
    pub ctf_exchange_address: String,
    pub neg_risk_exchange_address: String,
    pub chain_id: u64,
    pub domain_name: String,
    pub domain_version: String,
}

/// Market eligibility filter.
///
/// Behavior:
/// 1. Block lists always win — anything matching is skipped.
/// 2. If *any* allow list is non-empty, the market must match at least one
///    allow rule for ANY of (slug / category / tag) to be eligible.
/// 3. If all allow lists are empty, the bot falls back to allow-everything-
///    minus-blocklist semantics (so users who haven't curated yet aren't
///    stranded with zero trades).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FiltersConfig {
    #[serde(default)]
    pub slug_allow: Vec<String>,
    #[serde(default)]
    pub slug_block: Vec<String>,
    #[serde(default)]
    pub categories_allow: Vec<String>,
    #[serde(default)]
    pub categories_block: Vec<String>,
    #[serde(default)]
    pub tags_allow: Vec<String>,
    #[serde(default)]
    pub tags_block: Vec<String>,

    /// Per-category cap on simultaneously open USD notional.
    /// e.g. `{ "Politics": 500, "Sports": 300 }`.
    #[serde(default)]
    pub per_category_max_open_usd: std::collections::HashMap<String, f64>,

    /// Per-tag cap on simultaneously open USD notional.
    #[serde(default)]
    pub per_tag_max_open_usd: std::collections::HashMap<String, f64>,
}

impl FiltersConfig {
    /// True when at least one allow list is populated — strict allowlist mode.
    pub fn is_strict(&self) -> bool {
        !self.slug_allow.is_empty()
            || !self.categories_allow.is_empty()
            || !self.tags_allow.is_empty()
    }
}

/// Take-profit / stop-loss config (percent of entry price).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TpSlConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_tp_pct")]
    pub default_take_profit_pct: f64,
    #[serde(default = "default_sl_pct")]
    pub default_stop_loss_pct: f64,
    #[serde(default)]
    pub per_category_tp_pct: std::collections::HashMap<String, f64>,
    #[serde(default)]
    pub per_category_sl_pct: std::collections::HashMap<String, f64>,
    #[serde(default = "default_monitor_secs")]
    pub poll_interval_secs: u64,
}

impl Default for TpSlConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            default_take_profit_pct: 50.0,
            default_stop_loss_pct: 30.0,
            per_category_tp_pct: Default::default(),
            per_category_sl_pct: Default::default(),
            poll_interval_secs: 15,
        }
    }
}

fn default_tp_pct() -> f64 {
    50.0
}
fn default_sl_pct() -> f64 {
    30.0
}
fn default_monitor_secs() -> u64 {
    15
}

#[derive(Clone, Default)]
pub struct Credentials {
    pub private_key: String,
    pub funder_address: String,
    pub api_key: Option<String>,
    pub api_secret: Option<String>,
    pub api_passphrase: Option<String>,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("private_key", &redacted_presence(&self.private_key))
            .field("funder_address", &redacted_presence(&self.funder_address))
            .field("api_key", &redacted_option(&self.api_key))
            .field("api_secret", &redacted_option(&self.api_secret))
            .field("api_passphrase", &redacted_option(&self.api_passphrase))
            .finish()
    }
}

#[derive(Deserialize)]
struct CredentialsFile {
    bot: CredentialsBot,
}

#[derive(Deserialize)]
struct CredentialsBot {
    private_key: String,
    funder_address: String,
    api_key: Option<String>,
    api_secret: Option<String>,
    api_passphrase: Option<String>,
}

impl AppConfig {
    pub fn load(config_path: &Path, credentials_path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(config_path)
            .with_context(|| format!("reading public config from {}", config_path.display()))?;
        let mut cfg: AppConfig = serde_json::from_str(&raw).context("parsing config.json")?;

        // Credentials file is optional — without it we can still run in mock mode.
        if credentials_path.exists() {
            let raw = std::fs::read_to_string(credentials_path).with_context(|| {
                format!("reading credentials from {}", credentials_path.display())
            })?;
            let parsed: CredentialsFile =
                serde_yaml::from_str(&raw).context("parsing config.yaml")?;
            cfg.credentials = Credentials {
                private_key: parsed.bot.private_key,
                funder_address: parsed.bot.funder_address,
                api_key: parsed.bot.api_key,
                api_secret: parsed.bot.api_secret,
                api_passphrase: parsed.bot.api_passphrase,
            };
        }

        // Environment-variable overrides (handy for CI / Docker).
        if let Ok(key) = std::env::var("PM_PRIVATE_KEY") {
            cfg.credentials.private_key = key;
        }
        if let Ok(addr) = std::env::var("PM_FUNDER_ADDRESS") {
            cfg.credentials.funder_address = addr;
        }

        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<()> {
        if self.bot.wallets_to_track.is_empty() {
            anyhow::bail!("config.bot.wallets_to_track must contain at least one address");
        }
        for wallet in &self.bot.wallets_to_track {
            validate_address(wallet, "config.bot.wallets_to_track")?;
        }
        validate_endpoint(
            &self.site.gamma_api_base,
            &["http", "https"],
            "site.gamma_api_base",
        )?;
        validate_endpoint(
            &self.site.clob_api_base,
            &["http", "https"],
            "site.clob_api_base",
        )?;
        validate_endpoint(
            &self.site.polygon_ws_url,
            &["ws", "wss"],
            "site.polygon_ws_url",
        )?;
        validate_address(
            &self.exchange.ctf_exchange_address,
            "exchange.ctf_exchange_address",
        )?;
        validate_address(
            &self.exchange.neg_risk_exchange_address,
            "exchange.neg_risk_exchange_address",
        )?;

        finite_nonnegative(self.strategy.copy_size, "strategy.copy_size")?;
        finite_nonnegative(self.strategy.trade_multiplier, "strategy.trade_multiplier")?;
        finite_nonnegative(
            self.strategy.min_order_size_usd,
            "strategy.min_order_size_usd",
        )?;
        finite_nonnegative(
            self.strategy.max_order_size_usd,
            "strategy.max_order_size_usd",
        )?;
        if self.strategy.min_order_size_usd > self.strategy.max_order_size_usd {
            anyhow::bail!("strategy.min_order_size_usd must not exceed max_order_size_usd");
        }
        finite_nonnegative(
            self.strategy.min_whale_shares_to_copy,
            "strategy.min_whale_shares_to_copy",
        )?;
        finite_nonnegative(
            self.strategy.adaptive_threshold_usd,
            "strategy.adaptive_threshold_usd",
        )?;
        finite_nonnegative(
            self.strategy.adaptive_min_percent,
            "strategy.adaptive_min_percent",
        )?;
        finite_nonnegative(
            self.strategy.adaptive_max_percent,
            "strategy.adaptive_max_percent",
        )?;
        if self.strategy.adaptive_min_percent > self.strategy.adaptive_max_percent {
            anyhow::bail!("strategy.adaptive_min_percent must not exceed adaptive_max_percent");
        }

        finite_nonnegative(self.trading.price_buffer, "trading.price_buffer")?;
        finite_nonnegative(self.risk.large_trade_shares, "risk.large_trade_shares")?;
        if self.risk.consecutive_trigger == 0 {
            anyhow::bail!("risk.consecutive_trigger must be greater than zero");
        }
        if self.risk.sequence_window_secs == 0 || self.risk.trip_duration_secs == 0 {
            anyhow::bail!("risk sequence and trip durations must be greater than zero");
        }
        // Zero would make `pnl >= take_profit` (or `pnl <= -stop_loss`) true on
        // the first quote, closing every position as soon as it opens.
        finite_positive(
            self.tp_sl.default_take_profit_pct,
            "tp_sl.default_take_profit_pct",
        )?;
        finite_positive(
            self.tp_sl.default_stop_loss_pct,
            "tp_sl.default_stop_loss_pct",
        )?;
        if self.tp_sl.poll_interval_secs == 0 {
            anyhow::bail!("tp_sl.poll_interval_secs must be greater than zero");
        }
        for (name, values) in [
            ("tp_sl.per_category_tp_pct", &self.tp_sl.per_category_tp_pct),
            ("tp_sl.per_category_sl_pct", &self.tp_sl.per_category_sl_pct),
        ] {
            for value in values.values() {
                finite_positive(*value, name)?;
            }
        }
        for (name, values) in [
            (
                "filters.per_category_max_open_usd",
                &self.filters.per_category_max_open_usd,
            ),
            (
                "filters.per_tag_max_open_usd",
                &self.filters.per_tag_max_open_usd,
            ),
        ] {
            for value in values.values() {
                finite_nonnegative(*value, name)?;
            }
        }
        Ok(())
    }

    pub fn validate_live_credentials(&self) -> Result<()> {
        let key = self.credentials.private_key.trim().trim_start_matches("0x");
        if key.len() != 64 || hex::decode(key).is_err() {
            anyhow::bail!("live credentials contain an invalid private key");
        }
        validate_address(
            &self.credentials.funder_address,
            "live credentials funder address",
        )?;
        for (name, value) in [
            ("api_key", &self.credentials.api_key),
            ("api_secret", &self.credentials.api_secret),
            ("api_passphrase", &self.credentials.api_passphrase),
        ] {
            if value
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .is_none()
            {
                anyhow::bail!("live credentials missing {name}");
            }
        }
        Ok(())
    }

    /// Whether the operator has explicitly requested live execution.
    pub fn live_trading_allowed(&self) -> bool {
        self.bot.enable_trading && !self.bot.mock_trading
    }

    /// Live execution stays fail-closed until order status/fill reconciliation,
    /// depth enforcement, and validated venue precision are all available.
    pub fn ensure_live_trading_ready(&self) -> Result<()> {
        if self.live_trading_allowed() {
            self.validate_live_credentials()?;
            anyhow::bail!(
                "live trading is disabled until fill reconciliation, depth enforcement, and order validation are available"
            );
        }
        Ok(())
    }
}

fn validate_endpoint(value: &str, schemes: &[&str], name: &str) -> Result<()> {
    let url = reqwest::Url::parse(value).with_context(|| format!("{name} is not a valid URL"))?;
    if !schemes.iter().any(|scheme| url.scheme() == *scheme) {
        anyhow::bail!("{name} must use one of: {}", schemes.join(", "));
    }
    Ok(())
}

fn validate_address(value: &str, name: &str) -> Result<()> {
    Address::from_str(value).with_context(|| format!("{name} is not a valid Ethereum address"))?;
    Ok(())
}

fn finite_nonnegative(value: f64, name: &str) -> Result<()> {
    if !value.is_finite() || value < 0.0 {
        anyhow::bail!("{name} must be finite and non-negative");
    }
    Ok(())
}

fn finite_positive(value: f64, name: &str) -> Result<()> {
    if !value.is_finite() || value <= 0.0 {
        anyhow::bail!("{name} must be finite and greater than zero");
    }
    Ok(())
}

fn redacted_presence(value: &str) -> &'static str {
    if value.is_empty() {
        "<empty>"
    } else {
        "<redacted>"
    }
}

fn redacted_option(value: &Option<String>) -> &'static str {
    if value.as_deref().map(str::is_empty).unwrap_or(true) {
        "<empty>"
    } else {
        "<redacted>"
    }
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> AppConfig {
        serde_json::from_str(include_str!("../config.json")).unwrap()
    }

    #[test]
    fn committed_config_passes_validation() {
        config().validate().unwrap();
    }

    #[test]
    fn inverted_order_limits_are_rejected() {
        let mut cfg = config();
        cfg.strategy.min_order_size_usd = 501.0;
        let error = cfg.validate().unwrap_err().to_string();
        assert!(error.contains("min_order_size_usd"));
    }

    #[test]
    fn zero_take_profit_or_stop_loss_is_rejected() {
        for (name, apply) in [
            ("tp_sl.default_take_profit_pct", 0usize),
            ("tp_sl.default_stop_loss_pct", 1usize),
        ] {
            let mut cfg = config();
            if apply == 0 {
                cfg.tp_sl.default_take_profit_pct = 0.0;
            } else {
                cfg.tp_sl.default_stop_loss_pct = 0.0;
            }
            let error = cfg.validate().unwrap_err().to_string();
            assert!(error.contains(name), "{name} not rejected: {error}");
        }

        let mut cfg = config();
        cfg.tp_sl.per_category_tp_pct.insert("Crypto".into(), 0.0);
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn credential_debug_output_is_redacted() {
        let credentials = Credentials {
            private_key: "private-key-value".into(),
            funder_address: "funder-address-value".into(),
            api_key: Some("api-key-value".into()),
            api_secret: Some("api-secret-value".into()),
            api_passphrase: Some("api-passphrase-value".into()),
        };
        let debug = format!("{credentials:?}");
        for secret in [
            "private-key-value",
            "funder-address-value",
            "api-key-value",
            "api-secret-value",
            "api-passphrase-value",
        ] {
            assert!(!debug.contains(secret));
        }
        assert!(debug.contains("<redacted>"));
    }

    #[test]
    fn live_request_is_rejected_after_credential_preflight() {
        let mut cfg = config();
        cfg.bot.enable_trading = true;
        cfg.bot.mock_trading = false;
        cfg.credentials = Credentials {
            private_key: "00".repeat(32),
            funder_address: "0x0000000000000000000000000000000000000000".into(),
            api_key: Some("key".into()),
            api_secret: Some("secret".into()),
            api_passphrase: Some("passphrase".into()),
        };
        let error = cfg.ensure_live_trading_ready().unwrap_err().to_string();
        assert!(error.contains("live trading is disabled"));
    }
}
