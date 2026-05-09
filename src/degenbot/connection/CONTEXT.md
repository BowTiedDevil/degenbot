# Context — Infrastructure

| Term | Definition | Aliases to avoid |
| ---- | ---------- | ---------------- |
| **Anvil Fork** | A local forked blockchain instance running via Foundry's Anvil client for testing | Fork, local chain |
| **Provider** | An adapter wrapping an RPC connection for blockchain reads (sync or async) | RPC client, web3 |
| **Connection Manager** | A class managing provider instances keyed by chain ID; instances owned by Bot | Connection |
| **Pool State Message** | A publisher/subscriber message notifying that a pool's state has changed | State update message |
| **Bot** | The central session class that owns all I/O, registries, config, and database connections; single entry point for all pool and token operations | Bot session |

## Relationships

- A **Bot** owns a **Connection Manager** (via `bot.connections`)
- A **Bot** owns **Pool**, **Token**, and **Managed Pool Registries**
- A **Bot** owns a **DatabaseSessionManager** (via `bot.db()`)
- There are no module-level singletons — all state flows through **Bot**

## Resolved Ambiguities

### Connection Manager (class) vs connection_manager module

**Ruling: **`ConnectionManager`** = the class. **`degenbot.connection.connection_manager`** = the module containing the class. Don't use the module name to refer to an instance.**

- ✅ "Create a `ConnectionManager` instance"
- ✅ "Bot's `connections` attribute is a `ConnectionManager`"
- ❌ "Import the connection_manager" (import the class, not the module as a singleton proxy)
