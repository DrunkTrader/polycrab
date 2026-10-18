# API and integrations

PolyCrab does not expose a local HTTP API, health endpoint, metrics endpoint,
or browser dashboard. The terminal dashboard is an in-process UI. The worker
uses the following external integrations.

## Polygon WebSocket

The configured `site.polygon_ws_url` is used for JSON-RPC WebSocket messages.
PolyCrab sends `eth_subscribe` for `logs` with filters for:

- the configured Polymarket CTF Exchange and negative-risk exchange addresses;
- the `OrderFilled` topic-0 selector; and
- the first configured source wallet as the indexed maker topic.

The subscription reconnects with backoff when the connection ends. Raw logs are
decoded by [`src/service/parse.rs`](../src/service/parse.rs) into `WhaleTrade`.

## Gamma market metadata

For a token ID, the market cache calls:

```text
GET https://gamma-api.polymarket.com/markets?clob_token_ids=<token-id>
```

The base URL is configurable through `site.gamma_api_base`. The response is
used to resolve the market question, slug, outcome, category, tags, closed
state, and expiry. Results are cached in memory for five minutes.

## CLOB midpoint

The TP/SL monitor calls:

```text
GET https://clob.polymarket.com/midpoint?token_id=<token-id>
```

The base URL is configurable through `site.clob_api_base`. Midpoints are used
for session-local P&L and exit decisions; they are not account fills.

## CLOB order submission

Live execution builds an EIP-712 CTF order, signs it locally, creates L2 HMAC
headers from the configured API credentials, and posts:

```text
POST https://clob.polymarket.com/order
```

The `OrderExecutor` is the safety boundary: it creates a CLOB client only when
`enable_trading` is true and `mock_trading` is false. A successful HTTP response
is recorded as a submission response, not as confirmed exchange execution.

## No local service contract

There is no `TcpListener` in the application runtime, no `/health` route, and
no REST or GraphQL endpoint for monitoring or control. Use the TUI or logs for
local observation, and use `run copy-trading` for headless operation.
