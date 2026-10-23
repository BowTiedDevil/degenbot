# Balancer builder compliance — Phase 1 re-measurement and classification
Ergo NPUH7E (epic KXY2YU) · analysis worker · read-only · tree clean at start (HEAD 84d1b1c2e, `main`)

## TL;DR

1. **The Balancer pytest surface is green today**: the two named suites pass 79/79 in ~12 s
   (unpinned, forking the live head 26251856); the whole `tests/balancer/` directory passes
   109/109 under the fork marker and 159/159 offline.
2. **The earlier "14 errors" were not pool-build refusals.** The preserved lane log
   (`/tmp/fork_final2.log`) shows every one of the 14 errors is the *same* read-side failure:
   `getRate()` on **bb-s-USDC (`0xbFA413A2ff0F20456d57B643746133F54bFE0cd2`)** reverting with
   `panic: arithmetic underflow or overflow (0x11)`, raised in the fixtures at
   `test_stable_pool_class.py:224` / `test_stable_pools.py:776` — before any pool is registered.
   No `SpecViolationError`, no V2-reserve check, no Rust builder involved.
3. **Root cause is a poisoned anvil per-block RPC storage cache**, reproduced deterministically:
   `anvil --fork-url <reth> --fork-block-number 26200011` (storage caching on, the default)
   panics on that `getRate()`; the same fork with `--no-storage-caching` returns `1e18`; direct
   `eth_call` against reth returns `1e18` at every block probed (19 samples, 25.9M–26.25M, plus a
   dense 26 200 005–26 200 085 scan). Moving
   `~/.foundry/cache/rpc/mainnet/26200011/` aside makes the cache-enabled fork return `1e18`.
4. **There is no Balancer broken-pool taxonomy in the Rust core.** The known-broken set exists only
   in Python (`BROKEN_BALANCER_V2_POOLS`, one pool); the Rust weighted builder **builds that pool**
   (measured). Conversely, the "pool build refused: out of spec" class is *reproducible* for Balancer
   addresses through `Bot.build_pool`, but for a different reason (identity fallback to the generic
   Curve arm), not a V2 reserve check.
5. Recommended pin: **26204126** (the Curve sweep pin) — all nine Balancer builds and every
   rate-provider read verified good there.

---

## 1. Reproduction (item 1)

Commands run, foreground, logs under `/tmp`:

```
set -a && source tests.env && set +a
just py pytest tests/balancer/test_stable_pool_class.py tests/balancer/test_stable_pools.py \
    -q -m '(online_rpc or base) and not slow' --timeout=1400 -p no:cacheprovider -v
   -> 79 passed in 11.98s                     (log: /tmp/balancer_analysis_run1.log)

just py pytest tests/balancer -q -m '(online_rpc or base) and not slow' --timeout=1400 -p no:cacheprovider
   -> 109 passed, 159 deselected in 17.56s

just py pytest tests/balancer -q -p no:cacheprovider      # offline default marker set
   -> 159 passed, 109 deselected in 0.32s
```

Zero errors, zero failures, zero skips. Note the suites are **unpinned**
(`fork_mainnet_archive` default → `fork_block=None` → anvil forks the node's *current* head), so
the green result is a claim about head 26251856, not about a replayable block.

### What the earlier lane actually hit (exact census, from /tmp/fork_final2.log)

| # | test (× parametrization) | fixture site | failing call |
|---|---|---|---|
| 4 | `test_stable_pool_class.py::TestBalancerV2StablePoolComposableExact` — `given_in_usdc_to_usdt[100/1000]`, `given_out_usdt_for_usdc[100/1000]` | `_build_stable_pool` line 224 | `rp_c.functions.getRate()` on `0xbFA413A2…` |
| 10 | `test_stable_pools.py::TestComposableStablePoolSwaps` — `given_in_usdc_to_usdt`, `usdt_to_dai`, `usdc_to_dai`, `given_out_usdc_for_usdt`, `usdt_for_dai` (×100/1000) | `_build_composable_pool_data` line 776 | `rp_contract.functions.getRate()` on `0xbFA413A2…` |

Exactly 14 (`grep -c '^ERROR tests/balancer' /tmp/fork_final2.log` → 14) — matching the epic's
count. All 14 are the **bb-s-USD pool** legs; the TUSD-bsp legs in the same classes passed.

`0xbFA413A2ff0F20456d57B643746133F54bFE0cd2` = **bb-s-USDC** ("Balancer Sturdy Boosted Pool (USDC)"),
`getRateProviders()[1]` of `0x779d01F939D78a918A3de18cC236ee89221dfd4E` (bb-s-USD). The panic is
`StableMath`-style checked arithmetic inside the linear pool's `getRate()`; the *inputs* are what the
fork served wrong.

### Cache poison proof (cache on vs off, same block, same command)

```
anvil --fork-url <reth> --fork-block-number 26200011                 -> getRate() reverts, panic 0x11
anvil --fork-url <reth> --fork-block-number 26200011 --no-storage-caching -> 1000000064775661584 (1e18)
cast call 0xbFA4… 'getRate()(uint256)' --block 26200011 (direct reth)     -> 1000000064775661584
mv ~/.foundry/cache/rpc/mainnet/26200011 /tmp/quarantine_foundry_cache/   (cache dir dated Oct 22 11:38)
anvil --fork-url <reth> --fork-block-number 26200011                 -> 1000000064775661584  ✅
```

Anvil's fork storage cache is `~/.foundry/cache/rpc/<chain>/<block>/storage.json` — **per block, on
disk, and shared by every instance**. A cache written while the archive node could not answer
consistently for that block (the lane ran at 11:51, cache dir created 11:38 — the node's head was
exactly 26200011) poisons *every later fork at that block*, forever, while direct node calls are
correct. That is the whole "flaky Balancer composable errors" story: not the builder, not the
fixtures' logic, not a spec check.

---

## 2. Classification (item 2)

Failure class for the historical 14, as measured:

| test class | pool | address | refusal class | root cause |
|---|---|---|---|---|
| `TestBalancerV2StablePoolComposableExact` (4) | bb-s-USD (composable stable) | `0x779d01F939D78a918A3de18cC236ee89221dfd4E` | **none** — `ContractLogicError` from an `eth_call` read, not a build refusal | anvil per-block storage cache served state inconsistent with the block header → bb-s-USDC `getRate()` panicked (0x11) |
| `TestComposableStablePoolSwaps` (10) | bb-s-USD (composable stable) | same | **none** — same read | same |

No `SpecViolationError`, no `BuildFailure::Spec`, no `PoolBuilderError::Spec`, no V2-reserve narrow,
no BPT/premint handling, no missing code, no rate-provider *builder* defect. Both files' composable
fixtures read rate providers *Python-side through the fork* before touching Rust at all
(`tests/helpers/contract_compat.py:60` → `AlloyProvider.call` → `src/degenbot/provider/sync.py:199`).

### Where the "out of spec" message really comes from (and where it *can* still fire for Balancer)

`BuildFailure::Spec` has exactly one producer chain and it is V2/V3/V4 **registration**, plus
`PoolBuilderError::Spec` from the builder:

- `rust/crates/engine/degenbot-bot/src/bot_core/pool_builder/builder.rs:106` —
  `#[error("out-of-spec V2 reserve")] Spec` (one variant, reused by every spec site: V2 reserve
  `narrow_v2_reserve` at `:470,:548`, Curve broken-set/min-coins at `:1085,:1103,:1123`,
  balancer scaling-factor docs at `:694,:745`)
- `…/registration_ledger.rs:566` `PoolBuilderError::Spec → BuildFailure::Spec`
- `rust/crates/shells/degenbot-python/src/bot/errmap.rs:150` → `SpecViolationError("pool build refused: out of spec")`

The Balancer **builders themselves have no Spec arm at all** (`build_balancer_weighted` `:695`,
`build_balancer_stable` `:746`; errors are `Rpc`/`Decoding` only), and the Balancer **registration**
path has no `SpecViolation` either (`degenbot-pools/src/registration_refusal.rs` has no Balancer arm;
`balance_vector_orchestration.rs:327` `register_balancer_stable_pool` *panics* on duplicate/arity
instead of refusing).

Yet the message is reproducible for Balancer addresses — through identity resolution, not the builder:

```
Bot(database=":memory:").build_pool("0x53BC3cBa…")     # composable, at pinned block 26204126
   -> SpecViolationError: pool build refused: out of spec
Bot(database=":memory:").build_pool("0x32296969…")     # MetaStable
   -> SpecViolationError: pool build refused: out of spec
Bot(database=":memory:").build_pool("0x753BD6a5…")     # known-broken YUME/WETH
   -> SpecViolationError: pool build refused: out of spec
```

Mechanism: `src/degenbot/bot/_bot.py:583-600` — when `resolve_pool_type` raises (no DB descriptor for
the address, nothing enumerable to probe), `build_pool` falls back to the **generic Curve descriptor**
(`family=STABLESWAP, variant=None`) and routes through the Curve construction arm; `build_curve_pool`
then finds <2 coins on a Balancer pool and returns `PoolBuilderError::Spec`. Passing the identity
explicitly proves the builders are fine:

```
bot._py_bot.construct_pool(addr, family="stableswap", variant="balancer_stable",
                           block=26204126, db=False, vault=0xBA12…F2C8)
   -> (1, '', '', '0x53BC3cBa…', 'balancer_stable')      ✅
   -> (2, '', '', '0x32296969…', 'balancer_stable')      ✅
bot._py_bot.construct_pool(addr, family="weighted", variant="balancer_weighted", …)
   -> (3, '', '', '0x5c6Ee304…', 'balancer_weighted')    ✅
```

### Rust builder outcomes measured at block 26204126 (the sweep universe as it stands)

| pool | kind | Rust build | bpt_idx | invariant_version | note |
|---|---|---|---|---|---|
| `0x53BC3cBa…` tusd-bsp | composable stable | **OK** (pool_id 1) | 1 ✅ | 1 (V1) ✅ | amp 600000 |
| `0x779d01F9…` bb-s-USD | composable stable | **OK** | 0 ✅ | 1 (V1) ✅ | amp 2000000; rates `[1e18, 1.000000064775661584e18, 1e18, 0.002874251497005989e18]` |
| `0x32296969…` wstETH/WETH | MetaStable | **OK** | `None` ✅ | **1 (V1) — the suite requires V2** ⚠️ | pool-id specialization = `0x0002` (TwoToken) |
| `0x9c6d47Ff…` cbETH/wstETH | MetaStable | **OK** | `None` | (same heuristic) | |
| `0x8A34b5ad…` ankrETH/WETH | MetaStable | **OK** | `None` | (same heuristic) | |
| `0x1E19CF2D…` rETH/WETH | MetaStable | **OK** | `None` | (same heuristic) | |
| `0x5c6Ee304…`, `0x3e5fa951…`, `0xff083f57…` | weighted | **OK** ×3 | – | – | |
| `0x753BD6a5…` YUME/WETH 1/99 | weighted | **BUILT — no refusal** ❌ | – | – | `BROKEN_BALANCER_V2_POOLS` member; swaps disabled (BAL#327) |

Two genuine gaps fall out of that table:

**(A) The known-broken Balancer set has no Rust twin.** `src/degenbot/balancer/deployments.py:15`
`BROKEN_BALANCER_V2_POOLS = {0x753BD6a5…}` is consulted at
`src/degenbot/builders/balancer_builder.py:90` and `src/degenbot/bot/_bot.py:895` (both the *legacy*
Python builder path). `Bot.build_pool` no longer goes through that path — it goes
`_bot.py:515 → _py_bot.construct_pool → mod.rs:1000/1017 → build_balancer_weighted/stable`. There is
nothing in that chain that knows the set. Measured consequence: the swaps-disabled pool registers
happily and will only fail later, at swap simulation.

**(B) The invariant-version heuristic mis-derives MetaStablePools.** `builder.rs:759-772` maps
`specialization == 1 → 2 (V2)` else `1 (V1)`, mirroring Python's
`balancer_builder_base.py:131-157` (`resolve_invariant_version`, measured: spec 2 → 1, spec 1 → 2,
spec 0 → 1). wstETH/WETH's pool id carries `0x0002` (TwoToken), so both paths derive **V1**, while
`tests/balancer/test_stable_pools.py` documents and asserts that the deployed MetaStablePool needs
**V2** (roundUp `D_P`) for exact on-chain matching — the test suite works around the heuristic by
passing `invariant_version=INVARIANT_V2` explicitly. A sweep that lets the heuristic decide builds the
metapools with the wrong rounding rule (the ~0.01 % class of error the suite's docstring names).

---

## 3. Archive comparison (item 3)

`archive/main-20260721` (HEAD `b37c88d3`, **2026-06-17**) predates Balancer support entirely:

```
git ls-tree archive/main-20260721 src/degenbot/balancer/
  100644 … e69de29 src/degenbot/balancer/__init__.py      # empty blob
  040000 …         src/degenbot/balancer/libraries
  100644 … e69de29 src/degenbot/balancer/managers.py      # empty blob
  100644 … c5c9d56 src/degenbot/balancer/pools.py         # 194 lines, no BPT/rate-provider logic
  100644 … 6bb549e src/degenbot/balancer/types.py
git grep -l BrokenPool archive/main-20260721 -- src/  ->  curve/curve_stableswap_liquidity_pool.py,
                                                          exceptions/liquidity_pool.py   (Curve only)
```

So there is **nothing to port from the archive**: no Balancer known-broken set, no composable-stable
premint/BPT special handling, no rate-provider cache logic. The composable-stable BPT handling the
suites exercise (`bpt_idx` detection, dropping BPT from the invariant, `_cacheTokenRateIfNecessary`
emulation) was written *after* the archive snapshot and lives in the current tree:
`rust/…/builder.rs:762` (`bpt_idx = tokens.iter().position(|t| *t == address)`), the registration
arity asserts in `balance_vector_orchestration.rs:327`, and the Python companions
(`src/degenbot/balancer/stable_pools.py`, `tests/helpers/balancer_pool_factory.py`,
`CacheAwareRateProvider` in the suites).

The only real known-broken Balancer artifact anywhere is the current-tree
`BROKEN_BALANCER_V2_POOLS` (1 pool) — Python-only, as noted in gap (A).

---

## 4. Recommended implementation plan (item 4), dependency order

Each item: exact sites · red/green test · gate.

### P0 — Fork-cache hygiene (blocks all measurement; do first)
- **Why**: the metric being chased is poisoned by on-disk anvil state, and it is *persistent*:
  `~/.foundry/cache/rpc/mainnet/<block>/storage.json`, created by the run at 11:38 and still
  reproducing the panic 6 h later. Today's green run is green partly because the suites fork a fresh
  head (26251856) that has no cache file yet.
- **Sites**: `tests/conftest.py:487` (`_fork(storage_caching=True)`), `src/degenbot/fork/__init__.py`
  (`ForkLaunchConfig.storage_caching` → anvil `--no-storage-caching`).
- **Red/green**: parametrize the two suites at 26200011 (`indirect=True`); `storage_caching=True`
  reproduces the 14 `ContractLogicError`s, `storage_caching=False` is green. Then either flip the
  pinned sweeps to `storage_caching=False` (a pinned block's state is immutable; the cache only
  buys repeated-spawn latency and costs correctness), or add a startup cache-consistency probe.
- **Gate**: the two suites at both cache settings; `just py pytest tests/fork/ -q` for the wrapper.

### P1 — Pin the Balancer suites at 26204126
- **Sites**: `tests/balancer/test_stable_pool_class.py` (class-level
  `@pytest.mark.parametrize("fork_mainnet_archive", [_SWEEP_FORK_BLOCK], indirect=True)`) and
  `tests/balancer/test_stable_pools.py`; mirror the comment block at
  `tests/curve/test_curve_stableswap_pool.py:318-321`.
- **Red/green**: a green run at the pin *and* `grep`-ed absence of the head-dependency (a test that
  asserts `fork_mainnet_archive.get_block_number() == 26204126`).
- **Verified feasible**: every rate-provider read and all nine builds in §2 were measured at that
  block.
- **Gate**: the two suites.

### P2 — Port the Balancer broken set into the core (gap A)
- **Sites**: `rust/crates/engine/degenbot-bot/src/bot_core/pool_builder/builder.rs` — new
  `pub const BROKEN_BALANCER_V2_POOLS: &[Address]` next to `BROKEN_CURVE_V1_POOLS` (`:1073`), gates at
  the top of `build_balancer_weighted` (`:695`) and `build_balancer_stable` (`:746`) returning
  `PoolBuilderError::Spec` before any RPC; keep/retire the Python twins
  (`src/degenbot/balancer/deployments.py:15`, `balancer_builder.py:90`, `_bot.py:895`) per the
  no-back-compat rule (hard cutover: the Rust const becomes the one home, Python reads it via the
  facade or drops the gate with the legacy builder).
- **Red/green**: mirror `rust/…/pool_builder/tests.rs:1736`
  (`build_curve_pool_rejects_a_known_broken_v1_pool`) as
  `build_balancer_weighted_rejects_a_known_broken_pool` / `…_stable_…` with `FakeRpc::new()` (no
  responses needed — the gate must fire before any I/O). Python-side:
  `construct_pool(0x753BD6a5…, family="weighted", variant="balancer_weighted", …)` →
  `SpecViolationError` (today: silent build).
- **Gate**: `just test-rust-nextest -p degenbot-bot` + the balancer suites.

### P3 — Make `Bot.build_pool` resolve Balancer identity (or forbid the fallback)
- **Why**: measured — an address with no DB descriptor falls back to the generic Curve arm
  (`src/degenbot/bot/_bot.py:583-600`) and refuses *any* Balancer pool as "out of spec". This is the
  same message class the epic attributes to V2-reserve checks, and it is the mechanism that would
  make a `build_pool`-driven Balancer sweep look catastrophically broken.
- **Sites**: `_bot.py:583-600`; `src/degenbot/builders/type_resolution.py` (resolver);
  factory rows already shipped in `src/degenbot/registry/deployments.json` (13 Balancer factories,
  chain 1). Either probe the Balancer factories with `isPoolFromFactory(address)` (measured: that
  selector answers on all five factory revisions probed; `getPoolCount`/`getPools`/`getPoolAt` revert
  everywhere) or refuse with the typed `UnsupportedPoolFamilyError` instead of silently taking the
  Curve arm.
- **Red/green**: `Bot(database=":memory:").build_pool("0x779d01F9…", state_block=26204126)` returns a
  `BalancerV2StablePool` (today: `SpecViolationError`).
- **Gate**: the balancer suites + `just py pytest tests/builders -q`.

### P4 — Fix the MetaStablePool invariant-version derivation (gap B)
- **Sites**: `builder.rs:759-772` and Python twin `balancer_builder_base.py:131-157`.
- **Red test**: build wstETH/WETH through `construct_pool(... invariant_version=None)` at 26204126 and
  compare a `GIVEN_IN` swap against `BalancerQueries.querySwap`; today the V1 rounding produces the
  documented ~0.01 % divergence, and `balancer_invariant_version == 1`. Green when the probe
  classifies MetaStablePools by contract shape (e.g. presence of `getRateProviders()`/
  `getScalingFactors()` and `_calculateInvariant(roundUp=true)` bytecode marker) or when the V1/V2
  choice moves into the pool-id/type descriptor.
- **Gate**: `just test-rust-nextest -p degenbot-bot` + the two suites.

### P5 — Build the Balancer sweep (the epic's actual "Done when")
- **Constraint measured**: Balancer V2 has **no enumerable registry** — `getPoolCount()`,
  `getPools()`, `getPoolAt(0)` revert on Weighted rev1/rev4, Stable rev2, Composable rev4/rev6; only
  `isPoolFromFactory(address)` answers. So the sweep needs a **pinned pool corpus** (checked-in JSON,
  e.g. under `tests/balancer/data/`, generated once from the Balancer subgraph or a Vault
  `PoolRegistered(bytes32,address,uint256)` log scan, `topic0 =
  0x89e4ce247bb757a34cf1ec673df97f44c9b8fef96bce623e360c1edc1def9b1d`) or an explicit identity per
  row.
- **Sites**: new `tests/balancer/test_balancer_sweep.py`, modeled on
  `tests/curve/test_curve_stableswap_pool.py:326`/`:358` — `_SWEEP_FORK_BLOCK = 26204126`,
  `@pytest.mark.online_rpc`, `@pytest.mark.parametrize("fork_mainnet_archive", [_SWEEP_FORK_BLOCK],
  indirect=True)`, per-pool loop with
  `except (BrokenPool, NoLiquidity, SpecViolationError): continue` and the
  `AssertionError(f"{type(e).__name__}: {e} — pool {i}/{n} @ {address}, block {block}. Reproduce: …")`
  idiom. Build through `construct_pool(..., family=…, variant=…, vault=…)` (measured working) rather
  than `Bot.build_pool` until P3 lands.
- **Gate**: sweep green at 26204126 printing a census (`n built / n skipped-typed / n failed`); the
  census is the epic's evidence line ("every pool either builds or is refused as typed Spec with a
  named reason").

### P6 — Give `Spec` a named reason (epic acceptance)
- **Sites**: `builder.rs:106` (`Spec` → `Spec(SpecRefusal)` with a reason enum: V2Reserve,
  CurveBrokenPool, CurveTooFewCoins, BalancerBrokenPool, ScalingFactorRange…),
  `registration_ledger.rs:566`, `errmap.rs:150`, plus the `RefusalKind` detail plumbing.
- **Red/green**: a curve broken pool and a balancer broken pool each produce a **distinct** named
  reason string on the Python side (today both render "pool build refused: out of spec").
- **Gate**: `just test-rust-nextest -p degenbot-bot -p degenbot-pools` + `just py pytest tests/balancer tests/curve -q`.

### Which failures are legitimate skips vs real gaps
- **Legitimate typed refusals the sweep should SKIP** (`except SpecViolationError: continue`):
  the known-broken Balancer pool `0x753BD6a5…` (once P2 lands), plus whatever the census surfaces as
  <2-token / out-of-range-scale pools. The Curve pattern is the model — the *skip must be by type*,
  never by message.
- **Not a skip**: the 14 historical `ContractLogicError`s (a fork-side I/O defect; P0).
- **Real builder gaps to fix**: P2 (broken-set gate), P3 (identity resolution), P4 (invariant
  derivation). None of them is a V2-reserve check.

---

## 5. Pinned block recommendation (item 5)

**Yes, pin — at 26204126**, the same constant as the Curve sweeps
(`tests/curve/test_curve_stableswap_pool.py:321`). Rationale, all measured:

- All nine Balancer builds in the universe succeed there, including both composable-stable pools with
  correct `bpt_idx` (1 and 0) and correct scaling factors.
- Every rate-provider read the suites need returns cleanly there — bb-s-USDC `getRate()` =
  `1000000064775661584` at 26200011, 26204126, 26230000 and 25 further sampled blocks; nothing
  block-fragile was found in 25.9M–26.25M.
- One pin for both sweeps keeps the suite's replay story single-valued ("stage-complete local-node
  block, advanced deliberately").
- Determinism: today's green run forks head 26251856; a head that moves under the suites is not a
  gate. The failures being chased were exactly head-dependent.

**Combine the pin with P0.** Pinning alone does *not* immunise: a poisoned
`~/.foundry/cache/rpc/mainnet/26204126/storage.json` would reproduce the panic at the pin too. The pin
makes the replay deterministic; disabling the storage cache (or probing its health) makes it correct.
Recommendation: pinned sweeps run `storage_caching=False`.

---

## 6. Caveats and housekeeping

- **Read-only**: no tracked file was edited. Scratch probes live in
  `/tmp/probe_balancer_build*.py`, `/tmp/probe_construct.py`, `/tmp/probe_balancer_production.py`;
  the run log is `/tmp/balancer_analysis_run1.log`.
- **Local cache quarantine**: the poisoned directory
  `~/.foundry/cache/rpc/mainnet/26200011/` was **moved** (not deleted) to
  `/tmp/quarantine_foundry_cache/26200011` — untracked local anvil cache, re-fetched on demand. Restore
  it only to reproduce the historical failure.
- **Gates not run** (out of lane): `just test`, `just test-rust`, `just test-python`, `just test-fork`.
  Everything above is either a scoped pytest run or a direct `cast`/probe.
- **`Bot.build_pool` probe used `database=":memory:"`** — the identity fallback defect (P3) is measured
  in that configuration; a bot with a resolved descriptor may behave differently, which is exactly why
  P3 needs a red test before it is "fixed".
- **Ergo lifecycle**: NPUH7E left untouched (PM owns state); no commit, no push.
- **Foreign tree activity**: none observed — `git status --short` was empty at start and end; HEAD is
  `84d1b1c2e`.
