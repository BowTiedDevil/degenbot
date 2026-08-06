# Migration guide: `PyBotIo` retirement disposition (LWKLMP — D0–D5)

Settles the end-state of `PyBotIo` and the Python `builders/` tree now that the
construction choreography has moved core-side. Decisions D0–D5 are recorded
formally in **ADR-023**. This doc is the per-method `stays-python` disposition
table (the LWKLMP deliverable) and the handoff to follow-up epic **`VK3YDM`**.

## Objective restated

LWKLMP does **not** retire `PyBotIo`. It:
1. deletes the already-vestigial Python builder surface (`v2_builder_base.py`);
2. trims `PyBotIo` to a strict `extract → detach → core call → wrap` translator
   (D1/D3);
3. documents the residual `PyBotIo` + `Py*Row` surface per-method as
   `stays-python` (this table);
4. fixes the doc rot left by the wrap-up of slices that never fully landed.

Full retirement is owned by **`VK3YDM`** (Rust ERC-20 + Curve + Balancer-update
core port), which removes the last live Python consumers.

## Current state (verified 2026-08-06)

- Every choreography `fetch_*`/`probe_*` on `PyBotIo` already delegates to
  `degenbot_bot::bot_core::pool_builder::{choreography,curve_choreography,builder}`
  (no GIL round-trip), and the 12 DB + 7 RPC methods delegate through the
  `ConstructionIo` trait objects.
- V2/V3/V4/Aerodrome/Balancer pool construction is Rust-delegated via
  `PyBot.build_v2_pool / build_v3_pool / build_aerodrome_v2_pool /
  build_balancer_weighted_pool / build_balancer_stable_pool / build_v4_pool` +
  `resolve_v4_identity`.
- Loop: the Python fallback (`call_kw` / `provider.getattr`, plus inlined
  `self.alloy` RPC bodies) survives only in the **7 generic RPC methods**. The
  choreography path is already alloy-only (`required_construction_io()`).
- `v2_builder_base.py` is dead in `src/` (only `tests/builders/test_from_chain.py`
  mentions it).

## Disposition table

Disposition keys: **translator** = thin `block_on(core)` seam, keep; **stays-python**
= genuine Python-visible surface with no Rust twin yet, keep for this epic;
**remove** = deleted by this epic (D1/D3/D5); **VK3YDM** = retirement target of
the follow-up epic.

| Method / class | Disposition | Live consumer (why it stays) | Retire in |
|---|---|---|---|
| `fetch_erc20_token` (+ `Erc20TokenRow`) | stays-python | `Erc20Builder` reads `.id/.decimals` | VK3YDM (core erc20) |
| `update_erc20_token_metadata` | translator | `Erc20Builder` write-back | VK3YDM |
| `fetch_pool_row`/`fetch_pool_kind`/`fetch_token_by_id`/`fetch_exchange` | translator | type_resolution / refresh | VK3YDM |
| `fetch_liquidity_positions`/`fetch_managed_*`/`fetch_pool_manager`/`fetch_v4_pool_by_pool_hash`/`fetch_initialization_maps` | translator | V2/V4 refresh paths | VK3YDM |
| `db/pool_read.py` six (`PyLiquidityPoolRow` etc.) | stays-python (D2) | builders read row attrs | VK3YDM |
| `get_block_number`/`get_block`/`get_block_timestamp` | **remove fallback (D1/D3)** → single `construction_io` path | all integer-block | VK3YDM adds tag support |
| `get_code`/`get_balance`/`call`/`call_raw` | **remove fallback (D1/D3)** → single `construction_io` path | curve/balancer reads | VK3YDM |
| `call_kw` / `provider` getter / `provider.getattr` branches | **remove (D1)** | legacy doubles only | — |
| inlined `self.alloy` RPC bodies | **remove (D3)**; keep `alloy_provider()` accessor + field | chain-arm tick-bootstrap; `required_construction_io()` | VK3YDM |
| `fetch_erc20_metadata(_batch)` | translator | `Erc20Builder` (every pool build) | VK3YDM |
| `fetch_token_balance/allowance/total_supply` | translator | `Erc20Builder` + `_bot.py` | VK3YDM |
| `fetch_v2_immutable_data`/`fetch_v2_reserves`/`fetch_camelot_state` | translator | `_bot.py` refresh (`:110`) | VK3YDM |
| `fetch_v3_immutable_data`/`fetch_v3_slot0_liquidity`/`fetch_tick_bitmap`/`fetch_tick_data` | translator | `_bot.py` refresh (`:148`) + `tick_data_fetcher` | VK3YDM |
| `fetch_v4_slot0_liquidity`/`fetch_v4_tick_*` | translator | `_build_v4_managed` + `tick_data_fetcher` | VK3YDM |
| `fetch_curve_pool_params`/`fetch_curve_balances` | translator | `CurvePoolBuilder` | VK3YDM (`build_curve_pool` unexposed today) |
| `fetch_balancer_*` family / `probe_balancer_pool_type` | translator | `BalancerBuilder.update()` | VK3YDM |
| `probe_pool_type` / `fetch_factory_address` | translator | `type_resolution` | VK3YDM |
| `__repr__`/`db`/`database_path` getters | stays-python (introspection) | `_bot.py`/tests | VK3YDM |
| `v2_builder_base.py` (`V2BuilderBase`) | **remove (D5)** | none in `src/` | — |

## Handoff to VK3YDM (the retirement path)

1. Core ERC-20 token builder over `ConstructionIo` (reuse the already-choreographed
   `fetch_erc20_metadata`/`fetch_token_*`); expose via `PyBot` + umbrella.
2. Expose `build_curve_pool` (`pub use` from the `degenbot` umbrella + a
   `PyBot.build_curve_pool` seam); re-platform `CurvePoolBuilder` (construction
   + update).
3. Re-platform `Erc20Builder` + Balancer `update()` core-side.
4. Delete `PyBotIo` + `Py*Row` mirrors once no live Python construction reaches
   them; `_bot.py` becomes a pure delegating shell over `PyBot.build_*`.
5. Add block-tag support to `RpcConstruction` (trait extension, from D1).
6. Extend Tier-0/Tier-1/Tier-2 parity for each crossed family (ADR-005).

## Files touched by LWKLMP

- `rust/crates/degenbot-python/src/bot/py_bot_io.rs` (D1/D3 trim)
- `src/degenbot/builders/v2_builder_base.py` (delete, D5)
- `rust/crates/degenbot-bot/src/bot_core/pool_builder/mod.rs` (stale comment)
- `CONTEXT.md` (false "retires fully" claim), `docs/adr/ADR-023-…`
- `tests/builders/test_pybot_io.py` (no change: the four fallback double classes
  were already dead/never-instantiated and the live tests already use offline
  alloy fixtures — S1 verified)
