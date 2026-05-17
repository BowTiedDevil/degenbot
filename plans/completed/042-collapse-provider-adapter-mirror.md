# Plan 042: Collapse the Provider Adapter Mirror Hierarchy

## Overview

Replace the 5 hand-written backend adapter classes and 2 near-identical protocols in `provider/interface.py` with a single backend protocol + `__getattr__` dispatch on the adapters. Each adapter remains (they have real logic differences), but the 15 identical delegation methods on `ProviderAdapter` and `AsyncProviderAdapter` collapse to a single forwarding mechanism. The two near-identical protocols (`EthereumProvider` and `_SyncProviderBackend`) merge into one.

## Files Involved

**Primary:**
- `src/degenbot/provider/interface.py` (914 lines → ~550 lines) — merge protocols, collapse delegation methods, add `__getattr__` dispatch
- `src/degenbot/provider/offline_provider.py` — may need minor adjustments if `OfflineProvider` interface changes

**Secondary:**
- `src/degenbot/connection/connection_manager.py` — no change (uses `ProviderAdapter` which keeps its public API)
- `src/degenbot/builders/*.py` — no change (builders use `ProviderAdapter` which keeps its public API)
- `tests/provider/` — verify `ProviderAdapter` public API unchanged after refactor

## Problem

### Deletion test

If you deleted `_Web3Adapter`, `_AlloyAdapter`, and `_OfflineAdapter`, the complexity would reappear as conditional logic inside `ProviderAdapter` — but it would be less organized. The adapters provide real value: they translate between different backend APIs (web3.py, Alloy, Offline). They earn their keep.

But if you deleted the *delegation methods* on `ProviderAdapter` (e.g., `get_code`, `get_balance`, `get_storage_at`), each would reappear as `self._backend.get_code(...)` at every call site. The delegation is pure boilerplate — it doesn't earn its keep.

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|--------------|
| **5 adapter classes, ~650 lines of boilerplate** | `provider/interface.py` | `_Web3Adapter`, `_AlloyAdapter`, `_OfflineAdapter`, `_AsyncWeb3Adapter`, `_AsyncAlloyAdapter` — each re-implements the same 10 methods |
| **2 near-identical protocols** | `EthereumProvider` and `_SyncProviderBackend` | One has `close()` and `call_raw()`, the other doesn't. Otherwise identical. |
| **15 delegation methods × 2 adapters** | `ProviderAdapter` + `AsyncProviderAdapter` | `call()`, `get_code()`, `get_balance()`, `get_storage_at()`, `get_transaction_count()`, `get_logs()`, `get_block()`, `get_block_number()`, `is_connected()`, `close()` — each is `return self._backend.method(...)` |
| **Adding a new RPC method = 7 edits** | Across all classes and protocols | `make_request()` was added as a one-off; the next method will need the same 7-file treatment |

### The actual differences

The three sync backends have real but small differences:

| Difference | `_Web3Adapter` | `_AlloyAdapter` | `_OfflineAdapter` |
|------------|----------------|------------------|---------------------|
| `call()` | `w3.eth.call(tx, block)` | `alloy.call(to, data, block_number=block)` | `offline.call(to, data, block_number=block)` |
| `call_raw()` | `w3.eth.call(tx, block)` | `alloy.call(to, data, block_number=block)` | `offline.call(tx["to"], tx["data"], block_number=block)` |
| `get_block()` | Direct pass-through | String→int conversion for block identifiers | Direct pass-through |
| `is_connected()` | `w3.is_connected()` | Always `True` | Always `True` |
| `close()` | Conditional `w3.close()` | Conditional `alloy.close()` | Conditional `offline.close()` |

Everything else is identical delegation.

## Solution

### Step 1: Merge `EthereumProvider` and `_SyncProviderBackend` into one protocol

```python
@runtime_checkable
class ProviderBackend(Protocol):
    """Protocol for sync provider backends.

    Replaces the former EthereumProvider (public) and
    _SyncProviderBackend (private) with a single protocol.
    """

    @property
    def chain_id(self) -> int: ...
    @property
    def block_number(self) -> int: ...
    def get_block_number(self) -> int: ...
    def get_block(self, block_identifier: int | str) -> dict[str, Any] | None: ...
    def get_logs(
        self,
        from_block: int,
        to_block: int,
        addresses: list[str] | None,
        topics: list[list[str]] | None,
    ) -> list[dict[str, Any]]: ...
    def call(self, to: str, data: bytes, block: int | None) -> HexBytes: ...
    def call_raw(self, tx: dict[str, Any], block: int | None) -> HexBytes: ...
    def get_code(self, address: str, block: int | None) -> HexBytes: ...
    def get_balance(self, address: str, block: int | None) -> int: ...
    def get_storage_at(self, address: str, position: int, block: int | None) -> HexBytes: ...
    def get_transaction_count(self, address: str, block: int | None) -> int: ...
    def is_connected(self) -> bool: ...
    def close(self) -> None: ...
```

This merges `EthereumProvider` (missing `call_raw` and `close`) with `_SyncProviderBackend`. The `call_raw` method is added to the public protocol since it's already exposed on `ProviderAdapter`.

### Step 2: Add `__getattr__` dispatch to `ProviderAdapter`

Instead of 15 explicit delegation methods, `ProviderAdapter` uses `__getattr__` to forward unknown attribute lookups to the backend:

```python
class ProviderAdapter:
    def __init__(
        self, backend: ProviderBackend, *, provider_type: str, raw_provider: Any = None
    ) -> None:
        self._backend = backend
        self._provider_type = provider_type
        self._raw_provider = raw_provider

    # Properties stay explicit (they don't match backend method names 1:1)
    @property
    def chain_id(self) -> int:
        return self._backend.chain_id

    @property
    def block_number(self) -> int:
        return self._backend.block_number

    # Methods with extra logic stay explicit
    def call(self, to: str, data: bytes, block: int | None = None) -> HexBytes:
        return self._backend.call(to, data, block)

    def call_raw(self, tx: dict[str, Any], block: int | None = None) -> HexBytes:
        return self._backend.call_raw(tx, block)

    def batch_call(self, calls: list[dict[str, Any]], block: int | None = None) -> list[HexBytes]:
        return [self._backend.call_raw(tx, block) for tx in calls]

    def get_block_timestamp(self, block: int | None = None) -> int:
        block_data = self._backend.get_block(block if block is not None else "latest")
        if block_data is None:
            msg = f"Block {block} not found"
            raise ValueError(msg)
        return block_data["timestamp"]

    def make_request(self, method: str, params: list[Any]) -> Any:
        if hasattr(self._raw_provider, "make_request"):
            return self._raw_provider.make_request(method, params)
        msg = f"Provider type '{self._provider_type}' does not support make_request"
        raise AttributeError(msg)

    # Pure delegation methods use __getattr__
    def __getattr__(self, name: str) -> Any:
        """Forward unknown attribute lookups to the backend.

        This provides delegation for methods that are pure pass-throughs:
        get_block_number, get_block, get_logs, get_code, get_balance,
        get_storage_at, get_transaction_count, is_connected, close.
        """
        if name.startswith("_"):
            raise AttributeError(name)
        return getattr(self._backend, name)
```

This eliminates ~80 lines of explicit delegation methods. The methods that have extra logic (`call`, `call_raw`, `batch_call`, `get_block_timestamp`, `make_request`) stay explicit.

**Alternative considered:** `__getattr__` is magical and can confuse type checkers. A more explicit alternative is a single `_delegate` method + explicit thin wrappers that call it. But this adds more boilerplate than it removes.

**Mitigation:** The `__getattr__` is clearly documented, and the type checker sees the explicit methods on `ProviderAdapter`'s class body. The delegated methods are discoverable via `ProviderBackend` protocol. IDEs that follow `__getattr__` will also show the backend methods.

### Step 3: Keep the 3 backend adapters — they have real differences

`_Web3Adapter`, `_AlloyAdapter`, and `_OfflineAdapter` stay. They translate between different backend APIs (web3.py's `w3.eth.call(tx, block)` vs Alloy's `alloy.call(to, data, block_number=block)`). This is real translation logic, not boilerplate.

However, they can be simplified by removing the redundant `block is not None` checks. Web3.py and Alloy both accept `None` for "latest block" — the guards are unnecessary:

```python
# Before:
def get_code(self, address: str, block: int | None) -> HexBytes:
    if block is not None:
        return self._w3.eth.get_code(address, block)
    return self._w3.eth.get_code(address)


# After:
def get_code(self, address: str, block: int | None) -> HexBytes:
    return self._w3.eth.get_code(address, block)
```

This simplification removes ~30 lines of `if block is not None` guards across the adapters.

### Step 4: Apply the same treatment to `AsyncProviderAdapter`

`_AsyncProviderBackend` protocol + `_AsyncWeb3Adapter` + `_AsyncAlloyAdapter` + `AsyncProviderAdapter` follow the same pattern. Merge the protocol, add `__getattr__`, simplify the block guards.

### Step 5: Update `__all__` and deprecation warnings

`EthereumProvider` is re-exported from the module. If external code references it, keep it as an alias to `ProviderBackend` with a deprecation warning:

```python
# Backward compatibility
EthereumProvider = ProviderBackend  # deprecated — use ProviderBackend
```

## Implementation Order

1. **Merge `EthereumProvider` and `_SyncProviderBackend`** into `ProviderBackend` protocol
2. **Add `__getattr__`** to `ProviderAdapter` — tests pass (delegated methods reach backend)
3. **Remove explicit delegation methods** from `ProviderAdapter` that are now handled by `__getattr__`
4. **Simplify block guards** in `_Web3Adapter`, `_AlloyAdapter`, `_OfflineAdapter`
5. **Apply same changes to async side** — merge `_AsyncProviderBackend`, add `__getattr__` to `AsyncProviderAdapter`
6. **Add `EthereumProvider = ProviderBackend` alias** for backward compatibility
7. **Verify all tests pass** — public API is unchanged

## Testing

### Existing tests

All existing tests pass. The `ProviderAdapter` public API is identical.

### New unit tests (optional)

```python
def test_provider_adapter_delegates_to_backend():
    """Verify __getattr__ forwards unknown methods to backend."""
    w3 = FakeWeb3()
    adapter = ProviderAdapter.from_web3(w3)
    # get_code is not defined explicitly on ProviderAdapter,
    # so it should be forwarded to the backend
    result = adapter.get_code("0x1234...")
    assert result == expected


def test_provider_backend_protocol_satisfaction():
    """Verify _Web3Adapter satisfies ProviderBackend."""
    w3 = FakeWeb3()
    backend = _Web3Adapter(w3)
    assert isinstance(backend, ProviderBackend)
```

## Benefits

- **~300 lines of boilerplate removed** (80 per adapter class × ~3.5 adapter classes)
- **Adding a new RPC method = 2 edits** (backend adapter + protocol), not 7
- **Single protocol** instead of two nearly identical ones
- **Block guard simplification** removes ~30 lines of `if block is not None` patterns
- **Public API unchanged** — `ProviderAdapter`'s interface is identical; callers see no difference
- **Async side consistency** — same pattern applied to both adapters

## Risks

- **`__getattr__` and type checkers:** mypy/pyright don't understand `__getattr__` for type inference. Methods only available via `__getattr__` won't be suggested by IDEs. Mitigated by: (1) keeping the `ProviderBackend` protocol as the source of truth, (2) the most commonly used methods (`call`, `call_raw`, `chain_id`, `block_number`) are explicit on `ProviderAdapter` and will be visible to type checkers, (3) the delegated methods are "standard Ethereum provider" methods that are well-known.

- **`__getattr__` debugging:** if a backend method raises `AttributeError`, `__getattr__` will silently try the fallback and potentially mask the error. Mitigated by: `__getattr__` only activates for names that don't start with `_` (private attributes raise `AttributeError` immediately), and the dispatched method's error is the backend's error, not a masking one.

- **Alternative: explicit delegation is clearer.** If the team prefers explicit code over DRY, each delegation method can stay. The plan reduces to "merge the protocols + simplify block guards" which is ~100 lines removed instead of ~300. This is a valid scope reduction.

## Relationship to Other Plans

- **Plan 025** (Remove Web3 Bypass): Complete. All RPC calls now go through `ProviderAdapter`. This plan simplifies the adapter's internals without changing the routing.
- **Plan 039–041** (Curve deepening): Orthogonal. Those plans reorganize pool internals; ProviderAdapter is the I/O boundary they depend on, but its internals are independent.
- **ADR-001** (I/O-Free Pools): The `ProviderAdapter` is the I/O boundary. This plan doesn't change what crosses the boundary, only how the boundary is implemented.
