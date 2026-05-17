# AGENTS.md

## Refactoring & Feature Development

Use Red/Green TDD while refactoring and implementing new features.

## Commands

Uses `just` (see justfile) and `uv` as the package runner. Key commands:

### Python
- `just test-python` - Run Python tests (includes `compile-test-contracts` via Forge)
- `just test-python-cov` - Run Python tests with coverage
- `just test-rust-python` - Run Rust-wrapped Python tests

### Rust
- `just test-rust` - Run Rust tests
- `just lint-rust` - Run Rust linter (clippy)

**Important**: The Rust extension is automatically rebuilt on import by maturin. Do NOT manually rebuild the extension, recreate the virtual environment, or reinstall the package after making Rust code changes — just run the tests and the import will trigger a rebuild.

### Combined
- `just test-all` - Run all tests (Rust + Python)
- `just lint` - Run clippy, ruff, and mypy
- `just format` - Run `cargo fmt` and `ruff format`

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

Optional but encouraged. Closed list: `curve`, `aave`, `v2`, `v3`, `v4`, `aerodrome`, `camelot`, `balancer`, `arbitrage`, `database`, `rust`, `sdk`. Omit scope for cross-cutting changes.

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

### Docstrings
Minimal PEP 257. Type hints supersede parameter docs. No reST/Sphinx tags:
```python
class SomeClass:
    """
    Class description.
    
    [additional detail as needed]
    """
```

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
| UniswapV2Pool | `V2PoolState` | `UniswapV2PoolCalc` | Base V2 |
| AerodromeV2Pool | `AerodromeV2PoolState` | `AerodromeV2PoolCalc` | `if self._stable` eliminated |
| CamelotLiquidityPool | (inherits V2) | `CamelotPoolCalc` | `if self.stable_swap` eliminated |
| UniswapV3Pool | `V3PoolState` | `UniswapV3PoolCalc` | Base V3 |
| UniswapV4Pool | `V4PoolState` | `UniswapV4PoolCalc` | V4-specific swap calc stays in pool |
| CurveStableswapPool | `StableswapPoolState` | `DyCalculator` seam | Calculators in `curve/calculators/`; pure math in `calculations/stableswap.py` |

### Protocols (replacing ABCs)

Three `runtime_checkable` protocols replace removed ABCs:
- `ConstantProductPool` — 6 properties (reserves, fees)
- `ConcentratedLiquidityPool` — 7 properties (sqrt_price, tick, liquidity)
- `StableswapPool` — 1 property (tokens)

Use `hasattr()` structural checks for class-level dispatch (not `issubclass()` — Python's `runtime_checkable` protocols with `@property` raise `TypeError` on `issubclass()`).

### Calculations Module

Standalone pure-math functions in `src/degenbot/calculations/`: `constant_product`, `solidly_stable`, `camelot`, `stableswap`, `concentrated_liquidity`. No `self`, no class references. V3/V4 libraries remain in `uniswap/v3_libraries/` and `v4_libraries/`.

### Exception Module Structure

4 domain-aligned files in `src/degenbot/exceptions/`:
- `base.py` — `DegenbotError`, `DegenbotValueError`, `DegenbotTypeError`, `ExternalServiceError`
- `pool.py` — EVM, Curve, Tracker, and LiquidityPool exceptions
- `arbitrage.py` — Solver and swap encoding exceptions
- `infrastructure.py` — Connection, Fetching, Registry, Database, Anvil, ERC20 exceptions

## Ubiquitous Language

Each module has a `CONTEXT.md` defining domain terms, aliases to avoid, and resolved ambiguities. The root [`CONTEXT-MAP.md`](CONTEXT-MAP.md) indexes all modules and holds cross-cutting content (relationships, ambiguity rulings). Read the relevant module context before naming variables, classes, or docstrings in that area.

## Architecture Patterns

### The Bot Session Pattern

All pool and token creation flows through the `Bot` class. `Bot` owns registries (`pools`, `tokens`, `managed_pools`) and connection managers. Pool updates use a **Builder Registry** (`dict[type, PoolBuilder]`) keyed by concrete pool class — no isinstance chains. The `PoolBuilder` protocol (`src/degenbot/builders/protocol.py`) replaces the former 4-way union type, and `_dispatch_build()` forwards all kwargs uniformly instead of branching on builder type.

```python
# Correct: Bot handles I/O and injects data into I/O-free pools
bot = degenbot.Bot.from_config_file()
pool = bot.build_pool("0x...")  # Auto-resolves pool type from DB, registry, or on-chain probing
token = bot.build_erc20token("0x...")  # Fetches metadata, registers in token registry
```

**Don't** instantiate pools directly from classes in new code — that's the **deprecated singleton pattern**.

#### Type Resolution

`build_pool(address)` automatically determines the concrete pool subclass by consulting these sources in order:

1. **Pool Registry** — return the existing pool if already built this session
2. **Database `kind` column** — the polymorphic identity (e.g., `sushiswap_v2`) directly identifies the invariant and variant
3. **Pool Type Registry** — a module-level singleton mapping `(chain_id, factory_address) → pool class + identity + deployment data`; each DEX module self-registers at import time via `pool_type_registry.register()`
4. **On-chain probing** — fallback: call `slot0()`, `getReserves()`, or `coins()` to identify the invariant

V4 pools are identified by passing `pool_id` to `build_pool(address, pool_id=...)`.

Typed builders (`build_v2_pool`, `build_v3_pool`, `build_v4_pool`, `build_curve_pool`) emit `DeprecationWarning` (Plan 044) — use `build_pool()` instead. Adding a new pool family now requires only creating a builder and registering it via `register_builder()`, down from 5 touch points.

### Fetcher Protocols

**Curve pools** use a **CurveDataProvider** seam for fully I/O-free operation — all on-chain data access flows through a single injected object with 13 methods (`D()`, `gamma()`, `virtual_price()`, `base_virtual_price()`, `price_scale()`, `admin_balances()`, `lending_rate()`, `redemption_price()`, `block_timestamp()`, `block_number()`, `token_balance()`, `token_total_supply()`, `is_crypto()`). The pool calls `self._data_provider.xxx()` on-demand:

```python
# Bot creates a _CurveDataProviderImpl via the Curve Pool Builder's fetcher factory
# Pool calls data_provider methods on-demand
pool = bot.build_pool("0xbEbc44782C7db0a1A60Cb6fe97d0b483032FF1C7")
```

The former 13 individual fetcher callback constructor parameters (`_D_fetcher`, `_gamma_fetcher`, `_virtual_price_fetcher`, etc.) have been collapsed into a single `data_provider: CurveDataProvider | None` parameter (Plan 040). Pickle config simplified from 13 drops+reconstructs to 1. Builders call `fetchers.create_provider()` instead of 13 individual fetcher closures. Tests use `FakeCurveDataProvider` instead of individual lambda fetchers.

The `DyCalculator` seam (Plan 039) replaces 14 `match`/`if` dispatch branches in `get_dy()` with injectable calculator objects keyed on `SwapStyle`, `MetapoolRateStyle`, and `MetapoolUnderlyingStyle` enums. Pure math functions in `calculations/stableswap.py` raise `ValueError`; pool wrappers catch and re-raise as `EVMRevertError` for backward compat.

**DyCalculationInputs** (Plan 045) replaces the `pool: CurveStableswapPool` parameter in `DyCalculator.calculate()` with a frozen dataclass carrying pre-resolved data. The pool's `get_dy()` performs all I/O (rate resolution, cache lookups, block data, invariant solver closure construction) before constructing a `DyCalculationInputs` and passing it to the calculator. Calculators are pure consumers of this object — no private member access, no I/O, no cache mutation. This eliminated 77 SLF001 (private member access) errors across the calculator modules.

**V2/V3/V4/Aerodrome/Camelot pools** are fully I/O-free — builders fetch all data from DB/RPC, pass it to the pool constructor, and no provider references remain on the pool object. All updates flow through `external_update()` (pure logic). No pool class imports `ProviderAdapter` or carries provider-dependent methods (ADR-001 Phase 3 complete).

See `docs/architecture/io-free-pools.md` and `src/degenbot/curve/CONTEXT.md` for details.

### Enum Naming: PoolFamily vs PoolInvariant

Two enums cover related but distinct concepts:
- **`PoolFamily`** (in `types/pool_type.py`) — identifies a pool's mathematical invariant family for type resolution and DB kind derivation. Values: `CONSTANT_PRODUCT`, `CONCENTRATED_LIQUIDITY`, `STABLESWAP`, `WEIGHTED`.
- **`PoolInvariant`** (in `types/hop_types.py`) — identifies the solver dispatch path for arbitrage optimization. Values: `CONSTANT_PRODUCT`, `BOUNDED_PRODUCT`, `SOLIDLY_STABLE`, `CURVE_STABLESWAP`, `BALANCER_WEIGHTED`, `BALANCER_MULTI_TOKEN`.

A `PoolFamily` maps 1:1 to `PoolInvariant` for V2/V3, but N:1 for Curve/Stable and Balancer/Weighted (e.g., `STABLESWAP` → `CURVE_STABLESWAP` or `SOLIDLY_STABLE`).

### CacheablePool Protocol

Pools that register with the Rust solver cache implement the `CacheablePool` protocol, providing `reserves_for_cache()` and `fee_for_cache()` methods. This replaces the previous `getattr`-based introspection in the adapter.

### Swap Encoding Pipeline

Each `SwapAmounts` subclass (V2, V3, Curve, V4) has an `encode(recipient=)` method that produces an `EncodedCall(to, data, value)`, plus `input_amount()` / `output_amount()` for generic amount extraction (replacing the former match/case dispatch). Pool classes implement `build_swap_amount()` from the `ArbitragePathPool` protocol, making the per-pool swap-amount construction fully local. The `generate_payloads()` function wires a three-layer pipeline:

1. **Per-hop encoding** — `SwapAmounts.encode()` (pool-type-specific ABI encoding)
2. **Approval injection** — `ApprovalStrategy` protocol (default: `NoApprovals`)
3. **Call composition** — `PayloadComposer` protocol (default: `FlatComposer`)

Library callers extend this by implementing custom `ApprovalStrategy` and `PayloadComposer` for their specific smart contracts. V4 encoding requires a custom `PayloadComposer` since V4 uses an unlock/swap callback pattern. The `V4PoolKey` dataclass is available on `UniswapV4PoolSwapAmounts.pool_key` for V4 dispatch. See `src/degenbot/arbitrage/CONTEXT.md` for full terminology.

## Agent skills

### Issue tracker

Local markdown files under `.scratch/<feature-slug>/`. See `docs/agents/issue-tracker.md`.

### Triage labels

Default vocabulary: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`. See `docs/agents/triage-labels.md`.

### Domain docs

Multi-context — `CONTEXT-MAP.md` at root pointing to per-module `CONTEXT.md` files. See `docs/agents/domain.md`.

## Architecture Plans

Refactoring plans live in `plans/`. Completed plans are in `plans/completed/`. Plans 001–045 are all complete; the only remaining active plan is 014 (Async REPL) and the arbitrage optimizer project. See `plans/README.md` for the full list.

When a plan is marked complete: (1) move its file from `plans/` to `plans/completed/`, (2) move its row in `plans/README.md` from the Active Plans table to the Completed Plans table, and (3) update the link to point to the new `completed/` path.

### Legacy Arbitrage Cycles (Deprecated)

The legacy cycle classes (`UniswapLpCycle`, `UniswapCurveCycle`, etc.) have been moved to `src/degenbot/arbitrage/_legacy/` with underscore-prefixed names and `DeprecationWarning` on import. They are replaced by `ArbitragePath` + `ArbSolver`. `AbstractArbitrage` and `get_arbitrage_helpers()` have been deleted (dead code — `ArbitragePath` never inherited `AbstractArbitrage`). The `cvxpy` dependency is now optional (`pip install degenbot[legacy-cycles]`). See `docs/migration-guides/legacy-cycles-to-arbitrage-path.md` for the migration guide.

### Functions Module Decomposition

`src/degenbot/functions.py` has been split into domain-aligned modules:

| New Module | Contents |
|---|---|
| `provider/call_helpers.py` | `raw_call`, `async_raw_call`, `encode_function_calldata`, `extract_argument_types_from_function_prototype` |
| `provider/log_fetching.py` | `fetch_logs_retrying`, `fetch_logs_retrying_async` |
| `contract/addresses.py` | `create2_address`, `eip_1167_clone_address` |
| `calculations/evm_math.py` | `evm_divide`, `next_base_fee`, `raise_if_invalid_uint256` |
| `provider/block_helpers.py` | `get_number_for_block_identifier`, `get_number_for_block_identifier_async` |

`eip_191_hash` was deleted (dead code, zero imports). The original `functions.py` no longer exists — all callers have been migrated.

### Pool-to-Hop Conversion

`solver_hop_builders.py` has been deleted — each pool's `to_hop_state()` method is the single source of truth for pool→hop conversion. The `PoolCompatibility` enum has been removed. The thin free functions `_pool_to_hop_state`, `_extract_fee`, and `_check_pool_compatibility` have been inlined in `arbitrage_path.py`.

## Solidity

### Arithmetic wrapping behavior
- Solidity arithmetic silently wraps for versions < 0.8.0, e.g. `uint8(255) + 1 == 0`
- Solidity arithmetic is checked by default for versions 0.8.0+, e.g. `uint8(255) + 1` will revert

### Porting to Python
- Solidity contracts requires explicit integer division to match the EVM. The Python `//` operator is equivalent to the Solidity `/` operator.
