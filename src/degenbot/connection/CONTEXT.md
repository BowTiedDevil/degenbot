# Context — Connection Management

**Provider**:
An adapter wrapping an RPC connection for blockchain reads (sync or async). Implemented in `degenbot.provider` as `ProviderAdapter` and `AsyncProviderAdapter`; defined here because Connection Manager cannot be explained without it.
_Avoid_: RPC client, web3

**ProviderBackend**:
A `@runtime_checkable` protocol defining the contract for sync RPC backends (methods like `get_block_number`, `eth_call`, `get_logs`, etc.). `ProviderAdapter` wraps any `ProviderBackend` and delegates via `__getattr__` for methods that don't need extra logic. Formerly split into `EthereumProvider` protocol + `_SyncProviderBackend` adapter; the split has been collapsed. The `EthereumProvider` backward-compatibility alias has been removed.
_Avoid_: Backend, sync backend, Ethereum provider (use **ProviderBackend**)

**SyncSubscriptionSupport**:
A mixin class providing default `raise SubscriptionNotSupported` implementations for all 5 `subscribe_*` methods on sync backends. Sync adapters (`_Web3Adapter`, `_AlloyAdapter`, `_OfflineAdapter`) and `ProviderAdapter` inherit this mixin instead of copy-pasting stubs. Subclasses override `_subscription_transport` and `_subscription_rpc_url` properties for error messages.
_Avoid_: subscription stubs, sync subscription base

**AsyncSubscriptionSupport**:
The async counterpart — a mixin providing default `raise SubscriptionNotSupported` async stubs for backends that don't support WS/IPC subscriptions (e.g. `_AsyncWeb3Adapter`). `_AsyncAlloyAdapter` does NOT inherit this — it has real subscription implementations.
_Avoid_: async subscription stubs, async subscription base

**AsyncProviderBackend**:
The async counterpart of `ProviderBackend` — a `@runtime_checkable` protocol for async RPC backends. Formerly `_AsyncProviderBackend`.
_Avoid_: Async backend

**Subscription**:
A Rust-backed async iterator yielding push events from an Ethereum node via `eth_subscribe`. Created by `AsyncProviderAdapter.subscribe_*()` methods. Uses a double-buffer pattern: the Rust pump task writes raw events to an active buffer (zero GIL), and `drain()` atomically swaps + bulk-converts the stale buffer to Python dicts. Iterated with `async for` (which uses `drain()` internally with a local batch). `started()` awaits WS subscription confirmation; raises on failure. `drain()` returns `list[dict]` for bulk consumption. Terminates with `StopAsyncIteration` (clean) or `SubscriptionDisconnected` (connection lost). Requires WS or IPC transport; HTTP providers raise `SubscriptionNotSupported`.
_Avoid_: subscription stream, subscription handle, event stream

**LogListener**:
A pure Python dispatch registry that maps `(address, topic0)` → handler set. Receives raw log dicts via `dispatch(log)`, looks up handlers by address and first topic, and calls them sequentially. Exact match only — no wildcards. Handlers are sync `Callable[[dict], None]`. Exceptions propagate (fail loudly). Used with unfiltered `eth_subscribe("logs", {})` which guarantees logIndex ordering. ~200ns per miss, ~160μs/block for 800 discarded events. Created by the user; not owned by Bot or any adapter.
_Avoid_: subscription handler, event dispatcher, log dispatcher

**LogSubscriptionFilter**:
Filter parameters for log subscriptions with `addresses` and `topics` only — no block range (meaningless for push subscriptions). Separate from polling `LogFilter`.
_Avoid_: log filter subscription, subscription filter

**LOG_HANDLERS**:
A `ClassVar[dict[str, Callable]]` on pool types that maps event topic0 → decoder function. Each decoder takes a raw log dict and returns a closure that applies the decoded update to a pool instance. V2/V3/V4/Aerodrome pools declare their events; Curve pools have `LOG_HANDLERS = {}` (polling only). The user wires `LOG_HANDLERS` to a **LogListener** after `build_pool()`.
_Avoid_: event handlers, event decoders, pool handlers

**Connection Manager**:
A class managing provider instances keyed by chain ID; instances owned by Bot.
_Avoid_: Connection

## Relationships

- A **Connection Manager** stores `ChainId → Provider` mappings and delegates RPC calls to the appropriate **Provider**
- A **Bot** owns a **Connection Manager** (via `bot.connections`)

## Resolved Ambiguities

### Connection Manager (class) vs connection_manager module

**Ruling: **`ConnectionManager`** = the class. **`degenbot.connection.connection_manager`** = the module containing the class. Don't use the module name to refer to an instance.**

- ✅ "Create a `ConnectionManager` instance"
- ✅ "Bot's `connections` attribute is a `ConnectionManager`"
- ❌ "Import the connection_manager" (import the class, not the module as a singleton proxy)

## Example dialogue

> **Dev:** "I need to make an RPC call. Should I grab the **connection_manager** module?"
> **Domain expert:** "No — that's the module, not an instance. Create a **Connection Manager** (the class) and pass it to **Bot**. Bot owns the instance via `bot.connections`."
>
> **Dev:** "So I call `connection_manager.get_provider()`?"
> **Domain expert:** "You call `bot.connections.get_provider(chain_id)`. **Bot** owns the **Connection Manager** — always go through Bot."
