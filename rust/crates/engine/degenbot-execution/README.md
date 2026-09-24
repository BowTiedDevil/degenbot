# degenbot-execution

`degenbot-execution` is the generic foreign-adapter seam (ADR-025): a pyo3-free
crate owning `ExecutionAdapter`, `PayloadComposer`, the solve-result view, and
the probe/assess/fee value types used to build a payload for a user-defined
execution contract. Its `ComposerInputs` intentionally carries only
solver-driven amounts and adapter-agnostic options; it does not carry
`cmd_executor` addresses or command-encoding options.

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
