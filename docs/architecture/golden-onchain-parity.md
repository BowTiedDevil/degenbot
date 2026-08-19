# Golden on-chain parity tests

## Problem

A large family of tests assert that a local pool-calculation function returns a
value **exactly equal** to the value returned by the deployed contract on-chain
(Uniswap V2/V3/V4 quoter, Curve `get_dy`/`get_dy_underlying`, Camelot / Aerodrome
`getAmountOut`, etc.). Today these tests drive a live RPC (via an Anvil fork at an
unpinned tip, or an external archive node URI from `tests.env`), which is:

- **slow** — fork spin-up + per-amount `eth_call`s round-trip the RPC;
- **flaky** — external public RPCs rate-limit / drop connections;
- **unsafe for CI/CD** — `tests.env` carries API keys that must not leak; and
- **non-deterministic** — many tests fork at *tip* (`fork_mainnet_full` with no
  `fork_block`), so the "golden" value changes every run.

We want these tests to assert **the same exact-equality contract** but read the
on-chain truth from a recorded file, so CI runs with **zero RPC** and **zero
secrets**.

## Existing assets we build on

1. **`OfflineProvider`** (`src/degenbot/provider/offline_provider.py`) — already
   serves pre-recorded `eth_call` responses from per-block JSON files under
   `tests/fixtures/chain_data/`. Replay-only; no recorder exists yet.
2. **I/O-free pool tests** (`tests/uniswap/v2/test_v2_pool_io_free.py`,
   `test_v3_pool_io_free.py`, `test_v4_pool_io_free.py`,
   `tests/curve/test_curve_io_free_example.py`) — already build pools from
   hardcoded constants + recorded tick data, no RPC. These are the
   **architectural template** (and align with ADR-005: pools are I/O-free;
   builders fetch state, pools receive values).
3. **`tests/uniswap/v3/test_v3_offline.py`** — the proven offline-replay wiring
   (`OfflineProvider.from_json_file` → `raw_call` → I/O-free pool).

The gap: the offline tests *dropped* the exact quoter parity assertion
(replaced with loose ranges), and the *recorder* that would populate a cassette
was never generalised — the existing `chain_data` files were hand-produced.

## Two layers, one goal

On-chain parity tests need **two** kinds of off-chain truth:

| Layer | What it records | Mechanism | Status |
|-------|-----------------|-----------|--------|
| **L1 — pool state** | slot0 / reserves / balances / tick bitmap+data / `get_code` needed to *construct* the pool I/O-free | `OfflineProvider` cassette (`tests/fixtures/chain_data/<chain>/block_<N>.json`) | exists (replay); **needs a recorder** |
| **L2 — oracle truth** | the quoter / `get_dy` / `getAmountOut` result for a given `(pool, tokenIn, tokenOut, amount)` | **`GoldenOracle`** — human-readable JSON of ints (`tests/golden/data/…`) | **new (this doc)** |

L1 and L2 compose. A converted test builds the pool I/O-free (L1 or hardcoded
constants) **and** asserts calc == `golden.check(…)` (L2). Neither touches a live
RPC in CI.

### Why a separate L2 instead of folding the quoter into the L1 cassette?

The quoter call in existing tests goes through a test-local contract interface —
its own provider connection, **not** the cassette-bound provider the pool under
test uses. A provider-level cassette would not capture it without re-routing
every quoter call through the same provider instance. L2 intercepts the oracle
call **at the test boundary**, so:

- the oracle truth is a small file of plain ints (trivially reviewable in a PR);
- it is decoupled from quoter-ABI / calldata churn;
- it works uniformly across V2/V3/V4/Curve/Aerodrome regardless of how the
  test currently invokes the on-chain contract.

## Identification method

A test is an **on-chain parity candidate** iff it satisfies **all three**:

1. It builds a pool and calls a **local calculation method**
   (`calculate_tokens_out_from_tokens_in`, `calculate_tokens_in_from_tokens_out`,
   `calc_withdraw_one_coin`, `calc_token_amount`, `simulate_*`, …).
2. It calls the **deployed contract** — quoter / `get_dy` / `getAmountOut` /
   raw `eth_call` to `quoteExactInputSingle`, etc. — and binds the result to a
   variable named like `contract_amount(_out|_in)`, `quoter_amount_*`.
3. It asserts `local == contract` (exact equality, not a range / `pytest.approx`).

### Discovery (heuristic)

```
rg -n --type py \
  'quoter|quoteExact|get_dy|getAmountOut|\.functions\..*\.call\(\)' tests \
  | rg -v '__pycache__'
```

then cherry-pick lines co-located with `fork_` fixtures and an `assert .* ==
.*amount` whose RHS came from step 2.

### Durable indexing (the real index)

Apply a registered pytest marker to every converted test:

```python
@pytest.mark.onchain_oracle
def test_cached_calculations(golden_factory, ...): ...
```

`onchain_oracle` is registered in `[tool.pytest.ini_options].markers` (see
`pyproject.toml`). The marker set **is** the inventory; `pytest --markers |
rg onchain_oracle` or `pytest -m onchain_oracle --collect-only -q` lists every
converted test. The grep above is only the *initial* triage.

### Exclusion rule — when NOT to convert

Do **not** convert a test if its purpose is **discovery**, not regression — i.e.
it iterates a **live registry** to surface *new* or *broken* pools, where the
set of pools is unknown ahead of time and the test's value is catching the
unexpected. Converting would freeze the pool set and silently stop discovering
new failures.

In this repo:

- `tests/curve/test_curve_stableswap_pool.py::test_factory_stableswap_pools`
- `tests/curve/test_curve_stableswap_pool.py::test_base_registry_pools`
- `tests/uniswap/v3/…::test_first_200_pools` / `test_first_200_pools_with_snapshot`
  (unless you also freeze the pool list + block — see "partial" below)
- `tests/uniswap/v4/…::test_first_200_pools` / `…_with_snapshot`

These tests catch arbitrary exceptions and re-raise them as `AssertionError`
with a "Reproduce with `test_single_pool @ <addr>`" hint — that is a discovery
contract, not a fixed-truth assertion. They must stay on a live fork (mark them
`slow` + the chain marker and exclude from CI via `-m`).

**Partial conversion** (optional, for the `first_200_*` tests): if the pool list
comes from a committed JSON (`testing_pools()` fixture), you *may* pin a block +
convert each pool's quoter results to L2 — but keep the live-registry variant
(`test_*_registry_pools`) as the discovery gate that runs out-of-band.

## The L2 scaffold — `tests/golden/oracle.py`

`GoldenOracle` is the record/replay seam for oracle truth.

**File layout** (one JSON per test function, derived from `nodeid`):

```
tests/golden/data/<module path>/<TestName>.json
```

e.g. `tests/golden/data/uniswap/v3/test_uniswap_v3_liquidity_pool/test_cached_calculations.json`.

**Format** (human-readable, sorted keys):

```json
{
  "chain_id": 1,
  "block_number": 17600000,
  "recorded_at": "2026-06-27T12:00:00Z",
  "entries": {
    "0xbEbc…|quoteExactInputSingle|token0→token1|100000000": { "value": 15808930695950518795 },
    "0xbEbc…|quoteExactOutputSingle|…": { "reverted": true, "exception": "ContractLogicError" }
  }
}
```

Reverts are recorded as entries too, so the replay reproduces the test's
`try/except ContractLogicError: continue` (skip) path faithfully — a revert at
record time stays a skip at replay time.

### API

```python
def test_cached_calculations(golden_factory, offline_wbtc_weth_v3_pool):
    pool = offline_wbtc_weth_v3_pool          # built I/O-free (L1/constants)
    golden = golden_factory(chain_id=1, block_number=17_600_000)  # L2

    for amount in GOLDEN_AMOUNTS:             # fixed list, NOT hypothesis
        for token_in, token_out in [(pool.token0, pool.token1), (pool.token1, pool.token0)]:
            key = f"{pool.address}|quoteExactInputSingle|{token_in.address}→{token_out.address}|{amount}"
            oracle = golden.check(
                key,
                contract=lambda: quoter.functions.quoteExactInputSingle(
                    token_in.address, token_out.address, pool.fee, amount, <limit>,
                ).call(),
            )
            if oracle.reverted:          # reproduce the record-time skip
                continue
            amount_out = pool.calculate_tokens_out_from_tokens_in(token_in, amount)
            assert amount_out == oracle.value
```

- **Record mode** (`--golden-mode=record`): `golden.check` invokes `contract()`
  against the live fork, captures the value **or** the exception, writes/updates
  the JSON, and returns a `GoldenResult`. The test's own `assert` still runs, so
  a record run is also a live parity run — it fails loudly if calc ≠ oracle at
  record time. Write incrementally so a partial run still persists what it saw.
- **Replay mode** (default, CI): `golden.check` **never** calls `contract()`;
  it returns the recorded `GoldenResult`. The `contract=` callable is required
  purely as the source of truth for the *next* record run (and as living
  documentation of which on-chain call the int came from). The `assert` runs
  against the recorded int. No RPC. No secrets.

### Drift & missing-entry semantics

- Missing key in replay → `GoldenError("no golden entry for <key>; re-record")`
  (fails the test, tells you what to run).
- Mismatch in replay → the test's own `assert … == oracle.value` fails with a
  clear diff. The scaffold does not add a second assertion.
- Updating a stale golden value intentionally → re-run with `--golden-mode=record`
  against a fork pinned to the **same block** recorded in the file header.

## Conversion recipe (per test)

1. **Pin the block.** Replace `fork_mainnet_full` (tip) with a pinned-fork
   fixture: `@pytest.mark.parametrize("fork_mainnet_archive", [N], indirect=True)`.
   Use the same `N` for record + replay (the L2 file header records it; the L2
   scaffold asserts the replay block matches the recorded block).
2. **Make pool construction RPC-free.** Preferred (ADR-005-aligned): build the
   pool I/O-free from constants + recorded tick data (copy the
   `offline_wbtc_weth_v3_pool` fixture pattern; record the tick data into the
   existing `tests/fixtures/chain_data/` cassette). Fallback: wrap the fork in an
   L1 recorder → `OfflineProvider` cassette.
3. **Freeze the inputs.** Replace `@hypothesis.given` amounts with a fixed,
   parametrized list committed alongside the golden file. Hypothesis-generated
   amounts are non-deterministic and cannot be replayed against recorded oracles.
4. **Route the oracle call through `golden.check(key, contract=…)`.** Keep the
   test's `assert local == oracle.value` and its `try/except … continue` revert
   handling intact (L2 preserves the revert as a recorded entry).
5. **Mark it.** `@pytest.mark.onchain_oracle`.
6. **Record once.** `just record-golden -- <test nodeid>` against an anvil fork.
   Commit `tests/golden/data/…/*.json` (+ the L1 cassette if you added one).
7. **Replay.** `just test-python` — the converted test runs offline in CI.

### Hypothesis tests (`test_cached_calculations` V3/V4)

These are the trickiest. They use `@hypothesis.given(integers(1, MAX_INT256))` to
explore *many* amounts against the quoter. Strategy:

- Keep hypothesis **as a separate, online-only** property test (mark `slow +
  ethereum`), OR
- **Replace** it with a curated fixed list of amounts drawn from a real
  hypothesis run (the shrunk failing examples + representative spans) and convert
  that list to L2. The fixed list becomes the regression gate that runs in CI.
  The hypothesis variant remains the deep-exploration gate that runs on-demand.

Recommendation: replace with a fixed list + L2 for CI; keep the hypothesis
variant `slow`-marked for periodic deep runs. Do **not** try to record an
unbounded hypothesis search into L2.

## Central-plumbing changes

- `pyproject.toml` — register `onchain_oracle` marker (and, as a separate
  rollout decision, an `online_rpc` marker added to the default `-m` exclusion
  list so unconverted live-RPC tests are skipped in CI instead of failing).
- `tests/conftest.py` — `--golden-mode`/`--golden-root` options + the
  `golden_factory` fixture.
- `justfile` — `record-golden` (runs pytest with `--golden-mode=record` + the
  fork fixtures, requires a working RPC/anvil) and `test-online-parity`
  (runs only `-m onchain_oracle` in replay, fully offline).

## Rollout order (smallest blast radius first)

1. Land scaffold: `tests/golden/oracle.py` + conftest options + marker + justfile
   + a self-contained deterministic scaffold test (`tests/golden/test_oracle.py`).
2. **Tracer bullet (single pool, single assert):** convert
   `test_create_camelot_v2_pool` (Camelot `getAmountOut`, one assertion) — proves
   record + replay + revert handling end to end.
3. **Fixed-block, fixed-amounts:** convert V3/V4 `test_cached_calculations`
   (replace hypothesis with fixed amounts) + the `test_calculate_tokens_*_with_override`
   family.
4. **Multi-direction Curve:** convert `test_tripool`, `test_base_pool`,
   `test_single_pool`, `test_tricrypto_pool`, `test_metapool_*` (L2 for `get_dy`
   / `get_dy_underlying` / `calc_withdraw_one_coin` / `calc_token_amount`).
5. **Aerodrome / Pancake / V2 router:** `test_calculation_volatile/stable`,
   `test_pancakeswap_calculations`, `test_calculate_tokens_out_from_ratio_out`.
6. **Leave as live discovery:** the `test_*_registry_pools` /
   `test_factory_stableswap_pools` registry sweeps (exclusion rule above).

Each step is one commit; the marker set is the progress dashboard.

## Non-goals

- Recording full RPC cassettes for arbitrary chains of `eth_call`s (L1 recorder)
  is intentionally deferred — it is its own concern and only needed where a pool
  cannot yet be built I/O-free. Track it as a follow-up (likely an `ergo` task).
- We do not change the **calculation** behaviour of pools; only the test oracle.
- We do not remove the live-fork fixtures — record mode still needs them.