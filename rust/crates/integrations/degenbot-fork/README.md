# degenbot-fork

Pure-Rust Anvil fork lifecycle + dev-RPC core (alloy node-bindings).

Anvil fork lifecycle management plus the development RPC core on alloy node-bindings, for driving forked-chain test environments from Rust.

## Usage

```toml
degenbot-fork = "0.6.0-alpha.5"
```

Or: `cargo add degenbot-fork` (the pre-release version must be pinned explicitly, e.g. "0.6.0-alpha.5").

Part of [degenbot](https://github.com/BowTiedDevil/degenbot) — a Rust-first MEV bot for EVM chains. The in-repo root README and `docs/` cover the full architecture; this crate is published standalone so you can depend on exactly the pieces you need.
