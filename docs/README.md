# PolyCrab documentation

This is the technical documentation index for PolyCrab, a Rust Polymarket
copy-trading toolkit. The current implementation has one bot (`Copy Trading`)
and one venue (`Polymarket`).

## Getting started

- [Quick start](QUICKSTART.md) — install prerequisites, run tests, and start
  simulated Copy Trading or the terminal dashboard.

## Architecture and design

- [Architecture](ARCHITECTURE.md) — runtime components and the event-to-order
  pipeline.
- [Trading modes](TRADING_MODES.md) — simulated/live behavior, order safety,
  and current execution boundaries.

## Configuration and development

- [Configuration](CONFIGURATION.md) — public settings, private credentials,
  CLI paths, environment overrides, filters, sizing, risk, and TP/SL.
- [Development](DEVELOPMENT.md) — pinned toolchain, repository layout, local
  commands, and coding workflow.

## Trading and APIs

- [API and integrations](API.md) — outbound Polygon, Gamma, and CLOB
  interfaces, plus the absence of a local HTTP API.

## Testing and deployment

- [Testing](TESTING.md) — tests, formatting, linting, builds, and benchmark
  limitations.
- [Deployment](DEPLOYMENT.md) — Docker image usage and GitHub release
  packaging.

## Security and operations

- [Security](SECURITY.md) — credential handling and live-trading warnings.
- [Troubleshooting](TROUBLESHOOTING.md) — verified setup and runtime failure
  cases.

## Attribution

PolyCrab is maintained at
<https://github.com/DrunkTrader/polycrab> and released under the [MIT License](../LICENSE).
The codebase evolved from the upstream
[Prediction-Markets-Trading-Bot-Toolkits](https://github.com/HarrierOnChain/Prediction-Markets-Trading-Bot-Toolkits)
project; the upstream copyright and license notice are retained.
