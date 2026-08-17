# RevM Inspector-native simulation diagnostics — implementation spec

Ergo epic **63I7WJ** (task **JHPW5W**). Converts the spike
([`docs/spikes/revm-inspector-diagnostics.md`](../spikes/revm-inspector-diagnostics.md),
task KCKGP4) + prototype (committed `c59151ea`, task 2LMT7A) into a
decision-resolved implementation plan. This is the gate for the implementation
tasks; it is approved before any production wiring lands.

## Architectural framing (carried from the epic + spike + prototype)

- **ADR-019 D4/D7** — engine owns generic primitives; strategy owns the
  Drift/SolverCalc/Encoding *policy*. The inspectors live in
  `degenbot-simulation` (the engine crate); the four-way classifier policy
  stays in `examples/eth_backrun_helpers.py` +
  `logs/permutation_analyzer.py`, re-pointed at the engine-supplied
  captured data.
- **ADR-005 Tier 0** — a `cargo add degenbot` consumer reaches `CallTrace` +
  captured swaps + reverting-frame attribution from the engine directly.
- **ADR-013** — the FFI seam is private; the PyO3 wrapper is a thin
  `#[pyclass]` re-export of the captured structs, no business logic.
- **AGENTS.md** — retirements are irreversible (no back-compat layer for the
  retired onchain-recompute path).

## 1. The `SimInspector` type alias (nested tuple)

The production `BlockEvm` inspector type widens from bare `AccessListCollector`
to the nested tuple:

```rust
pub type SimInspector = (
    AccessListCollector,
    (CallTraceInspector, SwapEventCaptureInspector),
);
```

**Hard constraint (prototype 2LMT7A finding):** revm's blanket `Inspector`
impl covers 2-tuples `(L, R)` only
(`revm-inspector-42/src/inspector.rs:150`). A flat 3-tuple does NOT satisfy
`Inspector`. The composition MUST be nested: `AccessListCollector` is `L`, the
`CallTraceInspector`/`SwapEventCaptureInspector` pair is `R`. The prototype's
`inspectors/mod.rs::SimInspector` alias already encodes this shape.

**Wiring:** `rust/crates/degenbot-simulation/src/sim/evm/simulator.rs::BlockEvm`
type parameter flips from `AccessListCollector` to `SimInspector`. The
strategy's `simulate_path_on_evm` constructs the composed tuple via
`AccessListCollector::new()` + `CallTraceInspector::new()` +
`SwapEventCaptureInspector::new()` and passes it to `inspect_one(tx, tuple)`;
after the run, drains all three handles.

## 2. The captured-struct field set (finalized from the prototype)

The prototype's fields are the production surface (proven by the
composition-parity test `tests/inspector_composition.rs`):

```rust
// sim/evm/inspectors/call_trace.rs
pub struct CallFrame {
    pub depth: usize,
    pub caller: Address,
    pub target: Address,
    pub selector: [u8; 4],
    pub gas_limit: u64,
    pub outcome: Option<FrameOutcome>,
}
pub enum FrameOutcome {
    Success { gas_used: u64, output: Bytes },
    Revert { gas_used: u64, data: Bytes },   // <- classify_revert input
    Halt { gas_used: u64 },
}
pub struct CallTrace { pub frames: Vec<CallFrame> }

// sim/evm/inspectors/swap_event.rs
pub struct CapturedSwap {
    pub emitter: Address,
    pub family: SwapFamily,                // V2 | V3 | V4
    pub amount0: I256,
    pub amount1: I256,
    pub sqrt_price_x96: U256,
    pub liquidity: U256,
    pub tick: i32,
}
```

**Open decision — V2 reserves vs amounts.** The V2 `Sync` event carries
*reserves* (absolute `uint112`), not *amounts* (the V3/V4 `Swap` event carries
signed `amount0`/`amount1`). The prototype stores zeros for V2 amounts. Since
the swap-event capture replaces the onchain recompute, V2 amounts are
derivable from consecutive `Sync` reserve deltas — **but** a single-hop V2
swap emits exactly one `Sync`, so the *delta* requires the pre-swap reserve
(the engine-state view). Resolution: extend `CapturedSwap` with a
`V2Reserves { reserve0, reserve1 }` variant (or an `Option` pair) so the
strategy classifier can compare `hop_outputs[i]` against
`|reserve_post - reserve_pre|` using the engine's tracked pre-swap reserve.
**This is the one field-set change from the prototype** — flagged for the
implementation task, not a prototype re-spin.

## 3. The `SimFailure` deepening (revert attribution)

`rust/crates/degenbot-settlement-strategy/src/simulator.rs::SimFailure` today:

```rust
pub struct SimFailure {
    pub path_id: u64,
    pub bucket: String,                  // classify_revert label, top-level only
    pub fail_index: Option<usize>,      // 0–6 top-level call index
    pub revert_data: alloy::primitives::Bytes,
}
```

Replace `fail_index` + the top-level-only `revert_data` with the reverting
*frame*'s attribution, surfaced across the FFI:

```rust
pub struct SimFailure {
    pub path_id: u64,
    pub bucket: String,
    pub reverting_frame: Option<RevertingFrame>,
}

pub struct RevertingFrame {
    pub depth: usize,                   // call depth (1 = top-level execute())
    pub target: Address,                // the reverting contract
    pub selector: [u8; 4],              // the call's selector at the reverting frame
    pub revert_data: Bytes,             // the reverting frame's data (classify_revert input)
    pub label: String,                  // classify_revert(revert_data)
}
```

**`classify_revert` stays** (`degenbot_decoders::revert`); it is now fed by
`CallTrace::reverting_frame_label()` at the reverting frame, not the top-level
bubble. The prototype's `CallTrace::reverting_frame_label()` implements this
walk (deepest `Revert` frame + `classify_revert`).

## 4. The `diagnostic.rs` retirement boundary

`rust/crates/degenbot-bot/src/solvers/arb_engine/diagnostic.rs` (the
"mixed Uniswap arbitrage engine" diagnostic path) splits cleanly:

**DELETE (the onchain-recompute half — replaced by swap-event capture):**

- `fetch_onchain` (the Multicall3 RPC fetch, L757) — the swap events are
  captured in-process; no re-fetch.
- `recompute_v2_amount_out`, `recompute_v3_amount_out`,
  `recompute_v4_amount_out` (L154/L190/L221) — the off-chain swap-math
  recompute against a separately-fetched snapshot.
- `recompute_cl_amount_out_onchain` (the onchain-state scalar-slot0 recompute).
- `populate_*_recompute`, `refresh_*_recompute_onchain` (L342/L372/L405/L…).
- `HopRecompute` (L513) + the `DiagnosticHop::recompute` field (L631) + every
  `recompute: Some/None` site (L677…L2212).
- `HopFetch` (L845), `require_success` (L855), `build_v2/v3/v4_calls`,
  `decode_v2/v3/v4_results` (L959/L973/…), `FetchOutcome` — the RPC transport.
- The hand-rolled ABI helpers (`fn_selector`, `encode_call`, `uint_value`,
  `parse_hex_*`, `u256_to_hex`) — replaced by `degenbot-abi`/`degenbot-decoders`.

**RETAIN (the engine-state-read half — answers a different question):**

- `DiagnosticPoolState`, `DiagnosticHop` (minus `recompute`),
  `DiagnosticPathState`, `FieldDiff` — pure data + serde.
- `compute_field_diffs` (L70) — pure math, no RPC.
- `UniswapEngine::diagnostic_path_state` (L1138) — returns
  `Option<DiagnosticPathState>` (API shape preserved).

**Collapse:** `format_sim_diag_line`'s `DriftArtifact` timing guard collapses
(the run's own swap events have no block-tag ambiguity — there is no
"post-publish live read" when the source-of-truth is the run's own emitted
events, not a separate `fetch_onchain`). The Python-side
`hops`/`engine_state`/`onchain_state` block in
`examples/eth_backrun_helpers.py::format_sim_diag_line` is re-pointed at the
captured swaps + the engine-state-read half.

**Cross-dependency note:** task `WQENYW` ("Split diagnostic.rs into
per-concern submodules; gate RPC behind feature") is a separate in-flight
refactor of the SAME file. The retirement here + the split there MUST be
sequenced (retire-then-split, or merge); the implementation task coordinates.

## 5. The Tier-2 parity-pair fixture (ADR-005 dual-path coverage)

When `CallTrace`/`CapturedSwap`/`RevertingFrame` cross the FFI, add the
parity pair:

- **Shared fixture:** `tests/standalone_parity/fixtures/inspector_swap.json` —
  a path's inputs + the expected captured swaps (per-hop `amount0`/`amount1`,
  `emitter`, `family`) + the expected reverting-frame attribution (depth,
  target, selector, label). Both sides read this file for inputs AND expected
  outputs (per the V3/V4 fixture-drift resolution — no copied constants).
- **Rust half:** `rust/crates/degenbot/tests/parity_inspector.rs` — drives the
  composed `SimInspector` via `BotState`, asserts the captured swaps + the
  reverting frame match the fixture.
- **Python half:** `tests/standalone_parity/test_inspector_dual_driver.py` —
  drives the same fixture via `PyBot`, asserts the same.

**V4 caveat:** V4-amount correctness is blocked on `5RI47E` (the transient
seeder). The parity fixture's V2 + V3 slices land now; the V4 slice lands
when `5RI47E` flips the seeder (deliberate deferral, not a gap).

## 6. The implementation sequencing (follow-on tasks)

Ordered, dependency-resolved. Each is one atomic, reviewable change. File
these as the JHPW5W task's children OR as a separate epic on approval.

1. **Compose `SimInspector` into `BlockEvm`** — flip the `BlockEvm` type
   parameter from bare `AccessListCollector` to `SimInspector`; wire the
   strategy's `simulate_path_on_evm` to construct + drain the tuple. AL
   parity test stays green (the prototype already pins it).
2. **Deepen `SimFailure`** — replace `fail_index`/`revert_data` with
   `RevertingFrame` (depth/target/selector/revert_data/label), fed by
   `CallTrace::reverting_frame_label()`. Surface across the FFI.
3. **Widen the decoders to accept `primitives::Log`** (recommended) OR wrap
   at the inspector boundary — removes the `primitives::Log` →
   `rpc::types::Log` conversion the prototype carries. Decide in-task;
   widening is cleaner (the decoders only read `.topics()`/`.data`).
4. **PyO3 surface** — `#[pyclass]` thin shells for `CallTrace`/`CallFrame`/
   `FrameOutcome`/`CapturedSwap`/`RevertingFrame`. No business logic across
   the FFI (ADR-013).
5. **Re-point the classifier** in `logs/permutation_analyzer.py` +
   `examples/eth_backrun_helpers.py` at the decoded swap-event amounts; drop
   the `DEGENBOT_SIM_TRACE` eprintln block in `simulate_path_on_evm`.
6. **Retire the `diagnostic.rs` onchain-recompute half** — delete the symbols
   in §4 (DELETE list). Coordinate with `WQENYW`'s split. Green once steps 1–5
   land.
7. **Rewire the example bot** — `examples/eth_backrun_v2_v3_v4_rust.py`
   renders the new `RevertingFrame` attribution + the captured swaps;
   `logs/permutation_analyzer.py`'s TSV columns stay stable (the labels are
   the operator contract).
8. **Tier-2 parity pair** — the shared fixture + `parity_inspector.rs` +
   `test_inspector_dual_driver.py` (V2+V3 now, V4 when `5RI47E` lands).

## Implementation status (autonomous session — exercised against the
backrun bot's test harness)

Delivered + exercised (committed):

1. **Compose `SimInspector` into `BlockEvm`** ✓ (`188b2d5c`) — the nested-tuple
   `(AccessListCollector, (CallTraceInspector, SwapEventCaptureInspector))`
   is the `BlockEvm` + `simulate_in_process_with_db` + `simulate_path_on_evm`
   inspector type. The two `simulate_in_process_with_db` smoke tests confirm
   the real 7-call orchestration runs with the composed tuple end-to-end.
2. **Deepen `SimFailure` with `RevertingFrame`** ✓ (`8b815694`) —
   `CallTrace::failing_frame()` (deepest non-`Success` frame, covers `Revert`
   AND `Halt`) feeds a `RevertingFrame { depth, target, selector,
   revert_data, label }` on `SimFailure` via the new `FailBuckets::record_revert`.
   Drained right after `finalize` so both branches have it.
3. **PyO3 surface** ✓ (`57556e65` + `55737e91`) — `captured_swaps` on
   `SimResult` (success) + `SimFailure` (revert); the `failures()` getter
   surfaces `reverting_frame` + `captured_swaps` dicts; the
   `profitable_captured_swaps` getter surfaces the success-path swaps (one
   entry per profitable survivor, same dict shape as `failures()`
   `captured_swaps` — DRY via the `captured_swap_to_dict` helper).
7. **Rewire the example bot** ✓ (`ca0b124f`) — `_render_sim_failures` now
   surfaces `revert@depth=N target=… sel=… label=… swaps_before=M revert=…`
   when `reverting_frame` is set; falls back to `fail_idx=…` for non-revert
   buckets. New `test_reverting_frame_surfaces_deep_attribution`.

Exerciser proof (the "helps find/fix real bugs" claim):

- A new smoke injects a `REVERT(0xcafebabe)` executor + asserts the
  reverting_frame surfaces the executor target + the 4-byte selector data +
  the `unknown:0xcafebabe` label at depth 1 — through the REAL 7-call
  orchestration, not a unit mock.
- The existing `0xfe` Halt smoke is enriched to assert the halting frame is
  attributed (label `empty`).
- Bugs found + fixed en route: the prototype's LIFO `call_end` pairing
  (parent outcome stayed `None`), the nested-tuple constraint (revm's
  blanket `Inspector` impl is 2-tuple-only — a flat 3-tuple does NOT satisfy
  `Inspector`), the `u64::to_be_bytes()` PUSH2 bytecode misalignment, and the
  `log`-vs-`log_full` surprise (the spec's Q1 finding).

Delivered (ergo epic 63I7WJ, task AM5AJW — commits `a11b48ea` +
`d571553a`):

5. **Re-point classifier + drop `DEGENBOT_SIM_TRACE`** ✓ (`a11b48ea` +
   `d571553a`) — `format_sim_diag_line` now builds the `[sim-diag]` JSON from
   the failure record's `captured_swaps` + `hop_outputs` + `optimal_input`
   (no `fetch_onchain`, no recompute). `logs/permutation_analyzer.py`'s
   `classify_candidate` compares each captured swap's output amount (positive
   delta) vs `hop_outputs[i]`: SolverCalc = mismatch, Encoding = all match but
   sim reverted, Unknown = bare revert / no swaps / V4 (gated on `5RI47E`).
   The `DEGENBOT_SIM_TRACE` eprintln block in `simulate_path_on_evm` is
   DROPPED — the inspector's structured capture (captured_swaps +
   reverting_frame) supersedes the per-call gas/halt/revert `eprintln` spam.
6. **Retire `diagnostic.rs` onchain-recompute half** ✓ (`d571553a`) — -1,722
   lines. Deleted `fetch_onchain`, the `recompute_v2/v3/v4_amount_out` family,
   the Multicall3 batching + per-family `build_*`/`decode_*` calls, the
   hand-rolled ABI hex parsers, `HopRecompute` + `DiagnosticHop.recompute`,
   `apply_onchain_fetch`, and the recompute-population loop. RETAINED:
   `FieldDiff` + `compute_field_diffs` (pure typed-diff utilities, re-exported
   for a future lightweight drift detector), `DiagnosticPoolState`,
   `DiagnosticHop` (minus recompute), `DiagnosticPathState`,
   `diagnostic_path_state` (engine-state read, no RPC). PyO3
   `diagnostic_inspect_path` stripped its `fetch_onchain` RPC branch.

Deferred (live-RPC-gated — NOT safely doable without a mainnet provider):

_LEGACY (superseded by step 5+6 delivery above) — the old deferral text is
retained for provenance:_
   and (c) must be sequenced with `WQENYW`'s in-flight `diagnostic.rs`
   submodule split (retire-then-split, or merge). Per AGENTS.md this is a
   coordinated multi-file deletion across Rust + Python + the kill-list
   discipline — NOT a safe unilateral change. **Fork**: do it as its own
   spec-coordinated task once `5RI47E` (V4 seeder) + `WQENYW` (submodule
   split) land, so the V4 recompute retires with the same proof + the file
   split doesn't collide with the deletion.
8. **Tier-2 dual-driver parity pair** ✓ (`437593b2`) — the formal ADR-005
   tier-2 dual-driver pair is DELIVERED. The new `simulate_in_process_revert_probe`
   PyO3 binding exposes the in-process path (`simulate_in_process_with_db` over
   `CacheDB<EmptyDB>`, no RPC) so Python can drive the SAME `0xcafebabe` REVERT
   fixture the Rust smoke test uses. Shared JSON fixture
   (`tests/standalone_parity/fixtures/inspector_cafebabe_revert.json`) carries
   the recorded expected output (reverting_frame depth/target/selector/label,
   captured_swaps=[], bucket) — both `rust/crates/degenbot/tests/parity_inspector.rs`
   (Rust consumer) + `tests/standalone_parity/test_inspector_dual_driver.py`
   (Python consumer) load it + assert byte-exact. A deliberately-wrong fixture
   edit fails BOTH halves (RED-verified). The oracle is a recorded constant
   (weaker than the calc parity's closed form — the revm EVM run is the truth);
   noted in the test header. V4 slice deferred (gated on `5RI47E`).

_LEGACY (superseded by the delivery above) — the old deferral text is
retained for provenance:_

## Real-mainnet validation (archive node — step 6's "prove on real mainnet
paths" gate SATISFIED)

The `swap_capture_correctness` example binary
   (`rust/crates/degenbot-simulation/examples/swap_capture_correctness.rs`,
   commit `e7c88cda`) replays real mainnet swap transactions through a
   `CacheDB<WrapDatabaseAsync<AlloyDB>>` EVM pinned at the parent block with
   the `SwapEventCaptureInspector` attached, asserting the captured swap
   events (emitter + family + amount0 + amount1) byte-match the onchain
   receipts. Opt-in (`DEGENBOT_SWAP_CAPTURE_PROBE=1`); skips cleanly when no
   RPC is configured.

**Result (block 25612576, archive node):**

- V2: tx[0], 1 `Swap` event captured — exact match (emitter + amounts).
- V3: tx[3], 7 `Swap` events captured — all 7 exact-match (emitters + signed
  amounts + sqrt-price/liquidity/tick).

This proves the inspector captures real mainnet V2/V3 `Swap` events with
byte-exact amounts — the ground-truth validation of the captured-swaps
replacement for `diagnostic.rs::recompute_v2/v3_amount_out` (no
`getAmountOut` recompute, no Multicall3 reserves re-fetch needed). The V2
slice required a spec-aligned fix mid-validation (see below).

## V2 capture fix: `Swap` (amounts), not `Sync` (reserves)

Mid-validation, the probe revealed a spec gap: the prototype V2 capture keyed
on the `Sync(uint112,uint112)` event (reserves) and ZEROED the amounts — so
the "`decode_swap_log(event).amount == solver.hop_outputs[i]`" claim (direct
amount comparison, no recompute) could NOT hold for V2. Fix (commit `67f4166e`):

- New `degenbot-decoders::v2_swap_decoder` (mirrors `v3_swap_decoder`):
  `V2_SWAP_TOPIC` + `decode_v2_swap_log` → `V2SwapEvent { sender/to,
  amount0_in/out, amount1_in/out }` + 7 decoder unit tests.
- The inspector now keys V2 on the `Swap` event + maps the in/out amounts to
  the V3 signed-delta convention (`amount0 = amount0_out - amount0_in`: positive
  = token received). The captured V2 amount IS the hop output — retires
  `recompute_v2_amount_out` entirely.
- The composition test was updated to emit a V2 `Swap` (LOG3 + 3 topics +
  128-byte data) + assert `amount0=-1000` (token0 paid in) /
  `amount1=+3000` (token1 received).

## Decisions resolved (no `TBD`)

- **Nested-tuple composition** (not flat 3-tuple) — revm's blanket impl is
  2-tuple-only. Hard constraint.
- **`log_full` over `log`** for swap capture (spike Q1) — `log` fires only for
  frame-init value-transfer logs.
- **Decode-at-capture** in `SwapEventCaptureInspector` (the prototype's shape) —
  `CapturedSwap` stores decoded fields, not raw `Log`s.
- **`classify_revert` stays**, fed at depth by `call_end`'s revert data.
- **The onchain-recompute half of `diagnostic.rs` is DELETED**, not
  feature-gated; the engine-state-read half is RETAINED.
- **V4 amount correctness blocked on `5RI47E`**; V2/V3 unblocked.

## Validation gates (per implementation task)

- `just check-no-pyo3-in-cores` green (the inspectors add no `pyo3`).
- `just lint-rust` green.
- `just test-rust` green — the composition-parity test
  (`tests/inspector_composition.rs`) + the spike probe stay green.
- `just test-python` green once the PyO3 surface + the example rewire
  land.
- Tier-2 parity pair: BOTH halves green (the mechanically-enforced
  "one inspector, two consumers" claim).
