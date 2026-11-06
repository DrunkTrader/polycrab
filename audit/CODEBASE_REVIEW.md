# PolyCrab codebase review

**Review date:** 2026-10-18  
**Revision reviewed:** `fb1ec5d` (`updated README.md`)  
**Scope:** tracked source, configuration, documentation, workflows, dependency manifest/lockfile, and tests.

## Review status and validation

This review is read-only with respect to application code. The only workspace
artifact created by this task is this Markdown report.

The ignored local `config.yaml` was not opened because it is the private
credential file. The review covers its loader and the committed
`config.yaml.example`; generated `target/` contents were not treated as source.

Checks run against the reviewed revision:

| Check | Result |
|---|---|
| `make test` | Passed: 46 unit tests, 0 failures. |
| `cargo test --locked --offline` | Passed after `cargo fetch --locked` populated the local cache. |
| `make fmt-check` | Passed. |
| `make lint` | Passed with `-D warnings`. |
| `make build` | Passed: locked release build. |
| `python3 -m json.tool config.json` | Passed. |
| `cargo tree --locked --offline` | Passed; all declared direct dependencies are referenced by the code. |
| Future-incompatibility report | Warning from transitive `proc-macro-error2 v2.0.1`; see the dependency section. |
| Docker image build | Not run; Docker daemon availability was not established. |

The passing checks show that the current code is buildable and internally
tested. They do **not** establish that live order submission, fill tracking, or
shutdown behavior is safe against real Polymarket responses.

## Executive summary

PolyCrab is a compact, understandable Rust binary/library crate with a clear
event-to-order pipeline and a useful simulated mode. Configuration, the
centralized `OrderExecutor`, bounded operator activity, reconnecting WebSocket
subscription, and explicit documentation of current limitations are good
choices. This is a reasonable prototype and simulation tool without needing a
large architectural rewrite.

The main boundary is live trading. The current implementation can be enabled
for real orders, but several advertised safeguards are incomplete:

1. `RiskCheck::FetchBook` is logged and then allowed through because no book
   provider is connected (`src/service/order_executor.rs:195-217`).
2. A successful `POST /order` response is treated as an open filled position
   (`src/service/order_executor.rs:274-277`), although the documentation
   correctly says that HTTP acceptance is not a fill.
3. Order sizing can produce zero shares and reports the pre-quantization USD
   amount as the order notional (`src/service/order_executor.rs:219-251`,
   `src/utils.rs:10-16`).
4. Network calls have no explicit timeout, and a failed or ambiguous live
   submission has no idempotency or reconciliation path.

Until those are addressed, the recommended operating posture is **simulation
only**. The highest-value next step is a live-execution safety slice that fails
closed on missing risk data and introduces pending-order/fill state instead of
opening a position on HTTP success.

## Current architecture

### Runtime shape

The application is one Tokio process with two entry modes:

```text
main.rs
  ├─ load public JSON + optional private YAML credentials
  ├─ initialize tracing
  ├─ `run copy-trading`  → headless bot runtime
  └─ `tui` / no command   → ratatui operator UI + bot runtime
```

`src/bot/mod.rs` owns bot dispatch and lifecycle. The current dispatcher has
one `BotKind`, `CopyTrading`, and starts
`copy_trading::run_with_operator_and_positions` in a cancellable Tokio task.
The TUI reads an `OperatorSnapshot` from `OperatorBus`; it does not directly
perform trading actions.

### Trading pipeline

`src/bot/copy_trading.rs` creates one HTTP client, a `RiskGuard`, a five-minute
`MarketCache`, an `OrderExecutor`, a TP/SL monitor, and a Polygon WebSocket
subscription. Relevant logs are passed through this sequence:

1. `src/service/onchain.rs` subscribes to `OrderFilled` logs for the two
   configured exchange addresses and the first configured maker wallet.
2. `src/service/parse.rs` decodes the event into a `WhaleTrade`.
3. `src/service/market_cache.rs` resolves token metadata through Gamma.
4. `src/service/eligibility.rs` applies closed-market and allow/block rules.
5. `src/service/strategy.rs` computes percentage, fixed, or adaptive sizing.
6. `src/service/position_store.rs` applies local category/tag exposure caps.
7. `src/service/risk_guard.rs` applies the large-trade circuit breaker and
   exposes a book-depth phase that is not currently wired to a provider.
8. `src/service/order_executor.rs` builds the planned order and chooses paper
   recording or live CLOB submission.
9. `src/service/clob.rs` signs EIP-712 data and posts L2-authenticated orders
   in live mode.
10. `src/service/position_monitor.rs` polls CLOB midpoints and creates TP/SL
    exits for local positions.

### State and integration boundaries

- Public settings are in `config.json`; private credentials are loaded from an
  ignored YAML file with two environment overrides.
- State is process-local: positions, simulated orders, counters, and P&L are
  held in `PositionStore` and `OperatorBus`.
- There is no database, account-balance reader, order-status/fill
  reconciliation, local HTTP API, health endpoint, or metrics endpoint.
- Only the first `bot.wallets_to_track` value is active.
- Only Polymarket and Copy Trading are exposed.
- Source sells are intentionally ignored; TP/SL is the configured exit path.
- The WebSocket reconnects with exponential backoff, but it does not backfill
  events missed during a disconnect.

### What is already well-designed

- **One execution safety boundary:** bots route through `OrderExecutor`, rather
  than each bot deciding independently whether to sign or submit.
- **Safe shipped defaults:** `enable_trading: false` and `mock_trading: true`
  prevent the committed configuration from submitting orders.
- **Useful separation of pure logic:** parsing, eligibility, sizing, P&L,
  risk decisions, and UI formatting have focused functions and unit tests.
- **Bounded operator history:** activity and recent-trade queues are capped at
  100 entries.
- **No lock is held across an `.await`:** the primary `RwLock`/`Mutex` uses are
  short synchronous state operations, which is appropriate for this scope.
- **Reproducibility basics:** the toolchain is pinned, `Cargo.lock` is tracked,
  and the Makefile provides repeatable test/build/lint/format commands.
- **Operational honesty:** `docs/ARCHITECTURE.md`, `docs/SECURITY.md`, and
  `docs/TRADING_MODES.md` explicitly document in-memory state, missing depth
  enforcement, ignored sells, and the difference between order acceptance and
  fills. The code should be brought into line with those warnings rather than
  the warnings removed.

## Key problems at a glance

| Priority | Problem | Main evidence | Impact |
|---|---|---|---|
| P0 | Configured minimum depth is bypassed in live mode. | `src/service/order_executor.rs:200-208` | A stated liquidity safeguard does not protect real orders. |
| P0 | HTTP order acceptance is recorded as a filled/open position. | `src/service/order_executor.rs:274-277` | TP/SL and exposure can act on positions that never filled or only partially filled. |
| P0 | Zero/invalid order quantities are not rejected after sizing/rounding. | `src/utils.rs:10-16`, `src/service/clob.rs:268-277` | Invalid live requests and incorrect exposure/P&L are possible. |
| P1 | Long-running network operations have no bounded timeout or robust cancellation. | `src/bot/copy_trading.rs:104-128`, `src/service/market_cache.rs:134-146`, `src/service/midprice.rs:24-37`, `src/service/clob.rs:199-230` | A hung upstream can stall ingestion, TP/SL, and shutdown. |
| P1 | Reconnects do not deduplicate or backfill logs. | `src/service/onchain.rs:18-25`, `src/service/onchain.rs:238-258` | Trades can be missed or duplicated across connection failures. |
| P1 | Credential-bearing types derive `Debug`, and live credential preflight is delayed until the first order. | `src/config.rs:11-31`, `src/config.rs:190-197`, `src/service/clob.rs:109-136` | Accidental debug output can expose secrets; bad live setup fails late. |
| P1 | Cryptographic HMAC/SHA-256/base64 are hand-implemented in the live auth path. | `src/service/clob.rs:315-460` | A security-sensitive, high-maintenance implementation is harder to audit. |
| P1 | CI installs Clippy but does not run it; release actions are not SHA-pinned. | `.github/workflows/rust.yml:28-40`, `.github/workflows/release.yml:17-18` | Regressions and supply-chain changes can reach releases unnoticed. |
| P2 | Cache and UI derived views grow/recompute without a meaningful bound. | `src/service/market_cache.rs:55-63`, `src/ui/mod.rs:842-890` | Long sessions can accumulate memory and waste frame time. |
| P2 | The current one-bot abstraction has duplicate lifecycle paths and dead branches. | `src/bot/mod.rs:28-41`, `src/bot/copy_trading.rs:50-61`, `src/service/clob.rs:287-291` | More code must be kept consistent without providing current flexibility. |

## Detailed review

### 1. Architecture and separation of concerns

The architecture is appropriate for a single-venue prototype, but several
responsibilities are coupled more tightly than they need to be:

- `copy_trading.rs` owns runtime lifecycle, WebSocket setup, event handling,
  operator event translation, and trade-pipeline orchestration. It is the
  largest integration seam and will become difficult to change once more bots,
  venues, or replay sources are introduced.
- `OrderExecutor` is a good central boundary, but it currently combines
  policy (eligibility, sizing, risk, exposure), order construction, live-mode
  selection, local position recording, and CLOB dispatch. The fill/reconciliation
  problem is a direct result of local state being updated inside the submission
  path.
- `AppConfig` is deserialized directly into public mutable structs and then
  passed wholesale into services. There is no validated runtime configuration
  boundary. Invalid or contradictory values therefore reach the financial
  path.
- `OperatorEvent` is a useful decoupling mechanism, but result and reason
  values are largely free-form `String`s. This makes UI behavior depend on
  string literals such as `"SKIPPED"` and `"REJECTED"`.
- `BotKind`, `BOTS`, and the dispatcher are abstractions for future bots, but
  there is currently one bot and one venue. The extra layer is not harmful
  today, but the duplicate headless runner is real maintenance cost.

The scalable direction is not a generic plugin framework. Keep one pipeline,
but isolate (a) validated configuration, (b) pure order decision, (c) order
submission/status, and (d) local session projection. That preserves the
current stack and avoids a premature venue-adapter hierarchy.

### 2. Code quality and maintainability

#### Strengths

- Names generally describe domain concepts (`WhaleTrade`, `PlannedOrder`,
  `MarketInfo`, `RiskCheck`, and `OpenPosition`).
- The pure functions in `strategy.rs`, `eligibility.rs`, `position_store.rs`,
  `position_monitor.rs`, and `utils.rs` are small enough to test directly.
- Error context is present at the filesystem, configuration, signing, HTTP,
  and WebSocket boundaries.
- The code uses typed `Side`, `VenueId`, `OrderType`, and risk/eligibility
  enums instead of passing every decision as a string.

#### Issues

- `src/ui/mod.rs` is 1,367 lines and contains state, input handling, layout,
  rendering, formatting, and tests. It is coherent but too broad for safe
  feature work. Split by responsibility only when the next UI change requires
  it; a wholesale rewrite is not justified.
- `src/operator.rs` is 703 lines and is both the event model and the complete
  state reducer. This is workable, but event result strings and repeated
  formatting make it easy for a producer and the UI to drift.
- `src/service/clob.rs` is 495 lines because transport, signing, L2 auth, and
  a cryptographic implementation live together. The cryptographic block is
  not domain logic and should not be maintained inline with order serialization.
- `SizingDecision::skipped` has useful specific reasons, but
  `OrderExecutor` collapses all of them into `SkipReason::BelowSizing` at
  `src/service/order_executor.rs:168-178`. That loses operator-facing
  diagnostics.
- The adaptive-sizing comment says the curve interpolates "linearly in
  log-space", while `adaptive_percent` actually uses
  `exp(-ln(1 + ratio))`, equivalent to a reciprocal decay. Either the comment
  or the formula should be corrected and tested against the intended curve.
- `std::sync` lock poisoning is handled with `unwrap()` in the operator,
  cache, risk, and position stores. The locks are short-lived and not a current
  performance problem, but one panic can turn unrelated future reads into
  process panics. Either recover/clear poisoned state deliberately or document
  the fail-fast policy consistently.
- `parse_log` silently turns malformed hexadecimal block numbers into block
  zero (`src/service/onchain.rs:249-251`). Invalid upstream data should be an
  error, not a plausible-looking value.

### 3. Prioritized recommendations

The following recommendations are ordered by risk and value. **P0 items should
be completed before enabling live trading.**

#### R-01 — P0 — Fail closed when the depth guard cannot run

- **Problem:** `RiskGuard::check_fast` returns `FetchBook` for large trades,
  but `OrderExecutor::execute` logs a warning and continues at
  `src/service/order_executor.rs:200-208`. `check_with_book` is only exercised
  by unit tests; no production order-book provider calls it.
- **Why it matters:** `risk.min_depth_usd` looks like a configured safety
  control but is not enforced in live mode. A thin or empty book can therefore
  receive an order that the operator intended to block.
- **Suggested solution:** Either implement a small CLOB order-book client and
  call `check_with_book` before signing, or return a risk-blocked outcome when
  `FetchBook` is requested and no provider is configured. Do not silently
  downgrade a configured protection to a warning. Record the exact reason in
  the operator event.
- **Files/modules likely affected:**
  `src/service/order_executor.rs`, `src/service/risk_guard.rs`,
  `src/service/clob.rs` or a new small `src/service/order_book.rs`,
  `src/config.rs`, `docs/TRADING_MODES.md`, `docs/SECURITY.md`.
- **Estimated complexity:** Medium.

#### R-02 — P0 — Separate order submission from fill-confirmed position state

- **Problem:** On any successful CLOB HTTP response, `OrderExecutor` calls
  `record_open` immediately (`src/service/order_executor.rs:274-277`). The TP/SL
  monitor then treats that local position as filled and closes it after a
  successful exit response (`src/service/position_monitor.rs:164-221`).
- **Why it matters:** A successful HTTP response means the order was accepted
  by the endpoint, not that it filled. Partial fills, later cancellations,
  network timeouts after acceptance, and rejected matching can all make the
  local position wrong. The bot can then submit an exit for inventory it does
  not own, or count exposure/P&L that never existed.
- **Suggested solution:** Introduce a small order lifecycle: `Planned` →
  `Submitted` → `PartiallyFilled`/`Filled`/`Rejected`/`Cancelled`. Keep
  submitted orders in a pending store keyed by client/order ID; poll or consume
  the venue's status/fill endpoint; promote only actual filled quantity into
  `PositionStore`; handle partial fills; reconcile pending orders on startup;
  and explicitly cancel or report live orders during shutdown. Until status
  support exists, the safest fallback is to refuse live mode rather than
  fabricate positions.
- **Files/modules likely affected:**
  `src/service/order_executor.rs`, `src/service/position_monitor.rs`,
  `src/service/position_store.rs`, `src/service/clob.rs`,
  `src/models.rs`, `src/operator.rs`, `docs/SECURITY.md`.
- **Estimated complexity:** High.

#### R-03 — P0 — Validate financial inputs and reject zero/invalid orders

- **Problem:** `usd_to_shares` floors to a whole number and can return zero
  (`src/utils.rs:10-16`). `OrderExecutor` still constructs and can submit a
  zero-share `PlannedOrder` (`src/service/order_executor.rs:219-251`). It keeps
  `sizing.copy_usd` as `planned.usd_notional` even when quantization reduces
  the actual quantity. `usd_and_share_amounts` then casts `f64` values directly
  to `u128` (`src/service/clob.rs:268-277`) without finite/range checks.
- **Why it matters:** Zero or negative/NaN/infinite values can create invalid
  live requests. The mismatch between intended and signed notional corrupts
  exposure caps, dashboard metrics, and P&L. Whole-share flooring may also
  discard valid fractional token quantities if the venue supports them.
- **Suggested solution:** Validate all public numeric configuration at load
  time; require finite, positive prices and quantities at the order boundary;
  use the venue's documented quantity/price precision instead of an implicit
  whole-share floor; recompute `usd_notional` from the quantized quantity and
  limit price; reject `shares <= 0` and below-minimum venue size before signing;
  and use checked fixed-point conversion for scaled integer amounts. Add
  explicit live-mode tests for tiny, fractional, zero, negative, and
  out-of-range values.
- **Files/modules likely affected:**
  `src/config.rs`, `src/service/strategy.rs`, `src/utils.rs`,
  `src/service/order_executor.rs`, `src/service/clob.rs`,
  `src/service/position_store.rs`.
- **Estimated complexity:** Medium.

#### R-04 — P1 — Bound every network operation and make shutdown cancellation complete

- **Problem:** The HTTP clients created in `copy_trading.rs`, `clob.rs`,
  `market_cache.rs`, and `midprice.rs` do not set a request timeout. WebSocket
  connection/read operations also have no explicit deadline. A full execution
  channel is allowed to block `tx.send(log).await` without selecting on
  shutdown (`src/service/onchain.rs:203-205`).
- **Why it matters:** A stalled Gamma, CLOB, midpoint, or RPC connection can
  stop the serial trade consumer, delay every subsequent trade and TP/SL quote,
  and prevent `q`/Ctrl-C from completing. A timeout after a live POST is also
  an ambiguous outcome that must not be blindly retried.
- **Suggested solution:** Define one conservative timeout policy and apply it
  to HTTP request, connect, and response phases; select channel sends and
  cleanup waits against shutdown; use a cancellation token or an explicit
  shutdown sender for all child tasks; and classify POST timeout as
  `UnknownSubmission` requiring reconciliation, not an automatic retry. Check
  `watch::Receiver::changed()` errors as cancellation, not only as a changed
  `true` value.
- **Files/modules likely affected:**
  `src/bot/mod.rs`, `src/bot/copy_trading.rs`, `src/service/onchain.rs`,
  `src/service/market_cache.rs`, `src/service/midprice.rs`,
  `src/service/clob.rs`, `src/service/position_monitor.rs`.
- **Estimated complexity:** Medium.

#### R-05 — P1 — Make ingestion idempotent and recoverable across reconnects

- **Problem:** `RawLog` contains address, topics, data, transaction hash, and
  block number but no log index (`src/service/onchain.rs:18-25`). The consumer
  has no processed-event set, and reconnect logic starts a new subscription
  without replaying a block range.
- **Why it matters:** A reconnect can lose events permanently. A provider or
  retry can also deliver the same log more than once. `AlreadyOpen` prevents
  some duplicate entries, but it does not protect after a position closes or
  when an event is processed more than once in another lifecycle.
- **Suggested solution:** Parse and retain `logIndex` (and, if useful,
  `removed`) from the JSON-RPC log. Deduplicate on `(transaction_hash,
  log_index)` with a bounded TTL/LRU. Track the last finalized block and
  backfill `eth_getLogs` on reconnect before resuming live subscription, or
  document an explicit at-most-once loss policy. Add a replay fixture test.
- **Files/modules likely affected:**
  `src/service/onchain.rs`, `src/service/parse.rs`,
  `src/bot/copy_trading.rs`, `src/models.rs`, `docs/API.md`.
- **Estimated complexity:** High.

#### R-06 — P1 — Redact credentials and perform live preflight before the first trade

- **Problem:** `AppConfig` and `Credentials` derive `Debug` while containing
  private key, API secret, and passphrase fields (`src/config.rs:11-31` and
  `src/config.rs:190-197`). `ClobClient::new` validates the private key and
  funder, but missing L2 credentials are only reported in `l2_headers` during
  the first POST (`src/service/clob.rs:233-264`).
- **Why it matters:** A future `debug!(?cfg)` or error context can expose
  secrets. Late credential failure makes a live process appear healthy while
  it is already ingesting events and signing work in memory.
- **Suggested solution:** Implement redacted `Debug` for credential-bearing
  types or remove `Debug` from them; keep secrets out of `Display` and event
  payloads; add a `validate_live_credentials()` preflight called during
  startup; validate endpoint URLs and exchange addresses at the same boundary;
  and add environment overrides for all three L2 fields only if that is an
  intentional supported deployment path.
- **Files/modules likely affected:**
  `src/config.rs`, `src/main.rs`, `src/service/clob.rs`,
  `config.yaml.example`, `docs/CONFIGURATION.md`, `docs/SECURITY.md`.
- **Estimated complexity:** Low to Medium.

#### R-07 — P1 — Replace hand-rolled cryptography in the L2 auth path

- **Problem:** `src/service/clob.rs:315-460` contains custom SHA-256, HMAC, and
  unpadded base64url implementations. The code has a few known-vector tests,
  but it is still bespoke cryptographic code in the live authentication path.
- **Why it matters:** Known-vector tests do not substitute for the audit,
  maintenance, constant-time behavior, and edge-case coverage of established
  primitives. A subtle implementation or future compiler issue can prevent
  authentication or create an unsafe signing boundary.
- **Suggested solution:** Use maintained `sha2`, `hmac`, and `base64` crates
  directly (or a single existing audited abstraction) and keep only the
  Polymarket-specific prehash/header composition in this module. Preserve the
  RFC 4231 tests and add a full header/prehash compatibility test with fixed
  inputs. Do not depend on an indirect transitive crate without declaring it.
- **Files/modules likely affected:** `src/service/clob.rs`, `Cargo.toml`,
  `Cargo.lock`.
- **Estimated complexity:** Medium.

#### R-08 — P1 — Add tests around the actual financial and lifecycle boundaries

- **Problem:** The suite has 46 inline unit tests and one local Gamma HTTP
  fixture, but no integration tests for live order serialization/status,
  midpoint behavior, WebSocket lifecycle, configuration validation, or
  shutdown. The existing tests cover pure decisions more strongly than the
  side-effecting paths.
- **Why it matters:** The highest-risk behavior is exactly where current tests
  are thinnest: the code that signs, posts, records positions, retries, and
  stops. Passing the current suite cannot catch the HTTP-accepted-as-filled
  defect or the missing depth provider.
- **Suggested solution:** Add a small `tests/` or focused module-level suite
  using local HTTP/WebSocket fixtures. At minimum cover: zero/fractional order
  quantities; actual-notional calculation; invalid config and credentials;
  depth-provider absence; CLOB request body and L2 headers; non-2xx and
  `success:false` responses; partial/unknown submission state; midpoint
  timeout/invalid values; duplicate log replay; malformed block numbers;
  stop/resume and child-task shutdown; and operator counters for failures.
  Keep live-network tests opt-in and offline tests deterministic.
- **Files/modules likely affected:** new `tests/`,
  `src/service/order_executor.rs`, `src/service/clob.rs`,
  `src/service/onchain.rs`, `src/service/position_monitor.rs`,
  `src/config.rs`, `Makefile`.
- **Estimated complexity:** Medium.

#### R-09 — P1 — Make CI enforce the checks that developers are told to run

- **Problem:** `.github/workflows/rust.yml` installs the Clippy component but
  runs only formatting, tests, and a debug build (`:32-40`); it never invokes
  `make lint`. The release workflow grants `contents: write` to the whole
  workflow and uses mutable action tags such as `@v4`, `@v2`, and `@v1.99.0`.
- **Why it matters:** Clippy regressions can merge despite a documented lint
  command. Mutable third-party actions increase release supply-chain risk, and
  broad write permission violates least privilege for build jobs.
- **Suggested solution:** Add `make lint` to the normal CI job, run the
  canonical release build (or a representative release build) in CI, pin
  third-party actions to reviewed commit SHAs, scope `contents: write` to the
  publish job, and add a dependency/advisory review workflow. Keep the
  toolchain and lockfile pinning already present.
- **Files/modules likely affected:** `.github/workflows/rust.yml`,
  `.github/workflows/release.yml`, optionally `.github/dependabot.yml` or a
  repository security workflow.
- **Estimated complexity:** Low to Medium.

#### R-10 — P1 — Make the trade pipeline explicit about backpressure and skipped work

- **Problem:** `copy_trading.rs` processes one log at a time, and
  `OrderExecutor::execute` resolves Gamma metadata before checking that a
  source sell is intentionally ignored or that a position is already open
  (`src/service/order_executor.rs:116-149`). A slow metadata or CLOB request
  therefore blocks the single consumer and causes the bounded WebSocket
  channel to fill.
- **Why it matters:** This is both unnecessary I/O and a throughput ceiling.
  A source wallet that emits several fills close together can delay later
  trades and TP/SL work. The current bounded channel provides backpressure,
  which is safer than unbounded spawning, but there is no explicit policy for
  what happens when processing cannot keep up.
- **Suggested solution:** Move unconditional skips (source sells and already
  open tokens) ahead of network metadata lookup, while retaining a token-only
  operator record. Add latency/queue-depth counters. If concurrency is later
  needed, introduce a small bounded worker pool only after position
  reservation and event idempotency are in place; do not simply spawn an
  unbounded task per log.
- **Files/modules likely affected:**
  `src/service/order_executor.rs`, `src/bot/copy_trading.rs`,
  `src/service/position_store.rs`, `src/operator.rs`.
- **Estimated complexity:** Medium.

#### R-11 — P2 — Bound and simplify market-cache retention

- **Problem:** `MarketCache` keeps every fetched slug and token index forever;
  TTL makes entries stale but does not remove them (`src/service/market_cache.rs:55-63`,
  `:98-115`). There is also no in-flight lookup deduplication.
- **Why it matters:** A long-running source wallet can touch many markets,
  making memory grow with session history. Concurrent callers in a future
  worker pool could issue duplicate Gamma requests for the same token.
- **Suggested solution:** Use a bounded TTL cache with eviction (or explicit
  stale-entry cleanup) and add a small per-token in-flight guard if concurrent
  execution is introduced. Keep the current read-through interface and avoid
  adding a general cache framework.
- **Files/modules likely affected:** `src/service/market_cache.rs`,
  `src/bot/copy_trading.rs`.
- **Estimated complexity:** Low to Medium.

#### R-12 — P2 — Remove duplicate lifecycle code and dead current-path branches

- **Problem:** `src/bot/copy_trading.rs:50-61` duplicates the headless
  Ctrl-C/task orchestration already in `src/bot/mod.rs:28-41`, but no repository
  caller uses `copy_trading::run`. `OrderType::Gtd` and the `Side::Sell` branch
  of `order_type_for` are unreachable in the current executor because source
  sells return at `:141-145`. The depth-check data types are similarly only
  connected to tests until a provider is added.
- **Why it matters:** Duplicate lifecycle paths can diverge in shutdown and
  error handling. Dead branches make the current contract harder to see and
  can falsely suggest that sell execution or depth checks are supported.
- **Suggested solution:** Keep one public lifecycle entry point. Either remove
  the unused `copy_trading::run` or make it a thin wrapper around
  `BotRuntime`. Delete or clearly feature-gate `Gtd`/sell-only behavior until
  sell execution exists; retain `BookSnapshot` only with an explicit provider
  boundary. Do not build generic bot/venue traits until a second implementation
  is required.
- **Files/modules likely affected:** `src/bot/mod.rs`,
  `src/bot/copy_trading.rs`, `src/models.rs`, `src/service/clob.rs`,
  `src/service/order_executor.rs`, `src/service/risk_guard.rs`.
- **Estimated complexity:** Low.

#### R-13 — P2 — Replace stringly typed execution results with domain outcomes

- **Problem:** `TradeProcessed.result` is a `String`, and the operator reducer
  branches on literal values such as `"SKIPPED"` and `"REJECTED"`
  (`src/operator.rs:437-447`). `SkipReason::BelowSizing` also discards the
  specific strategy reason.
- **Why it matters:** A typo or new result value silently changes dashboard
  color/counter behavior. Producers and UI must coordinate through undocumented
  string values.
- **Suggested solution:** Add a small `ExecutionStatus` enum and preserve the
  nested sizing/risk reason. Convert to display text at the UI/log boundary.
  Keep serialized wire compatibility irrelevant because there is no local API.
- **Files/modules likely affected:** `src/operator.rs`,
  `src/service/order_executor.rs`, `src/service/strategy.rs`, `src/ui/mod.rs`.
- **Estimated complexity:** Low to Medium.

#### R-14 — P2 — Cache UI-derived views per snapshot instead of recomputing every frame

- **Problem:** `refresh_runtime` computes `visible_positions(app)` and the
  renderer computes it again (`src/ui/mod.rs:247-253`, `:754-840`). The helper
  clones positions, lowercases several fields, filters, and sorts on every
  120 ms frame (`:842-890`). Activity items are also cloned for rendering.
- **Why it matters:** The current cap is small enough for normal use, but the
  cost scales with open positions and becomes visible in a long session or on
  a small terminal. It also makes UI complexity independent of actual engine
  work.
- **Suggested solution:** Cache the filtered/sorted position list and paused
  activity projection in `UiState`, invalidating on snapshot/search/sort
  changes. Preserve the current rendering behavior; do not optimize until a
  larger-position workload demonstrates a need.
- **Files/modules likely affected:** `src/ui/mod.rs`.
- **Estimated complexity:** Low to Medium.

#### R-15 — P2 — Reassess the credentials format and trim unused Tokio features

- **Problem:** `serde_yaml` resolves as `0.9.34+deprecated` and is used only to
  parse a small credentials structure (`src/config.rs:221-232`). The normal
  Tokio dependency enables `io-util`, `net`, and `fs`, while production code
  uses `io-util`/`net` primarily in a local test fixture and no code uses
  `tokio::fs`.
- **Why it matters:** An unmaintained parser in the secret-loading path is a
  maintenance and supply-chain concern. Extra feature flags enlarge builds
  without improving the runtime. This is not an urgent correctness issue, and
  the existing YAML file is a documented user-facing contract.
- **Suggested solution:** Do not make a breaking format change casually. For
  the next configuration version, prefer a maintained format already used by
  the project or environment-only secret injection, with a migration note.
  Independently remove `fs` and isolate test-only Tokio features where Cargo's
  feature unification permits it. Add a dependency review command to CI and
  monitor the `proc-macro-error2` future-incompatibility warning rather than
  patching transitive code blindly.
- **Files/modules likely affected:** `Cargo.toml`, `Cargo.lock`,
  `src/config.rs`, `config.yaml.example`, `docs/CONFIGURATION.md`,
  `.github/workflows/rust.yml`.
- **Estimated complexity:** Medium.

#### R-16 — P2 — Harden the runtime image and release reproducibility

- **Problem:** The final Docker image runs the binary as root and the base
  images/actions are selected by mutable tags (`Dockerfile:1`, `:9`,
  `.github/workflows/release.yml:44-48`). The Docker build also copies
  `README.md` and `LICENSE` into the builder even though `cargo build` does not
  need them.
- **Why it matters:** A compromised process or dependency has more container
  privileges than necessary. Mutable base tags make a release less
  reproducible. The copied documentation is small but indicates the build
  context is not minimal.
- **Suggested solution:** Add a non-root `polycrab` user in the runtime stage,
  run with `USER polycrab`, and pin base images by reviewed digest while
  retaining the toolchain pin. Keep credentials mounted outside image layers.
  Remove builder-only copies that are not consumed by the build; package docs
  only in the release archive as the workflow already does.
- **Files/modules likely affected:** `Dockerfile`,
  `.github/workflows/release.yml`, `docs/DEPLOYMENT.md`.
- **Estimated complexity:** Low to Medium.

#### R-17 — P2 — Add a validated configuration command and safer CLI ergonomics

- **Problem:** The CLI has `tui` and `run copy-trading`, but no
  `validate-config`, no `--version`, and no startup report that distinguishes
  invalid public values from missing live credentials. Relative paths are
  intentionally resolved from the current working directory
  (`src/main.rs:16-24`), which is easy to misuse in service managers.
- **Why it matters:** Operators discover configuration errors only when a
  relevant event arrives or when the TUI starts. A headless deployment needs a
  cheap readiness check that does not connect to Polygon or submit anything.
- **Suggested solution:** Add a `validate-config` subcommand that loads both
  files, validates numeric/address/URL invariants, redacts credentials, and
  reports whether the mode is simulated or live. Add `#[command(version)]` and
  keep explicit `--config`/`--credentials` paths for service managers rather
  than silently changing path semantics.
- **Files/modules likely affected:** `src/main.rs`, `src/config.rs`,
  `docs/QUICKSTART.md`, `docs/CONFIGURATION.md`, `docs/TROUBLESHOOTING.md`.
- **Estimated complexity:** Medium.

#### R-18 — P3 — Make child-task failures visible and reduce lock-poison surprises

- **Problem:** The main bot ignores the join results of the subscription and
  monitor (`src/bot/copy_trading.rs:179-181`). A subscription task panic or
  unexpected channel closure is not surfaced as a runtime error. Several state
  accessors use `RwLock::unwrap()`/`Mutex::unwrap()`.
- **Why it matters:** A dead monitor can leave positions unmanaged while the
  process appears alive. A poisoned lock can turn one isolated panic into
  further panics without a useful operator explanation.
- **Suggested solution:** Propagate or publish `JoinError`, distinguish clean
  shutdown from child failure, and ensure the receiver owns a cancellation
  path when the main loop exits unexpectedly. Decide explicitly whether lock
  poisoning should fail fast or recover with an error event; apply that policy
  consistently.
- **Files/modules likely affected:** `src/bot/copy_trading.rs`,
  `src/bot/mod.rs`, `src/service/onchain.rs`, `src/service/position_monitor.rs`,
  `src/operator.rs`, `src/service/market_cache.rs`,
  `src/service/position_store.rs`, `src/service/risk_guard.rs`.
- **Estimated complexity:** Low to Medium.

## Performance review

There is no evidence that CPU time is currently the bottleneck. The dominant
cost is intentional network I/O: one Gamma lookup on a cache miss, one
midpoint request per open position per polling cycle, and optional CLOB
submission. The following are justified improvements rather than speculative
micro-optimizations:

- **Serial pipeline:** one slow metadata or CLOB request blocks the entire
  `mpsc` consumer. This is a reliability and throughput issue; R-04 and R-10
  address it without jumping straight to unbounded concurrency.
- **Ignored sells:** metadata is currently resolved before the unconditional
  source-sell skip. Moving that skip earlier removes an avoidable external API
  call for every source sell.
- **Midpoint polling:** positions are polled sequentially. With many open
  positions, one slow request delays all others. After adding timeouts, bounded
  concurrency or a small request budget can improve freshness without flooding
  the CLOB.
- **Cache retention:** stale `MarketInfo` entries and token indexes are never
  evicted. R-11 addresses the only clear unbounded data structure in the
  runtime.
- **TUI work:** filtering, lowercasing, cloning, and sorting occur on every
  frame. This is acceptable at the current scale but should be cached if the
  UI becomes a primary operator surface (R-14).
- **Exposure checks:** category/tag checks scan all open positions and perform
  case-insensitive map searches. This is simple and likely adequate for a
  small session. Normalize keys or maintain incremental counters only after a
  measured position-count workload justifies the extra state.

The biggest performance win is therefore bounded I/O and early skip logic,
not changing `HashMap` to another collection or rewriting the UI renderer.

## Testing review

### Existing coverage

The current suite is entirely inline with the modules and contains 46 tests.
It covers:

- event signature/filter construction and basic ABI decoding;
- eligibility allow/block behavior;
- percentage, fixed, adaptive, minimum, and maximum sizing;
- category/tag exposure totals and P&L direction;
- circuit-breaker and book-depth pure decisions;
- local Gamma metadata parsing;
- paper execution through a local TCP fixture;
- HMAC/SHA/base64 known vectors;
- TP/SL threshold decisions; and
- small-terminal rendering, search, and sorting.

That is a good unit-test foundation for a prototype. Tests are deterministic
and the documented test command can run offline after dependencies have been
fetched.

### Important gaps

The gaps are concentrated at side-effect and safety boundaries:

1. **Configuration:** no tests for missing/partial credentials, environment
   overrides, custom paths, defaulted sections, invalid numeric ranges,
   malformed URLs/addresses, or redacted debug output.
2. **Order math:** no tests prove nonzero minimum quantity, fractional venue
   precision, actual-vs-intended notional, invalid float rejection, or signed
   integer conversion at rounding boundaries.
3. **CLOB integration:** no local HTTP fixture asserts signed order JSON,
   headers, order type, non-2xx handling, `success: false`, or status/fill
   transitions.
4. **Risk integration:** no test proves that an unavailable depth provider
   blocks a live order. The existing `check_with_book` test does not exercise
   the production executor path.
5. **WebSocket lifecycle:** no test covers reconnect, backoff cancellation,
   wrong subscription IDs, malformed block numbers, channel backpressure,
   duplicate events, or clean shutdown while connecting/receiving.
6. **Position lifecycle:** no test covers partial/unknown live submission,
   timeout/retry behavior, duplicate TP/SL ticks, or restart reconciliation.
7. **Bot/UI lifecycle:** no test covers `BotRuntime::stop`/`resume`, child task
   failure propagation, a dropped sender, or a TUI quit while a network call is
   pending.

The test plan in R-08 should be implemented before any live-mode rollout. No
live-network test should be required for `make test`.

## Developer experience and workflow

### Good foundations

- `rust-toolchain.toml` pins Rust `1.99.0` and includes rustfmt/Clippy.
- `Cargo.lock` is tracked for a binary crate.
- `Makefile` exposes `test`, `build`, `fmt-check`, `lint`, and Docker build
  entry points.
- `docs/QUICKSTART.md`, `docs/DEVELOPMENT.md`, `docs/CONFIGURATION.md`, and
  `docs/TROUBLESHOOTING.md` give concrete commands and explain the two-file
  configuration model.
- The committed configuration is safe simulation mode, and the credentials
  file is ignored by Git and the Docker build context.

### Workflow issues

- The canonical CI workflow does not run the canonical `make lint` target even
  though the toolchain installs Clippy (R-09).
- `make test` is intentionally offline, but a fresh machine must first run
  `cargo fetch --locked`; the quick start explains this. A single `make setup`
  target is optional, not necessary, and should only be added if onboarding
  feedback shows the two-step flow is confusing.
- There is no aggregate `make check` target. This is a small convenience, not
  a correctness issue; CI should still call individual commands so failures
  are clear.
- There is no `validate-config`/readiness command, so service managers must
  discover errors by starting the worker (R-17).
- The CLI has no explicit version flag and its default paths are relative to
  the current directory. Both are documented, but `--version` and explicit
  service-manager paths would improve supportability.
- CI path filters intentionally omit documentation-only changes. That is
  reasonable for build cost, but documentation changes to security/config
  semantics should be reviewed alongside code changes.
- No coverage, dependency advisory, or release smoke test is configured.

## Security and reliability review

### Positive observations

- No private credential file is tracked; `config.yaml` is ignored.
- The example file contains placeholders/all-zero values with warnings and is
  not used as a secret source.
- The default mode does not construct a `ClobClient`, sign, or post orders.
- `reqwest` uses rustls with default features disabled, avoiding a native OpenSSL
  dependency.
- The project does not expose a server endpoint, so there is no unauthenticated
  local control API to secure.
- The code does not log the private key or L2 secret directly in its current
  paths.

### Risks

- R-01, R-02, and R-03 are live-funds risks, not style concerns.
- `Credentials` being `Debug`-printable is an accidental secret-exposure path,
  even though the current `main.rs` logging is careful.
- There is no file-permission check or warning for a credentials file readable
  by other local users. This is lower risk for a single-user workstation but
  relevant on shared hosts.
- The final Docker image runs as root (R-16).
- Release actions and action toolchains are mutable rather than SHA-pinned
  (R-09). The release workflow has `contents: write` at workflow scope even
  though only publishing needs it.
- There is no order cancellation/reconciliation on shutdown or restart. The
  limitation is documented, but documentation does not prevent orphaned live
  orders.
- Arbitrary endpoint values are accepted from local config. This is not an
  exposed-server SSRF issue because the operator controls the file, but URL
  scheme/host validation would prevent accidental HTTP or typo endpoints.

No committed credential was found in the tracked file list. I did not inspect
the ignored local credentials file.

## Dependency review

### Direct dependencies that are justified

The direct dependency set is broadly reasonable for the current feature set:

| Dependency group | Use in repository |
|---|---|
| Tokio, futures-util | Async runtime, channels, signals, WebSocket stream/sink. |
| reqwest, tokio-tungstenite | Gamma/CLOB HTTP and Polygon WebSocket. |
| Serde JSON/YAML | Public config, private credential config, external payloads. |
| anyhow, tracing, tracing-subscriber | Application errors and operator/runtime logs. |
| Clap | CLI parsing and bot enum. |
| Chrono | UTC timestamps and serialized config/domain values. |
| Alloy primitives/sol types/signers | ABI event decoding, EIP-712 order construction, signing. |
| hex, rand | Hex encoding and unique order salt. |
| ratatui, crossterm | Terminal dashboard. |

`cargo tree --depth 1` did not reveal an obviously unused direct package. A
large transitive graph is expected from Alloy, rustls/reqwest, and the TUI; a
dependency rewrite is not justified merely to reduce the lockfile size.

### Dependency actions worth taking

- **P1 / R-07:** use direct audited cryptographic primitives instead of the
  hand-rolled implementation. This may add explicit direct dependencies but
  removes more security-sensitive code.
- **P2 / R-15:** `serde_yaml` resolves to `0.9.34+deprecated`. Its use is
  limited to credentials, so a future credentials-format migration is a
  reasonable way to remove the maintenance risk; preserve YAML compatibility
  until a deliberate migration is designed.
- **P2 / R-15:** remove unused `tokio` feature flags (`fs` is not used; `net`
  and `io-util` are primarily test-fixture needs) if the resulting feature
  graph remains correct for release builds.
- **P2:** the build emits a future-incompatibility warning from transitive
  `proc-macro-error2 v2.0.1`, pulled through the Alloy macro stack. Track the
  upstream fix and keep the warning visible in CI; do not add a local patch
  without a demonstrated need.
- **P3:** broad semver requirements such as `tokio = "1"` are normal when the
  lockfile is committed. A scheduled, reviewed `cargo update --locked` is
  preferable to manually pinning every transitive version.

## Feature roadmap

These are features that naturally follow from the current architecture. They
are ordered by safety/usefulness, not by feature count.

### Essential

| Priority | Feature | Why it fits | Likely modules | Complexity |
|---|---|---|---|---|
| P0 | Fill-aware live execution | The existing live path already signs/posts; it needs pending/fill/rejection state before it can safely maintain positions. | `clob.rs`, `order_executor.rs`, `position_store.rs`, `position_monitor.rs`, `models.rs` | High |
| P0 | Enforced order-book depth guard | `RiskGuard` already defines the policy and tests; adding the CLOB book adapter closes the existing safety gap. | `risk_guard.rs`, `order_executor.rs`, new `order_book.rs` | Medium |
| P1 | Config validation/readiness command | Uses the existing Clap/config loader and makes deployment errors deterministic. | `main.rs`, `config.rs`, docs | Medium |
| P1 | Event deduplication and reconnect backfill | Directly improves the existing Polygon subscription without changing the trading strategy. | `onchain.rs`, `parse.rs`, `copy_trading.rs` | High |
| P1 | Bounded HTTP/WebSocket timeouts and graceful shutdown | Makes the current headless/TUI workflows operable under upstream failure. | bot lifecycle, network clients, monitor | Medium |
| P1 | Offline integration fixtures for CLOB and WebSocket | Protects the real side-effect boundaries while keeping `make test` reproducible. | new `tests/`, service modules | Medium |

### Optional / nice-to-have

| Priority | Feature | Why it fits | Likely modules | Complexity |
|---|---|---|---|---|
| P2 | Persistent session state and restart reconciliation | Natural follow-on once order/fill state exists; required for reliable long-running live use. | new persistence module, position/order stores | High |
| P2 | Headless health/metrics output | The current `OperatorSnapshot` already has counters and connection state; expose them to supervisors without adding trading control. | `operator.rs`, CLI or a metrics sink | Medium |
| P2 | Multi-wallet fan-in | `wallets_to_track` already accepts a list, but the runtime currently selects `.first()`. Implement only with per-wallet event identity and rate limits. | `copy_trading.rs`, `onchain.rs`, models/config | High |
| P2 | Alerts for risk trips, rejected orders, and stale connections | Uses existing `OperatorEvent` categories and is more useful than a generic feature. | operator/event sink, CLI config | Medium |
| P3 | Source-sell mirroring | This is a strategy change, not a simple missing branch; add only after fill/reconciliation and position accounting are correct. | executor, order lifecycle, docs | High |
| P3 | Replay/backtest mode | The pure parser/strategy/risk modules are suitable for deterministic event fixtures and regression analysis. | new replay command, fixtures | Medium |
| P3 | Local HTTP control/API | Not needed for the current TUI/headless use case; add only if an external supervisor genuinely needs control/metrics. | new service boundary, auth | High |

## Refactoring roadmap

This order minimizes churn and keeps the current simulated behavior intact:

1. **Freeze live mode as simulation-only in operational guidance** until R-01
   through R-03 are implemented. Add regression tests that demonstrate the
   desired fail-closed behavior before changing execution code.
2. **Create validated runtime configuration** and redacted credentials. This
   gives every subsequent service the same safe invariants and catches bad
   setup before network activity.
3. **Fix order math and lifecycle state:** reject zero/invalid quantities,
   compute actual notional after quantization, then represent submitted/pending
   orders separately from filled positions.
4. **Wire the depth provider or block large trades**, then add local CLOB
   fixtures for order-book, order-post, response, and status behavior.
5. **Add timeouts and cancellation propagation** across HTTP, WebSocket,
   monitor, and bot child tasks. Surface join failures instead of ignoring them.
6. **Add event identity, deduplication, and reconnect recovery** before any
   bounded concurrency. This prevents a performance refactor from creating
   duplicate financial actions.
7. **Harden CI/release and the image:** run Clippy in CI, pin actions, scope
   permissions, review dependency warnings, and run the process as non-root.
8. **Apply low-risk cleanup:** remove duplicate lifecycle code, preserve
   specific skip reasons, replace result strings with enums, and trim unused
   features.
9. **Only then optimize scale:** bound/evict the market cache, cache TUI views,
   and add bounded concurrency if measured workloads require it.

This roadmap avoids introducing a generic plugin or database before the live
execution contract is correct.

## Quick wins

These changes provide immediate value with limited churn:

- Change `RiskCheck::FetchBook` to a blocking outcome when no provider exists.
- Reject non-finite, non-positive, and zero-share orders before signing or
  recording a position.
- Add request/connect timeouts and select channel sends against shutdown.
- Move source-sell/already-open checks before Gamma lookup.
- Redact `Credentials`/`AppConfig` debug output and preflight all live fields.
- Add `make lint` to `.github/workflows/rust.yml`.
- Pin release actions and narrow `contents: write` to the publish job.
- Make malformed `blockNumber` parsing return an error instead of zero.
- Remove the unused `tokio::fs` feature and the duplicate headless runner if no
  external library consumer needs it.

## Long-term improvements

The following require more work but change the reliability ceiling of the
project:

- Venue-confirmed order/fill reconciliation with partial-fill accounting and
  restart recovery.
- Durable pending-order and position state, plus shutdown cancellation policy.
- Polygon reconnect backfill and event deduplication.
- A bounded market cache and bounded concurrent midpoint/order-book polling.
- Multi-wallet fan-in with per-wallet identity/rate limits.
- A supervisor-facing health/metrics channel built from the existing operator
  state, not a second trading control plane.
- A replay/backtest command based on recorded `RawLog` fixtures.

## Recommended next step

Implement **R-02 as a focused live-execution safety slice**, starting with a
pending-order state and a local CLOB status fixture. In the same change, make
the current behavior fail closed: do not record a position on HTTP acceptance,
and do not enable TP/SL or exposure accounting until a fill quantity is known.
Pair it with the zero/invalid-order regression tests from R-03.

This is the single most useful next task because it corrects the boundary where
real funds can diverge from local state. It also creates the state model needed
for depth enforcement, retry/idempotency, shutdown cancellation, persistence,
alerts, and any future source-sell or multi-wallet feature. It is more valuable
than a UI split, dependency refresh, or broad architectural rewrite.

## Final synthesis

### Current Architecture

PolyCrab is a single Tokio process with a Clap CLI, an optional ratatui TUI,
one Copy Trading bot, and a Polymarket-only service layer. Polygon WebSocket
logs are parsed into `WhaleTrade` values, Gamma metadata is cached, eligibility/
sizing/exposure/risk policies produce a `PlannedOrder`, and the centralized
executor either records a simulated order or signs/posts to the CLOB. An
in-memory position store feeds a midpoint-based TP/SL monitor and an operator
event bus. There is no durable state, local API, account reader, or fill
reconciliation.

### Key Problems

1. Live depth protection is explicitly bypassed when no book provider exists.
2. HTTP order acceptance is treated as a filled position despite the docs
   warning that it is not fill confirmation.
3. Order quantity/notional math lacks a final validation boundary and can
   produce zero or inaccurately reported orders.
4. Network calls, ambiguous submissions, reconnect gaps, and shutdown are not
   bounded/reconciled robustly.
5. Credentials can be debug-formatted; live preflight is incomplete.
6. The most security-sensitive auth primitive is hand-rolled.
7. CI and release hardening lag behind the quality of the local Makefile/docs.
8. Cache/UI retention and current one-bot abstractions have avoidable complexity,
   but they are secondary to live-trading correctness.

### Refactoring Roadmap

Follow R-01 through R-09 first: fail closed, validate order math, model fills,
bound I/O, recover/deduplicate events, protect credentials, replace custom
crypto, add side-effect integration tests, and harden CI/release. Then apply
R-10 through R-18 selectively based on measured workload and future feature
needs. Do not introduce generic adapters, a database, or a local API before the
execution lifecycle warrants them.

### Feature Roadmap

The essential features are fill-aware live execution, an enforced depth guard,
configuration readiness validation, reconnect recovery/deduplication, bounded
timeouts/shutdown, and offline CLOB/WebSocket fixtures. Persistence,
supervisor metrics, multi-wallet fan-in, alerts, replay, and source-sell
mirroring are valuable follow-ons; a local control API is optional rather than
an immediate need.

### Quick Wins

Fail closed on missing depth data, reject zero/invalid orders, add network
timeouts, move unconditional skips before metadata lookup, redact credential
debug output, run Clippy in CI, pin release actions, and surface malformed
block numbers. These are focused changes with immediate safety or operations
value.

### Long-Term Improvements

The long-term ceiling is a durable, fill-reconciled order lifecycle with
restart recovery and event replay. Once that exists, bounded concurrency,
multi-wallet support, persistence, alerts, and supervisor metrics can be added
without guessing about local state.

### Recommended Next Step

Build the pending-order/fill-reconciliation slice described in R-02, with
fail-closed tests and the R-03 quantity checks. It addresses the highest-risk
user-visible failure and supplies the contract that the rest of the roadmap
needs.
