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
`mock_trading: false`. The `OrderExecutor` is the current safety boundary that
prevents signing and posting while either flag remains restrictive.

## Known operational risks

- Positions, simulated orders, and P&L are in-memory and session-scoped.
- The process does not read account balances or reconcile exchange fills/status.
- A successful CLOB response is not proof that an order filled.
- Shutdown does not cancel already submitted live orders.
- Source sells are ignored; TP/SL midpoint polling is the configured exit path.
- No production order-book depth provider is connected, so the configured
  minimum-depth threshold is not enforced in the live entry path.
- Review Polymarket terms and applicable laws before using live funds. PolyCrab
  is not financial advice and provides no profit guarantee.
