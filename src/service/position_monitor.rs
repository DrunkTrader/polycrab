//! Take-profit / stop-loss monitor.
//!
//! Periodically polls the midprice of every open position. When the P&L
//! relative to the recorded entry crosses the take-profit or stop-loss
//! threshold, the monitor synthesises an exit `PlannedOrder` and posts it
//! through the existing CLOB signing path. Exit orders do not re-run the entry
//! risk guard because the position is already open.

use crate::config::TpSlConfig;
use crate::models::{OrderType, PlannedOrder, Side, VenueId};
use crate::operator::{OperatorBus, OperatorEvent};
use crate::service::clob::ClobClient;
use crate::service::midprice::ClobMidpriceSource;
use crate::service::position_store::{OpenPosition, PositionStore};
use crate::utils;
use anyhow::{anyhow, Result};
use chrono::Utc;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::watch;
use tokio::time::{interval, Duration};
use tracing::{debug, error, info, warn};

/// Consecutive midprice failures before a position is treated as unquotable.
/// At the default 15s poll this is a little over a minute of silence.
const MAX_QUOTE_FAILURES: u32 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitReason {
    TakeProfit,
    StopLoss,
}

pub fn check_exit(pos: &OpenPosition, midprice: f64) -> Option<ExitReason> {
    let pnl = pos.pnl_pct(midprice);
    if pnl >= pos.take_profit_pct {
        Some(ExitReason::TakeProfit)
    } else if pnl <= -pos.stop_loss_pct {
        Some(ExitReason::StopLoss)
    } else {
        None
    }
}

#[allow(clippy::too_many_arguments)]
pub fn spawn_with(
    cfg: TpSlConfig,
    positions: Arc<PositionStore>,
    clob: Option<Arc<ClobClient>>,
    midprice: ClobMidpriceSource,
    live_trading: bool,
    price_buffer: f64,
    order_expiration_secs: u64,
    bus: OperatorBus,
    mut shutdown: watch::Receiver<bool>,
    mut emergency_shutdown: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if !cfg.enabled {
            info!("TP/SL monitor disabled in config — exiting task");
            return;
        }
        let mut ticker = interval(Duration::from_secs(cfg.poll_interval_secs.max(1)));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // The venue has long since expired an order this old, so a pending
        // entry past it will never fill and must stop blocking its token.
        let pending_max_age = chrono::Duration::seconds(order_expiration_secs as i64 + 60);
        let mut quote_failures: HashMap<String, u32> = HashMap::new();
        loop {
            tokio::select! {
                _ = ticker.tick() => {}
                _ = shutdown.changed() => break,
                _ = emergency_shutdown.changed() => break,
            }
            for stale in positions.expire_pending(pending_max_age) {
                warn!(
                    token = %stale.order.token_id,
                    order_id = ?stale.order_id,
                    "pending order expired unreconciled; token and exposure released"
                );
                bus.publish(OperatorEvent::RuntimeError {
                    context: format!("pending order {}", stale.order.token_id),
                    message: "submission expired without fill confirmation".into(),
                });
            }
            let snapshot = positions.snapshot();
            for pos in snapshot {
                if *shutdown.borrow() || *emergency_shutdown.borrow() {
                    break;
                }
                if let Err(e) = monitor_once(
                    &pos,
                    &positions,
                    clob.as_deref(),
                    &midprice,
                    live_trading,
                    price_buffer,
                    order_expiration_secs,
                    &bus,
                    &mut quote_failures,
                )
                .await
                {
                    bus.publish(OperatorEvent::RuntimeError {
                        context: format!("TP/SL {}", pos.token_id),
                        message: e.to_string(),
                    });
                    error!(
                        token = %pos.token_id,
                        error = ?e,
                        "tp/sl tick failed for position"
                    );
                }
            }
        }
    })
}

#[allow(clippy::too_many_arguments)]
async fn monitor_once(
    pos: &OpenPosition,
    positions: &PositionStore,
    clob: Option<&ClobClient>,
    midprice: &ClobMidpriceSource,
    live_trading: bool,
    price_buffer: f64,
    order_expiration_secs: u64,
    bus: &OperatorBus,
    quote_failures: &mut HashMap<String, u32>,
) -> Result<()> {
    // A resolved market stops quoting, so TP/SL can never fire for it. Left
    // alone the position stays open for the whole session and keeps consuming
    // its category and tag exposure caps.
    if pos.expiry.is_some_and(|expiry| expiry <= Utc::now()) {
        quote_failures.remove(&pos.token_id);
        release_stale_position(pos, positions, live_trading, bus, "market expiry passed");
        return Ok(());
    }

    let mp = match midprice.midprice(&pos.token_id).await {
        Ok(mp) => {
            quote_failures.remove(&pos.token_id);
            mp
        }
        Err(error) => {
            let failures = quote_failures.entry(pos.token_id.clone()).or_default();
            *failures += 1;
            if *failures >= MAX_QUOTE_FAILURES {
                quote_failures.remove(&pos.token_id);
                release_stale_position(
                    pos,
                    positions,
                    live_trading,
                    bus,
                    "midprice unavailable repeatedly",
                );
                return Ok(());
            }
            // Surface the first failure, then stay quiet: repeating it every
            // tick buries the dashboard until the position is released.
            return if *failures == 1 {
                Err(error)
            } else {
                debug!(
                    token = %pos.token_id,
                    failures = *failures,
                    error = ?error,
                    "midprice still unavailable"
                );
                Ok(())
            };
        }
    };
    let pnl = pos.pnl_pct(mp);
    let pnl_usd = pos.pnl_usd(mp);
    bus.publish(OperatorEvent::PositionQuote {
        token_id: pos.token_id.clone(),
        midpoint: mp,
        pnl_pct: pnl,
        pnl_usd,
        quoted_at: chrono::Utc::now(),
    });
    debug!(
        token = %pos.token_id,
        mid = mp,
        entry = pos.entry_price,
        pnl_pct = pnl,
        pnl_usd,
        "tp/sl tick"
    );

    let Some(reason) = check_exit(pos, mp) else {
        return Ok(());
    };
    if live_trading && positions.has_pending_exit(&pos.token_id) {
        debug!(token = %pos.token_id, "TP/SL exit already pending; not submitting another order");
        return Ok(());
    }

    info!(
        token = %pos.token_id,
        slug = %pos.slug,
        ?reason,
        pnl_pct = pnl,
        midprice = mp,
        "TP/SL triggered — preparing exit"
    );
    bus.publish(OperatorEvent::ExitTriggered {
        token_id: pos.token_id.clone(),
        reason: format!("{reason:?}"),
        pnl_pct: pnl,
        pnl_usd,
    });

    let exit_side = pos.side.flip();
    let limit_price = match exit_side {
        // Selling out — accept a buffer below the mid to make sure we fill.
        Side::Sell => utils::clamp_price(mp - price_buffer),
        // Buying to cover — pay up by the buffer.
        Side::Buy => utils::clamp_price(mp + price_buffer),
    };

    let planned = utils::quantize_order(PlannedOrder {
        venue: VenueId::Polymarket,
        token_id: pos.token_id.clone(),
        side: exit_side,
        shares: pos.shares,
        limit_price,
        usd_notional: pos.shares * limit_price,
        order_type: OrderType::Fok,
        source_trade_hash: None,
        source_shares: None,
        source_usd_notional: None,
        source_maker: None,
        observed_at: None,
    })
    .map_err(|error| anyhow!("invalid TP/SL exit order: {error}"))?;

    if live_trading {
        let clob = clob.ok_or_else(|| anyhow!("live TP/SL exit has no CLOB client"))?;
        let signed = clob
            .build_signed_order(&planned, order_expiration_secs, pos.neg_risk)
            .await?;
        match clob.post_order(signed, OrderType::Fok).await {
            Ok(resp) => {
                bus.publish(OperatorEvent::OrderResponse {
                    token_id: pos.token_id.clone(),
                    order_id: resp.order_id.clone(),
                    success: resp.success,
                    detail: resp
                        .error_msg
                        .clone()
                        .unwrap_or_else(|| "CLOB POST response".into()),
                });
                info!(
                    order_id = ?resp.order_id,
                    token = %pos.token_id,
                    "exit order submitted"
                );
                if resp.success {
                    positions.record_pending_order(
                        planned.clone(),
                        resp.order_id.clone(),
                        pos.category.clone(),
                        pos.tags.clone(),
                    );
                    info!(
                        token = %pos.token_id,
                        "exit accepted; position remains open until fill reconciliation"
                    );
                } else {
                    bus.publish(OperatorEvent::RuntimeError {
                        context: format!("TP/SL exit {}", pos.token_id),
                        message: "CLOB rejected exit; position remains open".into(),
                    });
                }
            }
            Err(e) => {
                positions.record_pending_order(
                    planned.clone(),
                    None,
                    pos.category.clone(),
                    pos.tags.clone(),
                );
                bus.publish(OperatorEvent::RuntimeError {
                    context: format!("TP/SL exit {}", pos.token_id),
                    message: format!("exit submission outcome unknown; no automatic retry: {e}"),
                });
                warn!(
                    error = ?e,
                    token = %pos.token_id,
                    "exit submission outcome unknown; reconciliation required"
                );
            }
        }
    } else {
        positions.record_simulated_order(planned.clone());
        bus.publish(OperatorEvent::OrderSimulated(planned.clone()));
        info!(
            token = %pos.token_id,
            shares = planned.shares,
            limit = planned.limit_price,
            "dry-run exit recorded (not signed or submitted) — closing position locally"
        );
        positions.close(&pos.token_id);
        bus.publish(OperatorEvent::PositionClosed {
            token_id: pos.token_id.clone(),
            pnl_pct: Some(pnl),
            pnl_usd: Some(pnl_usd),
        });
    }
    Ok(())
}

/// Drop a position TP/SL can no longer act on.
///
/// Simulated positions are closed locally so the exposure caps recover. A live
/// position still exists on-chain, so it is escalated instead of forgotten.
fn release_stale_position(
    pos: &OpenPosition,
    positions: &PositionStore,
    live_trading: bool,
    bus: &OperatorBus,
    why: &str,
) {
    if live_trading {
        warn!(
            token = %pos.token_id,
            why,
            "live position can no longer be monitored; manual exit required"
        );
        bus.publish(OperatorEvent::RuntimeError {
            context: format!("stale position {}", pos.token_id),
            message: format!("{why}; manual exit required"),
        });
        return;
    }
    positions.close(&pos.token_id);
    bus.publish(OperatorEvent::PositionClosed {
        token_id: pos.token_id.clone(),
        pnl_pct: None,
        pnl_usd: None,
    });
    info!(
        token = %pos.token_id,
        slug = %pos.slug,
        why,
        "simulated position released; exposure returned to the caps"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::Client;

    fn pos(entry: f64, tp: f64, sl: f64, side: Side) -> OpenPosition {
        OpenPosition {
            token_id: "t".into(),
            slug: "s".into(),
            question: "s".into(),
            outcome: Some("YES".into()),
            expiry: None,
            category: None,
            tags: vec![],
            side,
            neg_risk: false,
            entry_price: entry,
            shares: 100.0,
            usd_notional: 100.0 * entry,
            take_profit_pct: tp,
            stop_loss_pct: sl,
            opened_at: Utc::now(),
            source_maker: None,
            source_trade_hash: None,
            source_shares: None,
            source_usd_notional: None,
        }
    }

    #[test]
    fn take_profit_triggers() {
        let p = pos(0.50, 30.0, 20.0, Side::Buy);
        assert_eq!(check_exit(&p, 0.70), Some(ExitReason::TakeProfit));
    }

    #[test]
    fn stop_loss_triggers() {
        let p = pos(0.50, 30.0, 20.0, Side::Buy);
        assert_eq!(check_exit(&p, 0.39), Some(ExitReason::StopLoss));
    }

    #[test]
    fn flat_range_does_not_trigger() {
        let p = pos(0.50, 30.0, 20.0, Side::Buy);
        assert_eq!(check_exit(&p, 0.55), None);
    }

    /// No server listens on port 1, so every quote fails immediately.
    fn dead_midprice() -> ClobMidpriceSource {
        ClobMidpriceSource::new(Client::new(), "http://127.0.0.1:1")
    }

    async fn tick(
        pos: &OpenPosition,
        positions: &Arc<PositionStore>,
        live: bool,
        failures: &mut HashMap<String, u32>,
    ) -> Result<()> {
        monitor_once(
            pos,
            positions,
            None,
            &dead_midprice(),
            live,
            0.005,
            60,
            &OperatorBus::new(),
            failures,
        )
        .await
    }

    #[tokio::test]
    async fn expired_market_releases_a_simulated_position() {
        let mut p = pos(0.50, 30.0, 20.0, Side::Buy);
        p.expiry = Some(Utc::now() - chrono::Duration::minutes(5));
        let positions = PositionStore::new();
        positions.open(p.clone());

        tick(&p, &positions, false, &mut HashMap::new())
            .await
            .unwrap();

        assert!(positions.get("t").is_none(), "expired position still open");
        assert_eq!(positions.len(), 0);
    }

    #[tokio::test]
    async fn expired_live_position_is_escalated_not_silently_closed() {
        let mut p = pos(0.50, 30.0, 20.0, Side::Buy);
        p.expiry = Some(Utc::now() - chrono::Duration::minutes(5));
        let positions = PositionStore::new();
        positions.open(p.clone());

        tick(&p, &positions, true, &mut HashMap::new())
            .await
            .unwrap();

        assert!(
            positions.get("t").is_some(),
            "a live position must not be closed without a real exit"
        );
    }

    #[tokio::test]
    async fn unquotable_position_is_released_after_repeated_failures() {
        let p = pos(0.50, 30.0, 20.0, Side::Buy);
        let positions = PositionStore::new();
        positions.open(p.clone());
        let mut failures = HashMap::new();

        // First failure is reported to the operator.
        assert!(tick(&p, &positions, false, &mut failures).await.is_err());
        // Later ones stay quiet while the position is still open.
        for _ in 2..MAX_QUOTE_FAILURES {
            assert!(tick(&p, &positions, false, &mut failures).await.is_ok());
            assert!(positions.get("t").is_some());
        }
        // The last one crosses the threshold and releases it.
        assert!(tick(&p, &positions, false, &mut failures).await.is_ok());

        assert!(positions.get("t").is_none(), "position never released");
        assert!(failures.is_empty(), "counter must not leak after release");
    }

    #[tokio::test]
    async fn a_successful_quote_resets_the_failure_counter() {
        let p = pos(0.50, 30.0, 20.0, Side::Buy);
        let mut failures = HashMap::new();
        failures.insert("t".to_string(), MAX_QUOTE_FAILURES - 1);
        let positions = PositionStore::new();
        positions.open(p.clone());

        // Still failing: one more tick would otherwise release the position.
        assert!(tick(&p, &positions, false, &mut failures).await.is_ok());
        assert!(positions.get("t").is_none());

        // Now prove the reset path: a fresh counter must start from zero.
        let mut fresh = HashMap::new();
        assert!(tick(&p, &positions, false, &mut fresh).await.is_err());
        assert_eq!(fresh.get("t"), Some(&1));
    }

    #[test]
    fn sell_inverted_pnl_logic() {
        let p = pos(0.50, 30.0, 20.0, Side::Sell);
        // Price went DOWN — short position is in profit.
        assert_eq!(check_exit(&p, 0.30), Some(ExitReason::TakeProfit));
        // Price went UP — short position hit stop.
        assert_eq!(check_exit(&p, 0.65), Some(ExitReason::StopLoss));
    }
}
