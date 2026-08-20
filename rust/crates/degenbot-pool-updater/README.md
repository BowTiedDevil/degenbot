# degenbot-pool-updater

Pure-Rust pool-updater chunk-loop core: typed RPC event fetching + decode-to-row-input mapping (epic 2SFL6I).

The pool-updater chunk-loop core: typed RPC event fetching and the decode-to-row-input mapping, the Rust side of the DB-aware pool state update path.

## Usage

```toml
degenbot-pool-updater = "0.6.0-alpha.5"
```

Or: `cargo add degenbot-pool-updater` (the pre-release version must be pinned explicitly, e.g. "0.6.0-alpha.5").

Part of [degenbot](https://github.com/BowTiedDevil/degenbot) — a Rust-first MEV bot for EVM chains. The in-repo root README and `docs/` cover the full architecture; this crate is published standalone so you can depend on exactly the pieces you need.
