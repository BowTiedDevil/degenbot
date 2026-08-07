# T6 — Bookkeeping: ADR + docs wiring (done)

- `CONTEXT.md` "Pool structural families" section now carries the **Orchestration
  module layout** paragraph: the three family splits (`cl_orchestration.rs` /
  `reserve_pair_orchestration.rs` / `balance_vector_orchestration.rs`), what stays
  resident in `bot_core/mod.rs` (registry/reorg dispatch, solver-facing CLI,
  `BotCurveBasePoolPort`), the resident-test-module decision, and pointers to the
  plan + RED-neutral evidence docs.
- `docs/plans/botstate-god-file-split.md` — durable design record (written at epic
  creation; Q1–Q11 decisions, CL cut list, risk mitigations, non-goals).
- Task-result slices documented at `docs/plans/botstate-t4-gate.md` (pilot gate),
  `botstate-t5-generalize.md` (both non-CL splits), `botstate-t3-decision.md`
  (test-module deferral).

**No ADR amendment required.** This is an inherent-impl rearrangement of the
`BotState` method set only: the `BotState` struct, its single-registry ownership
(ADR-003), and the three-layer FFI seam (ADR-005) are untouched — every moved
method remains an inherent `BotState` method reachable from any module, so no
call site, `pub use`, `#[pyfunction]`, or `standalone_consumer` slice changed.
ADR text changes are not warranted for an internal module layout.
