# Migration guide: Construction-I/O choreography absorption (scope & ordering)

Spike delivery for task **W5DXGB** (epic `Z5CNPB`). This scopes and orders
the **builder-choreography port** named as the follow-up in
[`construction-io-trait.md`](construction-io-trait.md): moving the ~22
choreographed encode→call→decode wrappers off `PyBotIo` and into the Rust core
as pure functions over `ConstructionIo`, then deleting the temporary Python
seam (`PyBotIo.provider`, `forward_call_to_provider`, the non-alloy fallback).

## Objective

A standalone `cargo add degenbot` consumer must be able to fetch + decode every
piece of on-chain pool/token/curve/balancer construction data using only core
crates, and a Python-driven bot must register pools with **zero** remaining
Python construction logic. The atomic `ConstructionIo` surface (7 RPC + 12 DB,
already landed) is the substrate; the choreography wrappers are the next layer
up that must become core functions, composed by the `PoolBuilder` (task
`3FVZF4`).

## Current state (slice A landed)

- `degenbot-bot/bot_core/construction_io/`:
  - `trait DbConstruction` — 12 construction-time DB reads/writes returning
    `degenbot_db::rows::*` core row types, propagating `DbError` loudly.
  - `trait RpcConstruction` — `get_block_number` / `get_block` /
    `get_block_timestamp` / `get_code` / `get_balance` / `call` (alloy-shaped,
    propagating `ProviderError`).
  - adapters `NoDb`, `DegenbotDbConstruction` (held `DegenbotDb`),
    `AlloyRpcConstruction` (wraps `degenbot-rpc` `AlloyProvider`).
  - composite `ConstructionIo { db: Arc<dyn DbConstruction>, rpc: Arc<dyn
    RpcConstruction> }` held by `Bot`, attached via
    `Bot::set_construction_io` / `PyBot::attach_construction_io`.
- `degenbot-python/src/bot/py_bot_io.rs` still holds the choreography: **22
  wrapper fns** + helper fns (`fetch_no_arg_uint`, `fetch_no_arg_int`,
  `fetch_address_returning_method`, `forward_call_to_provider`,
  `decode_dynamic_array_words`) that compose over the temporary
  `forward_call_to_provider` / `self.provider` seam.

### Wrappers to absorb (classified by family)

| Family | Wrappers | Underlying atomic methods |
|--------|----------|---------------------------|
| ERC-20 | `fetch_erc20_metadata`, `fetch_token_balance`, `fetch_token_allowance`, `fetch_token_total_supply` | `call` + `DbConstruction` metadata write-back |
| V2 | `fetch_factory_address`, `fetch_v2_reserves`, `fetch_camelot_state` | `call` |
| V3 | `fetch_v3_immutable_data` (factory/token0/token1/fee/tickSpacing), `fetch_v3_slot0_liquidity` (slot0 4/6-field + liquidity), `fetch_tick_bitmap`, `fetch_tick_data`, `fetch_missing_tick_word` | `call` |
| V4 | `fetch_v4_slot0_liquidity` (getSlot0 `syscall`-style with `bytes32 pool_id`) | `call` |
| Curve | `fetch_curve_pool_params`, `fetch_curve_balances` | `call` + DB |
| Balancer | `fetch_balancer_pool_id`, `fetch_balancer_swap_fee`, `fetch_balancer_amp`, `fetch_balancer_weights`, `fetch_balancer_rate_providers`, `fetch_balancer_vault_tokens`, `fetch_balancer_rate`, `probe_balancer_pool_type` | `call` |
| Type probe | `probe_pool_type` (slot0→getReserves→getPoolId/getNormalizedWeights→stableswap) | `call` |

All encode/decode already route through `degenbot_rpc::abi` (shared with
`AlloyTickBootstrapRpc`, the standalone consumer path), so the choreography
stays **byte-identical** across the pyo3 adapter and the pure-Rust consumer.

## Target shape

Each wrapper becomes a **free-standing async core function** over
`&ConstructionIo` (or over the concrete RPC + a lightweight DB handle),
returning decoded core types — never `Py*` mirrors. Home:
`degenbot-bot/bot_core/pool_builder/` (the module built by `3FVZF4`), with the
pure encode/decode staying in `degenbot_rpc::abi`. Signature principle:

```text
async fn fetch_v3_slot0_liquidity(io: &ConstructionIo,
    address: Address, block: Option<u64>)
    -> Result<(U256 /*sqrt*/, i32 /*tick*/, u128 /*liq*/), ...>;
```

The `PyBotIo` public functions that Python `builders/` still call become thin
pyo3 adapters that `block_on` the core fn (release GIL → drive future → wrap
result) — no business logic at the seam.

## Ordering

1. **Port the shared helpers + call-return decode** (encode/decode already in
   `degenbot_rpc::abi`; add a small `decode_call_return` + ABI-decode helper
   that both the core fns and the pyo3 seam share).
2. **Port each family wrapper core-side**, re-pointing the corresponding
   `PyBotIo` method to the new core fn (behavior-identical; Python builders
   keep working unchanged). Land families in the dependency order the
   `PoolBuilder` needs: ERC-20 → V2 → V3 → V4 (V3/V4 for the MEV core), then
   Curve/Balancer (deferred by the epic's decision D-C — their wrappers can be
   ported later without disturbing the V2/V3/V4 path).
3. **Delete the temporary seam** once every wrapper is re-pointed:
   `PyBotIo.provider`, `PyBotIo.forward_call_to_provider`, the non-alloy
   provider fallback, and the now-dead helper fns. This is the point at which
   `PyBotIo`'s choreography surface is gone and Python construction is a pure
   driver over Rust.
4. **Compose** the absorbed wrappers into `PoolBuilder` orchestration
   (task `3FVZF4`), which is where family dispatch + assembly live.

## Validation

- Behavior must be byte-identical: reuse `degenbot_rpc::abi` encode/decode;
  assert against fixtures via the existing Rust test conventions.
- Tier-1 reachability: the new core wrapper fns must be `pub use`-reachable
  from the umbrella (`degenbot::bot_core::pool_builder::*`) so the pyo3
  binding and a standalone consumer both reach them; `reachability.rs` must not
  regress (no undocumented cross-boundary import).
- Tier-2 dual-driver parity: where a wrapper crosses the FFI boundary, extend
  `rust/crates/degenbot/tests/parity_*.rs` + `tests/standalone_parity/` with
  the shared-fixture shape per ADR-005.

## Files

- New: `rust/crates/degenbot-bot/src/bot_core/pool_builder/{mod,choreography,abi_helper,tests}.rs`
- Moved-from/re-pointed: `rust/crates/degenbot-python/src/bot/py_bot_io.rs`
  (the 22 wrapper fns → thin adapters; then seam deletion)
- Adapters: unchanged (the 7 RPC + 12 DB atomic surface already composes the
  absorb.
