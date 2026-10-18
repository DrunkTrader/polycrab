# Configuration

PolyCrab uses two configuration files:

- [`config.json`](../config.json) — committed public/runtime settings.
- `config.yaml` — ignored private credentials, optional while simulated mode is
  active. Start from [`config.yaml.example`](../config.yaml.example).

The public file is required. The credentials file is loaded only when it
exists, so the shipped simulated configuration can run without private keys.

## CLI paths

Global options must appear before the subcommand:

| Option | Default | Purpose |
|---|---|---|
| `--config <PATH>` | `config.json` | Public JSON configuration. |
| `--credentials <PATH>` | `config.yaml` | Private YAML credentials. |

Example:

```bash
polycrab \
  --config settings/config.json \
  --credentials secrets/config.yaml \
  run copy-trading
```

## Public JSON settings

The checked-in file currently contains these important values:

| Section | Current behavior and sample values |
|---|---|
| `bot` | One source wallet; `enable_trading: false`; `mock_trading: true`. Only the first wallet is consumed. |
| `site` | Gamma: `https://gamma-api.polymarket.com`; CLOB: `https://clob.polymarket.com`; Polygon WebSocket: `wss://polygon.drpc.org`. |
| `strategy` | `PERCENTAGE`, `copy_size: 20.0`, multiplier `1.0`, order range `$0`–`$500`, minimum source size `10` shares. `FIXED` and `ADAPTIVE` modes are also implemented. |
| `trading` | Price buffer `0.005`, fee rate `0` bps, order expiration `60` seconds. |
| `risk` | Large trade threshold `1500` shares, `2` same-token large trades in `30` seconds, cooldown `120` seconds, configured minimum depth `$200`. |
| `exchange` | Polymarket CTF and negative-risk exchange addresses, Polygon chain ID `137`, and the CTF EIP-712 domain. |
| `filters` | Categories allowlisted: Politics, Sports, Crypto; category blocklist: Memes; tag blocklist: `satire`; category caps: Politics `$500`, Sports `$300`, Crypto `$400`. |
| `tp_sl` | Enabled; default take-profit `50%`; default stop-loss `30%`; category overrides for Politics and Sports; midpoint polling every `15` seconds. |

When omitted from JSON, `enable_trading` defaults to `false`,
`mock_trading` defaults to `true`, and TP/SL defaults to enabled with `+50%`,
`-30%`, and a 15-second polling interval. Other top-level sections are required
by the configuration parser.

## Credentials YAML

The template contains:

```yaml
bot:
  private_key: "<64 hex characters>"
  funder_address: "0x<40 hex characters>"
  api_key: "<L2 API key>"
  api_secret: "<L2 API secret>"
  api_passphrase: "<L2 API passphrase>"
```

Live order submission requires a valid private key, funder address, and all
three L2 CLOB credentials. PolyCrab does not provision L2 credentials
automatically. The private-key parser requires 32 bytes of hexadecimal data and
accepts an optional `0x` prefix.

## Environment overrides

These variables override values read from YAML:

| Variable | Overrides |
|---|---|
| `PM_PRIVATE_KEY` | `bot.private_key` |
| `PM_FUNDER_ADDRESS` | `bot.funder_address` |

The `PM_` names are retained for compatibility. The current implementation has
no environment overrides for the L2 credential fields.

## Filters, sizing, and safety semantics

- Blocklists always win over allowlists.
- If any slug, category, or tag allowlist is populated, a market must match at
  least one allow rule. With all allowlists empty, the bot allows everything
  not blocked.
- Category and tag caps apply to simultaneously open local exposure.
- `PERCENTAGE` treats `copy_size` as a percentage of source USD notional;
  `FIXED` treats it as USD per copy; `ADAPTIVE` derives a percentage from the
  adaptive settings.
- `enable_trading: true` is not enough for live execution. Both
  `enable_trading: true` and `mock_trading: false` are required.

See [Trading modes](TRADING_MODES.md) and [Security](SECURITY.md) before
changing the shipped safety defaults.
