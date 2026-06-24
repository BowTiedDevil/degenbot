# Context Map

## Language

**Bot**: The central session class that owns all I/O, registries, config, and database connections; single entry point for all pool and token operations; resolves pool types automatically via `build_pool()`; delegates I/O orchestration to typed **Builders**; constructs a `PyBot` PyO3 wrapper (ADR-005) to hold Rust-owned pool/token state.
_Avoid_: Bot session, session, orchestrator, bot instance

**Pool State Message**: A publisher/subscriber message notifying that a pool's state has changed.
_Avoid_: State update message, state update

**Anvil Fork**: A local forked blockchain instance running via Foundry's Anvil client for testing.
_Avoid_: Fork, local chain

## Module contexts

- [Pool Types & Trackers](src/degenbot/types/CONTEXT.md) — pool types, type resolution, trackers, fee representations, and I/O-free architecture terms
- [Uniswap](src/degenbot/uniswap/CONTEXT.md) — V2/V3/V4 pools, concentrated liquidity, tick mechanics, event types, and **V3 vs V4 amountSpecified sign convention** (opposite!)
- [Tokens](src/degenbot/erc20/CONTEXT.md) — ERC-20 tokens, ether placeholder, and chain ID
- [Pool Registries](src/degenbot/registry/CONTEXT.md) — address-based registries and the pool type registry
- [Arbitrage, Solvers & Adapters](src/degenbot/arbitrage/CONTEXT.md) — arbitrage cycles, solvers, adapters, swap encoding, and the **Engine Registry** (the one canonical way to start a `UniswapArbEngine` operator)
- [Aave](src/degenbot/aave/CONTEXT.md) — lending markets, assets, collateral, debt, and liquidation; domain types (`ScaledTokenEvent`, `Operation`, `TokenType`) in `aave/operations.py` and `aave/types.py`; boundary invariant: `aave/` must never import from `cli/`
- [Curve StableSwap](src/degenbot/curve/CONTEXT.md) — StableSwap pools, CurveDataProvider seam, DyCalculator, variant and strategy enums (with `make_calculator()` factory methods), PerBlockCache (mirror-free design, extracted from pool), pure-value DyCalculationInputs (zero callables)
- [Provider](src/degenbot/provider/CONTEXT.md) — ProviderBackend/AsyncProviderBackend protocols, ProviderAdapter/AsyncProviderAdapter facades, subscription support mixins, and helper modules
- [Chainlink](src/degenbot/chainlink/CONTEXT.md) — price feeds, aggregators, round data
- [Builders](src/degenbot/builders/CONTEXT.md) — pool builders, PoolIO seam (7-method protocol), BuilderContext, BuildPoolRequest, `@staticmethod` `update()` on PoolBuilder/AsyncPoolBuilder protocols (type-enforced I/O separation), V2BuilderBase/V3BuilderBase/V4BuilderBase/BalancerBuilderBase shared helpers, and shared type resolution
- [Rust Extension](rust/CONTEXT.md) — PyO3-wrapped ABI encode/decode, GIL discipline, subscription double-buffer, two-level type intern, f64↔U256 conversion, UniswapArbEngine with V2+V3+V4 composition, **Bot** as the single state owner (ADR-003) with V2/V3/V4 `PoolEntry` variants, **Removed-Flag** reorg detection feeding the **Reorg Journal** (default 32 blocks), eager per-log processing (`apply_log` + `solve_dirty` coalesced at pump loop top, pump-mutates-core/engine-reads-by-reference asymmetry per the **Pool's Authority Over Its Own Math** rule), pure Uniswap event-log decoders in the `degenbot-decoders` alloy-only leaf crate (Plan 104), DEX identity presets + V2 swap encoding in the `degenbot-uniswap` protocol-domain crate (Plan 105), shared tick update helpers (`update_tick_liquidity` and `apply_liquidity_to_tick_range` in `tick_bitmap.rs`, matching Solidity's `Tick.update()`), V4 hook filtering (0xCC mask) and dynamic fee exclusion (0x100000), V4 amountSpecified sign convention (negative = exact-input, opposite to V3), unbounded result channel with incremental `ResultBatch` diffs, **Unregister Seam** (`BotState::unregister_pool` + `PyBot::unregister_pool`, ADR-007) as the symmetric removal half of the register seam propagating `PoolRegistry.remove`/`_reset` to Rust state (V2/V3 on `PyBot`; V4 engine-side, deferred); **sim-revert diagnostics** — `DiagnosticPathState` snapshot via `diagnostic_inspect_path` (engine/onchain state, `drift`/`field_drift`/`recompute` per hop), the always-on `[sim-diag]` JSON line per reverted candidate, and the four-way Drift/SolverCalc/Encoding/Unknown classifier (see [`docs/architecture/sim-revert-diagnostics.md`](docs/architecture/sim-revert-diagnostics.md))
- [Aerodrome](src/degenbot/aerodrome/CONTEXT.md) — Aerodrome V2 pools (Base/Arbitrum): per-pool solidly-stable-vs-constant-product duality (on-chain `stable()`), per-pool on-chain fee (`factory.getFee`), the solidly-stable invariant solved Python-side via `SolidlyStableHop.swap_fn`; design memo (WFY235) chooses strategy-on-`LiquidityPool` companion (Option A/D) over a separate Rust variant or third-family framing
- [Balancer V2](src/degenbot/balancer/CONTEXT.md) — Weighted pools, FixedPoint math, PowVersion detection, scaling helpers, Vault architecture, StableMath invariant versions (V1/V2), MetaStablePool, ComposableStablePool with BPT index, CacheAwareRateProvider, BalancerRateProvider protocol, StaleRateResult, BalancerBuilder, BalancerPairView, BalancerV2SwapAmounts, external_update, to_hop_state, build_swap_amount
- [Contract Reference](contract_reference/README.md) — Verified Solidity sources for Uniswap (V2/V3/V4) and Aave V3; ground truth for integer-exact Python ports

## Instructions

1. **Terms belong to one module.** Add new terms to the `CONTEXT.md` in the module that owns the concept. Don't duplicate definitions at root.
2. **Ambiguity rulings go where the ambiguity lives.** If both terms are in the same module, put the ruling in that module. Only cross-module ambiguities (e.g., Pool vs Market, Reserves vs Asset) go in root.
3. **Relationships follow the same rule.** If all terms in a relationship belong to one module, put it in that module's `## Relationships`. Only cross-module seams (where a term from one module relates to a term from another) go in root's `## Cross-module relationships`.
4. **When adding a module**, create its `CONTEXT.md` with term table, `## Relationships`, and `## Resolved ambiguities` sections as needed, then add a link to this map.
5. **Keep this map in sync.** When a module context changes (new terms, new rulings), update the bullet summary in this map to reflect it.
6. **Root contains only cross-cutting content:** shared term definitions, module index, cross-module relationships, cross-module ambiguity rulings, and the example dialogue.

## Cross-module relationships

- **Bot** owns all session state: Pool Registry, Token Registry, Managed Pool Registry, config, database
- **Bot.build_pool()** resolves the pool type automatically via DB `kind` column, Pool Type Registry, or on-chain probing, then dispatches to the **Builder Registry** (`dict[type, PoolBuilder]`) keyed by concrete pool class
- A **Pool Type Registry** (module-level singleton) maps (chain ID, factory address) → pool class + identity + deployment data; each DEX module self-registers at import time; auto-derives invariant, variant, and kind from the class
- A **Pool Registry** (class, owned by Bot) indexes all **Pools** across all chains; a **Token Registry** indexes all **Tokens**; a **Managed Pool Registry** indexes **V4 Managed Pools** by (chain ID, PoolManager address, Pool ID)
- An **Arbitrage Cycle** (deprecated) was an ordered sequence of **Pools** that form a closed token loop; replaced by **Arbitrage Path**
- An **Arbitrage Path** wraps an ordered sequence of **Pools** with a **Solver** and subscribes to **Pool State Messages**; a **Pool Adapter** translates each **Pool** into a **Hop State** via the pool's `to_hop_state()` (single source of truth; N-token pools take `token_in`/`token_out` kwargs)
- A **Pool Cache Adapter** (removed) formerly subscribed to **Pool State Messages** and auto-registered pools in the Rust solver cache; that path is deleted — see `arbitrage/CONTEXT.md`
- **Swap Amounts** carry per-pool swap parameters and self-encode into **EncodedCall**s; `input_amount()`/`output_amount()` provide generic extraction; `build_swap_amount()` on pool classes replaces instanceof-chain factory; `generate_payloads()` wires encoding → **ApprovalStrategy** → **PayloadComposer**
- An **Aave Market** contains many **Assets**, each wrapping an **Erc20Token** plus lending state
- A **Curve Pool Tracker** tracks **Curve StableSwap Pools** and delegates construction to **Bot**
- V2/V3/V4/Aerodrome **Pools** are fully I/O-free — **Builders** fetch all data from DB/RPC and pass values; no pool class imports `ProviderAdapter` or carries provider-dependent methods
- **PoolIO** is the builder-facing I/O seam — a 7-method protocol (`call`, `call_raw`, `get_block_number`, `get_block`, `get_block_timestamp`, `get_code`, `get_balance`) with sync (`SyncPoolIO`) and async (`AsyncPoolIO`) adapters wrapping `ProviderAdapter`/`AsyncProviderAdapter`; **Bot** and **AsyncBot** create the appropriate adapter and pass `io=` to all builder calls; `BuilderContext` no longer carries a `connections` field — all I/O flows through `io: PoolIO` at call sites; **AsyncBot** delegates its 4 token/ether balance I/O methods to `AsyncErc20Builder`
- **PoolFamily** (identity enum in `types/pool_type.py`) is the sole identity enum; **Pool Invariant** (in `types/hop_types.py`) is the solver-dispatch enum
- The **Rust extension** owns the arbitrage engine (`UniswapArbEngine`), the per-chain `Bot` state owner, and the PyO3 FFI topology — see `rust/CONTEXT.md`

Module-internal relationships are documented in each module's context file.

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

### 4. Singleton (removed) vs Class instance

**Ruling: All former module-level singletons have been removed. Always refer to class instances owned by Bot.**

The following were previously module-level singletons; they are now classes instantiated and owned by Bot:
- `PoolRegistry` / `TokenRegistry` / `ManagedPoolRegistry` — formerly `pool_registry` / `token_registry` / `managed_pool_registry`
- `DatabaseSessionManager` — formerly `db_session`
- `Config` — formerly `config` (LazyConfig proxy)

The former `ConnectionManager` / `AsyncConnectionManager` (and the swallowed multi-chain `bot.connections` indirection) were deleted in ADR-006 slice 8b — one Bot per chain now owns a single `ProviderAdapter` (`bot.provider`), with the chain identity in `config.default_chain_id` and enforced at construction.

**Exception:** The `PoolTypeRegistry` (`pool_type_registry`) is a module-level singleton. This is intentional: the (chain ID, factory address) → class + identity + deployment data mapping is global knowledge that does not vary between Bot instances.

- ✅ "Create a `PoolRegistry`/`TokenRegistry` owned by Bot"
- ✅ "Bot's single chain comes from `config.default_chain_id`; `bot.provider` is the `ProviderAdapter`"
- ✅ "The `pool_type_registry` maps SushiSwap's factory to a `LiquidityPool` registration carrying the `sushiswap-v2` `DexIdentity` preset + `variant="sushiswap"` (post slice-7 collapse — the per-DEX `SushiswapV2Pool` subclass is deleted)"
- ❌ "Import the connection_manager" (deleted in ADR-006 slice 8b — `bot.connections` no longer exists)

## Example dialogue

> **Dev:** "I'm adding a new DEX pool type. Should I register it in the **Pool Registry** directly or go through a **Pool Tracker**?"
>
> **Domain expert:** "Create a **Pool Tracker** subclass for that DEX's **Exchange Deployment**. The **Pool Tracker** handles discovery and tracking — it's the off-chain helper. The **Factory** is the on-chain contract that actually creates the **Pools**. The **Pool Registry** is just an index owned by **Bot** — **Pools** get added there automatically when they're discovered by the tracker."
>
> **Dev:** "And when someone calls `bot.build_pool(address)`, how does it know which subclass to use?"
>
> **Domain expert:** "The type resolver checks three sources in order. First, the database `kind` column — if the pool was persisted, it knows the exact subclass. Second, the **Pool Type Registry** — each DEX module registers its factory addresses at import time, so the resolver maps factory → class. Third, if neither source matches, it probes the contract on-chain — tries `slot0()` for concentrated-liquidity, `getReserves()` for constant-product. This all happens inside `build_pool` automatically."
>
> **Dev:** "What about V4 pools that don't have their own contract address?"
>
> **Domain expert:** "V4 **Managed Pools** live inside a **PoolManager** contract and are identified by **Pool ID**, not address. Call `build_managed_pool(address, pool_id)` — the `address` is the PoolManager and `pool_id` is required. They have their own `BuildManagedPoolRequest` dataclass with `pool_id` as a required field, separate from `BuildPoolRequest` used for V2/V3/Curve/Balancer."
>
> **Dev:** "So `build_pool()` doesn't accept `pool_id` anymore?"
>
> **Domain expert:** "Right — `build_pool()` is for non-V4 pools only now. `build_pool()` takes a small set of optional kwargs, and `build_managed_pool()` takes V4-specific required parameters. The deployer and init-hash resolution used to be passed through `build_pool()` kwargs or silently overwritten from `FACTORY_DEPLOYMENTS` — now `pool_type_registry` is the sole source of truth."
>
> **Dev:** "And for the **Arbitrage Path**, I just add the V4 pool to `swap_pools`?"
>
> **Domain expert:** "Yes, but make sure the **Swap Vectors** line up — each pool's **Token Out** must equal the next pool's **Token In**, and the last pool must return the **Input Token**. The **Solver** will compute the optimal **Input Amount** for that single path, and you'll get back a **Calculation Result** with per-pool **Swap Amounts**. (Comparing multiple paths and selecting the best is a separate, not-yet-built concern; **Solver** is the sole term in the codebase today.)"
>
> **Dev:** "Got it. One more thing — the Aave **Asset** for USDC shows a borrow rate change. Should I call that a pool update?"
>
> **Domain expert:** "No — that's an Aave **Market**, not a **Pool**. The USDC **Asset** is one token's lending state inside that **Market**. A **Pool** is always a DEX contract. Keep the terms separate."
>
> **Dev:** "But the Aave contract is literally called Pool.sol — can I say 'Pool contract' to be specific?"
>
> **Domain expert:** "Yes — **Pool contract** is fine when you mean the on-chain contract. 'The Market's Pool contract emitted a Supply event' is perfectly clear. Just don't use **Pool** alone to mean the lending system — that's always a **Market**."
