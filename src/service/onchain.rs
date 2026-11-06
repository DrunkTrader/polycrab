//! Polygon WebSocket subscription.
//!
//! Subscribes to `eth_subscribe`/`logs` on the configured Polygon WS endpoint
//! and forwards each matching log as a [`RawLog`] over a tokio channel. Filter
//! parameters are pushed server-side so the bot is woken only when the watched
//! whale interacts with the configured CTF exchange contracts.

use crate::operator::{ConnectionStatus, OperatorBus, OperatorEvent};
use anyhow::{anyhow, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio::sync::watch;
use tokio::time::{interval_at, timeout, Duration, Instant, MissedTickBehavior};
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, error, info, warn};

const NETWORK_TIMEOUT: Duration = Duration::from_secs(15);

/// How long the socket may stay silent before we treat it as dead.
///
/// A filtered subscription on a single wallet is idle most of the time, so this
/// must be far longer than the connect timeout. We keep the link proven with
/// our own pings rather than by waiting for whale activity.
const IDLE_TIMEOUT: Duration = Duration::from_secs(90);

/// Ping cadence. Each pong resets the idle timer, so a healthy but quiet socket
/// is never dropped, while a genuinely dead one is caught within IDLE_TIMEOUT.
const PING_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct RawLog {
    pub address: String,
    pub topics: Vec<String>,
    pub data: String,
    pub tx_hash: String,
    pub block_number: u64,
    pub log_index: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LogFilter {
    pub address: Vec<String>,
    pub topics: Vec<Option<Vec<String>>>,
}

pub fn spawn_subscription_with(
    ws_url: String,
    filter: LogFilter,
    tx: mpsc::Sender<RawLog>,
    bus: OperatorBus,
    mut shutdown: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut backoff_secs = 1u64;
        loop {
            if *shutdown.borrow() {
                break;
            }
            match run_once(
                &ws_url,
                &filter,
                &tx,
                &bus,
                shutdown.clone(),
                IDLE_TIMEOUT,
                PING_INTERVAL,
            )
            .await
            {
                Ok(()) => {
                    if *shutdown.borrow() {
                        break;
                    }
                    // Server closed cleanly — reconnect after short pause.
                    bus.publish(OperatorEvent::Connection {
                        status: ConnectionStatus::Reconnecting,
                        detail: Some("WebSocket closed; reconnecting".into()),
                    });
                    warn!("polygon WS subscription ended; reconnecting");
                    if wait_or_shutdown(&mut shutdown, 2).await {
                        break;
                    }
                    backoff_secs = 1;
                }
                Err(e) => {
                    bus.publish(OperatorEvent::Connection {
                        status: ConnectionStatus::Error,
                        detail: Some(e.to_string()),
                    });
                    bus.publish(OperatorEvent::RuntimeError {
                        context: "polygon websocket".into(),
                        message: e.to_string(),
                    });
                    error!(error = ?e, backoff_secs, "polygon WS error; reconnecting");
                    if wait_or_shutdown(&mut shutdown, backoff_secs).await {
                        break;
                    }
                    backoff_secs = (backoff_secs * 2).min(30);
                }
            }
        }
        bus.publish(OperatorEvent::Connection {
            status: ConnectionStatus::Stopped,
            detail: Some("subscription stopped".into()),
        });
    })
}

#[allow(clippy::too_many_arguments)]
async fn run_once(
    ws_url: &str,
    filter: &LogFilter,
    tx: &mpsc::Sender<RawLog>,
    bus: &OperatorBus,
    mut shutdown: watch::Receiver<bool>,
    idle_timeout: Duration,
    ping_interval: Duration,
) -> Result<()> {
    bus.publish(OperatorEvent::Connection {
        status: ConnectionStatus::Connecting,
        detail: Some(format!("connecting to {ws_url}")),
    });
    let (mut ws, _resp) = tokio::select! {
        result = timeout(NETWORK_TIMEOUT, tokio_tungstenite::connect_async(ws_url)) => {
            result.context("connecting polygon ws timed out")??
        }
        _ = shutdown.changed() => return Ok(()),
    };
    bus.publish(OperatorEvent::Connection {
        status: ConnectionStatus::Connected,
        detail: Some(format!("connected to {ws_url}")),
    });
    info!(url = %ws_url, "polygon WS connected");

    let sub_req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_subscribe",
        "params": ["logs", filter]
    });
    tokio::select! {
        result = timeout(NETWORK_TIMEOUT, ws.send(Message::Text(sub_req.to_string()))) => {
            result.context("sending polygon subscription timed out")??
        }
        _ = shutdown.changed() => return Ok(()),
    }
    info!(
        ?filter,
        "polygon eth_subscribe request sent; awaiting acknowledgement"
    );

    let mut subscription_id = None;
    let mut notifications = 0u64;
    let mut keepalive = interval_at(Instant::now() + ping_interval, ping_interval);
    keepalive.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        let Some(msg) = (tokio::select! {
            _ = keepalive.tick() => {
                timeout(NETWORK_TIMEOUT, ws.send(Message::Ping(Vec::new())))
                    .await
                    .context("sending polygon ping timed out")??;
                debug!("polygon WS keepalive ping sent");
                continue;
            },
            msg = timeout(idle_timeout, ws.next()) => {
                msg.context("polygon websocket went silent (no log, pong or control frame)")?
            },
            _ = shutdown.changed() => return Ok(()),
        }) else {
            break;
        };
        let msg = msg?;
        let text = match msg {
            Message::Text(t) => t,
            Message::Binary(b) => String::from_utf8_lossy(&b).into_owned(),
            Message::Ping(p) => {
                timeout(NETWORK_TIMEOUT, ws.send(Message::Pong(p)))
                    .await
                    .context("sending polygon pong timed out")??;
                continue;
            }
            Message::Close(_) => return Ok(()),
            _ => continue,
        };

        let value: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                debug!(?e, "non-JSON frame from WS");
                continue;
            }
        };

        if let Some(id) = parse_subscription_ack(&value)? {
            subscription_id = Some(id.clone());
            bus.publish(OperatorEvent::Connection {
                status: ConnectionStatus::Subscribed,
                detail: Some(format!("subscription {id}")),
            });
            info!(subscription = %id, "polygon eth_subscribe accepted; waiting for log notifications");
            continue;
        }

        // Other JSON-RPC responses are control traffic, not log notifications.
        if value.get("method").and_then(|v| v.as_str()) != Some("eth_subscription") {
            debug!(message = %text, "polygon WS control message ignored");
            continue;
        }

        let notification_subscription = value
            .get("params")
            .and_then(|p| p.get("subscription"))
            .and_then(Value::as_str);
        if let (Some(expected), Some(actual)) =
            (subscription_id.as_deref(), notification_subscription)
        {
            if expected != actual {
                warn!(
                    expected,
                    actual, "polygon notification belongs to another subscription"
                );
                continue;
            }
        }

        let params = value
            .get("params")
            .and_then(|p| p.get("result"))
            .ok_or_else(|| anyhow!("missing params.result"))?;

        let log = parse_log(params)?;
        notifications += 1;
        bus.publish(OperatorEvent::LogReceived {
            block_number: log.block_number,
        });
        debug!(
            notifications,
            address = %log.address,
            topics = log.topics.len(),
            tx = %log.tx_hash,
            block = log.block_number,
            "polygon log notification received"
        );
        tokio::select! {
            result = tx.send(log) => {
                if result.is_err() {
                    // Consumer dropped — exit cleanly.
                    return Ok(());
                }
            }
            _ = shutdown.changed() => return Ok(()),
        }
        debug!(
            notifications,
            "polygon log notification forwarded to trade pipeline"
        );
    }

    info!(notifications, "polygon WS stream ended");
    Ok(())
}

async fn wait_or_shutdown(shutdown: &mut watch::Receiver<bool>, secs: u64) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(std::time::Duration::from_secs(secs)) => false,
        _ = shutdown.changed() => true,
    }
}

fn parse_subscription_ack(value: &Value) -> Result<Option<String>> {
    if value.get("id").and_then(Value::as_u64) != Some(1) {
        return Ok(None);
    }
    if let Some(error) = value.get("error") {
        return Err(anyhow!("eth_subscribe rejected: {error}"));
    }
    let id = value
        .get("result")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("eth_subscribe response missing result"))?;
    Ok(Some(id.to_string()))
}

fn parse_log(v: &Value) -> Result<RawLog> {
    #[derive(Deserialize)]
    struct LogShape {
        address: String,
        topics: Vec<String>,
        data: String,
        #[serde(rename = "transactionHash")]
        tx_hash: String,
        #[serde(rename = "blockNumber")]
        block_number: String,
        #[serde(default, rename = "logIndex")]
        log_index: Option<String>,
    }
    let shape: LogShape = serde_json::from_value(v.clone())?;
    let block_number = parse_hex_u64(&shape.block_number, "blockNumber")?;
    let log_index = shape
        .log_index
        .as_deref()
        .map(|value| parse_hex_u64(value, "logIndex"))
        .transpose()?;
    Ok(RawLog {
        address: shape.address.to_lowercase(),
        topics: shape.topics.into_iter().map(|t| t.to_lowercase()).collect(),
        data: shape.data,
        tx_hash: shape.tx_hash,
        block_number,
        log_index,
    })
}

fn parse_hex_u64(value: &str, field: &str) -> Result<u64> {
    u64::from_str_radix(value.trim_start_matches("0x"), 16)
        .with_context(|| format!("{field} is not valid hexadecimal"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A quiet subscription must survive far longer than the idle timeout,
    /// because our pings draw pongs that reset it. Before keepalive existed the
    /// read timeout alone ended the connection, losing every log in the gap.
    #[tokio::test]
    async fn quiet_socket_survives_past_the_idle_timeout_via_keepalive() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            // Wait for eth_subscribe, then acknowledge it.
            let _ = ws.next().await.unwrap().unwrap();
            ws.send(Message::Text(
                json!({"jsonrpc":"2.0","id":1,"result":"0xsub"}).to_string(),
            ))
            .await
            .unwrap();

            // Stay silent well past the client's idle timeout, answering only
            // pings (tungstenite pongs automatically while we read).
            let quiet = tokio::time::sleep(Duration::from_millis(900));
            tokio::pin!(quiet);
            loop {
                tokio::select! {
                    _ = &mut quiet => break,
                    msg = ws.next() => if msg.is_none() { return },
                }
            }

            // Only now does a real log arrive.
            ws.send(Message::Text(
                json!({
                    "jsonrpc":"2.0",
                    "method":"eth_subscription",
                    "params":{"subscription":"0xsub","result":{
                        "address":"0xABC",
                        "topics":["0x01"],
                        "data":"0x00",
                        "transactionHash":"0xlate",
                        "blockNumber":"0x10",
                        "logIndex":"0x02"
                    }}
                })
                .to_string(),
            ))
            .await
            .unwrap();
            tokio::time::sleep(Duration::from_millis(200)).await;
        });

        let (tx, mut rx) = mpsc::channel(8);
        let (_stop, shutdown) = watch::channel(false);
        let filter = LogFilter {
            address: vec!["0xabc".into()],
            topics: vec![None],
        };
        let worker = tokio::spawn(async move {
            run_once(
                &format!("ws://{address}"),
                &filter,
                &tx,
                &OperatorBus::new(),
                shutdown,
                Duration::from_millis(300),
                Duration::from_millis(80),
            )
            .await
        });

        let log = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("connection was dropped before the late log arrived")
            .expect("sender closed without forwarding the log");
        assert_eq!(log.tx_hash, "0xlate");
        assert_eq!(log.block_number, 16);
        worker.abort();
    }

    #[test]
    fn accepts_subscription_ack() {
        let value = json!({"jsonrpc":"2.0", "id":1, "result":"0xsub"});
        assert_eq!(
            parse_subscription_ack(&value).unwrap(),
            Some("0xsub".into())
        );
    }

    #[test]
    fn surfaces_subscription_error() {
        let value = json!({
            "jsonrpc":"2.0",
            "id":1,
            "error":{"code":-32602,"message":"invalid filter"}
        });
        let error = parse_subscription_ack(&value).unwrap_err().to_string();
        assert!(error.contains("eth_subscribe rejected"));
    }

    #[test]
    fn parses_forwarded_log_notification() {
        let value = json!({
            "address":"0xABC",
            "topics":["0x01"],
            "data":"0x00",
            "transactionHash":"0xtx",
            "blockNumber":"0x10",
            "logIndex":"0x02"
        });
        let log = parse_log(&value).unwrap();
        assert_eq!(log.address, "0xabc");
        assert_eq!(log.topics, vec!["0x01"]);
        assert_eq!(log.block_number, 16);
        assert_eq!(log.log_index, Some(2));
    }

    #[test]
    fn rejects_malformed_block_number() {
        let value = json!({
            "address":"0xABC",
            "topics":["0x01"],
            "data":"0x00",
            "transactionHash":"0xtx",
            "blockNumber":"not-hex"
        });
        assert!(parse_log(&value).is_err());
    }

    #[test]
    fn rejects_malformed_log_index() {
        let value = json!({
            "address":"0xABC",
            "topics":["0x01"],
            "data":"0x00",
            "transactionHash":"0xtx",
            "blockNumber":"0x10",
            "logIndex":"not-hex"
        });
        assert!(parse_log(&value).is_err());
    }

    #[test]
    fn allows_logs_without_log_index_for_fallback_identity() {
        let value = json!({
            "address":"0xABC",
            "topics":["0x01"],
            "data":"0x00",
            "transactionHash":"0xtx",
            "blockNumber":"0x10"
        });
        assert_eq!(parse_log(&value).unwrap().log_index, None);
    }
}
