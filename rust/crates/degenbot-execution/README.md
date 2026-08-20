# degenbot-execution

The ExecutionStrategy seam (ADR-025) — a pyo3-free crate owning the user-owned execution layer: the ExecutionStrategy trait + its value types (solve-result view, gate protocol, ExecutionResult, ComposerInputs) and the PayloadComposer Encode part. No default strategy ships here.

The ExecutionStrategy seam (ADR-025): the trait and its value types (solve-result view, gate protocol, ExecutionResult, composer inputs) for user-owned execution layers. No default strategy ships here.

## Usage

```toml
degenbot-execution = "0.6.0-alpha.5"
```

Or: `cargo add degenbot-execution` (the pre-release version must be pinned explicitly, e.g. "0.6.0-alpha.5").

Part of [degenbot](https://github.com/BowTiedDevil/degenbot) — a Rust-first MEV bot for EVM chains. The in-repo root README and `docs/` cover the full architecture; this crate is published standalone so you can depend on exactly the pieces you need.
