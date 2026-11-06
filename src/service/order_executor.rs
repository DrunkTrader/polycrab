//! High-level execution surface: glue between metadata → eligibility →
//! sizing → exposure → risk → signing → POST → position recording.
//!
//! Copy Trading routes its decided trades through [`OrderExecutor::execute`].
//! Safety flags are checked here, so individual bots never accidentally
//! bypass `enable_trading` or `mock_trading`.

use std::sync::Arc;

use crate::config::AppConfig;
use crate::models::{OrderType, PlannedOrder, Side, WhaleTrade};
use crate::service::clob::ClobClient;
use crate::service::eligibility::{self, Eligibility};
use crate::service::market_cache::{MarketCache, MarketInfo};
use crate::service::position_store::{OpenPosition, PositionStore};
use crate::service::risk_guard::{BlockReason, RiskCheck, RiskGuard};
use crate::service::strategy;
use crate::utils;
use anyhow::Result;
use chrono::Utc;
use tracing::{debug, info, warn};

pub struct OrderExecutor {
    cfg: AppConfig,
    clob: Option<Arc<ClobClient>>,
    risk: Arc<RiskGuard>,
    markets: Arc<MarketCache>,
    positions: Arc<PositionStore>,
}

#[derive(Debug, Clone)]
pub enum ExecutionOutcome {
    Skipped(SkipReason),
    DryRun(PlannedOrder),
    Submitted {
        order: PlannedOrder,
        order_id: Option<String>,
        success: bool,
        detail: String,
    },
    UnknownSubmission {
        order: PlannedOrder,
        detail: String,
    },
}

#[derive(Debug, Clone)]
pub enum SkipReason {
    BelowSizing(strategy::SkipReason),
    InvalidOrder(String),
    MarketMetadataUnavailable,
    Ineligible(Eligibility),
    ExposureCategoryCap {
        category: String,
        cap: f64,
        current: f64,
        want: f64,
    },
    ExposureTagCap {
        tag: String,
        cap: f64,
        current: f64,
        want: f64,
    },
    RiskBlocked(BlockReason),
    TradingDisabled,
    AlreadyOpen,
    /// Whale closed/reduced their position. We delegate exits to the TP/SL
    /// monitor rather than mirroring the unwind, so this is a no-op by design.
    WhaleExitIgnored,
}

impl OrderExecutor {
    pub fn new(
        cfg: AppConfig,
        risk: Arc<RiskGuard>,
        markets: Arc<MarketCache>,
        positions: Arc<PositionStore>,
    ) -> Result<Self> {
        cfg.ensure_live_trading_ready()?;
        let clob = if cfg.live_trading_allowed() {
            Some(Arc::new(ClobClient::new(&cfg)?))
        } else {
            info!(
                enable_trading = cfg.bot.enable_trading,
                mock_trading = cfg.bot.mock_trading,
                "mock/dry-run mode enabled; CLOB credentials are not required"
            );
            None
        };
        Ok(Self {
            cfg,
            clob,
            risk,
            markets,
            positions,
        })
    }

    /// Return the underlying CLOB client (for the position monitor's exit path).
    pub fn clob(&self) -> Option<Arc<ClobClient>> {
        self.clob.as_ref().cloned()
    }

    pub fn positions(&self) -> Arc<PositionStore> {
        Arc::clone(&self.positions)
    }

    pub fn cached_market(&self, token_id: &str) -> Option<MarketInfo> {
        self.markets.peek_by_token(token_id)
    }

    pub async fn execute(&self, trade: &WhaleTrade) -> Result<ExecutionOutcome> {
        debug!(
            token = %trade.token_id,
            side = ?trade.side,
            shares = trade.shares,
            usd = trade.usd_notional,
            "execution pipeline started"
        );

        if let Err(error) = utils::validate_trade(trade) {
            let reason = error.to_string();
            warn!(token = %trade.token_id, %reason, "trade skipped: invalid source trade");
            return Ok(ExecutionOutcome::Skipped(SkipReason::InvalidOrder(reason)));
        }

        // These decisions do not need market metadata or network I/O.
        if trade.side == Side::Sell {
            info!(token = %trade.token_id, "trade skipped: whale sell is handled by TP/SL exits");
            return Ok(ExecutionOutcome::Skipped(SkipReason::WhaleExitIgnored));
        }
        // Sweep here as well as in the TP/SL monitor: the monitor is not spawned
        // when `tp_sl.enabled` is false, and a submission the venue has already
        // expired must neither block this token below nor inflate the exposure
        // totals checked later.
        let expired = self.positions.expire_pending(chrono::Duration::seconds(
            self.cfg.trading.order_expiration_secs as i64 + 60,
        ));
        if !expired.is_empty() {
            warn!(
                count = expired.len(),
                "dropped pending orders the venue has already expired"
            );
        }

        if self.positions.get(&trade.token_id).is_some()
            || self.positions.has_pending_entry(&trade.token_id)
        {
            info!(token = %trade.token_id, "trade skipped: position or entry already pending");
            return Ok(ExecutionOutcome::Skipped(SkipReason::AlreadyOpen));
        }

        // Resolve metadata after unconditional skips so ignored trades do not
        // consume Gamma requests.
        debug!(token = %trade.token_id, "resolving market metadata");
        let market = match self.markets.by_token_id(&trade.token_id).await {
            Ok(m) => m,
            Err(e) => {
                warn!(
                    token = %trade.token_id,
                    error = ?e,
                    "trade skipped: market metadata lookup failed"
                );
                return Ok(ExecutionOutcome::Skipped(
                    SkipReason::MarketMetadataUnavailable,
                ));
            }
        };
        debug!(
            token = %trade.token_id,
            slug = %market.slug,
            category = ?market.category,
            tags = ?market.tags,
            closed = market.closed,
            "market metadata resolved"
        );

        // 1. Eligibility (allowlist / blocklist / closed).
        debug!(slug = %market.slug, "checking market eligibility");
        let eligibility = eligibility::check(&self.cfg.filters, &market);
        if eligibility != Eligibility::Allowed {
            info!(
                slug = %market.slug,
                category = ?market.category,
                ?eligibility,
                "trade skipped: market not eligible"
            );
            return Ok(ExecutionOutcome::Skipped(SkipReason::Ineligible(
                eligibility,
            )));
        }
        debug!(slug = %market.slug, "market eligibility passed");

        // 3. Sizing.
        debug!(token = %trade.token_id, "sizing copied order");
        let sizing = strategy::size_for_trade(&self.cfg.strategy, trade);
        if let Some(r) = sizing.skipped {
            info!(
                token = %trade.token_id,
                ?r,
                "trade skipped: sizing rule"
            );
            return Ok(ExecutionOutcome::Skipped(SkipReason::BelowSizing(r)));
        }
        debug!(
            token = %trade.token_id,
            copy_usd = sizing.copy_usd,
            effective_percent = sizing.effective_percent,
            "sizing passed"
        );

        // 4. Risk guard.
        match self.risk.check_fast(trade) {
            RiskCheck::Allow => {
                debug!(token = %trade.token_id, "risk check passed");
            }
            RiskCheck::DepthUnverified => {
                if self.cfg.live_trading_allowed() {
                    self.risk.trip(BlockReason::BookUnavailable);
                    warn!(
                        token = %trade.token_id,
                        "trade blocked: order-book depth is unavailable"
                    );
                    return Ok(ExecutionOutcome::Skipped(SkipReason::RiskBlocked(
                        BlockReason::BookUnavailable,
                    )));
                }
                warn!(
                    token = %trade.token_id,
                    "paper trade has no order-book provider; continuing without live depth enforcement"
                );
            }
            RiskCheck::Block(reason) => {
                warn!(
                    token = %trade.token_id,
                    ?reason,
                    "trade skipped: risk guard blocked trade"
                );
                return Ok(ExecutionOutcome::Skipped(SkipReason::RiskBlocked(reason)));
            }
        }

        // 5. Build and validate the actual order amounts.
        let limit_price = limit_price_for(trade, &self.cfg);
        let shares = utils::usd_to_shares(sizing.copy_usd, limit_price);
        let order_type = OrderType::Fok;
        let planned = match utils::quantize_order(PlannedOrder {
            venue: trade.venue,
            token_id: trade.token_id.clone(),
            side: trade.side,
            shares,
            limit_price,
            usd_notional: sizing.copy_usd,
            order_type,
            source_trade_hash: trade.tx_hash.clone(),
            source_shares: Some(trade.shares),
            source_usd_notional: Some(trade.usd_notional),
            source_maker: Some(trade.maker.clone()),
            observed_at: Some(trade.observed_at),
        }) {
            Ok(order) => order,
            Err(error) => {
                let reason = error.to_string();
                warn!(token = %trade.token_id, %reason, "trade skipped: invalid planned order");
                return Ok(ExecutionOutcome::Skipped(SkipReason::InvalidOrder(reason)));
            }
        };
        if planned.usd_notional < self.cfg.strategy.min_order_size_usd {
            return Ok(ExecutionOutcome::Skipped(SkipReason::BelowSizing(
                strategy::SkipReason::BelowMinOrderSize,
            )));
        }

        // 6. Per-category / per-tag exposure caps use actual quantized notional.
        if let Some(skip) = self.check_exposure(&market, planned.usd_notional) {
            info!(token = %trade.token_id, ?skip, "trade skipped: exposure cap");
            return Ok(ExecutionOutcome::Skipped(skip));
        }
        debug!(token = %trade.token_id, "exposure checks passed");

        // Dry-run never signs or posts. This is deliberately before the CLOB
        // lookup so mock mode works with no private key or API credentials.
        if !self.cfg.live_trading_allowed() {
            info!(
                token = %planned.token_id,
                side = ?planned.side,
                shares = planned.shares,
                price = planned.limit_price,
                usd = planned.usd_notional,
                "mock order recorded; not signed or submitted"
            );
            self.positions.record_simulated_order(planned.clone());
            self.record_open(&market, &planned);
            return Ok(ExecutionOutcome::DryRun(planned));
        }

        let Some(clob) = self.clob.as_ref() else {
            warn!(
                token = %planned.token_id,
                "trade skipped: live trading requested but CLOB client is unavailable"
            );
            return Ok(ExecutionOutcome::Skipped(SkipReason::TradingDisabled));
        };

        // 7. Sign.
        debug!(token = %planned.token_id, "building signed CLOB order");
        let signed = clob
            .build_signed_order(
                &planned,
                self.cfg.trading.order_expiration_secs,
                market.neg_risk,
            )
            .await?;

        // 8. Dispatch. Acceptance is not a fill: retain a pending order and
        // wait for reconciliation before creating an open position.
        info!(token = %planned.token_id, "submitting live CLOB order");
        let resp = match clob.post_order(signed, order_type).await {
            Ok(response) => response,
            Err(error) => {
                let detail = format!("CLOB submission outcome unknown: {error}");
                self.positions.record_pending_order(
                    planned.clone(),
                    None,
                    market.category.clone(),
                    market.tags.clone(),
                );
                return Ok(ExecutionOutcome::UnknownSubmission {
                    order: planned,
                    detail,
                });
            }
        };
        let detail = resp
            .error_msg
            .clone()
            .unwrap_or_else(|| "CLOB POST response".into());
        if resp.success {
            self.positions.record_pending_order(
                planned.clone(),
                resp.order_id.clone(),
                market.category.clone(),
                market.tags.clone(),
            );
            if resp.order_id.is_none() {
                return Ok(ExecutionOutcome::UnknownSubmission {
                    order: planned,
                    detail: "CLOB accepted order without an order id; reconciliation required"
                        .into(),
                });
            }
        }
        Ok(ExecutionOutcome::Submitted {
            order: planned,
            order_id: resp.order_id,
            success: resp.success,
            detail,
        })
    }

    fn check_exposure(&self, market: &MarketInfo, want_usd: f64) -> Option<SkipReason> {
        let filters = &self.cfg.filters;
        if let Some(cat) = market.category.as_ref() {
            if let Some(&cap) = filters
                .per_category_max_open_usd
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(cat))
                .map(|(_, v)| v)
            {
                let current = self.positions.open_usd_by_category(cat)
                    + self.positions.pending_usd_by_category(cat);
                if current + want_usd > cap {
                    return Some(SkipReason::ExposureCategoryCap {
                        category: cat.clone(),
                        cap,
                        current,
                        want: want_usd,
                    });
                }
            }
        }
        for tag in &market.tags {
            if let Some(&cap) = filters
                .per_tag_max_open_usd
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(tag))
                .map(|(_, v)| v)
            {
                let current =
                    self.positions.open_usd_by_tag(tag) + self.positions.pending_usd_by_tag(tag);
                if current + want_usd > cap {
                    return Some(SkipReason::ExposureTagCap {
                        tag: tag.clone(),
                        cap,
                        current,
                        want: want_usd,
                    });
                }
            }
        }
        None
    }

    fn record_open(&self, market: &MarketInfo, planned: &PlannedOrder) {
        // Only entries open positions; exits remove them in the monitor.
        if planned.side != Side::Buy {
            return;
        }
        let (tp_pct, sl_pct) = self.tp_sl_for(market);
        // Polymarket prices cap at 0.99, so a percentage take-profit on a
        // high-priced entry can target a price that cannot occur. Say so at
        // open rather than leaving the operator to wonder why TP never fires.
        let tp_target = planned.limit_price * (1.0 + tp_pct / 100.0);
        if planned.side == Side::Buy && tp_target > utils::PRICE_MAX {
            warn!(
                token = %planned.token_id,
                entry = planned.limit_price,
                tp_pct,
                tp_target,
                "take-profit target is above the venue price ceiling; only stop-loss or expiry can close this position"
            );
        }
        let pos = OpenPosition {
            token_id: planned.token_id.clone(),
            slug: market.slug.clone(),
            question: market.display_name().to_string(),
            outcome: market
                .outcome_for_token(&planned.token_id)
                .map(str::to_string),
            expiry: market.expiry,
            category: market.category.clone(),
            tags: market.tags.clone(),
            side: planned.side,
            neg_risk: market.neg_risk,
            entry_price: planned.limit_price,
            shares: planned.shares,
            usd_notional: planned.usd_notional,
            take_profit_pct: tp_pct,
            stop_loss_pct: sl_pct,
            opened_at: Utc::now(),
            source_maker: planned.source_maker.clone(),
            source_trade_hash: planned.source_trade_hash.clone(),
            source_shares: planned.source_shares,
            source_usd_notional: planned.source_usd_notional,
        };
        self.positions.open(pos);
        debug!(
            token = %planned.token_id,
            shares = planned.shares,
            usd = planned.usd_notional,
            "position recorded"
        );
    }

    fn tp_sl_for(&self, market: &MarketInfo) -> (f64, f64) {
        let tp_default = self.cfg.tp_sl.default_take_profit_pct;
        let sl_default = self.cfg.tp_sl.default_stop_loss_pct;
        if let Some(cat) = market.category.as_ref() {
            let tp = self
                .cfg
                .tp_sl
                .per_category_tp_pct
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(cat))
                .map(|(_, v)| *v)
                .unwrap_or(tp_default);
            let sl = self
                .cfg
                .tp_sl
                .per_category_sl_pct
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(cat))
                .map(|(_, v)| *v)
                .unwrap_or(sl_default);
            (tp, sl)
        } else {
            (tp_default, sl_default)
        }
    }
}

fn limit_price_for(trade: &WhaleTrade, cfg: &AppConfig) -> f64 {
    let buf = cfg.trading.price_buffer;
    let raw = match trade.side {
        Side::Buy => trade.price + buf,
        Side::Sell => trade.price - buf,
    };
    utils::clamp_price(raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Credentials;
    use crate::models::VenueId;
    use reqwest::Client;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn gamma_fixture() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 2048];
            let _ = stream.read(&mut request).await.unwrap();
            let body = r#"[{"slug":"mock-market","question":"Mock?","closed":false,"clobTokenIds":["123","456"],"feeType":"crypto_fees_v2"}]"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        format!("http://{address}")
    }

    fn config(gamma_api_base: String) -> AppConfig {
        let mut cfg: AppConfig = serde_json::from_str(include_str!("../../config.json")).unwrap();
        cfg.site.gamma_api_base = gamma_api_base;
        cfg.site.clob_api_base = "http://127.0.0.1:1".into();
        cfg.bot.enable_trading = true;
        cfg.bot.mock_trading = true;
        cfg.credentials = Credentials::default();
        cfg
    }

    fn trade() -> WhaleTrade {
        WhaleTrade {
            venue: VenueId::Polymarket,
            maker: "0x1111111111111111111111111111111111111111".into(),
            side: Side::Buy,
            token_id: "123".into(),
            shares: 100.0,
            price: 0.5,
            usd_notional: 50.0,
            tx_hash: Some("0xtx".into()),
            block_number: Some(1),
            observed_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn paper_mode_accepts_gamma_fee_type_and_records_order() {
        let gamma = gamma_fixture().await;
        let cfg = config(gamma);
        let risk = RiskGuard::new(cfg.risk.clone());
        let markets = MarketCache::new(Client::new(), cfg.site.gamma_api_base.clone());
        let positions = PositionStore::new();
        let executor = OrderExecutor::new(cfg, risk, markets, Arc::clone(&positions)).unwrap();

        assert!(executor.clob().is_none());
        let outcome = executor.execute(&trade()).await.unwrap();
        let order = match outcome {
            ExecutionOutcome::DryRun(order) => order,
            other => panic!("expected dry-run order, got {other:?}"),
        };

        assert_eq!(order.token_id, "123");
        assert_eq!(positions.simulated_orders().len(), 1);
        assert_eq!(positions.len(), 1);
        let position = positions.get("123").unwrap();
        assert_eq!(position.slug, "mock-market");
        assert_eq!(position.category.as_deref(), Some("Crypto"));
        assert!(position.tags.is_empty());
        assert_eq!(position.usd_notional, order.usd_notional);
    }

    #[tokio::test]
    async fn paper_mode_keeps_large_trade_usable_without_a_book_provider() {
        let gamma = gamma_fixture().await;
        let cfg = config(gamma);
        let risk = RiskGuard::new(cfg.risk.clone());
        let markets = MarketCache::new(Client::new(), cfg.site.gamma_api_base.clone());
        let positions = PositionStore::new();
        let executor = OrderExecutor::new(cfg, risk, markets, Arc::clone(&positions)).unwrap();
        let mut large = trade();
        large.shares = 2_000.0;
        large.usd_notional = 1_000.0;

        assert!(matches!(
            executor.execute(&large).await.unwrap(),
            ExecutionOutcome::DryRun(_)
        ));
        assert_eq!(positions.simulated_orders().len(), 1);
    }

    #[test]
    fn live_mode_is_rejected_before_a_clob_client_is_created() {
        let mut cfg: AppConfig = serde_json::from_str(include_str!("../../config.json")).unwrap();
        cfg.bot.enable_trading = true;
        cfg.bot.mock_trading = false;
        cfg.credentials = Credentials {
            private_key: "00".repeat(32),
            funder_address: "0x0000000000000000000000000000000000000000".into(),
            api_key: Some("test-key".into()),
            api_secret: Some("test-secret".into()),
            api_passphrase: Some("test-passphrase".into()),
        };
        let risk = RiskGuard::new(cfg.risk.clone());
        let markets = MarketCache::new(Client::new(), cfg.site.gamma_api_base.clone());
        let positions = PositionStore::new();

        let error = match OrderExecutor::new(cfg, risk, markets, positions) {
            Ok(_) => panic!("live mode must be rejected before client construction"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("live trading is disabled"));
    }
}
