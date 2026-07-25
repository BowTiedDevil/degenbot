# Spike: revm Inspector hooks on the simulation stack

Ergo task **KCKGP4** (epic **63I7WJ** — RevM Inspector-native simulation
diagnostics). Probe: `rust/crates/degenbot-simulation/tests/inspector_spike_probe.rs`
(run: `cargo test -p degenbot-simulation --test inspector_spike_probe -- --ignored --nocapture`).

The probe builds a `ProbeInspector` mirroring revm's `TestInspector` (the event-
capture reference shape in `revm-inspector-42/src/test_inspector.rs`) and attaches
it to `inspect_one` over `CacheDB<EmptyDB>` with hand-rolled bytecode fixtures
(no live RPC, no real pool bytecode). It answers the four gating questions the
implementation-definition task (**JHPW5W**) needs.

## Q1 — LOG capture inside a call

**Answer: yes, but via `log_full`, not `log`.**

- `Inspector::log_full` fires for every LOG opcode emitted during instruction
  execution, and carries the `&mut Interpreter` (so the emitter address is
  available via `interp.input.target_address()`).
- `Inspector::log` does **NOT** fire for instruction-emitted logs. It fires
  only for the frame-init value-transfer path (the `interpreter = None` arm of
  `inspect_logs` in `revm-inspector-42/src/handler.rs`). Evidence: the probe's
  `log_fired` counter stayed at 0 while `log_full_fired` reached 1 for a single
  LOG1 opcode.
- The captured `Log` is a `primitives::Log` (= `alloy_primitives::Log`,
  re-exported by revm). It carries the emitter `address`, `topics()`, and
  `data.data` the decoders expect.
- **Implication for the implementation surface:** the `SwapEventCaptureInspector`
  must override `log_full` (not just `log`), and it receives the interpreter
  context. The V2 `Sync` / V3 `Swap` / V4 `Swap` logs emitted inside a simulated
  `execute()` will be captured here.

### The Log-type conversion finding (important)

`revm::Inspector::log`/`log_full` hand out `alloy_primitives::Log`, but the
`degenbot_decoders::{decode_sync_log, decode_v3_swap_log, decode_v4_swap_log}`
functions consume `alloy::rpc::types::Log` (an RPC wrapper around
`primitives::Log` + optional block/tx metadata). They are **distinct types**.
The probe proved the round-trip by wrapping the captured `primitives::Log` into
an RPC `Log { inner, block_hash: None, …, removed: false }` before calling
`decode_sync_log`, which returned the correct `SyncEvent { pool_address, reserve0=1000, reserve1=2000 }`.

**Decision for JHPW5W:** the implementation-definition task should decide whether
to (a) widen the decoders to accept `primitives::Log` directly (they only read
`.topics()` + `.data`, both present on `primitives::Log` — the RPC wrapper's
block/tx metadata is never read), or (b) wrap at the inspector boundary. Option
(a) is cleaner (the decoders are already pure leaves; the RPC `Log` coupling is
incidental) and removes a conversion the engine shouldn't carry. Flagged for
JHPW5W.

## Q2 — tuple composition on the shared EVM

**Answer: yes — `(AccessListCollector, ProbeInspector)` composes cleanly on one
`inspect_one` run, with no borrow-ordering issues.**

- revm's blanket `Inspector` impl for `(L, R)` (`revm-inspector-42/src/inspector.rs:189+`)
  delegates every hook to both members. The probe attached the tuple to one
  `inspect_one` and confirmed both inspectors fired: the `AccessListCollector`
  produced an access list, and the `ProbeInspector` captured call frames.
- The access list from the composed tuple is **parity-equal** to the
  `AccessListCollector`-alone case (slot set `{slot 1, slot 2}` matches the
  existing `access_list_collector_matches_state_journal` fixture).
- Each inspector keeps its own `Rc<RefCell<…>>` handle; both drain
  independently after `inspect_one` moves the tuple into the EVM. No borrow
  conflicts.
- **Implication:** the production `BlockEvm` inspector type can be a composed
  tuple `(AccessListCollector, CallTraceInspector, SwapEventCaptureInspector)`
  with zero coupling, mirroring `TracerEip3155`'s internal `gas_inspector`
  composition.

## Q3 — `call_end` revert attribution at depth

**Answer: yes — `call_end` receives the `CallOutcome` of the deepest reverting
frame (the child), with its revert data; not just the top-level bubble.**

- The probe's parent contract STATICCALLs a child that reverts with
  `0x00..00_deadbeef` (32 bytes, right-aligned). The top-level call succeeds
  (STATICCALL swallows the child revert; the parent POPs the 0 success flag +
  STOPs). The `call_end` hook for the child frame carries `Revert` with the
  full revert data.
- Captured frames: `depth=1` parent `Success { gas_used=2653 }`,
  `depth=2` child `Revert { gas_used=18, data=0x..deadbeef }`. The reverting
  target address (`0x20..20`) is visible at the reverting frame's `call_end`.
- `call_end` fires LIFO (innermost call before its parent); the
  `CallTraceInspector` must pair frames by matching the most-recently-pushed
  frame whose outcome is still unset (the innermost unmatched), confirmed by the
  probe.
- A contrast run (a top-LEVEL-only revert, no child) showed the reverting frame
  at `depth=1` — so the depth of the reverting frame distinguishes a top-level
  revert from a nested one.
- **Implication:** the `call_end` hook is the attribution seam. The reverting
  frame's `target` + the revert data give `classify_revert` the right input at
  the right depth. The `RevertFrameLocator` walks the `CallTrace` for the
  deepest `Revert` frame after the run. This replaces `SimFailure::fail_index`
  (0–6 top-level call index) with `(reverting_frame_depth,
  reverting_frame_target, reverting_frame_selector, revert_label)`.

## Q4 — V4 swap-event correctness vs the transient seeder

**Answer: DEFERRED — requires the real V4 PoolManager bytecode over the
production DB stack with the transient seeder (task 5RI47E).**

- This `CacheDB<EmptyDB>` probe cannot emit a real V4 `Swap` event (no
  PoolManager bytecode, no transient storage). Q1 proved a hand-rolled LOG1
  round-trips through the decoder, which validates the capture mechanism; but
  V4 `Swap`-event *amount correctness* (do the captured `amount0`/`amount1`
  match the solver's `hop_outputs`?) needs the production stack.
- The blocker is `apply_v4_transient_state`
  (`degenbot-simulation/src/sim/evm/v4_transient.rs`) being a no-op until
  `5RI47E` lands the V4 PoolManager transient-storage slot-key mapping. With
  the seeder a no-op, the PoolManager reads stale on-chain state and the V4
  `Swap` amounts would not match the solver's `hop_outputs` for reasons
  unrelated to a solver bug — a false-positive "SolverCalc."
- **Implication:** the V4 swap-event capture must be gated on `5RI47E` in the
  implementation. JHPW5W should record V4-amount-correctness as blocked on
  `5RI47E`; V2/V3 capture can land independently (V2 `Sync` / V3 `Swap` have no
  transient-storage dependency).

## Summary of findings feeding JHPW5W

1. **Use `log_full`** (not `log`) for swap-event capture; it carries the
   interpreter. `log` is frame-init-only.
2. **Log-type conversion:** capture yields `primitives::Log`; decoders consume
   `rpc::types::Log`. Cleanest fix is widening the decoders to accept
   `primitives::Log` (decision for JHPW5W).
3. **Tuple composition works** — the production `BlockEvm` inspector is a
   composed tuple; `AccessListCollector` parity is preserved.
4. **`call_end` is the revert-attribution seam** — deepest-`Revert` frame walk
   replaces `SimFailure::fail_index`.
5. **V4 capture blocked on `5RI47E`**; V2/V3 unblocked.

## Artifacts

- Probe: `rust/crates/degenbot-simulation/tests/inspector_spike_probe.rs` (4
  `#[ignore]`d tests; run with `--ignored --nocapture`).
- No production code changes. `just check-no-pyo3-in-cores` + `cargo clippy -p
  degenbot-simulation --tests` green.
