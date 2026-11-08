# Your execution strategy, not degenbot's

degenbot's execution side is **your** adapter — the encode/gate seam that turns
a solved path into one transaction for your own contract and decides whether its
simulated result was profitable. If you use the bundled `cmd_executor` contract,
everything just works. But you should **never** be forced onto it. This document
is the contract degenbot offers you for supplying your **own** execution
strategy — your own transaction encoding, your own observation policy, and your
own success/failure assessment — whether you write Rust or Python.

This is the `ExecutionAdapter` seam (ADR-025, deepened by ADR-098 D2). Skip to
the [quick examples](#quick-examples) if you just want to plug in.

## The shape of one opportunity

**One solved opportunity becomes exactly one transaction.** Your Encode part
returns a complete `TransactionRequest`:

```text
to:    Address   # the contract that receives the call
data:  Bytes     # your ABI, your byte format, your choice
value: U256      # native value sent with the call (wei)
```

A contract may batch internally, but the seam carries no multi-transaction
choreography and no consumer-owned simulation loop: the engine executes your one
transaction and hands back evidence. `value` is preserved verbatim from your
adapter through simulation and submission, so a contract funded by `msg.value`
is a first-class citizen.

## The parts

An execution strategy has four parts. Two are *your* code, one is *your*
declared data, and one is the defaulted pricing half of assessment.

| Part | What it does | What you supply | Engine/degenbot provides |
|---|---|---|---|
| **Encode** | solved path → ONE `{to, data, value}` transaction | a function (Rust trait impl / Python callable) + its target/value binding | the `ExecutionAdapter` / `PayloadComposer` seams |
| **Probe** | which pre/post reads to snapshot — a contract call with complete calldata, or a native-account balance | declared data | run the reads, warm cache, access list, report readings |
| **Assess** | typed `ExecutionObservation` → profit + pass/fail | a built-in shape (sum-of-deltas / return-data) **or your own interpreter** | transaction execution, gas, return data, events |
| **Fee** | priority-fee/gas pricing | defaulted (market-percentile); you may override | `FeePolicy` primitives |

`Fee` is the **defaulted pricing half of Assess** — `net = gross − gas ×
(base_fee_next + priority_fee)` is *defined in terms of* your pricing policy, so
it can't be ordered independently. You get a sane default; override it only if
you price differently. A custom assessment reuses `ExecutionResult::price` so
the pricing half stays the seam's.

**You are never asked to own the sim loop.** The engine keeps running your
transaction through evm2 and returns a typed observation; you name *what to
observe* (Probe) and *how to interpret it* (Assess). There is no free-form
"here's the whole `Evm2BlockSimHandle`, you do everything" hook.

## What the engine observes

Assess receives a typed `ExecutionObservation`. Every field is evidence the
engine captured; nothing here is a simulation handle.

| Field | When it is available |
|---|---|
| `transaction` | always — the complete `{to, data, value}` the engine executed |
| `status` | always — `ExecutionStatus::Success` or `ExecutionStatus::Revert` |
| `gas_used` | always — gas consumed by the top-level call, un-inflated |
| `base_fee_next` | always — the simulated block's base fee (wei/gas), the pricing input |
| `return_data` | when the engine captured it; a successful call may return empty data (`Some`, zero bytes), `None` means not captured |
| `probe_reads` | one entry per declared `ProbeSpec`, in declared order — present even when a read failed (`before`/`after` then `None`) |
| `events` | every event captured during the call, in execution order; empty when none were emitted |

A `ProbeReading` exposes `delta()` only when **both** edges were observed; a
missing read is absent, never a fabricated zero. Traces and access lists stay
engine-owned submission/diagnostic mechanics — they are not assessment evidence
and never appear in the observation itself (the leaf hands the access list back
beside it, for submission and diagnostics).

## How the engine runs your transaction

The Rust entry point is `degenbot_simulation::sim::production` —
[`simulate_opportunity`] for the whole seam, and `simulate_transaction` when a
caller already holds the encoded `TransactionRequest`. It is the generic leaf
the canonical dispatch path drives: your Encode, your declared probes (none for
a contract that reports its own receipt, as the built-in executor does), the
engine's execution, and your Assess, in that order.

```rust
let simulation = degenbot_simulation::sim::production::simulate_opportunity(
    &mut session,        // the per-block evm2 session
    &adapter,            // &dyn ExecutionAdapter — foreign or built-in
    &path,               // the neutral hop descriptors
    &solve_result,       // the solved amounts (`compose_view` input)
    &ProductionSimContext { caller, gas_limit },
)?;
// simulation.observation — the typed evidence above
// simulation.result      — your assessment of it
// simulation.access_list — engine-owned submission/diagnostic mechanics
```

The composition contract:

- `ProductionSimContext` carries only the block/deployment facts the engine
  cannot read from the EVM: who sends the transaction (`caller`) and its
  top-level `gas_limit`. The base fee comes from the session's own block env —
  the fee the transaction actually ran under, never a second copy of a caller
  parameter.
- `ProductionSimulation` returns the observation and the assessment together,
  so a caller never assembles evidence itself.
- The leaf composes no encoder, no settlement token, no `PoolManager`, no
  command stream, no version/release policy, and no per-adapter runtime
  override. Actual deployed code is the simulation authority: this leaf injects
  no code and silently substitutes nothing.
- One opportunity is one transaction. The leaf has no simulation-loop hook and
  no multi-transaction choreography.

How the evidence is assembled:

- The transaction executes exactly as Encode returned it, including a nonzero
  native `value`.
- A declared `ProbeSpec` is one of two neutral executable forms, and the leaf
  executes whichever you declared:
  - **`ProbeSpec::Call`** — a contract call carrying its **complete calldata**
    (the 4-byte selector *plus* its ABI arguments). A WETH-shaped
    `balanceOf(executor)` read is the selector followed by the executor's ABI
    word; an ERC6909-shaped `balanceOf(owner, id)` read carries both words. The
    calldata is passed to the declared callee verbatim, and the call's return
    data must be **exactly one ABI word (32 bytes)** — that word is the figure.
    Nothing about the call is inferred from the label.
  - **`ProbeSpec::NativeBalance`** — the declared account's own native (ETH)
    balance, read from the journaled state at that step. No transaction is
    built and nothing is spent on the account's behalf, so the read costs no gas
    and cannot revert; the account *is* the thing observed. An account the state
    layer holds no record of holds zero, which is a fact with both edges present
    (it is not an absent reading).
  Declared pre-reads run in declared order before the transaction and post-reads
  after it, all inside ONE journaled sequence, so both edges (and the
  transaction) observe the same accumulated state.
- A **call** read that yields no figure is absent, never a fabricated zero: a
  reverted or halted call, a call that returned no data, and a call whose return
  data is **not exactly one word** (a short payload is not left-padded into a
  value, and a multi-word payload's words — different fields — are not chosen
  between). The observation still carries one `ProbeReading` per declared spec,
  in declared order, so the default sum-of-deltas assessment reports
  `AssessError::Evidence` rather than pricing a guessed word. The retired
  `address: None` pseudo-read is gone: every declaration is executable, and a
  native read names its account instead of pretending to be a call.
- `gas_used` is the engine's per-call measurement: never inflated by a
  submission safety margin, and never the envelope's gas limit.
- `events` are the transaction's committed logs, with emitter, topics, data, and
  their index within the transaction, in execution order.
- `status` is two-way: a halt (out of gas, invalid opcode) reports `Revert`,
  because it moved no value either.

[`simulate_opportunity`]: ../rust/crates/engine/degenbot-simulation/src/sim/production.rs

### Typed failure semantics

The leaf returns `Result<ProductionSimulation, ProductionSimError>` and never
crashing the bot is part of its contract. The error variants name the part of
the seam that could not deliver:

| Variant | Meaning |
|---|---|
| `Encode(ComposeError)` | your Encode declined this opportunity; nothing was executed |
| `Engine(Evm2SimError)` | the engine refused the journaled sequence (`step_index()` names the refusing step) |
| `MissingExecution` | the engine returned no result for the executed step, so no evidence exists to assess |
| `Assess(AssessError)` | your Assess needs evidence the observation does not carry, or cannot decode it |

An **economic decline is not an error**: it is `Ok` with `result.passed == false`.
A **revert** is a successful run whose assessment declines: the observation
reports the revert, and the shared `ExecutionResult::price` invariant prices it
to zero gross no matter what the payload bytes decode to.

## Assessment semantics

`assess` returns `Result<ExecutionResult, AssessError>`:

- **`Ok(result)` with `passed = true`** — the gate cleared
  (`net_profit ≥ min_net_profit`).
- **`Ok(result)` with `passed = false`** — an economic decline. `gross_profit`
  and `net_profit` are reported; a loss below gas cost floors `net_profit` at
  zero rather than underflowing. A **reverted** observation assesses to a
  zero-gross decline while preserving the observed gas and base fee: the revert
  is evidence on the observation, not an assessment error. This is enforced by
  the shared `ExecutionResult::price` helper, so it holds for **every**
  assessment — a custom `assess` that decodes profitable-looking return data or
  event evidence from a reverted transaction still reports zero gross,
  `passed = false`, and the observed gas/fee facts. A revert never passes.
- **`Err(AssessError::Evidence(..))`** — the observation does not carry the
  evidence your assessment needs (a missing probe reading, no return data, no
  such event). No verdict was reached.
- **`Err(AssessError::Decode(..))`** — the evidence is present but could not be
  decoded.

A failure is never a silent zero-profit pass, and it never crashes the bot: the
engine treats an assessment error as a decline carrying the reason.

The built-in shapes are defaults, not requirements:

- `AssessRule::SumOfDeltas` (default) — `gross = Σ probe deltas`; requires a
  complete before/after reading for **every** declared probe.
- `AssessRule::ReturnData` — `gross = first 32-byte word of the transaction's
  return data`; requires at least one full word.

A contract that reports profit its own way (a bespoke event, its own receipt
struct, a credit-ledger read) overrides `assess` and never selects a built-in
rule.

## Where you plug in

There is **one** seam — `degenbot-execution` — and two ways to reach it:

- **Rust user:** implement a trait in your own crate.
  ```toml
  # Cargo.toml
  degenbot-execution = "0.x"   # the seam: ExecutionAdapter, TransactionRequest, ExecutionObservation, PathInfo/HopInfo
  degenbot-pools = "0.x"       # the solve result type (SolvePathResult)
  ```
  The seam owns the neutral opportunity descriptors, so a foreign adapter
  never depends on the built-in `degenbot-executor` (ADR-098 D1).
  A complete, green reference is the `degenbot-execution-sample` crate in this
  repo — a foreign `SimpleExecutor` strategy wiring all four parts, with its
  own recorded expected-bytes corpus.
- **Python user:** pass a callable plus its transaction binding. The PyO3 layer
  (`degenbot._ffi.execution.PayloadAdapter`, feature `execution`) lifts your
  callable into the same `ExecutionAdapter` trait — the Polars-`map_elements`
  model (Rust holds the callable and invokes it under the GIL). No Rust
  required. Python declares the encoding, the `to`/`value` binding, the probe
  list, and the minimum net profit; the engine and the Rust seam do the rest.
  A custom assessment interpreter is a Rust-side override — Python states
  policy data and does not run a second assessment engine.

Both meet the same seam. degenbot bundles **one** default adapter — the
settlement-arbitrage strategy's `cmd_executor` path (`degenbot-arbitrage`,
Rust-canonical). It is not the only way; it's the default.

## The solve result view

Your Encode part receives a `SolveResult` — the solved path's amounts + hop
descriptors, i.e. `SolvePathResult` (`optimal_input`, `hop_outputs`,
`consumed_inputs`) + `PathInfo` (the seam's hop descriptors: pool addresses,
tokens, direction), exposed to Python as a typed view.

```python
from typing import Protocol

class SolveResult(Protocol):
    path_id: int
    hop_count: int
    optimal_input: int          # the flash input (u256 integer)
    hop_outputs: list[int]      # per-hop outputs (integers, NOT floats)
    consumed_inputs: list[int]  # per-hop consumed inputs (concentrated-liquidity clamp matters)
    net_profit: int
```

Amounts are **integer fixed-point** (not floats) — decimal place matters, so
keep them integers.

## Encoding only? Bind it explicitly

If all you have is a calldata function, implement the smaller `PayloadComposer`
(bytes in, bytes out) and bind it to the transaction it executes:

```rust
let adapter = PayloadAdapter::new(MyComposer, my_contract)   // target
    .with_value(U256::from(5_000_000_000_000_000u64))        // native value
    .with_probes(my_probes)
    .with_assess_options(AssessOptions::default());
```

Calldata alone never pretends to carry a target or a value: binding is an
explicit act, and the binding is what meets the production seam. A composer
whose target or value varies per opportunity implements
`ExecutionAdapter::encode` directly instead.

## Quick examples

Both examples build the same **foreign** `SimpleExecutor` payload — a plain
ABI `execute(uint256,uint256,uint256[])` call whose selector + argument shape
are deliberately distinct from the default `cmd_executor` adapter (the
recorded corpus in the repo asserts this).

### Rust — implement `ExecutionAdapter` for your contract

```rust
use degenbot_execution::{
    AssessError, AssessOptions, ComposeError, ComposerInputs, ExecutionAdapter,
    ExecutionObservation, ExecutionResult, FeePolicy, PathInfo, PayloadComposer, ProbeSpecs,
    TransactionRequest,
};
use alloy::primitives::{Address, Bytes, U256};

pub struct MyComposer { pub executor: Address }

// Encoding-only route: calldata for your contract.
impl PayloadComposer for MyComposer {
    fn compose(&self, _path: &PathInfo, inputs: &ComposerInputs<'_>)
        -> Result<Bytes, ComposeError>
    {
        let final_out = inputs.hop_outputs.last().copied()
            .ok_or_else(|| ComposeError::encode("no hops"))?;
        Ok(build_call(self.executor, (inputs.optimal_input, final_out)))
    }
}

// The full four-part strategy: Encode (complete transaction) + Probe
// (declared data) + Assess/Fee (defaults you may override). See
// `degenbot-execution-sample` for the complete, green implementation.
struct MyAdapter { composer: MyComposer, probes: ProbeSpecs, value: U256 }
impl ExecutionAdapter for MyAdapter {
    fn encode(&self, path: &PathInfo, inputs: &ComposerInputs<'_>)
        -> Result<TransactionRequest, ComposeError>
    {
        let data = self.composer.compose(path, inputs)?;
        Ok(TransactionRequest::new(self.composer.executor, data, self.value))
    }
    fn probe_spec(&self) -> &ProbeSpecs { &self.probes }   // declared reads
    fn assess_options(&self) -> AssessOptions { AssessOptions::default() }
    fn fee_policy(&self) -> FeePolicy { FeePolicy::default() }

    // A contract with its own profit shape overrides the interpretation only.
    // `ExecutionResult::price` still owns the pricing half AND the
    // terminal-status invariant: a reverted observation always prices to zero
    // gross with `passed = false`, whatever this decoder returned.
    fn assess(&self, observation: &ExecutionObservation)
        -> Result<ExecutionResult, AssessError>
    {
        let gross = observation.first_return_word()
            .ok_or_else(|| AssessError::evidence("the contract reports profit in its return data"))?;
        Ok(ExecutionResult::price(
            gross, Vec::new(), observation, U256::ZERO, &self.fee_policy(),
        ))
    }
}
```

### Python — the whole "blob" plus its binding

```python
from degenbot._ffi.execution import PayloadAdapter, abi_encode_call

# Encode — your blob: `SolveResult -> bytes` for YOUR contract.
def my_payload(result):
    return abi_encode_call(
        "execute(uint256,uint256,uint256[])",      # selector + ABI args, degenbot.abi-backed
        [result.optimal_input, result.hop_outputs[-1], list(result.hop_outputs)],
    )

WETH = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
EXECUTOR = "0x7777777777777777777777777777777777777777"

adapter = PayloadAdapter(
    my_payload,
    to=EXECUTOR,                                        # your contract
    value=0,                                            # native value (wei)
    probes=[
        # A contract-call read carries COMPLETE calldata: selector + ABI args.
        {"label": "WETH", "to": WETH, "data": abi_encode_call("balanceOf(address)", [EXECUTOR])},
        # A native-account read observes the account's own ETH balance.
        {"label": "ETH", "account": EXECUTOR},
    ],
    min_net_profit=0,
)
```

Probe (declared pre/post read-calls) and Assess/Fee (built-in sum-of-deltas gate
+ market-percentile pricing) are supplied as declared data / defaults — you do
**not** re-implement the gate or the fee. A complete, runnable example is
`examples/execution_strategy_foreign.py`.

That's it in the common case: a payload function, its target (and value), and
optionally a probe list — the gate and the fee default to the market-shaped
behavior.

### Python — putting that adapter in a session (heterogeneous deployments)

A session that executes at more than one contract declares each additional
deployment and hands the list to the batch executor's one construction
boundary. The fan-out itself is the Rust core's: every solved path becomes one
candidate per registered deployment, in registration order, each with its own
`CandidateId` (path, deployment), its own assessment, and its own submit row.

```python
from degenbot.dispatch import (
    SimulationDeployment,
    build_batch_executor,
    builtin_deployment_id,          # the built-in settlement deployment's id
)

my_deployment = SimulationDeployment(
    2,                              # operator-assigned deployment id
    adapter,                        # the PayloadAdapter above
    "0x19E7…f2A",                   # owner / `from` — the session signer's account
    "0x7777…7777",                  # execution target: the `to` your adapter encodes
    1_000_000,                      # gas limit for the opportunity's transaction
    # code=b"\x60\x00…",           # optional declared runtime (an undeployed build)
)

executor = build_batch_executor(
    context=sim_ctx,
    dispatcher=dispatcher,
    engine=engine,
    signer=signer,
    submit_provider=provider,
    operator_nonce=nonce,
    dry_run=True,
    deployments=[my_deployment],    # None / [] is the built-in one-entry session
)
```

The six declared facts are the whole Python surface:

| Fact | Meaning |
| --- | --- |
| `deployment_id` | the operator-assigned identity; the deployment half of every `CandidateId` it executes. It must be distinct from every other deployment — including `builtin_deployment_id()` |
| `adapter` | the existing `PayloadAdapter` (Encode + declared Probe + assessed minimum net profit). Not a second adapter framework |
| owner | the transaction's `from`; must be the account the session signer holds |
| target | the `to` of the transaction the adapter encodes — the address the submit join stamps |
| `gas_limit` | the top-level gas limit the transaction may burn |
| `code` | an optional declared runtime installed at `target` in the simulated block (explicit development data for an undeployed build); omitted means "simulate the deployed contract" |

A Python deployment deliberately declares **no** settlement warmup (a foreign
contract has no WETH or PoolManager identity, so the engine derives only the
words its paths name) and **keeps adapter-owned fee sizing**: the adapter's own
`ExecutionResult` — priority fee, net, threshold, verdict — reaches the caller
unchanged. Market-aware re-pricing belongs to the built-in settlement binding
only; it is not a second Python strategy.

Composition refusals are the core's, and they arrive as ordinary Python
exceptions — never a `PanicException`, never a Rust abort:

- a **duplicate identity** (`ValueError`: "deployment … is registered twice"),
  including a deployment reusing `builtin_deployment_id()`;
- an **owner the session signer does not hold** (`ValueError`: "… cannot sign
  for …");
- a malformed declaration (`ValueError` for an unparseable address, an empty
  declared runtime, or a zero gas limit; `TypeError` for a non-adapter).

Per-candidate refusals stay per candidate: an adapter that encodes a target other
than its deployment's declared `target` is refused as `target-mismatch` on that
one record, and a revert or failed assessment stays that target's row. No
target's verdict suppresses another's.

Driver path: a session that names its deployments on
`degenbot.runner._sim_submit.ExecutorInputs.deployments` passes them through the
same constructor; a session that names none keeps the one-entry built-in
behavior unchanged.


## What is NOT on offer

- **Re-deriving degenbot's canonical bundle in Python.** The `cmd_executor`
  composer + the settlement-arbitrage balance gate are Rust and stay Rust. The seam
  lets you add *your own contract*; it does not let you re-implement the
  default in another language. Python users *use* the default, and supply their
  own for a *different* contract.
- **A sim-loop hook.** The engine runs the sim; you don't. If you need to
  observe something unusual, say so in **Probe** / **Assess**, not by grabbing
  the engine.
- **Multi-transaction choreography.** One opportunity is one transaction.
- **A fifth seam for fee.** Pricing is the defaulted half of Assess.

## Migration notes (this interface change)

Every consumer-visible transition, Rust and Python:

| Was | Is |
| --- | --- |
| `ExecutionAdapter::encode` / `PayloadComposer` blanket `ExecutionAdapter` returned `Result<Bytes, ComposeError>` | `ExecutionAdapter::encode` returns `Result<TransactionRequest, ComposeError>` — `{to, data, value}`; a bytes-only composer is bound explicitly with `PayloadAdapter::new(composer, to).with_value(..)` |
| `ExecutionAdapter::compose_view` returned `Result<Bytes, _>` | returns `Result<TransactionRequest, _>` |
| `ExecutionAdapter::assess(deltas: &[i128], gas_used: u64, base_fee_next: u128) -> ExecutionResult` | `assess(&ExecutionObservation) -> Result<ExecutionResult, AssessError>`; the engine supplies status, gas, base fee, return data, probe readings, and events |
| `assess_rule() -> Option<AssessRule>` + `min_net_profit() -> U256` trait getters | one `assess_options() -> AssessOptions` (`rule: AssessRule`, `min_net_profit: U256`) |
| `AssessOptions { rule: Option<AssessRule>, .. }` | `AssessOptions { rule: AssessRule, .. }` — a custom interpreter is an `assess` override, not `rule: None` |
| `AssessRule::ReturnValue` (a probe read's value) | `AssessRule::ReturnData` (the transaction's first 32-byte return word) |
| no `AssessError` | `AssessError::{Evidence, Decode}` distinguishes "evidence absent" from "economic decline" |
| `ExecutionResult` built only inside the trait default | unchanged fields, plus `ExecutionResult::price(..)` so a custom `assess` reuses the pricing half; the helper pins the terminal-status invariant — a reverted observation is always a zero-gross, `passed = false` decline, even when a custom decoder found profitable-looking return data or events |
| `degenbot-execution-sample`: `compose_solution(&SolveResult) -> Bytes` | `encode_solution(&SolveResult) -> TransactionRequest` (target + value included) |
| Python `PayloadComposer(callable)` was the whole strategy (implied full adapter) | `PayloadComposer(callable)` stays the encoding-only route; `PayloadAdapter(callable, to, value=.., probes=.., min_net_profit=..)` is the full adapter returning `{to, data, value}` |
| `ProbeSpec::new(label, Option<Address>, [u8; 4])` — one selector-only form, `address: None` for a pseudo-read with no callee | `ProbeSpec::call(label, to, data)` — a contract call with COMPLETE calldata (selector + ABI arguments), whose figure is exactly one 32-byte ABI return word (any other return length is an absent reading) — or `ProbeSpec::native_balance(label, account)` — the declared account's own native balance read from state. `ProbeSpec::label()` is the diagnostic name. A native read is a real observation, never a fabricated call, and every declaration is executable (the no-callee pseudo-read is retired) |
| Python `probes=[("WETH", "0x…", b"\x70\xa0\x82\x31")]` (`(label, address, selector)` triples; a native read was `(label, None, selector)`) | `probes=[{"label": "WETH", "to": "0x…", "data": <complete calldata>}, {"label": "ETH", "account": "0x…"}]` — one dict per probe; keys must be exactly one declaration's set, so the retired triple (which cannot carry a call's arguments) fails loud at the boundary with `ValueError` |
| a `degenbot-execution-sample` probe declaring WETH's selector with no argument | the sample declares `WETH.balanceOf(executor)` as complete calldata, so the read observes the sample executor's own balance |
| `Evm2SimRun` carried transaction steps only | `Evm2SimStep::ReadNativeBalance(account)` is a sequence step that reports the account's balance in `Evm2SimRun::native_balance` (zero gas, no calldata, `None` on transaction steps); native reads therefore sit at their declared position in the one journaled sequence |
| no engine-side entry point took an adapter | `degenbot_simulation::sim::production::{simulate_opportunity, simulate_transaction}` execute one adapter transaction beside its declared probes and return the adapter's assessment. Canonical dispatch drives that leaf: the built-in deployment's adapter is the only producer of its transaction, and it assesses the contract's own `ExecutionReceipt` instead of a balance bundle |

## Boundaries this chunk does not cross

- **Target SELECTION and version policy.** Deployment identity and explicit
  all-target fan-out are here (`DeploymentId`/`CandidateId`, the ordered
  `DeploymentRegistry`, `dispatch_across_deployments`); _choosing_ one target
  from several (ordered / first-compatible selection) is its own policy, as is
  `cmd_executor` protocol-version/release policy. The generic seam carries no
  capability, version, or release metadata, and an adapter keeps any such state
  private to itself.

## Yes, this works for the top two DEs

Concrete, green, golden-pinned references ship in this repo:

- **Rust** — `rust/examples/degenbot-execution-sample` (a foreign `SimpleExecutor`
  strategy wiring all four parts; its recorded expected-bytes corpus is pinned
  as its own golden, distinct from `cmd_executor`).
- **Python** — `examples/execution_strategy_foreign.py` (the Encode blob over
  `degenbot._ffi.execution` + `abi_encode_call`, bound with `PayloadAdapter`).

Both produce the **same** recorded foreign payload byte-for-byte (cross-layer
parity oracle). The Python example is the seam lift alone; registering that
adapter in a live session is the deployment configuration above
(`SimulationDeployment`), which rides the same canonical fan-out the Rust
consumer drives — one fan-out implementation, two shells.

## See also

- [ADR-025 — the `ExecutionAdapter` seam](adr/ADR-025-execution-strategy-seam.md)
- [ADR-098 — executor-agnostic production execution](adr/ADR-098-executor-agnostic-production-execution.md)
- `GLOSSARY.md` — the seam / two-consumer (Rust engine, Python driver shell)
  framing.
