# Migration Guide: Hop/Encoding Relay Retirement

> **Spike output** for ergo epic `6Y2PBF` (task `2AMXML`).
> Confirms the dead mirror, the relay path, the flatten feasibility, and the
> parity-oracle dispositions. Produces the implementation-task breakdown for
> the epic.

## TL;DR

The redundant Python hop-descriptor relay is **bigger than originally scoped**.
Not just `SwapAmounts.encode()` / `generate_payloads` — the *entire*
`ArbitragePathPool` protocol surface (`build_swap_amount`, `to_hop_state`,
`extract_fee`, the legacy `simulate_swap`), `ArbitrageCalculationResult`, and
`BalancerV2SwapAmounts` are **dead in production**. The driver never calls any
of them; calldata is produced Rust-side by `degenbot_executor::composers::encode_cmd_stream`.
The Python `HopInfo`/`PathInfo` dataclasses are a **marshalling relay**:
built from pool-object attributes (which themselves come from the Rust
`PyLiquidityPool` handle), stored on `EngineRegistry.paths`, then re-extracted
back into Rust via `extract_path_info` (GIL-held `is_instance` per hop) on every
dispatch. The flatten is feasible: identity lives on `BotState`'s `PoolEntry`,
so a core projection can emit the encoder's `PathInfo` directly.

One nuance preserved: the **render path** (`path_info_to_py` reconstructing
Python hops for the `[profit]` log) is a one-directional display reconstruction,
not a state mirror — it can stay (as a display view) or switch to plain dicts.
The **build-side relay** is the part that retires.

## Q1 — Confirm the dead mirror

`rg` for every live (non-test) construction/call of `*SwapAmounts` /
`generate_payloads` / `EncodedCall` / `AbstractSwapAmounts` /
`ArbitrageCalculationResult` / `build_swap_amount`:

| Symbol | Production callers | Test callers | Disposition |
|---|---|---|---|
| `generate_payloads` | **none** | (none direct) | delete + `__init__` re-export |
| `EncodedCall` (py) | **none** (Rust `composers::EncodedCall` is the authority) | (none direct) | delete with the callers |
| `AbstractSwapAmounts` | only the Protocol decl + the subclass defs | `test_swap_amounts_protocol.py` | delete |
| `UniswapV2PoolSwapAmounts` / `V3` / `V4` / `CurveStableSwapPoolSwapAmounts` | constructed only by pool `build_swap_amount` (itself dead in prod) | `test_v4v4_encoding.py`, `test_swap_encoder.py` | §4.3 delete-with-its-tests |
| `BalancerV2SwapAmounts` (`balancer/swap_amounts.py`) | constructed only by `BalancerPool`/`BalancerStablePool.build_swap_amount` (dead in prod) | (none direct) | delete with the build_swap_amount callers |
| `ArbitrageCalculationResult` | **none live** (only `__init__` re-exports + stale `tests/htmlcov/` artifact) | none live | delete + `__init__` re-export |
| `build_swap_amount` (V2/V3/V4/Curve/Balancer/Aerodrome pools) | **none** | `test_v4v4_encoding.py`, `test_arbitrage_path_pool_protocol.py`, `test_pool_protocol_satisfaction.py` | delete from pools + Protocol |

**Critical wider finding.** `build_swap_amount` is part of the
`ArbitragePathPool` Protocol (`src/degenbot/types/pool_protocols.py`). Every
pool implements `build_swap_amount` / `to_hop_state` / `extract_fee` /
`simulate_swap` to satisfy it — yet:

- `to_hop_state` — **zero live callers** (only tests: `test_arbitrage_path.py`,
  `test_curve_legacy_equivalence.py`, `test_solver_hop_builders.py`,
  `test_liquidity_pool_camelot_fold.py`).
- `extract_fee` — only called by `to_hop_state` itself.
- `simulate_swap` (the protocol method) — only the unrelated
  `simulate_swap_with_fetch`/`_with_override` Rust-pyo3 delegation appears.
- `ArbitragePath` **class does not exist** (grepped — gone).

So the *entire* `ArbitragePathPool` / `PoolSimulation` / `ArbitrageCapablePool`
Protocol surface + the `types/hop_types.py` hop-state machinery (`HopType`,
`BoundedProductHop`, `ConstantProductHop`, `V3TickRangeInfo`, `SolidlyStableHop`,
`BalancerMultiTokenHop`, `PoolInvariant`) is legacy parity-oracle machinery,
dead in production, kept alive only by tests.

### Per-callsite §4.3 dispositions

- `src/degenbot/__init__.py` re-exports (`ArbitrageCalculationResult`,
  `EncodedCall`, `generate_payloads`) → **delete**, retire with the symbols.
- `src/degenbot/arbitrage/__init__.py` re-exports (`ArbitrageCalculationResult`,
  `EncodedCall`, `generate_payloads`, `V4PoolKey`) → **delete** (`V4PoolKey`
  moves to a Rust-mirrored pyclass if the encoder still needs it exposed; see Q3).
- `src/degenbot/arbitrage/encoding.py` → **delete** (dead in prod; the
  `FlatComposer`/`NoApprovals`/`PayloadComposer`/`ApprovalStrategy` abstractions
  never reached production — the driver uses Rust `dispatch_profitable`).
- `src/degenbot/arbitrage/types.py` (`AbstractSwapAmounts` + the per-pool
  `*SwapAmounts`) → **delete** with `encoding.py`.
- `src/degenbot/balancer/swap_amounts.py` (`BalancerV2SwapAmounts`) → **delete**.
- `build_swap_amount` on every pool + on the `ArbitragePathPool` Protocol →
  **delete**.
- `to_hop_state` / `extract_fee` on every pool + on the Protocol → **delete**
  (with the `types/hop_types.py` legacy shape; see Q4).
- `src/degenbot/types/pool_protocols.py` → **delete the
  `ArbitragePathPool`/`ArbitrageCapablePool` Protocols** (or shrink to what's
  still live — `PoolSimulation` may have live `subscribe`/`unsubscribe`/
  `simulate_swap` consumers worth verifying before full delete).

## Q2 — The relay cost (honest)

`extract_path_info` (in `rust/crates/degenbot-python/src/executor/mod.rs`)
runs **once per `DispatchCandidate`, per block** on the dispatch path
(`PyDispatchCandidate::#[new]` calls it under the GIL to build
`extract_path_info(path_info, &types)`). It:

1. `PyModule::import("degenbot.arbitrage.hop_info")` + `getattr` of the 4
   `PyType`s (cached per call only — not `PyOnceLock`-cached across calls;
   `HopTypes::load` re-imports each `encode_cmd_stream` / candidate build).
2. iterates `PathInfo.hops`, `is_instance` per hop against `V2HopInfo`/`V3HopInfo`/`V4HopInfo`.
3. `getattr` ~5-8 fields per hop → Rust `HopInfo`.

The driver dispatches at most a handful of profitable candidates per block, so
**the perf cost is negligible** — there is no hot-path motivation. The
motivation is **locality + depth + removing a redundant shape**, not perf.

(There's also a reverse path: `path_info_to_py` / `hop_to_py` reconstruct
Python `PathInfo`/`HopInfo` dataclasses for the `[profit]` log rendering
(`outcome.path_infos` getter, consumed by `_render_profit_logs` /
`_render_sim_failures` in the example). Same negligible cost, one-directional —
see Q3 for the treatment.)

## Q3 — The flatten (feasible)

**Feasible.** The encoder's `HopInfo` is fully derivable from `BotState`
identity:

- `ResolvedHop` (in `degenbot-solvers::mixed`) carries **solve state**
  (reserves / tick sequences / Solidly state), NOT identity.
- `MixedPoolRef` / `PoolHop` (the path-registration types) carry only
  `pool_id: u64` + `zero_for_one: bool`, NOT identity.
- Identity lives on `BotState`'s `PoolEntry`, on the per-family identity
  structs:
  - `V2PoolIdentity { address, token0, token1, fee_token0/1, factory, deployer, init_hash, dex_variant }`
  - `V3PoolIdentity { address, token0, token1, fee, tick_spacing, factory, deployer, init_hash }`
  - `V4PoolIdentity { pool_manager, pool_id, pool_key: V4PoolKey { currency0/1, fee, tick_spacing, hooks } }`

The encoder's `composers::HopInfo` needs exactly the identity fields:
`pool_address`, `token0/1`, `fee`, `zfo` (V2/V3); `pool_manager`,
`pool_id_hex`, `currency0/1`, `fee`, `tick_spacing`, `hook_address`, `zfo` (V4).
100% present on the identity structs. (For V2, `fee_token0/1` is a
`(gamma_numer, fee_denom)` pair — the encoder wants bips-of-10000; the
projection does the same scaling `build_hops_from_pools` does today. For
Solidly hops the encoder currently has NO `SolidlyHopInfo` variant in Rust
`HopInfo` — `extract_hop` rejects it; `SolidlyHopInfo` is informational only
for `path_type` display. The Rust `HopInfo` enum would need a `Solidly` variant
OR the Solidly path stays V2-shaped at encode time (verify in implementation).)

### Where the projection lives (DAG constraint)

`degenbot-bot` does NOT depend on `degenbot-executor` today. The encoder
(`degenbot-executor::composers`) is depended on by `degenbot-python` (optional),
`degenbot-simulation`, and the umbrella `degenbot`. Two clean placement
options for a core `fn path_info_for(path_id) -> PathInfo` projection:

- **Option A — `degenbot-simulation`**: already depends on both
  `degenbot-executor` and (transitively) the BotState identity crates. The
  projection sits beside `simulate_one.rs` / `dispatch_profitable.rs`, which
  already consume `PathInfo`. `PyDispatchCandidate` would receive a Rust
  `PathInfo` (or a `path_id` resolved internally) instead of a Python
  dataclass → `extract_path_info` retires.
- **Option B — `degenbot-bot`**: add `degenbot-executor` as a (non-optional)
  dep. This widens the core's surface but keeps the projection with the state
  owner (ADR-003). Adds a DAG edge `degenbot-bot → degenbot-executor` (verify
  no cycle; `degenbot-executor` is a leaf over `degenbot-abi`/`degenbot-core`
  alloy primitives only, so likely safe).

Either option retires `extract_path_info` + `HopTypes` (the GIL-held
`is_instance` relay) from the encode path. Implementation pick deferred to the
task — both are valid; Option A touches fewer crates.

### The build-side relay vs the render path (the nuance)

- **Build-side relay (retires):** `build_hops_from_pools` (reads pool-object
  attrs → builds Python `HopInfo`) + `engine_registry.paths[path_id]` storage
  of the Python `PathInfo` + `extract_path_info` (re-extracts to Rust). This
  is a pure relay — pool objects get their attributes FROM the Rust
  `PyLiquidityPool` handle, so today's flow is
  `Rust identity → Py pool attrs → Py HopInfo → extract → Rust HopInfo`.
  The flatten cuts it to `Rust identity → Rust HopInfo`.
- **Render path (keep or simplify):** `path_info_to_py` / `hop_to_py`
  reconstruct Python `PathInfo`/`HopInfo` for the `[profit]` log rendering
  (`outcome.path_infos`). This is a one-directional **display reconstruction**,
  not a state mirror — acceptable under ADR-003/ADR-013 (a rendered view is
  not authoritative state). Two sub-options:
  - keep the `hop_info` dataclasses as a **display-only** type (delete
    `build_hops_from_pools`, keep the frozen dataclasses + `path_info_to_py`),
    OR
  - switch `outcome.path_infos` to return plain Python `dict`s and delete the
    `hop_info` dataclasses entirely.

Recommendation: keep the `hop_info` dataclasses as a display type for the first
cutover (lowest risk), delete `build_hops_from_pools` + the relay, then in a
follow-up switch rendering to plain dicts if desired. This splits the work into
two independently-verifiable steps.

### `EngineRegistry.paths` new shape

Today: `dict[int, PathInfo]` where `PathInfo` is the Python dataclass holding
`list[HopInfo]` (the relay). After the flatten:
- **Option A (projection in `degenbot-simulation`):** `EngineRegistry.paths`
  becomes `dict[int, PathInfo_display]` holding only what rendering needs
  (pool addresses/tokens for the `[profit]` log) — the encoder reads the Rust
  `PathInfo` directly from the projection, never from this map. Or the map is
  dropped entirely and rendering reads `outcome.path_infos` (already produced
  Rust-side).
- **Option B (projection in `degenbot-bot`):** `EngineRegistry.paths` could
  hold a Rust `PathInfo` handle (`Py<PyPathInfo>` pyclass exposing the Rust
  type), consumed by both `DispatchCandidate` (no re-extraction) and rendering.

## Q4 — Parity-oracle dispositions (`solvers/hop_types.py` + `types/hop_types.py`)

| Symbol | Live prod callers | Test callers | Disposition |
|---|---|---|---|
| `Solver` / `SolverMethod` / `SolveInput` / `SolveResult` (`arbitrage/solvers/hop_types.py`) | **none** | `test_engine_vs_brent_parity.py`, `test_solver_tagged_hops.py`, `test_solver_hop_builders.py`, `test_quantamm_basket_parity.py` | §4.3 retire with the parity tests |
| `BrentSolver` (`arbitrage/solvers/brent_solver.py`) | **none** (engine owns all solve branches: V2/CL Möbius + Balancer w/stable + Curve + Solidly + QuantAMM basket) | `test_engine_vs_brent_parity.py` (the looser f64 oracle), `test_curve_solver.py`, `test_curve_legacy_equivalence.py` | §4.3 retire — gated on confidence the engine arms are proven (they're shipped; retirement is a confidence call, not a code gap) |
| `BalancerMultiTokenSolver` (`arbitrage/solvers/balancer_multi_token_solver.py`) | **none** (already a delegating shell — math retired to Rust QuantAMM `solve_balancer_weighted_basket`) | `test_quantamm_basket_parity.py` | §4.3 retire the shell + its parity test together (the Rust `#[cfg(test)]` corpus in `balancer_weighted_basket.rs` is the regression set) |
| `HopType` / `BoundedProductHop` / `ConstantProductHop` / `V3TickRangeInfo` / `SolidlyStableHop` / `BalancerMultiTokenHop` / `PoolInvariant` (`types/hop_types.py`, 250 lines) | **none** (consumed only by `to_hop_state` (dead) + the Brent/basket solver oracles + `_solver_utils.py` generic simulators) | `test_arbitrage_path.py`, `test_solver_*`, `test_curve_legacy_equivalence.py` | retire with the `to_hop_state` pool methods + the solver oracles; `_solver_utils.py` generic simulators move/retire with the two kept Curve tests |

**Retirement ordering matters.** `test_curve_legacy_equivalence.py` +
`test_fake_curve_pool.py` reuse `_solver_utils._simulate_mixed_path_int` as a
generic simulator (per `arbitrage/solvers/__init__.py` docstring) — these are
NOT Solidly-specific. Retire them only when their Curve-equivalence purpose is
obsolete (the engine Curve arm is shipped + cross-validated). Until then,
`types/hop_types.py` + `_solver_utils.py` stay as the legacy oracle substrate.

Net: the `solvers/` package + `types/hop_types.py` retire as a UNIT once the
Brent + QuantAMM parity gates are deemed proven enough to delete the oracles.
This is a **separate** sequence from the hop-relay flatten (Q3) — the relay
flatten can proceed first, independently.

## Q5 — ADR-005 / ADR-013 interaction

Consistent — the retirement is *mandated* by both:

- **ADR-003** ("delete, not migrate") — the Python `HopInfo`/`PathInfo` /
  `SwapAmounts` dataclasses mirroring the Rust encoder's intake IS a Python
  mirror of Rust-owned state. The build-side relay retirement is required.
- **ADR-013** (`_ffi` seam is private) — `extract_path_info` lives in
  `degenbot-python` (the binding layer), reaching into
  `degenbot.arbitrage.hop_info` Python dataclasses. Moving the projection
  core-side removes binding-layer-to-companion coupling.
- **ADR-005** (no business logic in the binding layer) — `extract_path_info`
  + `HopTypes::load` (the `is_instance` dispatch logic) is exactly the kind
  of "translates a Python dataclass to a Rust struct" logic the binding layer
  should not hold long-term.

The render-side reconstruction kept as a display view is NOT a state mirror —
it's a rendered projection, not authoritative — so keeping a display-only
`hop_info` dataclass for `_render_profit_logs` does not violate ADR-003.

## Implementation task breakdown (for epic `6Y2PBF`)

Ordered to keep tests green at each step (red-green per sub-step, §3 of the
transition guide). Each is independently shippable.

1. **Delete the dead `SwapAmounts` encoding mirror.** Delete
   `arbitrage/encoding.py`, `arbitrage/types.py` (`AbstractSwapAmounts` +
   per-pool `*SwapAmounts` + `ArbitrageCalculationResult`), `balancer/swap_amounts.py`,
   the `__init__` re-exports. Delete the parity tests
   (`test_v4v4_encoding.py`, `test_swap_encoder.py`, `test_swap_amounts_protocol.py`).
   Keeps `V4PoolKey` if the Rust encoder exposes it (verify).
2. **Delete `build_swap_amount` from the pools + the `ArbitragePathPool`
   Protocol.** Remove the method from every pool + the Protocol declaration.
   Retire `test_pool_protocol_satisfaction.py` / `test_arbitrage_path_pool_protocol.py`
   expectations for `build_swap_amount`.
3. **Flatten the build-side relay.** Land the core projection
   (`fn path_info_for(path_id) -> PathInfo`, Option A in `degenbot-simulation`
   preferred). Reroute `PyDispatchCandidate` to take the Rust `PathInfo` (or a
   `path_id` resolved internally). Retire `extract_path_info` + `HopTypes::load`
   from the encode path. `EngineRegistry.paths` stops storing the Python
   `PathInfo` for the encode path (render path may still read
   `outcome.path_infos`). Keep `hop_info` dataclasses as a display-only type
   fed by `path_info_to_py`.
4. **(Optional follow-up) Switch render path to plain dicts.** Replace
   `outcome.path_infos` Python dataclass reconstruction with plain dicts;
   delete the `hop_info` dataclasses entirely.
5. **Retire the Brent + QuantAMM parity oracles** (separate sequence, gated on
   confidence). Delete `arbitrage/solvers/brent_solver.py`,
   `balancer_multi_token_solver.py`, `solvers/hop_types.py`, `types/hop_types.py`,
   `_solver_utils.py` + their parity tests once the engine arms are deemed
   proven. (Keeps the engine's own `#[cfg(test)]` corpus as the regression set.)

Tasks 1-2 are independent of task 3 and of task 5 — they can proceed in
parallel. Task 4 is a follow-on to task 3. Task 5 is a separate sequence gated
on oracle-confidence, not on the relay flatten.

## Validation gates (per task)

```
just test-rust            # cargo test --workspace
just test-rust-python     # pytest tests/rust
just test-python          # full pytest suite
just lint-rust            # clippy --deny warnings
just check-no-pyo3-in-cores
just lint-python          # ruff + ty
just format               # apply
```

Plus: import probe (`uv run python -c 'import degenbot'`) after each cutover
to confirm the extension rebuilt + the `_ffi` seam still resolves.

## Implementation log (tasks `LYP6L2` + `DU4X3E`)

Landed as two coordinated cuts (pool-method + Protocol deletion `LYP6L2`,
then the type/encoding mirror deletion `DU4X3E`):

- **Deleted pool methods:** `build_swap_amount` (6 pools), `to_hop_state`
  (7 pools — V2/V3/V4/Aerodrome/Balancer weighted/Balancer stable/Curve),
  `extract_fee` (4 `*_pool_calc` + Balancer/Curve pools). The V3/V4
  `_get_tick_ranges`/`_compute_tick_ranges`/`_TICK_RANGE_CACHE` helpers
  (called only by `to_hop_state`) retired with it.
- **Deleted Protocols:** `ArbitrageCapablePool` + `ArbitragePathPool` from
  `types/pool_protocols.py`. `PoolSimulation` / `TwoTokenSwapCalculation` /
  `MultiTokenSwapCalculation` KEPT (live methods).
- **Deleted mirror:** `arbitrage/encoding.py` (incl. the dead Python
  `fits_int128`/`INT128_MAX`/`INT128_MIN` — the Rust
  `composers::fits_int128` owns the real overflow check), `arbitrage/types.py`
  (incl. `V4PoolKey` — only consumed by the deleted `build_swap_amount`; no
  re-home needed), `balancer/swap_amounts.py`. Re-exports removed from
  `degenbot/__init__.py`, `arbitrage/__init__.py`, `balancer/__init__.py`,
  `types/__init__.py`.
- **Tests:** augmented `test_pool_protocols.py` / `test_pool_protocol_satisfaction.py`
  (kept `PoolSimulation`) and `test_arbitrage_path.py` (kept
  `TestV3VirtualReservesIntegerMath`). Deleted: `test_v4v4_encoding`,
  `test_swap_encoder`, `test_swap_amounts_protocol`, `test_int128_range`,
  `test_arbitrage_path_pool_protocol`, `test_pool_adapter`,
  `test_to_hop_state_pair_selection`, `test_liquidity_pool_camelot_fold`,
  `test_solver_hop_builders`.

### Fork resolved (within `LYP6L2` scope: "keep only what's still live; check each")

Removing `to_hop_state` forced retirement of the Brent/Curve solver-oracle
TEST substrate — `test_engine_vs_brent_parity`, `test_fake_curve_pool`,
`test_curve_solver`, `test_curve_legacy_equivalence`. These were scoped to
`LK34EF` (task 5) as "confidence-gated, separate sequence," but they
cannot exist without `to_hop_state`, so they retired here.

### Impact on `LK34EF` (task 5)

`LK34EF` becomes **CODE-ONLY** retirement: `BrentSolver` +
`BalancerMultiTokenSolver` + `arbitrage/solvers/hop_types.py` +
`types/hop_types.py` + `arbitrage/solvers/_solver_utils.py` (the last still
consumed by `brent_solver.py`). `BrentSolver` is transiently untested
pending `LK34EF` (dead-in-prod; acceptable). The "confidence-gating" is
moot: the engine solve arms are shipped; the oracle tests are gone.

### Gates

`just lint-python` (ruff + ty) clean; `just test-python` green (2600 passed);
Rust-Python seam tests green; `just lint-markdown` clean.
