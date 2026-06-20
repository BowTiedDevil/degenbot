# Plan 102: Consolidate bot startup to one canonical path

## Overview

Collapse the two bot-startup entry points into one. The Rust library exposes
`subscribe()` → `backfill_from_snapshot()` → `resume()` (the documented
two-phase ADR-006 sequence); the *orchestration* of that sequence — the
`EngineRegistry` that orders the phases, derives the backfill target, configures
verification, maps Python pools to engine `pool_id`s, and builds paths — is the
one canonical "way to start," promoted out of the backrun example into
`degenbot.arbitrage`. The competing one-shot `PyUniswapArbEngine::start()` /
`BlockPump::spawn` is deleted, and the V4 hook/dynamic-fee pool-admission rule
becomes a typed Rust exception (single source, no Python duplicate) so
`build_paths` classifies by type instead of fragile string matching.

## Problem

### Deletion test

If you deleted `PyUniswapArbEngine::start()` / `BlockPump::spawn`: **nothing
breaks.** `spawn`'s only caller is `start()`; `start()` has **zero Python
callers** (every example/test/src path uses `EngineRegistry.start()` →
`subscribe`/`backfill` + `resume`, never `engine.start()`). The `.pyi` stub
doesn't even declare `start`. The one-shot is dead surface that competes with
the canonical flow for the reader's attention — so delete it, don't reorganize
it.

If you deleted the Python-side V4 hook/dynamic-fee pre-check in
`EngineRegistry.register_v4_pool`: **also nothing breaks.** Rust's
`BotState::register_v4_pool` (`mod.rs:1427-1439`) already refuses amount-
modifying-hook and dynamic-fee pools with `Err(String)` → `PyValueError` at the
PyO3 seam. The Python pre-check is a **pure duplicate** that exists only to
emit a friendlier message string — calcification, not load-bearing.

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|--------------|
| Two "start the bot" entry points | `PyUniswapArbEngine::start()` vs `EngineRegistry.start()` | A reader can't tell which is canonical; the one-shot has no callers but the most-visible doc (`docs/architecture/rust-owned-bot.md:18` startup diagram) shows `→ engine.start(rpc_url)` |
| `BlockPump::spawn` labeled "legacy" but not deprecated | `block_pump.rs:224` (pre-existing doc) | Implies slated-for-removal that was never the intent; the two-phase split exists to insert backfill + consumer attachment, not to mark `spawn` for deletion |
| `EngineRegistry` (the canonical orchestrator) lives in the example | `examples/eth_backrun_v2_v3_v4_rust.py:724`; tests import it via `import examples.eth_backrun… as runner` | The library ships no in-package "way to start"; every operator copies the example class |
| V4 hook filter expressed twice (Python `ValueError` + Rust `Err(String)`) | `examples:867-880` + `rust/bot_core/mod.rs:1427` | The Python copy must be kept in sync by hand; lifting it to the library would calcify the duplication |
| `build_paths` classifies V4 rejections by string-match | `build_paths` `except ValueError` matches `"amount-modifying hooks"` / `"dynamic fees"` | Same fragile pattern `7SSOJX` already removed for verification errors; not type-safe |

## Solution

### Step 1 — Delete the legacy one-shot

Remove `PyUniswapArbEngine::start(rpc_url)` (`py_binding.rs:1480`) and
`BlockPump::spawn` (`block_pump.rs:229`). Note the **distinct** `SolveCoordinator::start()`
(the drain-lock divergence assertion, still called from `resume()` at
`py_binding.rs:1742`) and `Bot::start()` (`mod.rs:2076`, an `unimplemented!()`
ADR-006 slice-5 placeholder, `#[allow(dead_code)]`) are NOT the same method and
stay. Repurpose the `legacy_spawn_processes_blocks_in_order…` test
(`block_pump::tests`) to `resume_anchors_to_subscribe_block` — the
`run_with_stream` anchoring invariant it actually exercises survives; only its
name framed it around the deleted method.

### Step 2 — Typed Rust V4 pool-admission exceptions (7SSOJX closure)

Make the V4 admission refusal a typed exception (the `7SSOJX` treatment for
pool admission) so Python classifies by type, not string. Refactor
`BotState::register_v4_pool` to return `Result<u64, RegisterV4PoolError>` where

```rust
pub(crate) enum RegisterV4PoolError {
    HookedPool { hook_flags: u16 },
    DynamicFee { fee: u32 },
    AlreadyRegistered { pool_manager: Address, pool_id: [u8; 32] },
}
```

and map at the PyO3 seam in `py_binding.rs::register_v4_pool`:

```rust
// before:  .map_err(pyo3::exceptions::PyValueError::new_err)
// after:   .map_err(map_register_v4_err)
```

with two new `create_exception!` types, both subclassing `PyValueError` for
backward-compat with broad `except ValueError`:

```rust
create_exception!(degenbot_rs, HookedPoolRejectedError, pyo3::exceptions::PyValueError,
    "V4 pool with an amount-modifying hook — solver's CL math assumption breaks");
create_exception!(degenbot_rs, DynamicFeePoolRejectedError, pyo3::exceptions::PyValueError,
    "V4 pool with a dynamic fee — solver needs a fixed fee");
```

`AlreadyRegistered` stays `PyValueError` (a wiring/programming error, distinct
from the two admission categories). Then **delete the Python pre-check** in
`EngineRegistry.register_v4_pool` (still example-local at this point) and
re-point `build_paths`'s `except ValueError` arm to:
`except HookedPoolRejectedError → v4_hook_rejected`; `except DynamicFeePoolRejectedError
→ v4_dynamic_fee_rejected`; remaining `except ValueError → v4_other`. Add `.pyi`
stubs + Red/Green tests (Rust unit test on the `RegisterV4PoolError`→exception
mapping; Python test that both types exist, subclass `ValueError`, are distinct).

### Step 3 — Lift `EngineRegistry` + the engine-facing `HopInfo` family into the library

Move into `src/degenbot/arbitrage/` (new `engine_registry.py` colocated with a
new `hop_info.py` for the engine-facing hop descriptors):

- `EngineRegistry` — the orchestrator (`start()` ritual + `register_v2/v3/v4_pool` key maps + `register_path`), **without** the now-deleted V4 pre-check
- `V2HopInfo` / `V3HopInfo` / `V4HopInfo` / `HopInfo` — the engine-facing hop descriptors (distinct from the solver's `degenbot.types.hop_types::HopType`); frozen dataclasses reading only pool attributes
- `PathInfo` (its `path_type` property)
- `build_hops_from_pools`

Re-export `EngineRegistry` (and the `HopInfo` family) from
`degenbot.arbitrage`. Re-point `tests/arbitrage/test_engine_registry_start.py`
from `import examples.eth_backrun_v2_v3_v4_rust as runner` to
`import degenbot.arbitrage as runner` (or the explicit module). The example
keeps its local copies **for this slice only** (duplicated, still working);
slice 4 removes them.

**Lift scope guard (B-mid, not B-full):** what stays example-side — the
main-loop `BackrunSession`/`Dispatcher` (nonce range, simulate-concurrency,
age-decay, path suppression, priority-fee percentiles), `build_paths`,
`consume_result_batches`, `get_snapshots`, simulation overrides, per-bot config
constants. These are *deployment* policy, not engine-operation machinery; lifting
them would marry the library to one execution strategy.

### Step 4 — Re-point example + validate + docs

Delete the now-duplicate `EngineRegistry` / `PathInfo` / `HopInfo` family /
`build_hops_from_pools` from `examples/eth_backrun_v2_v3_v4_rust.py` and
`examples/eth_backrun_helpers.py`; repoint the example imports to
`degenbot.arbitrage`. Run `just test-all` + `just lint`. Update
`CONTEXT-MAP.md` + `src/degenbot/arbitrage/CONTEXT.md` to record the new
`EngineRegistry` term and its "one canonical way to start" role. File the
general **path-rejection predicate** follow-up as a separate ergo task (see
Relationships) — it is independently useful but out of scope here, since the
V4 double-filter is already gone via Step 2 without needing a predicate.

### Design decisions

- **Decision: V4 rule lives in Rust (pool admission), not as a Python path predicate.** Per ADR-005's standalone-core constraint, a Rust consumer (no Python) must be protected from hooked/dynamic-fee pools — the refusal must not strand across the future crate boundary. The check is *pool*-admission (a correctness floor: CL math assumes no hook intervention), not *path*-composition policy. Surfaced as typed Python exceptions (not a panic) for useful feedback, mirroring `7SSOJX`.
- **Decision: path predicate is a separate task, not a slice here.** You raised it as the alternative to the double-filter; Step 2 resolves the duplication by typed Rust exceptions (no predicate needed for V4). A general path-composition predicate (`PathRejection`/`PathPredicate`/`PathRejectedError`) is genuinely useful but is a *new feature*; bundling it into a consolidation plan mixes concerns. Filed as a sibling ergo task referenced below.
- **Decision: B-mid, not B-min or B-full.** B-min leaves `EngineRegistry.start` a pass-through to a free function (shallow seam); B-full lifts deployment-specific dispatch/simulation policy into the library (not "one way to start," just "one deployment"). B-mid lifts the engine-facing orchestrator + its cohesive `HopInfo` family — the actual "way to start" — and nothing more.
- **Decision: typed exceptions subclass `ValueError`, not `RuntimeError`.** Pool rejection is a recoverable, per-candidate decision (`build_paths` skips one path); `ValueError` is the precedent already used for the same V4 refusals, and keeps broad `except ValueError` handlers working.
- **Decision: `_v2_keys` never risked the duplicate-register panic.** Preserved (V2 is pre-registered by `bot.build_pool`; caching is the registry's only job — ADR-006 slice 9).

## Files Involved

**Primary:**
- `rust/src/optimizers/uniswap_engine/py_binding.rs` — delete `start()`; typed V4 exceptions + `map_register_v4_err`; new `create_exception!` exports
- `rust/src/bot_core/mod.rs` — `BotState::register_v4_pool` returns `Result<u64, RegisterV4PoolError>`; define `RegisterV4PoolError`; keep `Bot::start()` placeholder untouched
- `rust/src/bot_core/block_pump.rs` — delete `spawn`; repurpose the `legacy_spawn…` test
- `rust/src/lib.rs` + `rust/src/optimizers/uniswap_engine/mod.rs` — register/re-export the new exception types
- `src/degenbot/arbitrage/engine_registry.py` (new) — lifted `EngineRegistry`
- `src/degenbot/arbitrage/hop_info.py` (new) — lifted `V2HopInfo`/`V3HopInfo`/`V4HopInfo`/`HopInfo`/`PathInfo`/`build_hops_from_pools`
- `src/degenbot/arbitrage/__init__.py` — re-exports
- `src/degenbot/degenbot_rs.pyi` — `HookedPoolRejectedError`/`DynamicFeePoolRejectedError` stubs
- `examples/eth_backrun_v2_v3_v4_rust.py` + `examples/eth_backrun_helpers.py` — delete duplicates, repoint imports, `build_paths` isinstance classification

**Secondary:**
- `rust/CONTEXT.md` — remove `BlockPump::spawn (legacy)` (5DM6JJ) entry; fix `UniswapEnginePump` "spawned by start" + `PyUniswapArbEngine` three-phase line + stale `v2/v3/v4_engine.start()` fictions (lines 281/290/291)
- `docs/architecture/rust-owned-bot.md` — startup diagram: `→ engine.start(rpc_url)` → real sequence
- `CONTEXT-MAP.md` + `src/degenbot/arbitrage/CONTEXT.md` — new `EngineRegistry` term
- `tests/arbitrage/test_engine_registry_start.py` — repoint import to library
- `tests/rust/test_verification_exceptions.py` (or a new `test_v4_admission_exceptions.py`) — typed-exception tests
- `rust/src/bot_core/lifecycle.rs` or `solver_dispatch.rs` — if `RegisterV4PoolError` propagates through any other call site (audit during Step 2)

**No change needed:**
- `rust/src/bot_core/solve_coordinator.rs::SolveCoordinator::start()` — distinct (drain-lock assertion), still called from `resume()`
- `rust/src/bot_core/mod.rs::Bot::start()` — the `unimplemented!()` ADR-006 slice-5 placeholder (delete that separately when slice-5 wiring lands)

## Implementation Order

### Slice 1: Delete the legacy one-shot

1. Delete `PyUniswapArbEngine::start()` + `BlockPump::spawn`; repurpose `legacy_spawn…` test
2. Run: `just test-rust` — expect green; `just lint-rust` — expect clean

### Slice 2: Typed V4 pool-admission exceptions

1. Add `RegisterV4PoolError` enum; refactor `BotState::register_v4_pool`; add `map_register_v4_err` + `create_exception!` types; register in module
2. Delete the Python V4 pre-check in `EngineRegistry.register_v4_pool`; repoint `build_paths` to `isinstance` classification
3. Add `.pyi` stubs + Red/Green tests (Rust mapping unit test + Python exception-surface test)
4. Run: `just test-rust` + `just lint-rust` + `uv run pytest tests/rust/test_v4_admission_exceptions.py tests/arbitrage/` — expect green

### Slice 3: Lift `EngineRegistry` + `HopInfo` family into the library

1. Create `src/degenbot/arbitrage/engine_registry.py` + `hop_info.py`; copy (not move yet) `EngineRegistry`/`HopInfo`/`PathInfo`/`build_hops_from_pools`; re-export from `degenbot.arbitrage`
2. Re-point `tests/arbitrage/test_engine_registry_start.py` to `degenbot.arbitrage`
3. Run: `just test-python` — expect green (example still uses its local copy)

### Slice 4: Re-point example + docs + validate

1. Delete the example-local `EngineRegistry`/`HopInfo`/`PathInfo`/`build_hops_from_pools`; repoint example imports to `degenbot.arbitrage`
2. Update `CONTEXT-MAP.md` + `src/degenbot/arbitrage/CONTEXT.md`; remove all "legacy spawn" references; file the path-predicate follow-up ergo task
3. Run: `just test-all` + `just lint` — expect green; `git grep -i "engine.start(rpc_url)\|BlockPump::spawn"` — expect no live references

## Testing

### Per-slice test runs

Each slice runs `just test-rust` and/or `just test-python` as noted; `just
test-all` + `just lint` in the final slice. No compatibility period — the
legacy entry is deleted, not shimmed.

### New unit tests

```python
# tests/rust/test_v4_admission_exceptions.py
def test_hooked_pool_rejected_error_is_typed_value_error() -> None: ...
def test_dynamic_fee_pool_rejected_error_is_typed_value_error() -> None: ...
def test_admission_errors_are_distinct_value_errors() -> None: ...
```

```rust
// rust/src/bot_core/mod.rs (or a tests module)
#[test] fn register_v4_pool_hooked_maps_to_hooked_pool_rejected() { ... }
#[test] fn register_v4_pool_dynamic_fee_maps_to_dynamic_fee_pool_rejected() { ... }
#[test] fn register_v4_pool_already_registered_stays_value_error() { ... }
```

### Integration tests

`tests/arbitrage/test_engine_registry_start.py` already covers `EngineRegistry.start`'s
two-phase ritual (`FakeEngine` records subscribe → stream → backfill → verify,
never resume). Re-pointed to the library import, it validates the lift
behavior-preserving. `tests/arbitrage/test_backrun_session.py` covers the
end-to-end `BackrunSession` flow against the lifted `EngineRegistry`.

## Benefits

- **Locality**: one canonical startup sequence, library-resident — the reader finds it in `degenbot.arbitrage`, not by copying an example class.
- **Depth**: the two "start the bot" entry points collapse to one; the competing Rust one-shot (no callers, dead surface) is gone, not reorganized.
- **Leverage**: typed V4-admission exceptions (`HookedPoolRejectedError`/`DynamicFeePoolRejectedError`) give every consumer type-safe classification — the same `7SSOJX` pattern, now applied to pool admission, reusable for future admission rules.
- **Seam**: `EngineRegistry` in the library is the injectable boundary (`engine=` testability seam preserved) — the ADR-005 three-layer target realized for the orchestrator.
- **Standalone-safe**: the V4 admission floor stays in Rust (ADR-005 constraint), not stranded across the future crate boundary.

## Risks

- **Renaming the `legacy_spawn…` test loses the 5DM6JJ anchoring narrative** — mitigated by repurposing to `resume_anchors_to_subscribe_block` with an updated docstring; the `run_with_stream` invariant it pins is unchanged.
- **`RegisterV4PoolError` enum refactor may touch call sites beyond `py_binding`** — mitigated by auditing `grep register_v4_pool` across `rust/src/` before Step 2; expected to be localized (the `Err(String)` only flows out of `BotState` through the one PyO3 seam).
- **Lift changes the import path for `EngineRegistry`; out-of-tree consumers break** — accepted (0.x, AGENTS.md no-backwards-compat); the example + tests are the in-tree consumers and both are repointed in-slice.
- **`EngineRegistry.start` reaching into snapshot internals (`min(s.newest_block)`) couples the registry to snapshot objects** — pre-existing; the lift preserves the coupling rather than redesigning it (the snapshot-block derivation is general, not deployment-specific).

## Relationship to Other Plans

- **7SSOJX** (typed verification exceptions): **closed by Step 2.** The V4 admission refusal is the last `ValueError`-string-matching site for the same family of "refusal" errors; this plan extends the `7SSOJX` typed-exception pattern to pool admission.
- **ADR-005 / slice 10 (`6BPRAH`, UniswapEngine lock unification):** **complementary, orthogonal.** This plan fixes layer 3 (Python orchestration) + the V4 admission call into layer 2; slice 10 later unifies the engine's `Bot` handle. No overlap.
- **ADR-005 / slice 14 (`QPH55N`, `PyBotIo`):** **preceded by this plan.** Once sequencing is Python-side, the per-phase Rust I/O drivers can be ported to `PyBotIo` under the same Python orchestration without touching control flow. Doing this plan first is the right order.
- **Plan 098/100 (Rust-owned bot, reorg journal):** **orthogonal.**
- **5DM6JJ** (anchor legacy `BlockPump::spawn`): **superseded by Slice 1.** The 5DM6JJ fix anchored `spawn`; this plan deletes `spawn` outright. The `run_with_stream` anchoring invariant 5DM6JJ pinned survives in the repurposed test.
- **New follow-up (filed in Slice 4):** general path-rejection predicate (`PathRejection`/`PathPredicate`/`PathRejectedError`) — independently useful for token denylists, hop-count policy, min-liquidity gates; out of scope here.

## Status

[x] Slice 1: delete the legacy one-shot
[x] Slice 2: typed V4 pool-admission exceptions
[x] Slice 3: lift `EngineRegistry` + `HopInfo` family into the library
[x] Slice 4: re-point example + docs + validate; file path-predicate follow-up
