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
use std::sync::Arc;
use tokio::sync::watch;
use tokio::time::{interval, Duration};
use tracing::{debug, error, info, warn};

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
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if !cfg.enabled {
            info!("TP/SL monitor disabled in config — exiting task");
            return;
        }
        let mut ticker = interval(Duration::from_secs(cfg.poll_interval_secs.max(1)));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = ticker.tick() => {}
                _ = shutdown.changed() => break,
            }
            let snapshot = positions.snapshot();
            for pos in snapshot {
                if let Err(e) = monitor_once(
                    &pos,
                    &positions,
                    clob.as_deref(),
                    &midprice,
                    live_trading,
                    price_buffer,
                    order_expiration_secs,
                    &bus,
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
) -> Result<()> {
    let mp = midprice.midprice(&pos.token_id).await?;
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

    let planned = PlannedOrder {
        venue: VenueId::Polymarket,
        token_id: pos.token_id.clone(),
        side: exit_side,
        shares: pos.shares,
        limit_price,
        usd_notional: pos.shares * limit_price,
        order_type: OrderType::Fak,
        source_trade_hash: None,
        source_shares: None,
        source_usd_notional: None,
        source_maker: None,
        observed_at: None,
    };

    if live_trading {
        let clob = clob.ok_or_else(|| anyhow!("live TP/SL exit has no CLOB client"))?;
        let signed = clob
            .build_signed_order(&planned, order_expiration_secs)
            .await?;
        match clob.post_order(signed, OrderType::Fak).await {
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
                    positions.close(&pos.token_id);
                    bus.publish(OperatorEvent::PositionClosed {
                        token_id: pos.token_id.clone(),
                        pnl_pct: Some(pnl),
                        pnl_usd: Some(pnl_usd),
                    });
                } else {
                    bus.publish(OperatorEvent::RuntimeError {
                        context: format!("TP/SL exit {}", pos.token_id),
                        message: "CLOB rejected exit; position remains open".into(),
                    });
                }
            }
            Err(e) => {
                bus.publish(OperatorEvent::RuntimeError {
                    context: format!("TP/SL exit {}", pos.token_id),
                    message: e.to_string(),
                });
                warn!(error = ?e, token = %pos.token_id, "exit POST failed; will retry next tick");
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

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

    #[test]
    fn sell_inverted_pnl_logic() {
        let p = pos(0.50, 30.0, 20.0, Side::Sell);
        // Price went DOWN — short position is in profit.
        assert_eq!(check_exit(&p, 0.30), Some(ExitReason::TakeProfit));
        // Price went UP — short position hit stop.
        assert_eq!(check_exit(&p, 0.65), Some(ExitReason::StopLoss));
    }
}
