# Plan 030: Consolidate Exception Module Files

## Overview

Consolidate the 12 exception sub-files in `src/degenbot/exceptions/` into 3-4 domain-aligned files. The current split creates a shallow module for every 2-3 exception classes —12 files totaling 583 lines, with 6 files under 26 lines each. Related exceptions are distributed across files by technical category rather than by domain, forcing readers to open 12 files to understand what can go wrong in one area.

## Files Involved

**Primary:**
- `src/degenbot/exceptions/` — all 12 exception files consolidated into 3-4

**Secondary:**
- `src/degenbot/exceptions/__init__.py` — update re-exports (class names unchanged, import paths change)
- All files that import from `degenbot.exceptions.*` — update import paths if they import from sub-modules directly

## Problem

### Deletion test

If you merge all exception files into a single `exceptions.py`, complexity does NOT vanish across callers — it concentrates in one file. But that's exactly what we want. The current 12-file split is a **deletion-test pass-through**: the merge doesn't spread complexity, it gathers it.

The real question is: does the split provide any locality benefit? Would you ever need to read just `evm.py` (19 lines, 2 classes) without also needing `liquidity_pool.py` (132 lines, 10 classes)?

Answer: No. `EVMRevertError` is raised by pool code and caught by pool code. It belongs with pool exceptions, not in a standalone file.

### Current file sizes and contents

| File | Lines | Exception classes | Domain |
|------|-------|------------------|--------|
| `base.py` | 44 | `DegenbotError`, `DegenbotValueError`, `DegenbotTypeError`, `ExternalServiceError` | Foundation |
| `arbitrage.py` | 97 | `ArbitrageError`, `ArbCalculationError`, `RateOfExchangeBelowMinimum`, `InvalidSwapPathError`, `NoLiquidity`, `InvalidForwardAmount`, `IncompatiblePoolInvariant`, `Unprofitable`, `NoSolverSolution`, `OptimizationError` | Arbitrage/Solver |
| `liquidity_pool.py` | 132 | `LiquidityPoolError`, `AddressMismatch`, `LiquidityMapWordMissing`, `BrokenPool`, `ExternalUpdateError`, `IncompleteSwap`, `LateUpdateError`, `NoPoolStateAvailable`, `InvalidSwapInputAmount`, `PossibleInaccurateResult`, `UnknownPool`, `UnknownPoolId` | Pools |
| `fetching.py` | 46 | `FetchingError`, `LogFetchingTimeout`, `BlockFetchingTimeout` | RPC/Network |
| `connection.py` | 56 | `DegenbotConnectionError`, `ConnectionTimeout`, `IPCSocketTimeout`, `Web3ConnectionTimeout` | RPC/Network |
| `curve.py` | 21 | `CurveError`, `MissingCurveData` | Pools (Curve) |
| `evm.py` | 19 | `EVMRevertError`, `InvalidUint256` | Pools (EVM) |
| `manager.py` | 26 | `ManagerError`, `PoolNotAssociated`, `PoolCreationFailed`, `ManagerAlreadyInitialized` | Pools (Trackers) |
| `registry.py` | 17 | `RegistryError`, `RegistryAlreadyInitialized` | Infrastructure |
| `database.py` | 13 | `BackupExists` | Infrastructure |
| `erc20.py` | 16 | `Erc20TokenError`, `NoPriceOracle` | Tokens |
| `anvil.py` | 23 | `AnvilError` | Infrastructure (Anvil) |
| `__init__.py` | 73 | Re-exports from all of the above | — |

### Key observations

1. **Pool-related exceptions are split across 5 files:** `liquidity_pool.py`, `curve.py`, `evm.py`, `manager.py`, and `arbitrage.py` (since `NoLiquidity` is an arbitrage exception that's raised by pool code). Understanding "what can go wrong with pools?" requires reading 5 files. Note: `manager.py` contains Pool Tracker exceptions — the class names still use "Manager" pending a full rename (see CONTEXT.md for the Tracker↔Manager ruling).
2. **Infrastructure exceptions are split across 4 files:** `connection.py`, `fetching.py`, `registry.py`, `database.py`, `anvil.py`. Understanding "what can fail at the infrastructure layer?" requires reading 5 files.
3. **Tiny files with no independent value:** `evm.py` (2 classes), `database.py` (1 class), `anvil.py` (1 class), `registry.py` (2 classes), `erc20.py` (2 classes), `curve.py` (2 classes). None of these justify their own file.
4. **`NoLiquidity` is an arbitrage exception but it's raised by pool code.** This cross-domain reference is a code smell. After consolidation, the exception hierarchy makes the relationship clearer.

## Solution

### Proposed file structure

```
src/degenbot/exceptions/
├── __init__.py        # Re-exports (unchanged API)
├── base.py            # Foundation: DegenbotError, DegenbotValueError, DegenbotTypeError, ExternalServiceError
├── pool.py            # Pool + EVM + Curve + Tracker exceptions (merges liquidity_pool.py, curve.py, evm.py, manager.py)
├── arbitrage.py       # Arbitrage/Solver exceptions (unchanged — largest, most coherent)
├── infrastructure.py  # Connection + Fetching + Registry + Database + Anvil + ERC20 (merges 6 files)
```

4 files instead of 12. The `base.py` and `arbitrage.py` files are unchanged (moved as-is).

### File: `pool.py` (merges `liquidity_pool.py` + `curve.py` + `evm.py` + `manager.py`)

> **Note:** The `manager.py` file contains exceptions for Pool Trackers (off-chain discovery/tracking helpers). The exception class names (`ManagerError`, etc.) still use "Manager" pending a full class rename. The file section header below reflects the new Tracker terminology.

~200 lines. Contains all pool-related exceptions in a single file with clear section headers:

```python
"""Pool-related exceptions.

Includes exceptions for:
- Generic pool operations (LiquidityPoolError, BrokenPool, ...)
- EVM execution (EVMRevertError, InvalidUint256)
- Curve StableSwap (CurveError, MissingCurveData)
- Pool trackers (ManagerError, PoolNotAssociated, ...)
"""


# --- EVM ---
class EVMRevertError(DegenbotError): ...


class InvalidUint256(EVMRevertError): ...


# --- Curve ---
class CurveError(DegenbotError): ...


class MissingCurveData(CurveError): ...


# --- Pool Trackers ---
class ManagerError(DegenbotError): ...  # TODO: rename to TrackerError


# ...


# --- Liquidity Pools ---
class LiquidityPoolError(DegenbotError): ...


# ...
```

**Rationale:** EVM, Curve, and pool tracker exceptions are all raised and caught within pool construction and update paths. A developer working on pool code needs all of these.

### File: `infrastructure.py` (merges `connection.py` + `fetching.py` + `registry.py` + `database.py` + `anvil.py` + `erc20.py`)

~190 lines. Contains all infrastructure and token exceptions:

```python
"""Infrastructure and token exceptions.

Includes exceptions for:
- RPC connections (DegenbotConnectionError, ConnectionTimeout, ...)
- Data fetching (FetchingError, LogFetchingTimeout, BlockFetchingTimeout)
- Registry operations (RegistryError, RegistryAlreadyInitialized)
- Database operations (BackupExists)
- Anvil fork operations (AnvilError)
- ERC-20 token operations (Erc20TokenError, NoPriceOracle)
"""
```

**Rationale:** These exceptions are all raised and caught at the infrastructure layer (connection managers, providers, database, registries). A developer debugging "why can't I connect?" needs all of these.

### File: `base.py` (unchanged)

`DegenbotError`, `DegenbotValueError`, `DegenbotTypeError`, `ExternalServiceError`. These are the parent classes — keeping them in a separate file makes the inheritance tree discoverable.

### File: `arbitrage.py` (unchanged)

10 exception classes, 97 lines. Already coherent — all exceptions relate to arbitrage/solver operations. No change needed.

### Update `__init__.py`

The `__init__.py` re-exports all public exception classes. The import paths change internally, but the exported names are identical:

```python
from degenbot.exceptions.base import DegenbotError, DegenbotTypeError, DegenbotValueError, ExternalServiceError
from degenbot.exceptions.pool import (
    BrokenPool, CurveError, EVMRevertError, InvalidSwapInputAmount, InvalidUint256,
    LiquidityPoolError, ManagerError, MissingCurveData, ...  # ManagerError → TrackerError pending rename
)
from degenbot.exceptions.arbitrage import (
    ArbCalculationError, IncompatiblePoolInvariant, InvalidForwardAmount,
    InvalidSwapPathError, NoLiquidity, NoSolverSolution, OptimizationError,
    RateOfExchangeBelowMinimum, Unprofitable,
)
from degenbot.exceptions.infrastructure import (
    AnvilError, BackupExists, BlockFetchingTimeout, ConnectionTimeout,
    DegenbotConnectionError, Erc20TokenError, FetchingError,
    LogFetchingTimeout, NoPriceOracle, RegistryError, ...
)
```

### Handle direct sub-module imports

Some code imports directly from sub-modules:

```python
from degenbot.exceptions.curve import MissingCurveData
from degenbot.exceptions.arbitrage import NoLiquidity
```

After consolidation, these become:
```python
from degenbot.exceptions.pool import MissingCurveData
from degenbot.exceptions.arbitrage import NoLiquidity
```

**Migration approach:** Keep the old sub-modules as thin re-export stubs for one release cycle with deprecation warnings. This avoids a flag-day breaking change:

```python
# src/degenbot/exceptions/curve.py — DEPRECATED
"""Deprecated. Import from degenbot.exceptions.pool instead."""

import warnings
from degenbot.exceptions.pool import CurveError, MissingCurveData

warnings.warn(
    "Importing from degenbot.exceptions.curve is deprecated. Use degenbot.exceptions.pool instead.",
    DeprecationWarning,
    stacklevel=2,
)
```

## Implementation Order

1. **Create `pool.py` and `infrastructure.py`** with the consolidated exception classes — no behaviour change (old files still exist)
2. **Update `__init__.py`** to import from the new files instead of the old ones — the public API is unchanged
3. **Convert old sub-modules to re-export stubs** with deprecation warnings — backwards-compatible
4. **Update all internal imports** to use the new paths — search and replace
5. **Update tests** that import from old sub-modules
6. **Remove old sub-modules** after deprecation period

## Testing

### Unit tests

No functional tests needed — this is a file reorganization. The exception class hierarchy and public API are unchanged.

### Import path tests

```python
def test_public_api_unchanged():
    """All exception classes are still importable from degenbot.exceptions."""
    from degenbot.exceptions import (
        BrokenPool, CurveError, DegenbotError, EVMRevertError,
        MissingCurveData, NoLiquidity, OptimizationError, ...
    )
    # All classes exist and have correct inheritance
    assert issubclass(CurveError, DegenbotError)
    assert issubclass(EVMRevertError, DegenbotError)
    assert issubclass(NoLiquidity, DegenbotError)

def test_deprecated_imports_work():
    """Old import paths still work with deprecation warning."""
    import warnings
    with warnings.catch_warnings(record=True) as w:
        warnings.simplefilter("always")
        from degenbot.exceptions.curve import MissingCurveData
        assert len(w) == 1
        assert issubclass(w[0].category, DeprecationWarning)
```

### Full suite

Run `just test-python` to verify no import errors.

## Benefits

- **Locality:** Understanding "what can go wrong with pools?" requires reading one file (`pool.py`) instead of five (`liquidity_pool.py`, `curve.py`, `evm.py`, `manager.py` (tracker exceptions), and parts of `arbitrage.py`).
- **Leverage:** The public API (`from degenbot.exceptions import ...`) doesn't change. Callers don't need to update. But the internal organization is simpler.
- **Fewer files to maintain:** 12 → 4. Less boilerplate, less navigation, less cognitive overhead.

## Risks

- **Breaking change for direct imports:** Code that imports from `degenbot.exceptions.curve` or `degenbot.exceptions.evm` will break if not updated. The deprecation stubs mitigate this but add temporary maintenance burden.
- **File size increase:** `pool.py` will be ~200 lines. This is well within reason — the current `liquidity_pool.py` is already 132 lines on its own.
- **Cross-domain boundary in `pool.py`:** `EVMRevertError` is technically not a pool-specific error — it's raised by EVM execution in general. However, in degenbot, it's only raised and caught in pool code. The consolidation is pragmatic, not theoretical.

## Relationship to Other Plans

- **All other plans:** Independent. Exception consolidation is a code organization improvement that doesn't affect behaviour. It can be done at any time without coordinating with other plans.
- **Plan 026/027/029** (Curve improvements): If `MissingCurveData` gains new subtypes as part of strategy/fetcher work, they would be added to the consolidated `pool.py` file.
