//! Shared operator state for the terminal dashboard.
//!
//! The trading engine publishes typed events here. The UI reads a cloned
//! snapshot, so rendering never waits on a network client or trading lock.

use crate::models::{PlannedOrder, Side, WhaleTrade};
use crate::service::market_cache::MarketInfo;
use crate::service::position_store::OpenPosition;
use chrono::{DateTime, Utc};
use std::collections::VecDeque;
use std::sync::{Arc, RwLock};
use std::time::Instant;

// ponytail: std locks trade parking_lot's non-poisoning behavior for zero direct lock dependency; recover poison or restore it if panic isolation matters.
pub const ACTIVITY_LIMIT: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TradingMode {
    Simulated,
    Live,
}

impl TradingMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Simulated => "SIMULATED",
            Self::Live => "LIVE",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionStatus {
    Stopped,
    Connecting,
    Connected,
    Subscribed,
    Reconnecting,
    Error,
}

impl ConnectionStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Stopped => "STOPPED",
            Self::Connecting => "CONNECTING",
            Self::Connected => "CONNECTED",
            Self::Subscribed => "SUBSCRIBED",
            Self::Reconnecting => "RECONNECTING",
            Self::Error => "ERROR",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityLevel {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone)]
pub struct ActivityItem {
    pub at: DateTime<Utc>,
    pub level: ActivityLevel,
    pub kind: String,
    pub detail: String,
}

#[derive(Debug, Clone)]
pub struct PositionView {
    pub token_id: String,
    pub slug: String,
    pub question: String,
    pub outcome: Option<String>,
    pub expiry: Option<DateTime<Utc>>,
    pub category: Option<String>,
    pub tags: Vec<String>,
    pub side: Side,
    pub entry_price: f64,
    pub midpoint: Option<f64>,
    pub market_value_usd: Option<f64>,
    pub shares: f64,
    pub usd_notional: f64,
    pub pnl_pct: Option<f64>,
    pub pnl_usd: Option<f64>,
    pub quoted_at: Option<DateTime<Utc>>,
    pub opened_at: DateTime<Utc>,
    pub take_profit_pct: f64,
    pub stop_loss_pct: f64,
    pub source_maker: Option<String>,
    pub source_trade_hash: Option<String>,
    pub source_shares: Option<f64>,
    pub source_usd_notional: Option<f64>,
}

impl From<&OpenPosition> for PositionView {
    fn from(position: &OpenPosition) -> Self {
        Self {
            token_id: position.token_id.clone(),
            slug: position.slug.clone(),
            question: position.question.clone(),
            outcome: position.outcome.clone(),
            expiry: position.expiry,
            category: position.category.clone(),
            tags: position.tags.clone(),
            side: position.side,
            entry_price: position.entry_price,
            midpoint: None,
            market_value_usd: None,
            shares: position.shares,
            usd_notional: position.usd_notional,
            pnl_pct: None,
            pnl_usd: None,
            quoted_at: None,
            opened_at: position.opened_at,
            take_profit_pct: position.take_profit_pct,
            stop_loss_pct: position.stop_loss_pct,
            source_maker: position.source_maker.clone(),
            source_trade_hash: position.source_trade_hash.clone(),
            source_shares: position.source_shares,
            source_usd_notional: position.source_usd_notional,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TradeView {
    pub at: DateTime<Utc>,
    pub market: String,
    pub slug: Option<String>,
    pub outcome: Option<String>,
    pub expiry: Option<DateTime<Utc>>,
    pub source_wallet: String,
    pub source_side: Side,
    pub source_shares: f64,
    pub source_usd_notional: f64,
    pub copied_side: Option<Side>,
    pub copied_shares: Option<f64>,
    pub copied_usd_notional: Option<f64>,
    pub copy_ratio: Option<f64>,
    pub result: String,
    pub reason: Option<String>,
    pub tx_hash: Option<String>,
    pub latency_ms: u64,
}

#[derive(Debug, Clone, Default)]
pub struct OperatorCounters {
    pub observed: u64,
    pub skipped: u64,
    pub simulated: u64,
    pub post_responses: u64,
    pub errors: u64,
    pub exits: u64,
    pub wins: u64,
    pub losses: u64,
    pub latency_total_ms: u64,
    pub latency_samples: u64,
}

#[derive(Debug, Clone)]
pub struct OperatorSnapshot {
    pub bot_name: String,
    pub bot_running: bool,
    pub mode: TradingMode,
    pub configured_wallets: usize,
    pub active_wallet: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub connection: ConnectionStatus,
    pub connection_detail: Option<String>,
    pub last_block: Option<u64>,
    pub risk_tripped: bool,
    pub risk_reason: Option<String>,
    pub risk_until: Option<Instant>,
    pub counters: OperatorCounters,
    pub realized_pnl_usd: f64,
    pub positions: Vec<PositionView>,
    pub recent_trades: VecDeque<TradeView>,
    pub activity: VecDeque<ActivityItem>,
}

impl Default for OperatorSnapshot {
    fn default() -> Self {
        Self {
            bot_name: "Idle".into(),
            bot_running: false,
            mode: TradingMode::Simulated,
            configured_wallets: 0,
            active_wallet: None,
            started_at: None,
            connection: ConnectionStatus::Stopped,
            connection_detail: None,
            last_block: None,
            risk_tripped: false,
            risk_reason: None,
            risk_until: None,
            counters: OperatorCounters::default(),
            realized_pnl_usd: 0.0,
            positions: Vec::new(),
            recent_trades: VecDeque::new(),
            activity: VecDeque::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum OperatorEvent {
    BotStarted {
        name: String,
        mode: TradingMode,
        configured_wallets: usize,
        active_wallet: String,
    },
    BotStopped {
        detail: String,
    },
    Connection {
        status: ConnectionStatus,
        detail: Option<String>,
    },
    LogReceived {
        block_number: u64,
    },
    TradeObserved(WhaleTrade),
    TradeProcessed {
        trade: WhaleTrade,
        market: Option<Box<MarketInfo>>,
        order: Option<Box<PlannedOrder>>,
        result: String,
        reason: Option<String>,
        latency_ms: u64,
    },
    ExecutionSkipped {
        token_id: String,
        reason: String,
    },
    OrderSimulated(PlannedOrder),
    OrderResponse {
        token_id: String,
        order_id: Option<String>,
        success: bool,
        detail: String,
    },
    PositionOpened(OpenPosition),
    PositionQuote {
        token_id: String,
        midpoint: f64,
        pnl_pct: f64,
        pnl_usd: f64,
        quoted_at: DateTime<Utc>,
    },
    ExitTriggered {
        token_id: String,
        reason: String,
        pnl_pct: f64,
        pnl_usd: f64,
    },
    PositionClosed {
        token_id: String,
        pnl_pct: Option<f64>,
        pnl_usd: Option<f64>,
    },
    RiskStatus {
        tripped: bool,
        reason: Option<String>,
        until: Option<Instant>,
    },
    RuntimeError {
        context: String,
        message: String,
    },
}

#[derive(Debug, Clone)]
pub struct OperatorBus {
    inner: Arc<RwLock<OperatorSnapshot>>,
}

impl Default for OperatorBus {
    fn default() -> Self {
        Self::new()
    }
}

impl OperatorBus {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(OperatorSnapshot::default())),
        }
    }

    pub fn snapshot(&self) -> OperatorSnapshot {
        self.inner.read().unwrap().clone()
    }

    pub fn publish(&self, event: OperatorEvent) {
        let mut snapshot = self.inner.write().unwrap();
        let (level, kind, detail) = apply_event(&mut snapshot, event);
        snapshot.activity.push_back(ActivityItem {
            at: Utc::now(),
            level,
            kind,
            detail,
        });
        while snapshot.activity.len() > ACTIVITY_LIMIT {
            snapshot.activity.pop_front();
        }
    }
}

fn apply_event(
    snapshot: &mut OperatorSnapshot,
    event: OperatorEvent,
) -> (ActivityLevel, String, String) {
    match event {
        OperatorEvent::BotStarted {
            name,
            mode,
            configured_wallets,
            active_wallet,
        } => {
            snapshot.bot_name = name.clone();
            snapshot.bot_running = true;
            snapshot.mode = mode;
            snapshot.configured_wallets = configured_wallets;
            snapshot.active_wallet = Some(active_wallet.clone());
            snapshot.started_at = Some(Utc::now());
            snapshot.connection = ConnectionStatus::Connecting;
            (
                ActivityLevel::Info,
                "BOT".into(),
                format!("{name} started in {} mode", mode.label()),
            )
        }
        OperatorEvent::BotStopped { detail } => {
            snapshot.bot_running = false;
            snapshot.connection = ConnectionStatus::Stopped;
            (
                ActivityLevel::Info,
                "BOT".into(),
                format!("bot stopped: {detail}"),
            )
        }
        OperatorEvent::Connection { status, detail } => {
            snapshot.connection = status;
            snapshot.connection_detail = detail.clone();
            let level = match status {
                ConnectionStatus::Error => ActivityLevel::Error,
                ConnectionStatus::Reconnecting => ActivityLevel::Warning,
                ConnectionStatus::Subscribed | ConnectionStatus::Connected => {
                    ActivityLevel::Success
                }
                ConnectionStatus::Connecting | ConnectionStatus::Stopped => ActivityLevel::Info,
            };
            (
                level,
                "WS".into(),
                detail.unwrap_or_else(|| status.label().to_string()),
            )
        }
        OperatorEvent::LogReceived { block_number } => {
            snapshot.last_block = Some(block_number);
            (
                ActivityLevel::Info,
                "CHAIN".into(),
                format!("log received at block {block_number}"),
            )
        }
        OperatorEvent::TradeObserved(trade) => {
            snapshot.counters.observed += 1;
            (
                ActivityLevel::Info,
                "WHALE".into(),
                format!(
                    "{} {} · {:.2} shares · ${:.2}",
                    side_label(trade.side),
                    trade.token_id,
                    trade.shares,
                    trade.usd_notional
                ),
            )
        }
        OperatorEvent::TradeProcessed {
            trade,
            market,
            order,
            result,
            reason,
            latency_ms,
        } => {
            snapshot.counters.latency_total_ms += latency_ms;
            snapshot.counters.latency_samples += 1;
            let market_name = market
                .as_ref()
                .map(|info| info.display_name().to_string())
                .unwrap_or_else(|| trade.token_id.clone());
            let outcome = market
                .as_ref()
                .and_then(|info| info.outcome_for_token(&trade.token_id))
                .map(str::to_string);
            let copied_shares = order.as_ref().map(|item| item.shares);
            let copied_usd_notional = order.as_ref().map(|item| item.usd_notional);
            let copy_ratio = copied_usd_notional
                .filter(|_| trade.usd_notional > 0.0)
                .map(|value| value / trade.usd_notional);
            snapshot.recent_trades.push_back(TradeView {
                at: trade.observed_at,
                market: market_name.clone(),
                slug: market.as_ref().map(|info| info.slug.clone()),
                outcome,
                expiry: market.as_ref().and_then(|info| info.expiry),
                source_wallet: trade.maker.clone(),
                source_side: trade.side,
                source_shares: trade.shares,
                source_usd_notional: trade.usd_notional,
                copied_side: order.as_ref().map(|item| item.side),
                copied_shares,
                copied_usd_notional,
                copy_ratio,
                result: result.clone(),
                reason: reason.clone(),
                tx_hash: trade.tx_hash.clone(),
                latency_ms,
            });
            while snapshot.recent_trades.len() > ACTIVITY_LIMIT {
                snapshot.recent_trades.pop_front();
            }
            let copy = copied_usd_notional
                .map(|value| format!(" · copy ${value:.2}"))
                .unwrap_or_default();
            let reason = reason
                .map(|value| format!(" · {value}"))
                .unwrap_or_default();
            (
                match result.as_str() {
                    "SKIPPED" => ActivityLevel::Warning,
                    "REJECTED" => ActivityLevel::Error,
                    _ => ActivityLevel::Success,
                },
                "TRADE".into(),
                format!(
                    "{}{} · source ${:.2} · {result} · {latency_ms}ms{reason}",
                    market_name, copy, trade.usd_notional
                ),
            )
        }
        OperatorEvent::ExecutionSkipped { token_id, reason } => {
            snapshot.counters.skipped += 1;
            (
                ActivityLevel::Warning,
                "SKIP".into(),
                format!("{token_id} · {reason}"),
            )
        }
        OperatorEvent::OrderSimulated(order) => {
            snapshot.counters.simulated += 1;
            (
                ActivityLevel::Success,
                "PAPER".into(),
                format!(
                    "{} {} · {:.2} shares · ${:.2} · not submitted",
                    side_label(order.side),
                    order.token_id,
                    order.shares,
                    order.usd_notional
                ),
            )
        }
        OperatorEvent::OrderResponse {
            token_id,
            order_id,
            success,
            detail,
        } => {
            snapshot.counters.post_responses += 1;
            let level = if success {
                ActivityLevel::Success
            } else {
                ActivityLevel::Error
            };
            (
                level,
                "POST".into(),
                format!(
                    "{token_id} · {}{}",
                    detail,
                    order_id.map(|id| format!(" · id {id}")).unwrap_or_default()
                ),
            )
        }
        OperatorEvent::PositionOpened(position) => {
            let view = PositionView::from(&position);
            if let Some(existing) = snapshot
                .positions
                .iter_mut()
                .find(|item| item.token_id == view.token_id)
            {
                *existing = view;
            } else {
                snapshot.positions.push(view);
            }
            (
                ActivityLevel::Success,
                "POSITION".into(),
                format!(
                    "opened {} · ${:.2} local exposure",
                    position.slug, position.usd_notional
                ),
            )
        }
        OperatorEvent::PositionQuote {
            token_id,
            midpoint,
            pnl_pct,
            pnl_usd,
            quoted_at,
        } => {
            if let Some(position) = snapshot
                .positions
                .iter_mut()
                .find(|item| item.token_id == token_id)
            {
                position.midpoint = Some(midpoint);
                position.market_value_usd = Some(position.shares * midpoint);
                position.pnl_pct = Some(pnl_pct);
                position.pnl_usd = Some(pnl_usd);
                position.quoted_at = Some(quoted_at);
            }
            (
                ActivityLevel::Info,
                "QUOTE".into(),
                format!("{token_id} · mid {midpoint:.4} · P&L {pnl_usd:+.2} USD ({pnl_pct:+.2}%)"),
            )
        }
        OperatorEvent::ExitTriggered {
            token_id,
            reason,
            pnl_pct,
            pnl_usd,
        } => (
            ActivityLevel::Warning,
            "TP/SL".into(),
            format!("{token_id} · {reason} · P&L {pnl_usd:+.2} USD ({pnl_pct:+.2}%)"),
        ),
        OperatorEvent::PositionClosed {
            token_id,
            pnl_pct,
            pnl_usd,
        } => {
            snapshot.counters.exits += 1;
            if let Some(value) = pnl_usd {
                snapshot.realized_pnl_usd += value;
                if value >= 0.0 {
                    snapshot.counters.wins += 1;
                } else {
                    snapshot.counters.losses += 1;
                }
            }
            snapshot.positions.retain(|item| item.token_id != token_id);
            let pnl = match (pnl_usd, pnl_pct) {
                (Some(usd), Some(pct)) => format!(" · P&L {usd:+.2} USD ({pct:+.2}%)"),
                _ => String::new(),
            };
            (
                ActivityLevel::Info,
                "POSITION".into(),
                format!("closed {token_id} session position{pnl}"),
            )
        }
        OperatorEvent::RiskStatus {
            tripped,
            reason,
            until,
        } => {
            snapshot.risk_tripped = tripped;
            snapshot.risk_reason = reason.clone();
            snapshot.risk_until = until;
            if tripped {
                (
                    ActivityLevel::Error,
                    "RISK".into(),
                    reason.unwrap_or_else(|| "circuit breaker tripped".into()),
                )
            } else {
                (
                    ActivityLevel::Info,
                    "RISK".into(),
                    "circuit breaker clear".into(),
                )
            }
        }
        OperatorEvent::RuntimeError { context, message } => {
            snapshot.counters.errors += 1;
            (
                ActivityLevel::Error,
                "ERROR".into(),
                format!("{context}: {message}"),
            )
        }
    }
}

fn side_label(side: Side) -> &'static str {
    match side {
        Side::Buy => "BUY",
        Side::Sell => "SELL",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{OrderType, VenueId};
    use chrono::Utc;

    fn position() -> OpenPosition {
        OpenPosition {
            token_id: "token".into(),
            slug: "market".into(),
            question: "Market?".into(),
            outcome: Some("YES".into()),
            expiry: None,
            category: None,
            tags: vec![],
            side: Side::Buy,
            entry_price: 0.5,
            shares: 10.0,
            usd_notional: 5.0,
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
    fn snapshot_tracks_positions_quotes_and_closes() {
        let bus = OperatorBus::new();
        bus.publish(OperatorEvent::PositionOpened(position()));
        bus.publish(OperatorEvent::PositionQuote {
            token_id: "token".into(),
            midpoint: 0.6,
            pnl_pct: 20.0,
            pnl_usd: 1.0,
            quoted_at: Utc::now(),
        });
        let snapshot = bus.snapshot();
        assert_eq!(snapshot.positions.len(), 1);
        assert_eq!(snapshot.positions[0].midpoint, Some(0.6));
        assert_eq!(snapshot.positions[0].pnl_pct, Some(20.0));

        bus.publish(OperatorEvent::PositionClosed {
            token_id: "token".into(),
            pnl_pct: Some(20.0),
            pnl_usd: Some(1.0),
        });
        let snapshot = bus.snapshot();
        assert!(snapshot.positions.is_empty());
        assert_eq!(snapshot.realized_pnl_usd, 1.0);
        assert_eq!(snapshot.counters.wins, 1);
    }

    #[test]
    fn activity_is_bounded_and_counters_are_distinct() {
        let bus = OperatorBus::new();
        for _ in 0..(ACTIVITY_LIMIT + 5) {
            bus.publish(OperatorEvent::ExecutionSkipped {
                token_id: "token".into(),
                reason: "test".into(),
            });
        }
        bus.publish(OperatorEvent::OrderSimulated(PlannedOrder {
            venue: VenueId::Polymarket,
            token_id: "token".into(),
            side: Side::Buy,
            shares: 10.0,
            limit_price: 0.5,
            usd_notional: 5.0,
            order_type: OrderType::Fak,
            source_trade_hash: None,
            source_shares: None,
            source_usd_notional: None,
            source_maker: None,
            observed_at: None,
        }));
        bus.publish(OperatorEvent::OrderResponse {
            token_id: "token".into(),
            order_id: Some("id".into()),
            success: true,
            detail: "accepted".into(),
        });
        let snapshot = bus.snapshot();
        assert_eq!(snapshot.activity.len(), ACTIVITY_LIMIT);
        assert_eq!(snapshot.counters.skipped, (ACTIVITY_LIMIT + 5) as u64);
        assert_eq!(snapshot.counters.simulated, 1);
        assert_eq!(snapshot.counters.post_responses, 1);
    }
}
