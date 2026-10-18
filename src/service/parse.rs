//! ABI log decoding for Polymarket CTF Exchange V2 events.
//!
//! Decodes `OrderFilled` and projects it into the [`WhaleTrade`] canonical
//! view. Topic-0 selectors are recomputed from the canonical event signature.

use crate::models::{Side, VenueId, WhaleTrade};
use alloy_primitives::{B256, U256};
use alloy_sol_types::{sol, SolEvent};
use anyhow::{anyhow, Result};

use super::onchain::RawLog;

sol! {
    /// CTF Exchange V2 — fired once per filled maker order.
    event OrderFilled(
        bytes32 indexed orderHash,
        address indexed maker,
        address indexed taker,
        uint8 side,
        uint256 tokenId,
        uint256 makerAmountFilled,
        uint256 takerAmountFilled,
        uint256 fee,
        bytes32 builder,
        bytes32 metadata
    );
}

/// Topic-0 selector for `OrderFilled`, derived once at compile time via the
/// `sol!` macro.
pub fn order_filled_topic() -> B256 {
    OrderFilled::SIGNATURE_HASH
}

/// Returns `Ok(Some(trade))` if the log is an `OrderFilled` for the given whale
/// as maker, `Ok(None)` if it is not relevant, or `Err` on a malformed log.
pub fn decode_whale_trade(log: &RawLog, whale_address: &str) -> Result<Option<WhaleTrade>> {
    use tracing::debug;

    let whale = whale_address.to_lowercase();
    let target_topic = order_filled_topic();

    let topic0 = log
        .topics
        .first()
        .ok_or_else(|| anyhow!("log has no topic0"))?;
    let topic0_b256: B256 = topic0
        .parse()
        .map_err(|e| anyhow!("topic0 not hex bytes32: {e}"))?;
    if topic0_b256 != target_topic {
        debug!(
            tx = %log.tx_hash,
            topic0 = %topic0,
            "on-chain log skipped: not OrderFilled V2"
        );
        return Ok(None);
    }

    if log.topics.len() < 4 {
        debug!(
            tx = %log.tx_hash,
            topics = log.topics.len(),
            "on-chain log skipped: OrderFilled has incomplete topics"
        );
        return Ok(None);
    }
    let maker = topic_to_address(&log.topics[2])?;
    let taker = topic_to_address(&log.topics[3])?;
    if maker.to_lowercase() != whale {
        debug!(
            tx = %log.tx_hash,
            maker = %maker,
            taker = %taker,
            "on-chain log skipped: maker is not tracked whale"
        );
        return Ok(None);
    }

    let data_bytes = hex::decode(log.data.trim_start_matches("0x")).map_err(|e| anyhow!(e))?;
    if data_bytes.len() < 7 * 32 {
        return Err(anyhow!("OrderFilled V2 data shorter than 7*32 bytes"));
    }

    let side = match U256::from_be_slice(&data_bytes[0..32]) {
        v if v == U256::ZERO => Side::Buy,
        v if v == U256::from(1u8) => Side::Sell,
        v => return Err(anyhow!("unsupported OrderFilled V2 side {v}")),
    };
    let token_id = U256::from_be_slice(&data_bytes[32..64]).to_string();
    let maker_amount = U256::from_be_slice(&data_bytes[64..96]);
    let taker_amount = U256::from_be_slice(&data_bytes[96..128]);

    // BUY: maker pays collateral and receives outcome shares.
    // SELL: maker gives outcome shares and receives collateral.
    let (shares, usd_notional) = match side {
        Side::Buy => (
            to_f64_with_decimals(taker_amount, 6),
            to_f64_with_decimals(maker_amount, 6),
        ),
        Side::Sell => (
            to_f64_with_decimals(maker_amount, 6),
            to_f64_with_decimals(taker_amount, 6),
        ),
    };
    let price = if shares > 0.0 {
        usd_notional / shares
    } else {
        0.0
    };

    debug!(
        tx = %log.tx_hash,
        maker = %maker,
        taker = %taker,
        ?side,
        token = %token_id,
        shares,
        usd = usd_notional,
        "OrderFilled V2 decoded for tracked whale"
    );

    Ok(Some(WhaleTrade {
        venue: VenueId::Polymarket,
        maker,
        side,
        token_id,
        shares,
        price,
        usd_notional,
        tx_hash: Some(log.tx_hash.clone()),
        block_number: Some(log.block_number),
        observed_at: chrono::Utc::now(),
    }))
}

fn topic_to_address(topic: &str) -> Result<String> {
    let bytes = hex::decode(topic.trim_start_matches("0x")).map_err(|e| anyhow!(e))?;
    if bytes.len() != 32 {
        return Err(anyhow!("address topic not 32 bytes"));
    }
    Ok(format!("0x{}", hex::encode(&bytes[12..32])))
}

fn to_f64_with_decimals(v: U256, decimals: u32) -> f64 {
    // Safe for typical USDC amounts; clamps if overflowing f64 mantissa.
    let s = v.to_string();
    s.parse::<f64>().unwrap_or(0.0) / 10f64.powi(decimals as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHALE: &str = "0x1111111111111111111111111111111111111111";
    const TAKER: &str = "0x2222222222222222222222222222222222222222";

    fn word(value: u128) -> String {
        format!("{value:064x}")
    }

    fn topic_address(address: &str) -> String {
        format!("0x{}{}", "0".repeat(24), &address[2..])
    }

    fn v2_log(side: u128, token_id: u128, maker_amount: u128, taker_amount: u128) -> RawLog {
        let data = [
            word(side),
            word(token_id),
            word(maker_amount),
            word(taker_amount),
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
                topic_address(WHALE),
                topic_address(TAKER),
            ],
            data: format!("0x{data}"),
            tx_hash: "0xtest".into(),
            block_number: 1,
        }
    }

    #[test]
    fn uses_ctf_exchange_v2_order_filled_selector() {
        assert_eq!(
            hex::encode(order_filled_topic().as_slice()),
            "d543adfd945773f1a62f74f0ee55a5e3b9b1a28262980ba90b1a89f2ea84d8ee"
        );
    }

    #[test]
    fn decodes_v2_buy_with_maker_and_taker_topics() {
        let trade = decode_whale_trade(&v2_log(0, 1234, 10_000_000, 20_000_000), WHALE)
            .unwrap()
            .unwrap();

        assert_eq!(trade.maker, WHALE);
        assert_eq!(trade.side, Side::Buy);
        assert_eq!(trade.token_id, "1234");
        assert!((trade.shares - 20.0).abs() < 1e-9);
        assert!((trade.usd_notional - 10.0).abs() < 1e-9);
        assert!((trade.price - 0.5).abs() < 1e-9);
    }

    #[test]
    fn decodes_v2_sell_amounts() {
        let trade = decode_whale_trade(&v2_log(1, 5678, 20_000_000, 10_000_000), WHALE)
            .unwrap()
            .unwrap();

        assert_eq!(trade.side, Side::Sell);
        assert_eq!(trade.token_id, "5678");
        assert!((trade.shares - 20.0).abs() < 1e-9);
        assert!((trade.usd_notional - 10.0).abs() < 1e-9);
    }

    #[test]
    fn ignores_non_whale_maker() {
        let mut log = v2_log(0, 1234, 10_000_000, 20_000_000);
        log.topics[2] = topic_address("0x3333333333333333333333333333333333333333");
        assert!(decode_whale_trade(&log, WHALE).unwrap().is_none());
    }
}
