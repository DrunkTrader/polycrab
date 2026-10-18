# Quick start

This guide runs the shipped PolyCrab configuration in simulated mode. It does
not submit orders, but the worker still needs network access to Polygon and the
Polymarket APIs while it is running.

## Prerequisites

- Git.
- Rustup. The repository pins Rust `1.99.0` in
  [`rust-toolchain.toml`](../rust-toolchain.toml).
- GNU Make for the convenience targets.
- Outbound access to the configured Polygon WebSocket and Polymarket HTTP
  endpoints.
- An interactive terminal for the TUI; headless mode does not require one.

Docker is optional. See [Deployment](DEPLOYMENT.md).

## Fresh clone

```bash
git clone https://github.com/DrunkTrader/polycrab.git
cd polycrab
cargo fetch --locked
make test
```

The committed [`config.json`](../config.json) is already configured for
simulation with `enable_trading: false` and `mock_trading: true`. A private
`config.yaml` is optional in this mode. If you need credentials, copy
[`config.yaml.example`](../config.yaml.example) and fill it in; never commit
the resulting `config.yaml`.

## Run the worker

Run Copy Trading without the dashboard:

```bash
cargo run --release --locked -- run copy-trading
```

Run the terminal dashboard explicitly:

```bash
cargo run --release --locked -- tui
```

Running with no subcommand also opens the dashboard:

```bash
cargo run --release --locked
```

The executable is named `polycrab`. To install the local binary into Cargo's
user bin directory:

```bash
cargo install --path . --locked
polycrab --help
```

## Next steps

- [Configuration](CONFIGURATION.md) — customize source wallets, filters,
  sizing, risk, and credentials.
- [Trading modes](TRADING_MODES.md) — understand simulated versus live
  execution before changing safety flags.
- [Development](DEVELOPMENT.md) — format, lint, build, and inspect the code.
- [Troubleshooting](TROUBLESHOOTING.md) — diagnose common startup issues.
