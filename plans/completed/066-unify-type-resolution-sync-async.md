# Plan 066: Unify Type Resolution Sync/Async Mirror Functions

## Overview

Extract shared pure-logic steps from the sync/async mirror functions in `type_resolution.py` into testable pure functions, so each resolution step is defined once instead of twice. Collapse the 4 probe/resolve functions (2 sync/async pairs) into 2 thin wrappers backed by shared pure logic. Leave `fetch_factory_from_chain` / `fetch_factory_from_chain_async` as-is — they're too small to benefit from extraction.

## Problem

### Deletion test

If you delete the three async functions (`resolve_pool_type_async`, `fetch_factory_from_chain_async`, `resolve_pool_type_by_probing_async`), the same logic exists in their sync counterparts. The async functions add `await` keywords and `async`/`await` syntax — nothing else. This confirms they are shadow implementations.

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|-------------|
| `resolve_pool_type` vs `resolve_pool_type_async` | `type_resolution.py:238–290` vs `292–339` | ~50 lines of near-identical code differing only in `await` |
| `resolve_pool_type_by_probing` vs `resolve_pool_type_by_probing_async` | `type_resolution.py:123–181` vs `183–236` | ~55 lines each, differing only in `await` and `AsyncPoolIOProtocol` type |
| `fetch_factory_from_chain` vs `fetch_factory_from_chain_async` | `type_resolution.py:87–103` vs `105–121` | 17 lines each, only `await` differs — **too small to extract, leave as-is** |
| Every new resolution step adds 2 functions | N/A | Adding a new resolution step (e.g., V4 PoolManager detection) means defining it in two places |

### What's already shared

`pool_class_for_descriptor()` is already a pure function shared by both paths. It's the model for the new extractions.

## Solution

### Approach: Keep separate protocols, extract shared logic into pure functions

The root cause of the duplication is that `PoolIO.call()` is sync and `AsyncPoolIOProtocol.call()` is async. The `await` keyword can't be conditionally applied, so some wrapper duplication is unavoidable. But the *logic* around each I/O step can be defined once.

**Approach A (unified async protocol)** — making `PoolIO.call()` async too would force `await` on all builder call sites, which contradicts the AGENTS.md ruling: *"Making `build()` async on all builders would force async on sync users."* The ruling is about builder `build()` methods specifically, but the same reasoning applies to the I/O protocol: forcing all PoolIO consumers to go async penalizes sync-only users for no benefit.

**Approach B (keep separate protocols, extract shared logic)** — define pure functions for the non-I/O steps, then have thin sync/async wrappers that:
1. Call the pure function (DB result → descriptor)
2. Call `io.call()` or `await io.call()` (I/O — the one line that differs)
3. Call the pure function (method name + factory → descriptor)

This replaces 4 functions (probing + resolve, sync + async) with 2 thin wrappers + shared pure logic. The `fetch_factory_from_chain` pair stays as-is.

### Extracted pure functions

#### `_build_descriptor_from_db_result(pool_from_db) -> PoolTypeDescriptor | None`

Maps a `LiquidityPoolTable` DB row to a `PoolTypeDescriptor`. This is the mapping logic currently duplicated at lines 247–263 (sync) and 301–317 (async). The DB session query itself is synchronous in both cases (SQLAlchemy), so it stays in the wrappers — this function just does the mapping.

```python
def _build_descriptor_from_db_result(
    pool_from_db: LiquidityPoolTable,
) -> PoolTypeDescriptor | None:
    """Pure logic — maps a DB row to a PoolTypeDescriptor.

    Returns None if the kind can't be resolved from the registry.
    """
    kind = pool_from_db.kind
    descriptor = pool_type_registry.get_descriptor_by_kind(kind)
    if descriptor is not None:
        return PoolTypeDescriptor(
            family=descriptor.family,
            variant=descriptor.variant,
            kind=descriptor.kind,
            factory=get_checksum_address(pool_from_db.exchange.factory),
        )
    return None
```

**Dependency on `pool_type_registry`**: This function calls `pool_type_registry.get_descriptor_by_kind()`, a read-only lookup on a module-level mutable singleton. The function is "pure" in the sense that it does no I/O, but it has a read-only dependency on the registry state. Testing requires either registering test entries in the registry or mocking it. This is acceptable — `pool_class_for_descriptor` already has the same dependency.

#### `_descriptor_from_probing_result(*, succeeded_method: str | None, chain_id: ChainId, factory: ChecksumAddress) -> PoolTypeDescriptor`

Maps "which on-chain method succeeded" to a `PoolTypeDescriptor`. Includes the registry lookup that the current probing functions do inside their try/else blocks. If `succeeded_method` is `None` (no method succeeded), returns a STABLESWAP fallback.

```python
def _descriptor_from_probing_result(
    *,
    succeeded_method: str | None,
    chain_id: ChainId,
    factory: ChecksumAddress,
) -> PoolTypeDescriptor:
    """Pure logic — maps 'which method succeeded' to a PoolTypeDescriptor.

    If the factory is registered in pool_type_registry, uses the registry
    descriptor. Otherwise derives a default descriptor from the method name.
    """
    if succeeded_method is None:
        # No method succeeded — fallback to STABLESWAP
        return PoolTypeDescriptor(
            family=PoolFamily.STABLESWAP,
            variant=None,
            kind=derive_kind(PoolFamily.STABLESWAP, None),
            factory=factory,
        )

    registry_descriptor = pool_type_registry.get_descriptor(chain_id, factory)
    if registry_descriptor is not None:
        return registry_descriptor

    match succeeded_method:
        case "slot0":
            family = PoolFamily.CONCENTRATED_LIQUIDITY
        case "getReserves":
            family = PoolFamily.CONSTANT_PRODUCT
        case _:
            family = PoolFamily.STABLESWAP

    return PoolTypeDescriptor(
        family=family,
        variant=None,
        kind=derive_kind(family, None),
        factory=factory,
    )
```

This function handles *one* probing attempt's result. The probing wrappers call it after each I/O attempt, short-circuiting when a method succeeds. The name `succeeded_method` makes it explicit which call succeeded (e.g., `"slot0"`, `"getReserves"`).

### Restructured wrappers

After extraction, the probing functions become:

```python
def resolve_pool_type_by_probing(
    address: ChecksumAddress,
    *,
    chain_id: ChainId,
    factory: ChecksumAddress,
    io: PoolIO,
) -> PoolTypeDescriptor:
    """Determine pool type by probing the contract on-chain."""
    # Try V3: slot0()
    try:
        io.call(to=address, data=encode_function_calldata("slot0()", None))
    except Web3Exception:
        pass
    else:
        return _descriptor_from_probing_result(
            succeeded_method="slot0", chain_id=chain_id, factory=factory,
        )

    # Try V2: getReserves()
    try:
        io.call(to=address, data=encode_function_calldata("getReserves()", None))
    except Web3Exception:
        pass
    else:
        return _descriptor_from_probing_result(
            succeeded_method="getReserves", chain_id=chain_id, factory=factory,
        )

    # Fallback: STABLESWAP
    return _descriptor_from_probing_result(
        succeeded_method=None, chain_id=chain_id, factory=factory,
    )


async def resolve_pool_type_by_probing_async(
    address: ChecksumAddress,
    *,
    chain_id: ChainId,
    factory: ChecksumAddress,
    io: AsyncPoolIOProtocol,
) -> PoolTypeDescriptor:
    """Determine pool type by probing the contract on-chain (async)."""
    # Try V3: slot0()
    try:
        await io.call(to=address, data=encode_function_calldata("slot0()", None))
    except Web3Exception:
        pass
    else:
        return _descriptor_from_probing_result(
            succeeded_method="slot0", chain_id=chain_id, factory=factory,
        )

    # Try V2: getReserves()
    try:
        await io.call(to=address, data=encode_function_calldata("getReserves()", None))
    except Web3Exception:
        pass
    else:
        return _descriptor_from_probing_result(
            succeeded_method="getReserves", chain_id=chain_id, factory=factory,
        )

    # Fallback: STABLESWAP
    return _descriptor_from_probing_result(
        succeeded_method=None, chain_id=chain_id, factory=factory,
    )
```

The wrappers still have the try/except I/O structure (the `await` can't be factored out), but the *result interpretation* is concentrated in one place. Adding a new probing step (e.g., `getPoolId()` for Balancer) means adding one `try/else` block that calls `_descriptor_from_probing_result(succeeded_method="getPoolId", ...)` to both wrappers — no duplicated descriptor construction.

The top-level `resolve_pool_type` / `resolve_pool_type_async` similarly use `_build_descriptor_from_db_result` for the DB-mapping step, eliminating the duplicated descriptor construction there.

### Design decisions

- **Approach B over Approach A**: The AGENTS.md ruling rejects forcing async on sync users for builders. The same reasoning extends to the I/O protocol: making `PoolIO.call()` async would force `await` on all builder call sites, penalizing sync users. Approach B respects this.
- **Keep `resolve_pool_type` and `resolve_pool_type_async` as module-level functions**: Don't force callers to instantiate a class. The shared pure logic lives as module-level functions, and the existing module-level functions delegate to them.
- **Leave `fetch_factory_from_chain` / `fetch_factory_from_chain_async` as-is**: Each is 17 lines with ~2 differing lines. The extraction overhead exceeds the benefit. These already have independent callers (V4 fast-path) so they need to remain as standalone functions regardless.
- **`_build_descriptor_from_db_result` takes the DB result object, not the session**: The DB session query is synchronous in both sync and async paths (SQLAlchemy). This function just maps `LiquidityPoolTable → PoolTypeDescriptor | None`, making it trivially testable with a mock/fake row object.
- **`_descriptor_from_probing_result` includes the registry lookup**: The current probing code calls `pool_type_registry.get_descriptor(chain_id, factory)` inside each `else` branch. This lookup is pure logic and should be in the shared function. The `succeeded_method` parameter tells the function which method succeeded, and it checks the registry first (using the factory), falling back to a default by method name.
- **`pool_type_registry` singleton dependency**: The pure functions call `pool_type_registry.get_descriptor()` and `pool_type_registry.get_descriptor_by_kind()`, which are read-only lookups on a module-level mutable singleton. Tests need to register entries or mock the singleton. This matches `pool_class_for_descriptor`'s existing pattern.

### Updated function count

| Before | After |
|--------|-------|
| 6 functions (3 sync/async pairs) | 6 total: 2 new pure functions + 2 thin sync/async wrappers (probing + resolve) + 2 unchanged (factory fetch) |
| ~220 lines of duplicated logic | ~40 lines of shared pure logic + ~90 lines of wrappers + ~34 lines unchanged = ~164 lines (~56 lines saved) |

## Files Involved

**Primary:**
- `src/degenbot/builders/type_resolution.py` — extract 2 pure functions; collapse 4 functions into 2 thin wrappers + shared logic; leave factory pair as-is

**Secondary:**
- `src/degenbot/bot.py` — no change (already calls `resolve_pool_type`)
- `src/degenbot/async_bot.py` — no change (already calls `resolve_pool_type_async`)
- `tests/builders/test_type_resolution.py` — add tests for new pure functions

**No change needed:**
- `src/degenbot/builders/pool_io.py` — PoolIO and AsyncPoolIOProtocol remain separate (per the ruling)
- Callers of the type_resolution functions — no API change, same function signatures

## Implementation Order

### Slice 1: Extract `_build_descriptor_from_db_result` and `_descriptor_from_probing_result` as pure functions

1. Extract `_build_descriptor_from_db_result(pool_from_db) -> PoolTypeDescriptor | None` from `resolve_pool_type` and `resolve_pool_type_async` (lines 247–263, 301–317)
2. Extract `_descriptor_from_probing_result(succeeded_method, chain_id, factory) -> PoolTypeDescriptor` from `resolve_pool_type_by_probing` and its async twin (the `else` branches in lines 143–149, 157–163, 203–209, 217–223)
3. Write tests for these pure functions — no I/O needed. Tests for `_descriptor_from_probing_result` must cover: slot0 → CONCENTRATED_LIQUIDITY, getReserves → CONSTANT_PRODUCT, None → STABLESWAP, factory-in-registry → registry descriptor (requires registering a test entry or mocking `pool_type_registry`)
4. Run: `just test-python` — expect all green

### Slice 2: Collapse probing functions

1. Rewrite `resolve_pool_type_by_probing()` to call `_descriptor_from_probing_result()` after each I/O attempt instead of constructing `PoolTypeDescriptor` inline
2. Rewrite `resolve_pool_type_by_probing_async()` similarly
3. Verify existing tests pass
4. Run: `just test-python` — expect all green

### Slice 3: Collapse top-level resolution functions

1. Rewrite `resolve_pool_type()` to call `_build_descriptor_from_db_result()` for the DB-mapping step and the shared probing helper via `resolve_pool_type_by_probing()`
2. Rewrite `resolve_pool_type_async()` similarly
3. Verify existing tests pass
4. Run: `just test-python` — expect all green

### Slice 4: Validate and clean up

1. Run `just lint` + `just test-all`
2. Verify `type_resolution.py` line count dropped by ~56 lines (duplicated descriptor construction removed from wrappers)
3. Confirm that `fetch_factory_from_chain` / `fetch_factory_from_chain_async` remain as the only untouched sync/async pair
4. Update relevant `CONTEXT.md` files to note the extracted pure functions

## Testing

### Per-slice test runs

Each slice runs `just test-python`. Existing type resolution tests must pass after each slice.

### New unit tests

```python
# tests/builders/test_type_resolution_pure.py

from unittest.mock import MagicMock
from degenbot.builders.type_resolution import (
    _build_descriptor_from_db_result,
    _descriptor_from_probing_result,
)
from degenbot.types.pool_type import PoolFamily, PoolTypeDescriptor


def test_build_descriptor_from_db_result_returns_descriptor():
    """_build_descriptor_from_db_result maps DB row to PoolTypeDescriptor."""
    mock_row = MagicMock()
    mock_row.kind = "uniswap_v2"
    mock_row.exchange.factory = "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"
    # Note: requires a matching pool_type_registry entry for this kind,
    # or a mock of pool_type_registry.get_descriptor_by_kind()
    result = _build_descriptor_from_db_result(mock_row)
    assert result is not None
    assert result.family == PoolFamily.CONSTANT_PRODUCT


def test_build_descriptor_from_db_result_returns_none_for_unknown_kind():
    """_build_descriptor_from_db_result returns None if kind isn't in registry."""
    mock_row = MagicMock()
    mock_row.kind = "nonexistent_kind"
    result = _build_descriptor_from_db_result(mock_row)
    assert result is None


def test_descriptor_from_probing_result_slot0():
    """slot0() succeeding maps to CONCENTRATED_LIQUIDITY."""
    result = _descriptor_from_probing_result(
        succeeded_method="slot0",
        chain_id=1,
        factory="0xUnknownFactory",  # type: ignore[arg-type]
    )
    assert result.family == PoolFamily.CONCENTRATED_LIQUIDITY


def test_descriptor_from_probing_result_getreserves():
    """getReserves() succeeding maps to CONSTANT_PRODUCT."""
    result = _descriptor_from_probing_result(
        succeeded_method="getReserves",
        chain_id=1,
        factory="0xUnknownFactory",  # type: ignore[arg-type]
    )
    assert result.family == PoolFamily.CONSTANT_PRODUCT


def test_descriptor_from_probing_result_fallback():
    """No method succeeding maps to STABLESWAP."""
    result = _descriptor_from_probing_result(
        succeeded_method=None,
        chain_id=1,
        factory="0xUnknownFactory",  # type: ignore[arg-type]
    )
    assert result.family == PoolFamily.STABLESWAP


def test_descriptor_from_probing_result_prefers_registry():
    """When factory is registered, returns registry descriptor regardless of method."""
    # Requires a registered factory in pool_type_registry
    # Use a known-registered factory (e.g., UniswapV2 factory on chain 1)
    factory = "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"
    result = _descriptor_from_probing_result(
        succeeded_method="slot0",
        chain_id=1,
        factory=factory,  # type: ignore[arg-type]
    )
    # Registry descriptor should take precedence over method-based default
    assert result is not None
```

### Integration tests

Existing `tests/builders/test_type_resolution.py` and `tests/test_bot.py` / `tests/test_async_bot.py` exercise type resolution end-to-end through `build_pool()`. These remain unchanged and must continue to pass.

## Benefits

- **Locality**: Pure-logic steps for type resolution concentrate in one place; add a new resolution step once (in `_descriptor_from_probing_result`), not twice
- **Leverage**: One set of pure functions serves both sync and async paths
- **Depth**: The 4 mirrored functions (probing + resolve, sync + async) become 2 thin wrappers + 2 deep pure functions. The pure functions hide the variant-detection complexity; the wrappers just dispatch I/O.
- **Testability**: The pure functions are testable without I/O. `_build_descriptor_from_db_result` takes a mockable DB row object; `_descriptor_from_probing_result` takes a string.

## Risks

- **Approach B doesn't fully eliminate duplication**: The sync/async thin wrappers still exist as separate functions — the `await` keyword can't be conditionally applied. Each wrapper is now ~30–45 lines (I/O dispatch + call to pure logic), down from 55+ lines. The `fetch_factory_from_chain` pair remains duplicated (17 lines each).
- **`pool_type_registry` singleton in pure functions**: The pure functions call `pool_type_registry.get_descriptor()` and `pool_type_registry.get_descriptor_by_kind()`. Tests must either register test entries in the singleton or mock the calls. This matches `pool_class_for_descriptor`'s existing dependency.
- **Adding new probing steps still requires two edits**: Adding a new on-chain method (e.g., `getPoolId()` for Balancer) means adding a `try/else` block to both the sync and async probing wrappers. The reward per new step is smaller — each wrapper gains ~6 lines instead of ~15 (no duplicated `PoolTypeDescriptor` construction). A future plan could further reduce this by parameterizing the probing steps as a list of `(method_name, calldata)` pairs, but that adds indirection for marginal gain.

## Relationship to Other Plans

- **Plan 065** (AsyncBot inline I/O): Orthogonal — different module, but both reduce duplication between sync/async paths.
- **Plan 067** (BuildPoolRequest): Complementary — after this plan, `build_pool()` calls one resolver instead of code that branches on sync/async; the kwargs tunnel refactor is simpler.
- **Plan 048** (Async Builder Shared): This extends Plan 048's approach of sharing pure-logic helpers between sync/async paths, applying it to type resolution.
- **Plan 070** (Balancer Builder): If Balancer probing is added, this plan's `_descriptor_from_probing_result` function is the single place to add a new `succeeded_method` case.

## Status

[x] Slice 1: Extract `_build_descriptor_from_db_result` and `_descriptor_from_probing_result` pure functions
[x] Slice 2: Collapse probing functions
[x] Slice 3: Collapse top-level resolution functions
[x] Slice 4: Validate and clean up
