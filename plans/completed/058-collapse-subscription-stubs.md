# Plan 058: Collapse Subscription Stubs in Provider Adapters

## Overview

Extract subscription capability from `ProviderBackend` into a separate opt-in protocol with default "raise `SubscriptionNotSupported`" implementations, removing 20 identical stub methods from sync adapters and `ProviderAdapter`. The sync `ProviderBackend` protocol shrinks from 17+ methods to 12; only the async `AsyncProviderAdapter` exposes subscription methods when its backend satisfies the new protocol.

## Problem

### Deletion test

If you deleted all 5 `subscribe_*` stub methods from every sync adapter (`_Web3Adapter`, `_AlloyAdapter`, `_OfflineAdapter`) and from `ProviderAdapter`, the subscription methods would vanish from sync providers entirely. This is correct behavior — sync providers don't support subscriptions. The `ProviderBackend` protocol currently *requires* these methods, forcing every new adapter to re-implement the same 5 stubs. The complexity doesn't vanish on deletion because the protocol definition would be violated — it shifts to every future adapter author.

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|--------------|
| 20 identical "raise SubscriptionNotSupported" method bodies | `provider/interface.py` lines 105–140 (protocol), 214–237 (_Web3Adapter), 306–327 (_AlloyAdapter), 390–410 (_OfflineAdapter), 729–770 (ProviderAdapter) | A reader scanning the file sees the same 3-line pattern repeated 20 times. Each stub is individually testable but collectively zero-value — they all do the same thing. |
| Protocol too wide | `ProviderBackend` protocol | The protocol declares 17+ methods, mixing "things every backend can do" (call, get_balance) with "things only async WS backends can do" (subscribe_blocks). A reader can't tell which methods are load-bearing for a given backend. |
| Adding a new sync backend requires 5 stubs | Any new `_FooAdapter` | Every new sync backend adapter must implement 5 subscribe stubs or be structurally invalid. This is interface tax, not depth. |
| Async protocol also repeats the stub pattern | `_AsyncWeb3Adapter` lines 912–930 | 5 more stub methods, this time returning `Subscription` type but still raising. The only real implementations live in `_AsyncAlloyAdapter`. |

## Solution

### Step 1: Define `SyncSubscriptionSupport` mixin with default stubs

Create a mixin class that provides default "raise `SubscriptionNotSupported`" implementations for all 5 subscribe methods. Sync adapters inherit from this mixin instead of implementing stubs themselves.

```python
class SyncSubscriptionSupport:
    """Mixin providing default subscription stubs for sync backends.

    Sync providers never support subscriptions. This mixin satisfies
    the protocol requirement without duplicating 5 identical stubs.
    """

    def subscribe_blocks(self) -> None:
        raise SubscriptionNotSupported(transport="sync", rpc_url=getattr(self, "_rpc_url", "unknown"))

    def subscribe_full_blocks(self) -> None:
        raise SubscriptionNotSupported(transport="sync", rpc_url=getattr(self, "_rpc_url", "unknown"))

    def subscribe_pending_transactions(self) -> None:
        raise SubscriptionNotSupported(transport="sync", rpc_url=getattr(self, "_rpc_url", "unknown"))

    def subscribe_full_pending_transactions(self) -> None:
        raise SubscriptionNotSupported(transport="sync", rpc_url=getattr(self, "_rpc_url", "unknown"))

    def subscribe_logs(self, _addresses=None, _topics=None) -> None:
        raise SubscriptionNotSupported(transport="sync", rpc_url=getattr(self, "_rpc_url", "unknown"))
```

### Step 2: Define `AsyncSubscriptionSupport` mixin for async non-WS backends

```python
class AsyncSubscriptionSupport:
    """Mixin providing default subscription stubs for async backends
    that don't support WS/IPC subscriptions (e.g. AsyncWeb3 over HTTP).
    """

    async def subscribe_blocks(self) -> Subscription:
        raise SubscriptionNotSupported(transport="web3", rpc_url=getattr(self, "_rpc_url", "unknown"))

    # ... same pattern for the other 4 methods
```

### Step 3: Remove stub methods from sync adapters and ProviderAdapter

Each sync adapter (`_Web3Adapter`, `_AlloyAdapter`, `_OfflineAdapter`) inherits from `SyncSubscriptionSupport` and deletes its 5 stub methods. `ProviderAdapter` also inherits the mixin and deletes its 5 forwarding stubs.

### Step 4: Remove stub methods from async adapters

`_AsyncWeb3Adapter` inherits `AsyncSubscriptionSupport` and deletes its 5 stub methods. `_AsyncAlloyAdapter` keeps its real subscription implementations (it's the only adapter that actually supports them).

### Step 5: Keep `ProviderBackend` protocol unchanged (for now)

The protocol still declares the 5 subscribe methods. This is intentional — the mixin satisfies the requirement structurally. A future plan could remove them from the protocol and use `hasattr` / `isinstance` checks instead, but that's a broader interface change. This plan focuses on eliminating duplication, not narrowing the protocol.

### Design decisions

- **Mixin, not Protocol split**: Splitting `ProviderBackend` into `ProviderBackend` + `SubscriptionCapable` would require `isinstance` checks at every call site. A mixin with default stubs is simpler and backward-compatible — every existing `ProviderBackend` still satisfies the protocol after the change.
- **Keep protocol wide for now**: Removing subscribe methods from `ProviderBackend` would be a breaking change for any external code that uses the protocol structurally. The mixin approach is additive and non-breaking.
- **Unified error message via `_rpc_url` attribute**: The stub methods need a URL for the error message. Rather than hardcoding per-adapter, each adapter sets `self._rpc_url` (or similar attribute) and the mixin reads it. Existing adapters already have the URL available internally; expose it.
- **No changes to `AsyncProviderAdapter`**: `AsyncProviderAdapter`'s subscription methods are forwarding methods (delegating to the backend), not stubs. They stay as-is.

## Files Involved

**Primary:**
- `src/degenbot/provider/interface.py` — add 2 mixin classes, remove 25 stub methods from 4 adapters and 1 adapter class, update adapter `__init__` to inherit mixins

**Secondary:**
- `src/degenbot/provider/__init__.py` — add new mixin names to `__all__` if they are public (likely not — they're internal implementation details)
- `tests/provider/` — remove any tests asserting "subscribe raises on sync provider" per-adapter; replace with single test against the mixin

**No change needed:**
- `src/degenbot/connection/connection_manager.py` — doesn't call subscribe methods
- `src/degenbot/bot.py` — uses `AsyncProviderAdapter.subscribe_*()` directly, which is unaffected
- `src/degenbot/provider/offline_provider.py` — `OfflineProvider` class is unrelated to the adapter stubs

## Implementation Order

### Slice 1: Add `SyncSubscriptionSupport` mixin, apply to `_Web3Adapter`

1. Define `SyncSubscriptionSupport` in `interface.py` with 5 default stub methods
2. Add `_rpc_url` attribute or property to `_Web3Adapter` (extract from `self._w3.provider`)
3. Make `_Web3Adapter` inherit `SyncSubscriptionSupport`, delete its 5 subscribe stub methods
4. Run: `just test-python` — expect all tests green (mixin satisfies protocol structurally)

### Slice 2: Apply `SyncSubscriptionSupport` to remaining sync adapters

1. Make `_AlloyAdapter` inherit `SyncSubscriptionSupport`, delete its 5 subscribe stubs
2. Make `_OfflineAdapter` inherit `SyncSubscriptionSupport`, delete its 5 subscribe stubs
3. Make `ProviderAdapter` inherit `SyncSubscriptionSupport`, delete its 5 forwarding stubs
4. Run: `just test-python` — expect all tests green

### Slice 3: Add `AsyncSubscriptionSupport` mixin, apply to `_AsyncWeb3Adapter`

1. Define `AsyncSubscriptionSupport` in `interface.py` with 5 default async stub methods
2. Make `_AsyncWeb3Adapter` inherit `AsyncSubscriptionSupport`, delete its 5 subscribe stub methods
3. Run: `just test-python` — expect all tests green

### Slice 4: Validate and clean up

1. Run `just lint` + `just test-all`
2. Run `grep -c "raise SubscriptionNotSupported" src/degenbot/provider/interface.py` — expect count reduced from 20 to ~12 (5 sync mixin + 5 async mixin + 2 real raises)
3. Verify no adapter class still defines its own subscribe stubs
4. Update `src/degenbot/provider/CONTEXT.md` (if it exists) or `src/degenbot/connection/CONTEXT.md` with mixin terminology

## Testing

### Per-slice test runs

Each slice runs `just test-python`. The mixin satisfies the protocol structurally, so existing protocol-compliance tests should pass unchanged.

### New unit tests

```python
# tests/provider/test_subscription_support.py


def test_sync_subscription_support_raises():
    """SyncSubscriptionSupport mixin raises SubscriptionNotSupported for all subscribe methods."""
    support = SyncSubscriptionSupport()
    for method_name in ["subscribe_blocks", "subscribe_full_blocks", "subscribe_pending_transactions", "subscribe_full_pending_transactions", "subscribe_logs"]:
        with pytest.raises(SubscriptionNotSupported):
            getattr(support, method_name)()


def test_async_subscription_support_raises():
    """AsyncSubscriptionSupport mixin raises SubscriptionNotSupported for all subscribe methods."""
    support = AsyncSubscriptionSupport()
    for method_name in ["subscribe_blocks", "subscribe_full_blocks", "subscribe_pending_transactions", "subscribe_full_pending_transactions", "subscribe_logs"]:
        with pytest.raises(SubscriptionNotSupported):
            asyncio.get_event_loop().run_until_complete(getattr(support, method_name)())
```

### Integration tests

Existing tests for `AsyncProviderAdapter.subscribe_*()` (driven by `Bot.start_listening()`) cover the real subscription path. The stub path is covered by the new unit tests above.

## Benefits

- **Locality**: Subscription stub logic lives in one place (the mixin). A change to the error message format or exception type touches one class, not 6.
- **Leverage**: `ProviderBackend` protocol effectively shrinks from 17+ required methods to 12 for practical purposes. New sync adapters inherit 5 stubs automatically instead of copy-pasting.
- **Depth**: The adapter classes become shallower in a good way — they expose only the methods that vary per backend, not the ones that are identical everywhere.

## Risks

- **Protocol structural check**: If any code uses `isinstance(adapter, ProviderBackend)` with `runtime_checkable`, the mixin inheritance must preserve protocol compliance. Mitigation: the mixin provides all 5 methods, so structural checks pass.
- **`_rpc_url` attribute**: The mixin needs an RPC URL for error messages. Adapters that don't expose one need a fallback. Mitigation: use `getattr(self, "_rpc_url", "unknown")` with a sensible default.
- **No functional change**: This is a refactoring plan. The stubs still raise the same exceptions. The only observable change is a potentially different `rpc_url` string in the exception message.

## Relationship to Other Plans

- **Plan 042** (Collapse Provider Adapter Mirror): Completed. Merged `EthereumProvider` + `_SyncProviderBackend` → `ProviderBackend`. This plan continues that consolidation by removing the stub duplication that survived the merge. The `EthereumProvider` backward-compatibility alias was removed by Plan 061.
- **Plan 046** (eth_subscribe Support): Completed. Added subscription methods. This plan cleans up the stub pattern that Plan 046 introduced across all adapters.
- **Plan 047** (Event-Driven Log Listener): Completed. Uses `AsyncProviderAdapter.subscribe_*()`. Orthogonal — this plan doesn't change the async subscription API surface.
- **Plan 059** (Delete Deprecated `build_*` Pass-Throughs from Bot): Orthogonal. Different module.
- **Plan 060** (Unify Sync/Async Builder Orchestration): Orthogonal. Different module.

## Status

[x] Slice 1: Add `SyncSubscriptionSupport` mixin, apply to `_Web3Adapter`
[x] Slice 2: Apply `SyncSubscriptionSupport` to remaining sync adapters and `ProviderAdapter`
[x] Slice 3: Add `AsyncSubscriptionSupport` mixin, apply to `_AsyncWeb3Adapter`
[x] Slice 4: Validate and clean up
