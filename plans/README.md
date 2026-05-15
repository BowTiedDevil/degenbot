# Architecture Deepening Plans

Plans are numbered sequentially in a single `0xx` series, grouped by domain.

See the [skill vocabulary](https://github.com/user/skills/improve-codebase-architecture) for terms: **module**, **interface**, **depth**, **seam**, **adapter**, **leverage**, **locality**.

## Active Plans

| # | Plan | Summary |
|---|------|---------|
| 014 | [Async REPL](14-async-repl.md) | `python -m degenbot` with top-level `await`. |
| — | [Arbitrage Optimizer](arbitrage-optimizer/) | Multi-file project for production arbitrage optimization. |
| 033 | [Consolidate Dual Pool-to-Hop Conversion](033-dual-hop-conversion-consolidation.md) | **COMPLETE** — Inlined thin wrappers, removed `PoolCompatibility` enum, deleted `solver_hop_builders.py`. |
| 034 | [Delete Legacy Arbitrage Cycle Classes](034-delete-legacy-arbitrage-cycles.md) | **REJECTED** — superseded by Plan 038. |
| 035 | [Builder Protocol — Replace Union Type with Shared Interface](035-builder-protocol.md) | `PoolBuilder` protocol replaces 4× union type annotation. `_dispatch_build()` isinstance chain → `**kwargs` forwarding one-liner. Typed `build_xxx_pool()` methods stay as delegates (60+ test sites). |
| 036 | [Consolidate SwapAmounts Dispatch into Self-Contained Subclasses](036-swap-amounts-consolidation.md) | `AbstractSwapAmounts` gets `input_amount()`/`output_amount()` methods (avoids V3/V4 `amount_in` field collision). `_extract_amount_in/out` deleted. Pool `build_swap_amount()` replaces isinstance chain. Depends on Plan 038. |
| 037 | [Split `functions.py` into Domain-Aligned Modules](037-split-functions-module.md) | ~~14-function grab-bag → 5 domain-aligned modules. `eip_191_hash` deleted (dead code, zero imports). 56 import sites to migrate. No circular import risk. Independent of 033–038.~~ **COMPLETE** |
| 038 | [Deprecate Legacy Arbitrage Cycle Classes](038-deprecate-legacy-arbitrage-cycles.md) | Move legacy cycles to `_legacy/` with underscore class names + `DeprecationWarning`. Delete dead `AbstractArbitrage`/`get_arbitrage_helpers()`. Migration guide. cvxpy → optional dep. Prerequisite for Plan 033. |

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
