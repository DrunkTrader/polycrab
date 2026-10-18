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
}

#[derive(Debug, Clone)]
pub enum SkipReason {
    BelowSizing,
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

        // 0. Resolve metadata before early skips so the operator feed can still
        // name the contract and outcome for ignored trades.
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

        // 1a. Whale sells are not mirrored — TP/SL is the configured exit path.
        if trade.side == Side::Sell {
            info!(token = %trade.token_id, "trade skipped: whale sell is handled by TP/SL exits");
            return Ok(ExecutionOutcome::Skipped(SkipReason::WhaleExitIgnored));
        }
        // 1b. Don't pyramid into a market we're already long.
        if self.positions.get(&trade.token_id).is_some() {
            info!(token = %trade.token_id, "trade skipped: position already open");
            return Ok(ExecutionOutcome::Skipped(SkipReason::AlreadyOpen));
        }

        // 2. Eligibility (allowlist / blocklist / closed).
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
            return Ok(ExecutionOutcome::Skipped(SkipReason::BelowSizing));
        }
        debug!(
            token = %trade.token_id,
            copy_usd = sizing.copy_usd,
            effective_percent = sizing.effective_percent,
            "sizing passed"
        );

        // 4. Per-category / per-tag exposure caps (entries only).
        if trade.side == Side::Buy {
            if let Some(skip) = self.check_exposure(&market, sizing.copy_usd) {
                info!(token = %trade.token_id, ?skip, "trade skipped: exposure cap");
                return Ok(ExecutionOutcome::Skipped(skip));
            }
        }
        debug!(token = %trade.token_id, "exposure checks passed");

        // 5. Risk guard.
        match self.risk.check_fast(trade) {
            RiskCheck::Allow => {
                debug!(token = %trade.token_id, "risk check passed");
            }
            RiskCheck::FetchBook => {
                // Caller (or a follow-up) should pull the book and call
                // `risk.check_with_book`. For now we proceed but flag the
                // pre-trade book fetch as the next integration point.
                warn!(
                    token = %trade.token_id,
                    "risk check requested order book; no book provider configured, continuing"
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

        // 6. Build the order.
        let limit_price = limit_price_for(trade, &self.cfg);
        let shares = utils::usd_to_shares(sizing.copy_usd, limit_price);
        let order_type = order_type_for(trade.side);
        let planned = PlannedOrder {
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
        };

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
            .build_signed_order(&planned, self.cfg.trading.order_expiration_secs)
            .await?;

        // 8. Dispatch.
        info!(token = %planned.token_id, "submitting live CLOB order");
        let resp = clob.post_order(signed, order_type).await?;
        let detail = resp
            .error_msg
            .clone()
            .unwrap_or_else(|| "CLOB POST response".into());
        if resp.success {
            self.record_open(&market, &planned);
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
                let current = self.positions.open_usd_by_category(cat);
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
                let current = self.positions.open_usd_by_tag(tag);
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

fn order_type_for(side: Side) -> OrderType {
    match side {
        Side::Buy => OrderType::Fak,
        Side::Sell => OrderType::Gtd,
    }
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
}
