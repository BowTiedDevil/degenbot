# degenbot-submission

Pure-Rust transaction-submission mechanism — operator-key holding LocalSigner + EIP-1559 type-2 encoding, nonce-leased dispatch, raw/relay broadcast, receipt monitoring, and submission-ledger reconciliation.

From a profitable candidate to a confirmed nonce: the operator-key-holding LocalSigner, EIP-1559 fee finalization and type-2 signing, nonce-leased dispatch, raw and relay broadcast, receipt monitoring, and the submission ledger reconciled against each new head.

## Usage

```toml
degenbot-submission = "0.6.0-alpha.5"
```

Or: `cargo add degenbot-submission` (the pre-release version must be pinned explicitly, e.g. "0.6.0-alpha.5").

Part of [degenbot](https://github.com/BowTiedDevil/degenbot) — a Rust-first MEV bot for EVM chains. The in-repo root README and `docs/` cover the full architecture; this crate is published standalone so you can depend on exactly the pieces you need.
