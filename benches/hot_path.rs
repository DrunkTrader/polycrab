//! Per-trade hot-path benchmarks.
//!
//! Everything measured here runs between a Polygon log arriving and an order
//! being handed to the CLOB, with no network I/O. The question these answer is
//! "can the decision path keep up with a busy whale", and which step would
//! dominate if it could not.
//!
//! Run with `cargo bench` (or `make bench`). Deliberately dependency-free:
//! a stable `harness = false` target keeps `make test --offline` working.

use std::hint::black_box;
use std::time::Instant;

use polycrab::config::{CopyStrategy, FiltersConfig, RiskConfig, StrategyConfig};
use polycrab::models::{OrderType, PlannedOrder, Side, VenueId, WhaleTrade};
use polycrab::service::eligibility;
use polycrab::service::market_cache::MarketInfo;
use polycrab::service::onchain::RawLog;
use polycrab::service::parse::{decode_whale_trade, order_filled_topic};
use polycrab::service::position_store::{OpenPosition, PositionStore};
use polycrab::service::risk_guard::RiskGuard;
use polycrab::service::strategy;
use polycrab::utils;

const WHALE: &str = "0x1111111111111111111111111111111111111111";

fn bench(name: &str, iters: u32, mut body: impl FnMut()) {
    for _ in 0..(iters / 10).max(1) {
        body();
    }
    let start = Instant::now();
    for _ in 0..iters {
        body();
    }
    let elapsed = start.elapsed().as_secs_f64();
    let per_op = elapsed / f64::from(iters);
    println!(
        "{name:<48}{:>10.0} ns/op{:>14.0} ops/s",
        per_op * 1e9,
        1.0 / per_op
    );
}

fn word(value: u128) -> String {
    format!("{value:064x}")
}

fn order_filled_log() -> RawLog {
    let data = [
        word(0),          // side = BUY
        word(1234),       // tokenId
        word(10_000_000), // makerAmount = 10 USDC
        word(20_000_000), // takerAmount = 20 shares
        word(0),
        word(0),
        word(0),
    ]
    .join("");
    RawLog {
        address: "0xe111180000d2663c0091e4f400237545b87b996b".into(),
        topics: vec![
            format!("0x{}", hex::encode(order_filled_topic().as_slice())),
            format!("0x{}", word(1)),
            format!("0x{}{}", "0".repeat(24), &WHALE[2..]),
            format!("0x{}{}", "0".repeat(24), "2".repeat(40)),
        ],
        data: format!("0x{data}"),
        tx_hash: "0xbench".into(),
        block_number: 1,
        log_index: Some(0),
    }
}

fn trade() -> WhaleTrade {
    WhaleTrade {
        venue: VenueId::Polymarket,
        maker: WHALE.into(),
        side: Side::Buy,
        token_id: "1234".into(),
        shares: 200.0,
        price: 0.5,
        usd_notional: 100.0,
        tx_hash: Some("0xbench".into()),
        block_number: Some(1),
        observed_at: chrono::Utc::now(),
    }
}

fn market(category: &str, tags: &[&str]) -> MarketInfo {
    MarketInfo {
        slug: "bench-market".into(),
        question: "Bench?".into(),
        event_title: None,
        yes_token_id: "1234".into(),
        no_token_id: "5678".into(),
        category: Some(category.into()),
        tags: tags.iter().map(|t| t.to_string()).collect(),
        closed: false,
        expiry: None,
        neg_risk: false,
    }
}

fn strategy_cfg(copy_strategy: CopyStrategy) -> StrategyConfig {
    StrategyConfig {
        copy_strategy,
        copy_size: 20.0,
        trade_multiplier: 1.0,
        min_order_size_usd: 1.0,
        max_order_size_usd: 500.0,
        min_whale_shares_to_copy: 10.0,
        adaptive_threshold_usd: 1000.0,
        adaptive_min_percent: 5.0,
        adaptive_max_percent: 30.0,
    }
}

fn filters() -> FiltersConfig {
    FiltersConfig {
        categories_allow: vec!["Politics".into(), "Sports".into(), "Crypto".into()],
        categories_block: vec!["Memes".into()],
        tags_block: vec!["satire".into()],
        ..Default::default()
    }
}

fn position(token: &str, category: &str, usd: f64) -> OpenPosition {
    OpenPosition {
        token_id: token.into(),
        slug: "bench-market".into(),
        question: "Bench?".into(),
        outcome: Some("YES".into()),
        expiry: None,
        category: Some(category.into()),
        tags: vec!["election2026".into(), "us".into()],
        side: Side::Buy,
        neg_risk: false,
        entry_price: 0.50,
        shares: usd * 2.0,
        usd_notional: usd,
        take_profit_pct: 50.0,
        stop_loss_pct: 30.0,
        opened_at: chrono::Utc::now(),
        source_maker: None,
        source_trade_hash: None,
        source_shares: None,
        source_usd_notional: None,
    }
}

fn planned() -> PlannedOrder {
    PlannedOrder {
        venue: VenueId::Polymarket,
        token_id: "1234".into(),
        side: Side::Buy,
        shares: 40.123_456_7,
        limit_price: 0.505,
        usd_notional: 20.0,
        order_type: OrderType::Fok,
        source_trade_hash: None,
        source_shares: None,
        source_usd_notional: None,
        source_maker: None,
        observed_at: None,
    }
}

fn exposure_store(count: usize) -> std::sync::Arc<PositionStore> {
    let store = PositionStore::new();
    for i in 0..count {
        let category = if i % 2 == 0 { "Politics" } else { "Crypto" };
        store.open(position(&format!("tok-{i}"), category, 25.0));
    }
    store
}

fn main() {
    println!("polycrab hot-path benchmarks (release, single thread)\n");

    // 1. Ingestion: ABI decode of one OrderFilled log.
    let log = order_filled_log();
    bench(
        "parse::decode_whale_trade (matching whale)",
        200_000,
        || {
            black_box(decode_whale_trade(black_box(&log), WHALE).unwrap());
        },
    );

    // The common case on a busy exchange is a log for somebody else. That is
    // rejected before the data payload is decoded, so it should be cheaper.
    let mut other = order_filled_log();
    other.topics[2] = format!("0x{}{}", "0".repeat(24), "3".repeat(40));
    bench("parse::decode_whale_trade (other maker)", 200_000, || {
        black_box(decode_whale_trade(black_box(&other), WHALE).unwrap());
    });

    // 2. Eligibility.
    let allowed = market("Politics", &["election2026"]);
    let blocked = market("Memes", &["satire"]);
    let f = filters();
    bench("eligibility::check (allowed)", 1_000_000, || {
        black_box(eligibility::check(black_box(&f), black_box(&allowed)));
    });
    bench("eligibility::check (blocked)", 1_000_000, || {
        black_box(eligibility::check(black_box(&f), black_box(&blocked)));
    });

    // 3. Sizing.
    let t = trade();
    let pct = strategy_cfg(CopyStrategy::Percentage);
    let adaptive = strategy_cfg(CopyStrategy::Adaptive);
    bench("strategy::size_for_trade (percentage)", 1_000_000, || {
        black_box(strategy::size_for_trade(black_box(&pct), black_box(&t)));
    });
    bench("strategy::size_for_trade (adaptive/log)", 1_000_000, || {
        black_box(strategy::size_for_trade(
            black_box(&adaptive),
            black_box(&t),
        ));
    });

    // 4. Order maths at the venue boundary.
    bench("utils::usd_to_shares", 5_000_000, || {
        black_box(utils::usd_to_shares(black_box(20.0), black_box(0.505)));
    });
    let p = planned();
    bench("utils::quantize_order", 1_000_000, || {
        black_box(utils::quantize_order(black_box(p.clone())).unwrap());
    });

    // 5. Circuit breaker (locks a mutex).
    let guard = RiskGuard::new(RiskConfig {
        large_trade_shares: 1500.0,
        consecutive_trigger: 2,
        sequence_window_secs: 30,
        trip_duration_secs: 120,
    });
    bench("RiskGuard::check_fast (small trade)", 1_000_000, || {
        black_box(guard.check_fast(black_box(&t)));
    });

    // 6. Exposure accounting: an O(open positions) scan under a read lock, run
    // once per candidate trade, so the scale factor matters.
    for count in [10usize, 200] {
        let store = exposure_store(count);
        bench(
            &format!("PositionStore::open_usd_by_category ({count} open)"),
            200_000,
            || {
                black_box(store.open_usd_by_category(black_box("Politics")));
            },
        );
        bench(
            &format!("PositionStore::snapshot ({count} open)"),
            20_000,
            || {
                black_box(store.snapshot());
            },
        );
    }

    // 7. TP/SL evaluation, per position per tick.
    let pos = position("tok-pnl", "Politics", 100.0);
    bench("OpenPosition::pnl_pct", 5_000_000, || {
        black_box(pos.pnl_pct(black_box(0.62)));
    });

    // 8. The whole decision path for one copied trade, minus I/O: decode →
    // eligibility → sizing → price → quantize. This bounds how many whale
    // fills per second one bot can process.
    bench("full decision path (no I/O)", 100_000, || {
        let decoded = decode_whale_trade(black_box(&log), WHALE).unwrap().unwrap();
        if eligibility::check(&f, &allowed) != eligibility::Eligibility::Allowed {
            return;
        }
        let sizing = strategy::size_for_trade(&pct, &decoded);
        let price = utils::clamp_price(decoded.price + 0.005);
        let shares = utils::usd_to_shares(sizing.copy_usd, price);
        let order = utils::quantize_order(PlannedOrder {
            venue: decoded.venue,
            token_id: decoded.token_id,
            side: decoded.side,
            shares,
            limit_price: price,
            usd_notional: sizing.copy_usd,
            order_type: OrderType::Fok,
            source_trade_hash: decoded.tx_hash,
            source_shares: Some(decoded.shares),
            source_usd_notional: Some(decoded.usd_notional),
            source_maker: Some(decoded.maker),
            observed_at: Some(decoded.observed_at),
        })
        .unwrap();
        black_box(order);
    });
}
