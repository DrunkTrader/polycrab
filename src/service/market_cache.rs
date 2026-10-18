//! In-memory cache for Polymarket market metadata (slug → CLOB token IDs).
//!
//! Backed by the Gamma API. The cache is read-through with a per-entry TTL;
//! callers never await against the network unless the key is missing or stale.

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tracing::debug;

const DEFAULT_TTL: Duration = Duration::from_secs(300);

#[derive(Debug, Clone)]
pub struct MarketInfo {
    pub slug: String,
    pub question: String,
    pub event_title: Option<String>,
    pub yes_token_id: String,
    pub no_token_id: String,
    pub category: Option<String>,
    pub tags: Vec<String>,
    pub closed: bool,
    pub expiry: Option<DateTime<Utc>>,
}

impl MarketInfo {
    /// Returns true if this market metadata describes the given outcome token.
    pub fn contains_token(&self, token_id: &str) -> bool {
        self.yes_token_id == token_id || self.no_token_id == token_id
    }

    pub fn outcome_for_token(&self, token_id: &str) -> Option<&'static str> {
        if self.yes_token_id == token_id {
            Some("YES")
        } else if self.no_token_id == token_id {
            Some("NO")
        } else {
            None
        }
    }

    pub fn display_name(&self) -> &str {
        if self.question.is_empty() {
            self.event_title.as_deref().unwrap_or(&self.slug)
        } else {
            &self.question
        }
    }
}

#[derive(Debug)]
pub struct MarketCache {
    http: Client,
    gamma_base: String,
    /// Slug → (info, when fetched)
    entries: RwLock<HashMap<String, (MarketInfo, Instant)>>,
    /// Token-id → slug, so token-id lookups can hit the slug cache.
    token_index: RwLock<HashMap<String, String>>,
    ttl: Duration,
}

impl MarketCache {
    pub fn new(http: Client, gamma_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            http,
            gamma_base: gamma_base.into(),
            entries: RwLock::new(HashMap::new()),
            token_index: RwLock::new(HashMap::new()),
            ttl: DEFAULT_TTL,
        })
    }

    /// Resolve a CLOB token id (either YES or NO leg) to its full market info.
    /// First consults the in-memory token-index; on miss, queries Gamma.
    pub async fn by_token_id(&self, token_id: &str) -> Result<MarketInfo> {
        if let Some(slug) = self.token_index.read().unwrap().get(token_id).cloned() {
            if let Some(info) = self.peek(&slug) {
                debug!(token_id, slug = %slug, "market metadata cache hit");
                return Ok(info);
            }
        }
        debug!(token_id, "market metadata cache miss; querying Gamma");
        let info = self.fetch_by_token(token_id).await?;
        self.insert(info.clone());
        debug!(token_id, slug = %info.slug, "market metadata cached");
        Ok(info)
    }

    pub fn peek_by_token(&self, token_id: &str) -> Option<MarketInfo> {
        let slug = self.token_index.read().unwrap().get(token_id).cloned()?;
        self.peek(&slug)
    }

    fn peek(&self, slug: &str) -> Option<MarketInfo> {
        let g = self.entries.read().unwrap();
        g.get(slug).and_then(|(info, when)| {
            if when.elapsed() < self.ttl {
                Some(info.clone())
            } else {
                None
            }
        })
    }

    fn insert(&self, info: MarketInfo) {
        let mut e = self.entries.write().unwrap();
        let mut t = self.token_index.write().unwrap();
        t.insert(info.yes_token_id.clone(), info.slug.clone());
        t.insert(info.no_token_id.clone(), info.slug.clone());
        e.insert(info.slug.clone(), (info, Instant::now()));
    }

    async fn fetch_by_token(&self, token_id: &str) -> Result<MarketInfo> {
        // Gamma accepts ?clob_token_ids=<id> on the markets endpoint.
        let url = format!("{}/markets?clob_token_ids={}", self.gamma_base, token_id);
        let body = self.fetch_markets_url(&url).await?;
        let m = body
            .iter()
            .find(|m| {
                if let Ok(parsed) = parse_market(m) {
                    parsed.contains_token(token_id)
                } else {
                    false
                }
            })
            .ok_or_else(|| anyhow!("token_id {token_id} not resolved by gamma"))?;
        parse_market(m)
    }

    async fn fetch_markets_url(&self, url: &str) -> Result<Vec<serde_json::Value>> {
        let resp = self
            .http
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .json::<serde_json::Value>()
            .await?;
        resp.as_array()
            .cloned()
            .ok_or_else(|| anyhow!("gamma /markets returned non-array for {url}"))
    }
}

#[derive(Debug, Deserialize)]
struct ClobTokenIds(Vec<String>);

fn parse_market(m: &serde_json::Value) -> Result<MarketInfo> {
    let slug = m
        .get("slug")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("market missing slug"))?
        .to_string();
    let question = m
        .get("question")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let event_title = m
        .get("eventTitle")
        .or_else(|| m.get("event_title"))
        .or_else(|| m.get("title"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let closed = m.get("closed").and_then(|v| v.as_bool()).unwrap_or(false);
    let expiry = m
        .get("endDate")
        .or_else(|| m.get("end_date"))
        .and_then(|v| v.as_str())
        .and_then(parse_timestamp);

    // Gamma stores the two CLOB token IDs as a JSON-encoded string array.
    // Decode defensively — older endpoints return them as a real array.
    let raw_tokens = m
        .get("clobTokenIds")
        .ok_or_else(|| anyhow!("market missing clobTokenIds"))?;
    let tokens: Vec<String> = match raw_tokens {
        serde_json::Value::String(s) => serde_json::from_str::<ClobTokenIds>(s)?.0,
        serde_json::Value::Array(_) => serde_json::from_value(raw_tokens.clone())?,
        _ => return Err(anyhow!("unrecognised clobTokenIds shape")),
    };
    if tokens.len() < 2 {
        return Err(anyhow!("expected 2 clob token ids, got {}", tokens.len()));
    }

    let category = m
        .get("category")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| match m.get("feeType").and_then(|v| v.as_str()) {
            Some("crypto_fees_v2") => Some("Crypto".into()),
            Some("politics_fees") => Some("Politics".into()),
            Some("sports_fees") => Some("Sports".into()),
            _ => None,
        });

    // `tags` is sometimes an array of strings, sometimes an array of objects
    // with `label` / `slug` fields depending on Gamma version. Read defensively.
    let tags = match m.get("tags") {
        Some(serde_json::Value::Array(arr)) => arr
            .iter()
            .filter_map(|t| {
                if let Some(s) = t.as_str() {
                    Some(s.to_string())
                } else {
                    t.get("label")
                        .and_then(|v| v.as_str())
                        .or_else(|| t.get("slug").and_then(|v| v.as_str()))
                        .map(|s| s.to_string())
                }
            })
            .collect(),
        _ => Vec::new(),
    };

    Ok(MarketInfo {
        slug,
        question,
        event_title,
        yes_token_id: tokens[0].clone(),
        no_token_id: tokens[1].clone(),
        category,
        tags,
        closed,
        expiry,
    })
}

fn parse_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|date| date.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_contract_metadata_and_outcomes() {
        let market = parse_market(&serde_json::json!({
            "slug": "solana-up-or-down",
            "question": "Solana Up or Down?",
            "eventTitle": "Solana Up or Down",
            "endDate": "2026-10-16T13:00:00Z",
            "closed": false,
            "clobTokenIds": ["yes", "no"],
            "category": "Crypto",
            "tags": [{"label": "Solana"}]
        }))
        .unwrap();

        assert_eq!(market.display_name(), "Solana Up or Down?");
        assert_eq!(market.outcome_for_token("yes"), Some("YES"));
        assert_eq!(market.outcome_for_token("no"), Some("NO"));
        assert_eq!(market.tags, vec!["Solana"]);
        assert!(market.expiry.is_some());
    }
}
