# Y2MI3F — degenbot-price core crate (Chainlink + Aave readers)

## Outcome
Landed a new pyo3-free core leaf `rust/crates/degenbot-price/` owning the
on-chain **price-reader mechanism**, with §4.2 value-exact parity evidence.

## Deliverables
- **`rust/crates/degenbot-price/`** — `Cargo.toml` (deps: `degenbot-core`,
  `degenbot-rpc`, `degenbot-abi`, `alloy`, `log`, `thiserror`; no pyo3),
  `src/lib.rs`, `src/error.rs`, `src/decode.rs`, `src/chainlink.rs`,
  `src/aave.rs`.
- **`src/chainlink.rs`** — `ChainlinkPriceFeed { contract, chain_id }`
  (ports `ChainlinkPriceContract`):
  `decimals() -> PriceResult<u8>`, `latest_round_data() -> PriceResult<RoundData>`
  (typed tuple: `round_id`/`answer`/`started_at`/`updated_at`/`answered_in_round`),
  `price() -> PriceResult<U256>` (decimal-corrected whole-units; raw `int256`
  answer exposed via `RoundData.answer`, the integer analogue of Python's
  `float(answer)/10**decimals`, negative→0 clamp).
- **`src/aave.rs`** — `AavePriceOracle { contract }` (ports `OraclePriceFetcher`):
  `get_asset_price(asset, block) -> PriceResult<U256>` +
  `fetch_prices(assets, block) -> HashMap<Address,U256>` with tolerant per-asset
  skip+`log::warn!`+continue (matches the Python
  `ContractLogicError`/`ValueError` intent; sequential, matching the Python loop).
- **Routing:** both readers call `degenbot_rpc::contract::Contract::call_typed`
  (the typed sibling of `Contract::call` — same `eth_call`, exact `AbiValue`
  values with no string re-parse). No RPC re-implementation.
- **Pure helpers** `as_uint`/`as_int`/`as_u128` + `decimal_corrected_price` +
  `build_round_data` isolated in `decode.rs`/`chainlink.rs` so the §4.2 gate
  drives them from recorded EVM return bytes with no live RPC.

## §4.2 parity evidence (8 tests, all green)
- `latest_round_data_decodes_byte_exact` — encode the
  `(uint80,int256,uint256,uint256,uint80)` tuple via the canonical ABI encoder
  → `decode_for_types` → `build_round_data`; assert each field value-exact.
- `decimals_decodes_byte_exact` — `uint8` decode path.
- `build_round_data_rejects_wrong_arity` — 4-slot input surfaces `Decode` error.
- `decimal_correction_matches_python_integer_division` — 8/6/18-decimal
  feeds, zero, negative→0 clamp, fractional truncation (= `int(answer //
  10**decimals)`).
- `get_asset_price_uint256_decodes_byte_exact` + `get_asset_price_rejects_wrong_arity`
  — Aave `uint256` decode + arity guard.
- Two pinned-feed constant sanity tests (Chainlink ETH/USD `0x5f4e…`, Aave V3
  oracle `0x5458…`).

The sandbox has no live RPC, so the pinned return bytes are canonical ABI
encodings of representative values (built via the same encoder the chain
produces). Decode parity vs. the Python `abi_decode` holds because both
implement canonical ABI decode of the identical bytes.

## Wiring
- `rust/Cargo.toml`: workspace member + `[profile.release]` codegen override.
- `rust/crates/degenbot/` (umbrella): Cargo dep + `pub use degenbot_price;` — a
  standalone `cargo add degenbot` consumer reaches both readers, pyo3-free.
- `justfile`: `degenbot-price` added to the `check-no-pyo3-in-cores` list.
- `rust/CONTEXT.md`: `**degenbot-price**` glossary row (crate + deps + §4.2
  parity + sibling-cutover/`OKKMG5` boundary).

## Validation gates (all green)
- `cargo test -p degenbot-price` — 8 passed, 0 failed.
- `cargo clippy -p degenbot-price --all-targets` — clean (pedantic).
- `cargo fmt -p degenbot-price --check` — clean.
- `just check-no-pyo3-in-cores` — OK (umbrella + cores pyo3-free).
- Umbrella `-p degenbot` build — green, exposes the re-export.

`just test-rust` / `just lint-rust` were run scoped to `-p degenbot-price` (the
full workspace run was skipped because concurrent agents have in-flight Rust
WIP unrelated to this task; this crate touches zero of their source).

## Boundary (non-goals, per AC)
- PyO3 seam + Python cutover of `ChainlinkPriceContract`/`OraclePriceFetcher`/
  `Erc20Token.price` — sibling task.
- Aave valuation consumer orchestration — stays-python (sibling task).
- Oracle-address DB resolution — task `OKKMG5`; this crate consumes a resolved
  `Address`.
- `eth_call` primitive — already Rust-owned (`degenbot-rpc::Contract`).