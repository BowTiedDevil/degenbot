# Ubiquitous Language

Module-level glossaries (terms, aliases, and module-specific ambiguity rulings):

- [Pool Types, Managers & DEX Protocols](src/degenbot/types/UBIQUITOUS_LANGUAGE.md) — Pool, Pool State, Reserves, Sqrt Price, Tick, Fee, Simulation, Pool Types by Invariant, Pool Manager, Pool Factory, Exchange Deployment, supported DEX protocols · Ambiguity rulings: Factory vs Pool Manager, Fee representations
- [Tokens](src/degenbot/erc20/UBIQUITOUS_LANGUAGE.md) — Token, Token0/Token1, Ether Placeholder, Wrapped Native Token, Chain ID
- [Pool Registries](src/degenbot/registry/UBIQUITOUS_LANGUAGE.md) — Pool Registry, Token Registry, Managed Pool Registry
- [Arbitrage, Solvers & Adapters](src/degenbot/arbitrage/UBIQUITOUS_LANGUAGE.md) — Arbitrage Cycle, Arbitrage Path, Input/Profit Token & Amount, Swap Vector, Solver, Optimizer, Hop State, Pool Adapter · Ambiguity ruling: Solver vs Optimizer
- [Aave](src/degenbot/aave/UBIQUITOUS_LANGUAGE.md) — Market, Asset, Reserve, Collateral, Debt, aToken/vToken, GHO, Health Factor, Liquidation, Scaled/Raw Amount, Index, Enrichment, Processor, E-Mode, Isolation Mode
- [Infrastructure](src/degenbot/connection/UBIQUITOUS_LANGUAGE.md) — Anvil Fork, Provider, Connection Manager, Pool State Message

## Instructions

1. **Terms belong to one module.** Add new terms to the `UBIQUITOUS_LANGUAGE.md` in the module that owns the concept. Don't duplicate definitions at root.
2. **Ambiguity rulings go where the ambiguity lives.** If both terms are in the same module (e.g., Solver vs Optimizer), put the ruling in that module. Only cross-module ambiguities (e.g., Pool vs Market, Reserves vs Asset) go in root.
3. **Relationships follow the same rule.** If all terms in a relationship belong to one module, put it in that module's `## Relationships`. Only cross-module seams (where a term from one module relates to a term from another) go in root's `## Cross-module relationships`.
4. **When adding a module**, create its `UBIQUITOUS_LANGUAGE.md` with term table, `## Relationships`, and `## Resolved ambiguities` sections as needed, then add a link to the root index.
5. **Keep the root index in sync.** When a module glossary changes (new terms, new rulings), update the bullet summary in the root index to reflect it.
6. **Root contains only cross-cutting content:** module index, cross-module relationships, cross-module ambiguity rulings, and the example dialogue.

## Cross-module relationships

- A **Pool Registry** indexes all **Pools** across all chains; a **Token Registry** indexes all **Tokens**
- A **Managed Pool Registry** indexes **V4 Pools** by (chain ID, PoolManager address, Pool ID)
- An **Arbitrage Cycle** contains an ordered sequence of **Pools** that form a closed token loop
- A **Pool Adapter** translates a **Pool** into a **Hop State** for a **Solver**
- An **Arbitrage Path** subscribes to **Pool State Messages**
- An **Aave Market** contains many **Assets**, each wrapping an **Erc20Token** plus lending state

Module-internal relationships are documented in each module's glossary.

## Cross-module ambiguity rulings

These ambiguities span module boundaries and are resolved here so all modules stay consistent.

### 1. Pool vs Market vs Pool Contract

**Ruling: **Pool** = DEX only. **Market** = Aave lending system. **Pool contract** = the on-chain Aave contract named Pool.sol. A Market *has* a Pool contract; they can coexist in context.**

- ✅ "The WBTC/WETH **Pool** has 0.30% fee"
- ✅ "The Aave **Market** on Ethereum has 8 **Assets**"
- ✅ "The Market's **Pool contract** is at 0x8787…"
- ❌ "The Aave **pool** has 8 reserves" (use **Market**)
- ❌ "The **pool** not initialized" (use **Pool contract** if referring to the on-chain contract, or **Market** if referring to the system)

### 2. Reserves (DEX) vs Asset (Aave)

**Ruling: **Reserves** (plural) = DEX token balances. **Asset** = Aave lending state for one token.**

- ✅ "The **Reserves** are 1000 WBTC and 2000 WETH"
- ✅ "The USDC **Asset** has a liquidity index of 1.02e27"
- ❌ "The reserve is 1000 WBTC" (use **Reserves**)
- ❌ "The USDC reserves on Aave" (use **Asset**)

### 3. Asset vs Token

**Ruling: **Token** for all ERC-20 contracts. **Asset** for an ERC20 token plus its Aave lending state.**

A **Token** is just the ERC-20 contract (address, symbol, decimals) — no lending context. An **Asset** is the Token plus its lending state within an Aave Market.

- ✅ "The USDC **Token** address is 0xA0b8…" (the ERC-20 contract)
- ✅ "The USDC **Asset** has a borrow rate of 3%" (Aave lending state)
- ❌ "The asset address is 0xA0b8…" (use **Token** address)

## Example dialogue

> **Dev:** "I'm adding a new DEX pool type. Should I register it in the **Pool Registry** directly or go through a **Pool Manager**?"
>
> **Domain expert:** "Create a **Pool Manager** subclass for that DEX's **Exchange Deployment**. The **Pool Manager** handles discovery and tracking — it's the off-chain helper. The **Factory** is the on-chain contract that actually creates the **Pools**. The **Pool Registry** is just an index — **Pools** get added there automatically when they're created by the manager."
>
> **Dev:** "What about V4 pools that don't have their own contract address?"
>
> **Domain expert:** "V4 pools live inside a **PoolManager** contract and are identified by **Pool ID**, not address. The **Pool Registry** delegates V4 lookups to the **Managed Pool Registry**, which keys by (chain ID, PoolManager address, Pool ID). You'll need to set up the **Pool Adapter** so the **Solver** can convert the V4 pool into a **Hop State**."
>
> **Dev:** "And for the **Arbitrage Cycle**, I just add the V4 pool to `swap_pools`?"
>
> **Domain expert:** "Yes, but make sure the **Swap Vectors** line up — each pool's **Token Out** must equal the next pool's **Token In**, and the last pool must return the **Input Token**. The **Solver** will compute the optimal **Input Amount** for that single path, and you'll get back a **Calculation Result** with per-pool **Swap Amounts**. If you're comparing multiple paths, that's the **Optimizer**'s job — it delegates to the **Solver** per path and picks the best."
>
> **Dev:** "Got it. One more thing — the Aave **Asset** for USDC shows a borrow rate change. Should I call that a pool update?"
>
> **Domain expert:** "No — that's an Aave **Market**, not a **Pool**. The USDC **Asset** is one token's lending state inside that **Market**. A **Pool** is always a DEX contract. Keep the terms separate."
>
> **Dev:** "But the Aave contract is literally called Pool.sol — can I say 'Pool contract' to be specific?"
>
> **Domain expert:** "Yes — **Pool contract** is fine when you mean the on-chain contract. 'The Market's Pool contract emitted a Supply event' is perfectly clear. Just don't use **Pool** alone to mean the lending system — that's always a **Market**."
