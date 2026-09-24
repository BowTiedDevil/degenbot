# degenbot-submission

Pure-Rust EIP-1559 transaction signing + fee finalization — operator-key holding LocalSigner + TxEnvelope/Typed2718 type-2 encoding.

EIP-1559 transaction signing and fee finalization: an operator-key-holding LocalSigner plus TxEnvelope / Typed2718 type-2 encoding for settlement submission.

## Usage

```toml
degenbot-submission = "0.6.0-alpha.5"
```

Or: `cargo add degenbot-submission` (the pre-release version must be pinned explicitly, e.g. "0.6.0-alpha.5").

Part of [degenbot](https://github.com/BowTiedDevil/degenbot) — a Rust-first MEV bot for EVM chains. The in-repo root README and `docs/` cover the full architecture; this crate is published standalone so you can depend on exactly the pieces you need.
