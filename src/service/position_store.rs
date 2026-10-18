//! Open-position tracking + per-category / per-tag exposure accounting.
//!
//! Single source of truth for "what are we currently long/short?". The order
//! executor inserts on successful entries; the position monitor reads
//! snapshots and removes on exit. Mock orders are retained for inspection.
//! Exposure totals are derived on demand — cheap because the working set is
//! bounded by `max_open_positions`-ish.

use crate::models::{PlannedOrder, Side};
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

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
        self.simulated_orders.write().unwrap().push(order);
    }

    pub fn simulated_orders(&self) -> Vec<PlannedOrder> {
        self.simulated_orders.read().unwrap().clone()
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
