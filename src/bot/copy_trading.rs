//! PolyCrab's Polymarket copy-trading bot.
//!
//! Architecture:
//!
//! 1. **Ingestion**: Polygon WebSocket `eth_subscribe`/`logs` filtered to the
//!    configured CTF Exchange V2 contracts, topic-0 = `OrderFilled`, and the
//!    watched whale address packed as topic-2 (maker). Server-side filtering
//!    means the bot is woken only when the whale actually transacts.
//! 2. **Parse**: decode raw logs into [`WhaleTrade`] via [`service::parse`].
//! 3. **Eligibility**: resolve market metadata via Gamma, then check against
//!    the operator's allow/block lists ([`service::eligibility`]).
//! 4. **Sizing**: apply the configured copy strategy ([`service::strategy`]).
//! 5. **Exposure caps**: per-category and per-tag open-USD limits enforced
//!    via [`service::position_store`].
//! 6. **Risk**: fast in-memory check, optional book/depth check
//!    ([`service::risk_guard`]).
//! 7. **Execute**: build EIP-712 signed CTF order, post via L2 auth
//!    ([`service::clob`], [`service::order_executor`]).
//! 8. **TP/SL**: background monitor polls midprice for every open position
//!    and submits an exit FAK when P&L crosses the configured thresholds
//!    ([`service::position_monitor`]).
//!
//! Safety: `enable_trading=false` OR `mock_trading=true` keeps the executor
//! in dry-run mode — the full pipeline runs but simulated orders are recorded,
//! not signed or submitted. Dry-run also records positions locally so the
//! TP/SL monitor exercises against real midprices.

use crate::config::AppConfig;
use crate::operator::{ConnectionStatus, OperatorBus, OperatorEvent, TradingMode};
use crate::service::{
    market_cache::MarketCache,
    midprice::ClobMidpriceSource,
    onchain::{spawn_subscription_with, LogFilter, RawLog},
    order_executor::{ExecutionOutcome, OrderExecutor},
    parse::{decode_whale_trade, order_filled_topic},
    position_monitor,
    position_store::PositionStore,
    risk_guard::RiskGuard,
};
use anyhow::{anyhow, Result};
use reqwest::Client;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;
use tokio::sync::watch;
use tracing::{debug, error, info, warn};

const LOG_CHANNEL_CAPACITY: usize = 256;

pub async fn run(cfg: AppConfig) -> Result<()> {
    let bus = OperatorBus::new();
    let (stop, shutdown) = watch::channel(false);
    let mut task = tokio::spawn(run_with_operator(cfg, bus, shutdown));
    tokio::select! {
        result = &mut task => Ok(result??),
        _ = tokio::signal::ctrl_c() => {
            let _ = stop.send(true);
            task.await??;
            Ok(())
        }
    }
}

pub async fn run_with_operator(
    cfg: AppConfig,
    bus: OperatorBus,
    shutdown: watch::Receiver<bool>,
) -> Result<()> {
    run_with_operator_and_positions(cfg, bus, shutdown, PositionStore::new()).await
}

pub async fn run_with_operator_and_positions(
    cfg: AppConfig,
    bus: OperatorBus,
    mut shutdown: watch::Receiver<bool>,
    positions: Arc<PositionStore>,
) -> Result<()> {
    let whale = cfg
        .bot
        .wallets_to_track
        .first()
        .cloned()
        .ok_or_else(|| anyhow!("config.bot.wallets_to_track is empty"))?;

    info!(
        whale = %whale,
        enable_trading = cfg.bot.enable_trading,
        mock_trading = cfg.bot.mock_trading,
        strict_allowlist = cfg.filters.is_strict(),
        tp_sl_enabled = cfg.tp_sl.enabled,
        "starting copy-trading bot"
    );
    bus.publish(OperatorEvent::BotStarted {
        name: "Copy Trading".into(),
        mode: if cfg.live_trading_allowed() {
            TradingMode::Live
        } else {
            TradingMode::Simulated
        },
        configured_wallets: cfg.bot.wallets_to_track.len(),
        active_wallet: whale.clone(),
    });

    let http = Client::builder().user_agent("polycrab/0.1").build()?;
    let risk = RiskGuard::new(cfg.risk.clone());
    let markets = MarketCache::new(http.clone(), cfg.site.gamma_api_base.clone());
    let executor = OrderExecutor::new(
        cfg.clone(),
        Arc::clone(&risk),
        Arc::clone(&markets),
        Arc::clone(&positions),
    )?;

    // TP/SL can also close positions locally in mock mode without CLOB credentials.
    let monitor = if cfg.tp_sl.enabled {
        let midprice = ClobMidpriceSource::new(http.clone(), cfg.site.clob_api_base.clone());
        let live = cfg.live_trading_allowed();
        let handle = position_monitor::spawn_with(
            cfg.tp_sl.clone(),
            Arc::clone(&positions),
            executor.clob(),
            midprice,
            live,
            cfg.trading.price_buffer,
            cfg.trading.order_expiration_secs,
            bus.clone(),
            shutdown.clone(),
        );
        info!(
            live_trading = live,
            poll_interval_secs = cfg.tp_sl.poll_interval_secs,
            "TP/SL monitor spawned"
        );
        Some(handle)
    } else {
        None
    };

    let filter = build_filter(&cfg, &whale)?;
    info!(
        addresses = ?filter.address,
        topics = ?filter.topics,
        "polygon log filter configured"
    );
    let (tx, mut rx) = mpsc::channel::<RawLog>(LOG_CHANNEL_CAPACITY);
    let sub = spawn_subscription_with(
        cfg.site.polygon_ws_url.clone(),
        filter,
        tx,
        bus.clone(),
        shutdown.clone(),
    );

    loop {
        tokio::select! {
            biased;
            changed = shutdown.changed() => {
                if changed.is_ok() && *shutdown.borrow() {
                    info!(open_positions = positions.len(), "shutdown signal received");
                    break;
                }
            }
            maybe_log = rx.recv() => {
                let Some(log) = maybe_log else {
                    warn!("on-chain subscription channel closed");
                    break;
                };
                if let Err(e) = handle_log(&executor, &whale, &log, &bus, &risk).await {
                    bus.publish(OperatorEvent::RuntimeError {
                        context: format!("trade pipeline {}", log.tx_hash),
                        message: e.to_string(),
                    });
                    error!(error = ?e, tx = %log.tx_hash, "handle_log failed");
                }
            }
        }
    }

    let _ = sub.await;
    if let Some(monitor) = monitor {
        let _ = monitor.await;
    }
    bus.publish(OperatorEvent::Connection {
        status: ConnectionStatus::Stopped,
        detail: Some("copy trading stopped".into()),
    });
    bus.publish(OperatorEvent::BotStopped {
        detail: format!("{} local position(s) remaining", positions.len()),
    });
    Ok(())
}

async fn handle_log(
    executor: &OrderExecutor,
    whale: &str,
    log: &RawLog,
    bus: &OperatorBus,
    risk: &Arc<RiskGuard>,
) -> Result<()> {
    debug!(
        tx = %log.tx_hash,
        address = %log.address,
        topics = log.topics.len(),
        data_bytes = log.data.len().saturating_sub(2) / 2,
        "processing polygon log"
    );
    let Some(trade) = decode_whale_trade(log, whale)? else {
        debug!(tx = %log.tx_hash, "polygon log skipped before trade execution");
        return Ok(());
    };
    info!(
        token = %trade.token_id,
        side = ?trade.side,
        shares = trade.shares,
        usd = trade.usd_notional,
        tx = %log.tx_hash,
        "whale trade detected"
    );
    bus.publish(OperatorEvent::TradeObserved(trade.clone()));
    let started = Instant::now();
    let outcome = executor.execute(&trade).await?;
    let latency_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
    let risk_status = risk.status();
    bus.publish(OperatorEvent::RiskStatus {
        tripped: risk_status.tripped,
        reason: risk_status.reason.map(|reason| format!("{reason:?}")),
        until: risk_status.until,
    });
    let (result, copied_order, reason) = match outcome {
        ExecutionOutcome::Skipped(r) => {
            let reason = format!("{r:?}");
            bus.publish(OperatorEvent::ExecutionSkipped {
                token_id: trade.token_id.clone(),
                reason: reason.clone(),
            });
            info!(
            token = %trade.token_id,
            tx = %log.tx_hash,
            ?r,
            "execution skipped"
            );
            ("SKIPPED".into(), None, Some(reason))
        }
        ExecutionOutcome::DryRun(order) => {
            bus.publish(OperatorEvent::OrderSimulated(order.clone()));
            if let Some(position) = executor.positions().get(&order.token_id) {
                bus.publish(OperatorEvent::PositionOpened(position));
            }
            info!(
            token = %order.token_id,
            side = ?order.side,
            shares = order.shares,
            usd = order.usd_notional,
            "simulated order recorded (not signed or submitted)"
            );
            ("PAPER".into(), Some(order), None)
        }
        ExecutionOutcome::Submitted {
            order,
            order_id,
            success,
            detail,
        } => {
            bus.publish(OperatorEvent::OrderResponse {
                token_id: trade.token_id.clone(),
                order_id: order_id.clone(),
                success,
                detail: detail.clone(),
            });
            if success {
                if let Some(position) = executor.positions().get(&trade.token_id) {
                    bus.publish(OperatorEvent::PositionOpened(position));
                }
            }
            info!(
                order_id = ?order_id,
                success,
                "order submitted to Polymarket CLOB"
            );
            if success {
                ("SUBMITTED".into(), Some(order), None)
            } else {
                ("REJECTED".into(), Some(order), Some(detail))
            }
        }
    };
    let market = executor.cached_market(&trade.token_id);
    bus.publish(OperatorEvent::TradeProcessed {
        trade,
        market: market.map(Box::new),
        order: copied_order.map(Box::new),
        result,
        reason,
        latency_ms,
    });
    Ok(())
}

fn build_filter(cfg: &AppConfig, whale: &str) -> Result<LogFilter> {
    let exchanges = vec![
        cfg.exchange.ctf_exchange_address.to_lowercase(),
        cfg.exchange.neg_risk_exchange_address.to_lowercase(),
    ];
    let topic0 = format!("0x{}", hex::encode(order_filled_topic().as_slice()));
    let whale_topic = pad_address_to_topic(whale)?;
    Ok(LogFilter {
        address: exchanges,
        topics: vec![Some(vec![topic0]), None, Some(vec![whale_topic])],
    })
}

fn pad_address_to_topic(addr: &str) -> Result<String> {
    let trimmed = addr.trim().trim_start_matches("0x").to_lowercase();
    if trimmed.len() != 40 {
        return Err(anyhow!("address must be 20 bytes / 40 hex chars"));
    }
    Ok(format!("0x{}{}", "0".repeat(24), trimmed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_targets_v2_exchange_and_maker_topic() {
        let cfg: AppConfig = serde_json::from_str(include_str!("../../config.json")).unwrap();
        let whale = cfg.bot.wallets_to_track.first().unwrap();
        let filter = build_filter(&cfg, whale).unwrap();

        assert_eq!(
            filter.address,
            vec![
                "0xe111180000d2663c0091e4f400237545b87b996b",
                "0xe2222d279d744050d28e00520010520000310f59"
            ]
        );
        assert_eq!(
            filter.topics[0],
            Some(vec![
                "0xd543adfd945773f1a62f74f0ee55a5e3b9b1a28262980ba90b1a89f2ea84d8ee".into()
            ])
        );
        assert_eq!(filter.topics[1], None);
        assert_eq!(
            filter.topics[2],
            Some(vec![pad_address_to_topic(whale).unwrap()])
        );
    }
}
