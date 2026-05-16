# Context — Connection Management

**Provider**:
An adapter wrapping an RPC connection for blockchain reads (sync or async). Implemented in `degenbot.provider` as `ProviderAdapter` and `AsyncProviderAdapter`; defined here because Connection Manager cannot be explained without it.
_Avoid_: RPC client, web3

**ProviderBackend**:
A `@runtime_checkable` protocol defining the contract for sync RPC backends (methods like `get_block_number`, `eth_call`, `get_logs`, etc.). `ProviderAdapter` wraps any `ProviderBackend` and delegates via `__getattr__` for methods that don't need extra logic. Formerly split into `EthereumProvider` protocol + `_SyncProviderBackend` adapter; collapsed via Plan 042. `EthereumProvider` remains as a backward-compatible alias for `ProviderBackend`.
_Avoid_: Backend, sync backend, Ethereum provider (use **ProviderBackend**)

**AsyncProviderBackend**:
The async counterpart of `ProviderBackend` — a `@runtime_checkable` protocol for async RPC backends. Formerly `_AsyncProviderBackend`; made public via Plan 042.
_Avoid_: Async backend

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
