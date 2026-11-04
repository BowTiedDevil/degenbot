# Inspector-captured simulation diagnostics

How a simulated run becomes attributable evidence: which contract failed, at what
call depth, with what revert data and what label — and what each hop's pool
actually emitted. The capture is one `Inspector` inside the EVM; every consumer
(Rust strategy, FFI record, Python renderer) reads the same engine-neutral
structs, so "the simulation says hop 2 underperformed" is a claim backed by the
run's own logs rather than a second RPC round trip. On the production path the
same evidence is assembled from the executed transaction's state layer instead
of the inspector hooks (evm2's frame dispatch is inspector XOR JIT runner) — see
*Two capture mechanisms*.

## Where the seam lives

| Layer | Home | Shape |
| --- | --- | --- |
| The inspector | `rust/crates/engine/degenbot-simulation/src/sim/evm2/inspectors.rs` | `Evm2SimInspector`, the composite `Inspector<BaseEvmTypes>` impl |
| Capture vocabulary | `.../src/sim/capture/` | `CallFrame`/`CallTrace`/`FrameOutcome`, `CapturedSwap`/`SwapFamily`, `CollectedSlots`, `pair_lifo` |
| Per-block handle | `.../src/sim/evm2/handle.rs` | `Evm2BlockSimHandle` bakes the inspector into the shared EVM at build |
| Sequence capture | `.../src/sim/evm2/session.rs` | `Evm2SequenceCapture` (trace, swaps, reverted swaps, log count, access list) |
| Strategy record | `rust/crates/engine/degenbot-arbitrage/src/simulator.rs` | `SimResult.captured_swaps`; `SimFailure.{reverting_frame, captured_swaps, reverted_swaps, log_full_count, call_trace}`; `RevertingFrame` |
| Retained state read | `rust/crates/engine/degenbot-bot/src/arb_engine/diagnostic.rs` | `DiagnosticPoolState`, `DiagnosticHop`, `DiagnosticPathState`, `FieldDiff`, `compute_field_diffs`, `diagnostic_path_state` |
| Classifier | `rust/crates/engine/degenbot-arbitrage/src/pool_divergence.rs` | `is_solver_calc_failure`, `diverging_pool_keys` |
| FFI records | `rust/crates/shells/degenbot-python/src/simulation/render.rs` | plain `dict` records (no pyclass, no dataclass) |
| Driver renderers | `src/degenbot/runner/_render.py` | the `[sim-fail]` / `[sim-diag]` log lines |

## The composite inspector

One struct, three buffers:

```rust
pub struct Evm2SimInspector {
    swaps: SwapEventBuffer,
    trace: CallTraceBuffer,
    slots: CollectedSlots,
}
```

The buffers live in `sim::capture` and carry no engine types, so the capture
vocabulary is the contract and the inspector is hook bindings over it. The
`Inspector<BaseEvmTypes>` impl translates evm2 hooks into buffer operations:

| evm2 hook | Fires | Drives |
| --- | --- | --- |
| `step` | every opcode | on `SLOAD`/`SSTORE`, records the stack-top slot for the running frame's destination into `CollectedSlots` |
| `log` | at LOG emission | decodes the log into a `CapturedSwap` and pushes it into the top tentative frame of `SwapEventBuffer` |
| `call` / `create` | at frame entry | pushes a `CallFrame` (depth, caller, target, calldata selector, full calldata, gas limit) and opens a tentative swap frame |
| `call_end` / `create_end` | at frame exit, LIFO | pairs the `FrameOutcome` with the innermost unmatched frame via `pair_lifo`, then commits or reverts that frame's swaps |

Draining is a typed retrieval, not a handle collection: `inspector_mut()` on the
EVM downcasts to `Evm2SimInspector` and the caller takes the `take_swaps`,
`take_reverted_swaps`, `take_trace` and `take_access_list` verbs, each of which
resets its buffer for the next run. `log_count()` reports every `log` hook
firing (non-swap logs and reverted-frame logs included) — the "did the hook fire
at all" diagnostic. Inside a journaled sequence the inspector instead rides the
EVM by the slot (`transact_with_inspector`) and the drained capture comes back
as `Evm2SequenceCapture`, so nothing needs a second borrow of the local EVM.

## The captured vocabulary

```rust
pub enum FrameOutcome {
    Success { gas_used: u64, output: Bytes },
    Revert { gas_used: u64, data: Bytes },   // classify_revert input
    Halt { gas_used: u64, reason: String },
}

pub struct CallFrame {
    pub depth: usize,          // 1 = top-level execute()
    pub caller: Address,
    pub target: Address,
    pub selector: [u8; 4],     // first 4 calldata bytes, zero-padded
    pub gas_limit: u64,
    pub data: Bytes,           // full calldata
    pub outcome: Option<FrameOutcome>,
}

pub struct CallTrace { pub frames: Vec<CallFrame> }

pub struct CapturedSwap {
    pub emitter: Address,      // the emitting pool / PoolManager
    pub family: SwapFamily,    // V2 | V3 | V4
    pub amount0: I256,         // signed: positive = token received
    pub amount1: I256,
    pub sqrt_price_x96: U256,  // V3/V4; zero for V2
    pub liquidity: U256,       // V3/V4; zero for V2
    pub tick: i32,             // V3/V4; zero for V2
}
```

`CallTrace` answers the attribution questions directly: `failing_frame()` (the
deepest non-`Success` frame, covering `Revert` and `Halt`), `deepest_revert()`
(`Revert` only), `reverting_frame_label()` (that frame plus its
`classify_revert` label), and `render_debug()` (indented chronological listing
of the whole call sequence — the debug aid for a deep multi-hop failure).

`CapturedSwap` stores decoded fields, never a raw `Log`. V4 emits from the
PoolManager while the V4 decoder carries only `pool_id`, so the emitter is
taken from the log's own address (`from_log_with_emitter`) rather than decoded.

## Two capture mechanisms

evm2's frame dispatch is inspector XOR interpreter-runner, so the production
inspector and the JIT runner cannot both ride a frame. `Evm2CaptureMode` makes
the choice explicit per journaled sequence:

- **`Seam` (production).** The step runs as a plain transaction with the JIT
  runner engaged, and the capture is assembled post-hoc from the state layer:
  slots from the executed transaction's `account_changes()` (every slot loaded
  or written), swaps from the result's journal-scoped committed logs. The
  journal already drops reverted-frame logs, which is the same set the swap
  buffer's LIFO scoping reproduces incrementally. `CallTrace` and the
  reverted-swap diagnostic have no state-layer source and come back empty.
- **`Inspector` (diagnostic).** The composite inspector rides the step via
  `transact_with_inspector` on the interpreted path, giving the full call trace
  and the reverted-frame swap buffer.

`rust/crates/engine/degenbot-simulation/examples/capture_seam_probe.rs` is the
equivalence gate between them: the seam's slot and swap sets must equal the
inspector's, with the JIT backend reporting compiled execution.

## Revert and halt attribution

`SimFailure` carries the failing frame, not just the top-level bubble:

```rust
pub struct RevertingFrame {
    pub depth: usize,
    pub target: Address,
    pub selector: [u8; 4],
    pub revert_data: Bytes,     // empty for a Halt
    pub label: String,          // classify_revert(revert_data)
    pub outcome_kind: &'static str, // "revert" | "halt"
    pub gas_used: u64,
}
```

`FailBuckets::record_revert` stores it beside the bucket label, the captured
swaps seen before the failure, the solver's `optimal_input` and its
`hop_outputs`. `classify_revert` (`degenbot-decoders::revert`) is unchanged —
it is simply fed the failing frame's own data instead of the top-level bubble.
A `Halt` (out-of-gas, invalid opcode, call-too-deep) carries no revert data, so
it lands in the `empty` bucket with `outcome_kind = "halt"`; `outcome_kind` plus
`gas_used` (against the frame's `gas_limit`) is what separates an OOG-truncated
march from a clean `require` failure.

When no trace was captured (the `Seam` mode above), the strategy synthesizes the
frame from the failing call's own envelope — top-level target, depth 1, the
envelope's selector — which is honest for the common executor-level
`REVERT`/`INVALID` and is explicitly not inner-pool attribution. Inner-frame
attribution requires the `Inspector` capture mode.

## Swap capture replaces the on-chain recompute

The pools' own `swap()` calls emit `Swap` logs during `execute()`; the log hook
decodes them with `degenbot-decoders` (`decode_v2_swap_log`,
`decode_v3_swap_log`, `decode_v4_swap_log`), so "did the solver's
`hop_outputs[i]` match reality" is a comparison between a captured amount and a
solver-reported number — no Multicall3 re-fetch, no `getAmountOut` recompute, no
tick-map fetch, no storage-layout coupling.

Two properties make the captured amount the hop output:

- **V2 keys on `Swap`, not `Sync`.** V2 pairs emit both `Sync` (reserves) and
  `Swap` (in/out amounts). Capturing `Sync` leaves amounts underivable from a
  single event; capturing `Swap` and mapping `amount0 = amount0_out -
  amount0_in` gives the V3 signed convention (positive = received by the
  swapper).
- **Reverted frames are scoped out.** `log` fires at LOG emission, before the
  enclosing frame's revert resolves, so a swap emitted in a reverting sub-call
  would over-capture against the on-chain receipt. The `SwapEventBuffer`
  frame stack (`begin_frame` / `end_frame_committed` / `end_frame_reverted`)
  keeps `take_swaps()` limited to committed frames and preserves
  reverted-frame swaps separately in `take_reverted_swaps()` — the divergence
  diagnostic that distinguishes "the hop never committed" from "the frame-stack
  pairing misclassified a commit".

The `SwapEventBuffer` carries the recorded evidence for both rules in its doc
comment: the reverted-frame over-capture surfaced at block 25615015 tx[0] (8
reverted-frame logs against 22 receipt commits), and the V2/V3 byte-exact
capture at block 25612576. The `swap_capture_correctness` mainnet probe binary
that produced those numbers retired with the revm lab cluster; the recorded
numbers remain the reference, and the offline `tier3_*_pool_swap_vs_reference_bytecode`
suites in `degenbot-simulation/tests/` gate the same claim against the
reference pool bytecode without needing RPC.

## Consumers

- **`SimResult.captured_swaps`** — the success path's per-hop actual amounts.
- **`SimFailure`** — `reverting_frame`, `captured_swaps` (the swaps seen before
  the failure, which is *why* a partial fill blew a price limit),
  `reverted_swaps`, `log_full_count`, and `call_trace` (a compact per-frame
  `depth,target,selector,outcome_kind,gas_used` summary for the no-profit
  case).
- **The classifier** — `is_solver_calc_failure` answers the dispatch question:
  `SolverCalc` means the failure carries captured swaps whose positive amount
  differs from the solver's `hop_outputs[i]` (a count mismatch is deliberately
  *not* `SolverCalc`). `diverging_pool_keys` names the mismatching hops so the
  feedback path can penalize those pools. The other verdicts in the log
  taxonomy (`Encoding`, `Drift`, `Unknown`) are render-side labels, not
  dispatch policy.
- **The FFI** — `degenbot-python/src/simulation/render.rs` builds plain dicts: `reverting_frame`
  (depth/target/selector/revert_data/label/outcome_kind/gas_used),
  `captured_swaps` and `reverted_swaps` rows
  (family/emitter/amount0/amount1/sqrt_price_x96/liquidity/tick), `hop_outputs`
  and `optimal_input` as the expected half. No business logic crosses the
  boundary.
- **The driver** — `src/degenbot/runner/_render.py` renders `[sim-fail]` /
  `[sim-diag]` from those dicts; `simulate_in_process_revert_probe` and
  `simulate_in_process_success_probe` expose the in-process path over
  `CacheDB<EmptyDB>` so Python can drive a fixture with no RPC at all.

## The `diagnostic.rs` boundary

The file's on-chain-recompute half — `fetch_onchain`, the
`recompute_v2/v3/v4_amount_out` family, the Multicall3 batching, the hand-rolled
ABI hex parsers, `HopRecompute` and `DiagnosticHop::recompute` — is gone;
capture replaced it. What remains is the engine-state-read half, which answers a
different question: `DiagnosticPoolState`/`DiagnosticHop`/`DiagnosticPathState`
plus the pure typed diff utilities `FieldDiff` and `compute_field_diffs`, served
by `diagnostic_path_state` with no RPC.

## Proof

- **Tier 1 (reachability)** —
  `rust/crates/facade/degenbot/tests/reachability.rs` asserts that every core
  crate the PyO3 binding delegates into is re-exported by the umbrella crate.
  That is what makes `simulate_in_process_with_db` and the captured structs
  resolvable from a standalone `cargo add degenbot` consumer.
- **Tier 2 (dual-driver behavioral parity)** — the shared fixture
  `tests/standalone_parity/fixtures/inspector_cafebabe_revert.json` (a
  `0xcafebabe` `REVERT` stub executor over a 2-hop V2 path against
  `CacheDB<EmptyDB>`) is asserted byte-exact by both halves:
  `rust/crates/facade/degenbot/tests/parity_inspector.rs` (Rust consumer) and
  `tests/standalone_parity/test_inspector_dual_driver.py` (Python consumer via
  the PyO3 probe). The oracle is a recorded constant — the evm2 run is the
  truth — so a dropped field, a changed address format or a re-rendered revert
  payload makes the two consumers disagree with the fixture.
- **Capture equivalence** — `examples/capture_seam_probe.rs` under
  `rust/crates/engine/degenbot-simulation/` (see above).
- **Offline pool truth** — the `tier3_*` reference-bytecode suites in
  `degenbot-simulation/tests/` run the canonical pool contracts on evm2 and
  prove the Rust swap math byte-exact against them, so the amounts the capture
  reports are checked against the same engine that produced them.

## Superseded revm-era API

The capture landed with revm first; the shapes below no longer exist and are
listed so a reader arriving from older notes can map them onto the evm2 stack.

| Retired | Current reality |
| --- | --- |
| `SimInspector`, a nested tuple `(AccessListCollector, (CallTraceInspector, SwapEventCaptureInspector))` type parameter on `BlockEvm` — the nesting was forced by revm's 2-tuple blanket `Inspector` impl | `Evm2SimInspector`, one struct implementing `Inspector<BaseEvmTypes>` over three shared buffers |
| revm `Inspector::log_full` for LOG opcodes (`log` fired only on the frame-init value-transfer path) | evm2's single `log` hook fires at LOG emission |
| revm `frame_start`/`frame_end` driving the swap frame stack | evm2 `call`/`create` and `call_end`/`create_end` drive it |
| `Rc<RefCell<…>>` inspector handles drained through `AccessListHandle`/`CallTraceHandle`/`SwapEventCaptureHandle` | typed retrieval: `inspector_mut()` + `NonStaticAny` downcast + the `take_*` verbs |
| `sim::evm::simulator::BlockSimHandle` and its `sim::evm` module | `sim::evm2::handle::Evm2BlockSimHandle` |
| `WrapDatabaseAsync<AlloyDB>` as the production cold-miss adapter | `BlockingAsyncDb` over `AlloyAsyncDb` |
| `format_sim_diag_line` in the Python oracle scripts | `simulation/render.rs` dict records + `_render.py` |
