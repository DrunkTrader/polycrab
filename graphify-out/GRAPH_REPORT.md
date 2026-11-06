# Graph Report - polycrab  (2026-11-06)

## Corpus Check
- Corpus is ~33,189 words - fits in a single context window. You may not need a graph.

## Summary
- 746 nodes · 1606 edges · 32 communities (26 shown, 6 thin omitted)
- Extraction: 95% EXTRACTED · 5% INFERRED · 0% AMBIGUOUS · INFERRED: 74 edges (avg confidence: 0.84)
- Token cost: 0 input · 0 output

## Community Hubs (Navigation)
- Docs & Audit Roadmap
- Shared Imports & Errors
- Operator Event Bus
- CLOB Signing & Auth
- Position Monitor & TP/SL
- Release Workflow
- Config & Logging Setup
- Order Math Utils
- TUI Terminal Rendering
- AppConfig Validation
- Risk Guard
- Copy Strategy Sizing
- Dashboard Drawing
- CI Rust Workflow
- Eligibility Filters
- Market Cache
- Execution Outcomes
- TUI Input & Focus
- Executor Test Fixtures
- UI Formatting Helpers
- Config Limits & Caps
- Cache TTL & Gamma
- Executor Position Access
- Position Sorting
- TUI Screenshot Overview
- TUI Screenshot Activity
- UI Tests
- UI Views
- Code of Conduct
- Crate Root

## God Nodes (most connected - your core abstractions)
1. `AppConfig` - 49 edges
2. `PositionStore` - 34 edges
3. `OperatorEvent` - 30 edges
4. `UiState` - 30 edges
5. `Side` - 25 edges
6. `MarketInfo` - 23 edges
7. `PlannedOrder` - 22 edges
8. `OpenPosition` - 22 edges
9. `WhaleTrade` - 21 edges
10. `OperatorBus` - 20 edges

## Surprising Connections (you probably didn't know these)
- `Copy-Trading Pipeline (Polygon WS to executor)` --semantically_similar_to--> `Copy-trading pipeline diagram and runtime flow`  [INFERRED] [semantically similar]
  CLAUDE.md → docs/ARCHITECTURE.md
- `Development Doc` --semantically_similar_to--> `Makefile Commands (test, fmt-check, lint, build)`  [INFERRED] [semantically similar]
  docs/DEVELOPMENT.md → CLAUDE.md
- `Live Mode Fail-Closed Constraint` --semantically_similar_to--> `Live-trading safeguards (fail closed)`  [INFERRED] [semantically similar]
  CLAUDE.md → docs/SECURITY.md
- `Live mode (rejected at startup)` --semantically_similar_to--> `Live Mode Fail-Closed Constraint`  [INFERRED] [semantically similar]
  docs/TRADING_MODES.md → CLAUDE.md
- `In-Memory Session State Constraint` --semantically_similar_to--> `State and boundaries`  [INFERRED] [semantically similar]
  CLAUDE.md → docs/ARCHITECTURE.md

## Import Cycles
- 1-file cycle: `src/service/midprice.rs -> src/service/midprice.rs`

## Hyperedges (group relationships)
- **P0 live-trading safety remediation (R-01, R-02, R-03)** — audit_codebase_review_r01_depth_guard, audit_codebase_review_r02_fill_state, audit_codebase_review_r03_order_validation, audit_progress_p0_safety [EXTRACTED 1.00]
- **Live mode fail-closed explained across docs** — claude_live_fail_closed, docs_security_live_safeguards, docs_trading_modes_live, docs_architecture_state_boundaries [INFERRED 0.85]
- **Copy-trading pipeline stages documented** — claude_pipeline, docs_architecture_pipeline, docs_api_polygon_ws, docs_api_gamma, docs_api_clob_order, docs_api_clob_midpoint [INFERRED 0.85]
- **PolyCrab TUI dashboard panels** — assets_image1_session_metrics_panel, assets_image1_positions_table, assets_image1_health_limits_panel [EXTRACTED 1.00]
- **PolyCrab TUI dashboard panels** — assets_image2_session_metrics_panel, assets_image2_trades_panel, assets_image2_activity_panel [EXTRACTED 1.00]

## Communities (32 total, 6 thin omitted)

### Community 0 - "Docs & Audit Roadmap"
Cohesion: 0.05
Nodes (69): Bug Report Issue Template, Feature Request Issue Template, PolyCrab Codebase Review, Feature Roadmap (fill-aware execution, depth guard, persistence), OrderExecutor (central execution boundary), R-01 Fail closed when depth guard cannot run, R-02 Separate submission from fill-confirmed position state, R-03 Validate financial inputs and reject zero orders (+61 more)

### Community 1 - "Shared Imports & Errors"
Cohesion: 0.06
Nodes (48): build_filter(), duplicate_log_identity_is_ignored_with_a_bounded_set(), fallback_identity_distinguishes_logs_without_log_index(), filter_targets_v2_exchange_and_maker_topic(), handle_log(), LOG_CHANNEL_CAPACITY, log_identity(), log_index_distinguishes_events_in_one_transaction() (+40 more)

### Community 2 - "Operator Event Bus"
Cohesion: 0.05
Nodes (49): activity_is_bounded_and_counters_are_distinct(), ACTIVITY_LIMIT, ActivityItem, ActivityLevel, Error, Info, Success, Warning (+41 more)

### Community 3 - "CLOB Signing & Auth"
Cohesion: 0.06
Nodes (29): ExchangeConfig, BookLevel, BookSnapshot, OrderType, Fak, Gtd, Side, Buy (+21 more)

### Community 4 - "Position Monitor & TP/SL"
Cohesion: 0.07
Nodes (23): TpSlConfig, PlannedOrder, check_exit(), ExitReason, StopLoss, TakeProfit, flat_range_does_not_trigger(), monitor_once() (+15 more)

### Community 5 - "Release Workflow"
Cohesion: 0.05
Nodes (46): name, permissions, runs-on, steps, strategy, os, target, tag (+38 more)

### Community 6 - "Config & Logging Setup"
Cohesion: 0.08
Nodes (12): BotKind, CopyTrading, BotRuntime, run(), run_with_operator(), spawn(), Cli, Command (+4 more)

### Community 7 - "Order Math Utils"
Cohesion: 0.11
Nodes (29): limit_price_for(), AMOUNT_SCALE, AMOUNT_STEP, clamp_price(), clamps_above_upper(), clamps_below_lower(), decimal_units(), decimal_units_rejects_overflow_instead_of_casting() (+21 more)

### Community 9 - "AppConfig Validation"
Cohesion: 0.10
Nodes (15): BotConfig, committed_config_passes_validation(), config(), Credentials, CredentialsBot, CredentialsFile, finite_nonnegative(), inverted_order_limits_are_rejected() (+7 more)

### Community 10 - "Risk Guard"
Cohesion: 0.15
Nodes (19): RiskConfig, BlockReason, BookUnavailable, ConsecutiveLargeTrades, InsufficientDepth, Tripped, cfg(), depth_check_blocks_thin_book() (+11 more)

### Community 11 - "Copy Strategy Sizing"
Cohesion: 0.19
Nodes (22): CopyStrategy, Adaptive, Fixed, Percentage, StrategyConfig, VenueId, Polymarket, WhaleTrade (+14 more)

### Community 12 - "Dashboard Drawing"
Cohesion: 0.22
Nodes (16): draw(), draw_dashboard(), draw_launcher(), format_duration(), format_expiry(), format_price(), missing_values_are_explicit(), pnl_cell() (+8 more)

### Community 13 - "CI Rust Workflow"
Cohesion: 0.11
Nodes (17): permissions, runs-on, steps, jobs, build, name, on, pull_request (+9 more)

### Community 14 - "Eligibility Filters"
Cohesion: 0.22
Nodes (16): FiltersConfig, category_block_wins_over_allow(), check(), closed_market_always_blocked(), default_filters_allow_everything(), Eligibility, Allowed, BlockedByCategory (+8 more)

### Community 15 - "Market Cache"
Cohesion: 0.22
Nodes (4): MarketCache, MarketInfo, parse_market(), parses_contract_metadata_and_outcomes()

### Community 16 - "Execution Outcomes"
Cohesion: 0.16
Nodes (16): ExecutionOutcome, DryRun, Skipped, Submitted, UnknownSubmission, SkipReason, AlreadyOpen, BelowSizing (+8 more)

### Community 17 - "TUI Input & Focus"
Cohesion: 0.20
Nodes (12): activity_items(), activity_max_scroll(), Focus, Activity, Positions, handle_key(), move_selection(), refresh_runtime() (+4 more)

### Community 18 - "Executor Test Fixtures"
Cohesion: 0.23
Nodes (6): config(), gamma_fixture(), live_mode_is_rejected_before_a_clob_client_is_created(), paper_mode_accepts_gamma_fee_type_and_records_order(), paper_mode_keeps_large_trade_usable_without_a_book_provider(), trade()

### Community 19 - "UI Formatting Helpers"
Cohesion: 0.27
Nodes (9): connection_color(), format_money(), format_signed_money(), metric_line(), mode_color(), render_health(), render_metrics(), risk_remaining() (+1 more)

### Community 20 - "Config Limits & Caps"
Cohesion: 0.22
Nodes (8): AppConfig, cap_utilization(), cleanup_terminal(), launcher_detail(), mode_label(), run(), short_address(), ui_loop()

### Community 21 - "Cache TTL & Gamma"
Cohesion: 0.22
Nodes (4): ClobTokenIds, DEFAULT_TTL, MAX_ENTRIES, parse_timestamp()

### Community 23 - "Position Sorting"
Cohesion: 0.29
Nodes (5): PositionSort, Contract, Exposure, Freshness, Pnl

### Community 24 - "TUI Screenshot Overview"
Cohesion: 0.47
Nodes (6): PolyCrab copy-trading TUI screenshot, Health & limits panel (mode, WS, risk, limit, cap util, fees), Keybindings bar (overview, activity, positions, search, sort, pause, stop, theme, quit), Simulated positions table (BTC/ETH/XRP Up or Down, entry, mid, P&L), Session metrics panel (open, deployed, value, unrealized, latency, block), Simulated trading mode (no real account balance)

### Community 25 - "TUI Screenshot Activity"
Cohesion: 0.47
Nodes (6): PolyCrab copy-trading TUI screenshot (simulated mode), Activity panel (QUOTE events with mid price and P&L), Keybinding footer (overview, activity, positions, search, sort, pause, stop, theme, quit), Session metrics panel (open, deployed, value, unrealized P&L, latency, block), Simulated/paper trading mode, Trades panel (source to copy, execution reason, paper results)

### Community 26 - "UI Tests"
Cohesion: 0.33
Nodes (4): config(), launcher_and_dashboard_render_in_small_terminal(), position(), position_search_and_sort_use_real_position_fields()

### Community 27 - "UI Views"
Cohesion: 0.40
Nodes (5): View, Activity, Launcher, Overview, Positions

## Knowledge Gaps
- **91 isolated node(s):** `polycrab`, `CopyTrading`, `Percentage`, `Fixed`, `Adaptive` (+86 more)
  These have ≤1 connection - possible missing edges or undocumented components. (Counts symbols only; 238 node(s) total have ≤1 connection when file, concept and rationale nodes are included.)
- **6 thin communities (<3 nodes) omitted from report** — run `graphify query` to explore isolated nodes.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `AppConfig` connect `Config Limits & Caps` to `Shared Imports & Errors`, `CLOB Signing & Auth`, `Position Monitor & TP/SL`, `Config & Logging Setup`, `Order Math Utils`, `TUI Terminal Rendering`, `AppConfig Validation`, `Risk Guard`, `Copy Strategy Sizing`, `Dashboard Drawing`, `Eligibility Filters`, `TUI Input & Focus`, `Executor Test Fixtures`, `UI Formatting Helpers`, `Executor Position Access`, `UI Tests`?**
  _High betweenness centrality (0.113) - this node is a cross-community bridge._
- **What connects `polycrab`, `CopyTrading`, `Percentage` to the rest of the system?**
  _91 weakly-connected nodes found - possible documentation gaps or missing edges._
- **Should `Docs & Audit Roadmap` be split into smaller, more focused modules?**
  _Cohesion score 0.0517503805175038 - nodes in this community are weakly interconnected._
- **Why does `strategy` connect `Release Workflow` to `Executor Test Fixtures`?**
  _High betweenness centrality (0.099) - this node is a cross-community bridge._
- **Should `Shared Imports & Errors` be split into smaller, more focused modules?**
  _Cohesion score 0.05517503805175038 - nodes in this community are weakly interconnected._
- **Should `Operator Event Bus` be split into smaller, more focused modules?**
  _Cohesion score 0.05446853516657853 - nodes in this community are weakly interconnected._
- **Should `CLOB Signing & Auth` be split into smaller, more focused modules?**
  _Cohesion score 0.05669199298655757 - nodes in this community are weakly interconnected._