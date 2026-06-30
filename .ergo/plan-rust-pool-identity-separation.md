# Rust core: separate immutable pool identity from mutable runtime state

## Goal

Make every `PoolEntry` variant `Vx(VxPoolIdentity, VxPoolState)` where
`VxPoolIdentity` is pure immutable registration data (address, tokens, fees,
factory, tick_spacing, A, rate_multipliers, strategy enums, descriptor tag…)
and `VxPoolState` is pure mutable runtime data (reserves/scalars/tick_data +
journal + update_block).

This mirrors `TokenEntry` (already pure identity — address/name/symbol/
decimals/chain_id, no mutable half), completes the half-done ADR-005 split
on V2, and brings V3/V4/Curve/Balancer — which currently have **no** identity
separation at all — to the same uniform shape.

## Why (and what it does NOT do)

### What it buys

1. **Coherence with `TokenEntry` and the ADR-005 doc.** The `V2PoolDescriptor`
   doc comment explicitly says identity belongs distinct from mutable state
   ("Distinct from `V2PoolState` (mutable swap state + the level-2 identity
   it already carries: address/tokens/fees/factory)") — then left
   address/tokens/fees/factory ON the mutable struct. That's an acknowledged
   TODO. This plan completes it.

2. **A single uniform data model.** Today V2 is half-split (state + descriptor);
   V3/V4/Curve/Balancer are fully bundled. After this, every registry entry is
   `(immutable identity, mutable runtime state)` keyed by `pool_id` — the same
   shape as `TokenEntry`. The `BotState` registry becomes coherent: "every
   entry = identity + runtime."

3. **Cleaner handle getter None-semantics.** Today the 6 V2 identity getters
   (`address`/`token0_address`/…/`fee_token1`) and the 3 descriptor getters all
   funnel through `get_v2_pool_state` / `get_v2_descriptor`, returning empty
   string / `(0,0)` / `None` on a deregistered pool. After the split there is
   one `get_v*_identity(pool_id)` accessor for all immutable reads, with the
   "what to return when absent" decision localized to identity reads rather
   than conflated through the mutable-state accessor.

4. **Makes "what's atomic" explicit.** The snapshot methods already extract
   only mutable scalars under one guard; identity is stable and never needed
   in an atomic snapshot. Splitting makes the "identity is not part of the
   atomic mutable snapshot" contract structural, not accidental.

### What it deliberately does NOT do

- **It does NOT fix `test_deleting_pool`.** That test reads `pool.address`
  after `bot.pools.remove(...)`. Under this split, `get_v2_identity(pool_id)`
  still returns `None` after unregister (deregistration forgets the entry —
  correct for a registry). The Python `LiquidityPool` companion caches
  immutable identity at construction (its own post-registration memory), so it
  is unaffected. This refactor is decoupled from that thread — verified: the
  companion holds the cached snapshot; the Rust-only user's `pool_id` is dead
  after unregister by design (they remember identity as a local).
- **It does NOT change lock granularity.** Everything stays behind one
  `RwLock<BotState>`. Splitting identity out does not enable finer locking
  here — that would be a separate, much larger slice. No perf claim is made
  on the lock path. (Verified: no caller clones a whole `*PoolState` just to
  read identity — the snapshot methods already return small tuples of mutable
  scalars, and the solver borrows identity off `&V2PoolState` cheaply. So the
  driver is coherence, not perf.)

## Architecture context (verified)

```
BotState (private deep module, rust/crates/degenbot-bot/src/bot_core/mod.rs)
  pools: HashMap<u64, PoolEntry>           # pool_id -> entry
  pool_addresses: HashMap<Address, u64>     # address -> pool_id
  v4_pool_ids: HashMap<(manager,pid),u64>   # V4 key -> pool_id
  tokens: HashMap<Address, TokenEntry>      # <-- the CLEAN precedent (identity-only)
```

`PoolEntry` (mod.rs:73):
- `V2(V2PoolState, V2PoolDescriptor)` — half-split today
- `V3(V3PoolState)` — fully bundled
- `V4(V4PoolState)` — fully bundled
- `Curve(CurvePoolState)` — fully bundled
- `BalancerWeighted(BalancerWeightedPoolState)` — fully bundled
- `BalancerStable(BalancerStablePoolState)` — fully bundled

Target (per variant): `Vx(VxPoolIdentity, VxPoolState)`.

### Field relocations (per variant)

| Variant | → `VxPoolIdentity` (immutable) | → `VxPoolState` (mutable) |
|---|---|---|
| V2 | address, token0, token1, fee_token0, fee_token1, factory, **+ fold V2PoolDescriptor** (variant, stable_swap, fee_denominator) | reserve0, reserve1, update_block, journal |
| V3 | address, token0, token1, fee, tick_spacing, factory | sqrt_price_x96, liquidity, tick, update_block, tick_data, journal, … |
| V4 | pool_manager, pool_id, pool_key | sqrt_price_x96, liquidity, tick, update_block, tick_data, coverage, journal, … |
| Curve | address, tokens, a_coefficient, fee, admin_fee, rate_multipliers, swap_style, lending_rate_style, d_variant, y_variant, yd_variant, base_pool | balances, update_block, journal |
| BalancerW | address, vault, pool_id, tokens, weights, scaling_factors, swap_fee, pow_version | balances, update_block, journal |
| BalancerS | (mirror BalancerW per its struct) | balances, update_block, journal |

### Migration surface (measured)

- ~110 identity-field reads (`state.address` / `.token0` / `.fee_token0` /
  `.tick_spacing` / `.pool_key` / …) across `bot_core/mod.rs`, the per-variant
  state files, `solvers/`, and `degenbot-python/src/bot/pool.rs`.
- 85 `get_v*_pool_state` / `get_v*_descriptor` call sites.
- **Zero tests assert on raw struct fields** (verified) — all tests go through
  public accessors, so behavior is preserved as long as the accessors keep
  returning the right values. This makes each variant a mechanical,
  compiler-driven migration with no test rewrites expected.

## Slicing strategy

**Tracer bullet first (V2), then one variant per slice.** V2 is already
half-split, lowest-risk, and establishes the pattern + the `get_v*_identity`
accessor convention. Each slice is green (Rust + Python tests) before the next
begins.

### Compiler-as-diff-driver (mandatory)

Per the lesson from the prior V2 epic: **do NOT script struct-literal edits
with regex/brace-matching.** It drifts across comments and mangles files. The
correct procedure for each variant:
1. Edit the state struct (move fields out) + define/rename the identity struct.
2. `cargo build` → read the FIRST error line → hand-fix that one site
   (reroute `state.X` → `identity.X`, or destructure `PoolEntry::V2(identity,
   state)` at the borrow site) → rebuild → next error.
3. Repeat to zero errors. The compiler is the only reliable diff driver for
   "relocate fields across an enum variant."

## Slice breakdown

### Slice 1 — V2 tracer bullet: complete the half-split, retire V2PoolDescriptor

**Define** `V2PoolIdentity { address, token0, token1, fee_token0, fee_token1,
factory, variant, stable_swap, fee_denominator }` (folds the old
`V2PoolDescriptor` fields in — one immutable identity struct per pool,
mirroring `TokenEntry`).

**Slim** `V2PoolState` to `{ reserve0, reserve1, update_block, journal }`
(pure mutable — exactly what `v2_snapshot` already extracts).

**`PoolEntry::V2(V2PoolIdentity, V2PoolState)`.**

**Accessors:**
- `get_v2_pool_state(pool_id) -> Option<&V2PoolState>` — now pure mutable.
- `get_v2_identity(pool_id) -> Option<&V2PoolIdentity>` — NEW; replaces
  `get_v2_descriptor` and serves all identity reads.
- Retire `get_v2_descriptor`.

**Construction** (`register_v2_pool`): build `PoolEntry::V2(V2PoolIdentity{…all
identity incl. variant/stable_swap/fee_denominator…}, V2PoolState{reserve0,
reserve1, update_block, journal})`.

**Handle** (`PyLiquidityPool`, `rust/crates/degenbot-python/src/bot/pool.rs`):
reroute the 9 V2 identity getters (`address`, `token0_address`, `token1_address`,
`factory`, `fee_token0`, `fee_token1`, `variant`, `stable_swap`,
`fee_denominator`) from `get_v2_pool_state`/`get_v2_descriptor` → `get_v2_identity`.

**Solver** (`solvers/uniswap_engine/solver_dispatch.rs`, `diagnostic.rs`): the
identity reads (`state.fee_token0`, `state.address`, `state.token0`,
`state.token1`) borrow off a `&V2PoolState` today. After the split, destructure
`PoolEntry::V2(identity, state)` at the borrow site and read off `identity`.
Compiler-driven per the rule above.

**Exit criteria:** `just test-rust` + `just test-python` green, `just lint-rust`
clean. `V2PoolDescriptor` deleted; `V2PoolIdentity` in place; `V2PoolState`
pure mutable. The `.pyi` stub unchanged (the handle's Python-visible getters
keep their signatures/values).

### Slice 2 — V3

Introduce `V3PoolIdentity { address, token0, token1, fee, tick_spacing,
factory }`. `V3PoolState` → `{ sqrt_price_x96, liquidity, tick, update_block,
tick_data, journal, <seed fields> }`. `PoolEntry::V3(V3PoolIdentity,
V3PoolState)`. Add `get_v3_identity`. The `V3FamilyPool` trait already projects
`fee`/`tick_spacing` off the mutable struct for the reader API — decide
whether it stays on mutable (the trait is a *mutable* read surface, so fee/
tick_spacing arguably belong on mutable since the reader reads current fee;
**NOTE: fee/tick_spacing are immutable config, NOT mutable** — they should
move to identity; the `V3FamilyPool` trait then projects off identity, which
needs the trait re-homing to take identity. **Flag for the slice:** this is the
one non-mechanical decision — the `V3FamilyPool` reader trait reads fee/
tick_spacing as if mutable; they're immutable config; after the split the
trait should borrow from identity. Resolve in-slice.)

**Exit criteria:** Rust+Python green. `V3PoolState` pure mutable.

### Slice 3 — V4

Introduce `V4PoolIdentity { pool_manager, pool_id, pool_key }`. `V4PoolState`
→ `{ sqrt_price_x96, liquidity, tick, update_block, tick_data, coverage,
journal, <seed fields> }`. `PoolEntry::V4(V4PoolIdentity, V4PoolState)`. The
`V3FamilyPool` V4 impl projects fee/tick_spacing out of `pool_key` — same
trait-re-homing note as V3.

**Exit criteria:** green. `V4PoolState` pure mutable (no `pool_key` on it).

### Slice 4 — Curve

Introduce `CurvePoolIdentity { address, tokens, a_coefficient, fee,
admin_fee, rate_multipliers, swap_style, lending_rate_style, d_variant,
y_variant, yd_variant, base_pool }`. `CurvePoolState` → `{ balances,
update_block, journal }`.

**Exit criteria:** green. `CurvePoolState` pure mutable.

### Slice 5 — BalancerWeighted + BalancerStable

Same pattern for both (parallel structs). Introduce
`BalancerWeightedPoolIdentity` / `BalancerStablePoolIdentity`; slim the
`*PoolState`s to `{ balances, update_block, journal }`.

**Exit criteria:** green. Both Balancer `*PoolState`s pure mutable.

### Slice 6 — Doc + invariant check

Update ADR-005 (and the per-variant CONTEXT.md pointers) to record the
achieved invariant: every `PoolEntry` variant is `(VxPoolIdentity,
VxPoolState)`; `VxPoolState` is pure mutable runtime (reserves/scalars/
tick_data + journal + update_block); `VxPoolIdentity` is pure immutable
registration data, mirroring `TokenEntry`. Add a one-sentence note that the
post-deregistration identity-cache concern is the Python companion's
responsibility (by design) and is unaffected by this split. Note the
`get_v*_identity` accessor convention.

**Exit criteria:** docs updated; full `just test-all` + `just lint` green.

## Optional deepening (NOT in the core plan — flag only)

After the per-variant split, a cross-variant `PoolIdentity` trait (projecting
`address`/`token0`/`token1`/`factory` uniformly) could give `PyLiquidityPool`
one identity-read path for all families, mirroring the existing `V3FamilyPool`
reader trait. This is a deep-module move (uniform small interface across
variants). **Defer** until Slices 1–5 land and the per-variant shapes are
stable; revisit as a separate slice if the per-family handle getter
boilerplate proves noisy.

## Risks & mitigations

- **Regex-scripting temptation:** forbidden by the compiler-driver rule above.
  Every variant migrated by edit-rebuild-fix-error.
- **`V3FamilyPool` trait re-homing (Slices 2–3):** the one non-mechanical
  decision — fee/tick_spacing are immutable config the trait currently reads
  off the mutable struct. Resolve in-slice (move to identity; trait borrows
  identity).
- **File-boundary coordination:** verified no other Pi terminal is active
  (only t-c28d), so `rust/` is exclusively mine. If a terminal arrives, hold
  `rust/` exclusively and let it take pure-Python; the per-variant slices are
  Rust-core-local and don't touch `src/degenbot/` beyond the `.pyi` stub
  (unchanged in Slices 1–5).
- **`test_deleting_pool` unchanged:** the split does not touch the
  deregistration lifecycle. The companion's identity cache is the mechanism
  for post-deregistration identity reads; that's by design and out of scope
  here.
- **Behavior preservation:** zero tests assert on raw struct fields, so the
  migration is behavior-preserving as long as accessors return the same values.
  Each slice's gate is the existing Rust + Python suites passing unchanged.