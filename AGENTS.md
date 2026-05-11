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

## Database
- Config file at `~/.config/degenbot/config.toml`; database path defaults to `~/.config/degenbot/degenbot.db` (overridable via `database.path` setting)
- SQLAlchemy ORM models in `src/degenbot/database/models/`
- Use the scoped session context manager: `with db_session() as session:` from `degenbot.database.db_session`

## Python Design

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

### Logging
- Use `from degenbot.logging import logger`

### Testing
- Add docstring to complex tests describing "what" and "why"
- Create a test double with `Fake` prefix instead of mocking

## Refactoring
- Unless directed otherwise, design standalone features without a backwards compatibility layer.

## Ubiquitous Language

Each module has a `CONTEXT.md` defining domain terms, aliases to avoid, and resolved ambiguities. The root [`CONTEXT-MAP.md`](CONTEXT-MAP.md) indexes all modules and holds cross-cutting content (relationships, ambiguity rulings). Read the relevant module context before naming variables, classes, or docstrings in that area.

## Architecture Patterns

### The Bot Session Pattern

All pool and token creation flows through the `Bot` class. `Bot` owns registries (`pools`, `tokens`, `managed_pools`) and connection managers.

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

V4 pools are identified by passing `pool_id` to `build_pool(address, pool_id=...)`, which routes to `build_v4_pool`.

Typed builders (`build_v2_pool`, `build_v3_pool`, `build_v4_pool`, `build_curve_pool`) remain public for callers who already know the type.

### Fetcher Protocols

**Curve pools** use **fetcher callbacks** for fully I/O-free operation — all on-chain data access flows through injected closures:

```python
# Bot creates fetcher closures (handles I/O internally)
# Pool calls fetchers on-demand via RateFetcher, VirtualPriceFetcher protocols
pool = bot.build_curve_pool("0xbEbc44782C7db0a1A60Cb6fe97d0b483032FF1C7")
```

**V2/V3/V4/Aerodrome pools** are I/O-free at construction — builders fetch all data from DB/RPC, pass it to the pool constructor, and no provider references remain on the pool object. The update path currently still reaches through the builder (Plan 017 tracks completion of this migration).

See `docs/architecture/io-free-pools.md` and `src/degenbot/curve/CONTEXT.md` for details.

### Enum Naming: PoolFamily vs PoolInvariant

Two enums cover related but distinct concepts:
- **`PoolFamily`** (in `types/pool_type.py`) — identifies a pool's mathematical invariant family for type resolution and DB kind derivation. Values: `CONSTANT_PRODUCT`, `CONCENTRATED_LIQUIDITY`, `STABLESWAP`, `WEIGHTED`.
- **`PoolInvariant`** (in `types/hop_types.py`) — identifies the solver dispatch path for arbitrage optimization. Values: `CONSTANT_PRODUCT`, `BOUNDED_PRODUCT`, `SOLIDLY_STABLE`, `CURVE_STABLESWAP`, `BALANCER_WEIGHTED`, `BALANCER_MULTI_TOKEN`.

A `PoolFamily` maps 1:1 to `PoolInvariant` for V2/V3, but N:1 for Curve/Stable and Balancer/Weighted (e.g., `STABLESWAP` → `CURVE_STABLESWAP` or `SOLIDLY_STABLE`). See Plan 020 for the migration from the previous dual `PoolInvariant` naming.

### CacheablePool Protocol

Pools that register with the Rust solver cache implement the `CacheablePool` protocol, providing `reserves_for_cache()` and `fee_for_cache()` methods. This replaces `getattr`-based introspection in the adapter (Plan 019).

### Swap Encoding Pipeline

Each `SwapAmounts` subclass (V2, V3, Curve, V4) has an `encode(recipient=)` method that produces an `EncodedCall(to, data, value)`. The `generate_payloads()` function wires a three-layer pipeline:

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

Refactoring plans live in `plans/`. Completed plans are in `plans/completed/`. Key active plans:

| # | Plan | Summary |
|---|------|---------|
| 017 | Complete I/O-Free Migration for V2/V3/V4/Aerodrome Pools | Remove all `ProviderAdapter`-taking methods from pool classes. Completes ADR-001 Phase 3. |
| 018 | Decompose CurvePoolBuilder.build() into Detection Sub-Modules | Break 400-line `build()` into 5 focused detectors. |
| 020 | Unify the Dual PoolInvariant Enum | Rename identity-level enum to `PoolFamily`. |
| 019 | Replace ArbPoolCacheAdapter getattr Chain with Protocol Methods | Add `reserves_for_cache()` / `fee_for_cache()` to pool protocol. |
| 021 | Extract SwapEncoder from UniswapLpCycle | Standalone swap calldata encoding module. |

See `plans/README.md` for the full list with dependencies and recommended implementation order.

## Solidity

### Arithmetic wrapping behavior
- Solidity arithmetic silently wraps for versions < 0.8.0, e.g. `uint8(255) + 1 == 0`
- Solidity arithmetic is checked by default for versions 0.8.0+, e.g. `uint8(255) + 1` will revert

### Porting to Python
- Solidity contracts requires explicit integer division to match the EVM. The Python `//` operator is equivalent to the Solidity `/` operator.
