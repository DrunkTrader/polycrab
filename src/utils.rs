//! Misc small helpers and order-boundary validation.

use crate::models::{PlannedOrder, WhaleTrade};
use std::fmt;

pub const PRICE_MIN: f64 = 0.01;
pub const PRICE_MAX: f64 = 0.99;
pub const PRICE_TICK: f64 = 0.001;
pub const AMOUNT_SCALE: f64 = 1_000_000.0;
pub const AMOUNT_STEP: f64 = 1.0 / AMOUNT_SCALE;
const QUANTIZATION_EPSILON: f64 = 1e-7;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrderValidationError {
    EmptyTokenId,
    NonFinite(&'static str),
    NonPositive(&'static str),
    OutOfRange(&'static str),
    QuantizedToZero(&'static str),
    IntegerOverflow(&'static str),
}

impl fmt::Display for OrderValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyTokenId => write!(f, "token_id must not be empty"),
            Self::NonFinite(field) => write!(f, "{field} must be finite"),
            Self::NonPositive(field) => write!(f, "{field} must be positive"),
            Self::OutOfRange(field) => write!(f, "{field} is outside the venue range"),
            Self::QuantizedToZero(field) => write!(f, "{field} is zero after quantization"),
            Self::IntegerOverflow(field) => {
                write!(f, "{field} does not fit in scaled integer units")
            }
        }
    }
}

impl std::error::Error for OrderValidationError {}

/// Clamp a Polymarket limit price to the venue's valid band [0.01, 0.99]
/// and round to the 0.001 tick.
pub fn clamp_price(p: f64) -> f64 {
    let p = p.clamp(PRICE_MIN, PRICE_MAX);
    (p / PRICE_TICK).round() * PRICE_TICK
}

/// Convert a USD amount + limit price into venue-precision shares.
///
/// The result is rounded down to six decimal places and never intentionally
/// exceeds the requested USD amount. Invalid inputs return zero and are
/// rejected again by [`quantize_order`] at the order boundary.
pub fn usd_to_shares(usd: f64, price: f64) -> f64 {
    if !usd.is_finite() || !price.is_finite() || usd <= 0.0 || price <= 0.0 {
        return 0.0;
    }
    let mut shares = floor_to_amount_step(usd / price);
    if shares * price > usd {
        shares = (shares - AMOUNT_STEP).max(0.0);
    }
    shares
}

/// Validate the numeric fields of an observed source trade before network I/O.
pub fn validate_trade(trade: &WhaleTrade) -> Result<(), OrderValidationError> {
    if trade.token_id.trim().is_empty() {
        return Err(OrderValidationError::EmptyTokenId);
    }
    validate_positive(trade.shares, "source shares")?;
    validate_positive(trade.price, "source price")?;
    validate_positive(trade.usd_notional, "source notional")?;
    Ok(())
}

/// Quantize and validate an order immediately before it is recorded or signed.
///
/// This keeps the public domain model readable while ensuring that the order
/// reaching either the paper position store or the CLOB uses the same actual
/// quantity, price, and notional.
pub fn quantize_order(mut order: PlannedOrder) -> Result<PlannedOrder, OrderValidationError> {
    if order.token_id.trim().is_empty() {
        return Err(OrderValidationError::EmptyTokenId);
    }
    validate_positive(order.shares, "shares")?;
    validate_positive(order.limit_price, "limit price")?;
    validate_positive(order.usd_notional, "order notional")?;

    let price = (order.limit_price / PRICE_TICK).round() * PRICE_TICK;
    if !(PRICE_MIN..=PRICE_MAX).contains(&price) {
        return Err(OrderValidationError::OutOfRange("limit price"));
    }

    let mut shares = floor_to_amount_step(order.shares);
    if shares * price > order.usd_notional {
        shares = (shares - AMOUNT_STEP).max(0.0);
    }
    if !shares.is_finite() {
        return Err(OrderValidationError::NonFinite("shares"));
    }
    if shares < AMOUNT_STEP {
        return Err(OrderValidationError::QuantizedToZero("shares"));
    }

    let actual_notional = shares * price;
    validate_positive(actual_notional, "quantized notional")?;
    if decimal_units(actual_notional, 6).is_err() {
        return Err(OrderValidationError::IntegerOverflow("order notional"));
    }

    order.shares = shares;
    order.limit_price = price;
    order.usd_notional = round_decimal(actual_notional, 6);
    Ok(order)
}

/// Convert a non-negative decimal value to an exact fixed-point integer.
///
/// Formatting at the requested precision avoids a direct float-to-integer cast,
/// which would otherwise silently saturate or truncate invalid values.
pub fn decimal_units(value: f64, decimals: usize) -> Result<u128, OrderValidationError> {
    if !value.is_finite() {
        return Err(OrderValidationError::NonFinite("decimal value"));
    }
    if value < 0.0 {
        return Err(OrderValidationError::NonPositive("decimal value"));
    }
    let fixed = format!("{value:.decimals$}");
    let (whole, fraction) = fixed.split_once('.').unwrap_or((&fixed, ""));
    let digits = format!("{whole}{fraction}");
    digits
        .parse::<u128>()
        .map_err(|_| OrderValidationError::IntegerOverflow("decimal value"))
}

fn validate_positive(value: f64, field: &'static str) -> Result<(), OrderValidationError> {
    if !value.is_finite() {
        return Err(OrderValidationError::NonFinite(field));
    }
    if value <= 0.0 {
        return Err(OrderValidationError::NonPositive(field));
    }
    Ok(())
}

fn round_decimal(value: f64, decimals: i32) -> f64 {
    let scale = 10f64.powi(decimals);
    (value * scale).round() / scale
}

fn floor_to_amount_step(value: f64) -> f64 {
    ((value * AMOUNT_SCALE) + QUANTIZATION_EPSILON).floor() / AMOUNT_SCALE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamps_below_lower() {
        assert!((clamp_price(0.0001) - 0.01).abs() < 1e-9);
    }

    #[test]
    fn clamps_above_upper() {
        assert!((clamp_price(2.0) - 0.99).abs() < 1e-9);
    }

    #[test]
    fn rounds_to_tick() {
        assert!((clamp_price(0.12345) - 0.123).abs() < 1e-9);
    }

    #[test]
    fn usd_to_shares_keeps_venue_precision() {
        assert!((usd_to_shares(10.0, 0.33) - 30.30303).abs() < 1e-9);
    }

    #[test]
    fn quantize_order_recomputes_actual_notional() {
        let order = PlannedOrder {
            venue: crate::models::VenueId::Polymarket,
            token_id: "1".into(),
            side: crate::models::Side::Buy,
            shares: 3.3333339,
            limit_price: 0.12345,
            usd_notional: 1.0,
            order_type: crate::models::OrderType::Fok,
            source_trade_hash: None,
            source_shares: None,
            source_usd_notional: None,
            source_maker: None,
            observed_at: None,
        };
        let order = quantize_order(order).unwrap();
        assert_eq!(order.shares, 3.333333);
        assert_eq!(order.limit_price, 0.123);
        assert_eq!(order.usd_notional, 0.41);
    }

    #[test]
    fn quantize_order_rejects_zero_and_non_finite_values() {
        let mut order = PlannedOrder {
            venue: crate::models::VenueId::Polymarket,
            token_id: "1".into(),
            side: crate::models::Side::Buy,
            shares: 0.0000001,
            limit_price: 0.5,
            usd_notional: 1.0,
            order_type: crate::models::OrderType::Fok,
            source_trade_hash: None,
            source_shares: None,
            source_usd_notional: None,
            source_maker: None,
            observed_at: None,
        };
        assert!(matches!(
            quantize_order(order.clone()),
            Err(OrderValidationError::QuantizedToZero("shares"))
        ));
        order.shares = f64::NAN;
        assert!(matches!(
            quantize_order(order),
            Err(OrderValidationError::NonFinite("shares"))
        ));
    }

    #[test]
    fn decimal_units_rejects_overflow_instead_of_casting() {
        assert_eq!(decimal_units(1.5, 6).unwrap(), 1_500_000);
        assert!(matches!(
            decimal_units(f64::MAX, 6),
            Err(OrderValidationError::IntegerOverflow("decimal value"))
        ));
    }
}
