# Architecture Deepening Plans

Plans are numbered sequentially in a single `0xx` series, grouped by domain.

See the [skill vocabulary](https://github.com/user/skills/improve-codebase-architecture) for terms: **module**, **interface**, **depth**, **seam**, **adapter**, **leverage**, **locality**.

## Active Plans

### Pool / Bot Architecture

| # | Plan | Summary |
|---|------|---------|
| 030 | [Consolidate Exception Module Files](completed/030-consolidate-exceptions.md) | Merge 12 exception files (583 lines) into 4 domain-aligned files: `base.py`, `pool.py`, `arbitrage.py`, `infrastructure.py`. Public API unchanged. |

### Aave

(No active Aave plans — Plans 008, 009, 010 are complete.)

### Arbitrage

(No active arbitrage plans — Plans 011, 019, 021 are complete.)

### Documentation

(No active documentation plans — Plan 031 is complete.)

### Pool / Bot Architecture

| # | Plan | Summary |
|---|------|---------|
| 032 | [Rename PoolManager → PoolTracker](032-rename-pool-manager-to-tracker.md) | Rename all off-chain pool manager classes, modules, and Bot API to "tracker" naming. 14 classes, 8 module files, Bot.add_tracker(). Backward-compat aliases included. |

### Infrastructure

| # | Plan | Summary |
|---|------|---------|
| 014 | [Async REPL](14-async-repl.md) | `python -m degenbot` with top-level `await`. |

### Extended Projects

| # | Plan | Summary |
|---|------|---------|
| — | [Arbitrage Optimizer](arbitrage-optimizer/) | Multi-file project for production arbitrage optimization. |

## Completed Plans

| # | Plan | Summary |
|---|------|---------|
| 001 | [Extract Pool Builders from Bot](completed/001-pool-builders.md) | 2110 → ~544 lines in bot.py. Five `build_*` methods and `update()` extracted into typed builder classes. I/O code removed from session class. |
| 002 | [Pool Class Registry](completed/002-pool-class-registry.md) | DEX self-registration replaces hard-coded class maps in Bot. `PoolClassRegistry` maps (chain_id, factory_address) → pool class. **Superseded by Plan 016.** |
| 003 | [Unify V3/V4 Tick Data Fetcher Factories](completed/003-unified-tick-fetcher.md) | Two near-identical fetcher factories unified into a single parameterized `make_tick_data_fetcher`. |
| 004 | [Eliminate isinstance Dispatch in Bot.update()](completed/004-update-dispatch.md) | 5 → 0 isinstance branches. Move type dispatch into per-type builders from Plan 001. Subsumed by Plan 001. |
| 005 | [Move Curve Fetcher Factories into Curve Module](completed/005-curve-fetcher-factory.md) | ~250 lines move out of bot.py. 12 `_make_curve_*` methods become a `CurveFetcherFactory` class in the Curve module. |
| 006 | [Universal `build_pool` with Type Resolution](completed/006-universal-build-pool.md) | Single `build_pool(address, pool_id=…)` entry point with type resolution from DB, registry, and on-chain probing. |
| 011 | [Unify UniswapLpCycle._calculate() Behind the ArbSolver Seam](completed/011-arbitrage-lp-cycle-solver-unification.md) | Deleted `_arb_profit()`, replaced `minimize_scalar` with `ArbSolver.solve()` delegation. Dual maintenance eliminated. |
| 007 | [Collapse Aave Token Processor Revision Matrix](completed/007-aave-token-processors.md) | Simplify the token processor revision system. |
| 012 | [Bot Session](completed/012-bot-session.md) | Bot session pattern. |
| 013 | [Curve StableSwap I/O-Free Architecture](completed/013-curve-io-free-architecture.md) | Migrate Curve StableSwap pools to the I/O-free architecture with fetcher protocols. |
| 015 | [Extract ChainDataSource Abstraction from Bot](completed/015-chain-data-source-abstraction.md) | Superseded by Plan 001. Builders already receive `connections`/`db` via DI — the I/O seam exists. |
| 016 | [Unified Pool Type Registry](completed/016-unified-pool-type-registry.md) | Replace scattered PoolClassRegistry + FACTORY_DEPLOYMENTS +_KIND_TO_DESCRIPTOR + _variant_from_class with single `pool_type_registry.register()`. Auto-derives invariant, variant, kind from class. |
| 019 | [Replace ArbPoolCacheAdapter getattr Chain with Protocol Methods](completed/019-pool-cache-adapter-protocol.md) | `CacheablePool` protocol with `reserves_for_cache()` / `fee_for_cache()` replaces `getattr` introspection. |
| 020 | [Unify the Dual PoolInvariant Enum](completed/20-unify-pool-invariant-enum.md) | Renamed identity-level `PoolInvariant` to `PoolFamily`. Kept `PoolInvariant` in `hop_types.py` for solver dispatch. |
| 021 | [Extract SwapEncoder from UniswapLpCycle and Deprecate Legacy Path](completed/021-extract-swap-encoder.md) | Standalone `encoding.py` with pluggable `ApprovalStrategy` / `PayloadComposer`. `UniswapLpCycle` deprecated in favor of `ArbitragePath`. |
| 017 | [Complete I/O-Free Migration for V2/V3/V4/Aerodrome Pools](completed/017-v2-v3-io-free-migration.md) | Remove all `ProviderAdapter`-taking methods from pool classes. Delete `get_reserves()`, `get_immutable_pool_values()`, `from_chain` classmethods. Completes ADR-001 Phase 3. |
| 022 | [Remove Backward Compatibility Shims and Aliases](completed/022-remove-backward-compat-shims.md) | Removed `*_legacy` functions, `hop_factory`/`Hop` alias, `pool_hop_adapter`, stale F401 re-exports, and legacy `register_web3()`. |
| 026 | [Replace Address-Dispatched Behaviour in CurveStableswapPool with Strategy Objects](completed/026-curve-strategy-objects.md) | Replace 26 `if self.address` dispatches with `SwapStyle`/`LendingRateStyle`/`MetapoolRateStyle`/`MetapoolUnderlyingStyle` enums. 66 addresses mapped in `_pool_strategies.py`. Pool class 2122→1708 lines. |
| 027 | [Convert Curve Lending-Rate Methods to Typed Fetcher Protocols](completed/027-curve-lending-rate-fetchers.md) | Remove 6 `_stored_rates_from_*()` methods (~250 lines), `provider_call`, `oracle_method`, `LENDING_PRECISION`. Add `LendingRateFetcher` protocol with 7 factory methods. Curve pools fully I/O-free. |
| 029 | [Externalize Curve Variant Group Addresses from Pool Class to Configuration](completed/029-variant-group-externalization.md) | Move 7 class-level frozensets (67 addresses) to `_variant_groups.py` with `resolve_d_variant()`/`resolve_y_variant()`/`resolve_yd_variant()`. Add `DVariant`/`YVariant`/`YDVariant` enums. |
| 024 | [Extract Generic Address Registry Base Class](completed/024-generic-registry-base-class.md) | Extract common key-handling and deduplication logic from `PoolRegistry`/`TokenRegistry`/`ManagedPoolRegistry` into PEP-695 parameterized `AddressRegistry[T]` / `MultiKeyAddressRegistry[T]` base classes. |
| 025 | [Remove Web3 Bypass — Route All RPC Through ProviderAdapter](completed/025-remove-web3-bypass.md) | Replace 49 direct `w3.eth.*` call sites with `ProviderAdapter` methods. Deprecate `get_web3()` and `.underlying`. |
| 008 | [Extract Per-OperationType Handlers Behind a Pipeline Seam](completed/008-aave-event-enrichment-handlers.md) | Replace the ~300-line `ScaledEventEnricher.enrich()` monolith with an `OperationHandler` pipeline. 13 handlers, `EnrichmentContext` shared services, feature flag removed. |
| 010 | [Parameterize Aave Event Model Taxonomy](completed/010-aave-event-models-parameterized.md) | 18 Pydantic event classes → single `EnrichedScaledTokenEvent`. Properties derive from `ScaledTokenEventType` enum via module-level sets. |
| 009 | [Separate I/O from Calculation in Position Analysis](completed/009-aave-position-analysis-io-free.md) | I/O-free architecture for Aave position analysis. Pure `core.py` + I/O `orchestrator.py` with `PriceFetcher`/`PositionQuery` protocols. Flat records replace ORM navigation. |
| 031 | [Context Docs Cleanup](031-context-docs-cleanup.md) | Align all CONTEXT.md files with grill-with-docs format: glossaries only, example dialogues, one authoritative definition per term, implementation details extracted to reference docs. |

## Dependency Graph

```
Plan 001 (Pool Builders) ✅           ← foundational; others build on it
  ├── Plan 002 (Class Registry) ✅     ← superseded by Plan 016
  ├── Plan 003 (Tick Fetcher) ✅        ← unified into builders
  ├── Plan 004 (Update Dispatch) ✅     ← subsumed by Plan 001
  └── Plan 005 (Curve Factory) ✅      ← moved into builders

Plan 006 (Universal build_pool) ✅     ← depends on Plan 002 ✅
Plan 013 (Curve I/O-Free) ✅           ← established fetcher pattern
Plan 016 (Unified Pool Type Registry) ✅ ← supersedes Plan 002
Plan 019 (Pool Cache Adapter Protocol) ✅ ← CacheablePool protocol replaces getattr chain
Plan 020 (Unify PoolInvariant Enum) ✅ ← PoolFamily / PoolInvariant split
Plan 021 (Extract SwapEncoder) ✅       ← encoding.py with pluggable pipeline

Plan 026 (Curve Strategy Objects) ✅     ← replace address dispatch with strategy enums
Plan 027 (Curve Lending-Rate Fetchers) ✅ ← typed LendingRateFetcher protocol, remove provider_call
Plan 028 (Builder Registry)             ← PoolBuilder protocol + registry on Bot
Plan 029 (Variant Group Externalization) ✅ ← move addresses to config (subset of 026)
Plan 030 (Exception Consolidation)       ← 12 files → 4
Plan 031 (Context Docs Cleanup) ✅        ← documentation only; no code changes
Plan 032 (Rename PoolManager → Tracker)   ← 14 classes, 8 modules, Bot API

--- Active Plans ---

Plan 008 (Aave Enrichment Handlers) ✅    ← 13 handlers, feature flag removed
Plan 009 (Aave Position Analysis I/O-Free) ✅ ← pure core + I/O orchestrator
Plan 010 (Aave Event Models) ✅          ← 18 classes → 1 unified model
Plan 023 (CLI Pool Update Consolidation) ← consolidates 12+ updater functions
Plan 025 (Remove Web3 Bypass) ✅           ← enables Alloy/Offline providers everywhere

--- Completed Plans ---

Plan 017 (V2/V3 I/O-Free) ✅            ← completes ADR-001 Phase 3; depends on Plan 001 ✅
Plan 018 (Curve Builder Decomposition) ✅  ← independent; simplifies CurvePoolBuilder
Plan 022 (Remove Backward Compat Shims) ✅ ← all shims removed
Plan 024 (Generic Registry Base) ✅      ← PEP-695 generic `AddressRegistry[T]` / `MultiKeyAddressRegistry[T]`

--- New Plans ---

Plan 026 (Curve Strategy Objects) ✅       ← replaces 26 address dispatches with strategy enums; deepest change
Plan 027 (Curve Lending-Rate Fetchers) ✅  ← depends on Plan 026 for LendingRateStyle enum; removes provider_call
Plan 028 (Builder Registry)               ← independent; simplifies Bot wiring
Plan 029 (Variant Group Externalization) ✅ ← subset of Plan 026; can be done first as a stepping stone
Plan 030 (Exception Consolidation)        ← independent; code organization only
```

## Recommended Implementation Order

1. ~~**Plan 005** (CurveFetcherFactory)~~ ✅
2. ~~**Plan 003** (Unified tick fetcher)~~ ✅
3. ~~**Plan 001** (Pool Builders)~~ ✅
4. ~~**Plan 004** (Update dispatch)~~ ✅
5. ~~**Plan 006** (Universal build_pool)~~ ✅
6. ~~**Plan 020** (Unify PoolInvariant Enum)~~ ✅
7. ~~**Plan 019** (Pool Cache Adapter Protocol)~~ ✅
8. ~~**Plan 021** (Extract SwapEncoder)~~ ✅
9. ~~**Plan 018** (Curve Builder Decomposition)~~ ✅ — decomposition, independently testable
10. ~~**Plan 017** (V2/V3 I/O-Free)~~ ✅ — largest change, completes ADR-001

11. ~~**Plan 008** (Aave Enrichment Handlers)~~ ✅ — complete new handlers, remove feature flag
12. ~~**Plan 009** (Aave Position Analysis I/O-Free)~~ ✅ — apply I/O-free to Aave
13. ~~**Plan 010** (Parameterize Aave Event Models)~~ ✅ — collapse 18 Pydantic classes to 1
14. **Plan 023** (CLI Pool Update Consolidation) — reduces pool.py by ~1000 lines
15. ~~**Plan 025** (Remove Web3 Bypass)~~ ✅ — enables Alloy/Offline providers everywhere
16. ~~**Plan 029** (Variant Group Externalization)~~ ✅ — smallest Curve improvement; stepping stone to 026
17. ~~**Plan 026** (Curve Strategy Objects)~~ ✅ — deepest Curve change; replace all address dispatches
18. ~~**Plan 027** (Curve Lending-Rate Fetchers)~~ ✅ — complete I/O-free for Curve; depends on 026
19. **Plan 028** (Builder Registry) — independent; simplifies Bot wiring
20. **Plan 030** (Exception Consolidation) — independent; do anytime
21. ~~**Plan 031** (Context Docs Cleanup)~~ ✅ — documentation only; no code changes
22. **Plan 032** (Rename PoolManager → PoolTracker) — large mechanical rename; do after 031 docs are in sync
