# Rust: V2PoolDescriptor + PyLiquidityPool identity getters

## Goal
- Add the per-pool immutable descriptor for V2 and expose every identity
  field through `PyLiquidityPool` getters, so the handle is self-describing
  for address/factory/fees/variant/stable_swap/fee_denominator.

## Context
- `V2PoolState` (`rust/crates/degenbot-bot/src/bot_core/mod.rs:155`) already
  holds address/tokens/fees/factory as immutable identity — these only need
  *getters*, no migration.
- `DexVariant` + `stable_swap` + `fee_denominator` are NOT on V2PoolState
  today. Per the locked decisions (B1): they live on a NEW sibling struct
  `V2PoolDescriptor { variant: DexVariant, stable_swap: bool,
  fee_denominator: Option<u64> }` — the home for builder-set immutable
  registration metadata (not pool *state*).
- `DexIdentity` presets + `preset_for_variant` already live in
  `degenbot-uniswap/src/dex_identity.rs` (slice 6). `py_pool.dex` resolves
  the preset from the stored `variant` tag.

### Locked decisions (reference)
- D1 (1A): tokens recovered from the same Bot (handled in the next task).
- D2/D3 (B1): `PoolEntry::V2(V2PoolState, V2PoolDescriptor)`; descriptor is
  the registration-metadata home, not a V2PoolState field. ADR-005's "not a
  field on V2PoolState" rule still holds for the *preset* (level-1 DEX
  data); the per-pool *tag* is a narrower registration echo.
- D4: lift `stable_swap`/`fee_denominator` now (retires the
  `pool.stable_swap = ...` post-construction mutation smell).

## Acceptance Criteria
- New `V2PoolDescriptor` struct in `degenbot-bot`'s `bot_core`
  (`V2PoolState` is the natural neighbor).
- `PoolEntry::V2(V2PoolState, V2PoolDescriptor)`. All existing `PoolEntry::V2`
  match arms updated.
- `RegisterV2PoolParams` extended with `variant: DexVariant`,
  `stable_swap: bool`, `fee_denominator: Option<u64>`.
- `register_v2_pool` stores the descriptor alongside the state.
- `PyLiquidityPool` exposes new read getters: `address` (hex str),
  `token0_address`/`token1_address` (hex str), `factory` (hex str),
  `fee_token0`/`fee_token1` (as `(u64,u64)` tuple), `variant` (kebab str),
  `stable_swap` (bool), `fee_denominator` (`Option<u64>` → `None`/`u64`), and
  `dex` (returns a `PyDexIdentity` via `preset_for_variant(variant)`).
- Existing V2 getters (`reserve0`, `reserve1`, `update_block`, `snapshot`)
  unchanged.
- Rust unit tests cover: descriptor round-trip through register/read; each
  new getter returns the registered value; `dex` getter returns the preset
  matching the registered variant.

## Validation Gates
- `just test-rust`
- `just check-no-pyo3-in-cores`
- `just lint-rust`
- `just format`

---

# Rust: PyLiquidityPool token-recovery getters

## Goal
- `PyLiquidityPool.get_token0()` / `get_token1()` return `PyErc20Token`
  handles (or `None`) by looking up the pool's token addresses in the same
  shared `BotState.tokens` registry.

## Context
- Locked Decision 1 (1A): tokens are recovered from the SAME Bot as the
  pool. `V2PoolState.token0`/`token1` hold the raw `Address`; the
  `BotState.tokens: HashMap<Address, TokenEntry>` registry already exists
  (`bot_core/mod.rs:257`); `PyBot.get_token(address)` already demonstrates
  the lookup pattern (`degenbot-python/src/bot/mod.rs:1119`).
- This enforces the ADR-006 invariant: one Bot per chain owns all assets.
  The test factory's cross-`PyBot` token convenience is a violation to
  fix (next task), not preserve.
- Must run AFTER the descriptor task (variant/getters land first).

## Acceptance Criteria
- `PyLiquidityPool.get_token0()` / `get_token1()` return
  `Option<PyErc20Token>`: read `V2PoolState.token0`/`token1`, check
  `BotState.has_token(&addr)`, build `PyErc20Token::new(core, addr)`.
- `None` return shape mirrors `PyBot.get_token` (address registered but no
  token metadata → `None`; this is the failure mode the test-factory fix
  addresses).
- Rust unit tests: registered token → `Some(handle)`; unregistered token
  address → `None`.

## Validation Gates
- `just test-rust`
- `just check-no-pyo3-in-cores`
- `just lint-rust`
- `just format`

---

# Python: Erc20Token construction guard + _from_py_token

## Goal
- `Erc20Token._from_py_token(cls, py_token, *, oracle_address=None,
  state_cache_depth=8) -> Self` classmethod; `Erc20Token.__init__` raises
  TypeError pointing at `Bot.get_token` / `make_erc20` (mirror the
  `LiquidityPool._from_py_pool` guard already landed).

## Context
- The V2 `_from_py_pool` rewrite (next task) recovers token companions via
  `Erc20Token._from_py_token(py_pool.get_token0())`. That requires the
  Erc20Token seam to exist first.
- Today `Erc20Token(py_token, ...)` is the direct constructor
  (`tests/helpers/erc20_factory.py:38`). Same Polars-style guard pattern
  as `LiquidityPool` (commit `d8e24022`).
- Can run in parallel with the Rust descriptor task.

## Acceptance Criteria
- `Erc20Token._from_py_token` classmethod; body = current `__init__` body
  on `self = cls.__new__(cls)`.
- `Erc20Token.__init__` raises `TypeError` naming `Bot.get_token` /
  `make_erc20`.
- `make_erc20` (`tests/helpers/erc20_factory.py`) switches to
  `Erc20Token._from_py_token`.
- Any production Erc20Token construction sites switch to `_from_py_token`
  (search `Erc20Token(` across `src/`).
- Red/Green test: `Erc20Token()` and `Erc20Token(fake_handle, ...)` raise
  TypeError; `_from_py_token` produces a working token.
- All existing Erc20Token tests pass.

## Validation Gates
- `just test-python`
- `just lint`
- `just format`

---

# Python: slim LiquidityPool._from_py_pool to (cls, py_pool)

## Goal
- `_from_py_pool(cls, py_pool) -> Self` — takes ONLY `py_pool`. Every
  identity field read off the handle. The Polars `_from_pydf` end state
  for V2.

## Context
- Depends on: Rust descriptor+getters (task 1), Rust token getters (task 2),
  Erc20Token._from_py_token (task 3).
- Today `_from_py_pool` takes 10 kwargs. After the Rust work, all are
  recoverable:
  - `address` = `py_pool.address`
  - `factory` = `py_pool.factory`
  - `fee_token0`/`fee_token1` = `Fraction(denom-gamma, denom)` from
    `py_pool.fee_token0`/`fee_token1` (handle returns retained-fraction
    `(gamma, denom)`; companion stores the FEE Fraction — same conversion
    the builder does today).
  - `dex` = `py_pool.dex` (a `PyDexIdentity`)
  - `deployer_address`/`init_hash` from the `dex` preset (the companion
    already does dex-preset fallback for these — now the only path).
  - `token0`/`token1` = `Erc20Token._from_py_token(py_pool.get_token0())`
    / `get_token1()`.
  - `chain_id` = `token0.chain_id`.
- The raising `__init__` guard stays (already landed).

## Acceptance Criteria
- `LiquidityPool._from_py_pool(cls, py_pool) -> Self` signature — no other
  params.
- Body reads every field from `py_pool` getters; no identity passed as args.
- The `dex`-preset fallback block for `factory`/`init_hash`/
  `deployer_address`/`fee_token0`/`fee_token1` simplifies: `dex` is always
  present (from the handle), so the fallback becomes the primary path.
- `stable_swap`/`fee_denominator` read off `py_pool.stable_swap` /
  `py_pool.fee_denominator` (no longer class-level attrs mutated by the
  builder).
- Public behavior unchanged: `reserves_*`, `name`, `simulate_*`,
  `external_update`, `_verified_address`, Camelot stable calc all identical.
- A delegation-style test asserts the companion reads identity from the
  handle (constructed with only `py_pool`; reads propagate through getters).

## Validation Gates
- `just test-python`
- `just lint`
- `just format`

---

# Builders + test factory: descriptor params, slim call sites

## Goal
- Builders pass `variant`/`stable_swap`/`fee_denominator` into
  `register_v2_pool` and call the slimmed `_from_py_pool(py_pool)`.
- `make_v2_pool` fixes the cross-Bot token invariant: tokens register in
  the pool's `PyBot`.

## Context
- Depends on the slimmed `_from_py_pool` (task 4).
- Builders (`v2_pool_builder.py`, `async_v2_pool_builder.py`) already resolve
  a `dex`/`variant`; pass it + Camelot's `stable_swap`/`fee_denominator`
  into `register_v2_pool`.
- The Camelot builder branch (`v2_pool_builder.py:186-187`) currently does
  `pool.stable_swap = ...` / `pool.fee_denominator = ...` post-construction —
  DELETE this; the descriptor carries them, the companion reads them off
  the handle.
- `make_v2_pool` (`tests/helpers/v2_pool_factory.py`): today creates a
  per-call `PyBot()` for the pool while accepting tokens built against a
  separate module-level `_PY_BOT`. Fix: build tokens in the pool's `py_bot`
  (the `py_bot=` param already exists; remove the cross-Bot path). Update
  `make_erc20` call sites in `make_v2_pool` callers if they pass pre-built
  tokens from a foreign `PyBot`.

## Acceptance Criteria
- `v2_pool_builder.py` + `async_v2_pool_builder.py`:
  `register_v2_pool(..., variant=..., stable_swap=...,
  fee_denominator=...)`; `pool = pool_class._from_py_pool(py_pool)`.
- No `pool.stable_swap = ...` / `pool.fee_denominator = ...` post-construction
  mutation remains.
- `make_v2_pool` builds tokens in the pool's `PyBot`; `_from_py_pool(py_pool)`
  with no kwargs.
- All `make_v2_pool` callers that relied on the cross-Bot token convenience
  are updated (pass the pool's `py_bot` to `make_erc20`, or let the factory
  register internally).
- Full V2 + builders + aerodrome + arbitrage + registry test suite green.

## Validation Gates
- `just test-python`
- `just lint`
- `just format`

---

# Docs + cleanup: ADR-005 note, CONTEXT.md, retire dead code

## Goal
- Record the ADR-005 clarification: the per-pool `DexVariant` tag is
  registration metadata echoed for encoding/identity recovery; the *preset*
  remains level-1 DEX data and is not stored per-pool.
- Update `uniswap/CONTEXT.md` if `V2PoolDescriptor` earns a term.
- Retire any now-dead Python identity code.

## Context
- ADR-005 "Placement of DEX identity" says DexIdentity is not a V2PoolState
  field — that rule holds for the *preset*. The per-pool tag is a new,
  narrower thing; document it so the next reviewer doesn't relitigate.
- Run AFTER all code tasks land (task 5).

## Acceptance Criteria
- ADR-005 (or a linked CONTEXT.md note) records the per-pool-tag framing.
- `src/degenbot/uniswap/CONTEXT.md` updated if `V2PoolDescriptor` merits a
  term entry.
- Dead Python identity code removed (no back-compat layer — root AGENTS.md).
  Candidate: `_verified_address` deployer/init_hash derivation if it becomes
  a thin read off `dex` — keep only if still load-bearing.
- `.ergo/` state committed: `plan: complete V2 Polars wrapper`.

## Validation Gates
- `just lint-markdown` (if docs touched)
- `just lint-context-maps` (if CONTEXT.md touched)
- `just test-all`