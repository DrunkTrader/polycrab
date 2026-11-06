//! Polymarket CLOB v2 client — EIP-712 order signing + L2 HMAC auth + POST.
//!
//! The Polymarket exchange expects orders as EIP-712 typed data signed by the
//! maker EOA (or a delegated signer for proxy/Safe accounts) against the
//! `Polymarket CTF Exchange` domain on chain 137. The signed payload is then
//! posted to `clob.polymarket.com/order` with L2 HMAC headers derived from
//! per-account API credentials.
//!
//! Safety: every public entry-point honours `enable_trading` and
//! `mock_trading`. The hot path will not transmit a signed order until both
//! flags are explicitly permissive (see [`AppConfig::live_trading_allowed`]).

use crate::config::{AppConfig, ExchangeConfig};
use crate::models::{OrderType, PlannedOrder, Side};
use crate::utils;
use alloy_primitives::{Address, B256, U256};
use alloy_signer::Signer;
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::{eip712_domain, sol, Eip712Domain, SolStruct};
use anyhow::{anyhow, Context, Result};
use base64::{
    engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD},
    Engine as _,
};
use hmac::{Hmac, Mac};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::str::FromStr;
use std::time::Duration;

sol! {
    /// Polymarket CTF Exchange order — EIP-712 typed data.
    ///
    /// NOTE: `side` and `signatureType` are encoded as `uint256` to match
    /// Polymarket's on-chain `Order` struct. If you point at a fork that
    /// declares them as `uint8`, narrow them here — the EIP-712 typehash
    /// embeds the field types verbatim, so this MUST match the deployed
    /// contract exactly or signatures will be rejected.
    struct Order {
        uint256 salt;
        address maker;
        address signer;
        address taker;
        uint256 tokenId;
        uint256 makerAmount;
        uint256 takerAmount;
        uint256 expiration;
        uint256 nonce;
        uint256 feeRateBps;
        uint256 side;
        uint256 signatureType;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SignatureType {
    Eoa = 0,
    PolyProxy = 1,
}

#[derive(Debug, Clone, Serialize)]
pub struct SignedOrder {
    pub salt: String,
    pub maker: String,
    pub signer: String,
    pub taker: String,
    #[serde(rename = "tokenId")]
    pub token_id: String,
    #[serde(rename = "makerAmount")]
    pub maker_amount: String,
    #[serde(rename = "takerAmount")]
    pub taker_amount: String,
    pub side: String,
    pub expiration: String,
    pub nonce: String,
    #[serde(rename = "feeRateBps")]
    pub fee_rate_bps: String,
    #[serde(rename = "signatureType")]
    pub signature_type: u8,
    pub signature: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct OrderPostBody {
    pub order: SignedOrder,
    pub owner: String,
    #[serde(rename = "orderType")]
    pub order_type: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OrderResponse {
    #[serde(default, rename = "orderID")]
    pub order_id: Option<String>,
    #[serde(default)]
    pub success: bool,
    #[serde(default, rename = "errorMsg")]
    pub error_msg: Option<String>,
}

pub struct ClobClient {
    http: Client,
    clob_base: String,
    signer: PrivateKeySigner,
    funder: Address,
    exchange: ExchangeConfig,
    fee_rate_bps: u32,
    signature_type: SignatureType,

    api_key: Option<String>,
    api_secret: Option<String>,
    api_passphrase: Option<String>,
}

const NETWORK_TIMEOUT: Duration = Duration::from_secs(15);

impl ClobClient {
    pub fn new(cfg: &AppConfig) -> Result<Self> {
        if !cfg.live_trading_allowed() {
            anyhow::bail!("CLOB client requires an explicit live-trading request");
        }
        cfg.ensure_live_trading_ready()?;
        let pk = parse_private_key(&cfg.credentials.private_key).context("loading private key")?;
        let signer = PrivateKeySigner::from_bytes(&B256::from(pk))
            .context("creating EOA signer from private key")?;
        let funder =
            Address::from_str(&cfg.credentials.funder_address).context("parsing funder address")?;

        // If a funder is provided that differs from the signer, we're using a
        // proxy/Safe; otherwise standard EOA signing.
        let signature_type = if funder == signer.address() {
            SignatureType::Eoa
        } else {
            SignatureType::PolyProxy
        };

        Ok(Self {
            http: Client::builder()
                .user_agent("polycrab/0.1")
                .timeout(NETWORK_TIMEOUT)
                .build()?,
            clob_base: cfg.site.clob_api_base.clone(),
            signer,
            funder,
            exchange: cfg.exchange.clone(),
            fee_rate_bps: cfg.trading.fee_rate_bps,
            signature_type,
            api_key: cfg.credentials.api_key.clone(),
            api_secret: cfg.credentials.api_secret.clone(),
            api_passphrase: cfg.credentials.api_passphrase.clone(),
        })
    }

    /// Build, sign, and prepare an order without posting it. Useful for unit
    /// tests and dry-run logging.
    pub async fn build_signed_order(
        &self,
        planned: &PlannedOrder,
        expiration_secs: u64,
        neg_risk: bool,
    ) -> Result<SignedOrder> {
        let planned = utils::quantize_order(planned.clone())
            .map_err(|error| anyhow!("invalid order before signing: {error}"))?;
        let token_id_u256 = U256::from_str(&planned.token_id)
            .map_err(|_| anyhow!("token_id must be a U256 decimal"))?;
        let (maker_amount, taker_amount) =
            usd_and_share_amounts(planned.shares, planned.limit_price, planned.side)?;

        let expiration = (chrono::Utc::now().timestamp() as u64).saturating_add(expiration_secs);

        let order = Order {
            salt: U256::from(rand::random::<u128>()),
            maker: self.funder,
            signer: self.signer.address(),
            taker: Address::ZERO,
            tokenId: token_id_u256,
            makerAmount: maker_amount,
            takerAmount: taker_amount,
            expiration: U256::from(expiration),
            nonce: U256::ZERO,
            feeRateBps: U256::from(self.fee_rate_bps),
            side: U256::from(planned.side.as_u8()),
            signatureType: U256::from(self.signature_type as u8),
        };

        // Neg-risk markets settle on their own exchange contract. The domain
        // separator hashes the verifying contract, so signing a neg-risk order
        // against the standard exchange produces a signature the venue rejects.
        //
        // shortcut: both domains reuse `domain_name`. Polymarket's V2 migration
        // page only documents the differing verifyingContract; a third-party
        // reference claims the neg-risk domain name is "Polymarket Neg Risk CTF
        // Exchange". Confirm against the SDK before enabling live neg-risk.
        let verifying_contract = Address::from_str(if neg_risk {
            &self.exchange.neg_risk_exchange_address
        } else {
            &self.exchange.ctf_exchange_address
        })?;
        let domain: Eip712Domain = eip712_domain! {
            name: self.exchange.domain_name.clone(),
            version: self.exchange.domain_version.clone(),
            chain_id: self.exchange.chain_id,
            verifying_contract: verifying_contract,
        };
        let digest: B256 = order.eip712_signing_hash(&domain);
        let sig = self
            .signer
            .sign_hash(&digest)
            .await
            .context("signing order digest")?;

        Ok(SignedOrder {
            salt: order.salt.to_string(),
            maker: format!("0x{:x}", order.maker),
            signer: format!("0x{:x}", order.signer),
            taker: format!("0x{:x}", order.taker),
            token_id: order.tokenId.to_string(),
            maker_amount: order.makerAmount.to_string(),
            taker_amount: order.takerAmount.to_string(),
            side: side_str(planned.side).to_string(),
            expiration: order.expiration.to_string(),
            nonce: order.nonce.to_string(),
            fee_rate_bps: order.feeRateBps.to_string(),
            signature_type: self.signature_type as u8,
            signature: format!("0x{}", hex::encode(sig.as_bytes())),
        })
    }

    /// Post a fully signed order to `/order`. Requires L2 API credentials.
    pub async fn post_order(
        &self,
        signed: SignedOrder,
        order_type: OrderType,
    ) -> Result<OrderResponse> {
        let body = OrderPostBody {
            order: signed,
            owner: format!("0x{:x}", self.funder),
            order_type: order_type_str(order_type).to_string(),
        };
        let path = "/order";
        let body_json = serde_json::to_string(&body)?;
        let url = format!("{}{}", self.clob_base, path);

        let headers = self.l2_headers("POST", path, &body_json)?;
        let mut req = self.http.post(&url).body(body_json);
        for (k, v) in headers {
            req = req.header(k, v);
        }
        let resp = req.send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(anyhow!(
                "CLOB rejected order (HTTP {}): {}",
                status.as_u16(),
                text
            ));
        }
        let parsed: OrderResponse = serde_json::from_str(&text).context("parsing CLOB response")?;
        Ok(parsed)
    }

    /// Build the L2 HMAC auth headers Polymarket expects on signed endpoints.
    fn l2_headers(
        &self,
        method: &str,
        path: &str,
        body: &str,
    ) -> Result<Vec<(&'static str, String)>> {
        let api_key = self
            .api_key
            .as_ref()
            .ok_or_else(|| anyhow!("L2 auth missing api_key — run the L1 sign-in flow first"))?;
        let api_secret = self
            .api_secret
            .as_ref()
            .ok_or_else(|| anyhow!("L2 auth missing api_secret"))?;
        let api_passphrase = self
            .api_passphrase
            .as_ref()
            .ok_or_else(|| anyhow!("L2 auth missing api_passphrase"))?;

        let ts = chrono::Utc::now().timestamp().to_string();
        let prehash = format!("{ts}{method}{path}{body}");
        let signature = hmac_sha256_base64url(api_secret, &prehash)?;

        Ok(vec![
            ("POLY_ADDRESS", format!("0x{:x}", self.signer.address())),
            ("POLY_SIGNATURE", signature),
            ("POLY_TIMESTAMP", ts),
            ("POLY_API_KEY", api_key.clone()),
            ("POLY_PASSPHRASE", api_passphrase.clone()),
            ("Content-Type", "application/json".into()),
        ])
    }
}

fn usd_and_share_amounts(shares: f64, price: f64, side: Side) -> Result<(U256, U256)> {
    // USDC has 6 decimals on Polygon; CTF shares are also 6-decimal scaled by Polymarket.
    let shares_units = utils::decimal_units(shares, 6)
        .map_err(|error| anyhow!("invalid share amount: {error}"))?;
    let usd_units = utils::decimal_units(shares * price, 6)
        .map_err(|error| anyhow!("invalid USD amount: {error}"))?;
    Ok(match side {
        // BUY: maker pays USDC, takes shares.
        Side::Buy => (U256::from(usd_units), U256::from(shares_units)),
        // SELL: maker gives shares, takes USDC.
        Side::Sell => (U256::from(shares_units), U256::from(usd_units)),
    })
}

fn side_str(side: Side) -> &'static str {
    match side {
        Side::Buy => "BUY",
        Side::Sell => "SELL",
    }
}

fn order_type_str(t: OrderType) -> &'static str {
    match t {
        OrderType::Fok => "FOK",
        OrderType::Gtd => "GTD",
    }
}

fn parse_private_key(raw: &str) -> Result<[u8; 32]> {
    let trimmed = raw.trim().trim_start_matches("0x");
    let bytes = hex::decode(trimmed).map_err(|_| anyhow!("private key not valid hex"))?;
    if bytes.len() != 32 {
        return Err(anyhow!(
            "private key must be 32 bytes (64 hex chars), got {} bytes",
            bytes.len()
        ));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

/// HMAC-SHA256 over `data`, keyed by the *decoded* API secret and returned as
/// url-safe base64 with padding.
///
/// Both details are load-bearing and match `py-clob-client`'s
/// `build_hmac_signature`: the stored secret is url-safe base64, so its decoded
/// bytes are the key, and the digest keeps its `=` padding. Keying with the raw
/// secret text, or stripping the padding, yields 401 Unauthorized.
fn hmac_sha256_base64url(secret: &str, data: &str) -> Result<String> {
    let key = URL_SAFE
        .decode(secret)
        .or_else(|_| URL_SAFE_NO_PAD.decode(secret))
        .map_err(|e| anyhow!("api_secret is not url-safe base64: {e}"))?;
    let mut mac = Hmac::<Sha256>::new_from_slice(&key).expect("HMAC accepts keys of every length");
    mac.update(data.as_bytes());
    Ok(URL_SAFE.encode(mac.finalize().into_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::Digest;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn test_client(clob_base: String) -> ClobClient {
        let cfg: AppConfig = serde_json::from_str(include_str!("../../config.json")).unwrap();
        ClobClient {
            http: Client::builder()
                .timeout(Duration::from_secs(2))
                .build()
                .unwrap(),
            clob_base,
            signer: PrivateKeySigner::from_bytes(&B256::from([0x11; 32])).unwrap(),
            funder: Address::ZERO,
            exchange: cfg.exchange,
            fee_rate_bps: cfg.trading.fee_rate_bps,
            signature_type: SignatureType::Eoa,
            api_key: Some("test-key".into()),
            // Polymarket issues the L2 secret as base64, and signing keys on
            // its decoded bytes, so the fixture must be decodable too.
            api_secret: Some("dGVzdC1zZWNyZXQ=".into()),
            api_passphrase: Some("test-passphrase".into()),
        }
    }

    fn test_order() -> SignedOrder {
        SignedOrder {
            salt: "1".into(),
            maker: "0x0000000000000000000000000000000000000000".into(),
            signer: "0x0000000000000000000000000000000000000000".into(),
            taker: "0x0000000000000000000000000000000000000000".into(),
            token_id: "123".into(),
            maker_amount: "1000000".into(),
            taker_amount: "2000000".into(),
            side: "BUY".into(),
            expiration: "1".into(),
            nonce: "0".into(),
            fee_rate_bps: "0".into(),
            signature_type: 0,
            signature: "0x00".into(),
        }
    }

    async fn clob_fixture(status: &str, body: &str) -> (String, tokio::task::JoinHandle<Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let status = status.to_string();
        let body = body.to_string();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let read = stream.read(&mut chunk).await.unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
                let Some(header_end) = request.windows(4).position(|part| part == b"\r\n\r\n")
                else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                if request.len() >= header_end + 4 + content_length {
                    break;
                }
            }
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
            request
        });
        (format!("http://{address}"), task)
    }

    /// Await the fixture's captured request with a bound. If `post_order` fails
    /// before sending anything the server waits forever, which would hang the
    /// suite instead of failing it.
    async fn captured_request(server: tokio::task::JoinHandle<Vec<u8>>) -> Vec<u8> {
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .expect("fixture server never received a complete request")
            .unwrap()
    }

    #[test]
    fn sha256_known_vectors() {
        // "abc" → ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad
        let h = Sha256::digest(b"abc");
        assert_eq!(
            hex::encode(h),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn base64url_round_trip_known() {
        // RFC 4648 §10 test vector: f → "Zg" (no padding)
        assert_eq!(URL_SAFE_NO_PAD.encode(b"f"), "Zg");
        assert_eq!(URL_SAFE_NO_PAD.encode(b"fo"), "Zm8");
        assert_eq!(URL_SAFE_NO_PAD.encode(b"foo"), "Zm9v");
    }

    /// Pins the two details that make L2 auth work: the key is the *decoded*
    /// secret, and the signature keeps its base64 padding.
    #[test]
    fn l2_signature_keys_on_the_decoded_secret_and_keeps_padding() {
        let secret = "c2VjcmV0"; // url-safe base64 of "secret"
        let message = "1700000000POST/order{}";

        let mut expected = Hmac::<Sha256>::new_from_slice(b"secret").unwrap();
        expected.update(message.as_bytes());
        let expected = URL_SAFE.encode(expected.finalize().into_bytes());

        let actual = hmac_sha256_base64url(secret, message).unwrap();
        assert_eq!(actual, expected);
        assert!(actual.ends_with('='), "padding must be preserved: {actual}");

        // Regression guard: keying on the raw secret text is the old bug.
        let mut raw = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        raw.update(message.as_bytes());
        assert_ne!(actual, URL_SAFE.encode(raw.finalize().into_bytes()));
    }

    #[test]
    fn unpadded_secret_is_still_accepted() {
        assert!(hmac_sha256_base64url("c2VjcmV0cw", "msg").is_ok());
    }

    #[tokio::test]
    async fn neg_risk_orders_sign_against_the_neg_risk_exchange() {
        let client = test_client("http://127.0.0.1:1".into());
        let planned = PlannedOrder {
            venue: crate::models::VenueId::Polymarket,
            token_id: "123".into(),
            side: Side::Buy,
            shares: 10.0,
            limit_price: 0.5,
            usd_notional: 5.0,
            order_type: OrderType::Fok,
            source_trade_hash: None,
            source_shares: None,
            source_usd_notional: None,
            source_maker: None,
            observed_at: None,
        };

        let standard = client
            .build_signed_order(&planned, 60, false)
            .await
            .unwrap();
        let neg_risk = client.build_signed_order(&planned, 60, true).await.unwrap();

        assert_ne!(
            client.exchange.ctf_exchange_address.to_lowercase(),
            client.exchange.neg_risk_exchange_address.to_lowercase(),
            "fixture must use two distinct exchanges for this to prove anything"
        );
        // Same order, different verifying contract ⇒ different domain
        // separator ⇒ different signature.
        assert_ne!(standard.signature, neg_risk.signature);
    }

    #[test]
    fn hmac_rfc4231_test_1() {
        // RFC 4231 §4.2 — HMAC-SHA-256 of "Hi There" with 20×0x0b key.
        let key = vec![0x0bu8; 20];
        let mut mac = Hmac::<Sha256>::new_from_slice(&key).unwrap();
        mac.update(b"Hi There");
        let mac = mac.finalize().into_bytes();
        assert_eq!(
            hex::encode(mac),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }

    #[tokio::test]
    async fn post_order_sends_l2_headers_and_order_type() {
        let (base, server) =
            clob_fixture("200 OK", r#"{"orderID":"order-1","success":true}"#).await;
        let response = test_client(base)
            .post_order(test_order(), OrderType::Fok)
            .await
            .unwrap();
        let request = String::from_utf8(captured_request(server).await).unwrap();
        let request = request.to_ascii_lowercase();

        assert_eq!(response.order_id.as_deref(), Some("order-1"));
        assert!(request.starts_with("post /order "));
        for header in [
            "poly_address:",
            "poly_signature:",
            "poly_timestamp:",
            "poly_api_key:",
            "poly_passphrase:",
        ] {
            assert!(request.contains(header), "missing {header}");
        }
        assert!(request.contains("\"ordertype\":\"fok\""));
        assert!(request.contains("\"tokenid\":\"123\""));
    }

    #[tokio::test]
    async fn post_order_rejects_non_success_http() {
        let (base, server) = clob_fixture("502 Bad Gateway", "upstream down").await;
        let error = test_client(base)
            .post_order(test_order(), OrderType::Fok)
            .await
            .unwrap_err()
            .to_string();
        let _ = captured_request(server).await;
        assert!(error.contains("HTTP 502"));
    }

    #[tokio::test]
    async fn post_order_preserves_success_false_response() {
        let (base, server) = clob_fixture(
            "200 OK",
            r#"{"success":false,"errorMsg":"not enough balance"}"#,
        )
        .await;
        let response = test_client(base)
            .post_order(test_order(), OrderType::Fok)
            .await
            .unwrap();
        let _ = captured_request(server).await;
        assert!(!response.success);
        assert_eq!(response.error_msg.as_deref(), Some("not enough balance"));
    }
}
