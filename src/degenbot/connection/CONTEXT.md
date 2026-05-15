# Context — Connection Management

**Provider**:
An adapter wrapping an RPC connection for blockchain reads (sync or async). Implemented in `degenbot.provider` as `ProviderAdapter` and `AsyncProviderAdapter`; defined here because Connection Manager cannot be explained without it.
_Avoid_: RPC client, web3

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
