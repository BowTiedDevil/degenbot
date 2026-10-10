# ADR-023: `PyBotIo` end-state is a shrunk translator + documented `stays-python` seam; full retirement deferred to the ERC-20/Curve core port (VK3YDM)

**Status: accepted (architecture); D0/D1/D2 disposition superseded 2026-10-10 (see
Post-adoption).** Recorded during the `LWKLMP` grilling
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

### D2 — `Py*Row` mirrors are retired with their consumers (closed 2026-10-10).

`Erc20TokenRow` + the six `db/pool_read.py` classes (`LiquidityPoolRow`,
`PoolKindRow`, `ExchangeRow`, `PoolManagerRow`, `LiquidityPositionRow`,
`InitializationMapRow`) were the Python mirror of `degenbot-db`'s row records,
read by the Python pool builders (`.id/.address/.decimals/.kind/.token0`).
**Closed under `UWA7ZH` (PC6): no Python mirror class remains.** As the builders'
DB reads moved behind the `PyBotIo` seam, each mirror became a thin `#[pyclass]`
over the `degenbot-db` typed record, so `degenbot-db` owns the record in both
directions. The three records no remaining consumer needed (`PoolKindRow`,
`LiquidityPositionRow`, `InitializationMapRow`) were deleted with their `PyBotIo`
methods. The four that are still the DB read shape (`LiquidityPoolRow`,
`ExchangeRow`, `PoolManagerRow`, `Erc20TokenRow`) stay as the FFI face of the
core-owned records, because live Python consumers (`builders/type_resolution.py`,
`builders/erc20_builder.py`, `updater/pool_updater_configs.py`) read their
fields — deleting them would change DB read behaviour, not retire a mirror.

### D3 — Delete the inlined `self.alloy` RPC duplication; keep the `alloy_provider()` accessor.

The redundant inlined RPC bodies are removed (the `ConstructionIo` handle is the
one path), but the `alloy_provider()` accessor + the underlying
`Arc<AlloyProvider>` field are retained — external chain-arm wiring builds an
`AlloyTickBootstrapRpc` from it, and `required_construction_io()` synthesizes
the transient handle over it.

### D4 — The `build_curve_pool` umbrella/Tier-1 gap is VK3YDM's work (closed).

`build_curve_pool`/`build_balancer_*` exist in core but were not re-exported from
the umbrella and not exposed on `PyBot`; no Rust ERC-20 builder existed. LWKLMP
only records the gap so `VK3YDM` closes it. It is now closed: VK3YDM-S2 added a
a core Rust ERC-20 builder (`build_erc20_metadata`) + `PyBot.build_erc20_token`,
and VK3YDM-S3 flat-re-exported `build_aerodrome_v2`/`build_balancer_*`/`build_curve_pool`
from the umbrella (`degenbot` lib.rs) and exposed `PyBot.build_curve_pool`
(construction + update already delegated core-side via `build_curve_pool` +
`fetch_curve_*`).

### D5 — Delete the vestigial Python surface + fix doc rot.

`v2_builder_base.py` (dead in `src/`) is deleted. Stale pointers are corrected:
`pool_builder/mod.rs`'s "future, task 3FVZF4" comment; `GLOSSARY.md`'s "retires
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
- ~~`_bot.py` remains a hybrid: a delegating shell for the Rust-built families and
  a `PyBotIo`-driven host for Curve/ERC-20/Balancer-update until `VK3YDM` lands.~~
  _Superseded 2026-10-10; see Post-adoption A4._

## Post-adoption (2026-10-10) — the deferral premise is overtaken; disposition upgrades from "scope cut" to "finish the collapse"

Recorded from the 2026-10-10 architecture review (Pool construction deepening), which
followed this ADR's `VK3YDM` tracking pointer and found it dangling.

### A1 — `VK3YDM` is not a tracking entity; its work landed anyway.

Zero occurrences in `.ergo/plans.jsonl` / `.ergo/backlog.jsonl`. The code work is
recorded in-tree: D4 (S2/S3), `parity_erc20.rs` (S6), `standalone_consumer.rs` (S2).
The epic ID survives only as comment provenance — a future reader should not look for
it in the task tracker.

### A2 — D0's disposition is superseded: the end-state is full `PyBotIo` retirement.

The stated blocker ("full retirement… is blocked behind the live ERC-20/Curve core
port") does not exist anymore. The core entries shipped; the Python call paths were
never collapsed onto them:

| Core entry (shipped) | Python residue to collapse |
| --- | --- |
| `PyBot.build_curve_pool` | `builders/curve_pool_builder.py`, the `from_handle` prefetch path |
| `PyBot.build_erc20_token` | `builders/erc20_builder.py` |
| `PyBot.build_balancer_{weighted,stable}_pool` | `builders/balancer_builder{,_base}.py` (update path) |

Per-field `stays-python` markings (D2) describe residue, not a terminal state.

### A3 — Re-home the orphaned follow-ups.

D1's block-tag support ("tracked in `VK3YDM`") and D2's `Py*Row` retirement ("folds into
`VK3YDM`") have no tracking home. Both are re-homed under the Pool construction
deepening, ergo epic **`GZPPS3`** (opened 2026-10-10): block-tag support as task
`YCUSGZ` (PC10), `Py*Row` retirement as task `UWA7ZH` (PC6 — closed 2026-10-10,
see D2).

### A4 — The Consequences bullet above is superseded.

"`_bot.py` remains a hybrid … until `VK3YDM` lands" is stale twice: the port landed,
and the hybrid persists anyway. Replacement text: *`_bot.py` is a delegating shell for
the Rust-built families and a `PyBotIo`-hosted residue for Curve/ERC-20/Balancer-update —
residue because the collapse onto the shipped core entries was never finished, not
because the entries are missing.*

### A5 — What stands.

D1's fallback deletion, D3's `alloy_provider()` accessor (caution: provider
*construction* moves to injection as the updater seam deepens; the accessor survives for
chain-arm wiring), D4 as history, D5's doc-rot fixes.
