# ADR-025: The `ExecutionStrategy` seam — a deep, user-owned execution layer over the thin engine

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

### D1 — A new `degenbot-execution` crate owns the `ExecutionStrategy` seam.

A dedicated, pyo3-free crate owns the `ExecutionStrategy` trait + its value
types (the solve-result view, the gate protocol, `ExecutionResult`). It holds
**no default strategy**. The constrain:

- It **cannot** live in `degenbot-simulation` — ADR-019's load-bearing
  consequence forbids re-wedging strategy into the thin engine.
- It **cannot** live in `degenbot-executor` — the executor is *the developer's
  `cmd_executor` adapter* (ADR-025-a), not a general execution layer.
- It **cannot** live in `degenbot-backrun-strategy` — that crate is one example
  strategy; a foreign searcher's crate must not depend on it just to reach the
  interface.

`ExecutionStrategy` is the execution-side twin of ADR-015's `degenbot-solvers`
relocation: a shared seam crate both the standalone-Rust path and the PyO3 shell
consume. Dep graph stays a DAG (`execution → {executor, simulation, solvers}`),
no cycles, pyo3 stays in the shell.

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

### D3 — Default stays Rust-canonical; the canonical dispatch is a wall.

`degenbot-backrun-strategy` implements `ExecutionStrategy` as the **default
adapter** (stays Rust-canonical, per ADR-019 decision R). The canonical
`dispatch_profitable_results` / `dispatch_profitable_py` **never reads a Python
transform** — it uses the Rust default adapter only and returns
`execute_calldata` exactly as today. This is what keeps Python from re-deriving
the canonical 7-call bundle (ADR-019 R + AGENTS.md "driver shell, not a
co-implementation"): the seam *adds* a foreign-contract path; it does not
re-derive the canonical one. A foreign user's transform is their own searcher
code over the thin engine, not a second implementation degenbot ships.

### D4 — The solve-result view protocol.

The seam's input is `SolvePathResult` (amounts: `optimal_input` /
`hop_outputs` / `consumed_inputs`) + `PathInfo` (hop descriptors), projected to
Python as a typed `SolveResult` view — because today the per-hop amounts do not
cross to Python on the clean path (`SimResult` carries pre-built
`execute_calldata`, not the amounts). This is the one genuinely new surface;
both consumer types stay symmetric (Rust uses the same two types directly).

### D5 — The original Candidate-1 work becomes internals of the default adapter.

The legacy encoders are deleted (facet B) and the 27+8 combinatorial fan-out is
collapsed behind `CmdExecutorComposer::compose` (facet A), as **internals of the
default adapter** behind the `PayloadComposer` seam — all Red→Green against the
existing golden-master vectors (`composers_parity.rs` / `composers_3hop_parity.rs` /
`native_eth_3hop_bridge.rs`), which now pin the default adapter's output.

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

- A Rust user `impl ExecutionStrategy` in their own crate; a Python user passes
  a callable + probe/assess spec via the PyO3 lift (`PyPayloadComposer` /
  `PyExecutionStrategy`) — both meet the *same* seam in `degenbot-execution`.
- `degenbot-executor` is repositioned as the developer's `cmd_executor` adapter;
  the general execution contract is "solve result + `degenbot.abi`".
- Canonical bot behavior is byte-identical (default adapter, golden-pinned);
  canonical dispatch never sees a foreign transform, preserving ADR-019 R.
- `pyo3` stays out of all core crates; the lift lives in `degenbot-python`.
- Low-ceremony: a new crate owning a trait + value types + no default. It
  refines ADR-019 (names the seam), ADR-005 (canonical `enc_*` primitives stay
  the single wire-format source), and ADR-015 (parallel seam shape).
