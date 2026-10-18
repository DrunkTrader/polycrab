# Troubleshooting

## `config.json` cannot be found

Run PolyCrab from the repository or release directory, or pass an explicit
public configuration path before the subcommand:

```bash
polycrab --config /path/to/config.json run copy-trading
```

The public JSON file is always required. The private credentials file is
optional in simulated mode.

## Live mode reports missing credentials

Confirm that the credentials YAML or environment overrides provide a valid
private key, funder address, API key, API secret, and API passphrase. PolyCrab
does not provision L2 credentials automatically. Also confirm:

```text
enable_trading = true
mock_trading   = false
```

Do not switch these flags until the credentials and live-wallet setup have been
reviewed.

## The worker connects but no trades are copied

Check the following in order:

1. The first entry in `bot.wallets_to_track` is the intended source wallet.
2. The source event is a supported `OrderFilled` maker event.
3. Source sells are not expected; they are ignored by design.
4. The market is not closed or blocked by slug, category, or tag rules.
5. The source trade meets `min_whale_shares_to_copy` and the sizing limits.
6. Category and tag exposure caps are not already full.
7. The risk circuit breaker has not blocked the trade.

In simulated mode, an accepted entry is stored only in the current session and
does not appear in an exchange account.

## Polygon or Polymarket requests fail

The worker needs outbound access to the configured `wss://` and `https://`
endpoints. Check the URLs in [`config.json`](../config.json), DNS/firewall
access, and upstream service status. The local test suite can run offline after
dependencies are cached:

```bash
make test
```

## The TUI is unusable in a non-interactive shell

Use headless mode:

```bash
polycrab run copy-trading
```

The TUI uses raw terminal mode and an alternate screen. Run it from a real
interactive terminal and exit with `q` or `Ctrl-C`.
