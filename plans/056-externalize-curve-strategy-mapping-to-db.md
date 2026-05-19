# Plan 056: Externalize Curve Address→Strategy Mapping to Database

## Overview

Move the hard-coded address→`PoolStrategies` mapping from `_pool_strategies.py` into the database, allowing the builder to look up strategies at pool construction time. The Python mapping file becomes a migration seed script. Pure-logic functions (`resolve_d_variant`, `resolve_y_variant`, `resolve_yd_variant` from `_variant_groups.py`) remain as validation defaults for addresses not in the database.

## Problem

### Deletion test

If you deleted `_pool_strategies.py` (451 lines) and `_variant_groups.py` (181 lines), the `CurveStableswapPool` constructor would receive `strategies=None`, default to `PoolStrategies()` (all STANDARD/NONE), and produce wrong swap calculations for every non-plain Curve pool. The mapping is earning its keep — but it's fragile, manually-curated, and limited to Ethereum mainnet.

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|--------------|
| Hard-coded Ethereum mainnet addresses | `_pool_strategies.py` lines 81–370 | ~60 mainnet pool addresses mapped in a Python dict. Adding support for Curve on another chain requires a new mapping file or extending the same one. |
| Provenance warning is unactionable | Module docstring: "⚠️ PROVENANCE WARNING… did NOT verify each address against the on-chain contract source" | The warning acknowledges that the mapping may contain wrong enum values, but there's no way to verify or fix them from the code. The database allows a `curve_strategies` table to be verified/corrected at migration time with a record of provenance. |
| No discovery mechanism | `_POOL_STRATEGIES.get(pool_address)` | If a pool address isn't in the mapping, it gets STANDARD/NONE defaults. For a non-Ethereum chain, every pool gets wrong strategies. The detection modules (`curve/detection/`) can determine the right strategies, but their results aren't persisted. |
| Variant groups use same pattern | `_variant_groups.py` has 7 class-level address frozensets → `resolve_{d,y,yd}_variant()` | Same hard-coded mainnet-only address sets. The D/Y/YD variants could also be detected and persisted. |
| Strategy resolution happens at import time | Pool strategies are resolved by `resolve_pool_strategies()` called from the builder | The builder already has database access. It could look up strategies from the DB instead of from a Python dict. |
| Detection results are discarded | `curve/detection/` modules detect coin types, lending, metapool status, A ramping, and crypto parameters | The builder runs detection, then resolves strategies from the hard-coded mapping (ignoring detection results for swap style). The detection results could feed the strategy directly. |

## Solution

### Step 1: Add `curve_pool_strategy` database model and migration

Create a SQLAlchemy model that stores per-pool strategy data:

```python
class CurvePoolStrategy(Base):
    """Per-pool Curve strategy configuration."""
    __tablename__ = "curve_pool_strategy"

    address: Mapped[str]           # Primary key (checksummed)
    chain_id: Mapped[int]          # Part of primary key
    swap_style: Mapped[str]        # SwapStyle enum value name
    lending_rate_style: Mapped[str]  # LendingRateStyle enum value name
    metapool_rate_style: Mapped[str | None]
    metapool_underlying_style: Mapped[str | None]
    d_variant: Mapped[str]         # DVariant enum value name
    y_variant: Mapped[str]         # YVariant enum value name
    yd_variant: Mapped[str]         # YDVariant enum value name
```

The composite primary key `(address, chain_id)` replaces the mainnet-only single-key mapping. Enum values are stored as strings for readability and forward compatibility.

### Step 2: Create migration seed script from existing `_pool_strategies.py`

Convert the `_POOL_STRATEGIES` dict into an Alembic migration seed that inserts all existing entries with `chain_id=1` (Ethereum mainnet). The provenance warning becomes a comment in the migration file.

```python
# migrations/versions/XXXX_seed_curve_strategies.py
# ⚠️ PROVENANCE: Derived from _pool_strategies.py address→enum mapping.
# See Plan 056 for details. Values have NOT been verified against contract source.

def upgrade():
    strategies = [
        {"address": "0xC61557...", "chain_id": 1, "swap_style": "STANDARD",
         "lending_rate_style": "NONE", "metapool_rate_style": "PRECISION_VP", ...},
        ...
    ]
    op.bulk_insert(CurvePoolStrategy.__table__, strategies)
```

### Step 3: Update `CurvePoolBuilder` to read strategies from database

The builder already has access to the database session via `BuilderContext.db_session`. Replace:

```python
# Before
strategies = resolve_pool_strategies(pool_address)

# After
strategies = _load_strategies_from_db(db_session, pool_address, chain_id)
if strategies is None:
    # Fallback: use detection results + defaults
    strategies = _infer_strategies_from_detection(detection_results)
```

The `_infer_strategies_from_detection` function uses the detection module results (which already run during builder construction) to determine `swap_style`, `lending_rate_style`, etc. This is the path for new chains where no DB entries exist yet.

### Step 4: Persist detection-derived strategies to database

When the builder derives strategies from detection (Step 3 fallback), persist them to the `curve_pool_strategy` table so subsequent `build_pool()` calls for the same address skip detection.

### Step 5: Deprecate `_pool_strategies.py` and `_variant_groups.py`

After the migration seed, the Python mapping files have no runtime callers. Mark them as deprecated and move them to a `_legacy/` directory or delete them. The `resolve_pool_strategies()` function can be kept temporarily as a fallback during transition.

### Design decisions

- **String enum storage**: Store enum value names (e.g., `"RATE_ADJUSTED"`) instead of integers. This makes the database readable without the Python enum definition and is robust to enum renumbering.
- **Chain ID in primary key**: The existing mapping is mainnet-only. Adding `chain_id` to the primary key enables per-chain strategy storage without schema changes.
- **Detection-first for unpersisted pools**: When a pool isn't in the database, the builder runs detection (which it already does) and infers strategies from detection results. This is more reliable than the hard-coded mapping because detection examines the actual contract.
- **Leave variant groups in pure logic**: The `resolve_d_variant`, `resolve_y_variant`, `resolve_yd_variant` functions from `_variant_groups.py` map addresses to D/Y/YD variants. These could also be moved to the database in the same `curve_pool_strategy` table. The pure-logic functions can remain as fallback defaults for detection-based inference.
- **No schema change to `DyCalculationInputs`**: The pool still receives a `PoolStrategies` instance. Where it comes from (hard-coded mapping vs. database) is a builder concern, not a pool concern. The pool's interface is unchanged.

## Files Involved

**Primary:**
- `src/degenbot/database/models/` — new `CurvePoolStrategy` model
- `migrations/versions/` — new migration: create table + seed data
- `src/degenbot/builders/curve_pool_builder.py` — replace `resolve_pool_strategies()` call with DB lookup + detection fallback
- `src/degenbot/curve/_pool_strategies.py` — deprecated, then deleted
- `src/degenbot/curve/_variant_groups.py` — deprecated, then deleted (or kept as pure-logic fallback)

**Secondary:**
- `src/degenbot/curve/curve_stableswap_liquidity_pool.py` — no change (still receives `PoolStrategies`)
- `src/degenbot/curve/types.py` — no change (`PoolStrategies` frozen dataclass unchanged)
- `src/degenbot/curve/detection/` — no change (already produces detection results)
- `src/degenbot/builders/context.py` — verify `db_session` is available (it is)

**No change needed:**
- `src/degenbot/curve/calculators/` — calculators read `DyCalculationInputs`, not strategies
- `src/degenbot/curve/data_provider_impl.py` — provider reads `LendingRateStyle` from strategy, but that's still on `PoolStrategies`

## Implementation Order

### Slice 1: Database model + migration

1. Create `CurvePoolStrategy` SQLAlchemy model with composite key `(address, chain_id)`
2. Generate Alembic migration for the table
3. Run: `just test-python` — expect all tests green (model exists but isn't used yet)

### Slice 2: Seed migration from existing mapping

1. Create seed migration that converts `_POOL_STRATEGIES` dict entries to `curve_pool_strategy` rows with `chain_id=1`
2. Include the D/Y/YD variant data from `_variant_groups.py`
3. Run: `just test-python` — expect all tests green

### Slice 3: Builder reads from database

1. Add `_load_strategies_from_db()` function to `curve_pool_builder.py`
2. Replace `resolve_pool_strategies(pool_address)` call with `_load_strategies_from_db(db_session, pool_address, chain_id)`
3. Add fallback: if not in DB, call `resolve_pool_strategies()` as before (temporary compatibility)
4. Run: `just test-python` — expect all tests green (both paths produce same results)

### Slice 4: Detection-based strategy inference for new pools

1. Add `_infer_strategies_from_detection()` function that maps detection results to `PoolStrategies`
2. Wire as the fallback in Step 3: DB → detection inference → `resolve_pool_strategies()` hard-coded
3. Persist inferred strategies to the database
4. Run: `just test-python` — expect all tests green

### Slice 5: Deprecate and remove hard-coded mapping

1. Mark `_pool_strategies.py` and `_variant_groups.py` as deprecated
2. Remove fallback to `resolve_pool_strategies()` from builder (detection inference is the fallback now)
3. Delete `_pool_strategies.py` and `_variant_groups.py`
4. Run: `just lint` + `just test-all`

### Slice 6: Validate and clean up

1. Run `just lint` + `just test-all`
2. Update `src/degenbot/curve/CONTEXT.md` — add `CurvePoolStrategy` term, update strategy resolution description
3. Verify Curve pool tests pass with DB-backed strategies (may need test DB setup)

## Testing

### Per-slice test runs

Each slice runs `just test-python`. Slices 1–3 should be green at each step. Slice 4 may require test database setup for detection inference.

### New unit tests

```python
# tests/curve/test_curve_pool_strategy_db.py


def test_load_strategies_from_db():
    """Builder reads strategies from database for a known pool address."""
    ...


def test_load_strategies_db_miss_falls_back():
    """When pool not in DB, builder falls back to detection inference."""
    ...


def test_infer_strategies_from_detection():
    """Detection results for a crypto pool produce CRYPTO swap_style."""
    ...


def test_persist_inferred_strategies():
    """Detection-inferred strategies are saved to DB for future lookups."""
    ...


def test_seed_migration_populates_mainnet():
    """Seed migration inserts all known mainnet pool strategies."""
    ...
```

### Integration tests

Existing Curve pool tests that use `bot.build_pool()` will automatically exercise the DB lookup path once wired. Tests using `FakeCurveDataProvider` still work because they construct pools directly with explicit `strategies=`.

## Benefits

- **Leverage**: The database becomes the single source of truth for strategy data. One query replaces a 450-line Python dict. Adding a new chain means inserting rows, not editing Python source.
- **Locality**: Strategy resolution moves from a standalone Python file to the builder, where all other pool construction logic already lives. The builder already has database access and detection results.
- **Chain-agnosticism**: The `CurveStableswapPool` class was already chain-agnostic (it reads `PoolStrategies`, doesn't know where they came from). The hard-coded mapping was the chain-specific artifact. Moving it to the DB removes the last chain-specific code from the Curve module.
- **Verifiability**: Database rows can be inspected, corrected, and audited. The provenance warning moves from code comments to migration metadata.

## Risks

- **Test database setup**: Tests that currently construct pools with `resolve_pool_strategies()` will need a test database or a mock. Mitigation: tests using `FakeCurveDataProvider` already pass explicit `strategies=`. Tests using `bot.build_pool()` can use an in-memory SQLite database.
- **Detection inference accuracy**: The `_infer_strategies_from_detection()` function must correctly determine `swap_style` from detection results. This is non-trivial for subtle variants (e.g., `RATE_ADJUSTED` vs `RATE_ADJUSTED_NO_ONE`). Mitigation: the detection modules already produce the data needed; the inference function maps detection results to enum values deterministically.
- **Migration complexity**: The seed migration must correctly translate all 60+ address entries. Mitigation: the seed is generated programmatically from the existing `_POOL_STRATEGIES` dict and `_variant_groups.py` frozensets, not manually.
- **Provenance still unverified**: Moving to the database doesn't fix the provenance problem — wrong enum values move from Python to SQL. But it makes them inspectable and correctable without code changes.

## Relationship to Other Plans

- **Plan 026** (Curve Strategy Objects): Completed. Established `PoolStrategies` and the address→strategy mapping pattern. This plan moves the mapping from code to data.
- **Plan 029** (Variant Group Externalization): Completed. Extracted variant groups from class-level frozensets to `_variant_groups.py`. This plan takes the next step: from module-level Python dicts to database rows.
- **Plan 018** (Curve Pool Builder Decomposition): Completed. Broke the builder into focused detection sub-modules. This plan uses those detection results for strategy inference.
- **Plan 054** (Consolidate Curve On-Chain Caches): Complementary. Plan 054 organizes the pool's cache fields; this plan organizes the builder's strategy resolution. No dependency.
- **Plan 053** (Delete Old Optimizer Hierarchy): Orthogonal. Different module.

## Status

[ ] Slice 1: Database model + migration
[ ] Slice 2: Seed migration from existing mapping
[ ] Slice 3: Builder reads from database
[ ] Slice 4: Detection-based strategy inference for new pools
[ ] Slice 5: Deprecate and remove hard-coded mapping
[ ] Slice 6: Validate and clean up
