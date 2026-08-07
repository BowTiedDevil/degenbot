# VK3YDM design: core ERC-20 construction + the return-shape map

Spike deliverable for epic `VK3YDM` (RS1 `KSEHWC`). Written after the `LWKLMP`
trim (commits `17661f64` / `a45a9f32` / `1c67013e`) so it reflects the current
`PyBotIo` translator surface. It pins the Rust-core return shapes and the
assembly/registration design for the family-port slices so `S2` (erc20), `S3`
(curve) and `S4` (balancer-update) can be written without re-deciding.

## 1. The core finding: registration already exists; only the *assembly* needs porting

The heavy lifting that a naive "port the Erc20 builder" would assume is **already
done**, and the boundary is cleaner than the disposition table implies:

| Piece | Where it lives today | Status |
|---|---|---|
| `BotState::register_token(addr, name, symbol, decimals, chain_id)` | `rust/crates/degenbot-bot/src/bot_core/mod.rs:4346` — pure Rust insert into `tokens: HashMap<Address, TokenEntry>` | **DONE** |
| `PyBot.register_token` | `rust/crates/degenbot-python/src/bot/mod.rs:2434` — `py.detach(lock; write().register_token(...))` then wraps `PyErc20Token` | **DONE** |
| `PyBot.get_token(addr) -> Option<PyErc20Token>` | `bot/mod.rs` (~after 2470) | **DONE** |
| `PyErc20Token` thin handle | `rust/crates/degenbot-python/src/bot/token.rs` — getters `address/decimals/symbol/name` read live from the `TokenEntry` (no metadata copy) | **DONE** |
| Public Python entry `Bot.build_erc20token(address)` / `Bot.get_token` | `src/degenbot/bot/_bot.py:491` → `self._erc20_builder.build(...)` | shell entry exists; orchestration still Python |
| The **assembly** (DB-first → on-chain → write-back → register) | `src/degenbot/builders/erc20_builder.py::Erc20Builder.build` | **the port target (the "no Rust twin" part)** |

So the core port is **not** a new token type or registry — it is a new
*choreography* function that reproduces `Erc20Builder.build` steps 3–5 (the
metadata resolution), leaving registration where it already is, in the `PyBot`
seam via `BotState::register_token`.

## 2. The core ERC-20 choreography fn (S2 deliverable)

Add to `rust/crates/degenbot-bot/src/bot_core/pool_builder/` (a new `erc20.rs`
module alongside `choreography.rs` / `curve_choreography.rs`):

```rust
/// Resolve token metadata DB-first, then on-chain, with UNKNOWN fallbacks.
/// Mirrors Erc20Builder.build steps 3-5: read the DB row (fetch_erc20_token);
/// prefer its name/symbol/decimals; backfill any missing field on-chain via
/// name()/NAME(), symbol()/SYMBOL(), decimals()/DECIMALS() with the
/// UNKNOWN_* sentinels; and report whether a DB write-back is warranted.
pub async fn build_erc20_metadata(
    io: &dyn ConstructionIo,        // or the handle type the siblings use
    chain_id: u64,
    address: Address,
    block: Option<u64>,
) -> Result<(String /*name*/, String /*symbol*/, u8 /*decimals*/, bool /*write_back*/), PoolBuilderError>;

/// Single call that resolves + registers: metadata above, then
/// state.write().register_token(...). Kept separate from build_erc20_metadata
/// so the metadata logic is testable without a BotState.
pub async fn build_erc20_token(
    state: &BotState,               // write side
    io: &ConstructionIo,
    chain_id: u64,
    address: Address,
    block: Option<u64>,
) -> Result<Address, PoolBuilderError>;
```

Reused core choreography (already in `choreography.rs`, no new RPC code):
- `fetch_erc20_token(chain_id, address) -> Option<degenbot_db::rows::Erc20TokenRow>` (DB-first; `.id` is used only for the write-back decision).
- `fetch_erc20_metadata(address, block) -> (name, symbol, decimals)` (batched 3-selector read).
- `fetch_erc20_string_field(address, signature, block)` / `fetch_erc20_uint_field` (the `NAME()`/`SYMBOL()`/`DECIMALS()` alternate-prototype fallback).
- `get_code(address, block)` (the "no contract deployed" guard).

The `UNKNOWN_NAME` / `UNKNOWN_SYMBOL` / `UNKNOWN_DECIMALS` sentinels (`16` decimals
fallback) and the per-field `(lower, UPPER)` prototype fallback loop are ported
verbatim from `erc20_builder.py`.

### PyBot seam (S2, thin driver — no business logic)

`PyBot.build_erc20_token(address, chain_id, block=None) -> PyErc20Token`:
1. `parse_address`; `required_construction_io()` (or the core fn's io arg).
2. `py.detach(|| get_runtime().block_on(builder::build_erc20_token(state, io, chain_id, addr, block)))`.
3. If `write_back`, call via the held `DegenbotDbConstruction` (the `update_erc20_token_metadata` path) — or return a flag for the Python shell to write back.
4. Wrap `PyErc20Token::new(self.bot.state_arc(), addr)` (same as `register_token`).

### Python shell stays (S2 — L3 companion, not a re-implementation)

`Erc20Builder.build` keeps only the **companion** concerns (ADR-005 L3 + `stays-python`):
- the Python `TokenRegistry` `get_or_add` idempotency (35NMBX Guard 1) + registry-first short-circuit;
- the `EtherPlaceholder` special case;
- `Erc20Token._from_py_token` (the Fraction-display wrapper over the `PyErc20Token` handle — `erc20/erc20.py` already reads everything through the handle, no metadata copy).

The DB-first/on-chain/UNKNOWN/write-back orchestration (steps 3–5) moves to the
core `build_erc20_metadata`.

## 3. Return-shape map (the per-family design)

Dispositions use the ADR-005 table. The rule: **the core returns a core row/struct
or nothing; `Py*` mirrors (and their `to_py`/`u256_to_py`/`create_hexbytes` wraps)
live only in the binding.** Everything the Python companion needs reachable as a
core-returned value, never as a hand-rolled `Py*`.

| Live consumer | Today (PyBotIo) | Core return shape (S2–S4) |
|---|---|---|
| `Erc20Builder.build` | `fetch_erc20_token` + `fetch_erc20_metadata(_batch)` + `get_code` + `update_erc20_token_metadata` | `build_erc20_metadata` (+ `build_erc20_token`) → `(name, symbol, decimals, write_back)` + `Option<Erc20TokenRow>` |
| `Erc20Builder.get_token_*` | `fetch_token_balance/allowance/total_supply` | keep as core choreography fns; Python companion calls core fn, wraps `u256` |
| `CurvePoolBuilder` (S3) | `fetch_curve_pool_params/balances` | `build_curve_pool` (exists, unexposed) returns core curve pool state; `update()` reuses the currency choreography |
| `BalancerBuilder.update()` (S4) | `fetch_balancer_*` + `probe_balancer_pool_type` | a core `build_balancer_*` update fn over the existing `fetch_balancer_*` choreography; returns the updated core scalars |
| `tick_data_fetcher` / `type_resolution` / `_bot.py` refresh | `fetch_v{2,3,4}_*`, `fetch_tick_*`, `probe_pool_type` | already core-choreographed; only for `S5` (retire) |

No new `Py*Row` mirrors are added. The six `db/pool_read.rs` mirrors + `Erc20TokenRow`
are retired in `S5` with `PyBotIo`.

## 4. Decisions recorded for the slices

- **D-VK1 (erc20):** registration (`BotState::register_token`) + `PyBot.register_token` stay as-is; `S2` adds only the assembly choreography + the thin `PyBot.build_erc20_token` seam. The Python `TokenRegistry`/`EtherPlaceholder`/`_from_py_token` companion stays `stays-python`.
- **D-VK2 (curve):** **in-scope.** `build_curve_pool` (existing, `builder.rs:731`) is `pub use`'d from the umbrella + exposed on `PyBot.build_curve_pool`, and `CurvePoolBuilder` (construction + update) re-platforms onto it. The curve `update()` path (`fetch_curve_pool_params/balances`) is part of `S3`, not carved out — it is exactly the live surface that blocks `PyBotIo` retirement.
- **D-VK3 (balancer):** **in-scope.** `BalancerBuilder.update()` re-platforms core-side in `S4`; construction is already `build_balancer_weighted/stable` on `PyBot`.
- **D-VK4 (return shapes):** core returns core types only; the binding wraps to Python; no new `Py*` mirrors.
- **D-VK5 (Tier-1):** each umbrella-visible symbol is `pub use`'d (or allowlisted with a migration note) or `reachability.rs` fails → `S2`/`S3` update `degenbot/src/lib.rs:134`.

## 5. Validation for the slices

- `S2`: `just test-standalone` reaches `build_erc20_token`; `cargo test -p degenbot --test reachability` (Tier-1); `just test-python` builders green; `just check-no-pyo3-in-cores` + clippy.
- `S3`: `test_curve_build_parity.py` re-platformed and green; reachability green.
- `S6`: Tier-2 dual-driver erc20 + curve (+ balancer-update) parity pairs added per ADR-005 fixture rule.
