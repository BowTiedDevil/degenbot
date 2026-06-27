# Golden on-chain parity — convert RPC-driven parity tests to recorded golden files

Container scope: convert on-chain parity tests (local calc == deployed-contract
result) to record/replay against golden files so CI runs with **no RPC and no
secrets**. Builds on the design at
`docs/architecture/golden-onchain-parity.md` and the L2 `GoldenOracle` scaffold.

Non-goals: changing pool *calculation* behaviour; recording full RPC cassettes
for arbitrary chain-of-calls (the L1 general recorder — tracked as a follow-up,
out of scope here); touching the live discovery tests
(`test_*_registry_pools`, `test_factory_stableswap_pools`, `test_first_200_pools*`).

Constraints:
- I/O-free pool construction is preferred (ADR-005) — build pools from constants
  + recorded tick data, not from an L1 cassette, where feasible.
- Every converted test is marked `@pytest.mark.onchain_oracle`; the marker set is
  the inventory + progress dashboard.
- Record mode needs a working fork (`tests.env` RPC or local node). Replay is
  fully offline.

Key decisions (resolved during planning):
- **Camelot tracer bullet is split** into revive (T2) + convert (T3): the test
  module is currently module-skipped pending a rewrite off the deleted
  `CamelotLiquidityPool` subclass, so it cannot be 1-line-converted.
- **Hypothesis parity tests** (`test_cached_calculations` V3/V4): replace the
  `@hypothesis.given` search with a curated fixed amount list recorded into L2
  (the CI gate), and keep the hypothesis variant `slow`-marked for periodic
  deep runs. Rationale: hypothesis amounts are non-deterministic and cannot be
  replayed against recorded oracle values.
- **`online_rpc` marker** (T8) is deferred until a few conversions prove the
  pattern — do not disable currently-passing CI tests prematurely.

---

# T1 — Land golden scaffold + design doc
## Goal
- Commit the already-written L2 `GoldenOracle` scaffold, design doc, conftest
  options, marker registration, and justfile recipes so the rest of the plan
  can depend on them.

## Context
- Written but uncommitted: `tests/golden/oracle.py`, `tests/golden/test_oracle.py`,
  `tests/golden/__init__.py`, `docs/architecture/golden-onchain-parity.md`,
  conftest `--golden-mode`/`--golden-root` + `golden_factory` fixture,
  `pyproject.toml` `onchain_oracle` marker, `justfile`
  `test-offline-parity` + `record-golden`.
- Builds on `OfflineProvider` (`src/degenbot/provider/offline_provider.py`),
  the existing L1 replay mechanism for pool-state cassettes.

## Acceptance Criteria
- `tests/golden/test_oracle.py` passes offline (no RPC): `uv run pytest tests/golden/test_oracle.py -q --no-header -p no:randomly`.
- `uv run pytest --markers` lists `onchain_oracle`.
- `uv run pytest --help` documents `--golden-mode` (default `replay`).
- `just --list` shows `test-offline-parity` and `record-golden`.

## Validation Gates
- `uv run pytest tests/golden/test_oracle.py -q --no-header -p no:randomly`
- `just lint` (ruff/format on the new files)

---

# T2 — Revive Camelot V2 parity test under new pool model (live fork)
## Goal
- `test_create_camelot_v2_pool` runs again (no module skip), building the
  WETH/USDC Camelot pool via `Bot.build_pool` → `LiquidityPool`
  (`dex.variant == "camelot-v2-volatile"`) and asserting
  `contract.getAmountOut(...) == lp.calculate_tokens_out_from_tokens_in(...)`
  against a **pinned** Arbitrum fork. Still RPC — this unblocks the golden
  conversion in T3.

## Context
- `tests/uniswap/v2/test_uniswap_v2_liquidity_pool.py` is module-level
  `pytest.skip(..., allow_module_level=True)` because it imports the deleted
  `CamelotLiquidityPool` subclass (see
  `docs/migration-guides/dex-subclass-collapse.md`).
- The new model registers `LiquidityPool` for the Camelot factory
  (`src/degenbot/camelot/__init__.py`); `Bot.build_pool` returns a
  `LiquidityPool` with `variant="camelot"`.
- Pool: `CAMELOT_WETH_USDC_LP_ADDRESS` (Arbitrum 0x84652b…3CE27).
- T3 cannot convert a test that doesn't run, hence this is a hard prerequisite.

## Acceptance Criteria
- The Camelot test runs (module skip removed or test moved to a non-skipped
  module) and passes against a pinned-block `fork_arbitrum_full`/archive fork.
- No import of the deleted `CamelotLiquidityPool`.
- The fork block is pinned (indirect param) and recorded in the test, so T3
  re-records at the same block.
- `getAmountOut` oracle call + the exact `==` assertion remain unchanged (T3
  routes the oracle through `golden.check`).

## Validation Gates
- `uv run pytest tests/uniswap/v2/...::test_create_camelot_v2_pool -q --no-header` (live, needs Arbitrum RPC/anvil in `tests.env`)
- `just lint`

---

# T3 — Convert Camelot V2 test to golden (tracer bullet)
## Goal
- `test_create_camelot_v2_pool` asserts against a recorded golden file, runs
  offline in replay mode, and is marked `@pytest.mark.onchain_oracle`. This is
  the first end-to-end proof of the L2 record→replay→revert contract.

## Context
- Depends on T2 (the test must run first) and T1 (the scaffold).
- Single oracle call (Camelot `getAmountOut`), single exact-equality assert —
  the smallest possible proof of the harness.

## Acceptance Criteria
- Oracle routed through `golden.check(key, contract=lambda: w3_contract.functions.getAmountOut(...).call())`.
- `golden.check` factory bound to the same pinned Arbitrum block as T2.
- `tests/golden/data/uniswap/v2/.../test_create_camelot_v2_pool.json` committed
  with the recorded `getAmountOut` return value.
- Replay (`--golden-mode=replay`, default) passes with **no RPC**: the Camelot
  pool must be built I/O-free (constants) OR, if an L1 cassette is needed,
  recorded under `tests/fixtures/chain_data/42161/` — note the chosen path in
  the completion note.
- `@pytest.mark.onchain_oracle` applied (so `just test-offline-parity` includes it).
- `just record-golden -- <camelot nodeid>` reproduces the golden file against
  a live fork.

## Validation Gates
- `just test-offline-parity` (replay, offline) — the converted test passes with no RPC
- `just record-golden -- tests/uniswap/v2/...::test_create_camelot_v2_pool` (record, live) reproduces the JSON
- `just lint`

---

# T4 — Convert V3 test_cached_calculations to golden
## Goal
- V3 `test_cached_calculations` asserts quoter `quoteExactInputSingle` /
  `quoteExactOutputSingle` against a golden file; runs offline in CI.

## Context
- Currently `@hypothesis.given(integers(1, MAX_INT256))` — non-deterministic,
  cannot be replayed against recorded oracle ints.
- Decision: replace the hypothesis search with a curated fixed amount list
  (shrunk failing examples from a real hypothesis run + representative spans
  across the [1, MAX_INT256] range) recorded into L2; keep the hypothesis
  variant as a separate `slow`+`ethereum` test for periodic deep runs.
- Pool: WBTC/WETH V3 (`fork_mainnet_full` today — must pin a block for record/replay parity).

## Acceptance Criteria
- Fixed amount list committed (module constant) replacing the `@hypothesis.given`.
- Both directions (token0→token1, token1→token0) recorded for exact-input and
  exact-output quoter calls; reverts recorded as `{"reverted": true}` entries
  preserving the test's `try/except ContractLogicError: continue` skip path.
- Fork block pinned + matches the golden file header; pool built I/O-free
  (reuse the `offline_wbtc_weth_v3_pool` fixture pattern + recorded tick data)
  or L1 cassette under `tests/fixtures/chain_data/1/`.
- `@pytest.mark.onchain_oracle`; hypothesis variant kept, marked `slow`+`ethereum`.

## Validation Gates
- `just test-offline-parity`
- `just record-golden -- tests/uniswap/v3/test_uniswap_v3_liquidity_pool.py::test_cached_calculations`
- `just lint`

---

# T5 — Convert V4 test_cached_calculations to golden
## Goal
- V4 `test_cached_calculations` asserts the V4 quoter
  (`quoteExactInputSingle`/`quoteExactOutputSingle` with `poolKey` tuple)
  against a golden file; runs offline in CI.

## Context
- Same hypothesis→fixed-list decision as T4 (T4 establishes the pattern; reuse
  the curated amount list approach).
- Pool: ETH/USDC V4 (`eth_usdc_v4` fixture on `fork_mainnet_full` — pin a block).
- V4 quoter call is a tuple-encoded `...call()` — L2 captures the decoded int
  result, so the call shape is irrelevant to the golden file.

## Acceptance Criteria
- Both directions recorded; reverts preserved (the test already
  `try/except ContractLogicError: continue`).
- Fork block pinned + matches golden header; pool built I/O-free or via L1
  cassette.
- `@pytest.mark.onchain_oracle`; hypothesis variant kept, marked `slow`+`ethereum`.

## Validation Gates
- `just test-offline-parity`
- `just record-golden -- tests/uniswap/v4/test_uniswap_v4_liquidity_pool.py::test_cached_calculations`
- `just lint`

---

# T6 — Convert Curve exact-parity tests to golden
## Goal
- Curve tests that assert `calc == contract.get_dy/get_dy_underlying/
  calc_withdraw_one_coin/calc_token_amount` read oracle truth from golden
  files; run offline in CI.

## Context
- Targets (in `tests/curve/test_curve_stableswap_pool.py`): `test_tripool`,
  `test_base_pool`, `test_single_pool`, `test_tricrypto_pool`,
  `test_metapool_with_valid_base_cache`,
  `test_metapool_over_multiple_blocks_to_verify_cache_behavior`.
- `_test_calculations` iterates token permutations × amount multipliers — each
  `(pool, token_in, token_out, amount)` is one L2 key; reverts (broken/unsupported
  pools) are recorded as `{"reverted": true}` so `_test_calculations`'s
  `except …: continue` reproduces.
- The multi-block metapool test records across several pinned blocks — each
  block gets its own L2 file (or keyed by block within entries; pick one and
  note it).
- DO NOT convert `test_base_registry_pools` / `test_factory_stableswap_pools`
  (live discovery — exclusion rule in the design doc).

## Acceptance Criteria
- Each converted test pinned to its block(s); pool built I/O-free or L1 cassette.
- All `get_dy`/`get_dy_underlying`/`calc_withdraw_one_coin`/`calc_token_amount`
  oracle results recorded; reverts preserved.
- `@pytest.mark.onchain_oracle` on each.
- Multi-block test handled deterministically (recorded blocks pinned in file header).

## Validation Gates
- `just test-offline-parity`
- `just record-golden -- tests/curve/test_curve_stableswap_pool.py::<each>`
- `just lint`

---

# T7 — Convert Aerodrome / Pancake / V2-router parity tests to golden
## Goal
- Remaining exact-parity tests (Aerodrome V2/V3 quoter, Pancake V2 router,
  V2 `test_calculate_tokens_out_from_ratio_out` if exact) read from golden files.

## Context
- `tests/aerodrome/test_aerodrome_pools.py::test_calculation_volatile`,
  `test_calculation_stable`, `test_aerodrome_v3_pool_calculation` (Base fork).
- `tests/pancakeswap/test_pools.py::test_pancakeswap_calculations` (Base fork).
- `tests/uniswap/v2/test_uniswap_v2_liquidity_pool.py::test_calculate_tokens_out_from_ratio_out`
  uses `pytest.approx` (NOT exact) — likely excluded; confirm and note.
- V2/Aerodrome amm `getAmountOut`/`getAmountOut`-style calls.

## Acceptance Criteria
- Each exact-equality parity test converted; `pytest.approx` tests left as-is
  (not golden candidates) with a note explaining why.
- All on Base fork → pin blocks; build pools I/O-free or L1 cassettes under
  `tests/fixtures/chain_data/8453/`.
- `@pytest.mark.onchain_oracle` on each converted test.

## Validation Gates
- `just test-offline-parity`
- `just record-golden -- <each nodeid>`
- `just lint`

---

# T8 — (decision) Add `online_rpc` marker to default `-m` exclusion
## Goal
- Decide whether unconverted live-RPC tests are **skipped** (not failed) in CI
  by adding `online_rpc` to the default addopts `-m "not slow and not base and not online_rpc"`.

## Context
- Today the addopts run `-m "not slow and not base"`: live-RPC parity tests
  that aren't `base`/`slow`-marked run (and fail) in CI when RPC/secrets are
  absent. Adding `online_rpc` would make CI green now but would silently hide
  unconverted tests.
- Recommendation: defer until T3–T7 land a meaningful set of conversions, then
  add `online_rpc` to the default exclusion + retro-mark remaining live tests.
  Premature exclusion risks hiding regressions in still-live tests.

## Checkpoint
- Produce: a list (`pytest --markers | rg onchain_oracle` count vs. total
  parity candidates) showing how many conversions are done vs. remaining.
- Then ask: "Add `online_rpc` to the default `-m` exclusion now, or wait until
  all identified parity candidates are converted?"
- Do not proceed past this point without user approval.

## Validation Gates
- `uv run pytest -m onchain_oracle --collect-only -q` returns the converted inventory
- `uv run pytest --co -q` total unaffected otherwise