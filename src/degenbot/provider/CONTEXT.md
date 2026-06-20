# Context — Provider Module

**ProviderBackend**:
A `@runtime_checkable` protocol defining the contract for sync RPC backends (methods like `get_block_number`, `call`, `get_logs`, etc.). Defined in `protocols.py`. The `_Web3Adapter`, `_AlloyAdapter`, and `_OfflineAdapter` private classes satisfy this protocol.
_Avoid_: Backend, sync backend, Ethereum provider (use **ProviderBackend**)

**AsyncProviderBackend**:
The async counterpart of `ProviderBackend` — a `@runtime_checkable` protocol for async RPC backends. Defined in `protocols.py`. `_AsyncWeb3Adapter` and `_AsyncAlloyAdapter` satisfy this protocol.
_Avoid_: Async backend

**ProviderAdapter**:
The public sync facade wrapping Web3, AlloyProvider, or OfflineProvider. Constructed via `from_web3()`, `from_alloy()`, or `from_offline()`. Delegates to an internal `ProviderBackend`. Supports pickle round-tripping via `__getstate__`/`__setstate__` + `set_provider()`. Defined in `sync_adapter.py`.
_Avoid_: sync adapter, provider wrapper

**AsyncProviderAdapter**:
The public async facade wrapping AsyncWeb3 or AsyncAlloyProvider. Constructed via `from_web3()` or `from_alloy()`. Delegates to an internal `AsyncProviderBackend`. Sync `chain_id`/`block_number` properties raise `NotImplementedError` — callers must use `await get_chain_id()`/`await get_block_number()`. Defined in `async_adapter.py`.
_Avoid_: async adapter, async provider wrapper

**SyncSubscriptionSupport**:
A mixin in `sync_adapter.py` providing default `raise SubscriptionNotSupported` stubs for all 5 `subscribe_*` methods. Sync private adapters and `ProviderAdapter` inherit this mixin. Subclasses override `_subscription_transport` and `_subscription_rpc_url` for error messages.
_Avoid_: subscription stubs, sync subscription base

**AsyncSubscriptionSupport**:
The async counterpart in `async_adapter.py` — a mixin providing default `raise SubscriptionNotSupported` async stubs for backends that lack WS/IPC support (e.g. `_AsyncWeb3Adapter`).
_Avoid_: async subscription stubs, async subscription base

**_AsyncAlloyAdapter**:
An adapter that does **not** inherit `AsyncSubscriptionSupport` because it implements real subscription methods (its backend genuinely supports WS/IPC). This asymmetry reflects a real capability difference, not a design flaw. No additional mixin needed.
_Avoid_: (private — avoid referencing outside this module)

**_backend_for_type**:
A private helper in `sync_adapter.py` that maps a `provider_type` label (`"web3"`, `"alloy"`, `"offline"`) to the correct private adapter class. Used by `ProviderAdapter.set_provider()` for pickle round-tripping. Not used by async code.
_Avoid_: (private — avoid referencing outside this module)

## File structure

| File | Contents |
|------|----------|
| `protocols.py` | `ProviderBackend`, `AsyncProviderBackend` — pure protocol seams, no implementation |
| `sync_adapter.py` | `SyncSubscriptionSupport`, `_Web3Adapter`, `_AlloyAdapter`, `_OfflineAdapter`, `ProviderAdapter`, `_backend_for_type` |
| `async_adapter.py` | `AsyncSubscriptionSupport`, `_AsyncWeb3Adapter`, `_AsyncAlloyAdapter`, `AsyncProviderAdapter` |
| `offline_provider.py` | `OfflineProvider` — local-recording provider for offline/testing use |
| `subscription.py` | `Subscription`, `LogSubscriptionFilter` — async iterator primitives for `eth_subscribe` |
| `call_helpers.py` | `raw_call`, `async_raw_call`, `encode_function_calldata`, `extract_argument_types_from_function_prototype` |
| `log_fetching.py` | `fetch_logs_retrying`, `fetch_logs_retrying_async` |
| `block_helpers.py` | `get_number_for_block_identifier`, `get_number_for_block_identifier_async` |
| `__init__.py` | `AlloyProvider` (Python wrapper re-export), public re-exports of adapters and protocols |

## Relationships

- `protocols.py` imports nothing from `degenbot.provider.*` — no cycle possible
- `sync_adapter.py` and `async_adapter.py` import from `protocols.py` only
- `__init__.py` re-exports `ProviderAdapter`, `AsyncProviderAdapter`, `ProviderBackend`, `AsyncProviderBackend` from the three focused modules
- **Bot** owns one `ProviderAdapter` per chain (post-ADR-006); see CONTEXT-MAP.md ambiguity #4

## Resolved Ambiguities

### protocols.py vs sync_adapter.py/async_adapter.py for protocol imports

**Ruling: Import protocols from `protocols.py` when you only need the seam. Import adapters from `sync_adapter.py`/`async_adapter.py` when you need the concrete adapter class.**

- ✅ `from degenbot.provider.protocols import ProviderBackend` (type annotation only)
- ✅ `from degenbot.provider.sync_adapter import ProviderAdapter` (constructing an instance)
- ✅ `from degenbot.provider import ProviderAdapter` (convenience via `__init__.py`)
- ❌ `from degenbot.provider.sync_adapter import ProviderBackend` (wrong file — protocols live in `protocols.py`)
