# degenbot-decoders

Pure-Rust Uniswap V2/V3/V4 and Aave V3 event-log decoders (alloy-only leaf; no pyo3/tokio/degenbot-core).

Event-log decoders for Uniswap V2/V3/V4 and Aave V3 as an alloy-only leaf crate — deliberately tiny (no pyo3, tokio, or core dependency) so decoding stays fast anywhere it is needed.

Every decoder returns a plain `Option<Event>`: `None` means the log is not that event (foreign topic, missing topic, or a payload that does not fit its declared signature). There is no catch-all variant.

## Aave V3 module homes

- `aave_event_decoder` — the typed operation events (Supply/Borrow/Repay/Withdraw/LiquidationCall/DeficitCreated, scaled-token Mint/Burn/BalanceTransfer, configuration, GHO discount, staking, oracle, and ERC-20 Transfer), dispatched by `decode_aave_log`.
- `aave_report_decoder` — the 25 neutral non-operation reports (Pool flash-loan, deficit-coverage, isolation-debt and position-manager statements; scaled-token fee distribution, credit delegation, ERC-20 Approval and `Initialized` upgrade reports; GHO aToken/vToken dependency links; stkAAVE emission, delegation, rewards and admin accounting), dispatched by `decode_aave_report_log`.

Topic and payload recognition is this crate's whole job. Which emitter role a report belongs to, whether a market tracks that emitter, and which revision introduced it are `degenbot-aave` decisions (ADR-105 D12); this leaf has no dependency on it and carries no ownership-role policy. Recognition is revision-agnostic — a decoder keys on the signature's `topic0`, never on a block number or a revision range — so a later compatible revision carrying a known report decodes exactly as an earlier one did, while an unknown topic fails closed. The topic constants are pinned against their declared signatures by a test.

## Usage

```toml
degenbot-decoders = "0.6.0-alpha.5"
```

Or: `cargo add degenbot-decoders` (the pre-release version must be pinned explicitly, e.g. "0.6.0-alpha.5").

Part of [degenbot](https://github.com/BowTiedDevil/degenbot) — a Rust-first MEV bot for EVM chains. The in-repo root README and `docs/` cover the full architecture; this crate is published standalone so you can depend on exactly the pieces you need.
