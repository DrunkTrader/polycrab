//! Circuit breaker for the hot path.
//!
//! Counts consecutive large trades per outcome token within a rolling window
//! and trips synchronously without any I/O. A single large trade is reported
//! as [`RiskCheck::DepthUnverified`]: there is no order-book provider, so the
//! caller decides whether to continue (paper) or fail closed (live).
//!
//! The breaker honours `trip_duration_secs`: once it fires, every subsequent
//! decision is blocked until the cooldown elapses.

use crate::config::RiskConfig;
use crate::models::WhaleTrade;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RiskCheck {
    Allow,
    /// Large trade with no order-book depth behind it. Live callers must fail
    /// closed; paper callers may continue.
    DepthUnverified,
    Block(BlockReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockReason {
    Tripped,
    ConsecutiveLargeTrades,
    BookUnavailable,
}

#[derive(Debug)]
pub struct RiskGuard {
    cfg: RiskConfig,
    inner: Mutex<Inner>,
}

#[derive(Debug)]
struct Inner {
    /// token_id → ring buffer of "large trade" timestamps.
    history: HashMap<String, VecDeque<Instant>>,
    tripped_until: Option<Instant>,
    trip_reason: Option<BlockReason>,
}

#[derive(Debug, Clone)]
pub struct RiskGuardStatus {
    pub tripped: bool,
    pub reason: Option<BlockReason>,
    pub until: Option<Instant>,
}

impl RiskGuard {
    pub fn new(cfg: RiskConfig) -> Arc<Self> {
        Arc::new(Self {
            cfg,
            inner: Mutex::new(Inner {
                history: HashMap::new(),
                tripped_until: None,
                trip_reason: None,
            }),
        })
    }

    /// Synchronous, in-memory only. Call before any network I/O.
    pub fn check_fast(&self, trade: &WhaleTrade) -> RiskCheck {
        let now = Instant::now();
        let mut g = self.inner.lock().unwrap();

        if let Some(until) = g.tripped_until {
            if now < until {
                return RiskCheck::Block(BlockReason::Tripped);
            }
            g.tripped_until = None;
            g.trip_reason = None;
        }

        if trade.shares < self.cfg.large_trade_shares {
            return RiskCheck::Allow;
        }

        let window = Duration::from_secs(self.cfg.sequence_window_secs);
        // Drop buckets whose newest entry is already expired, so the map does
        // not retain one key per token seen over the life of the process.
        g.history.retain(|_, bucket| {
            bucket
                .back()
                .map(|&last| now.duration_since(last) <= window)
                .unwrap_or(false)
        });
        let bucket = g.history.entry(trade.token_id.clone()).or_default();

        // Drop expired entries from the head.
        while let Some(&front) = bucket.front() {
            if now.duration_since(front) > window {
                bucket.pop_front();
            } else {
                break;
            }
        }
        bucket.push_back(now);

        if bucket.len() as u32 >= self.cfg.consecutive_trigger {
            g.tripped_until = Some(now + Duration::from_secs(self.cfg.trip_duration_secs));
            g.trip_reason = Some(BlockReason::ConsecutiveLargeTrades);
            return RiskCheck::Block(BlockReason::ConsecutiveLargeTrades);
        }

        // Large trade with no depth data behind it — let the caller decide.
        RiskCheck::DepthUnverified
    }

    /// Manual trip — e.g. when depth cannot be verified, prefer tripping to
    /// firing blind.
    pub fn trip(&self, reason: BlockReason) {
        let mut g = self.inner.lock().unwrap();
        g.tripped_until = Some(Instant::now() + Duration::from_secs(self.cfg.trip_duration_secs));
        g.trip_reason = Some(reason);
    }

    pub fn is_tripped(&self) -> bool {
        let g = self.inner.lock().unwrap();
        g.tripped_until.map(|t| Instant::now() < t).unwrap_or(false)
    }

    pub fn status(&self) -> RiskGuardStatus {
        let mut g = self.inner.lock().unwrap();
        let now = Instant::now();
        if let Some(until) = g.tripped_until {
            if now >= until {
                g.tripped_until = None;
                g.trip_reason = None;
            }
        }
        RiskGuardStatus {
            tripped: g.tripped_until.is_some(),
            reason: g.trip_reason.clone(),
            until: g.tripped_until,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Side, VenueId};

    fn cfg() -> RiskConfig {
        RiskConfig {
            large_trade_shares: 1000.0,
            consecutive_trigger: 2,
            sequence_window_secs: 30,
            trip_duration_secs: 60,
        }
    }

    fn trade(shares: f64) -> WhaleTrade {
        WhaleTrade {
            venue: VenueId::Polymarket,
            maker: "0x".into(),
            side: Side::Buy,
            token_id: "tok".into(),
            shares,
            price: 0.5,
            usd_notional: shares * 0.5,
            tx_hash: None,
            block_number: None,
            observed_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn small_trades_always_pass() {
        let g = RiskGuard::new(cfg());
        assert_eq!(g.check_fast(&trade(10.0)), RiskCheck::Allow);
        assert_eq!(g.check_fast(&trade(10.0)), RiskCheck::Allow);
    }

    #[test]
    fn first_large_reports_unverified_depth() {
        let g = RiskGuard::new(cfg());
        assert_eq!(g.check_fast(&trade(2000.0)), RiskCheck::DepthUnverified);
    }

    #[test]
    fn second_consecutive_large_trips() {
        let g = RiskGuard::new(cfg());
        let _ = g.check_fast(&trade(2000.0));
        let r = g.check_fast(&trade(2000.0));
        assert!(matches!(
            r,
            RiskCheck::Block(BlockReason::ConsecutiveLargeTrades)
        ));
        assert!(g.is_tripped());
        let status = g.status();
        assert!(status.tripped);
        assert_eq!(status.reason, Some(BlockReason::ConsecutiveLargeTrades));
        assert!(status.until.is_some());
    }

    #[test]
    fn expired_token_history_is_dropped_instead_of_growing() {
        let mut config = cfg();
        config.sequence_window_secs = 0; // every prior entry is already expired
        let g = RiskGuard::new(config);
        for token in 0..50 {
            let mut t = trade(2000.0);
            t.token_id = format!("tok-{token}");
            assert_eq!(g.check_fast(&t), RiskCheck::DepthUnverified);
        }
        // Only the token from the most recent call is retained.
        assert_eq!(g.inner.lock().unwrap().history.len(), 1);
    }
}
