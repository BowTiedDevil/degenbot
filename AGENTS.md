# AGENTS.md

## Refactoring & Feature Development

Use Red/Green TDD while refactoring and implementing new features.

## Commands

Uses `just` (see justfile) and `uv` as the package runner. Key commands:

### Python
- `just test-python` - Run Python tests
- `just test-rust-python` - Run Rust-wrapped Python tests

### Rust
- `just test-rust` - Run Rust tests
- `just lint-rust` - Run Rust linter (clippy)

**Important**: The Rust extension is automatically rebuilt on import by maturin. Do NOT manually rebuild the extension, recreate the virtual environment, or reinstall the package after making Rust code changes — just run the tests and the import will trigger a rebuild.

### Combined
- `just test-all` - Run all tests (Rust + Python)
- `just lint` - Run lint and type checks (Rust + Python)
- `just format` - Run formatters (Rust + Python)

## Git Commits

All commit messages must follow the project convention enforced by `commitlint`. Run `just setup-git-hooks` once after cloning to enable the local hook and editor template.

```
type(scope): subject

[optional body]

[optional footer]
```

### Types

| Type | Use for |
|------|---------|
| `feat` | New user-facing functionality |
| `fix` | Bug fixes |
| `refactor` | Structural changes with no public API change |
| `docs` | Documentation only |
| `lint` | Auto-formatter or linter-driven changes only |
| `test` | Adding or updating tests |
| `chore` | Repo maintenance (locks, CI, justfile, plan files) |
| `remove` | Pure deletion of dead code, shims, deprecated files |

### Scopes

Optional but encouraged. Closed list: `curve`, `aave`, `v2`, `v3`, `v4`, `aerodrome`, `camelot`, `balancer`, `arbitrage`, `builders`, `database`, `rust`, `python`, `sdk`. Omit scope for cross-cutting changes. Enforced in `.commitlintrc.yml` (the authoritative list — keep both in sync).

### Rules

- **Subject**: imperative mood, no trailing period, ≤72 characters
- **Body**: optional, wrap at 80 characters
- **Footer**: use `Plan: <number>` to reference an architecture plan (repeatable); use `BREAKING CHANGE: <description>` for breaking changes

### Examples

```
feat(curve): strategy objects & lending rate fetchers
fix(v3): correct tick spacing calculation
refactor: separate I/O from calculation in Aave position analysis
remove: delete position_analysis.py shim

Plan: 029
```

Bypass with `--no-verify` if needed, but CI will still reject non-conforming messages on PRs.

## Database
- Config file at `~/.config/degenbot/config.toml`; database path defaults to `~/.config/degenbot/degenbot.db` (overridable via `database.path` setting)
- SQLAlchemy ORM models in `src/degenbot/database/models/`
- Use the scoped session context manager: `with db_session() as session:` from `degenbot.database.db_session`

## Python Design

### Imports
- Add imports at the top level of each module
- Do not import inside classes or functions
- If a circular import is found, fix it immediately

### Patterns
- Prefer frozen `dataclass` for value objects passed between functions
- Prefer `TypedDict` if key and value types are known

### Error Handling
- All exceptions inherit from `DegenbotError` in `src/degenbot/exceptions/base.py`
- Create specific subtypes for distinct categories
- Catch specific exceptions (`except TimeoutError:`), avoid broad catches

#### Exception Messages

Use the `msg` variable pattern to avoid ruff TRY003 warnings:

```python
# Bad: message inline with exception (triggers TRY003)
raise ValueError(f"Unsupported pool type: {pool_type}")

# Good: assign to msg variable first
msg = f"Unsupported pool type: {pool_type}"
raise ValueError(msg)
```

### Logging
- Use `from degenbot.logging import logger`

### Testing
- Add docstring to complex tests describing "what" and "why"
- Create a test double with `Fake` prefix instead of mocking

## Refactoring
- Unless directed otherwise, design standalone features without a backwards compatibility layer.

## Pool Class Architecture

Pool classes use a 3-mixin composition: `class Pool(AbstractLiquidityPool, StateMixin, CalcMixin)`. The MRO is:

```
PoolClass -> PublisherMixin -> PoolPickleMixin -> StateMixin -> CalcMixin -> AbstractLiquidityPool -> AddressComparable -> ABC -> object
```

### State and Calc Mixins

| Pool | State Mixin | Calc Mixin | Notes |
|------|-------------|------------|-------|
| LiquidityPool | `V2PoolState` | `UniswapV2PoolCalc` | Base V2. Uses `StateCache[UniswapV2PoolState]` |
| AerodromeV2Pool | `AerodromeV2PoolState` | `AerodromeV2PoolCalc` | Uses `StateCache[AerodromeV2PoolState]`. `if self._stable` eliminated |
| CamelotLiquidityPool | (inherits V2) | `CamelotPoolCalc` | Inherits V2's `StateCache`. `if self.stable_swap` eliminated |
| UniswapV3Pool | `V3PoolState` | `UniswapV3PoolCalc` | Base V3. Uses `ConcentratedLiquidityStateManager` which composes `StateCache` |
| UniswapV4Pool | `V4PoolState` | `UniswapV4PoolCalc` | Same manager pattern as V3. V4-specific swap calc stays in pool |
| CurveStableswapPool | `StableswapPoolState` | `DyCalculator` seam | Curve uses `BoundedCache` (dict-based) for per-block on-chain data, not `StateCache`. Per-block cache fields are owned by `PerBlockCache` (in `curve/per_block_cache.py`) accessed via `self._cache.get_cached_*()`; mirror-free design — `get_cached_virtual_price()` resolves its own dependencies inline. Calculators in `curve/calculators/`; pure math in `calculations/stableswap.py` |
| BalancerV2Pool | `BalancerV2PoolState` | `WeightedMath` functions | Balancer uses no state cache. Math in `balancer/libraries/`; version-dependent pow via `PowVersion` enum. `external_update()` with `_state_lock`. `to_hop_state()` returns `BalancerWeightedHop` with `swap_fn`. `build_swap_amount()` raises for N>2 without explicit pair — use `BalancerPairView` for `ArbitragePathPool` conformance. Builder: `BalancerBuilder` |
| BalancerV2StablePool | `BalancerV2PoolState` | `StableMath` functions | MetaStable or Composable pool. Two invariant versions (V1/V2). `StaleRateResult` when no rate provider. `BalancerRateProvider` protocol for cache-aware rate resolution. `external_update()` with `_state_lock`. `to_hop_state()` returns `BalancerStableHop` with `swap_fn` that catches `StaleRateResult`. `build_swap_amount()` raises for N>2 without explicit pair — use `BalancerPairView` for `ArbitragePathPool` conformance. Builder: `BalancerBuilder` |

### StateCache

`StateCache[T: CacheableState]` (PEP 695 generic in `src/degenbot/types/state_cache.py`) owns the deque and lock for temporal state navigation. **The caller holds the lock** — all mutation methods (`append`, `discard_before_block`, `restore_before_block`) are unlocked; pools acquire `cache.lock()` for compound operations. V2/Aerodrome use `StateCache` directly; V3/V4 use it via `ConcentratedLiquidityStateManager`. Curve/Balancer are unaffected (different state model).

### Protocols (replacing ABCs)

Three `runtime_checkable` protocols replace removed ABCs:
- `ConstantProductPool` — 6 properties (reserves, fees)
- `ConcentratedLiquidityPool` — 7 properties (sqrt_price, tick, liquidity)
- `StableswapPool` — 1 property (tokens)

Use `hasattr()` structural checks for class-level dispatch (not `issubclass()` — Python's `runtime_checkable` protocols with `@property` raise `TypeError` on `issubclass()`).

### V4 Hook Filtering

V4 pools with amount-modifying hooks are rejected in Python before calling `register_v4_pool()`. The Rust engine is permissive — it trusts the caller. The `AMOUNT_MODIFYING_HOOK_MASK = 0xCC` covers 4 hook flags:

| Hook Flag | Bit | Effect |
|-----------|-----|--------|
| `BEFORE_SWAP` | 1<<7 | Hook can modify swap parameters before execution |
| `AFTER_SWAP` | 1<<6 | Hook can modify swap results after execution |
| `BEFORE_SWAP_RETURNS_DELTA` | 1<<3 | Hook returns custom swap delta |
| `AFTER_SWAP_RETURNS_DELTA` | 1<<2 | Hook returns custom swap delta |

If `(hook_flags & 0xCC) != 0`, the pool is excluded — it violates the solver's assumption that V3 CL math applies exactly. V4 pools with dynamic fees (`fee == 0x100000`) are also excluded. Both checks are performed in Python before `register_v4_pool` is called.

### V4 amountSpecified Sign Convention

V3 and V4 use **opposite** sign conventions for `amountSpecified`:

| Mode | V3 | V4 |
|------|----|----|
| Exact INPUT | `amountSpecified > 0` | `amountSpecified < 0` |
| Exact OUTPUT | `amountSpecified < 0` | `amountSpecified > 0` |

For arbitrage (always exact-input mode): V3 encoding uses **positive** values, V4 encoding uses **negative** values. This is verified in `v3_simulator.py:93` — `exact_input = amount_specified > 0`.

### Calculations Module

Standalone pure-math functions in `src/degenbot/calculations/`: `solidly_stable`, `camelot`, `stableswap`, `concentrated_liquidity`, `evm_math`. No `self`, no class references. V3/V4 libraries remain in `uniswap/v3_libraries/` and `v4_libraries/`. Constant-product swap math lives in `uniswap/v2_functions.py` (`constant_product_calc_exact_in`, `constant_product_calc_exact_out`); the former `calculations/constant_product.py` was deleted — its intermediate truncation in fee deduction diverged from on-chain behavior for small inputs.

### Exception Module Structure

4 domain-aligned files in `src/degenbot/exceptions/`:
- `base.py` — `DegenbotError`, `DegenbotValueError`, `DegenbotTypeError`, `ExternalServiceError`
- `pool.py` — EVM, Curve, Tracker, and LiquidityPool exceptions. `PossibleInaccurateResult` is a parent class with two domain-specific subclasses: `HookedPoolResult` (V4 hooks) and `StaleRateResult` (Balancer stale rates)
- `arbitrage.py` — Solver and swap encoding exceptions
- `infrastructure.py` — Connection, Fetching, Registry, Database, Anvil, ERC20 exceptions

## Ubiquitous Language

Each module has a `CONTEXT.md` defining domain terms, aliases to avoid, and resolved ambiguities. The root [`CONTEXT-MAP.md`](CONTEXT-MAP.md) indexes all modules and holds cross-cutting content (relationships, ambiguity rulings). Read the relevant module context before naming variables, classes, or docstrings in that area.

## Architecture Patterns

### The Bot Session Pattern

All pool and token creation flows through the `Bot` class. `Bot` owns registries (`pools`, `tokens`, `managed_pools`) and connection managers. Pool updates use a **Builder Registry** (`dict[type, PoolBuilder]`) keyed by concrete pool class — no isinstance chains. The `PoolBuilder` and `AsyncPoolBuilder` protocols define `build()` as an instance method and `update()` as a `@staticmethod` — this enforces at the type level that `update()` receives all I/O through its `io` parameter, never through `self`. `_dispatch_build()` forwards a typed `BuildRequest` object (`BuildPoolRequest | BuildManagedPoolRequest`) instead of `**kwargs` dict forwarding.

```python
# Correct: Bot handles I/O and injects data into I/O-free pools
bot = degenbot.Bot.from_config_file()
pool = bot.build_pool("0x...")  # Auto-resolves pool type from DB, registry, or on-chain probing
token = bot.build_erc20token("0x...")  # Fetches metadata, registers in token registry
```

**Don't** instantiate pools directly from classes in new code — that's the **deprecated singleton pattern**.

**AsyncBot** mirrors `Bot`'s delegation pattern: its public I/O methods (`get_token_balance`, `get_token_approval`, `get_token_total_supply`, `get_ether_balance`) delegate to `AsyncErc20Builder` instead of duplicating the logic inline, matching `Bot`'s delegation to `Erc20Builder`.

#### Type Resolution

`build_pool(address)` automatically determines the concrete pool subclass by consulting these sources in order:

1. **Pool Registry** — return the existing pool if already built this session
2. **Database `kind` column** — the polymorphic identity (e.g., `sushiswap_v2`) directly identifies the invariant and variant
3. **Pool Type Registry** — a module-level singleton mapping `(chain_id, factory_address) → pool class + identity + deployment data`; each DEX module self-registers at import time via `pool_type_registry.register()`
4. **On-chain probing** — fallback: call `slot0()`, `getReserves()`, or `coins()` to identify the invariant

The type resolution code in `type_resolution.py` collapses sync/async mirrors into thin wrappers over shared pure functions (`_build_descriptor_from_db_result`, `_descriptor_from_probing_result`), eliminating duplicated logic between sync and async paths.

V4 pools are built via `build_managed_pool(address, pool_id)` — a dedicated method with `BuildManagedPoolRequest` that requires `pool_id`. `build_pool()` no longer accepts `pool_id` or V4-specific kwargs.

Adding a new pool family requires only creating a builder and registering it via `register_builder()`. The typed `build_v2_pool`/`build_v3_pool`/`build_v4_pool`/`build_curve_pool` methods have been removed — `build_pool()` handles all non-V4 pools, `build_managed_pool()` handles V4.

#### BuilderContext

All pool builders accept a `BuilderContext` (frozen dataclass in `src/degenbot/builders/context.py`) instead of 5–6 individual constructor parameters. Bot creates one context object and passes it to all builders. Adding a new builder requires a one-line construction + `register_builder()` — zero additional wiring. `Erc20Builder` is the one exception (it's a leaf dependency used *by* the context, so it keeps its standalone constructor).

#### Builder Base Classes

Sync pool builders for each pool family inherit a base class with shared `@staticmethod` helpers for pure-logic operations (decode, DB extract, snapshot loading). Async builders call the same static methods without inheriting — mirroring the `AsyncV2PoolBuilder` pattern.

- **`V2BuilderBase`** — helpers: `decode_immutable_data`, `extract_db_values`, `resolve_deployer_and_init_hash` (`V2PoolBuilder`, `AerodromeV2Builder`, `CamelotBuilder` inherit)
- **`V3BuilderBase`** — helpers: `decode_immutable_data`, `decode_slot0`, `extract_db_values`, `load_tick_snapshot`, `resolve_tick_data_args`; frozen dataclasses `V3ImmutableData`, `V3Slot0Data`, `V3DbValues` (`V3PoolBuilder` inherits; `AsyncV3PoolBuilder` calls static methods)
- **`V4BuilderBase`** — helpers: `decode_slot0`, `extract_db_values`, `load_tick_snapshot`, `resolve_tick_data_args`; frozen dataclasses `V4Slot0Data`, `V4DbValues` (`V4PoolBuilder` inherits; `AsyncV4PoolBuilder` calls static methods)
- **`BalancerBuilderBase`** — helpers: `decode_pool_id`, `decode_vault_tokens`, `detect_bpt_index`, `resolve_invariant_version`, `_fetch_pool_id`, `_fetch_vault_tokens`, `_fetch_swap_fee`, `_fetch_weights`, `_fetch_amp`, `_fetch_rate_providers`, `_fetch_rates`, `_detect_pool_type`; frozen dataclasses `DecodedPoolId`, `VaultTokensResult`, `_BalancerPoolType` enum (`BalancerBuilder` inherits; future `AsyncBalancerBuilder` calls static methods)

### Polars-Inspired Three-Layer Architecture

The complement to the Bot Session Pattern above: how the Python `Bot` session reaches Rust-owned state across the FFI. For stateful Rust resources the three layers gain a sharing topology — a `#[pyclass]` wrapper (`PyBot`) holding `Arc<parking_lot::RwLock<Core>>` *is* the sharing mechanism; thin handles (`PyLiquidityPool` carrying a `pool_id` key, `PyErc20Token` carrying an `Address` key) clone that `Arc` so N Python objects reference one Rust-owned `Bot`; the Python `Bot` constructs `self._py_bot = PyBot()` and delegates Rust-owned state through it. Read methods take a read guard, mutations a write guard. Canonicalized in **ADR-005** (decision, rejected alternatives, deferred `UniswapEngine` unification); the stateful specialization of `rust/AGENTS.md`'s generic three-layer convention; complements **ADR-003** (which answers *who owns state*, not how Python reaches it).

### Fetcher Protocols

**Curve pools** use a **CurveDataProvider** seam for fully I/O-free operation — all on-chain data access flows through a single injected object with 13 methods (`D()`, `gamma()`, `virtual_price()`, `base_virtual_price()`, `price_scale()`, `admin_balances()`, `lending_rates()`, `redemption_price()`, `block_timestamp()`, `block_number()`, `token_balance()`, `token_total_supply()`, `is_crypto()`). The pool calls `self._data_provider.xxx()` on-demand:

```python
# Builder creates a CurveDataProviderImpl with a ProviderAdapter directly
# Pool calls data_provider methods on-demand
pool = bot.build_pool("0xbEbc44782C7db0a1A60Cb6fe97d0b483032FF1C7")
```

The former individual fetcher callback constructor parameters (`_D_fetcher`, `_gamma_fetcher`, `_virtual_price_fetcher`, etc.) were collapsed into a single `data_provider: CurveDataProvider | None` parameter. The closure-based `CurveFetcherFactory` has been replaced by `CurveDataProviderImpl` — a structured class in `data_provider_impl.py` with real methods and shared I/O helpers (`_call`, `_call_single`, `_call_raw_single`, `_wrap_revert`). Builders construct `CurveDataProviderImpl` directly with a `ProviderAdapter`. Tests use `FakeCurveDataProvider` instead of individual lambda fetchers.

The `DyCalculator` seam replaces `match`/`if` dispatch branches in `get_dy()` with injectable calculator objects keyed on `SwapStyle`, `MetapoolRateStyle`, and `MetapoolUnderlyingStyle` enums. Pure math functions in `calculations/stableswap.py` raise `ValueError`; calculators call these directly and wrap with `EVMRevertError`. The `DyCalculator` protocol and `PoolStrategies` dataclass live in `strategies.py` to break the circular dependency between `types.py` (enums, `DyCalculationInputs`) and the calculator modules. Factory functions (`make_swap_style_calculator`, `make_metapool_calculator`, `make_metapool_underlying_calculator`) construct calculators from enum values at top-level import.

**DyCalculationInputs** replaces the `pool: CurveStableswapPool` parameter in `DyCalculator.calculate()` with a frozen dataclass carrying pre-resolved data (including `d_variant`/`y_variant`/`yd_variant`/`a_precision` for variant-aware invariant solving). The pool's `get_dy()` performs all I/O (rate resolution, cache lookups, block data) before constructing a `DyCalculationInputs` and passing it to the calculator. Calculators are pure consumers of this object — no private member access, no I/O, no cache mutation, no closures. `DyCalculationInputs` is a pure value object (all fields are ints, tuples, enums, or None — zero callables); calculators call `stableswap_get_y()` / `stableswap_newton_y()` directly.

**V2/V3/V4/Aerodrome/Camelot pools** are fully I/O-free — builders fetch all data from DB/RPC, pass it to the pool constructor, and no provider references remain on the pool object. All updates flow through `external_update()` (pure logic). No pool class imports `ProviderAdapter` or carries provider-dependent methods.

See `docs/architecture/io-free-pools.md` and `src/degenbot/curve/CONTEXT.md` for details.

### Enum Naming: PoolFamily vs PoolInvariant

Two enums cover related but distinct concepts:
- **`PoolFamily`** (in `types/pool_type.py`) — identifies a pool's mathematical invariant family for type resolution and DB kind derivation. Values: `CONSTANT_PRODUCT`, `CONCENTRATED_LIQUIDITY`, `STABLESWAP`, `WEIGHTED`.
- **`PoolInvariant`** (in `types/hop_types.py`) — identifies the solver dispatch path for arbitrage optimization. Values: `CONSTANT_PRODUCT`, `BOUNDED_PRODUCT`, `SOLIDLY_STABLE`, `CURVE_STABLESWAP`, `BALANCER_WEIGHTED`, `BALANCER_MULTI_TOKEN`, `BALANCER_STABLESWAP`.

A `PoolFamily` maps 1:1 to `PoolInvariant` for V2/V3, but N:1 for Curve/Stable and Balancer/Weighted (e.g., `STABLESWAP` → `CURVE_STABLESWAP` or `SOLIDLY_STABLE` or `BALANCER_STABLESWAP`).

### CacheablePool Protocol

Pools that register with the Rust solver cache implement the `CacheablePool` protocol, providing `reserves_for_cache()` and `fee_for_cache()` methods. This replaces the previous `getattr`-based introspection in the adapter.

### Swap Encoding Pipeline

Each `SwapAmounts` subclass (V2, V3, Curve, V4, Balancer) has an `encode(recipient=)` method that produces an `EncodedCall(to, data, value)`, plus `input_amount()` / `output_amount()` for generic amount extraction (replacing the former match/case dispatch). Pool classes implement `build_swap_amount()` from the `ArbitragePathPool` protocol, making the per-pool swap-amount construction fully local. `BalancerV2SwapAmounts` encodes a Vault.swap() call with SingleSwap and FundManagement structs. The `generate_payloads()` function wires a three-layer pipeline:

1. **Per-hop encoding** — `SwapAmounts.encode()` (pool-type-specific ABI encoding)
2. **Approval injection** — `ApprovalStrategy` protocol (default: `NoApprovals`)
3. **Call composition** — `PayloadComposer` protocol (default: `FlatComposer`)

Library callers extend this by implementing custom `ApprovalStrategy` and `PayloadComposer` for their specific smart contracts. V4 encoding requires a custom `PayloadComposer` since V4 uses an unlock/swap callback pattern. The `V4PoolKey` dataclass is available on `UniswapV4PoolSwapAmounts.pool_key` for V4 dispatch. See `src/degenbot/arbitrage/CONTEXT.md` for full terminology.

### Command Stream Encoding (cmd_executor)

The `encode_cmd_stream()` function in `eth_backrun_helpers.py` builds the `execute(bytes)` calldata for the cmd_executor contract. It dispatches to type-specific encoders based on the hop composition:

| Path type | Encoder | Approach |
|-----------|---------|----------|
| All V2 (N≥2) | `_encode_cmd_v2_n_hop` | V2_SWAP_COMPACT flash + chained V2_SWAP_CALC |
| V2+V3 | `_encode_cmd_v2_v3` / `_encode_cmd_v3_v2` | Hybrid callback + auto-pay |
| V2+V4 | `_encode_cmd_v2_v4` / `_encode_cmd_v4_v2` | V4 unlock wrapping V2 callback |
| V3+V3 | `_encode_cmd_v3_v3` | Nested V3 callbacks with auto-pay |
| V3+V4 | `_encode_cmd_v3_v4` / `_encode_cmd_v4_v3` | V4 unlock + V3 callback |
| V4+V4 | `_encode_cmd_v4_v4` | V4 batch with dynamic amounts |
| 3-hop mixed | `_encode_cmd_3_hop` | Dispatches to 27 permutations |

**Address table index hygiene**: The `AddressTable` deduplicates by checksummed address. In list comprehensions that build index lists (e.g., `pool_indices`), the iteration variable **must** match the attribute accessor. A mismatch (e.g., `for h in hops` but `hop.pool_address`) silently references an outer-scope variable from a prior loop's final iteration — all addresses resolve to the same deduplicated index. This produces a command stream that calls the wrong pool for every hop, reverting with `V2_SWAP_CALC: no excess balance` or similar errors. Symptom: 100% simulation failure for a path type with zero sub-calls in `debug_traceCall`.

### Tstore Executor V4 Architecture

The `tstore_executor.vy` contract handles V2/V3/V4 arbitrage via a hybrid approach:

- **V4 swaps** execute directly via `extcall` in `unlockCallback` to capture `BalanceDelta` return values
- **V2/V3 operations** use the generic payload queue with `raw_call` delivery
- **Settlement** is automatic — all nonzero V4 currency deltas are settled after swaps and payloads

The `unlockCallback` has 4 phases:

| Phase | Purpose | Key Logic |
|-------|---------|-----------|
| **Phase 0** | Pre-settle | For V3→V4/V2→V4: calls `settle()` to credit forward ERC-20 to `t_v4_deltas` before V4 swap consumes them. Skips duplicate settlements when the same input currency appears across multiple V4 swaps |
| **Phase 1** | V4 swaps | Executes V4 swaps, tallies ALL currency deltas in `t_v4_deltas` (not just ETH/WETH). Handles `dynamic_amount` flag for V4-V4 paths |
| **Phase 2** | Queued payloads | Delivers remaining queued payloads (take/transfer for V4→V3/V4→V2). Zeros intermediate ERC-20 deltas if payloads were delivered |
| **Phase 3** | Auto-settle | Settles all nonzero deltas: native ETH, WETH, and intermediate ERC-20s. Uses `_v4_settle_currency` internal helper, which zeros the delta after settling to prevent double-settlement when the same ERC-20 appears in multiple pool keys |

**V2 callback difference**: Unlike V3 (which auto-pays WETH), the V2 `uniswapV2Call`/`hook`/`pancakeCall` callbacks directly resume payload delivery via `_deliver_remaining_payloads()` — no intermediate wrapper. V2 pair WETH payment must be an explicit payload in the queue. This is verified by the V4→V2 and V2→V4 contract tests.

**V3 auto-pay pattern**: The V3 callback computes `owed_token`/`owed_amount` first (determining which token the pool is owed and how much), then performs a single WETH transfer only if `owed_token == WETH_ADDR`. This unified pattern replaced the previous per-branch `staticcall`+`extcall` pair, ensuring auto-pay never fires for non-WETH debts. Encoders must NOT include separate WETH transfer payloads for auto-paid V3 pools.

**`NATIVE_ADDRESS` constant**: `constant(address) = empty(address)` — used throughout the contract to identify native ETH, replacing inline `native: address = empty(address)` locals. Named constant is clearer and avoids stack variable overhead.

**`_decode_swap_delta` unified helper**: Replaces the former `_decode_swap_delta_amount0`/`_decode_swap_delta_amount1` pair with a single parameterized function `_decode_swap_delta(swap_delta, byte_offset)`, eliminating code duplication.

**int128 overflow guard**: V4's `BalanceDelta` uses `int128` per component. The `fits_int128()` function in `degenbot.arbitrage.encoding` allows encoders to skip paths where amounts would overflow (`SafeCastOverflow` revert). All 5 V4 encoder functions check this before constructing swap params.

**V4→V2 amount_out encoding**: V2's `swap(amount0Out, amount1Out, ...)` specifies what V2 SENDS to the recipient. For USDC→WETH@V2, `amount_out` must be `weth_out` (the WETH output), NOT `forward_out` (the USDC input). Previous code passed `forward_out` — caused `INSUFFICIENT_LIQUIDITY` on mainnet.

## Agent skills

### Issue tracker

Local markdown files under `.scratch/<feature-slug>/`. See `docs/agents/issue-tracker.md`.

### Triage labels

Default vocabulary: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`. See `docs/agents/triage-labels.md`.

### Domain docs

Multi-context — `CONTEXT-MAP.md` at root pointing to per-module `CONTEXT.md` files. See `docs/agents/domain.md`.

## Architecture Plans

Refactoring plans live in `plans/`. Completed plans are in `plans/completed/`. Each plan tracks its own status via the checklist at the bottom of its file.

**New plans must follow [`plans/TEMPLATE.md`](plans/TEMPLATE.md).** The template requires: deletion test, specific friction table, vertical slices, design decisions, relationship to other plans, and status checklist.

When a plan is marked complete: (1) move its file from `plans/` to `plans/completed/` and (2) update the link in any referencing documents to point to the new `completed/` path.

### Legacy Arbitrage Cycles (Deprecated)

The legacy cycle classes (`UniswapLpCycle`, `UniswapCurveCycle`, etc.) have been moved to `src/degenbot/arbitrage/_legacy/` with underscore-prefixed names and `DeprecationWarning` on import. They are replaced by `ArbitragePath` + `ArbSolver`. `AbstractArbitrage` and `get_arbitrage_helpers()` have been deleted (dead code — `ArbitragePath` never inherited `AbstractArbitrage`). The `cvxpy` dependency is now optional (`pip install degenbot[legacy-cycles]`). See `docs/migration-guides/legacy-cycles-to-arbitrage-path.md` for the migration guide.

### Functions Module Decomposition

`src/degenbot/functions.py` has been split into domain-aligned modules:

| New Module | Contents |
|---|---|
| `provider/call_helpers.py` | `raw_call`, `async_raw_call`, `encode_function_calldata`, `extract_argument_types_from_function_prototype` |
| `provider/log_fetching.py` | `fetch_logs_retrying`, `fetch_logs_retrying_async` |
| `contract/addresses.py` | `create2_address`, `eip_1167_clone_address` |
| `contract/decoding.py` | `decode_address` |
| `calculations/evm_math.py` | `evm_divide`, `next_base_fee`, `raise_if_invalid_uint256` |
| `provider/block_helpers.py` | `get_number_for_block_identifier`, `get_number_for_block_identifier_async` |

`eip_191_hash` was deleted (dead code, zero imports). The original `functions.py` no longer exists — all callers have been migrated.

### Provider Interface Split

`src/degenbot/provider/interface.py` has been split into three focused modules:

| New Module | Contents |
|---|---|
| `provider/protocols.py` | `ProviderBackend`, `AsyncProviderBackend` — pure protocol seams |
| `provider/sync_adapter.py` | `SyncSubscriptionSupport`, `_Web3Adapter`, `_AlloyAdapter`, `_OfflineAdapter`, `ProviderAdapter`, `_backend_for_type` |
| `provider/async_adapter.py` | `AsyncSubscriptionSupport`, `_AsyncWeb3Adapter`, `_AsyncAlloyAdapter`, `AsyncProviderAdapter` |

The original `interface.py` no longer exists — all callers have been updated to import from the new modules. `provider/__init__.py` re-exports the four public names for convenience.

### Pool-to-Hop Conversion

`solver_hop_builders.py` has been deleted — each pool's `to_hop_state()` method is the single source of truth for pool→hop conversion. The `PoolCompatibility` enum has been removed. The thin free functions `_pool_to_hop_state`, `_extract_fee`, and `_check_pool_compatibility` have been inlined in `arbitrage_path.py`.

## Solidity

### Contract Testing

An isolated Ape + Foundry test suite under `contracts/tests/` verifies the executor's V4 settlement logic using fake contracts (no mainnet fork). 27 tests across 8 files covering V2/V3-only paths, V4-hybrid paths, callback variant selectors, settlement branches, three-hop paths, and encoding regressions.

Run from `contracts/tests/`:
```bash
uv run --with eth-ape --with ape-vyper --with ape-foundry ape test -v -n0
```

Always use `-n0` — Foundry's local EVM is single-process; parallel workers cause flakes. See `contracts/tests/README.md` for fake contract docs and test patterns.

**Key patterns**:
- V2 callback has NO auto-pay (unlike V3) — WETH transfer to V2 pair must be explicit payload
- V4→V2/V3 forward tokens: `take()` from PM inside `unlockCallback` Phase 2, then transfer to V2/V3
- V2/V3→V4: sync BEFORE transfer to PM (records zero balance), then Phase 0 settle() credits the delta before V4 swap
- V4→V2 `amount_out` must be WETH output (`weth_out`), NOT forward token input (`forward_out`) — the V2 `swap()` specifies what V2 SENDS

### int128 Overflow Guard

V4's `BalanceDelta` packs two int128 values. If `amountSpecified` exceeds ±2^127, V4 reverts with `SafeCastOverflow`. All V4 encoder functions guard against this:

```python
from degenbot.arbitrage.encoding import fits_int128

if not fits_int128(-optimal_input):
    return None  # Skip — would overflow
```

### V3 amountSpecified Sign Convention

V3 and V4 use **opposite** sign conventions for `amountSpecified`:

| | exact INPUT | exact OUTPUT |
|---|---|---|
| **V3** | `amountSpecified > 0` | `amountSpecified < 0` |
| **V4** | `amountSpecified < 0` | `amountSpecified > 0` |

This is verified in `v3_simulator.py:93` — `exact_input = amount_specified > 0`. When building V3 swap calldata for arbitrage (always exact-input mode), use **positive** values.

### Contract Reference Sources

Verified Solidity sources are in `contract_reference/` (see [`contract_reference/README.md`](contract_reference/README.md) for the full index). When porting on-chain logic to Python, include a `See: contract_reference/...` comment pointing to the exact source file and line range.

| Protocol | Path |
|----------|------|
| Uniswap V2 | `contract_reference/uniswap/V2/UniswapV2Factory.sol` |
| Uniswap V3 | `contract_reference/uniswap/V3/UniswapV3Factory.sol` |
| Uniswap V4 | `contract_reference/uniswap/V4/PoolManager.sol` |
| Aave V3 | `contract_reference/aave/` (revision-based: `Pool/rev_N.sol`, `AToken/rev_N.sol`, etc.) |

### Arithmetic wrapping behavior
- Solidity arithmetic silently wraps for versions < 0.8.0, e.g. `uint8(255) + 1 == 0`
- Solidity arithmetic is checked by default for versions 0.8.0+, e.g. `uint8(255) + 1` will revert

### Porting to Python
- Solidity contracts requires explicit integer division to match the EVM. The Python `//` operator is equivalent to the Solidity `/` operator.
- **Balancer V2**: When Solidity's `/` truncates toward zero for negative operands, Python's `//` floors toward -∞. The `_truncated_div` helper in `log_exp_math.py` matches Solidity's truncation-toward-zero semantics for Taylor series divisions. Deployed pool contracts use different `FixedPoint.pow` implementations depending on contract version — `PowVersion` (V1/V2) controls whether fast paths for y == ONE/TWO/FOUR are active. Fee ordering in GIVEN_OUT must match Solidity: downscale first, then add fee.
- **Balancer V2 StableMath**: Deployed contracts use two different invariant versions. V1 (`INVARIANT_V1`, always-roundDown with D_P accumulation) matches the monorepo `_calculate_invariant` — used by most ComposableStablePools. V2 (`INVARIANT_V2`, with `roundUp` param and P_D accumulation) matches `_calculate_invariant_deployed` — used by MetaStablePools. Using V2 when V1 is needed produces a systematic 1-wei output error. ComposableStablePools with time-varying rates need a `CacheAwareRateProvider` that replicates `_cacheTokenRateIfNecessary` exactly (read `getTokenRateCache()`, check expiry, call `getRate()` only if expired). Without a rate provider, `StaleRateResult` is raised.

## Rust Extension

The Rust extension (`rust/`) provides PyO3-wrapped ABI encoding/decoding, subscription handling, and Möbius solver integration. See `rust/CONTEXT.md` for full domain terminology.

### GIL Discipline

- **Hold the GIL** for sub-μs pure-compute functions (tick math, address utils). GIL release/reacquire overhead (~200ns) exceeds compute time (~20ns), so `py.detach()` is a net slowdown.
- **Release the GIL** for I/O-bound operations (`py.detach()` before `block_on()` in async provider and subscription code). All `Python::attach()` call sites have `// SAFETY` comments documenting the no-circular-wait contract.
- **Free-threaded Python 3.14+**: `RefCell` is unsafe under free-threaded builds; use `Mutex` for interior mutability.

### ABI Type Cache

`CachedAbiTypes` uses two-level interning to eliminate heap allocations on cache hits:
1. **String interner** (`TYPE_STR_INTERNER`) deduplicates the ~20 Solidity type strings → `Arc<str>`
2. **Value `Arc<CachedAbiTypes>`** — cache returns `Arc::clone` (O(1)) instead of deep-cloning the `DynSolType` tree
3. **Key type** — `TypeCacheKey = Arc<[Arc<str>]>` for cheap comparison and `Borrow` compatibility

`FunctionSignature` stores `Option<Arc<CachedAbiTypes>>` to avoid deep clones when cloning signatures.

### Optimizer Cache

`PyPoolCache` uses `parking_lot::Mutex<LruCache<u64, IntHopState>>` (10K capacity) instead of `HashMap` to bound memory in long-running processes. `IntHopState` pre-converts U256 fields to U512 at construction time, eliminating per-swap conversions.

### Subscription Buffer

`SubscriptionHandle` uses a double-buffer pattern with `drain_raw()` (pure Rust, no GIL) for GIL-free accumulation. The Python-facing `drain_buffer()` wraps it with `Python::attach()`.

### V3 Block Engine: Mint/Burn Event Handling

`V3BlockEngine.process_block()` decodes Swap, Mint, and Burn events from raw Alloy logs. The `update_tick_liquidity()` helper (shared in `rust/src/bot_core/tick_bitmap.rs`) matches Solidity's `Tick.update()`: both lower and upper tick receive `liquidity_gross += delta`, while `liquidity_net += delta` for the lower tick and `liquidity_net -= delta` for the upper tick (controlled by `is_lower_tick` parameter). Ticks with zero `liquidity_gross` after the update are removed (de-initialized). Mint/Burn decoders live in `rust/src/bot_core/v3_mint_burn_decoder.rs`. This replaces the previous O(n) full-tick-data dump from Python on every event (Plan 080 F4, completed).

### V4 Block Engine: ModifyLiquidity Event Handling

`V4BlockEngine.process_block()` decodes Swap and ModifyLiquidity events from raw Alloy logs. V4's `ModifyLiquidity(bytes32,address,int24,int24,int256,bytes32)` replaces V3's separate Mint and Burn — a signed `liquidityDelta` handles both directions. Both V3 and V4 engines delegate tick mutations to the shared `update_tick_liquidity()` and `apply_liquidity_to_tick_range()` helpers in `rust/src/bot_core/tick_bitmap.rs`. Decoder lives in `rust/src/bot_core/v4_modify_liquidity_decoder.rs`. V4 pools are keyed by `(pool_manager, pool_id)` instead of contract address.

### Rust Engine Pumps

Three individual pumps and one unified pump drive the Rust engines:
- **`V2EnginePump`** — fetches Sync logs for registered V2 pool addresses (standalone, per-block `eth_getLogs`)
- **`V3EnginePump`** — fetches Swap/Mint/Burn logs for registered V3 pool addresses (standalone, per-block `eth_getLogs`)
- **`V4EnginePump`** — fetches Swap/ModifyLiquidity logs for PoolManager addresses (standalone, per-block `eth_getLogs`)
- **`UniswapEnginePump`** — dual WS subscription (`newHeads` + unfiltered `logs`), processes each log eagerly via `apply_log` (immediate state update) with solves coalesced at the top of the pump loop. Rust-side filtering by topic + registered address. Block boundaries detected by `log.block_number > current_block` (primary) or `header.number > current_block` (empty-block fallback). Headers provide metadata (timestamp, fees) and trigger empty-block batches. 60s timeout with no activity → `eth_getLogs` for the missing range. Solver results flow to Python via an **unbounded `mpsc` channel** as incremental `ResultBatch` diffs (`fresh`/`updated`/`expired`/`removed` lists) — every diff is delivered, no silent drops. Spawned by `PyUniswapArbEngine.start(rpc_url)`. Individual V2/V3/V4 pumps are kept as alternative entry points for testing or single-protocol deployments.

All pumps follow the same pattern: WS `newHeads` subscription → log processing → `engine.process_block()`. No Python dependency. The bot's `main()` calls `engine.start(rpc_url)` after `freeze()` + `initial_solve()`; Python subscribes to `newHeads` only for fee/nonce updates and result dispatch.

### Snapshot Backfill

Before the Rust pump takes over state updates, the bot must bridge the gap between the last DB snapshot and the current block. This is handled by `backfill_from_snapshot()`, a PyO3 method on `PyUniswapArbEngine` called after `subscribe()` and before `resume()`.

**Startup sequence**: `subscribe(rpc_url)` → `fetch_snapshot_events()` → `backfill_from_snapshot(rpc_url, snapshot_block)` → `resume()` → `build_paths()`

`subscribe()` opens WS subscriptions (newHeads + unfiltered logs) and waits for the first *complete* block — one where both a header and at least one log for the same block number have been received. This guarantees the logs subscription didn't miss the start of the block (which could happen if the subscription was opened mid-block). The block number W is returned as the backfill boundary. **No events are buffered during subscribe** — the backfill is the sole authority for blocks S+1..W-1, and the pump (resume phase) is the sole authority for W onward. This eliminates overlap between the two sources.

`backfill_from_snapshot()` creates an HTTP provider, fetches Swap/Mint/Burn/ModifyLiquidity events in paginated chunks from `snapshot_block + 1` to `first_ws_block - 1` via `eth_getLogs`, and applies them to the V3/V4 engines via `process_backfill_logs()`. The `-1` avoids overlap with WS events that the pump will process from block W onward. For unregistered pools (all of them at this point), Mint/Burn/ModifyLiquidity events are buffered in `backfill_event_buffer` (never expired). WS pump events for unregistered pools are buffered separately in `pump_event_buffer` (expired normally). During `register_pool()`, buffers are applied in two staged steps — `apply_backfill_buffer()` then `apply_pump_buffer()` — so the caller can snapshot state at deterministic points for race-free verification.

When `register_pool()` is called later (during `build_paths`), it creates the pool entry with DB snapshot tick_data. The caller (`uniswap_engine.rs`) then sequences `apply_backfill_buffer()` and `apply_pump_buffer()` under the engine lock — bringing the engine state current without Python-side event fetching. If `verify_on_register` is enabled, deterministic state snapshots are captured at two points: (1) raw tick_data before any buffer (verified against on-chain at snapshot block), and (2) post-backfill state after `apply_backfill_buffer()` (verified against on-chain at backfill block). Verification RPC calls happen outside the lock. On mismatch, `RuntimeError` causes immediate shutdown.

The Python `fetch_snapshot_events()` function loads snapshots from the DB only (no `fetch_new_events()` calls). The Rust engine handles all event backfill autonomously.

After backfill and registration, the Rust pump owns all state updates. Python's role in the hot loop is reduced to reading results and submitting transactions.