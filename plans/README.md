# Architecture Deepening Plans

Plans are numbered sequentially in a single `0xx` series, grouped by domain.

See the [skill vocabulary](https://github.com/user/skills/improve-codebase-architecture) for terms: **module**, **interface**, **depth**, **seam**, **adapter**, **leverage**, **locality**.

## Writing Plans

New plans **must** follow the [template](TEMPLATE.md). The template is derived from the clearest completed plans (035, 039, 040, 045). Key requirements:

1. **Deletion test** — state what happens if you delete the code; this distinguishes reorganizing from removing
2. **Specific friction table** — concrete, falsifiable rows (not vague complaints)
3. **Vertical slices** — each slice ships independently with a green test suite
4. **Design decisions** — record non-obvious choices with rationale so reviewers don't infer them
5. **Relationship to other plans** — list every intersecting plan with its relationship (prerequisite / complementary / orthogonal / superseded)
6. **Status checklist** — `[ ]` unchecked items; mark `[x]` and note results when complete

## Active Plans

| # | Plan | Summary |
|---|------|---------|
| 014 | [Async REPL](014-async-repl.md) | `python -m degenbot` with top-level `await`. |
| 070 | [Balancer Builder](070-balancer-builder.md) | Builder, type resolution, `to_hop_state()`, and `external_update()` for Balancer V2 pools. |
| — | [Arbitrage Optimizer](arbitrage-optimizer/) | Multi-file project for production arbitrage optimization. |

## Completed Plans

| # | Plan | Summary |
|---|------|---------|
| 001 | [Extract Pool Builders from Bot](completed/001-pool-builders.md) | 2110 → ~544 lines in bot.py. Five `build_*` methods and `update()` extracted into typed builder classes. I/O code removed from session class. |
| 002 | [Pool Class Registry](completed/002-pool-class-registry.md) | DEX self-registration replaces hard-coded class maps in Bot. **Superseded by Plan 016.** |
| 003 | [Unify V3/V4 Tick Data Fetcher Factories](completed/003-unified-tick-fetcher.md) | Two near-identical fetcher factories unified into a single parameterized `make_tick_data_fetcher`. |
| 004 | [Eliminate isinstance Dispatch in Bot.update()](completed/004-update-dispatch.md) | 5 → 0 isinstance branches. Subsumed by Plan 001. |
| 005 | [Move Curve Fetcher Factories into Curve Module](completed/005-curve-fetcher-factory.md) | ~250 lines move out of bot.py. |
| 006 | [Universal `build_pool` with Type Resolution](completed/006-universal-build-pool.md) | Single `build_pool(address, pool_id=…)` entry point with type resolution from DB, registry, and on-chain probing. |
| 007 | [Collapse Aave Token Processor Revision Matrix](completed/007-aave-token-processors.md) | Simplify the token processor revision system. |
| 008 | [Extract Per-OperationType Handlers Behind a Pipeline Seam](completed/008-aave-event-enrichment-handlers.md) | Replace the ~300-line `ScaledEventEnricher.enrich()` monolith with an `OperationHandler` pipeline. 13 handlers, feature flag removed. |
| 009 | [Separate I/O from Calculation in Position Analysis](completed/009-aave-position-analysis-io-free.md) | I/O-free architecture for Aave position analysis. Pure `core.py` + I/O `orchestrator.py`. |
| 010 | [Parameterize Aave Event Model Taxonomy](completed/010-aave-event-models-parameterized.md) | 18 Pydantic event classes → single `EnrichedScaledTokenEvent`. |
| 011 | [Unify UniswapLpCycle._calculate() Behind the ArbSolver Seam](completed/011-arbitrage-lp-cycle-solver-unification.md) | `ArbSolver.solve()` delegation. Dual maintenance eliminated. |
| 012 | [Bot Session](completed/012-bot-session.md) | Bot session pattern. |
| 013 | [Curve StableSwap I/O-Free Architecture](completed/013-curve-io-free-architecture.md) | Migrate Curve StableSwap pools to the I/O-free architecture with fetcher protocols. |
| 015 | [Extract ChainDataSource Abstraction from Bot](completed/015-chain-data-source-abstraction.md) | Superseded by Plan 001. |
| 016 | [Unified Pool Type Registry](completed/016-unified-pool-type-registry.md) | Replace scattered mappings with single `pool_type_registry.register()`. Auto-derives invariant, variant, kind from class. |
| 017 | [Complete I/O-Free Migration for V2/V3/V4/Aerodrome Pools](completed/017-v2-v3-io-free-migration.md) | Remove all `ProviderAdapter`-taking methods from pool classes. Completes ADR-001 Phase 3. |
| 018 | [Decompose CurvePoolBuilder.build() into Detection Sub-Modules](completed/018-curve-pool-builder-decomposition.md) | Break 400-line `build()` into focused detectors. |
| 019 | [Replace ArbPoolCacheAdapter getattr Chain with Protocol Methods](completed/019-pool-cache-adapter-protocol.md) | `CacheablePool` protocol with `reserves_for_cache()` / `fee_for_cache()`. |
| 020 | [Unify the Dual PoolInvariant Enum](completed/20-unify-pool-invariant-enum.md) | Renamed identity-level `PoolInvariant` to `PoolFamily`. Kept `PoolInvariant` in `hop_types.py` for solver dispatch. |
| 021 | [Extract SwapEncoder from UniswapLpCycle and Deprecate Legacy Path](completed/021-extract-swap-encoder.md) | Standalone `encoding.py` with pluggable `ApprovalStrategy` / `PayloadComposer`. |
| 022 | [Remove Backward Compatibility Shims and Aliases](completed/022-remove-backward-compat-shims.md) | Removed `*_legacy` functions, `hop_factory`/`Hop` alias, stale re-exports, and legacy `register_web3()`. |
| 023 | [Consolidate CLI Pool Update Functions](completed/023-consolidate-pool-update-functions.md) | 14 near-identical updater functions → 3 parameterized implementations (V2, V3, V4) with frozen dataclass configs. pool.py: 1934 → 1027 lines. |
| 024 | [Extract Generic Address Registry Base Class](completed/024-generic-registry-base-class.md) | PEP-695 parameterized `AddressRegistry[T]` / `MultiKeyAddressRegistry[T]` base classes. |
| 025 | [Remove Web3 Bypass — Route All RPC Through ProviderAdapter](completed/025-remove-web3-bypass.md) | Replace 49 direct `w3.eth.*` call sites with `ProviderAdapter` methods. |
| 026 | [Replace Address-Dispatched Behaviour in CurveStableswapPool with Strategy Objects](completed/026-curve-strategy-objects.md) | 26 `if self.address` dispatches → `SwapStyle`/`LendingRateStyle`/`MetapoolRateStyle`/`MetapoolUnderlyingStyle` enums. 66 addresses in `_pool_strategies.py`. Pool class 2122→1708 lines. |
| 027 | [Convert Curve Lending-Rate Methods to Typed Fetcher Protocols](completed/027-curve-lending-rate-fetchers.md) | Remove 6 `_stored_rates_from_*()` methods, `provider_call`, `oracle_method`. Add `LendingRateFetcher` protocol with 7 factory methods. Curve pools fully I/O-free. |
| 028 | [Builder Registry & Pool Class Restructuring](completed/028-builder-registry.md) | 4 phases: (1) `calculations/` module, (2) state+calc mixins for all pool families, (3) protocols replace ABCs, (4) `dict[type, PoolBuilder]` dispatch on Bot. Adding a new pool family: 5→2 touch points. |
| 029 | [Externalize Curve Variant Group Addresses from Pool Class to Configuration](completed/029-variant-group-externalization.md) | 7 class-level frozensets (67 addresses) → `_variant_groups.py` with `DVariant`/`YVariant`/`YDVariant` enums. |
| 030 | [Consolidate Exception Module Files](completed/030-consolidate-exceptions.md) | 12 exception files → 4 domain-aligned files (`base`, `pool`, `arbitrage`, `infrastructure`). Public API unchanged. |
| 031 | [Context Docs Cleanup](completed/031-context-docs-cleanup.md) | Align all CONTEXT.md files with grill-with-docs format. |
| 032 | [Rename PoolManager → PoolTracker](completed/032-rename-pool-manager-to-tracker.md) | 14 classes, 8 module files, Bot API renamed. Eliminates naming collision with V4 on-chain PoolManager contract. |
| 033 | [Consolidate Dual Pool-to-Hop Conversion](completed/033-dual-hop-conversion-consolidation.md) | Inlined thin wrappers, removed `PoolCompatibility` enum, deleted `solver_hop_builders.py`. Pool `to_hop_state()` is single source of truth. |
| 034 | [Delete Legacy Arbitrage Cycle Classes](completed/034-delete-legacy-arbitrage-cycles.md) | **REJECTED** — superseded by Plan 038. |
| 035 | [Builder Protocol — Replace Union Type with Shared Interface](completed/035-builder-protocol.md) | `PoolBuilder` protocol replaces 4× union type annotation. `_dispatch_build()` isinstance chain → `**kwargs` forwarding. Typed `build_xxx_pool()` methods kept as delegates. |
| 036 | [Consolidate SwapAmounts Dispatch into Self-Contained Subclasses](completed/036-swap-amounts-consolidation.md) | `input_amount()`/`output_amount()` on AbstractSwapAmounts. `build_swap_amount()` on pool classes. Deleted `_extract_amount_in/out`. Protocol dispatch replaces isinstance chain. |
| 037 | [Split `functions.py` into Domain-Aligned Modules](completed/037-split-functions-module.md) | 5 domain-aligned modules: `provider/call_helpers.py`, `provider/log_fetching.py`, `contract/addresses.py`, `calculations/evm_math.py`, `provider/block_helpers.py`. `eip_191_hash` deleted (dead code). `functions.py` deleted. |
| 038 | [Deprecate Legacy Arbitrage Cycle Classes](completed/038-deprecate-legacy-arbitrage-cycles.md) | Legacy cycles moved to `_legacy/` with underscore names + `DeprecationWarning`. Deleted `AbstractArbitrage`/`get_arbitrage_helpers()`. `cvxpy` moved to optional `legacy-cycles` extra. Migration guide. |
| 040 | [Curve Data Provider](completed/040-curve-data-provider.md) | 13 fetcher callback parameters → 1 `CurveDataProvider` seam. Pool class 999→940 lines. Pickle: 13 drops+reconstructs → 1. Builder: 13 fetcher calls → 1 `create_provider()`. |
| 039 | [Curve DyCalculator Seam](completed/039-curve-dy-calculator-seam.md) | 14 `match`/`if` dispatch branches → injectable `DyCalculator` objects keyed on `SwapStyle`/`MetapoolRateStyle`/`MetapoolUnderlyingStyle`. Pure math in `calculations/stableswap.py`. Pool class 1698→999 (−41%). |
| 041 | [Elevate Curve State Mixin](completed/041-elevate-curve-state-mixin.md) | 25 attributes + 22 properties with `_xxx` private pattern. `StableswapPoolState` mixin. |
| 042 | [Collapse Provider Adapter Mirror](completed/042-collapse-provider-adapter-mirror.md) | Merged `EthereumProvider`+`_SyncProviderBackend`→`ProviderBackend`. `__getattr__` dispatch replaces 15× delegation methods. Block guards simplified. 914→780 lines (−15%). |
| 043 | [Extract V2 Variant Builders](completed/043-extract-v2-variant-builders.md) | `V2PoolBuilder` 375→118 lines (68% reduction). Per-variant builders: `V2BuilderBase`, `AerodromeV2Builder`, `CamelotBuilder`. |
| 044 | [Deprecate Bot Pass-Throughs](completed/044-deprecate-bot-pass-throughs.md) | Deprecated `build_v2_pool`, `build_v3_pool`, `build_v4_pool`, `build_curve_pool` with `DeprecationWarning`. Removed by Plan 059. |
| 045 | [Calculator Explicit Data](completed/045-calculator-explicit-data.md) | Replace `pool` parameter with `DyCalculationInputs` in DyCalculator. 77 SLF001 errors → 0. Calculators are pure consumers of pre-resolved data. |
| 046 | [eth_subscribe Support](completed/046-eth-subscribe.md) | `eth_subscribe` via AlloyProvider with AsyncProviderAdapter wiring and SubscriptionManager callback layer. |
| 047 | [Event-Driven Log Listener](completed/047-event-driven-listener.md) | Subscription double-buffer drain + LogListener dispatch registry + pool LOG_HANDLERS. Replaces SubscriptionManager. |
| 050 | [Generic StateCache for Pool State Temporal Navigation](completed/050-generic-state-cache.md) | 4 near-identical deque+lock+navigation implementations → 1 `StateCache[T]`. PEP 695 syntax. Caller holds lock. |
| 051 | [Extract BuilderContext from Bot Constructor Wiring](completed/051-builder-context.md) | 35 lines of builder wiring → 1 `BuilderContext` + 7 one-liners. Adding a new builder: 2 lines, not 7. |
| 049 | [Replace CurveFetcherFactory Closures with Structured CurveDataProvider Implementation](completed/049-curve-data-provider-impl.md) | 850-line closure bag → structured `CurveDataProviderImpl` (~350 lines) with shared helpers. 13 closures → real methods. Readable stack traces, individually testable methods, simpler pickle. |
| 048 | [Unify Bot and AsyncBot via Builder-Backed IO Seam](completed/048-async-builder-shared.md) | AsyncBot delegates to async builders instead of duplicating construction. `PoolIO` protocol parameterizes builders over sync/async. ~965→~466 lines in AsyncBot. |
| 052 | [Migrate V3/V4/Curve/ERC20 Builders to Full PoolIO](completed/052-v3v4curve-poolio-migration.md) | Remove all `ProviderAdapter`/`ConnectionManager` dependencies from V3, V4, Curve, and ERC20 builders. Remove `connections` from `BuilderContext`. |
| 053 | [Delete Old Optimizer Hierarchy](completed/053-delete-old-optimizer-hierarchy.md) | Remove deprecated `ArbitrageOptimizer` ABC, `OptimizerResult`/`OptimizerType`, and 7 concrete classes with zero production callers. Extract pure Möbius math into `_mobius_math.py`. |
| 054 | [Consolidate Curve Pool On-Chain Caches](completed/054-consolidate-curve-on-chain-caches.md) | 10 individual `BoundedCache` fields → single `CurveOnChainCache` object with try-cache→call-provider→store→return pattern. Delete dead code after `return inputs`. Pool class 1160→988 lines (−15%). |
| 055 | [Delete Deprecated Fetcher Protocol Dead Code](completed/055-delete-deprecated-fetcher-protocols.md) | Delete 8 deprecated `*Fetcher` protocol classes from `curve/types.py`. Zero callers; superseded by `CurveDataProvider` (Plan 040). |
| 056 | [Move Calculator Factory Functions to Enum Types](completed/056-externalize-curve-strategy-mapping-to-db.md) | Move `_make_dy_calculator` etc. from `_pool_strategies.py` onto enum types. Remove pool class import dependency on strategy resolution module. Make calculators non-optional on `PoolStrategies`. |
| 057 | [Document Curve Pool's Partial I/O Status](completed/057-document-curve-pool-partial-io-status.md) | Rename `_build_calculation_inputs` → `_resolve_calculation_inputs_via_io`. Add `requires_io_at_calculation_time` property. Amend ADR-001 with construction-time vs calculation-time I/O table. |
| 058 | [Collapse Subscription Stubs in Provider Adapters](completed/058-collapse-subscription-stubs.md) | `SyncSubscriptionSupport` and `AsyncSubscriptionSupport` mixins replace 25 duplicated `raise SubscriptionNotSupported` stubs across 5 adapters. Stub count: 25→10. |
| 059 | [Delete Deprecated `build_*` Pass-Throughs and `get_web3`](completed/059-delete-deprecated-build-pass-throughs.md) | Delete `build_v2_pool`, `build_v3_pool`, `build_v4_pool`, `build_curve_pool`, `get_web3` from Bot/AsyncBot/ConnectionManagers. ~243 lines. |
| 061 | [Delete `EthereumProvider` Backward-Compatibility Alias](completed/061-delete-ethereum-provider-alias.md) | Delete `EthereumProvider = ProviderBackend` alias and update all stale references (code, docstrings, domain docs, tests). Zero callers. |
| 060 | [Unify Sync/Async Builder Orchestration](completed/060-unify-builder-orchestration.md) | V3BuilderBase and V4BuilderBase with shared @staticmethod helpers (decode_slot0, extract_db_values, load_tick_snapshot). Frozen dataclasses for decoded values. ~150 lines of duplication removed. Async tick fetcher not viable (pool objects are sync). |
| 062 | [Extract Chainlink into Package](completed/062-extract-chainlink-package.md) | Move `chainlink.py` into `chainlink/` package with CONTEXT.md. Delete unused `CHAINLINK_PRICE_FEED_ABI`. 3-file package: `__init__.py`, `price_feed.py`, `CONTEXT.md`. |
| 063 | [Rust GIL, Alloc, Testing](completed/063-rust-gil-alloc-testing.md) | GIL discipline fixes, allocation reduction (ABI type cache, optimizer), testing gaps (concurrency, subscriptions, proptests). Fixed `f64_to_u256` bug (128-bit decomposition → 256-bit). |
| 064 | [CVXPY Usage Improvements](completed/064-cvxpy-usage-improvements.md) | Fix DPP assertion ordering, `enforce_dpp=True` on re-solve, solver constant references; benchmark COO backend (no benefit at N≤5); extract 2-pool problem factory deduplicating 3 test methods. |
| 065 | [Collapse AsyncBot Inline I/O](completed/065-collapse-asyncbot-inline-io.md) | 4 inline I/O methods (-61 lines) routed through AsyncErc20Builder. `str | Erc20Token` overloads on Bot, `str` on AsyncBot (auto-build wrapper). |
| 066 | [Unify Type Resolution Sync/Async](completed/066-unify-type-resolution-sync-async.md) | 4 mirror functions → 2 thin wrappers + 2 shared pure functions. ~56 lines of duplication removed. |
| 067 | [BuildPoolRequest](completed/067-build-pool-request.md) | Replace `dispatch_kwargs` dict + `**kwargs` forwarding with typed `BuildPoolRequest` frozen dataclass. All 9 builders migrated one-shot. Eliminates silent typo-swallowing. |
| 068 | [Absorb CurveOnChainCache into CurveStableswapPool](completed/068-absorb-curve-on-chain-cache.md) | Merge `CurveOnChainCache` into `CurveStableswapPool` as private `_cache_*` fields and `_get_cached_*` methods. Eliminates `getattr` dynamic dispatch, duplicate `_data_provider` reference, and separate pickle policy. |
