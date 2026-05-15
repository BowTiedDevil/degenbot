# Plan 032: Rename Off-Chain PoolManager Classes to PoolTracker

Rename all off-chain pool "manager" classes to "tracker" to eliminate the naming collision with the V4 on-chain `PoolManager` singleton contract.

## Problem

"Pool Manager" (off-chain, two words) and "PoolManager" (on-chain V4, one word) differ only by whitespace and capitalization. This causes confusion in speech, docs, and code comments. The domain terminology has already been updated in CONTEXT.md files (Plan 031) — this plan aligns the code.

## Scope

### What gets renamed (off-chain tracker concept)

| Current name | New name | File |
|-------------|----------|------|
| `AbstractPoolManager` | `AbstractPoolTracker` | `types/abstract/pool_manager.py` → `pool_tracker.py` |
| `AbstractUniswapV2PoolManager` | `AbstractUniswapV2PoolTracker` | `uniswap/managers.py` → `trackers.py` |
| `AbstractUniswapV3PoolManager` | `AbstractUniswapV3PoolTracker` | `uniswap/managers.py` → `trackers.py` |
| `UniswapV2PoolManager` | `UniswapV2PoolTracker` | `uniswap/managers.py` → `trackers.py` |
| `UniswapV3PoolManager` | `UniswapV3PoolTracker` | `uniswap/managers.py` → `trackers.py` |
| `SushiswapV2PoolManager` | `SushiswapV2PoolTracker` | `sushiswap/managers.py` → `trackers.py` |
| `SushiswapV3PoolManager` | `SushiswapV3PoolTracker` | `sushiswap/managers.py` → `trackers.py` |
| `PancakeswapV2PoolManager` | `PancakeswapV2PoolTracker` | `pancakeswap/managers.py` → `trackers.py` |
| `PancakeswapV3PoolManager` | `PancakeswapV3PoolTracker` | `pancakeswap/managers.py` → `trackers.py` |
| `AerodromeV2PoolManager` | `AerodromeV2PoolTracker` | `aerodrome/managers.py` → `trackers.py` |
| `AerodromeV3PoolManager` | `AerodromeV3PoolTracker` | `aerodrome/managers.py` → `trackers.py` |
| `_AbstractAerodromeV2PoolManager` | `_AbstractAerodromeV2PoolTracker` | `aerodrome/managers.py` → `trackers.py` |
| `SwapbasedV2PoolManager` | `SwapbasedV2PoolTracker` | `swapbased/managers.py` → `trackers.py` |
| `CurveStableswapPoolManager` | `CurveStableswapPoolTracker` | `curve/managers.py` → `trackers.py` |
| `BalancerV2PoolManager` | `BalancerV2PoolTracker` | `balancer/managers.py` → `trackers.py` |

### File renames (module files, one per DEX)

| Current | New |
|---------|-----|
| `types/abstract/pool_manager.py` | `types/abstract/pool_tracker.py` |
| `uniswap/managers.py` | `uniswap/trackers.py` |
| `sushiswap/managers.py` | `sushiswap/trackers.py` |
| `pancakeswap/managers.py` | `pancakeswap/trackers.py` |
| `aerodrome/managers.py` | `aerodrome/trackers.py` |
| `swapbased/managers.py` | `swapbased/trackers.py` |
| `curve/managers.py` | `curve/trackers.py` |
| `balancer/managers.py` | `balancer/trackers.py` |

### What does NOT get renamed (on-chain V4 PoolManager concept)

These refer to the V4 singleton contract, not the off-chain tracker:

- `UniswapPoolManagerDeployment` (in `uniswap/deployments.py`) — on-chain deployment data
- `PoolManagerTable` (in `database/models/pools.py`) — DB table for the V4 contract
- `ForeignKeyPoolManagerId` (in `database/models/types.py`) — FK type for V4
- `PoolManagerAddress` (in `uniswap/v4_snapshot.py`) — type alias for V4 contract address
- All references to `pool_manager` as a V4 contract field/parameter
- DB table `pool_managers` and its migrations — this is the V4 contract table
- The `pool_factory` class attribute on `AbstractPoolTracker` — this is the *pool class* the tracker handles, not the on-chain Factory; rename separately if desired but out of scope

### API renames

| Current | New |
|---------|-----|
| `Bot.add_manager()` | `Bot.add_tracker()` |
| `AsyncBot.add_manager()` | `AsyncBot.add_tracker()` |
| `Bot._managers` attribute | `Bot._trackers` |
| `AbstractPoolManager.pool_factory` | `AbstractPoolTracker.pool_factory` (attribute name unchanged — it refers to the pool *class*, not the Factory contract) |

### Backward-compat aliases (temporary)

Add deprecated aliases for the old class names in each module's `__init__.py`:

```python
# In uniswap/__init__.py
def __getattr__(name: str) -> Any:
    if name == "UniswapV2PoolManager":
        warnings.warn("UniswapV2PoolManager is deprecated, use UniswapV2PoolTracker", DeprecationWarning, stacklevel=2)
        return UniswapV2PoolTracker
    if name == "UniswapV3PoolManager":
        warnings.warn("UniswapV3PoolManager is deprecated, use UniswapV3PoolTracker", DeprecationWarning, stacklevel=2)
        return UniswapV3PoolTracker
    raise AttributeError(f"module {__name__!r} has no attribute {name!r}")
```

Similarly for `AbstractPoolManager` → `AbstractPoolTracker` in `types/abstract/__init__.py`, and all DEX submodules.

For `Bot.add_manager()`, add a deprecated wrapper:

```python
def add_manager(self, *args: Any, **kwargs: Any) -> Any:
    warnings.warn("add_manager() is deprecated, use add_tracker()", DeprecationWarning, stacklevel=2)
    return self.add_tracker(*args, **kwargs)
```

These aliases should be removed in a future plan (Plan 022 successor).

## Steps

### Step 1 — Rename `AbstractPoolManager` → `AbstractPoolTracker`

The base class everything depends on.

- Rename `src/degenbot/types/abstract/pool_manager.py` → `pool_tracker.py`
- Rename class `AbstractPoolManager` → `AbstractPoolTracker`
- Update docstring: "Base class for liquidity pool trackers."
- Update `types/abstract/__init__.py` exports
- Add deprecated `__getattr__` alias for `AbstractPoolManager`
- Update all imports across the codebase (7 files reference `AbstractPoolManager`)

**Files touched:** `types/abstract/pool_tracker.py`, `types/abstract/__init__.py`, `bot.py`, `async_bot.py`, `uniswap/trackers.py`, `curve/trackers.py`, `aerodrome/trackers.py`

### Step 2 — Rename Uniswap tracker classes and module

- Rename `uniswap/managers.py` → `trackers.py`
- Rename classes:
  - `AbstractUniswapV2PoolManager` → `AbstractUniswapV2PoolTracker`
  - `UniswapV2PoolManager` → `UniswapV2PoolTracker`
  - `AbstractUniswapV3PoolManager` → `AbstractUniswapV3PoolTracker`
  - `UniswapV3PoolManager` → `UniswapV3PoolTracker`
- Update `uniswap/__init__.py` exports
- Add deprecated `__getattr__` aliases for all 4 old names

**Files touched:** `uniswap/trackers.py`, `uniswap/__init__.py`

### Step 3 — Rename SushiSwap tracker classes and module

- Rename `sushiswap/managers.py` → `trackers.py`
- Rename classes:
  - `SushiswapV2PoolManager` → `SushiswapV2PoolTracker`
  - `SushiswapV3PoolManager` → `SushiswapV3PoolTracker`
- Update `sushiswap/__init__.py` exports
- Add deprecated `__getattr__` aliases

**Files touched:** `sushiswap/trackers.py`, `sushiswap/__init__.py`

### Step 4 — Rename PancakeSwap tracker classes and module

- Rename `pancakeswap/managers.py` → `trackers.py`
- Rename classes:
  - `PancakeswapV2PoolManager` → `PancakeswapV2PoolTracker`
  - `PancakeswapV3PoolManager` → `PancakeswapV3PoolTracker`
- Update `pancakeswap/__init__.py` exports
- Add deprecated `__getattr__` aliases

**Files touched:** `pancakeswap/trackers.py`, `pancakeswap/__init__.py`

### Step 5 — Rename Aerodrome tracker classes and module

- Rename `aerodrome/managers.py` → `trackers.py`
- Rename classes:
  - `_AbstractAerodromeV2PoolManager` → `_AbstractAerodromeV2PoolTracker`
  - `AerodromeV2PoolManager` → `AerodromeV2PoolTracker`
  - `AerodromeV3PoolManager` → `AerodromeV3PoolTracker`
- Update `aerodrome/__init__.py` exports
- Add deprecated `__getattr__` aliases

**Files touched:** `aerodrome/trackers.py`, `aerodrome/__init__.py`

### Step 6 — Rename SwapBased tracker class and module

- Rename `swapbased/managers.py` → `trackers.py`
- Rename class: `SwapbasedV2PoolManager` → `SwapbasedV2PoolTracker`
- Update `swapbased/__init__.py` exports
- Add deprecated `__getattr__` alias

**Files touched:** `swapbased/trackers.py`, `swapbased/__init__.py`

### Step 7 — Rename Curve tracker class and module

- Rename `curve/managers.py` → `trackers.py`
- Rename class: `CurveStableswapPoolManager` → `CurveStableswapPoolTracker`
- Update `curve/__init__.py` exports
- Add deprecated `__getattr__` alias

**Files touched:** `curve/trackers.py`, `curve/__init__.py`

### Step 8 — Rename Balancer tracker class and module

- Rename `balancer/managers.py` → `trackers.py`
- Rename class: `BalancerV2PoolManager` → `BalancerV2PoolTracker`
- Update `balancer/__init__.py` exports
- Add deprecated `__getattr__` alias

**Files touched:** `balancer/trackers.py`, `balancer/__init__.py`

### Step 9 — Rename `Bot.add_manager()` → `Bot.add_tracker()` and `Bot._managers` → `Bot._trackers`

- Rename `Bot._managers` → `Bot._trackers` (dict attribute)
- Rename `Bot.add_manager()` → `Bot.add_tracker()`
- Add deprecated `add_manager()` wrapper with `DeprecationWarning`
- Same for `AsyncBot`

**Files touched:** `bot.py`, `async_bot.py`

### Step 10 — Update top-level `__init__.py` exports

- Replace all `*PoolManager` names with `*PoolTracker` in `degenbot/__init__.py`
- Add deprecated `__getattr__` aliases for old names

**Files touched:** `degenbot/__init__.py`

### Step 11 — Update all internal references

Update all non-public references that use old names:

| File | What changes |
|------|-------------|
| `uniswap/v2_functions.py` | `pool_manager` parameter → `pool_tracker`; type hints |
| `uniswap/v4_pool_state.py` | Comment "PoolManager-tracked" → "PoolTracker-tracked" |
| `cli/pool.py` | Variable names, parameter names |
| `cli/exchange.py` | Variable names |
| `pathfinding.py` | Variable names (PoolManagerTable stays — it's the V4 DB table) |
| `database/models/pools.py` | `manager_id` → `tracker_id` on `ManagedLiquidityPoolTable` (the FK to PoolManagerTable); `manager` relationship → `tracker`. Note: `PoolManagerTable` name stays (it's the V4 contract) |
| `database/models/types.py` | `ForeignKeyPoolManagerId` stays (V4 concept) |
| `database/models/__init__.py` | Exports if any `manager` names changed |

**Important distinction:** `PoolManagerTable` (the DB model for the V4 contract) does NOT change. Only the `manager_id` FK and `manager` relationship on `ManagedLiquidityPoolTable` change to `tracker_id`/`tracker` — because that FK links a pool to the *tracker* that monitors it, not the V4 contract directly.

### Step 12 — Update tests

Split into sub-steps by test file:

| Test file | Changes |
|-----------|---------|
| `tests/test_bot.py` | `add_manager` → `add_tracker`; `UniswapV2PoolManager` → `UniswapV2PoolTracker` |
| `tests/uniswap/test_uniswap_managers.py` → `test_uniswap_trackers.py` | All class names and imports |
| `tests/uniswap/v2/test_v2_pool_io_free.py` | `add_manager` → `add_tracker`; import renames |
| `tests/uniswap/v3/test_v3_pool_io_free.py` | Same |
| `tests/registry/test_pool_subclass_selection.py` | Import renames |
| `tests/curve/test_curve_pool_manager.py` → `test_curve_pool_tracker.py` | Class name and imports |
| `tests/database/test_pools.py` | `manager_id` → `tracker_id` if applicable |
| `tests/database/conftest.py` | Same |

### Step 13 — Update CONTEXT.md files

Already done in Plan 031 + follow-up session. Verify no new "Pool Manager" references crept in.

### Step 14 — Add migration for DB column rename (if applicable)

If `ManagedLiquidityPoolTable.manager_id` is renamed to `tracker_id`, create an Alembic migration:

```python
def upgrade() -> None:
    op.alter_column("managed_liquidity_pools", "manager_id", new_column_name="tracker_id")

def downgrade() -> None:
    op.alter_column("managed_liquidity_pools", "tracker_id", new_column_name="manager_id")
```

## Dependency graph

```
Step 1 (AbstractPoolTracker) ←── foundational; all others depend on it
  ├── Step 2 (Uniswap trackers) ←── depends on Step 1
  │   ├── Step 3 (SushiSwap) ←── depends on Step 2 (subclasses Uniswap trackers)
  │   ├── Step 4 (PancakeSwap) ←── depends on Step 2
  │   ├── Step 5 (Aerodrome V3) ←── depends on Step 2 (V3 tracks like UniswapV3)
  │   └── Step 6 (SwapBased) ←── depends on Step 2 (subclasses UniswapV2 tracker)
  ├── Step 5 (Aerodrome V2) ←── depends on Step 1 (subclasses AbstractPoolTracker directly)
  ├── Step 7 (Curve) ←── depends on Step 1
  ├── Step 8 (Balancer) ←── depends on Step 1
  └── Step 9 (Bot API) ←── depends on Step 1
      ├── Step 10 (Top-level exports) ←── depends on Steps 2–8
      ├── Step 11 (Internal refs) ←── depends on Steps 1–9
      ├── Step 12 (Tests) ←── depends on Steps 1–11
      └── Step 14 (DB migration) ←── depends on Step 11
```

Recommended implementation order: 1 → 2 → 3 → 4 → 5 → 6 → 7 → 8 → 9 → 10 → 11 → 12 → 14 → 13 (verify)

## Verification

After all steps:

- [ ] `grep -r "PoolManager" src/degenbot/ --include="*.py"` returns only V4-contract references (`PoolManagerTable`, `UniswapPoolManagerDeployment`, `PoolManagerAddress`, `pool_manager` as a V4 field)
- [ ] `grep -r "add_manager" src/degenbot/ --include="*.py"` returns only the deprecated wrapper
- [ ] `grep -r "managers.py" src/degenbot/ --include="*.py"` returns nothing (all module files renamed)
- [ ] `just test-python` passes
- [ ] `just lint` passes (ruff, mypy)
- [ ] No `PoolManager` class name in any off-chain tracker context
