# Transition Guide: Sweeping Lingering Pure-Python Modules to the Three-Layer Architecture

> **Purpose.** ADR-005's stateful core (the `Bot`/`PyBot`/`PyLiquidityPool`/
> `PyErc20Token` family + the per-family companions) landed as slices 3–15 of
> the Polars three-layer migration (ergo `XQ5UX6`). That closed the
> **stateful** topology. The **stateless** half of the convention — pure-Rust
> math/decode/encode leaves wrapped by thin `#[pyfunction]`s and ultimately
> consumed by the companions — is only partially landed. Many Python modules
> still hold pure-math, decoding, and encoding logic that **already has a Rust
> counterpart** but is not yet routed through it, or that has no Rust port yet.
>
> This guide gives agents the guideposts to (1) evaluate a given module against
> the architecture, (2) move the remaining responsibility from Python to Rust,
> and (3) transition the tests. It is the rubric for the sibling ergo epic
> ("Three-layer architecture: sweep lingering pure-Python modules to Rust").

## 1. The three layers (recap)

Per `rust/AGENTS.md` "Three-Layer Pattern", specialized for shared state by
ADR-005, and realized across the Cargo workspace at `rust/crates/`:

| Layer | Where it lives | `pyo3`? | Holds |
|-------|----------------|---------|-------|
| **Rust core** | `rust/crates/degenbot-{core,-cl-math,-curve-math,-balancer-math,-abi,-decoders,-uniswap,-rpc,-bot}` | **none** (enforced by `just check-no-pyo3-in-cores`) | data + state-machine logic + pure math + protocols (DexIdentity, encoders, decoders) |
| **PyO3 wrapper** | `rust/crates/degenbot-python/src/<domain>/**` | all | `#[pyclass]`/`#[pyfunction]` only — arg extraction → GIL release → core call → result wrap. **No business logic.** |
| **Python companion** | `src/degenbot/**` | n/a | user-facing API, docstrings, I/O orchestration (SQLAlchemy, web3.py, publisher/subscriber, price oracle), immutable config dual-tracking, `Fraction`-based display |

The **standalone-Rust-core constraint** (ADR-005) is first-class: anything a
standalone Rust consumer (`examples/standalone_consumer.rs`, `cargo add
degenbot`) would need must live in a core crate from day one — never "move it
later," which strands it across the future crate boundary. `DexIdentity` is
the precedent: Rust-side at introduction, Python its first consumer.

Canonical references:
- `rust/AGENTS.md` — generic three-layer rule, the Nine Rules, GIL discipline, module-naming convention.
- `rust/CONTEXT.md` — glossary; {Polars-Inspired Three-Layer Architecture}, {PyBot}, {PyLiquidityPool}, {PyErc20Token}, {degenbot-decoders}, {degenbot-uniswap}.
- ADR-001 (I/O-free pools), ADR-003 (Bot as state owner), ADR-005 (FFI topology + crate-split target), ADR-006 (per-chain orchestrator), ADR-007 (unregister seam).
- `docs/architecture/rust-owned-bot.md` — component map + pump/engine lifecycle.

## 2. Evaluating a module: the triage rubric

Before touching a module, classify it. Run this rubric against every Python
file flagged by the sweep. The output of evaluation is **one of four
dispositions**, and the disposition decides the work:

### 2.1 The "where does this belong?" decision table

Adapted from `rust/AGENTS.md` "Porting Decision Framework":

| Criterion | Keep in Python (companion/`src/`) | Port to Rust (a core crate) |
|-----------|-----------------------------------|------------------------------|
| Hot path? | Called infrequently / at construction | Per-block, per-tx, in tight loops |
| GIL bottleneck? | Already releases GIL via I/O `await`/`.detach()` | CPU-bound, holds GIL during compute |
| Type shape? | Heavy Python-object manipulation (ORM, `WeakSet`, `Fraction`) | Pure numeric / byte / `Address` / `U256` |
| I/O shape? | ORM queries, WS subscriptions, price-oracle refresh lifecycle | Pure transform: logs→structs, int→int, bytes→calldata |
| Python-ecosystem coupling? | SQLAlchemy, web3.py, `publisher`/`WeakSet`, `Fraction` display | Standalone logic reusable without Python |
| Test shape? | Needs mocks/Anvil/RPC fixtures | Clear input→output, property-testable |

### 2.2 The four dispositions

1. **`done`** — the module's responsibility already lives in Rust and the
   Python side already delegates through the `Py*` wrapper (e.g. V2/V3/V4
   pool state and calc). **Action:** none. Record the disposition in the
   task body + close it. A fast, valid close; the sweep is a sweep, not a
   port-everything mandate.

2. **`partial`** — a Rust counterpart exists but the Python path is still the
   authority for some callsites (the common lingering case: a `degenbot-*-math`
   leaf exists but the companion's calc still calls the Python port).
   **Action:** finish the routing — make the companion call the Rust leaf via
   the `#[pyfunction]`/`#[pyclass]` seam, cross-check parity, delete the
   dead Python port.

3. **`port-now`** — no Rust counterpart yet, but the rubric says it belongs in
   Rust (pure math/decode/encode with no I/O or Python-ecosystem coupling).
   **Action:** land a new core leaf (or extend an existing one) under the
   three-layer discipline (§3), then route.

4. **`stays-python`** — the rubric says keep in Python (I/O, ORM, orchestration,
   price oracle, `Fraction` display). **Action:** none, but record *why* (which
   criterion), so the next sweep doesn't re-litigate.

### 2.3 The smell checklist

A module is likely `partial`/`port-now` if any of these hold:
- It is a 1:1 port of a Solidity/Vyper library and a `degenbot-*-math` leaf
  for that library already exists (`calculations/*`, `uniswap/v3_libraries/`,
  `uniswap/v4_libraries/`, `balancer/libraries/`, `aave/libraries/`,
  `curve/calculators/`).
- It hand-slices EVM log bytes and `degenbot-decoders` already decodes the
  same event (`uniswap/log_decoders.py`).
- It ABI-encodes/decodes and `degenbot-abi` already covers the signature
  (`abi_adapter.py`, `contract/decoding.py`).
- It holds mutable pool/token state that the companion now reads through a
  `Py*` handle, leaving it a dead mirror (`types/state_cache.py`,
  `uniswap/v{2,3,4}_pool_state.py`, `curve/stableswap_pool_state.py`,
  `aerodrome/v2_pool_state.py`, `uniswap/concentrated/*`).
- `rg` for the module's symbols shows zero live consumers outside the module
  and its own tests (dead code post-companion).

If `rg` shows the symbol is referenced only by parity/equivalence tests
against a Rust implementation, it is `partial` with the Python port retained
as the oracle — see §4.3 for the oracle-retirement protocol.

## 3. Moving responsibility from Python to Rust

The cutover discipline (from ADR-005 slice 11 "Curve family port", the
canonical three-sub-slice shape). Each port follows three ordered sub-steps;
**each leaves tests green** before the next starts (red-green per sub-step):

### 3.1 Sub-step A — pure-Rust core leaf (stateful: state port)

- New/extended code goes in the matching core crate (`rust/crates/degenbot-*-math`
  for pure math; `degenbot-decoders` for log decoders; `degenbot-uniswap` for
  DEX identity/encoding; `degenbot-abi` for encode/decode; `degenbot-bot` for
  state-coupled dispatch).
- **Zero `pyo3`** in the core file. Verify with `just check-no-pyo3-in-cores`
  (the gate walks `cargo tree` for pyo3 under default features for every core
  + the umbrella). If a `From<…> for PyErr` is needed, put it behind a
  non-default `pyo3` feature in the core (orphan-rule precedent in
  `degenbot-core::errors`).
- Idiomatic Rust, `Result<T, E>` returns, `#[cfg(test)]` unit tests —
  independently runnable without Python.
- Name the pure fn `*_rust()` or a plain `pub fn name()` in a `_py`-less file;
  never name a `#[pyfunction]` in a core crate.
- For a **math** port: a direct 1:1 port of the deployed contract arithmetic,
  cross-checked byte-for-byte against the Python oracle (see §4.2). Mirror the
  Solidity variable names where it aids direct-port correspondence
  (lint-allowed in `degenbot-balancer-math`/`-curve-math`).

### 3.2 Sub-step B — PyO3 wrapper (the `degenbot-python` binding layer)

- Lives in `rust/crates/degenbot-python/src/<domain>/**` (per-domain subdirs
  mirroring the cores: `abi/`, `cl_math/`, `rpc/`, `uniswap/`, `bot/`).
- `#[pyfunction]` for stateless ops (no `Py` prefix); `#[pyclass]` for
  stateful handles (keep the `Py` prefix unconditionally per ADR-005 — no
  `name=` override).
- **Thin translator only**: extract args → `py.detach()` for I/O/long CPU →
  call the core → wrap result. Extract owned data before `detach()`; the
  detached closure must not reference any `Bound<'_, PyAny>` (UB otherwise).
- Map errors at the boundary: Rust `Result` → Python exception via
  `From<…> for PyErr`. Cache Python refs with `PyOnceLock`
  (`conversion::cache`). Register every symbol in `c_api.rs`.
- Read/write guard discipline (ADR-005): stateful `#[pyclass]` wrappers
  classify each method read (`.read()`) or write (`.write()`).

### 3.3 Sub-step C — route the companion through Rust + delete the Python port

- The Python companion (`src/degenbot/**`) calls the Rust seam instead of the
  Python implementation. **Immutable config stays dual-tracked** (Rust carries
  it for the future math port; Python keeps it for its own calc/display) — this
  is the V3/V4/Curve/Balancer companion discipline.
- Update the *builder* to register via `PyBot::register_*` and hand the
  `PyLiquidityPool`/`PyErc20Token` handle to the companion ctor (mirror the
  `CurvePoolBuilder._register_pool` twin pattern + `make_*_pool` factories).
- Delete the now-dead Python implementation (no back-compat layer — per root
  `AGENTS.md`). Keep it only if it is the **parity oracle** for an
  as-yet-unrouted Rust calc (§4.3).

### 3.4 Cutover invariants (do not strand state)

- **One state owner.** Never leave a Python mirror of state that Rust now
  owns — that re-creates the retired `RustPoolCache`/`ArbPoolCacheAdapter`
  split (ADR-003 "delete, not migrate").
- **No dead Rust surface without a Python consumer**, and **no dead Python
  companion without a Rust backing** (except landed-but-inert standalone data
  like `DexIdentity`, whose first consumer is Rust unit tests).
- Each sub-step verified by: `cargo check`/`fmt --check`/`clippy --deny
  warnings`/`test` over the workspace; `just check-no-pyo3-in-cores`;
  `ruff check`/`ruff format --check`/`ty check` on `src/`; the affected
  companion's tests; import probe if the extension rebuilt.

## 4. Transitioning the tests

### 4.1 The test factory pattern (construction cutovers)

Every direct `Pool(...)`/`Token(...)` construction site in tests must route
through a `tests/helpers/<family>_factory.py::make_*` helper that builds via
`PyBot::register_*` → `get_pool`/`get_token` → companion — mirroring
`Bot.build_pool()` (ADR-005). Existing factories:
`v2_pool_factory.py`, `v3_pool_factory.py`, `v4_pool_factory.py`,
`curve_pool_factory.py`, `balancer_pool_factory.py`, `erc20_factory.py`,
`bot_factory.py`. Each call creates its own short-lived `PyBot` (the returned
handle holds an `Arc` clone, outliving the `PyBot`) so tests stay isolated.
A construction cutover = `sed`-swap the symbol + a `make_*` idempotent guard
(`get_*`-first because `Bot::register_*` asserts on duplicate).

### 4.2 The red-green parity cross-check

For a math/decode/encode port, **do not delete the Python port before the Rust
leaf is cross-checked against it**. The pattern (slices 5, 6, 11c):

1. **Red:** write the Rust leaf returning placeholder/`ZERO` values; the
   parity test (Rust result vs Python oracle over a fixture corpus = the
   *existing* Python port's outputs) fails.
2. **Green:** implement the Rust port as a direct 1:1 Solidity/Vyper
   transcription; the parity test passes byte-for-byte.
3. **Route + retire:** re-point the companion/the calc site at the Rust seam
   (§3.3); the Python port now has zero live consumers → delete it (and its
   parity tests, which become tautologies). Keep the Rust `#[cfg(test)]`
   corpus as the regression set.

Property-based (`proptest`) roundtrip encode→decode is the standard invariant
for ABI work; integer-exact agreement with the on-chain contract is the
standard for swap math (the `test_py_bot.py` parity tests are the canonical
reference and validate the *fee/retained-fraction convention*, which has bitten
ports before — see the slice-5 gamma-numer bug).

### 4.3 The oracle-retirement protocol

If a Rust calc exists but is not yet wired into the companion's calc path, the
Python port is the **parity oracle** and stays until the routing lands. When
routing lands, the oracle's parity tests are deleted *with* it. Do not leave an
"equivalence" test harness comparing two live implementations indefinitely —
the slice-12 `PiecewiseMobiusSolver` and the retired `_legacy/` package are the
precedent for deleting equivalence tests alongside the retired implementation.

### 4.4 Fork / Anvil tests

Fork-gated tests (marked with the anvil fixtures) validate calc parity against
on-chain `getAmountOut`/`get_dy`/slot0 — the ground truth a unit corpus can't
reach. A port must keep these green; if a fork test's premise was tied to a
deleted Python class, rewrite it under the companion+`dex.variant` model (see
`docs/migration-guides/dex-subclass-collapse.md` "Fork tests pending a
follow-up rewrite"). Four fork files were module-skipped there — each port task
should re-enable or close them.

### 4.5 Delegation-detection tests

When a companion **delegates** calc to Rust (slice 5), assert "Rust was hit
with the right args" rather than only "result matches" (parity tests already
cover the math). The `_DelegateSpy` pattern wraps a `PyLiquidityPool` to record
`calculate_tokens_out/in` calls via `__getattr__` pass-through. Use this for
any routing cutover where the math already has a parity test — it proves the
delegation seam, not just the numbers.

## 5. Validation gates (run after every sub-step)

```
just test-rust            # cargo test --workspace (Rust unit + integration)
just test-rust-python     # pytest tests/rust (PyO3-wrapped Python tests)
just test-python          # full pytest suite (compile-test-contracts first)
just lint-rust            # clippy --fix --all-targets --deny warnings
just check-no-pyo3-in-cores  # cores + umbrella pyo3-free under default features
just lint-python          # ruff + ty
just format               # cargo fmt + ruff format (apply); --check variants for CI
just lint-markdown        # if docs touched
just lint-context-maps    # if CONTEXT.md touched
just lint-commits         # commit-range commitlint
```

The extension is auto-rebuilt on import by maturin — **do not** manually rebuild
it, recreate the venv, or reinstall the package after Rust changes; any
`uv run`/`cargo test` triggers a rebuild if needed.

## 6. What already landed (don't redo)

ADR-005 slices 3–15 (ergo `XQ5UX6`, all done) closed the **stateful** topology:
- Slice 3 — `Erc20Token` companion over `PyErc20Token`.
- Slices 4–5 — V2 companion (state + calc delegation) over `PyLiquidityPool`.
- Slice 6 — `DexIdentity` + DEX presets in `degenbot-uniswap` (Rust core).
- Slice 7 — V2 DEX subclass collapse (Sushi/Pancake/Swapbased/Camelot hollow
  subclasses deleted; `UniswapV2Pool` + `dex.variant`).
- Slices 8–9 — V3/V4 companions over `PyLiquidityPool`.
- Slice 10 — `UniswapEngine` lock unification onto shared `Arc<RwLock<Bot>>`.
- Slices 11–12 — Curve + Balancer family ports (state + companion + pure-math
  leaves `degenbot-curve-math`/`degenbot-balancer-math`).
- Slice 13 — crate split (`degenbot-core`/`-python`/umbrella `degenbot` +
  `examples/standalone_consumer.rs`).
- Slice 14 — `PyBotIo` stateful I/O struct (sync RPC choreography ported;
  the Python `SyncPoolIO` stays as parity gate).
- Slice 15 — pickle multiprocessing retired + Rust-side parallel solve fan-out.

Blocked: ADR-003 `Bot`=state + engine=solving split; ADR-006 one `Bot` per
chain; ADR-007 unregister seam.

The remaining work is the **stateless leaves not yet wired** (math/decode/
encode) and **dead Python mirrors** of now-Rust-owned state — that is the
sweep this guide serves.

### Fork C — Price readers → Rust `degenbot-price` (ergo `Y2MI3F` + `3O2ZPN`)

The on-chain **price-reader mechanism** moved to a new pyo3-free core leaf
`degenbot-price` (task `Y2MI3F`): `ChainlinkPriceFeed` (ports
`ChainlinkPriceContract` — `decimals()` / `latest_round_data()` /
decimal-corrected `price()` over `latestRoundData` + `decimals` eth_calls) and
`AavePriceOracle` (ports `OraclePriceFetcher` — `get_asset_price(address)` /
tolerant `fetch_prices` batch over `getAssetPrice`). Both route `eth_call`
through `degenbot_rpc::Contract::call_typed` (the RPC primitive is already
Rust-owned). The §4.2 parity is pinned byte-exact against canonical ABI-decoded
return bytes.

Task `3O2ZPN` added the **PyO3 seam** (`PyChainlinkPriceFeed` /
`PyAavePriceOracle` in `degenbot-python/src/price/`) and cut the Python
readers over to delegating shells: `ChainlinkPriceContract` (and
`chainlink/__init__.py`) delegates `decimals` / `latest_round_data` to the
Rust reader and computes the float `price` = `float(answer) / 10**decimals` in
the display layer (preserving the prior float-exact behavior including the
fractional part — the Rust `price()` truncates to whole units); the inline
`provider.call_raw` / `abi_decode` bodies are deleted. `OraclePriceFetcher.fetch`
delegates to `PyAavePriceOracle` (the inline `raw_call` /
`encode_function_calldata` loop deleted); the tolerant per-asset
skip-on-error behavior now lives in the Rust core (matching the prior
`ContractLogicError` / `ValueError` catch). `Erc20Token._price_oracle`
(type `ChainlinkPriceContract`) is unchanged as a public surface — the shell
kept its full API, so callers (`Erc20Token.price`, `PositionAnalysisService`)
are unchanged.

The web3→alloy provider seam: `ProviderAdapter.to_alloy_provider()` resolves
the held `AlloyProvider` directly for alloy-backed adapters, or builds (and
caches) one from the underlying web3 IPC path / HTTP endpoint — so the Rust
readers can `eth_call` against a web3-backed `Bot` (the AnvilFork test
fixtures use `ProviderAdapter.from_web3(fork.w3)` over IPC; the seam bridges
this without forcing the whole bot onto alloy). `okAve` oracle-address
resolution from the DB (`OKKMG5`, Epic `AZGJUN`) is **not** a hard dependency —
the seam consumes a resolved `Address` passed in by the caller.

Validation: `tests/test_price_seam_parity.py` (8 tests) drives the full pyclass
→ Rust core → `eth_call` → ABI decode → shell path through a local in-process
JSON-RPC mock returning canned ABI-encoded bytes — value-exact §4.2 parity
including the non-whole fractional case, with no live RPC; the existing
`test_chainlink_price_feed.py` + `tests/erc20/test_erc20_token.py` pass against
the real AnvilFork (web3-backed bot).

### Fork D — EIP-1559 signing + fee finalization → Rust `degenbot-submission` (ergo `G6DNW4`)

The **transaction-submission signing mechanism** moved to a new pyo3-free core
leaf `degenbot-submission` (task `G6DNW4`): `signer::TxSigner` holds the
operator private key ONCE via `alloy-signer-local::PrivateKeySigner` (a
`LocalSigner<k256 SigningKey>`), constructed from Python-provided key
bytes/hex — the key never round-trips back into Python per-tx (the prior
`eth_account.Account.sign_transaction(..., private_key=...)` path crossed the
key on every submit — a security smell).

`fee::finalize_fees` sets `maxFeePerGas = int(1.5 * base_fee_next) +
priority_fee` + `maxPriorityFeePerGas = priority_fee` — the Python oracle's
inline fee computation (`examples/eth_backrun_v2_v3_v4_rust.py` L2623–L2624),
including the **float→int truncate boundary** documented on the `fee` module
(the same convention as `_compute_priority_fee`). `signer::TxSigner::sign_eip1559`
builds a type-2 `TxEip1559` `TxEnvelope` from `params::TxParams` and signs
synchronously via the `TxSignerSync` trait (RFC 6979 deterministic ECDSA —
`eth_account`-parity), returning the raw `Typed2718` bytes ready for
`eth_sendRawTransaction`. `chain_id` lives on the signer (replay-protection
context), not in `TxParams` — no duplicated-state smell.

**PyO3 seam** (`degenbot-python/src/submission/`): `PyTxSigner` (constructed
from key hex/bytes + chain id; the held `PrivateKeySigner` lives in Rust) +
`PyTxParams` (holds the EIP-1559 field set; the web3-shape access list is
parsed in the wrapper) + the `finalize_fees` pyfunction. `sign_eip1559`
releases the GIL around the synchronous ECDSA via `py.detach` (CPU-bound, no
network — unlike the price readers' async `eth_call`, signing needs no runtime
+ no `block_on`).

**Cross-epic boundary (REFERENCE, no hard edge):** the `priority_fee` is
CONSUMED from `YL2MTH`'s `_compute_priority_fee` (Simulation, not started);
submission does NOT sequence-depend on it — it reads the already-computed fee
off `tx_params`, and for §4.2 parity uses a fixture value. `base_fee_next`
comes from `JTLWA3`'s `next_base_fee` (committed crate, consumed). The
broadcast (`eth_sendRawTransaction` `bytes → TxHash`) is `ZUZANP` (carved out
— `degenbot-rpc` owns only the byte-broadcast; this task owns the SIGNING
producing those bytes).

**§4.2 HARD gate:** because both `eth_account` and `alloy-signer-local` use
RFC 6979 deterministic ECDSA over secp256k1, a pinned key + `tx_params`
produces **byte-for-byte identical** raw signed bytes in Rust and Python.
`tests/test_submission_seam_parity.py` (9 tests) pins the anvil-key-0 fixture
(byte-exact vs `eth_account`), the round-trip signer-recovery, chain-id
replay-protection (different chain → different bytes, same recovered
address), fee-math parity vs Python `int(1.5 * bf) + pf`, and an
`eth_account` version-drift guard; the Rust core `signed_bytes_match_eth_account_oracle_byte_for_byte`
test pins the same hash. The Python cutover (replacing the example's
`eth_account.sign_transaction` call) is a sibling downstream task.

### Fork A — JSON deployment identity → Rust builder → handle (ergo `AWGOXL`)

The per-(chain,factory) CREATE2 deployer + init_hash moved from Python
`ClassVar`s + the pool-type registry into the Rust builder.

- **Single source.** `registry/deployments.json` ships every factory row;
  `degenbot-uniswap::deployments` embeds it via `include_str!` and exposes a
  `(chain_id, factory)`-keyed `lookup`. The Python registry loads the same JSON
  for `pool_type` → companion-class resolution.
- **Store-on-identity (P62DKO/NSAZ4X).** `register_v2/v3_pool` resolves the
  effective deployer (with `deployer=null -> factory`, covering PancakeSwap
  V3's **separate deployer**) + init_hash and stores the verified pair on
  `V2PoolIdentity`/`V3PoolIdentity`. The `PyLiquidityPool` handle exposes them
  via `.deployer` / `.init_hash`; the V2 `dex` getter merges them into the
  protocol-const `DexIdentity` preset (factory/deployer/init_hash off the
  identity; fees/ABI shape off the variant preset). Non-JSON pools fall back to
  a Rust `const` (`UNISWAP_V2/V3_MAINNET_INIT_HASH`) — the retired Python
  `ClassVar`s' documented default.
- **Verification at registration (JC6OFG).** The builder recomputes the CREATE2
  address and rejects mismatches — a Rust-only `Bot` verifies pool addresses
  with no Python (D4-friendly). Ad-hoc/non-JSON registration is skipped.
- **ClassVars + `_verified_address` retired.** The Python companions read the
  verified identity off the handle; no Python mirror remains.

### Fork B — Solidly solve → Rust engine (ergo `RXWRJU`)

The Solidly stable invariant (`x³y + xy³ ≥ k`) solve — Aerodrome stable /
volatile pools and Camelot `stable_swap` pools — moved from the Python
`SolidlyStableSolver` (Möbius compose + Newton outer over the existing
`degenbot-solidly-math` integer leaf) into the Rust `UniswapArbEngine`.

- **Two-tier solver (`DMPSNG`).** `solve_solidly_path_int` runs a Möbius
  precheck on a V2-equivalent approximation of the Solidly curve to bracket
  the optimum, then a 25-iteration golden-section search over the integer
  leaf (`calc_exact_in_stable_solidly` / `calc_exact_in_stable_camelot`, via
  `get_y_solidly` / `get_y_camelot` — EVM-exact against on-chain Solidity),
  with an integer verification confirming the profit. Returns
  `(optimal_input, profit)` as `U256`.
- **Auto-derivation (`BFIWUG` + `2OWLDL`).** `HopType::SolidlyStable` is
  auto-derived from the `BotState` pool identity at `register_path` time
  (`derive_hop_type` reads the `AerodromeV2` variant's `stable` flag, or the
  `V2` variant's `stable_swap` for Camelot) — the PyO3 `register_path` seam
  takes only `(pool_id, zero_for_one)`; no Solidly tag is threaded through FFI.
- **Python plumbing (`WCT5KR`).** `SolidlyHopInfo` is the engine-facing hop
  descriptor (informational — `PathInfo.path_type` emits `Solidly`);
  `EngineRegistry.register_aerodrome_pool` + the Aerodrome branch of
  `register_path` route the shared-core pool_id through, mirroring V2.
- **Disposition.** `SolidlyStableSolver` moves from `stays-python` →
  **parity oracle only** (`FEMZJC` cross-validates the engine against it and
  `BrentSolver` at `1e-6` / `1e-4` relative — SolidlyStableSolver is the
  integer-exact same-algorithm tight oracle; BrentSolver is the looser f64-outer
  oracle). The Python solver package's `SolidlyStable` row in
  `src/degenbot/arbitrage/CONTEXT.md` records the disposition.
- **Bugfix on the path.** `snapshot_aerodrome` was returning `(u64, u64, u64)`
  and panicked on overflow for reserves ≥18 tokens of 18-dec; matched
  `snapshot_v2`'s U256 return shape so the manual pool-walk parity oracle can
  read live Aerodrome state.
- **§4.3 oracle retirement (task `3VGIDY`).** Now that the `FEMZJC` parity
  gate was green, the Python `SolidlyStableSolver` parity oracle was retired
  per the §4.3 delete-with-its-tests protocol: `solidly_stable.py`, its
  `solvers/__init__.py` re-export, `test_engine_vs_solidly_parity.py`, and the
  dedicated `test_solidly_stable_solver.py` suite were deleted together. The
  generic mixed-path simulators (`_simulate_mixed_path` /
  `_simulate_mixed_path_int`, plus the Solidly float fallback
  `_solidly_swap_output_float`) moved to `_solver_utils.py` because two kept
  Curve tests (`test_fake_curve_pool.py`, `test_curve_legacy_equivalence.py`)
  reuse them as generic sim helpers — they are NOT Solidly-specific.
  `SolidlyStableHop` (the hop type, constructed in `aerodrome/pools.py` +
  `v2_liquidity_pool.py` with a `swap_fn`) is untouched live production code.
  The engine's `solve_solidly_path_int` is the sole Solidly solve path; the
  disposition line "parity oracle only" above is the historical record.
  Rust `solver_dispatch.rs` retains the "Faithful port of
  `arbitrage.solvers.solidly_stable.SolidlyStableSolver`" comment as
  historical port provenance (the Python source it now points to is removed).

## 7. Anti-patterns to avoid

- **Keeping a Python mirror of Rust-owned state** "for the tests." Rewrite the
  tests against the `Py*` handle instead (ADR-003 "delete, not migrate").
- **Adding `pyo3` to a core crate file** outside a feature gate. It's a code
  smell and fails `just check-no-pyo3-in-cores`.
- **Business logic in the binding layer.** The wrapper extracts, calls, wraps.
  If logic crept in, move it to the core.
- **Dropping the `Py` prefix** or adding a `#[pyclass(name = "…")]` override.
  ADR-005 keeps the prefix unconditionally.
- **Leaving an equivalence/parity test harness** comparing two live
  implementations after the routing cutover — it becomes a tautology and hides
  regressions; retire it with the oracle (§4.3).
- **Landing standalone data on the Python side first** "to move later." The
  standalone constraint means it lands in a core crate from day one
  (`DexIdentity` precedent).
- **Manual extension rebuild / venv recreate.** Maturin rebuilds on import.