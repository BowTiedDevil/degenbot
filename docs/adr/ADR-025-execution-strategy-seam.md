# ADR-025: The `ExecutionAdapter` seam — a deep, user-owned execution layer over the thin engine

**Status: accepted.** In response to a Candidate-1 architecture review of the
composer explosion in `degenbot-executor/src/composers.rs`, grilling reframed
the repository's actual friction: the execution side of degenbot is **overfit to
one searcher's `cmd_executor` contract** (the 27-way `three_hop_*` permutation
fan-out + the dead `V4V4ArbitragePayload`/`V4V3ArbitragePayload`/
`CmdExecutorComposer` payload builders + the 7-call success/failure balance
gate). The fix is not just to collapse those functions — it is to give the
execution side a **real seam** so an arbitrary user can turn a solver result
into a payload for *their own* contract, and define *their own* simulation
success/failure gate, without being wedged into the developer's executor or the
backrun strategy's 7-call balance bundle.

## Context

Two first-class consumers exist per AGENTS.md — a pure-Rust MEV bot and a
Python-driven bot that is a thin driver shell (Rust is the engine; Python is a
cockpit, *not* a co-implementation). degenbot must not force either consumer
onto any one searcher's executor contract design.

Today the execution side is overfit. `degenbot-executor/src/composers.rs`
(189 KB) is a **shallow module**: its public entry `encode_cmd_stream` fans out
to 27 `three_hop_*` + 8 `encode_cmd_*` permutation bodies (V2/V3/V4 × …), each
hand-rolling the address-table setup, native↔WETH bridge, CL-clamp intake, and
`enc_*` primitive calls. Adding a 4th DEX family would multiply the surface
(64 + 16). On top of that, `V4V4ArbitragePayload`, `V4V3ArbitragePayload`, and
`CmdExecutorComposer` have **zero production callers** — they survive only via
the byte-parity tests in `tests/composers_parity.rs`. And the success/failure
gate is the backrun strategy's 7-call pre/post-balance bundle (WETH9
`balanceOf` / Multicall3 `getEthBalance` / PoolManager ERC6909) — inseparable
from one funding model and one executor contract.

The overfitting concern was stated explicitly: *"I don't want to force any user
to use my particular design on the execution side — this layer needs to support
an arbitrary user transforming a solver result into a payload compatible with
their contract."* That declared requirement is the **forcing function** that
makes the seam real (the codebase's two-adapter rule + "revisit only on a
forcing function" discipline from the multicall3-batch/ArcSwap dispositions).
It also refines ADR-019, which already established that the backrun 7-call
bundle + `decode_balance` + `compute_priority_fee` are *searcher code*, out of
scope for the thin `degenbot-simulation` engine.

## Decision

### D1 — `degenbot-execution` owns the generic foreign-adapter seam.

The dedicated, pyo3-free `degenbot-execution` crate owns the generic
`ExecutionAdapter` / `PayloadComposer` traits and their value types: the
solve-result view, `ComposerInputs`, the probe/assess protocol, and
`ExecutionResult`. Its `ComposerInputs` deliberately contains solver-driven
amounts and adapter-agnostic options only; it does not carry command-executor
addresses or `EncodeOptions`. The dependency direction remains a DAG, with
`pyo3` confined to the binding layer.

The concrete production `cmd_executor` adapter is separate and lives in
`degenbot-strategy`, not in this generic seam. `CmdExecutorAdapter` is the
built-in adapter for the developer's `cmd_executor` contract, while a foreign
searcher implements the generic seam in its own crate. This preserves the
original requirement that an arbitrary user can target a contract without
depending on the built-in strategy.

### D2 — The strategy decomposes into four obvious parts.

Ergonomics for a Python user with no Rust background (the Polars `map_elements`
ideal — "a simple blob of code that just works") are achieved by splitting the
strategy into parts that are either *user code* or *declared data*:

- **Encode** — a function / blob: `solve result → payload bytes`. This is the
  `PayloadComposer` seam (`compose(path, inputs) -> Bytes`). Rust users
  implement it; Python users supply a callable.
- **Probe** — declared data: which pre/post read-calls to snapshot
  `(label, addr, selector)`. The engine runs the reads / warm cache / AL.
- **Assess** — a gate rule: how deltas → gross and pass/fail. Built-in shapes
  (sum-of-deltas, return-value) + an optional tiny user interpreter.
- **Fee** — the **defaulted pricing half of Assess**, not a fifth seam.
  `compute_priority_fee` is already strategy-side (ADR-019); it stays a
  built-in market-percentile default (TARGET_PROFIT_RATIO / age-decay),
  overridable by a foreign searcher.

Only the genuinely variable logic (payload encoding, settlement interpretation)
is user code; the mechanical parts (probes, fee) are data/defaults. Net profit
is defined in terms of the pricing policy, so pricing is not independently
orderable — it is folded into Assess.

### D3 — The built-in command-executor path is strategy-owned and typed.

`degenbot-strategy::CmdExecutorAdapter` is the production built-in adapter for
the canonical `cmd_executor` path. It captures the strategy-owned session
`ExecutionContext` (the executor, authoritative V4 `PoolManager`, and WETH
addresses) at construction. Backrun boot builds that context once and gives
the same value to frame simulation and V4 descriptor/roster projection. Each
adapter call accepts `PathInfo`, `SolveResult`, and a per-call `EncodeOptions`;
the options are deliberately not added to the generic `ComposerInputs`.

The adapter returns `CmdExecutorOutcome::Encoded(Bytes)`,
`CmdExecutorOutcome::Declined(CmdExecutorDecline)`, or
`CmdExecutorOutcome::Rejected(CmdExecutorRejection)`. The five caller-facing
JSONL decline labels are preserved exactly: `unsupported_hop_shape`,
`amount_exceeds_uint96`, `encoding_failed:cmd_stream`,
`encoding_failed:execute_call`, and `mixed_pool_managers`. A
`CmdExecutorRejection::LedgerValidation` is always fatal under ADR-030 and is
never collapsed into a routine decline. The adapter owns the production
composition boundary; the lower-level `degenbot-executor` grammar and ABI
primitives remain implementation details beneath it.

### D4 — The solve-result view protocol.

The seam's input is `SolvePathResult` (amounts: `optimal_input` /
`hop_outputs` / `consumed_inputs`) + `PathInfo` (hop descriptors), projected to
Python as a typed `SolveResult` view — because today the per-hop amounts do not
cross to Python on the clean path (`SimResult` carries pre-built
`execute_calldata`, not the amounts). This is the one genuinely new surface;
both consumer types stay symmetric (Rust uses the same two types directly).

### D5 — The production adapter is the hard cutover boundary.

The landed hard cutover routes canonical strategy composition through
`CmdExecutorAdapter` and its typed outcome. The retired helper names
`compose_candidate`, `build_candidate_calldata`, and `ComposeReject` are not
compatibility aliases and do not exist in canonical strategy callers or tests.
A mechanical umbrella architecture gate scans the strategy `src/` and
`tests/` trees for those names, while a compile-time umbrella test pins the
public paths `degenbot::CmdExecutorAdapter` and
`degenbot::strategy::CmdExecutorAdapter`.

The generic foreign-adapter surface remains available unchanged: Rust users
implement `ExecutionAdapter` / `PayloadComposer` with their own contract, and
Python users continue to supply a callable through the existing PyO3 lift.
The production adapter is a concrete strategy implementation, not a new
`ComposerInputs` policy or a second generic seam.

## Considered options (rejected)

- **Enum-only deepen (no outer seam).** The overfitting concern is a declared
  second adapter, so the two-adapter rule justifies the seam; enum-only leaves
  foreign users forced onto `cmd_executor`. Rejected.
- **A `SimGate` hook inside `degenbot-simulation`.** Re-wedges strategy into the
  thin engine — the exact ADR-019 consequence the repo forbids. Rejected in
  favor of D2/D3 (engine keeps generic probe/execute/decode; the user supplies a
  thin probe-spec + gate, never a free-form sim-loop hook).
- **Seq only-shared-helpers internal collapse.** Improves texture but doesn't
  solve the overfit / don't-wedge concern. Rejected.

## See also

- [Execution strategy — user guide](../execution-strategy.md) — the
  Polars-style Encode blob + Probe declared reads + Assess options + the
  solve-result view protocol, with both the Rust and Python plug-in points and
  the concrete sample references.

## Consequences

- A Rust user `impl ExecutionAdapter` or `PayloadComposer` in their own crate;
  a Python user passes a callable + probe/assess spec via the existing PyO3
  lift. Both foreign paths meet the same generic seam in `degenbot-execution`.
- The built-in production adapter and its typed session context are reachable
  from the umbrella as `degenbot::CmdExecutorAdapter` and
  `degenbot::ExecutionContext`, and from the strategy namespace under
  `degenbot::strategy`; the adapter uses per-call `EncodeOptions`.
- `degenbot-executor` remains the low-level command grammar and ABI support;
  `degenbot-strategy` owns the canonical production adapter boundary. The
  generic execution contract remains "solve result + `degenbot.abi`".
- Canonical strategy behavior is routed through the typed
  `Encoded` / `Declined` / `Rejected` outcomes and the five preserved caller
  labels; ledger-validation rejection remains fatal.
- `pyo3` stays out of all core crates; the lift lives in `degenbot-python`.
- The generic seam stays foreign-adapter-shaped, while the hard cutover keeps
  the retired command-executor helper names out of canonical strategy callers
  and tests. This refines ADR-019, ADR-005, and ADR-015 without adding a
  compatibility layer.
