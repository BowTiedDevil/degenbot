# Aerodrome — Context & Architecture Design Memo

> Resolves ergo **WFY235** — "Investigate architecture for Aerodrome-style V2
> pools (solidly-stable + on-chain fees) in the three-layer model."
> Decision recorded: **Option A/D (they converge) — strategy-on-`LiquidityPool`
> companion**, with three surgical generalizations. See
> [Design decision](#design-decision-option-ad--strategy-on-liquiditypool-companion).
>
> This doubles as the module's `CONTEXT.md` per `CONTEXT-MAP.md` instructions.

## Terms

- **Aerodrome V2 pool**: A 2-token V2-shaped AMM on Base/Arbitrum whose
  invariant is **per-pool** either constant-product (`stable=False`) or the
  Solidly stable invariant `x³y + xy³ ≥ k` (`stable=True`). Distinct from
  Uniswap V2 in three ways (see [Divergences](#divergences-from-uniswap-v2)).
- **Solidly stable invariant**: `x³y + xy³ ≥ k` (a cubic). Not a Möbius
  transformation, so the closed-form V2-V2 solver does not apply; the
  Solver (see the Arbitrage module context) consumes it as a
  `SolidlyStableHop` carrying a Python-side exact `swap_fn`.
- **Solidly flavor**: Which `k_func` / `get_y_func` pair implements the cubic.
  Two flavors ship today: **Camelot** (`k_camelot` / `get_y_camelot`) and
  **Solidly/Aerodrome** (`calc_k` / `get_y_solidly`). Same invariant shape,
  different arithmetic ordering / convergence check.
- **Per-pool resolved fee**: A swap fee fetched on-chain per pool (Aerodrome:
  `factory.getFee(address, stable)` over `FEE_DENOMINATOR=10_000`; Camelot:
  `token0FeePercent` / `token1FeePercent` over `100_000`). Contrast with a
  **DEX-wide fee** baked into a `DexIdentity` preset (e.g. Uniswap's 3/1000).
- **`DexIdentity` preset**: Frozen deployment-identity value object
  (factory/deployer/init_hash/default fees/reserves ABI/variant tag) in
  `rust/crates/degenbot-uniswap/src/dex_identity.rs`. `AERODROME_V2_VOLATILE`
  and `AERODROME_V2_STABLE` presets **already exist**: factory/deployer on
  Base (8453), `init_hash = B256::ZERO` (Aerodrome uses a different address
  derivation — see [Address derivation](#address-derivation)), and fee
  params `(gamma, 10_000)` with `gamma=0` for stable as the "fetch per-pool"
  sentinel.
- **Companion** (`PyLiquidityPool`): the ADR-005 three-layer handle where Rust
  owns mutable reserves + reorg journal and the Python `LiquidityPool` reads
  them via an atomic `snapshot()`. `UniswapV2Pool`/`CamelotLiquidityPool` were
  converted to companions in slice 4; **Aerodrome was deliberately skipped**
  (no `PyLiquidityPool` handle today, reserves live in Python `StateCache`,
  the pool is **not** in the Rust engine's V2 dirty graph).

## Current state (verified against `main`)

- `aerodrome/pools.py::AerodromeV2Pool` is a 555-line pure-Python pool:
  `class AerodromeV2Pool(PublisherMixin, AerodromeV2PoolStateMixin,
  AerodromeV2PoolCalc, AbstractLiquidityPool)`. Owns state mixin, own log
  decoder (`_decode_aerodrome_v2_sync`), own `StateCache`, own
  `external_update`/`simulate_swap`/`to_hop_state`/`build_swap_amount`.
- `aerodrome/v2_pool_calc.py` wires the calc strategy at construction
  (`_wire_stable_calculations`) — no runtime `if self._stable` dispatch —
  and delegates to `solidly_stable::{calc_exact_in_stable,
  calc_exact_in_volatile, calc_k, get_y_solidly}`.
- `aerodrome/v2_pool_state.py` exposes a **unidirectional** fee
  (`fee_token0 == fee_token1 == self._fee`) and the `stable: bool` flag as
  immutable construction state.
- `builders/aerodrome_v2_builder.py` resolves `stable()` + per-pool
  `factory.getFee(address, stable)` on-chain (delegating to Rust
  `PyBotIo::fetch_aerodrome_v2_stable_and_fee` on the `Bot.build_pool` path),
  then constructs `AerodromeV2Pool` directly — **no `PyLiquidityPool`
  handle**, no `register_v2_pool` call, no Rust `V2PoolState` entry.
- `SolidlyStableHop` (`types/hop_types.py`) carries an optional
  `swap_fn: Callable[[int], int]` used by the arbitrage solver for exact
  evaluation. The Rust engine's V2 hop math is constant-product only
  (`IntHopState`); **all solidly-stable math is already solved Python-side**
  via `swap_fn` — today, by Camelot (`LiquidityPool._camelot_stable_swap_fn`).
- `LiquidityPool` (slice-7 collapsed class, `uniswap/liquidity_pool.py`)
  **already supports the solidly-stable strategy** via the
  `stable_swap: bool` class flag + `fee_denominator: int | None` + a
  `SolidlyStableHop` branch in `to_hop_state`
  (`_calculate_tokens_out_from_tokens_in_stable_swap`, hardcoded to
  `k_camelot`/`get_y_camelot`). This is the slice-7 Camelot fold, and it is
  the structural precedent Aerodrome generalizes.
- `V2PoolState` (Rust, `bot_core/mod.rs`) stores `fee_token0` /
  `fee_token1: (u64, u64)` as **per-pool registration-time state** via
  `RegisterV2PoolParams` — not a DEX-wide constant. So a resolved per-pool
  Aerodrome fee already has a home; no Rust change needed for fee storage.

## Divergences from Uniswap V2

1. **Solidly-stable-vs-constant-product duality** — per-pool, resolved on-chain
   via `stable()`. The *same* `V2PoolState` (reserves) backs both branches;
   the calc dispatch is per-pool.
2. **Per-pool on-chain fee** — `factory.getFee(address, stable)`, returned at
   query time. The `DexIdentity` preset's `fee_tokenN` is only a typical
   default (volatile) or the `gamma=0` "fetch per-pool" sentinel (stable).
3. **`get_pool_identity_values` batch fetch** — identity isn't derivable from
   `(chain_id, factory)` alone; the builder must probe the pool on-chain
   (`stable()` + `getFee` + `getReserves`) before construction. Today this
   already happens in `AerodromeV2Builder.build` and
   `PyBotIo::fetch_aerodrome_v2_stable_and_fee`.

### Address derivation

Aerodrome `init_hash = B256::ZERO` in the preset is documented, not a gap:
Aerodrome does not use the Uniswap-V2 pair CREATE2 formula
(`generate_v2_pool_address` / `_verified_address`). Aerodrome pools are
created by the Aerodrome `Pool` factory under a different derivation. Any
companion conversion must either route address verification through an
Aerodrome-specific derivation or treat the address as authoritative-by-DB
(no `_verified_address` re-derivation). This is an implementation concern for
the follow-up slice, not a structural blocker.

## Candidate architectures (evaluated)

- **A. Strategy-on-companion (minimal lift).** Convert Aerodrome to a
  `PyLiquidityPool` companion like Camelot: `LiquidityPool` carries
  `stable: bool` + a resolved per-pool fee as Python-side pool state (set at
  construction from the builder's on-chain `getFee`), wires the solidly-stable
  calc strategy when `stable`. Rust `V2PoolState` keeps the resolved
  per-pool `fee_tokenN` (already supported via `RegisterV2PoolParams`).

- **B. Separate Rust state variant.** Add `PoolEntry::SolidlyStableV2`
  holding reserves + stable flag + resolved fee + k/get_y function pointers /
  flavor enum. Heavier Rust lift; cleaner separation.

- **C. Third-family framing.** Treat Aerodrome-V2 as a third "family"
  alongside Curve/Balancer (ADR-003's "third family") with its own companion
  + own Rust state.

- **D. Hybrid: companion + per-pool fee override.** Companion carries
  `dex: DexIdentity` (deployment identity — factory/deployer/init_hash,
  which IS DEX-wide) + a separate per-pool `fee: Fraction` + `stable: bool`
  resolved at construction. Strategy wired off `stable` (like Camelot). The
  preset's `fee_tokenN` are only fallbacks when the builder doesn't fetch.

A and D converge: both are "companion + per-pool resolved fee +
strategy-off-stable". D just names the split (immutable `dex` identity vs
mutable per-pool fee) that A leaves implicit. **The choose-one distinction is
cosmetic; the implementation is identical.**

## Design decision: Option A/D — strategy-on-`LiquidityPool` companion

Aerodrome is **2-token V2-shaped** (Sync event, reserves0/reserves1, CREATE2
address). C mis-characterizes it: ADR-003/005's "third family" language refers
to Curve/Balancer (non-2-token, non-V2-shaped) pools. The right frame is "a
V2-family DEX with a per-pool invariant-strategy selector" — exactly the
Camelot fold generalized.

B is unnecessary: the solidly-stable invariant is solved **Python-side** via
`SolidlyStableHop.swap_fn` today (Camelot stable pools already work this way).
The Rust engine only does constant-product V2 math (`IntHopState`); a Rust
`SolidlyStableV2` variant would have no consumer. Adding it is pure surface
with no payoff.

So: **Option A/D**. The existing `LiquidityPool` solidly-stable path, with
three surgical generalizations:

### 1. Generalize the solidly-stable flavor selector

Replace the hardcoded `k_camelot` / `get_y_camelot` pair in
`LiquidityPool.to_hop_state` and `_calculate_tokens_out_from_tokens_in_stable_swap`
with a class-level strategy select. Two viable shapes:

- **Class callables** (preferred — mirrors the existing `k_func`/`get_y_func`
  plumbing in `SolidlyStableHop`): `class LiquidityPool: _stable_k = k_camelot;
  _stable_get_y = get_y_camelot`. An `AerodromeV2Pool(LiquidityPool)` subclass
  overrides `_stable_k = calc_k; _stable_get_y = get_y_solidly`.
- Or a `SolidlyFlavor` enum (`Camelot | Solidly`) dispatched in
  `_calculate_tokens_out_from_tokens_in_stable_swap`.

Camelot's current behavior is preserved bit-for-bit (it picks Camelot's pair);
Aerodrome picks Solidly's pair. `AerodromeV2PoolCalc` already wires these
exact functions (`calc_k` / `get_y_solidly`), so this is a relocation, not
new math.

### 2. Resolved per-pool fee (already supported — no code change)

`LiquidityPool.__init__` already accepts explicit `fee_token0` / `fee_token1`
that override the `dex` preset (explicit > dex > class-default precedence).
An Aerodrome companion sets `fee_token0 = fee_token1 = (gamma, 10_000)`,
where `gamma = 10_000 - fee_raw` is computed from the builder's on-chain
`factory.getFee(address, stable)` result. The preset's `gamma=0` stable
sentinel stays "fetch per-pool" — the builder fills it before construction.
`register_v2_pool` already accepts resolved per-pool `fee_tokenN`, so **no
Rust change** is needed for fee storage.

### 3. Companion conversion (the deferred-from-slice-4 lift)

Give Aerodrome a `PyLiquidityPool` handle so reserves + reorg journal live
Rust-side (as for `UniswapV2Pool` / `CamelotLiquidityPool`) and Aerodrome
syncs reach `BotState::apply_v2_sync` → `engine.dirty_v2` → the Rust solve
graph. The solidly-stable `swap_fn` path stays **Python-side** (no Rust
invariant needed), exactly as Camelot stable does today. This is the real
work of the follow-up slice — but it is the *same companion-conversion
recipe* already executed for the other V2 DEXes; Aerodrome was deliberately
skipped, not structurally excluded.

## What is rejected, and why

- **B (separate Rust variant):** no consumer — solidly-stable math is
  Python-side today; a Rust `SolidlyStableV2` entry adds surface and solves
  nothing. Revisit only if a future need moves solidly-stable solve into Rust.
- **C (third family):** mis-characterizes Aerodrome (which is 2-token
  V2-shaped); "third family" belongs to Curve/Balancer.
- **Speculative Rust `SolidlyStableHop` invariant:** the `swap_fn`-on-Python
  design is the existing, working contract (Camelot). Keep it.

## Validation gates (for the follow-up implementation slice)

- `just test-all` (Rust workspace + PyO3-wrapped + Python).
- Camelot stable parity unchanged: existing Camelot stable tests produce
  identical `swap_fn` outputs before/after the flavor-selector
  generalization (bit-for-bit regression guard).
- Aerodrome volatile paths solve equal to the current pure-Python
  `AerodromeV2Pool` (golden outputs).
- Aerodrome syncs reach `BotState.apply_v2_sync` and dirty the engine's V2
  path set (integration test asserting `dirty_v2` contains the registered
  Aerodrome pool_id after a simulated Sync).

## Relationships

- **Aerodrome V2 ↔ `LiquidityPool`** (slice-7 collapsed class): Aerodrome is
  a `LiquidityPool` subclass selecting the Solidly flavor of the
  solidly-stable strategy (Camelot selects the Camelot flavor). The
  `stable_swap`/`fee_denominator` ClassVars + the new `_stable_k`/
  `_stable_get_y` (or `SolidlyFlavor`) class hook are the per-DEX divergence
  points.
- **Aerodrome fee ↔ `DexIdentity` preset**: the preset's `fee_tokenN` is a
  default/sentinel only; the resolved per-pool fee (from the builder's
  on-chain `getFee`) is authoritative and stored on `V2PoolState` via
  `RegisterV2PoolParams`.
- **Aerodrome ↔ `SolidlyStableHop`**: stable Aerodrome pools produce a
  `SolidlyStableHop` carrying a Python `swap_fn` (binding Solidly's
  `calc_k`/`get_y_solidly`); the Rust engine solves only the constant-product
  V2 hops. Volatile Aerodrome pools produce a `ConstantProductHop`.

## Resolved ambiguities

- **"Is a separate Rust `PoolEntry::SolidlyStableV2` variant required?"** —
  No. Solidly-stable math is solved Python-side via `swap_fn` today; the Rust
  V2 engine only does constant-product. A Rust variant has no consumer.
- **"Is Aerodrome a third family alongside Curve/Balancer?"** — No. It is a
  V2-family DEX (2-token, Sync, reserves0/reserves1) with a per-pool
  invariant-strategy selector. The "third family" framing is reserved for
  non-V2-shaped pools.
- **"Where does the per-pool resolved fee live?"** — On `V2PoolState.fee_tokenN`
  (Rust), set at `register_v2_pool` time from the builder's on-chain `getFee`
  result. Not on `DexIdentity` (which is immutable deployment identity + a
  default/sentinel fee only).