# Development

PolyCrab is a Rust 2021 binary/library crate. The toolchain is pinned to Rust
`1.99.0` in [`rust-toolchain.toml`](../rust-toolchain.toml), and the dependency
lockfile is tracked.

The runtime uses Tokio, `reqwest` with rustls, `tokio-tungstenite`, Serde
JSON/YAML, Clap, ratatui, crossterm, and Alloy primitives/signers/Solidity
types for Polygon and EIP-712 order signing.

## Repository layout

```text
.
├── .github/                  # Issue templates and CI/release workflows
├── Cargo.toml                # Package metadata and dependencies
├── Cargo.lock                # Locked dependency graph
├── Dockerfile                # Single-process image
├── Makefile                  # Canonical local commands
├── config.json               # Public simulated configuration
├── config.yaml.example       # Credential template
├── docs/                     # Technical documentation
├── rust-toolchain.toml       # Pinned Rust toolchain
└── src/
    ├── main.rs               # CLI entry point
    ├── lib.rs                # Module root
    ├── config.rs             # Configuration loading
    ├── models.rs             # Shared domain types
    ├── operator.rs           # Dashboard event bus
    ├── bot/                  # Bot dispatch and Copy Trading
    ├── service/              # Market, risk, order, and position services
    └── ui/                   # Terminal dashboard
```

`target/`, `config.yaml`, logs, coverage output, and local environment files
are ignored. Do not add credentials or generated build output to Git.

## Local commands

Fetch dependencies once when network access is available:

```bash
cargo fetch --locked
```

The Makefile provides the repeatable checks:

```bash
make test        # cargo test --locked --offline
make fmt-check   # cargo fmt --all -- --check
make lint        # cargo clippy --all-targets --locked -- -D warnings
make build       # cargo build --release --locked
make docker-build
```

Equivalent direct commands are documented in [Testing](TESTING.md). Run
headless Copy Trading with:

```bash
cargo run --release --locked -- run copy-trading
```

Run the terminal dashboard with `cargo run --release --locked -- tui`.

## Dashboard controls

While a bot is running in the TUI:

| Key | Action |
|---|---|
| `1` / `2` / `3` | Overview, activity, and positions views. |
| `Tab` | Switch overview focus between positions and activity. |
| `↑` / `↓`, `j` / `k` | Navigate positions or scroll activity. |
| `/` | Search positions by contract, outcome, category, tag, slug, or token. |
| `o` | Cycle position sorting. |
| `p` | Pause displayed activity while the engine continues. |
| `s` | Stop the worker while keeping the dashboard open. |
| `r` | Resume after the worker stops. |
| `t` | Toggle the color theme. |
| `q` / `Ctrl-C` | Request shutdown and quit. |

## Change boundaries

Keep changes focused on the current Polymarket Copy Trading flow. There is no
generic venue adapter API, web frontend, local HTTP server, database, or
persistent position format to preserve. If a change affects configuration or
live order behavior, update the relevant document in `docs/` and add or adjust
a focused test when practical.
