//! Open-position tracking + per-category / per-tag exposure accounting.
//!
//! Single source of truth for "what are we currently long/short?". Simulated
//! entries are opened immediately; live submissions remain pending until a
//! fill-reconciliation path promotes them to positions.
//! Exposure totals are derived on demand — cheap because the working set is
//! bounded by `max_open_positions`-ish.

use crate::models::{PlannedOrder, Side};
use chrono::{DateTime, Duration, Utc};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Simulated orders are a session audit trail for the dashboard, not state the
/// engine reads. Keep the newest ones so a long paper run cannot grow forever.
const MAX_SIMULATED_ORDERS: usize = 1_000;

#[derive(Debug, Clone)]
pub struct OpenPosition {
    pub token_id: String,
    pub slug: String,
    pub question: String,
    pub outcome: Option<String>,
    pub expiry: Option<DateTime<Utc>>,
    pub category: Option<String>,
    pub tags: Vec<String>,
    pub side: Side,
    /// Mirrors [`crate::service::market_cache::MarketInfo::neg_risk`] so a TP/SL
    /// exit signs against the same exchange contract the entry used.
    pub neg_risk: bool,
    pub entry_price: f64,
    pub shares: f64,
    pub usd_notional: f64,
    pub take_profit_pct: f64,
    pub stop_loss_pct: f64,
    pub opened_at: DateTime<Utc>,
    pub source_maker: Option<String>,
    pub source_trade_hash: Option<String>,
    pub source_shares: Option<f64>,
    pub source_usd_notional: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct PendingOrder {
    pub key: String,
    pub order_id: Option<String>,
    pub order: PlannedOrder,
    pub category: Option<String>,
    pub tags: Vec<String>,
    pub submitted_at: DateTime<Utc>,
}

impl OpenPosition {
    /// Current unrealised P&L percentage given a midprice quote.
    ///
    /// For BUY entries (long the outcome), gain = +(midprice - entry) / entry.
    /// For SELL entries (rare on Polymarket, short the outcome), gain is
    /// inverted.
    pub fn pnl_pct(&self, midprice: f64) -> f64 {
        if self.entry_price <= 0.0 {
            return 0.0;
        }
        let raw = (midprice - self.entry_price) / self.entry_price * 100.0;
        match self.side {
            Side::Buy => raw,
            Side::Sell => -raw,
        }
    }

    /// Current unrealised P&L in USD using the entry notional as its base.
    pub fn pnl_usd(&self, midprice: f64) -> f64 {
        let direction = match self.side {
            Side::Buy => 1.0,
            Side::Sell => -1.0,
        };
        (midprice - self.entry_price) * self.shares * direction
    }
}

#[derive(Debug, Default)]
pub struct PositionStore {
    inner: RwLock<HashMap<String, OpenPosition>>,
    simulated_orders: RwLock<Vec<PlannedOrder>>,
    pending_orders: RwLock<HashMap<String, PendingOrder>>,
}

impl PositionStore {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn open(&self, pos: OpenPosition) {
        self.inner
            .write()
            .unwrap()
            .insert(pos.token_id.clone(), pos);
    }

    pub fn close(&self, token_id: &str) -> Option<OpenPosition> {
        self.inner.write().unwrap().remove(token_id)
    }

    pub fn get(&self, token_id: &str) -> Option<OpenPosition> {
        self.inner.read().unwrap().get(token_id).cloned()
    }

    pub fn snapshot(&self) -> Vec<OpenPosition> {
        self.inner.read().unwrap().values().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.inner.read().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.read().unwrap().is_empty()
    }

    pub fn record_simulated_order(&self, order: PlannedOrder) {
        let mut orders = self.simulated_orders.write().unwrap();
        orders.push(order);
        if orders.len() > MAX_SIMULATED_ORDERS {
            let overflow = orders.len() - MAX_SIMULATED_ORDERS;
            orders.drain(..overflow);
        }
    }

    pub fn simulated_orders(&self) -> Vec<PlannedOrder> {
        self.simulated_orders.read().unwrap().clone()
    }

    /// Record an accepted live order without claiming that it filled.
    pub fn record_pending_order(
        &self,
        order: PlannedOrder,
        order_id: Option<String>,
        category: Option<String>,
        tags: Vec<String>,
    ) -> String {
        let key = order_id.clone().unwrap_or_else(|| {
            format!(
                "unknown:{}:{}",
                order.token_id,
                Utc::now().timestamp_nanos_opt().unwrap_or_default()
            )
        });
        self.pending_orders.write().unwrap().insert(
            key.clone(),
            PendingOrder {
                key: key.clone(),
                order_id,
                order,
                category,
                tags,
                submitted_at: Utc::now(),
            },
        );
        key
    }

    /// Drop pending orders older than `max_age` and return them.
    ///
    /// Without fill reconciliation nothing else ever clears this map, so a
    /// submission whose outcome is unknown would block its token for the rest
    /// of the session and keep consuming exposure caps. The venue has already
    /// expired the order itself by the time it is this old.
    pub fn expire_pending(&self, max_age: Duration) -> Vec<PendingOrder> {
        let cutoff = Utc::now() - max_age;
        let mut pending = self.pending_orders.write().unwrap();
        let stale: Vec<String> = pending
            .values()
            .filter(|order| order.submitted_at < cutoff)
            .map(|order| order.key.clone())
            .collect();
        stale.iter().filter_map(|key| pending.remove(key)).collect()
    }

    pub fn pending_orders(&self) -> Vec<PendingOrder> {
        self.pending_orders
            .read()
            .unwrap()
            .values()
            .cloned()
            .collect()
    }

    pub fn has_pending_entry(&self, token_id: &str) -> bool {
        self.pending_orders
            .read()
            .unwrap()
            .values()
            .any(|pending| pending.order.token_id == token_id && pending.order.side == Side::Buy)
    }

    pub fn has_pending_exit(&self, token_id: &str) -> bool {
        self.pending_orders
            .read()
            .unwrap()
            .values()
            .any(|pending| pending.order.token_id == token_id && pending.order.side == Side::Sell)
    }

    pub fn pending_usd_by_category(&self, category: &str) -> f64 {
        self.pending_orders
            .read()
            .unwrap()
            .values()
            .filter(|pending| {
                pending
                    .category
                    .as_ref()
                    .map(|value| value.eq_ignore_ascii_case(category))
                    .unwrap_or(false)
            })
            .map(|pending| pending.order.usd_notional)
            .sum()
    }

    pub fn pending_usd_by_tag(&self, tag: &str) -> f64 {
        self.pending_orders
            .read()
            .unwrap()
            .values()
            .filter(|pending| {
                pending
                    .tags
                    .iter()
                    .any(|value| value.eq_ignore_ascii_case(tag))
            })
            .map(|pending| pending.order.usd_notional)
            .sum()
    }

    pub fn open_usd_by_category(&self, category: &str) -> f64 {
        self.inner
            .read()
            .unwrap()
            .values()
            .filter(|p| {
                p.category
                    .as_ref()
                    .map(|c| c.eq_ignore_ascii_case(category))
                    .unwrap_or(false)
            })
            .map(|p| p.usd_notional)
            .sum()
    }

    pub fn open_usd_by_tag(&self, tag: &str) -> f64 {
        self.inner
            .read()
            .unwrap()
            .values()
            .filter(|p| p.tags.iter().any(|t| t.eq_ignore_ascii_case(tag)))
            .map(|p| p.usd_notional)
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(slug: &str, cat: &str, tags: &[&str], usd: f64) -> OpenPosition {
        OpenPosition {
            token_id: format!("tok-{slug}"),
            slug: slug.into(),
            question: slug.into(),
            outcome: Some("YES".into()),
            expiry: None,
            category: Some(cat.into()),
            tags: tags.iter().map(|s| s.to_string()).collect(),
            side: Side::Buy,
            neg_risk: false,
            entry_price: 0.50,
            shares: usd * 2.0,
            usd_notional: usd,
            take_profit_pct: 50.0,
            stop_loss_pct: 30.0,
            opened_at: Utc::now(),
            source_maker: None,
            source_trade_hash: None,
            source_shares: None,
            source_usd_notional: None,
        }
    }

    #[test]
    fn category_exposure_sums_only_same_category() {
        let s = PositionStore::default();
        s.open(pos("a", "Politics", &[], 100.0));
        s.open(pos("b", "Politics", &[], 50.0));
        s.open(pos("c", "Crypto", &[], 200.0));
        assert_eq!(s.open_usd_by_category("Politics"), 150.0);
        assert_eq!(s.open_usd_by_category("crypto"), 200.0);
    }

    #[test]
    fn tag_exposure_sums_across_multiple_tags() {
        let s = PositionStore::default();
        s.open(pos("a", "Politics", &["election2024", "us"], 100.0));
        s.open(pos("b", "Politics", &["us"], 50.0));
        assert_eq!(s.open_usd_by_tag("us"), 150.0);
        assert_eq!(s.open_usd_by_tag("election2024"), 100.0);
    }

    #[test]
    fn accepted_orders_are_pending_not_open_positions() {
        let s = PositionStore::default();
        let order = PlannedOrder {
            venue: crate::models::VenueId::Polymarket,
            token_id: "123".into(),
            side: Side::Buy,
            shares: 10.0,
            limit_price: 0.5,
            usd_notional: 5.0,
            order_type: crate::models::OrderType::Fok,
            source_trade_hash: None,
            source_shares: None,
            source_usd_notional: None,
            source_maker: None,
            observed_at: None,
        };
        let key = s.record_pending_order(
            order,
            Some("order-1".into()),
            Some("Crypto".into()),
            vec!["solana".into()],
        );

        assert_eq!(key, "order-1");
        assert!(s.get("123").is_none());
        assert!(s.has_pending_entry("123"));
        assert_eq!(s.pending_usd_by_category("crypto"), 5.0);
        assert_eq!(s.pending_usd_by_tag("SOLANA"), 5.0);
    }

    fn planned(token: &str, side: Side) -> PlannedOrder {
        PlannedOrder {
            venue: crate::models::VenueId::Polymarket,
            token_id: token.into(),
            side,
            shares: 10.0,
            limit_price: 0.5,
            usd_notional: 5.0,
            order_type: crate::models::OrderType::Fok,
            source_trade_hash: None,
            source_shares: None,
            source_usd_notional: None,
            source_maker: None,
            observed_at: None,
        }
    }

    #[test]
    fn stale_pending_orders_are_expired_and_stop_blocking_their_token() {
        let s = PositionStore::default();
        let key = s.record_pending_order(
            planned("123", Side::Buy),
            Some("order-1".into()),
            Some("Crypto".into()),
            vec![],
        );
        // Backdate the submission past the cutoff.
        s.pending_orders
            .write()
            .unwrap()
            .get_mut(&key)
            .unwrap()
            .submitted_at = Utc::now() - Duration::seconds(600);

        assert!(s.has_pending_entry("123"));
        let expired = s.expire_pending(Duration::seconds(120));

        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].order_id.as_deref(), Some("order-1"));
        assert!(!s.has_pending_entry("123"), "token must be tradable again");
        assert_eq!(s.pending_usd_by_category("crypto"), 0.0);
    }

    #[test]
    fn fresh_pending_orders_survive_expiry_sweep() {
        let s = PositionStore::default();
        s.record_pending_order(planned("123", Side::Buy), None, None, vec![]);
        assert!(s.expire_pending(Duration::seconds(120)).is_empty());
        assert!(s.has_pending_entry("123"));
    }

    #[test]
    fn simulated_orders_are_capped_keeping_the_newest() {
        let s = PositionStore::default();
        for i in 0..MAX_SIMULATED_ORDERS + 25 {
            s.record_simulated_order(planned(&format!("tok-{i}"), Side::Buy));
        }
        let orders = s.simulated_orders();
        assert_eq!(orders.len(), MAX_SIMULATED_ORDERS);
        assert_eq!(orders.first().unwrap().token_id, "tok-25");
        assert_eq!(
            orders.last().unwrap().token_id,
            format!("tok-{}", MAX_SIMULATED_ORDERS + 24)
        );
    }

    #[test]
    fn pnl_pct_buy_long_direction() {
        let mut p = pos("a", "X", &[], 100.0);
        p.entry_price = 0.50;
        assert!((p.pnl_pct(0.75) - 50.0).abs() < 1e-9);
        assert!((p.pnl_pct(0.25) + 50.0).abs() < 1e-9);
    }

    #[test]
    fn pnl_pct_sell_short_direction() {
        let mut p = pos("a", "X", &[], 100.0);
        p.side = Side::Sell;
        p.entry_price = 0.50;
        assert!((p.pnl_pct(0.25) - 50.0).abs() < 1e-9);
    }

    #[test]
    fn pnl_usd_uses_shares_and_side_direction() {
        let mut p = pos("a", "X", &[], 100.0);
        p.entry_price = 0.50;
        assert!((p.pnl_usd(0.75) - 50.0).abs() < 1e-9);
        p.side = Side::Sell;
        assert!((p.pnl_usd(0.25) - 50.0).abs() < 1e-9);
    }
}
