//! Midprice quote source for the position monitor.
//!
//! Polymarket exposes a `/midpoint?token_id=X` endpoint that returns the
//! mid between best bid and best ask for a single outcome token. Cheap and
//! exactly what TP/SL polling needs.

use anyhow::{anyhow, Result};
use reqwest::Client;
use serde::Deserialize;

pub struct ClobMidpriceSource {
    http: Client,
    clob_base: String,
}

impl ClobMidpriceSource {
    pub fn new(http: Client, clob_base: impl Into<String>) -> Self {
        Self {
            http,
            clob_base: clob_base.into(),
        }
    }

    pub async fn midprice(&self, token_id: &str) -> Result<f64> {
        let url = format!("{}/midpoint?token_id={}", self.clob_base, token_id);
        let resp = self
            .http
            .get(&url)
            .send()
            .await?
            .error_for_status()?
            .json::<MidpointResponse>()
            .await?;
        parse_midpoint(&resp.mid)
    }
}

fn parse_midpoint(raw: &str) -> Result<f64> {
    let midpoint = raw
        .parse::<f64>()
        .map_err(|e| anyhow!("midpoint not a float: {e}"))?;
    if !midpoint.is_finite() {
        return Err(anyhow!("midpoint must be finite"));
    }
    Ok(midpoint)
}

#[derive(Debug, Deserialize)]
struct MidpointResponse {
    mid: String,
}

#[cfg(test)]
mod tests {
    use super::parse_midpoint;

    #[test]
    fn parses_finite_midpoint() {
        assert_eq!(parse_midpoint("0.42").unwrap(), 0.42);
    }

    #[test]
    fn rejects_non_finite_midpoint() {
        for value in ["NaN", "inf", "-inf"] {
            assert!(parse_midpoint(value).is_err(), "accepted {value}");
        }
    }
}
