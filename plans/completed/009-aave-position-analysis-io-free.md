# Plan 009: Separate I/O from Calculation in Position Analysis

## Overview

Apply the I/O-free architecture pattern (ADR-001) to Aave position analysis.
Split `position_analysis.py` into two modules: `core.py` with pure functions
(health factor math, position data builders) and `orchestrator.py` with
I/O (database queries, oracle price fetches). Define `PriceFetcher` and
`PositionQuery` protocols injected at construction.

## Files Involved

- **Existing:**
  - `src/degenbot/aave/position_analysis.py` (764 lines)
  - `src/degenbot/aave/models.py` (Pydantic models, used by analysis)
- **New:**
  - `src/degenbot/aave/analysis/__init__.py`
  - `src/degenbot/aave/analysis/core.py`        — pure functions, no DB/RPC imports
  - `src/degenbot/aave/analysis/protocols.py`   — PriceFetcher, PositionQuery protocols
  - `src/degenbot/aave/analysis/orchestrator.py` — assembles data, calls fetchers
- **Rewrite:**
  - `src/degenbot/aave/position_analysis.py` → shim re-exporting from analysis package

## Problem

`position_analysis.py` mixes three concerns in one file:

1. **Pure math:** `calculate_health_factor()`, `build_collateral_position_data()`,
   `calculate_actual_collateral_balance()` — no side effects, deterministic.
2. **Database queries:** `analyze_positions_for_market()` issues 4+ SQLAlchemy
   queries with `joinedload` for users, positions, collateral configs, assets.
3. **Live RPC calls:** `fetch_asset_prices()` calls `raw_call()` per-asset against
   the Aave oracle contract.

The entry point `analyze_positions_for_market()` takes an optional
`ProviderAdapter` and makes live calls mid-analysis. Unit testing the health
factor formula requires constructing full database models and a mock provider.
This is the exact problem ADR-001 solved for pools.

## Target State

```text
analysis/
├── __init__.py              — exports public API
├── protocols.py             — PriceFetcher, PositionQuery protocols
├── core.py                  — pure functions: health factor math, position builders
└── orchestrator.py          — assembles queries, calls fetchers, delegates to core
```

### `protocols.py`

```python
from collections.abc import Sequence
from typing import Protocol


class PriceFetcher(Protocol):
    """Fetch oracle prices for a set of asset addresses.

    The closure handles I/O (rpc, cache, etc.). The core module
    receives a simple dict mapping addresses to prices.
    """

    def fetch(self, asset_addresses: set[ChecksumAddress]) -> dict[ChecksumAddress, int]: ...


class PositionQuery(Protocol):
    """Query user positions and collateral config from the database.

    The closure handles SQLAlchemy session management. The core
    module receives plain dataclasses with no ORM references.
    """

    def get_users_with_debt(
        self, market_id: int, limit: int | None = None
    ) -> Sequence[UserRecord]: ...

    def get_collateral_positions(self, user_id: int) -> Sequence[CollateralPositionRecord]: ...

    def get_debt_positions(self, user_id: int) -> Sequence[DebtPositionRecord]: ...

    def get_collateral_config_map(self, user_id: int) -> dict[int, bool]: ...

    def get_oracle_address(self, market_id: int) -> ChecksumAddress | None: ...
```

### `core.py`

```python
"""Pure position analysis functions.

No database, no RPC, no ORM. All inputs are plain dataclasses
or primitives. All outputs are plain dataclasses.
"""

from dataclasses import dataclass


@dataclass(frozen=True, slots=True)
class CollateralPositionData:
    asset_address: ChecksumAddress
    scaled_balance: int
    actual_balance: int
    liquidation_threshold: int
    ltv: int
    is_enabled_as_collateral: bool
    in_emode: bool
    price: int | None = None

    @property
    def price_adjusted_balance(self) -> int:
        if self.price is None:
            return self.actual_balance
        return self.actual_balance * self.price

    @property
    def effective_liquidation_threshold(self) -> int:
        return self.liquidation_threshold if self.is_enabled_as_collateral else 0


# ... DebtPositionData, UserPositionSummary, PositionAnalysisResult same as today ...


def calculate_health_factor(
    collateral_positions: tuple[CollateralPositionData, ...],
    debt_positions: tuple[DebtPositionData, ...],
    isolation_mode_debt: int = 0,
    isolation_debt_ceiling: int | None = None,
) -> float | None:
    """Pure health factor calculation.

    All inputs are plain dataclasses. No database, no RPC.
    """
    ...


def build_collateral_position_data(
    position: CollateralPositionRecord,
    *,
    collateral_enabled: bool,
    price: int | None = None,
) -> CollateralPositionData:
    """Build CollateralPositionData from a plain record."""
    ...


def analyze_user_position(
    user: UserRecord,
    collateral_positions: list[CollateralPositionRecord],
    debt_positions: list[DebtPositionRecord],
    collateral_config_map: dict[int, bool],
    price_map: dict[ChecksumAddress, int] | None = None,
) -> UserPositionSummary:
    """Analyze a single user's position. Pure function."""
    ...
```

### `orchestrator.py`

```python
"""Orchestrator that assembles data and delegates to core functions.

This is where I/O lives: database queries and oracle price fetching.
"""

from dataclasses import dataclass


@dataclass(frozen=True)
class PositionAnalysisService:
    """Service that coordinates position analysis with I/O.

    Created by Bot or CLI. Injects fetchers for testing.
    """

    price_fetcher: PriceFetcher | None = None
    position_query: PositionQuery

    def analyze_market(
        self,
        market_id: int,
        health_factor_threshold: float = HEALTH_FACTOR_AT_RISK_THRESHOLD,
        limit: int | None = None,
    ) -> PositionAnalysisResult:
        """Main entry point — calls I/O, then delegates to core."""
        # 1. Query users with debt
        users = self.position_query.get_users_with_debt(market_id, limit)

        # 2. Fetch prices if fetcher available
        price_map: dict[ChecksumAddress, int] = {}
        if self.price_fetcher is not None:
            # Collect asset addresses
            asset_addresses = _collect_asset_addresses(users)
            price_map = self.price_fetcher.fetch(asset_addresses)

        # 3. Analyze each user via core
        result = PositionAnalysisResult()
        for user in users:
            collateral = self.position_query.get_collateral_positions(user.id)
            debt = self.position_query.get_debt_positions(user.id)
            config_map = self.position_query.get_collateral_config_map(user.id)

            summary = analyze_user_position(
                user=user,
                collateral_positions=collateral,
                debt_positions=debt,
                collateral_config_map=config_map,
                price_map=price_map,
            )
            result._categorize(summary, health_factor_threshold)

        result._sort()
        return result
```

### `Bot` Integration

```python
class Bot:
    def build_price_fetcher(self, market_id: int) -> PriceFetcher | None:
        """Create a PriceFetcher closure for this market."""
        oracle_address = self._get_oracle_address(market_id)
        if oracle_address is None:
            return None
        provider = self.connections.get_provider(self.chain_id)

        def fetch(asset_addresses: set[ChecksumAddress]) -> dict[ChecksumAddress, int]:
            return fetch_asset_prices(provider, oracle_address, asset_addresses)

        return fetch

    def build_position_query(self) -> PositionQuery:
        """Create a PositionQuery closure wrapping the database."""
        return _PositionQueryImpl(session_factory=self._db_session_factory)
```

## Migration Steps

1. **Create `analysis/protocols.py`** with `PriceFetcher` and `PositionQuery` protocols.
2. **Create `analysis/core.py`** by extracting pure functions from `position_analysis.py`.
   - Remove all `sqlalchemy`, `ProviderAdapter`, `raw_call` imports.
   - Replace `AaveV3CollateralPosition` ORM types with plain dataclass inputs.
   - Keep exact math logic (floor/ceil rounding, eMode, isolation mode).
3. **Create `analysis/orchestrator.py`** with `PositionAnalysisService`.
   - Copy DB query logic from `analyze_positions_for_market()`.
   - Inject `PriceFetcher` and `PositionQuery` at construction.
4. **Create `_PositionQueryImpl`** (private class) implementing `PositionQuery`.
   - Wraps SQLAlchemy session calls.
   - Converts ORM objects to plain records before passing to core.
5. **Update `Bot`** or create factory method for building fetchers/queries.
6. **Rewrite `position_analysis.py`** as shim:

   ```python
   """Legacy location — re-exports from analysis package."""

   from degenbot.aave.analysis.core import *  # noqa: F403
   from degenbot.aave.analysis.orchestrator import PositionAnalysisService  # noqa: F401
   ```

7. **Update all imports** to use `degenbot.aave.analysis`.
8. **Delete shim** once all callers migrated.

## Test Strategy

**Red phase:** Before touching `position_analysis.py`, write core tests that pass
Fake data directly:

```python
def test_health_factor_no_debt():
    result = calculate_health_factor(
        collateral_positions=(FAKE_COLLATERAL,),
        debt_positions=(),
    )
    assert result is None


def test_health_factor_liquidatable():
    result = calculate_health_factor(
        collateral_positions=(FAKE_COLLATERAL_WITH_LOW_LT,),
        debt_positions=(FAKE_DEBT_HIGH_VALUE,),
    )
    assert result is not None and result < 1.0


def test_emode_enhanced_ltv():
    collateral = FakeCollateralPosition(emode_category_id=1, in_emode=True)
    result = analyze_user_position(FAKE_USER, [collateral], [], {}, {})
    # eMode threshold > standard
```

**Green phase:** Core functions are tested without any database or RPC:

| Test module | Coverage |
|-------------|----------|
| `tests/aave/analysis/test_core_health_factor.py` | All HF formulas: no debt, safe, at-risk, liquidatable, isolation mode |
| `tests/aave/analysis/test_core_emode.py` | eMode category override for LTV and threshold |
| `tests/aave/analysis/test_core_orchestrator.py` | Orchestrator wires mock PriceFetcher + PositionQuery |

**Regression:** Existing `tests/aave/test_position_analysis.py` should pass
unchanged since shim re-exports same names.

## Risks

| Risk | Mitigation |
|------|------------|
| Plain dataclass `UserRecord` loses lazy-loaded relationships | `PositionQuery` eagerly loads all needed data before conversion |
| eMode/inolation mode requires asset config joins | `PositionQuery` flattens this into the plain record |
| `fetch_asset_prices` currently iterates sequentially (N RPC calls) | PriceFetcher closure can batch; not changed in this refactor |
| Health factor formula has subtle rounding differences | Keep exact same math; only change is input/output types |

## Rollback

`position_analysis.py` stays as shim during migration. If core tests fail,
revert to importing directly from the monolith.

## Completion Summary

**I/O-free architecture applied to Aave position analysis.**

**Files created:**
- `src/degenbot/aave/analysis/__init__.py` — package exports
- `src/degenbot/aave/analysis/core.py` — pure functions (353 lines): `calculate_health_factor`, `build_collateral_position_data`, `build_debt_position_data`, `analyze_user_position`, plus flat record types (`UserRecord`, `CollateralPositionRecord`, `DebtPositionRecord`) that replace ORM relationship navigation
- `src/degenbot/aave/analysis/protocols.py` — `PriceFetcher` and `PositionQuery` protocols
- `src/degenbot/aave/analysis/orchestrator.py` — `DatabasePositionQuery` (ORM → flat records), `OraclePriceFetcher` (RPC), `PositionAnalysisService` (orchestrator), `analyze_positions_for_market` (backward-compat entry point)
- `src/degenbot/aave/position_analysis.py` — shim re-exporting from analysis package (764 → 42 lines)

**Design decisions:**
- Used Option 1 from the plan discussion: flatten all needed fields into plain records at the query boundary. `CollateralPositionRecord` includes `asset_lt`, `asset_ltv`, `emode_lt`, `emode_ltv` — all the fields that previously required `position.asset.e_mode_category.liquidation_threshold` navigation
- `get_liquidation_threshold()` and `get_ltv()` are pure keyword-arg functions in core, replacing the ORM-navigating `get_liquidation_threshold_for_position()` and `get_ltv_for_position()`
- `PositionAnalysisResult.categorize()` and `.sort_by_risk()` replace the inline categorization/sorting in `analyze_positions_for_market()`

**Test results:** 338 Aave tests pass (306 existing + 32 new). Core has zero imports from sqlalchemy or degenbot.provider.

**Definition of Done:**

- [x] `analysis/core.py` has zero imports from `sqlalchemy` or `degenbot.provider`
- [x] All pure functions in `core.py` tested with Fake inputs (no DB, no RPC)
- [x] `PriceFetcher` protocol implemented as `OraclePriceFetcher` class
- [x] `PositionQuery` protocol implemented as `DatabasePositionQuery` class
- [x] Orchestrator delegates 100% of math to core functions
- [x] Existing `test_position_analysis.py` passes without modification (via shim)
- [ ] `Bot` can create a `PositionAnalysisService` with injected fetchers —
      `DatabasePositionQuery` and `OraclePriceFetcher` exist; Bot integration deferred
