# Post-cut-over adapter audit: what the session cut-over left in the shells

**Status: audited; one duplicated derivation found and consolidated into the
core (with its inverse pinned by a core test). One dead leftover recorded for
ledger work rather than fixed here. No residual batch-work, raw-row, or
payload-row derivation remains in the Python driver or in the example.**

ADR-093 moved the session consume loop into the core
(`degenbot_batch_executor::session::SessionConsumer`) and left both first-class
consumers holding only the display drain and the report rendering (§6.6 row 6).
This note is the audit of what the cut-over left behind in those adapters: does
any shell still derive the things the core derives — a raw row from a solver
result, a payload row from an inline-sim result, a batch's work from a result
batch and the session clock?

## The core's derivation surface and its one home

`rust/crates/engine/degenbot-batch-executor/src/session.rs`:

| Derivation | Core item |
|---|---|
| engine solve row → executor raw row (`U256` saturated to `u128`) | `raw_row` (`:691`) |
| engine inline-sim payload → executor payload row | `payload_row` (`:709`) |
| result batch + session clock → one `BatchWork` (rows, path-id-ordered payloads, the clock's per-block facts) | `batch_work` (`:847`) |
| executor payload row → engine inline-sim payload (the scripted door's direction) | `simulated_path` (`:778`) |

The fourth row is this audit's addition: its inverse used to live in the Python
shell, hand-written (see "Consolidated" below).

## Swept: the Python driver (`src/degenbot/`)

| File | Its share after the cut-over |
|---|---|
| `runner/_consume.py` | `consume_result_batches` (`:70`) runs the consumer and renders the report; `_drain_batch_outcomes` (`:113`) folds each drained `BatchOutcomeSet` into the render view and the silent-veto smoke FSM. Both read typed records; neither derives work. |
| `runner/_sim_submit.py` | The construction boundary only: `ExecutorInputs` (`:49`) resolves the runtime handles, `build_batch_executor` hands them to the FFI (which owns every policy default and clamp), `build_session_consumer` (`:173`) attaches the engine's hand-off. It carries the target-selection policy NAME (`target_selection`) and no policy value: the walk, its order, and its refusal taxonomy are the core's. |
| `runner/session.py` | The run ritual's wiring: `build_consumer` calls the session's consumer factory, `block_loop` assembles the drain's collaborators. The block clock is `current_block` reading the core's dispatcher — no Python mirror. |
| `dispatch/__init__.py`, `dispatch/records.py` | The FFI alias facade plus `typed_submit_record` (`records.py:57`), the typed decode of the wire submit-record dicts. |
| `runner/_render.py` | The render fold (`MergedOutcome.from_records`) — display, `stays-python` by design. |

**Result: no raw-row, payload-row, or batch-work equivalent exists in the Python
driver**, and nothing advances a clock or classifies a verdict on the Python
side. The batch event dicts that do exist belong to the `scripted()` test door,
not to production: the live door attaches the engine's hand-off in Rust, so
production crosses no dict at all.

One boundary observation, recorded and deliberately unchanged: the scripted
event parser's solver-row arm (`simulation/session.rs:645` in the FFI, not
Python) reads the row's `U256` fields with `extract::<u128>()`, so an
over-`u128` value raises at the seam where the core's `raw_row` saturates. That
is a boundary PARSE with a loud policy, not a duplicated derivation, and giving
it the core's saturation policy would be a behavior change outside this audit.

## Swept: the example (`rust/examples/settlement_bot/src/`)

| File | Its share after the cut-over |
|---|---|
| `main.rs` | Constructs the core consumer over the claimed hand-off (`:861` new, `:872` attach) with `SessionClock` + `ConsumerPolicy`, and prints the typed report's counts. |
| `consume.rs` | `drain_records` (`:24`) and `run_session_consumer` (`:40`) — the drain and the report's `(batch N)` detail. |
| `dispatch.rs` | The resolver adapters (`MapResolver`, `StagesResolver`), the receipt probe, and the row-17 fee reach proofs. States the batch-local payload-served set and the cross-block suppression registry as distinct policy inputs the core derives. |
| `pipeline.rs`, `run_loop.rs`, `session_watch.rs`, `progress.rs`, `telemetry.rs`, `live.rs`, `discovery.rs`, `policy.rs`, `operator_channel.rs` | Registration admission adapter, teardown ordering, end-verdict ranking, progress/heartbeat reads, enumeration — none of them builds batch work or a row. |

**Result: the example derives none of the three.** One leftover was found and is
recorded rather than fixed here:

**Found, left in place: `submission.rs` is a pre-cut-over leftover with zero
production call sites.** `submit_batch` (`:78`) re-implements the submit lane's
guard order (best-first sort, pools-claimed, dry-run, inject-code skips) and
`SubmissionSeam` / `LiveSubmissionSeam` (`:51`, `:137`) its signing boundary. The
only callers are the module's own `#[cfg(test)]` tests; `mod submission;` is
declared at `main.rs:74`, and nothing outside the module names any of its items.
Its module doc still cites `src/degenbot/runner/_dispatch.py::_submit_batch_records`,
a function the Python driver no longer has. **Why it stays**: it is the example's
row-18 receipt surface ("`submission.rs` dry-run seam never signs"), so retiring
it means re-anchoring parity-ledger row 18 — ledger work, not adapter work. It
also derives no row and no batch work, so it is outside this audit's
consolidation target.

## Consolidated: the Python shell's second copy of the payload field list

`rust/crates/shells/degenbot-python/src/simulation/session.rs` parsed a scripted
batch's payload dict twice over: `payload_row_from_dict` (the FFI's ONE dict
parser, `simulation/dispatch.rs:194`) into a core `PayloadRow`, then a
hand-written inversion back into the engine's `SimulatedPathResult` —
`simulated_path_result_from_dict` (`:726`) — that repeated the field list of the
core's forward `payload_row`, together with its own copies of the two private
inverses (`AccessList` → `AccessListRow`, `CapturedSwap` → `CapturedSwapRow`) and
the failure row's projection. A field added on either side would have crossed
one way only, silently.

The inversion is now the core's `simulated_path` (beside `payload_row`, umbrella
`degenbot::batch_executor::simulated_path`), pinned by
`session::tests::the_payload_row_and_the_simulated_path_are_inverse` — a
round-trip over a fixture that populates every field (calldata, access list,
captured swaps, failure). The shell keeps the dict parse and the calling
convention; the field list has one home. `hop_count` is an argument because the
row never carries it (the executor derives a candidate's hop count from resolved
path info).

**Rejected alternative: parse the dict straight into `SimulatedPathResult`.**
That would trade one duplicated field list for two competing dict parsers for the
same wire shape (`payload_row_from_dict` is also the merge seam's parser), which
is the duplication the seam exists to avoid.

## What stays in the shells, and why

| Shell share | Why it is not the core's |
|---|---|
| the display drain + render fold (`_consume.py`, `consume.rs`, `_render.py`) | Rendering reads typed records and emits operator lines; the core publishes records and never formats. |
| the report's end-verdict rendering (`pump-stopped` → the loud abort, the `(batch N)` detail) | The typed end verdict is the core's (`ConsumerEnd`); turning "this session must not survive" into a `RuntimeError` is the supervised consumer's policy. |
| `typed_submit_record` | Decoding the seam's wire dicts into typed records is the boundary translation. |
| the exception mapping (`abort_error`) | Classification is the core's `ConsumerErrorKind`; only the Python exception SHAPE is the shell's. Its message wording is another item's scope. |
| the row-17 fee reach proofs (`fetch_priority_fees`, `record_fee_history`) | Documented standalone-RPC reach proofs with no call sites by design; parity row 17 depends on them. |

## Gates

`just dev` (the extension carries the seam change), then
`just py pytest tests/standalone_parity/test_session_consumption_gate.py
tests/dispatch/test_batch_executor_seam.py` (the scripted door and the frozen
parity product stay green), then
`just test-rust-nextest -p degenbot-batch-executor -E 'binary_id(*session_consumption*)'`
(exactly five tests; a renamed id fails closed with "no tests to run", exit 4)
and the crate's whole nextest run for the new unit test. The whole-suite
`just test-python` leg belongs to the workspace ladder, not to this audit.
