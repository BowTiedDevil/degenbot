# Context — Infrastructure

| Term | Definition | Aliases to avoid |
| ---- | ---------- | ---------------- |
| **Anvil Fork** | A local forked blockchain instance running via Foundry's Anvil client for testing | Fork, local chain |
| **Provider** | An adapter wrapping an RPC connection for blockchain reads (sync or async) | RPC client, web3 |
| **Connection Manager** | A class managing provider instances keyed by chain ID; instances owned by Bot | Connection |
| **Pool State Message** | A publisher/subscriber message notifying that a pool's state has changed | State update message |
| **Bot** | The central session class that owns all I/O, registries, config, and database connections; single entry point for all pool and token operations; resolves pool types automatically via `build_pool()`; delegates I/O orchestration to typed **Builders** | Bot session |

## Relationships

- A **Bot** owns a **Connection Manager** (via `bot.connections`)
- A **Bot** owns a **Pool Registry**, **Token Registry**, and **Managed Pool Registry**
- A **Bot** owns a **DatabaseSessionManager** (via `bot.db()`)
- There are no module-level singletons — all state flows through **Bot**

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
> **Dev:** "And how do pools get notified when their state changes?"
> **Domain expert:** "Through **Pool State Messages**. Pools publish state changes; subscribers like **Arbitrage Paths** and **Pool Cache Adapters** listen and react."
>
> **Dev:** "So **Bot** is the single entry point for everything?"
> **Domain expert:** "Yes. **Bot** owns all I/O, registries, config, and database connections. You call `bot.build_pool()` or `bot.build_erc20token()` — never instantiate pools or tokens directly."
