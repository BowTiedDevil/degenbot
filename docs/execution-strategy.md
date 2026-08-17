# Your execution strategy, not degenbot's

degenbot's execution side is **your** `cmd_executor` adapter — the `enc_*`
opcode primitives + the composer that turn a solved path into `execute()`
calldata for your Vyper contract. If you use that contract, everything just
works. But you should **never** be forced onto it. This document is the
contract degenbot offers you for supplying your **own** execution strategy —
your own payload encoding and your own success/failure gate — for **your own**
contract, whether you write Rust or Python.

This is the `ExecutionStrategy` seam (ADR-025). Skip to the [quick
examples](#quick-examples) if you just want to plug in.

## The parts

An execution strategy has four obvious parts. Three are `user code` or
`declared data`; one is a built-in default.

| Part | What it does | What you supply | Engine/degenbot provides |
|---|---|---|---|
| **Encode** | solve result → payload `bytes` for *your* contract | a function / blob | the `PayloadComposer` seam |
| **Probe** | which pre/post read-calls to snapshot (`label, addr, selector`) | declared data | run the reads, warm cache, access list |
| **Assess** | how the deltas → profit + pass/fail | a gate rule (builtin shapes: sum-of-deltas, return-value) or a tiny interpreter | `decode`, gas/priority primitives |
| **Fee** | priority-fee/gas pricing | defaulted (market-percentile); you may override | `compute_priority_fee` primitives |

`Fee` is the **defaulted pricing half of Assess** — `net = gross − gas ×
(base_fee_next + priority_fee)` is *defined in terms of* your pricing policy, so
it can't be ordered independently. You get a sane default; override it only if
you price differently.

**You are never asked to own the sim loop.** The engine keeps running your
payload through revm and returning per-call outcomes; you name *what to
observe* (Probe) and *how to interpret it* (Assess). There is no free-form
"here's the whole `BlockSimHandle`, you do everything" hook.

## Where you plug in

There is **one** seam — `degenbot-execution` — and two ways to reach it:

- **Rust user:** implement a trait in your own crate.
  ```toml
  # Cargo.toml
  degenbot-execution = "0.x"   # the seam (PayloadComposer, ExecutionStrategy, Probe/Assess/Fee types)
  degenbot-executor = "0.x"    # PathInfo / HopInfo hop descriptors
  degenbot-solvers = "0.x"     # the solve result type (SolvePathResult)
  ```
  A complete, green reference is the `degenbot-execution-sample` crate in this
  repo — a foreign `SimpleExecutor` strategy wiring all four parts, with its
  own recorded expected-bytes corpus.
- **Python user:** pass a callable + a probe/assess spec. The PyO3 layer
  (`degenbot._ffi.execution.PyPayloadComposer`, feature `execution`) lifts your
  callable into the same `PayloadComposer` / `ExecutionStrategy` trait — the
  Polars-`map_elements` model (Rust holds the callable and invokes it under the
  GIL). No Rust required.

Both meet the same seam. degenbot bundles **one** default adapter — the
settlement-arbitrage strategy's `cmd_executor` path (`degenbot-arbitrage`,
Rust-canonical). It is not the only way; it's the default.

## The solve result view

Your Encode part receives a `SolveResult` — the solved path's amounts + hop
descriptors, i.e. `SolvePathResult` (`optimal_input`, `hop_outputs`,
`consumed_inputs`) + `PathInfo` (pool addresses, tokens, direction), exposed to
Python as a typed view.

```python
from typing import Protocol

class SolveResult(Protocol):
    path_id: int
    hop_count: int
    optimal_input: int          # the flash input (u256 integer)
    hop_outputs: list[int]      # per-hop outputs (integers, NOT floats)
    consumed_inputs: list[int]  # per-hop consumed inputs (CL clamp matters)
    net_profit: int
```

Amounts are **integer fixed-point** (not floats) — decimal place matters, so
keep them integers.

## Quick examples

Both examples build the same **foreign** `SimpleExecutor` payload — a plain
ABI `execute(uint256,uint256,uint256[])` call whose selector + argument shape
are deliberately distinct from the default `cmd_executor` adapter (the
recorded corpus in the repo asserts this).

### Rust — implement `ExecutionStrategy` for your contract

```rust
use degenbot_execution::{
    AssessRule, ComposeError, ComposerInputs, ExecutionStrategy, FeePolicy,
    PayloadComposer, ProbeSpec, ProbeSpecs,
};
use degenbot_executor::composers::PathInfo;
use alloy::primitives::{Address, Bytes};

pub struct MyComposer { pub executor: Address }

// Encode — the only unconditional user code (ADR-025 D2).
impl PayloadComposer for MyComposer {
    fn compose(&self, _path: &PathInfo, inputs: &ComposerInputs<'_>)
        -> Result<Bytes, ComposeError>
    {
        let final_out = inputs.hop_outputs.last().copied()
            .ok_or_else(|| ComposeError::encode("no hops"))?;
        Ok(build_call(self.executor, (inputs.optimal_input, final_out)))
    }
}

// The full four-part strategy: Encode + Probe (declared data) + Assess
// (built-in gate) + Fee (defaulted). See `degenbot-execution-sample` for the
// complete, green implementation.
struct MyStrategy { composer: MyComposer, probes: ProbeSpecs }
impl ExecutionStrategy for MyStrategy {
    fn encode(&self, path: &PathInfo, inputs: &ComposerInputs<'_>)
        -> Result<Bytes, ComposeError> { self.composer.compose(path, inputs) }
    fn probe_spec(&self) -> &ProbeSpecs { &self.probes }   // declared reads
    fn assess_rule(&self) -> Option<AssessRule> { Some(AssessRule::SumOfDeltas) }
    fn fee_policy(&self) -> FeePolicy { FeePolicy::default() }
}
```

### Python — the whole "blob"

```python
from degenbot._ffi.execution import PyPayloadComposer, abi_encode_call

# Encode — your blob: `SolveResult -> bytes` for YOUR contract.
def my_payload(result):
    return abi_encode_call(
        "execute(uint256,uint256,uint256[])",      # selector + ABI args, degenbot.abi-backed
        [result.optimal_input, result.hop_outputs[-1], list(result.hop_outputs)],
    )

composer = PyPayloadComposer(my_payload)   # lifted into the PayloadComposer seam
```

Probe (declared pre/post read-calls: `(label, addr, selector)`) and Assess/Fee
(built-in sum-of-deltas gate + market-percentile pricing) are supplied as
declared data / defaults — you do **not** re-implement the gate or the fee. A
complete, runnable example is `examples/execution_strategy_foreign.py`.

That's it in the common case: a payload function (+ optionally a probe list),
and the gate and the fee default to the market-shaped behavior.

## What is NOT on offer

- **Re-deriving degenbot's canonical bundle in Python.** The `cmd_executor`
  composer + the settlement-arbitrage 7-call balance gate are Rust and stay Rust. The seam
  lets you add *your own contract*; it does not let you re-implement the
  default in another language. Python users *use* the default, and supply their
  own for a *different* contract.
- **A sim-loop hook.** The engine runs the sim; you don't. If you need to
  observe something unusual, say so in **Probe** / **Assess**, not by grabbing
  the engine.
- **A fifth seam for fee.** Pricing is the defaulted half of Assess.

## Yes, this works for the top two DEs

Concrete, green, golden-pinned references ship in this repo:

- **Rust** — `rust/crates/degenbot-execution-sample` (a foreign `SimpleExecutor`
  strategy wiring all four parts; its recorded expected-bytes corpus is pinned
  as its own golden, distinct from `cmd_executor`).
- **Python** — `examples/execution_strategy_foreign.py` (the Encode blob over
  `degenbot._ffi.execution` + `abi_encode_call`).

Both produce the **same** recorded foreign payload byte-for-byte (cross-layer
parity oracle) and never touch the canonical `dispatch_profitable_*` fan-out.

## See also

- [ADR-025 — the `ExecutionStrategy` seam](adr/ADR-025-execution-strategy-seam.md)
- ADR-019 — `cmd_executor` + the settlement-arbitrage 7-call balance gate are Rust and stay
  Rust (the default adapter); pricing folds into Assess.
- `CONTEXT.md` — the seam / two-consumer (Rust engine, Python driver shell)
  framing.
