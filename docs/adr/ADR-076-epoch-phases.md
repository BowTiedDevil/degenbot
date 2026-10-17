# ADR-076: The epoch phases — `SolveCycle::run_epoch` decomposes into named phases behind one interface

**Status: accepted (2026-10-16).** Drafted at the architecture-review grilling (candidates C2; rounds 1–3, Q4/Q15/Q20). Builds on ADR-045 (the solve cycle as a deep module) and ADR-049 (machine-direct free functions).

## Context

ADR-045 regrouped `ArbitrageEngine` into `registry + cycle + config` and made `SolveCycle` the deep module of the per-block solve. The cycle has since regrown inward: `run_epoch` (`arb_engine/solve_cycle.rs:1333–2247`, ~914 lines; now `solve_cycle/mod.rs::run_epoch` over the `shed`/`resolve`/`solve`/`merge`/`deliver` submodules) inlines shed, the dirty-status scan, `SolveAnchor::resolve`, the admission draw, LPT partition, the detached arm, merge, clamp, delivery, and telemetry. A regression inside the epoch names no smaller home than the whole body.

The interface also drifted past production shape. Four test-only setters sit on the public surface — `set_force_deferred_for_test` (:2797), `set_merge_probe` (:2800), `set_sync_merge_for_test` (:2809), `set_resolve_parallel_for_test` (:2835) — and `pending_new_paths_mut` (:2756) hands callers a `&mut` to the internal stash (consumed by `test_harness.rs:160`, `tests/admission.rs:328`, and the cycle's own engine-side call at :3048). The epoch body reads ~27 fields in four clean clusters — status (`path_status`, `path_resolved`, `cursor`), admission (`solve_admission`, `admission_target_depth`, `detached_cycle`, `transients`, `deferred_re_record`, `force_deferred`), caches (`resolved_update_snapshot`, `hop_projection_cache`, `concentrated_liquidity_projection_memo`), and results/config (`results`, `cfg`, `runtime_cfg`, `path_cost_history`, `min_profit_floor`, `clamp_margin`, `inline_sim*`) — plus three test knobs (`test_solve_delay`, `test_solve_panic`, `test_sync_merge`) smeared inline through the production path.

## Decision

**D1 — Phases are machine-direct free functions over an explicit `EpochContext`.** `run_epoch` decomposes into `solve_cycle/` submodules — `shed`, `resolve`, `solve`, `merge`, `deliver` — each a free function taking `&mut EpochContext`, in the post-ADR-049 machine-direct style. `EpochContext` carries the four field clusters above; the phases own their cluster's mutation order. `run_epoch` keeps the one interface: `run_epoch(inputs) -> EpochOutcome`.

**D2 — Test knobs become construction-time fields, not interface methods.** The four `*_for_test` setters and the three inline test flags move to construction-time phase adapters on `SolveCycle` — internal seams the module's own tests use, never exposed through the public interface (seam discipline: internal seams stay internal).

**D3 — The stash stops crossing the seam.** `pending_new_paths_mut` is deleted; the pending-new-paths stash lives inside `EpochContext` and is consumed by the phases. The engine-side caller at :3048 moves inside the cycle.

**D4 — Replace, don't layer.** Tests built on the setters are rewritten against the construction-time adapters; epoch tests assert on `EpochOutcome` through `run_epoch`, not on internal state. The setter-driven tests are deleted, not kept as a compatibility layer.

## Consequences

An epoch regression names its phase. The public interface of `SolveCycle` shrinks to production shape, and the structural drift this file attracted (it is the repo's historical #1 churn site, per ADR-045) gets smaller blast radius per change. The phase extraction is byte-identical relocation per phase — no arithmetic changes in the same change (the ADR-071/073 playbook).
