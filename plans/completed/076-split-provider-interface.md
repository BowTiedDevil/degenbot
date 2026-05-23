# Plan 076: Split provider/interface.py into focused modules

## Overview

Split the 1630-line `provider/interface.py` into three focused files along the sync/async boundary: a pure protocols file, a sync adapters file (with its subscription mixin), and an async adapters file (with its subscription mixin). Delete `interface.py` and update all import sites.

## Problem

### Deletion test

If you deleted `interface.py`, all 11 classes would need to resurface somewhere — the module earns its keep by hiding adapter internals behind the `ProviderAdapter`/`AsyncProviderAdapter` factory methods. But the sync and async adapter hierarchies are orthogonal. Co-locating them forces a reader to scroll through the async code to understand the sync path (and vice versa).

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|--------------|
| 1630-line file | `provider/interface.py` | IDE navigation, grep, blame all degraded |
| Sync + async interleaved | Protocols & mixins at top, sync adapters middle, async protocol + adapters below | Understanding one path requires scrolling past the other |
| Protocols buried | `ProviderBackend` near top, `AsyncProviderBackend` in the middle of the file | A protocol user must find the seam inside the adapter file |
| Six private classes co-located | `_Web3Adapter`, `_AlloyAdapter`, `_OfflineAdapter`, `_AsyncWeb3Adapter`, `_AsyncAlloyAdapter`, two subscription mixins | Reader cannot tell at a glance which private class belongs to which adapter |

## Solution

### Step 1: Create `provider/protocols.py`

Extract the two public `Protocol` classes only. These are the seams — callers need them, not the adapters.

```python
# provider/protocols.py
@runtime_checkable
class ProviderBackend(Protocol):
    ...

@runtime_checkable
class AsyncProviderBackend(Protocol):
    ...
```

Pure interfaces — no implementation logic.

### Step 2: Create `provider/sync_adapter.py`

Move `SyncSubscriptionSupport`, `_Web3Adapter`, `_AlloyAdapter`, `_OfflineAdapter`, `ProviderAdapter`, and the `_backend_for_type` helper.

```python
# provider/sync_adapter.py
from degenbot.provider.protocols import ProviderBackend

class SyncSubscriptionSupport:
    ...

class _Web3Adapter(SyncSubscriptionSupport):
    ...

class _AlloyAdapter(SyncSubscriptionSupport):
    ...

class _OfflineAdapter(SyncSubscriptionSupport):
    ...

class ProviderAdapter(SyncSubscriptionSupport):
    ...

def _backend_for_type(...) -> ProviderBackend:
    ...
```

The sync mixin lives with its consumers — locality.

### Step 3: Create `provider/async_adapter.py`

Move `AsyncSubscriptionSupport`, `_AsyncWeb3Adapter`, `_AsyncAlloyAdapter`, `AsyncProviderAdapter`.

```python
# provider/async_adapter.py
from degenbot.provider.protocols import AsyncProviderBackend

class AsyncSubscriptionSupport:
    ...

class _AsyncWeb3Adapter(AsyncSubscriptionSupport):
    ...

class _AsyncAlloyAdapter:
    ...

class AsyncProviderAdapter:
    ...
```

The async mixin lives with its single consumer (`_AsyncWeb3Adapter`). `_AsyncAlloyAdapter` has no mixin — it implements real subscription methods directly (it wraps a WS/IPC-capable `AsyncAlloyProvider`).

### Step 4: Delete `provider/interface.py` and update all imports

Remove `interface.py`. Update `provider/__init__.py` to import directly from the three new files. Update all ~22 call sites and 3 test files to import from the new locations.

### Design decisions

- **Hard cutover, no shim**: No backwards-compatibility layer. `interface.py` is deleted. All import sites updated mechanically. Per AGENTS.md: "design standalone features without a backwards compatibility layer."
- **Protocols in their own file, mixins with their adapters**: `protocols.py` contains only the two `Protocol` classes — pure seams, no implementation. The subscription mixins go with their respective adapter files because each mixin is only consumed by adapters in its domain. This makes `protocols.py` scannable in ~120 lines and keeps adapter files self-contained.
- **`_AsyncAlloyAdapter` preserved as-is**: It doesn't inherit `AsyncSubscriptionSupport` because it implements real subscription methods (its backend genuinely supports WS/IPC). This asymmetry reflects a real capability difference, not a design flaw. No new mixin.
- **`_backend_for_type` stays with sync_adapter**: It's only used by `ProviderAdapter.set_provider()`. Not needed by async code.

## Files Involved

**Primary:**
- `src/degenbot/provider/protocols.py` — **new**: `ProviderBackend`, `AsyncProviderBackend`
- `src/degenbot/provider/sync_adapter.py` — **new**: `SyncSubscriptionSupport`, `_Web3Adapter`, `_AlloyAdapter`, `_OfflineAdapter`, `ProviderAdapter`, `_backend_for_type`
- `src/degenbot/provider/async_adapter.py` — **new**: `AsyncSubscriptionSupport`, `_AsyncWeb3Adapter`, `_AsyncAlloyAdapter`, `AsyncProviderAdapter`
- `src/degenbot/provider/interface.py` — **deleted**

**Secondary (import path updates):**
- `src/degenbot/provider/__init__.py` — change imports from `interface.py` to new files
- `src/degenbot/bot.py` — `from degenbot.provider.interface import ...` → new paths
- `src/degenbot/async_bot.py` — same
- `src/degenbot/builders/pool_io.py` — same
- `src/degenbot/provider/log_fetching.py` — same
- `src/degenbot/provider/call_helpers.py` — same
- `src/degenbot/provider/block_helpers.py` — same
- `src/degenbot/contract/__init__.py` — same
- `src/degenbot/uniswap/v4_snapshot.py` — same
- `src/degenbot/uniswap/v3_snapshot.py` — same
- `src/degenbot/aerodrome/pools.py` — same
- `src/degenbot/aave/analysis/orchestrator.py` — same
- `src/degenbot/cli/aave/*.py` — same (multiple files)
- `tests/provider/test_subscription_support.py` — same
- `tests/provider/test_provider_backend.py` — same
- `tests/provider/test_backend_adapters.py` — same

**New:**
- `src/degenbot/provider/CONTEXT.md` — module context document

## Implementation Order

### Slice 1: Create all three new files + delete interface.py + update imports

1. Create `src/degenbot/provider/protocols.py` with `ProviderBackend` and `AsyncProviderBackend`
2. Create `src/degenbot/provider/sync_adapter.py` with `SyncSubscriptionSupport`, `_Web3Adapter`, `_AlloyAdapter`, `_OfflineAdapter`, `ProviderAdapter`, `_backend_for_type`
3. Create `src/degenbot/provider/async_adapter.py` with `AsyncSubscriptionSupport`, `_AsyncWeb3Adapter`, `_AsyncAlloyAdapter`, `AsyncProviderAdapter`
4. Delete `src/degenbot/provider/interface.py`
5. Update `src/degenbot/provider/__init__.py` to import from the three new files
6. Update all ~22 source import sites from `degenbot.provider.interface` to the new file paths
7. Update all 3 test file imports
8. Verify `TYPE_CHECKING` / runtime import correctness: `async_adapter.py` uses `AsyncAlloyProvider` only in type annotations — confirm no `NameError` at runtime (the import must remain behind `if TYPE_CHECKING:` with `from __future__ import annotations` ensuring runtime safety)
9. Run: `just test-python` — all tests pass
10. Run: `just lint` — no new warnings

### Slice 2: Validate and create CONTEXT.md

1. Run `just lint` + `just test-all`
2. Verify no remaining references to `degenbot.provider.interface` anywhere (grep)
3. Create `src/degenbot/provider/CONTEXT.md` documenting the module structure
4. Run: `just test-all` — all tests pass

## Testing

### Per-slice test runs

Each slice runs `just test-python`. The mechanical import updates in Slice 1 are validated by the full test suite — if imports are wrong, the test runner won't even collect.

### New unit tests

No new tests needed. Existing tests cover:
- `ProviderAdapter.from_alloy()`, `from_web3()`, protocol conformance (`tests/rust/test_provider_interface.py`)
- `AsyncProviderAdapter` (covered via subscription tests)
- Pickle round-trip
- All backend adapter delegation (`tests/provider/test_backend_adapters.py`)
- Subscription support stubs (`tests/provider/test_subscription_support.py`)

The refactoring is pure file reorganization with zero logic changes.

### Integration tests

All ~22 import sites serve as integration coverage. If the imports resolve, they pass.

## Benefits

- **Locality**: Understanding the sync adapter path requires reading one file, not interleaved sync+async
- **Leverage**: Protocol file is pure seams — callers who only need `ProviderBackend` import just that, with no adapter noise
- **Depth**: No change — this is a co-location fix, not a deepening

## Risks

- **Import cycle**: `protocols.py` imports nothing from `degenbot.provider.*`; `sync_adapter.py` and `async_adapter.py` each import from `protocols.py` only. No cycle possible.
- **TYPE_CHECKING guard**: `async_adapter.py` references `AsyncAlloyProvider` only as a type annotation. The `from __future__ import annotations` import and `TYPE_CHECKING` guard must be preserved to avoid `NameError` at runtime. Verified explicitly in Slice 1 step 8.

## Relationship to Other Plans

- **Plan 077** (CurveStableswapPool cache extraction): Orthogonal — different module, different concern.
- **Plan 078** (Curve InputResolver): Orthogonal — different module, different concern.

## Status

[x] Slice 1: Create files, delete interface.py, update imports
[x] Slice 2: Validate and create CONTEXT.md
