# ADR-023: `PyBotIo` end-state is a shrunk translator + documented `stays-python` seam; full retirement deferred to the ERC-20/Curve core port (VK3YDM)

**Status: accepted (architecture).** Recorded during the `LWKLMP` grilling
(2026-08-06), settling decisions D0–D5: the disposition of `PyBotIo` and the
Python `builders/` tree once the construction choreography moved core-side.

## Context

`PyBotIo` (the `#[pyclass]` I/O façade) was framed in the `ConstructionIo`
slice-A migration guide as something that "retires fully once the 27
choreography wrappers move core-side." The wrappers did move core-side (FJQRH3 /
F2R2OC / 3FVZF4, epic `Z5CNPB`): every choreography method now delegates to
`degenbot_bot::bot_core::pool_builder::{choreography,curve_choreography,builder}`
over the `ConstructionIo` trait, and V2/V3/V4/Aerodrome/Balancer pool
construction is Rust-delegated via `PyBot.build_*_pool`. Investigation corrected
the premise: `PyBotIo` is **not** vestigial. It remains the live I/O executor for
genuine Python construction that has no Rust twin yet — the `Erc20Builder`
(token construction + balance/allowance/supply reads), `CurvePoolBuilder`
(Curve construction + update), `BalancerBuilder` (Balancer `update()`), the
`tick_data_fetcher`/`type_resolution` helpers, and `_bot.py`'s own
slot0/reserves refresh. Only the V3/V4/Aerodrome Python pool builders are
deleted, and `v2_builder_base.py` is dead in `src/`.

## Decision

### D0 — LWKLMP scopes to the defensible end-state, not full retirement.

`PyBotIo` is trimmed to a strict `extract → detach → core call → wrap`
translator; the already-vestigial Python builder surface is deleted; and the
residual `PyBotIo` + `Py*Row` surface is documented **per-method** as
`stays-python` under the ADR-005 rubric. Full `PyBotIo` retirement is **out of
scope** — it is blocked behind the live ERC-20/Curve/Balancer-update Python
construction and is owned by follow-up epic **`VK3YDM`**
("Rust ERC-20 + Curve construction port"). This is a deliberate deviation from
the slice-A "retire fully" framing.

### D1 — Delete the non-alloy Python fallback; block-tag support is a VK3YDM trait change.

The temporary seam (`call_kw`, `provider.getattr`, and the `provider`/`self.alloy`
routing branches in `get_block_number` / `get_block` / `get_block_timestamp` /
`get_code` / `get_balance` / `call` / `call_raw`) is deleted; all RPC methods
route through the `ConstructionIo` handle as the single path (with the transient
`(NoDb, AlloyRpcConstruction)` over `self.alloy` for bare fixtures). The
`"latest"` block-tag fallthrough dies with it — `RpcConstruction` has no tag
support. Tag support is a core-trait change tracked in `VK3YDM`, not a LWKLMP
blocker. Legacy Mock/MagicMock test doubles move onto alloy fixtures.

### D2 — `Py*Row` mirrors are `stays-python` for this epic.

`Erc20TokenRow` + the six `db/pool_read.py` classes are live (Python builders
read `.id/.address/.decimals/.kind/.token0`). They stay; their retirement folds
into `VK3YDM` with their consumers.

### D3 — Delete the inlined `self.alloy` RPC duplication; keep the `alloy_provider()` accessor.

The redundant inlined RPC bodies are removed (the `ConstructionIo` handle is the
one path), but the `alloy_provider()` accessor + the underlying
`Arc<AlloyProvider>` field are retained — external chain-arm wiring builds an
`AlloyTickBootstrapRpc` from it, and `required_construction_io()` synthesizes
the transient handle over it.

### D4 — The `build_curve_pool` umbrella/Tier-1 gap is VK3YDM's work.

`build_curve_pool`/`build_balancer_*` exist in core but are not re-exported from
the umbrella and not exposed on `PyBot`; no Rust ERC-20 builder exists. LWKLMP
does not touch this — it only records the gap so `VK3YDM` closes it
(`pub use` from the umbrella + `PyBot` seam + a new core erc20 builder).

### D5 — Delete the vestigial Python surface + fix doc rot.

`v2_builder_base.py` (dead in `src/`) is deleted. Stale pointers are corrected:
`pool_builder/mod.rs`'s "future, task 3FVZF4" comment; `CONTEXT.md`'s "retires
fully once the 27 wrappers move core-side" (now false); and `LWKLMP`'s
References claim that builders "receive `&ConstructionIo`" (they receive
`PyBotIo`).

## Consequences

- A future reader will see `PyBotIo` fully intact and (without this ADR)
  conclude the "retirement" was abandoned. This ADR states it was a deliberate
  scope cut, not failure, and points at `VK3YDM`.
- `tests/builders/test_pybot_io.py` models the removed three-tier fallback with
  four dead double classes (`_FactoryCallProvider` / `_Erc20MetadataProvider` /
  `_AddressArgProvider` / `_V2PoolProvider`) — they are never instantiated; the
  live tests already run on offline alloy fixtures (where the transient
  `(NoDb, AlloyRpcConstruction)` handle serves them). D1/D3 remove the path
  those doubles modeled, so no test change was required (S1).
- `_bot.py` remains a hybrid: a delegating shell for the Rust-built families and
  a `PyBotIo`-driven host for Curve/ERC-20/Balancer-update until `VK3YDM` lands.
