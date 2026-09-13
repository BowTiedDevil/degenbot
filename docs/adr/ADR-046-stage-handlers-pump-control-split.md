# ADR-046: StageHandlers stays the pure product seam; PumpControl is the driver-facing control seam

**Status: accepted** (2026-09-12; architecture-review candidate #2, grilling rounds 1–3, epic `KLLYHS`).
Landing tracked by ergo epic `KLLYHS` (red `2ecf97c64`, split `cc2849763`, twin cut `a5c47d343`, edge facts `a9149077f`).

## Context

ADR-041 retired the `DrainSink`/`Engine` dual seam onto the one `StageHandlers`
trait, and the arb engine's implementation (`EngineStages`) inherited the
pump↔engine coordination that used to be split across those two seams. The
result mixed two vocabularies on one interface: the eight pure stage hooks
(fact-carrying, product-shaped — what a block *produced*) and seven driver
pokes (bookkeeping and liveness — what the driver *told* the engine or asked
it). ADR-045 named this cross-interface drift "architecture-review
candidate #2" and left it as a mechanical follow-up once `SolveCycle` existed.

The mixed seam had already produced real twin drift, not just a cosmetic
imbalance:

- **`on_pump_ended` had an inherent `EngineStages` twin** that silently
  skipped the `op_error` loud-close log the 2026-08-20 delivery-liveness
  contract requires (incident #2). The trait method had the log; the twin the
  pump actually called did not. Two implementations of one concept, only one
  correct.
- **Cursor pokes were typed `u64`** (`set_last_solved_block(u64)`,
  `set_solve_anchor(u64)`) while the trait's block coordinate is `Epoch` — a
  parallel coordinate that could (and did) drift from the one block
  coordinate.
- **`solve_dirty` had a second entrance**: callers reach the solve cycle
  through `SolveCycle::run_epoch` / `register_and_solve_path` *deliberately
  bypassing pump semantics* (`solve_cycle.rs:764`), so a "one trait for
  everything" surface could not describe what those callers actually spoke.

The vocabulary test that resolves the mess is **which layer's conversation a
symbol belongs to**.

## Decision

### D1 — The two-layer vocabulary rule

Two distinct conversations travel the pump↔engine boundary, and each owns its
own surface:

- **Product / facts** — what a block epoch produced (a quiesce verdict,
  affected paths, candidates, sim facts, gate verdicts, a publish, a
  finalize). This crosses the **`StageHandlers`** seam.
- **Provenance / execution** — where a solve request came from and how it is
  driven (the dirty-set bookkeeping, the cursor, the resume anchor, the
  delivery clock, pump liveness). The typed **`CycleOutcome` / `SolveCycle`
  (ADR-045)** surface owns the provenance the cycle owner speaks for itself.

**Every seam deletion is judged by which layer's vocabulary its callers
speak.** A symbol whose callers speak product/facts belongs on
`StageHandlers`; a symbol whose callers speak provenance/execution stays on
the `SolveCycle` surface (or, for driver bookkeeping, on `PumpControl`,
below) — never "up" into `on_solve`.

### D2 — StageHandlers is 8 pure hooks; PumpControl is 7 pokes

**`StageHandlers` stays the pure product/facts seam: exactly the eight
required `on_*` stage hooks, fact-carrying outcomes, no control pokes.**
`on_simulate`, `on_gate`, and `on_rewind` stay **required** (no default
bodies) so ADR-041's compile-fails-if-incomplete property — exercised by the
`NoopStubEngine` conformance harness — survives intact. Outcomes carry the
facts the driver used to re-poke for:

- `SolveOutcome.solved: Epoch` — the anchor is a required field (a cycle
  always knows its epoch; never `Option`, never a separate poke).
- `FinalizeOutcome.cutoff: Epoch` — the delivery cutoff stays on the outcome.
- `drive_solve` stops poking `set_last_solved_block` on success
  (`block_pump.rs:1987`); the driver reads the fact from the outcome.
- `Finalize` drops the fabricated `PublishOutcome::default()` pass-through
  (1633/2122) — no outcome field exists only to carry a value nobody reads.

**`PumpControl` is a separate required trait** (`bot_core/pump_control.rs`),
injected *beside* `Arc<dyn StageHandlers>` at pump construction, owning the
seven driver pokes: `has_dirty_paths`, `set_last_solved_block(Epoch)`,
`set_solve_anchor(Epoch)`, `record_logs_this_block`, `last_processed_block()
-> Option<Epoch>`, `notify_block(block: u64, metadata)`, `on_pump_ended`.
Both `EngineStages` and the two test doubles (`NoopStubEngine`,
`FakeStageEngine`) implement both traits, so adding a poke is the same
compile error as adding a hook.

The coordinate rule: `PumpControl`'s engine cursors are `Epoch`-typed (the
one block coordinate); `notify_block` stays raw `u64` because a `newHeads`
tick is a **chain fact forwarded to the delivery-to-Python clock**, not engine
epoch work. Raw `u64` there is deliberate, not drift.

Every `EngineStages` inherent twin is **deleted hard** (no shims): `solve_dirty`,
`last_processed_block`, `send_result_batch`, `finalize_block`,
`set_last_solved_block(u64)`, `set_solve_anchor(u64)`, `record_logs_this_block`,
`on_pump_ended`. The `solve_dirty` callers that bypass pump semantics move
down to the `SolveCycle` surface, never up to `on_solve`.

## Considered options rejected

- **Keep the mixed trait and just fix the twins** — the drift recurs because
  the surface has no vocabulary boundary; the next poke re-forks. Rejected:
  the two-layer rule is the fix, twin repair is its consequence.
- **Default `on_simulate`/`on_gate`/`on_rewind` to no-ops** to shrink the
  trait — trades the compile-fails-if-incomplete property for a smaller
  surface. Rejected: ADR-041's executable spec is load-bearing.
- **Fold `PumpControl` onto `SolveCycle`** — the pokes are pump-driver
  bookkeeping (liveness, delivery clock, resume anchor), not solve-cycle
  provenance; the cycle never owns the delivery clock. Rejected on the D1
  vocabulary test.
- **A single `dyn` control object carrying both traits** — one Arc, one
  object, but re-mixes the two layers the split exists to separate. Rejected.

## Consequences

- The pump constructor takes **two Arcs** (`stages` + `control`); the
  `degenbot-python` driver gains only parameter plumbing (the `control` Arc
  clone in `bot/pump.rs`) and `PumpControl` routing in `bot/engine/solve.rs`.
  An FFI sweep confirmed **no Python-visible `solve_dirty` exposure existed**;
  the split changed no Python method signature.
- All the `for_test` knobs and FSM mutation points preserved by `VHCRD2` keep
  working; `EngineStages` still implements `StageHandlers` for the test
  surface.
- Hard cutover, no feature flag (AGENTS.md). The `degenbot-bot` lib suite is
  the characterization net; the red seam-pin tests (T1, `2ecf97c64`) name the
  target first.
- ADR-043 telemetry labels are byte-identical — no label moved with the split.
