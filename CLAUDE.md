# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

PolyCrab: a single-process async Rust (Tokio) copy-trading bot for Polymarket with a ratatui TUI. Toolchain is pinned to Rust 1.99.0 (`rust-toolchain.toml`); `Cargo.lock` is tracked and all commands use `--locked`.

## Commands

The Makefile is canonical (CI runs `fmt-check`, `test`, `lint`, `build` in that order):

```bash
make test        # cargo test --locked --offline (fixtures only, no network/credentials)
make fmt-check   # cargo fmt --all -- --check
make lint        # cargo clippy --all-targets --locked -- -D warnings
make build       # cargo build --release --locked
make bench       # cargo bench --locked --offline (hot-path timings, no deps)

cargo test --locked --offline <test_name>                  # single test
cargo run --locked -- validate-config                      # check config.json, no network
cargo run --release --locked -- run copy-trading           # headless
cargo run --release --locked -- tui                        # dashboard
```

CLI flags `--config` and `--credentials` select config files. `config.json` is the public simulated config; `config.yaml` (git-ignored, template: `config.yaml.example`) holds credentials.

## Architecture

Pipeline (see `docs/ARCHITECTURE.md` for the diagram): Polygon WebSocket `OrderFilled` logs (`service/onchain.rs`) → ABI decode to `WhaleTrade` (`service/parse.rs`) → Gamma market metadata via 5-min TTL cache (`service/market_cache.rs`) → eligibility filters → strategy sizing (percentage/fixed/adaptive) → category/tag exposure caps (`service/position_store.rs`) → circuit breaker (`service/risk_guard.rs`) → `service/order_executor.rs`. The executor either records a simulated order (default) or signs EIP-712 and POSTs via `service/clob.rs` (live). `service/position_monitor.rs` polls CLOB midpoints for TP/SL exits. `bot/copy_trading.rs` wires it all together.

`operator.rs` is an event bus: the pipeline publishes events and `ui/mod.rs` renders snapshots of them. Headless and TUI modes run the same bot (`bot::run*` entry points).

Constraints that aren't obvious from the code:
- Only the first `bot.wallets_to_track` entry is active; Polymarket and Copy Trading are the only venue/bot. There is no generic venue adapter, database, or HTTP server.
- All state (positions, orders, P&L) is in-memory per session; nothing persists.
- Live mode is fail-closed: an accepted CLOB HTTP response is not a fill, there is no fill reconciliation, and the binary refuses live startup until that exists. Large live trades are also blocked before signing because no order-book depth provider is connected.
- Source-wallet sells are ignored; TP/SL is the only exit path.
- The WebSocket is kept open by 30s pings against a 90s idle timeout (`onchain.rs`), because a filtered single-wallet subscription is silent most of the time. There is still no reconnect backfill, so a genuine disconnect loses events (dedup is by tx/log id within a bounded window).
- The monitor releases positions it can no longer act on — market expired, or five consecutive midpoint failures — so dead positions stop consuming exposure caps. Live positions are escalated, never auto-closed.

## Conventions

If a change affects configuration or live order behavior, update the relevant file in `docs/` and add a focused test. Never commit `config.yaml`, credentials, or build output.
