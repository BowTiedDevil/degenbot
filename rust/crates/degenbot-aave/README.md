# degenbot-aave

Pure-Rust Aave V3 domain crate: the updater chunk-loop (transactional apply of decoded Aave events under one rusqlite Transaction) + the position-analysis math (health-factor / LTV / eMode / isolation) + the Aave V3 fixed-point math libraries (WadRayMath / PercentageMath). Epic AZGJUN.

The Aave V3 domain: a transactional chunk-loop that applies decoded Aave events under one SQLite transaction, position analysis (health factor, LTV, eMode, isolation mode), and the V3 fixed-point math libraries (WadRayMath / PercentageMath).

## Usage

```toml
degenbot-aave = "0.6.0-alpha.5"
```

Or: `cargo add degenbot-aave` (the pre-release version must be pinned explicitly, e.g. "0.6.0-alpha.5").

Part of [degenbot](https://github.com/BowTiedDevil/degenbot) — a Rust-first MEV bot for EVM chains. The in-repo root README and `docs/` cover the full architecture; this crate is published standalone so you can depend on exactly the pieces you need.
