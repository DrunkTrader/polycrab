# Security and risk

PolyCrab can sign and submit real Polymarket orders. Treat live operation as a
financially risky activity, not as a harmless dry run.

## Credentials

- Keep private keys, funder addresses, and L2 API credentials out of Git,
  logs, issue reports, and container layers.
- Store credentials in the ignored `config.yaml`, a secret manager, or the
  supported environment overrides. The checked-in
  [`config.yaml.example`](../config.yaml.example) contains placeholders only.
- Do not use the all-zero example key or address for a real wallet.
- Rotate credentials if they have been exposed.
- The `config.yaml` filename is ignored by [`.gitignore`](../.gitignore);
  verify `git status` before committing.

## Live-trading safeguards

Leave these defaults in place while testing:

```json
{
  "bot": {
    "enable_trading": false,
    "mock_trading": true
  }
}
```

Live execution requires both `enable_trading: true` and
`mock_trading: false`, but the current binary also fails closed before worker
startup because fill reconciliation and production depth enforcement are not
implemented. The `OrderExecutor` and `ClobClient` retain defensive gates so
callers cannot bypass this restriction.

## Known operational risks

- Positions, simulated orders, and P&L are in-memory and session-scoped.
- The process does not read account balances or reconcile exchange fills/status.
- A successful CLOB response is not proof that an order filled.
- Shutdown does not cancel already submitted live orders.
- Source sells are ignored; TP/SL midpoint polling is the configured exit path.
- No production order-book depth provider is connected, so depth is never
  enforced in the live entry path; live startup is blocked rather than firing
  blind. Simulated large trades keep running without a book provider.
- Pending live submissions are swept once they are older than the order
  expiration plus 60 seconds, so an unknown outcome cannot block its token or
  consume exposure caps for the rest of the session. The sweep does not confirm
  whether the order filled — only that the venue has stopped accepting it.
- Accepted live submissions are represented as pending metadata and never
  create a local position on HTTP acceptance. The live path remains disabled
  until a venue status/fill adapter can promote confirmed quantities.
- Review Polymarket terms and applicable laws before using live funds. PolyCrab
  is not financial advice and provides no profit guarantee.
