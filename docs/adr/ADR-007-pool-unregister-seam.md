# ADR-007: Pool unregister seam (the symmetric half of register; B2 construction shape settled)

**Status: accepted.** Recorded during the IESD3L session, June 2026. Resolves the "what does
`Bot.build_pool` return" question carried in the IESD3L task body as a B1/B2/B3 open decision,
records why duplicate-registration stays refusal-on-panic, and adds the missing
`unregister_pool` seam so the Python `PoolRegistry` and the Rust `PyBot` stay symmetric about
removal (they are already symmetric about construction). Implementation is tracked as IESD3L
children G2TGP6 (Rust), PDRB42 (Python wire), 3K4ZQF (docs).

## Context

ADR-003 settled that `Bot` (formerly `BotCore`) is the single owner of pool/token state; thin
PyO3 handles (`PyLiquidityPool`, `PyErc20Token`) read it. ADR-005 then settled the **wrapper
topology** — and its Slice 4 implemented what the IESD3L task body called "Option B2": every
`Bot.build_pool` path registers the pool in `PyBot` first, then wraps the returned
`PyLiquidityPool` handle in a Python companion class:

- V2: `src/degenbot/builders/v2_pool_builder.py:146` calls `self._py_bot.register_v2_pool(...)`,
  then `self._py_bot.get_pool(pool_id)`, then constructs the Python `LiquidityPool(py_pool, ...)`.
- V3 and V4 builders do the same for `UniswapV3Pool` / `UniswapV4Pool`, mirroring
  `Erc20Token`-wraps-`PyToken` (the token-side split ADR-003 recorded).
- `EngineRegistry.register_v2_pool` (`src/degenbot/arbitrage/engine_registry.py:147-151`) is
  explicit that its register call would now **panic on the duplicate address** because the pool
  is *already* registered in the shared `BotState` — it caches the shared `pool_id` instead.

So the construction shape — the thing IESD3L was framed as needing to decide — is already the
B2 pattern the task body recommended. B1 (return the raw `PyPool` handle) would break every
library caller of `AbstractLiquidityPool` (`uniswap/trackers.py`, `curve/trackers.py`,
`examples/eth_backrun_*.py`); B3 (delete the `PoolRegistry`, make `BotState` the registry) is a
larger-blast-radius cleanup that is not needed for correctness. The only actually-open gap is the
**missing unregister half** of the seam, which surfaced concretely as a failing test:

`tests/uniswap/test_uniswap_trackers.py::test_pool_remove_and_recreate` does:

```python
uniswap_v2_pool_tracker.remove(pool_address=new_v2_weth_wbtc_lp.address)
bot.pools.remove(pool_address=new_v2_weth_wbtc_lp.address, chain_id=1)   # Python-only removal
super_new_v2_weth_wbtc_lp = uniswap_v2_pool_tracker.get_pool_from_tokens(...)  # re-build
```

`PoolRegistry.remove` (`src/degenbot/registry/pool.py:156`) deletes only the Python wrapper.
`BotState` retains the entry in `pool_addresses: HashMap<Address, u64>` and `pools: HashMap<u64,
PoolEntry>` (`rust/crates/degenbot-bot/src/bot_core/mod.rs:225-227`), because `BotState` has
`register_v2/v3/v4_pool` but **no `unregister`**. Re-`build_pool` calls `register_v2_pool` again
and panics: `pyo3_runtime.PanicException: pool already registered` (the `assert!` at
`bot_core/mod.rs:285` and `:348`). Confirmed pre-existing at clean HEAD; the two halves of the
seam were never built together.

## Decision

Three sub-decisions.

### U1 — B2 is settled; the construction shape is not revisited

`Bot.build_pool` / `build_erc20token` / `build_managed_pool` return the **Python companion
wrapping a `PyLiquidityPool` / `PyToken` handle** (Option B2 in the IESD3L option matrix). This is
what ADR-005 Slice 4 already implemented; this ADR records it as the resolved decision so the
B1/B2/B3 option matrix in the IESD3L task body is closed, not re-litigated. The matrix is retained
only as historical context.

The `Erc20Token`-wraps-`PyToken` pattern is the reference: Rust owns the state it computes on
(reserves, ticks, sqrt_price, token metadata); the Python companion owns I/O orchestration
(subscriptions, the price oracle, display). The pool-side follows the same split.

### U2 — Duplicate-registration stays refusal-on-panic (not idempotent)

`BotState::register_v2/v3/v4_pool` keep their `panic!`/`Err` on a duplicate address
(`mod.rs:285`, `:348`, V4 returns `Err`). They do **not** become "return the existing `pool_id`
if already present." Rationale:

- The panic is load-bearing as an invariant. `EngineRegistry` (`arbitrage/engine_registry.py:144,
  179, 220`) caches `address → pool_id` in `_v2_keys`/`_v3_keys`/`_v4_keys` precisely *because*
  the engine shares `BotState` with the bot and cannot re-register. ADR-006 D3 specified the
  engine never constructs pools; the cache-and-skip is the consequence. Making register
  idempotent would let those caches go stale silently — the divergent state the test is probing
  for would propagate instead of fail fast.
- Removal is the explicit, paired operation: `PoolRegistry.add` is preceded by a `PyBot`
  register in the builders; `PoolRegistry.remove`/`reset` must be **followed by** a `PyBot`
  unregister. The seam is symmetric — symmetric seams are cheap to reason about; "register is
  idempotent but remove is explicit" is not.

ADR-006 D1 records the panic behavior as load-bearing today ("`Bot::register_v2_pool`/
`register_v3_pool` **panic** on duplicate address … so the two registries *must* be separate or
the double-registration flow panics"). ADR-006 then unified the registries; the panic stayed as
the double-registration detector and `EngineRegistry` stopped double-registering. U2 retains the
panic for that detector role under the unified registry.

### U3 — `unregister_pool` seam: address-keyed for V2/V3, `(pool_manager, pool_id)`-keyed for V4

Add `BotState::unregister_pool` and a `PyBot::unregister_pool` PyO3 method:

```rust
// BotState — V2/V3 path (PyBot-exposed) + V4 path (engine-exposed, deferred here)
pub fn unregister_pool(&mut self, address: Address, pool_id: Option<PoolId>) -> bool
// PyBot — V2/V3 only (matches what PyBot exposes today: register_v2/v3_pool, no V4)
#[pyo3(signature = (address, pool_id=None))]
pub fn unregister_pool(&self, address: &str, pool_id: Option<Vec<u8>>) -> PyResult<bool>
```

(The V4 `pool_id: Some` arm is in `BotState` for when the engine-side unregister lands;
`PyBot` itself only takes the V2/V3 path — see above — and returns `PyResult<bool>`
matching `PoolRegistry.remove`'s silent-on-miss contract. `PoolId` is
`degenbot_decoders::v4_swap_decoder::PoolId` — the V4 `[u8; 32]` pool-id type.)

Disposal rules:

- **V2/V3 path** (`pool_id` is `None`): resolve `address` → `u64` via `pool_addresses`, drop the
  `PoolEntry` from `pools` and the entry from `pool_addresses`. For V3, also drain
  `v3_buffer` for that address (else a re-register after a remove would replay stale buffered
  Mint/Burn events onto the fresh pool). The reorg journal lives on the `PoolEntry` and is dropped
  with it — restore for a removed pool is a no-op target (the journal no longer exists).
- **V4 path** (`pool_id` is `Some`): the `address` arg is the **PoolManager contract address**
  (one PoolManager hosts many pool ids — address alone is ambiguous, hence the V4 tuple key).
  Resolve `(address, pool_id)` → `u64` via `v4_pool_ids`, drop the `PoolEntry` from `pools`, the
  tuple from `v4_pool_ids`, and drain `v4_buffer` for the same key (symmetric reason — stale
  buffered `ModifyLiquidity` must not replay onto a re-created pool).

  **V4 on `PyBot`: deferred to the engine-side seam.** `PyBot` does not expose a V4
  `register_v4_pool` today — V4 registration lives on `UniswapArbEngine`
  (`rust/src/py_binding.rs:1332`, invoked via `EngineRegistry.register_v4_pool` where
  `pool.address` is the PoolManager), not on `PyBot`. So `PyBot::unregister_pool` handles only
  the V2/V3 path; the V4 `(address=pool_manager, pool_id)` path lands on `UniswapArbEngine`
  alongside the engine-side unregister that "Consequences" already defers — the V4 removal
  is the matching half of the V4 registration that lives on the engine.
- **Return contract**: `true` if an entry was found and removed; `false` if the address/tuple was
  never registered (silent no-op, returning `false`). Mirrors the Python `PoolRegistry.remove`
  silent-on-miss behavior; the `bool` is for testability and a future engine-key cleanup.
- **`next_pool_id` is not reused.** Removed `pool_id`s are retired: a subsequent re-register
  allocates a fresh `next_pool_id`. This prevents a stale `PyLiquidityPool` handle (retained by a
  Python caller that missed the `remove` signal) from aliasing onto a *different* pool that
  happens to be assigned the recycled id. Retiring ids has no allocation cost (`u64` is wide and
  retired ids are never scanned).

The `v3_buffer` / `v4_buffer` are `LiquidityEventBuffer<K, U>`
(`rust/crates/degenbot-bot/src/optimizers/liquidity_event_buffer.rs`). It exposes
`buffer_backfill`/`buffer_pump`/`drain_backfill`/`drain_pump`/`event_count`/`flush`/`expire` but
**no per-key discard**. U3 adds one: `discard_for(&mut self, key: &K)` — drops all buffered
events for a single key (used by unregister). It is the symmetric inverse of `buffer_pump(key, …)`
+ `buffer_backfill(key, …)`; the existing `flush` is the global variant.

## Considered options

- **U2-alt — idempotent `register_*` returning the existing `pool_id`.** Rejected for the reasons
  in U2: it would mask the divergent state the failing test exists to probe, and would leave
  `EngineRegistry`'s address-keyed caches without their double-registration detector. A failure
  mode that the panic surfaces immediately becomes silent drift between Python `PoolRegistry` and
  Rust `BotState`.
- **U3-alt — make unregister a `panic!` on miss (matching register's panic-on-dup).** Rejected.
  Register panics because a duplicate is a *correctness* event (the caller is about to create two
  sources of truth for one address). Unregister on a miss is benign — the post-condition ("the
  address is no longer registered") already holds; panicking would force Python callers to guard
  every `remove` with a `pools.get(...)` probe. The asymmetry (register panics, unregister is
  lenient) reflects the asymmetry in the operations' invariants, not an inconsistency.
- **U3-alt — key V4 by `pool_id` alone (no PoolManager).** Rejected: V4's PoolManager hosts an
  unbounded number of pools on one contract; a bare `pool_id` collides across managers. The
  existing `v4_pool_ids: HashMap<(Address, PoolId), u64>` already encodes this; U3 mirrors it.
- **U3-alt — reuse `next_pool_id` after unregister.** Rejected (see U3): the recycling savings are
  nil and the aliasing risk is real — a `PyLiquidityPool` handle a Python caller retained through
  an unregister would resolve to a *different* pool after a recycle and silently corrupt reads.

## Consequences

- The Python `PoolRegistry` gains an optional `py_bot: PyBot | None` reference (constructor arg,
  default `None` so tests that construct `PoolRegistry()` standalone still work). `Bot.__init__`
  passes `py_bot=self._py_bot`. `PoolRegistry.remove`/`reset` + `ManagedPoolRegistry.remove` call
  `py_bot.unregister_pool(...)` before/after mutating the Python store (the Rust call is skipped
  when `py_bot is None`, preserving current behavior for non-bot registries).
- `test_pool_remove_and_recreate` passes. This is the acceptance test for U3.
- `EngineRegistry._v2_keys`/`_v3_keys`/`_v4_keys` cache a stale `address → pool_id` after a
  remove that goes through `bot.pools.remove`. **This is a known gap, explicitly out of scope
  here**: no test or production path exercises remove/recreate through the engine today
  (`EngineRegistry` has no `unregister_pool` and no caller). Recorded for a future cleanup; the
  `_v*_keys` dicts would need their own `unregister_pool` that pops the address and, if the engine
  holds per-path solver state referencing that `pool_id`, clears or invalidates those paths. ADR-006
  D3 already says "the engine never constructs pools" — the symmetric engine-side unregister is
  the matching half of *that* decision, separate from U3's Python↔Rust `Bot` seam.
- Reorg correctness for removed pools: `restore_before_block` on a removed `pool_id` is a no-op
  (the entry is gone). A re-created pool with a fresh `pool_id` has only its genesis journal
  delta; it cannot be restored to pre-removal state. This is correct — a removed pool *was*
  removed; there is no pre-removal state to restore *to*. The journal depth and pump reorg
  detection (ADR-003 Option α) are unaffected; they key off `pool_address`/`pool_id` that no
  longer resolve for the removed pool and naturally skip it.

## Related

- **ADR-003** (BotCore as state layer) — `BotState` owns `pool_addresses`/`pools`/`v4_pool_ids`
  that this ADR's `unregister_pool` mutates; the per-pool reorg journal this ADR drops on remove.
- **ADR-005** (Polars-inspired three-layer) — Slice 4 implemented B2 (the wrapper construction
  shape); this ADR records B2 as settled and adds the missing removal half.
- **ADR-006** (Bot as per-chain orchestrator) — D3 ("the engine never constructs pools") is the
  reason `EngineRegistry.register_v2_pool` caches instead of re-registering; U2's panic-on-dup
  remains its detector. The engine-side unregister (out of scope here, see Consequences) is the
  matching half of D3 for removal.

## Deferred

- **Engine-side unregister** (`EngineRegistry.unregister_pool` + path invalidation when a pool
  referenced by a registered path is removed). Out of scope: no caller exercises it.
- **`ManagedPoolRegistry._reset` parity with `PoolRegistry._reset`.** The V4 registry rarely
  holds entries that get reset in bulk; wire it only if a test or caller needs it. Python slice
  (PDRB42) implements only what `test_pool_remove_and_recreate` and symmetry require.
