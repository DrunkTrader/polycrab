# PolyCrab audit implementation plan

Source of truth: [`audit/CODEBASE_REVIEW.md`](CODEBASE_REVIEW.md).

## Guardrails

- Preserve the existing stack and simulated-mode behavior.
- Do not read, modify, log, or commit the ignored local `config.yaml`.
- Do not enable live trading while depth enforcement, validated order math, and
  fill-aware state are incomplete.
- Make one connected change at a time, add a focused regression test, and run
  the smallest relevant check before moving on.
- Do not add a generic adapter, database, local API, or other speculative layer.

## Ordered checklist

| ID | Priority | Status | Scope | Dependencies | Acceptance criteria |
|---|---|---|---|---|---|
| R-03 | P0 | Completed | Validate configuration/trade inputs; quantize orders; reject zero/invalid quantities; use actual notional; checked CLOB integer conversion. | Existing `PlannedOrder`/CLOB path; explicit venue precision policy. | Non-finite/non-positive values and zero-share orders fail before recording/signing; supported fractional quantity is retained; planned notional equals quantized quantity × price; checked scaled conversion has regression tests. |
| R-01 | P0 | Completed | Enforce the depth guard at the executor boundary. | R-03 order validation; existing `RiskGuard`. | Missing/unavailable depth blocks live execution; thin books block; paper mode remains usable; specific reason is observable. |
| R-02 | P0 | Completed | Separate accepted/pending orders from fill-confirmed positions; keep live mode disabled until reconciliation is real. | R-01/R-03; existing `PositionStore`, CLOB response model. | HTTP acceptance never opens a position; rejected/ambiguous submissions create no position; simulation remains immediate; live startup fails closed until fill/status reconciliation is implemented. |
| R-06 | P1 | Completed | Validate live configuration at startup and redact credential-bearing debug output. | Validated config boundary from R-03; current YAML/environment loader. | Invalid public settings fail before network activity; missing live credentials fail before ingestion; `Debug` output contains no secret material; simulation needs no credentials. |
| R-04 | P1 | In Progress | Add bounded HTTP/WebSocket operations and complete shutdown cancellation. | R-02 unknown-submission behavior; current Tokio lifecycle. | Stalled network/channel operations terminate within a bound; shutdown does not hang; closed watch channels cancel child tasks; live POST timeout is never blindly retried. |
| R-10 | P1 | Completed | Move unconditional skips before metadata lookup and keep backpressure bounded/observable. | Current executor flow; no unbounded task spawning. | Source sells/already-open tokens skip before Gamma I/O; operator behavior remains clear; channel remains bounded. |
| R-05 | P1 | In Progress (identity complete; backfill blocked) | Add event identity, duplicate suppression, and reconnect recovery/backfill where supported by current provider contract. | R-02 lifecycle; `RawLog` identity; local fixtures. | `logIndex` is parsed; duplicate logs produce one action; malformed block numbers error; reconnect behavior is tested and any provider limitation is explicit. |
| R-07 | P1 | Completed | Replace hand-written HMAC/SHA/base64 with maintained direct primitives. | Existing Cargo lock/cache; compatibility vectors. | Custom primitives are removed; known vectors and fixed prehash/header tests pass offline. |
| R-08 | P1 | In Progress (offline HTTP boundaries added) | Add offline side-effect integration/regression fixtures. | R-01/R-02/R-03 and local HTTP/WebSocket fixture approach. | Tests cover safety boundaries without live credentials or services; `make test` remains deterministic. |
| R-09 | P1 | In Progress (checks/permissions done; pinning blocked) | Harden CI/release checks and permissions. | Stable commands after code changes; reviewed action references. | CI runs lint/build checks; release write permission is scoped; action pinning/dependency warnings are handled without weakening checks. |
| R-11 | P2 | Completed | Bound market-cache retention. | Current cache behavior; no concurrency expansion required. | Stale entries cannot grow without bound; existing cache semantics remain intact. |
| R-12 | P2 | In Progress (one dead branch removed) | Remove duplicate lifecycle/dead current-path branches. | R-02/R-04 lifecycle decisions. | One headless lifecycle path remains; dead behavior is removed only where no caller/contract needs it. |
| R-13 | P2 | Completed | Replace stringly typed execution outcomes and preserve specific skip reasons. | R-02 outcome model; operator/UI callers. | Reducer does not branch on magic result strings; sizing skip reason is retained. |
| R-14 | P2 | Pending | Avoid unnecessary TUI derived-view recomputation. | No behavior change; measurable only if worthwhile. | UI behavior is unchanged and derived views are not recomputed needlessly. |
| R-15 | P2 | Partial (format migration blocked) | Reassess deprecated YAML and unused Tokio features. | Compatibility review; no credentials migration without an explicit contract. | No unnecessary feature flags remain; credentials format is not broken casually; transitive future-incompatibility remains visible. |
| R-16 | P2 | Completed | Harden Docker runtime user and release reproducibility. | Dockerfile/release workflow review; available image tooling. | Runtime image is non-root; builder context is minimal; unverified platform behavior is documented. |
| R-17 | P2 | Completed | Add `validate-config`/version CLI ergonomics. | R-03/R-06 validation APIs. | Operators can validate safely without network activity; output is redacted and mode-aware. |
| R-18 | P3 | In Progress (lock policy explicit) | Surface child-task failures and make lock-poison policy explicit. | R-04 cancellation model. | Child failures are observable; clean shutdown is distinguished from failure; lock policy is consistent. |

## Stop conditions and blockers

- If the external Polymarket order-status/order-book contract cannot be verified
  from repository evidence, implement a fail-closed boundary rather than
  guessing an endpoint or response schema.
- If a live-path change would require private credentials, use local fixtures or
  placeholders only; never inspect or copy the ignored credential file.
- A check is not marked passed unless it was run after the relevant change.
- If time or platform access prevents a recommendation, mark it `Blocked` and
  record the exact reason in `audit/progress.md`.
