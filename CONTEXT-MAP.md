# Context Map

## Language

**Bot**: The central session class that owns all I/O, registries, config, and database connections; single entry point for all pool and token operations; resolves pool types automatically via `build_pool()`; delegates I/O orchestration to typed **Builders**.
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
- [Arbitrage, Solvers & Adapters](src/degenbot/arbitrage/CONTEXT.md) — arbitrage cycles, solvers, adapters, and swap encoding
- [Aave](src/degenbot/aave/CONTEXT.md) — lending markets, assets, collateral, debt, and liquidation; domain types (`ScaledTokenEvent`, `Operation`, `TokenType`) in `aave/operations.py` and `aave/types.py`; boundary invariant: `aave/` must never import from `cli/`
- [Curve StableSwap](src/degenbot/curve/CONTEXT.md) — StableSwap pools, CurveDataProvider seam, DyCalculator, variant and strategy enums (with `make_calculator()` factory methods), PerBlockCache (mirror-free design, extracted from pool), pure-value DyCalculationInputs (zero callables)
- [Connection Management](src/degenbot/connection/CONTEXT.md) — connection managers, provider references, and subscription primitives
- [Provider](src/degenbot/provider/CONTEXT.md) — ProviderBackend/AsyncProviderBackend protocols, ProviderAdapter/AsyncProviderAdapter facades, subscription support mixins, and helper modules
- [Chainlink](src/degenbot/chainlink/CONTEXT.md) — price feeds, aggregators, round data
- [Builders](src/degenbot/builders/CONTEXT.md) — pool builders, PoolIO seam (7-method protocol), BuilderContext, BuildPoolRequest, `@staticmethod` `update()` on PoolBuilder/AsyncPoolBuilder protocols (type-enforced I/O separation), V2BuilderBase/V3BuilderBase/V4BuilderBase/BalancerBuilderBase shared helpers, and shared type resolution
- [Rust Extension](rust/CONTEXT.md) — PyO3-wrapped ABI encode/decode, GIL discipline, subscription double-buffer, Möbius solver cache, two-level type intern, f64↔U256 conversion, V2BlockEngine/V3BlockEngine/V4BlockEngine, UniswapArbEngine with V2+V3+V4 composition, V3 Mint/Burn event decoders (`update_tick_liquidity` matching Solidity's `Tick.update()`), V4 hook filtering (0xCC mask) and dynamic fee exclusion (0x100000), V4 amountSpecified sign convention (negative = exact-input, opposite to V3)
- [Balancer V2](src/degenbot/balancer/CONTEXT.md) — Weighted pools, FixedPoint math, PowVersion detection, scaling helpers, Vault architecture, StableMath invariant versions (V1/V2), MetaStablePool, ComposableStablePool with BPT index, CacheAwareRateProvider, BalancerRateProvider protocol, StaleRateResult, BalancerBuilder, BalancerPairView, BalancerV2SwapAmounts, external_update, to_hop_state, build_swap_amount
- [Contract Reference](contract_reference/README.md) — Verified Solidity sources for Uniswap (V2/V3/V4) and Aave V3; ground truth for integer-exact Python ports
- [Tstore Executor](contracts/README.md) — On-chain V2/V3/V4 arbitrage executor, V4 delta ledger, 4-phase auto-settlement, isolated test suite

## Instructions

1. **Terms belong to one module.** Add new terms to the `CONTEXT.md` in the module that owns the concept. Don't duplicate definitions at root.
2. **Ambiguity rulings go where the ambiguity lives.** If both terms are in the same module (e.g., Solver vs Optimizer), put the ruling in that module. Only cross-module ambiguities (e.g., Pool vs Market, Reserves vs Asset) go in root.
3. **Relationships follow the same rule.** If all terms in a relationship belong to one module, put it in that module's `## Relationships`. Only cross-module seams (where a term from one module relates to a term from another) go in root's `## Cross-module relationships`.
4. **When adding a module**, create its `CONTEXT.md` with term table, `## Relationships`, and `## Resolved ambiguities` sections as needed, then add a link to this map.
5. **Keep this map in sync.** When a module context changes (new terms, new rulings), update the bullet summary in this map to reflect it.
6. **Root contains only cross-cutting content:** shared term definitions, module index, cross-module relationships, cross-module ambiguity rulings, and the example dialogue.

## Cross-module relationships

- **Bot** owns all session state: Connection Manager, Pool Registry, Token Registry, Managed Pool Registry, config, database
- **Bot.build_pool()** resolves the pool type automatically via DB `kind` column, Pool Type Registry, or on-chain probing, then dispatches to the **Builder Registry** (`dict[type, PoolBuilder]`) keyed by concrete pool class
- A **Pool Type Registry** (module-level singleton) maps (chain ID, factory address) → pool class + identity + deployment data; each DEX module self-registers at import time; auto-derives invariant, variant, and kind from the class
- A **Pool Registry** (class, owned by Bot) indexes all **Pools** across all chains; a **Token Registry** indexes all **Tokens**
- A **Managed Pool Registry** indexes **V4 Managed Pools** by (chain ID, PoolManager address, Pool ID)
- An **Arbitrage Cycle** (deprecated) was an ordered sequence of **Pools** that form a closed token loop; replaced by **Arbitrage Path**
- An **Arbitrage Path** contains an ordered sequence of **Pools** that form a closed token loop, wraps them with a **Solver**, and subscribes to **Pool State Messages**
- A **Pool Adapter** translates a **Pool** into a **Hop State** for a **Solver** (inline in `ArbitragePath`; `solver_hop_builders.py` deleted)
- A **Pool Cache Adapter** subscribes to **Pool State Messages** and auto-registers both reserve orientations in the Rust solver cache
- An **Arbitrage Path** subscribes to **Pool State Messages**
- **Swap Amounts** carry per-pool swap parameters and self-encode into **EncodedCall**s; `input_amount()`/`output_amount()` provide generic extraction; `build_swap_amount()` on pool classes replaces instanceof-chain factory; `generate_payloads()` wires encoding → **ApprovalStrategy** → **PayloadComposer**
- **Pool → Hop conversion** flows through each pool's `to_hop_state()` method (single source of truth); N-token pools accept `token_in`/`token_out` keyword-only kwargs for pair selection, 2-token pools accept and ignore them
- An **Aave Market** contains many **Assets**, each wrapping an **Erc20Token** plus lending state
- A **Curve Pool Tracker** tracks **Curve StableSwap Pools** and delegates construction to **Bot**
- A **BalancerV2StablePool** handles MetaStablePool (`bpt_idx=None`, `invariant_version=INVARIANT_V2`, no rate cache — direct `getRate()` calls) and ComposableStablePool (`bpt_idx=int`, `invariant_version=INVARIANT_V1` for most deployed pools, rate cache refreshed before each swap via `_beforeSwapJoinExit`). With a `CacheAwareRateProvider` that replicates `_cacheTokenRateIfNecessary` (read `getTokenRateCache()` → check expiry → call `getRate()` only if expired), exact 0-wei matching is achieved. Without a rate provider, `StaleRateResult` is raised (child of `PossibleInaccurateResult`). `HookedPoolResult` is the V4-specific child for pools with active hooks
- **Invariant versions**: V1 (`INVARIANT_V1`, always-roundDown, D_P accumulation, matches monorepo `_calculate_invariant`) used by most ComposableStablePools; V2 (`INVARIANT_V2`, with `roundUp` param, P_D accumulation, matches `_calculate_invariant_deployed`) used by MetaStablePools. V2 with `roundUp=True` produces an invariant 1 wei higher than V1 — wrong version gives systematic ±1 wei error
- **BROKEN_BALANCER_V2_POOLS** in `balancer/deployments.py` follows the same pattern as **BROKEN_CURVE_V1_POOLS** in `curve/deployments.py` — a frozenset of pool addresses where on-chain swaps are disabled or the pool is otherwise broken, used to filter during discovery and testing
- The **BalancerBuilder** owns the full I/O choreography for Balancer V2 pool construction: probes `getNormalizedWeights()`/`getAmplificationParameter()` to detect pool type, fetches pool ID / vault tokens / fee / weights / amp / rate providers from RPC, then constructs `BalancerV2Pool` or `BalancerV2StablePool`. **BalancerBuilderBase** provides shared `@staticmethod` helpers — pure-logic decode helpers (`decode_pool_id`, `detect_bpt_index`, `resolve_invariant_version`) and I/O helpers (`_fetch_pool_id`, `_fetch_vault_tokens`, `_fetch_swap_fee`, `_fetch_weights`, `_fetch_amp`, `_fetch_rate_providers`, `_fetch_rates`, `_detect_pool_type`) — for async reuse
- **BalancerPairView** adapts an N-token Balancer pool to a 2-token pair view for `ArbitragePathPool` conformance. Implements subscription relay: subscribes to the underlying pool and re-publishes to its own subscribers with `publisher=self`, so `ArbitragePath._pool_index` identity checks work correctly
- **BalancerV2SwapAmounts** encodes a Vault.swap() call with SingleSwap (poolId, kind, assetIn, assetOut, amount, userData) and FundManagement (sender, fromInternalBalance, recipient, toInternalBalance) structs
- **Pool → Hop conversion** for Balancer: `to_hop_state()` returns `BalancerWeightedHop` (with `swap_fn`) or `BalancerStableHop` (with `swap_fn` that catches `StaleRateResult`); N-token pools accept `token_in`/`token_out` kwargs for pair selection
- Balancer V2 pools use `external_update()` with `_state_lock` (double-check-after-acquire pattern matching V2/Curve/V3 pools). No `StateCache` — simple `_state` replacement without temporal navigation
- On-chain probing for type resolution now includes Balancer: `getPoolId()` → `getNormalizedWeights()`/`getAmplificationParameter()` produces `PoolFamily.WEIGHTED` or `PoolFamily.STABLESWAP` with `balancer_weighted`/`balancer_stable` variants. `pool_class_for_descriptor` rejects Balancer variants without factory registration (hard error instead of silently constructing `CurveStableswapPool`)
- `PoolTypeRegistry.register()` accepts optional `family` override — when provided, bypasses `_derive_family()`. Used by Balancer V2 weighted pools (auto-derives as `STABLESWAP` due to `tokens` attribute, overridden to `WEIGHTED`)
- A **CurveDataProvider** is injected into **Curve Pools** by the **Curve Pool Builder** (invoked via `Bot.build_pool()`); pools never access connections directly. The builder creates a `CurveDataProviderImpl` (structured class with real methods) that wraps a `ProviderAdapter` — the former closure-based `CurveFetcherFactory` and individual fetcher callbacks have been replaced
- A **Curve Pool** holds a **PerBlockCache** (`_cache`) that owns per-block cache fields with accessor methods (`get_cached_*`) implementing the try-cache→call-provider→store→return pattern; mirror-free design — `get_cached_virtual_price()` resolves its own dependencies inline by calling `get_cached_base_cache_updated()` and `get_cached_base_virtual_price()`, eliminating the former side-effect mirrors (`_base_cache_updated_value`, `_base_virtual_price_value`); formerly per-pool `_cache_*` fields with `_get_cached_*` methods and side-effect mirrors
- Strategy enums (`SwapStyle`, `MetapoolRateStyle`, `MetapoolUnderlyingStyle`) provide `make_calculator()` factory methods returning the matching `DyCalculator` instance; `PoolStrategies` auto-constructs calculators from enum values
- V2/V3/V4/Aerodrome **Pools** are fully I/O-free — **Builders** fetch all data from DB/RPC and pass values; no pool class imports `ProviderAdapter` or carries provider-dependent methods
- **PoolIO** is the builder-facing I/O seam — a 7-method protocol (`call`, `call_raw`, `get_block_number`, `get_block`, `get_block_timestamp`, `get_code`, `get_balance`) with sync (`SyncPoolIO`) and async (`AsyncPoolIO`) adapters wrapping `ProviderAdapter`/`AsyncProviderAdapter`; **Bot** and **AsyncBot** create the appropriate adapter and pass `io=` to all builder calls; `BuilderContext` no longer carries a `connections` field — all I/O flows through `io: PoolIO` at call sites; **AsyncBot** delegates its 4 token/ether balance I/O methods to `AsyncErc20Builder`
- **Type Resolution** (`type_resolution.py`) provides shared pure-logic functions for pool class resolution; sync/async top-level functions are thin wrappers that delegate to `_build_descriptor_from_db_result` and `_descriptor_from_probing_result`; I/O-bearing steps come in sync/async pairs that accept `PoolIO`/`AsyncPoolIO`
- **DyCalculationInputs** is a frozen dataclass constructed by `CurveStableswapPool.get_dy()` that carries pre-resolved data for a single dy calculation (including `d_variant`/`y_variant`/`yd_variant`/`a_precision`); **DyCalculator** implementations receive this instead of the pool object, eliminating all private member access; calculators call pure `stableswap_get_y()`/`stableswap_newton_y()` directly — no closures, no pool references
- **PoolFamily** (identity enum in `types/pool_type.py`) is the sole identity enum; **Pool Invariant** (in `types/hop_types.py`) is the solver-dispatch enum
- **CacheablePool** protocol enables **Pool Cache Adapter** registration without introspection
- **Swap Amounts** carry per-pool swap parameters and self-encode into **EncodedCall**s; `generate_payloads()` wires encoding → **ApprovalStrategy** → **PayloadComposer**
- **V4PoolKey** lives on `UniswapV4PoolSwapAmounts` for custom **PayloadComposers** handling V4's unlock/swap callback dispatch
- The **Tstore Executor** (`contracts/tstore_executor.vy`) uses a hybrid V4 settlement approach: V4 swaps via `extcall` in `unlockCallback`, all-currency delta tracking in `t_v4_deltas` (not just ETH/WETH), and 4-phase auto-settlement. Phase 0 pre-settles ERC-20 for V3→V4/V2→V4 (with dedup for same-currency inputs); Phase 1 executes V4 swaps; Phase 2 delivers queued payloads (take/transfer) and zeros intermediate ERC-20 deltas; Phase 3 settles all nonzero deltas via `_v4_settle_currency`, which zeros each delta after settling to prevent double-settlement. `NATIVE_ADDRESS` constant replaces inline `empty(address)` locals. `_decode_swap_delta(swap_delta, byte_offset)` merges the former `_decode_swap_delta_amount0/1` pair. V2 callbacks inline `_deliver_remaining_payloads()` directly (no wrapper). V3 auto-pay computes `owed_token`/`owed_amount` first, then single transfer. Verified by 27 contract tests in isolated Ape + Foundry suite (`contracts/tests/`). See `contracts/README.md` and `contracts/tests/README.md`
- **int128 overflow guard** (`fits_int128()` in `degenbot.arbitrage.encoding`) prevents V4 `SafeCastOverflow` reverts by skipping paths where `amountSpecified` exceeds ±2^127. All 5 V4 encoder functions check this. Tested by 13 unit tests in `tests/arbitrage/test_int128_range.py`
- **V4→V2 amount_out fix**: V2 `swap(amount0Out, amount1Out, ...)` specifies what V2 SENDS. For USDC→WETH@V2, `amount_out` is `weth_out` (not `forward_out`). Regression test in `TestV4ToV2WrongAmountOut`
- A **Subscription** is a Rust-backed async iterator over push events from `eth_subscribe` with double-buffer drain for GIL-free accumulation; created by `AsyncProviderAdapter.subscribe_*()`; requires WS/IPC transport; raises **SubscriptionNotSupported** on HTTP providers; sync adapters and `_AsyncWeb3Adapter` inherit subscription stubs from **SyncSubscriptionSupport** / **AsyncSubscriptionSupport** mixins
- A **LogListener** is a pure Python dispatch registry mapping `(address, topic0)` → handler set; receives raw log dicts via `dispatch(log)`, calls handlers sequentially; created by the user, not owned by Bot
- **LogSubscriptionFilter** carries `addresses` + `topics` only (no block range) for log subscriptions
- **LOG_HANDLERS** is a `ClassVar[dict[str, Callable]]` on pool types mapping event topic0 → decoder function; each decoder takes a log dict and returns a closure that applies the update to a pool instance; the user wires LOG_HANDLERS to a LogListener after `build_pool()`
- **Bot.start_listening()** creates newHeads + unfiltered logs subscriptions for a chain, returns the subscription pair; stores the AsyncProviderAdapter per chain in `_async_adapters`
- The **Rust extension** provides PyO3-wrapped ABI encode/decode (`CachedAbiTypes` two-level intern: string `Arc<str>` interner + value `Arc<CachedAbiTypes>` return), subscription double-buffer (`drain_raw()` for pure-Rust GIL-free accumulation, `drain_buffer()` for Python), and Möbius solver cache (`PyPoolCache` with `parking_lot::Mutex<LruCache<u64, IntHopState>>`, 10K cap, pre-converted U512 fields). GIL discipline: hold for sub-μs compute (tick math, address utils), release for I/O (`py.detach()` before `block_on()`); all `Python::attach()` call sites have `// SAFETY` comments. `f64_to_u256` uses iterative 4-limb decomposition (the previous 2-limb version silently failed for values > 128 bits). `auto-initialize` is the default Cargo feature. **UniswapArbEngine** composes `V2BlockEngine` + `V3BlockEngine` + `V4BlockEngine` — V4 pools are concentrated liquidity identified by `(pool_manager, pool_id)` and are solved with the same `int_solve_v3_v3`/`exact_solve_mixed_v2_v3_sequence` as V3. V3 `process_block()` now decodes Swap, Mint, and Burn events; `apply_liquidity_update()` mutates tick_data matching Solidity's `Tick.update()` — both lower and upper tick receive `gross += delta`, while `net` flips sign for the upper tick. V4 pools with amount-modifying hooks (`AMOUNT_MODIFYING_HOOK_MASK = 0xCC`) or dynamic fees (`V4_DYNAMIC_FEE_FLAG = 0x100000`) are rejected at registration. V4 `amountSpecified` uses negative values for exact-input (opposite convention to V3). **UniswapEnginePump** uses dual WS subscriptions (`newHeads` + unfiltered `logs`) with Rust-side filtering by topic + address, buffering logs against block boundaries, and two backfill triggers: (1) 60s timeout → `eth_getLogs` for missing range; (2) empty block → `eth_getLogs` to verify. `BlockNotification` carries a `backfilled` field.

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
- `ConnectionManager` / `AsyncConnectionManager` — formerly `connection_manager` / `async_connection_manager`
- `PoolRegistry` / `TokenRegistry` / `ManagedPoolRegistry` — formerly `pool_registry` / `token_registry` / `managed_pool_registry`
- `DatabaseSessionManager` — formerly `db_session`
- `Config` — formerly `config` (LazyConfig proxy)

**Exception:** The `PoolTypeRegistry` (`pool_type_registry`) is a module-level singleton. This is intentional: the (chain ID, factory address) → class + identity + deployment data mapping is global knowledge that does not vary between Bot instances.

- ✅ "Create a `ConnectionManager` instance and pass it to Bot"
- ✅ "Bot's `connections` attribute is a `ConnectionManager`"
- ✅ "The `pool_type_registry` maps SushiSwap's factory to `SushiswapV2Pool`"
- ❌ "Import the connection_manager" (the module-level singleton no longer exists)

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
> **Domain expert:** "Yes, but make sure the **Swap Vectors** line up — each pool's **Token Out** must equal the next pool's **Token In**, and the last pool must return the **Input Token**. The **Solver** will compute the optimal **Input Amount** for that single path, and you'll get back a **Calculation Result** with per-pool **Swap Amounts**. If you're comparing multiple paths, that's the **Optimizer**'s job — it delegates to the **Solver** per path and picks the best."
>
> **Dev:** "Got it. One more thing — the Aave **Asset** for USDC shows a borrow rate change. Should I call that a pool update?"
>
> **Domain expert:** "No — that's an Aave **Market**, not a **Pool**. The USDC **Asset** is one token's lending state inside that **Market**. A **Pool** is always a DEX contract. Keep the terms separate."
>
> **Dev:** "But the Aave contract is literally called Pool.sol — can I say 'Pool contract' to be specific?"
>
> **Domain expert:** "Yes — **Pool contract** is fine when you mean the on-chain contract. 'The Market's Pool contract emitted a Supply event' is perfectly clear. Just don't use **Pool** alone to mean the lending system — that's always a **Market**."
