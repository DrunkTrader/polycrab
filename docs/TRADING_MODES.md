# Trading modes

PolyCrab ships in simulated mode. The execution pipeline still observes
Polygon logs, resolves market metadata, applies filters/sizing/risk checks, and
polls midpoints, but it does not sign or submit orders.

## Mode selection

| Mode | Required flags | Behavior |
|---|---|---|
| Simulated | `enable_trading: false` **or** `mock_trading: true` | Planned entries and exits are recorded in the in-memory position store. No private key or CLOB credentials are required. |
| Live | `enable_trading: true` **and** `mock_trading: false` | Orders are signed locally with EIP-712 and submitted to the configured Polymarket CLOB. Private-key, funder, and L2 credentials are required. |

Changing `enable_trading` alone does not enable live execution. Keep both
settings at their shipped safe values until a complete simulated session has
been reviewed.

## Entry and exit behavior

- Only Copy Trading is exposed, and only Polymarket is supported.
- Only the first wallet in `bot.wallets_to_track` is currently used.
- Source buys can create copied entries after metadata, eligibility, sizing,
  exposure, and risk checks pass.
- Source sells are ignored by design. TP/SL midpoint polling is the configured
  exit path.
- Simulated exits are local records. Live exits use the internal
  `OrderType::Fak` path and are serialized to the CLOB as `FOK`.
- Positions and P&L are session-local and are not reconciled with the account
  after restart.
- A successful CLOB HTTP response is not a confirmed exchange fill.
- Stopping PolyCrab does not cancel orders already submitted to the CLOB.

## Risk boundaries

The executor applies market allow/block rules, category/tag exposure caps,
minimum and maximum sizing, and the in-memory large-trade circuit breaker. The
configured `risk.min_depth_usd` value is covered by risk-module tests, but a
production order-book provider is not connected. When a large trade requests a
book check, the executor warns and continues.

See [Configuration](CONFIGURATION.md) for the exact flag and risk fields, and
[Security](SECURITY.md) before using live credentials.
