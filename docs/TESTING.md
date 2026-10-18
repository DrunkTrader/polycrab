# Testing and validation

Tests use local fixtures and do not require trading credentials or live
Polygon/Polymarket services after dependencies have been fetched.

## Canonical commands

From the repository root:

```bash
# Fetch the locked dependency graph once when network access is available
cargo fetch --locked

# Offline unit and documentation tests
make test

# Formatting and lint checks
make fmt-check
make lint

# Locked release build
make build

# Validate the committed public configuration (Python 3 is optional)
python3 -m json.tool config.json >/dev/null
```

The direct test command is:

```bash
cargo test --locked --offline
```

The suite covers event filtering and parsing, market metadata, eligibility,
sizing, exposure, risk checks, signing helpers, position/P&L behavior, and
small-terminal UI rendering. Tests do not prove live exchange execution.

## CI

`.github/workflows/rust.yml` pins Rust `1.99.0` and runs locked dependency
fetching, formatting, offline tests, and a locked build. Changes to source,
configuration, toolchain, Makefile, Dockerfile, or the workflow trigger CI.

## Benchmarks

There is no benchmark target or benchmark suite in this repository. The TUI's
latency value is runtime telemetry for observed trades, not a performance
guarantee.
