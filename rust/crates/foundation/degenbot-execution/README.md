# degenbot-execution

`degenbot-execution` is the generic foreign-adapter seam (ADR-025, ADR-098
D2): a pyo3-free, foundation-layer crate owning `ExecutionAdapter`, the Encode
seams (`PayloadComposer` for calldata, `PayloadAdapter` for binding it to a
target and native value), the complete `TransactionRequest { to, data, value }`,
the engine's typed `ExecutionObservation`, the solve-result view, the
probe/assess/fee value types used to decide whether a simulated transaction was
profitable, and the neutral opportunity descriptors
(`path::{PathInfo, HopInfo, V2HopInfo, V3HopInfo, V4HopInfo}`). The built-in
`degenbot-executor` depends on this seam, never the reverse (ADR-098 D1).

One solved opportunity is exactly one transaction. The engine owns simulation
mechanics and hands Assess a typed observation (`status`, `gas_used`,
`base_fee_next`, `return_data` when captured, per-probe `before`/`after`
readings, and captured events). The built-in assessment shapes (`sum_of_deltas`
over probe readings, `return_data` over the first return word) are defaults; a
contract that reports profit its own way overrides `assess` and reuses
`ExecutionResult::price` for the defaulted fee half — which also pins the
terminal-status invariant: a reverted observation is always a zero-gross,
`passed = false` decline, even when a custom decoder read profitable-looking
return data or events. An absent observation
(e.g. a failed probe read) is `AssessError::Evidence`, never a fabricated zero;
`Ok(passed = false)` is an economic decline.

Its `ComposerInputs` intentionally carries only solver-driven amounts and
adapter-agnostic options; it does not carry `cmd_executor` addresses or
command-encoding options, and the seam carries no capability, version, or
release metadata — that state stays private to an adapter (ADR-098 D4/D5).

## Validating an adapter (ADR-100)

`interface` adds the opt-in declaration facility beside the seam. Three tiers,
and the default pays none of them:

| Tier | What the operator declares | When it runs |
|---|---|---|
| 0 — none (default) | nothing | today's path, unchanged |
| 1 — contract interface | the contract's ABI plus optional target, selector, and native-value policies | every `encode`, once attached |
| 2 — validation cases | explicit path + amount + expected-transaction literals | an explicit call by the author or a CI test |

A tier-1 declaration is a `ContractInterface`, derived either from standard ABI
JSON (`from_abi_json`) or from the same `sol!` signatures that produce the
calldata (`from_signatures`) — a Rust adapter needs no second ABI source, so its
encoder and its declaration cannot drift. Attach it by wrapping:

```rust,ignore
let interface = ContractInterface::from_signatures([MyCalls::executeCall::SIGNATURE])?
    .with_target(my_contract)?;
let adapter = ValidatedAdapter::new(my_adapter, interface)?;
```

`ValidatedAdapter` forwards Encode, `compose_view`, the declared probes, the
assessment defaults, the fee policy, and the adapter's own `assess` — and
validates the encoded transaction before returning it. It never repairs one: a
violation is `ComposeError::Validation(ContractViolation)`, a hard encode
failure, not the routine capability decline another target could absorb. A
violation is dispositive; a pass is only a screen, because whether the contract
does what the calldata says is proven by execution, not by decoding.

Tier 2 runs the same checks plus an explicit expectation over a real `PathInfo`
and real amounts: `run_cases(&adapter, &interface, &cases)` returns one
`CaseReport` per case, offline, with no provider, engine, or registry.

The concrete built-in production adapter lives in `degenbot-strategy`, where
`CmdExecutorAdapter` owns the canonical `cmd_executor` path. It captures one
strategy `ExecutionContext` (executor, authoritative V4 `PoolManager`, and WETH)
built at boot; frame simulation and V4 descriptor setup consume that same value.
The adapter composes `PathInfo + SolveResult + EncodeOptions` per call. The
result is typed as `Encoded`, `Declined`, or `Rejected`; the five routine decline
labels remain the caller-facing JSONL labels, while a ledger-validation rejection
is fatal.

A pure-Rust consumer can reach the production adapter and context from the
umbrella at `degenbot::CmdExecutorAdapter` / `degenbot::ExecutionContext`, or
under `degenbot::strategy`. Foreign adapters continue to implement the generic seam in
`degenbot-execution` without depending on the built-in strategy adapter.

## Usage

```toml
degenbot-execution = "0.6.0-alpha.5"
```

Or: `cargo add degenbot-execution` (the pre-release version must be pinned
explicitly, e.g. "0.6.0-alpha.5").

Part of [degenbot](https://github.com/BowTiedDevil/degenbot) — a Rust-first MEV bot for EVM chains. The in-repo root README and `docs/` cover the full architecture; this crate is published standalone so you can depend on exactly the pieces you need.
