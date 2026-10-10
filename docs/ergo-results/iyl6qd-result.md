# IYL6QD (PC5b, epic GZPPS3): the update-path collapse + manifest consumers + Curve arm + core-side parity probes

STATUS: **GREEN on items 1-4** (items 1 + 4 by the prior worker; items 2 + 3 in the *Continuation round* below). **Item 5 (optional Curve `NoLiquidity` relocation) DEFERRED** — see the continuation section.

*(The items-1/4 section below is the prior worker's, unedited. The continuation round appended at the end carries items 2 + 3, the gate exits, the probe/flip records, and the item-5 decision.)*

The update path (`_update_pool` / `Bot.update` / `_builder_for_pool`) collapsed onto ONE core
refresh entry, and the manifest consumers collapsed onto the `degenbot-db::species`
projection. The Curve arm in `construct_pool`, the core-side parity-guard probes, and the
optional Curve `NoLiquidity` relocation are NOT landed (see *PENDING*).

Baseline cited: `docs/ergo-results/hqylek-result.md` run 2 (green). Landed pre-existing chunks
not re-opened: `937153a29`, `4a03f5e04`, `231334a8f`, `ad035c48a`, `345a8c118`, `5de576784`.

## Shapes changed (diff)

| file | intent |
|---|---|
| `rust/crates/engine/degenbot-bot/src/bot_core/refresh.rs` (NEW) | The update path's ONE core entry: `RefreshOutcome` (`Changed`/`Unchanged`), `RefreshError` (unknown pool / unsupported family / missing identity / spec-bound), and `async fn refresh_pool(state, pool_id, io, block)`. It dispatches on the registered family tag (`pool_ops::pool_family`) to seven per-family arms — V2 + AerodromeV2 (`getReserves()` → `apply_sync_by_pool_id`), V3 (`slot0()`+`liquidity()` → `apply_v3_swap_by_pool_id`), V4 (`getSlot0`/`getLiquidity` on the manager's state view → `apply_v4_swap_by_pool_id`), Curve (`balances(uint256)` → `apply_balance_update_by_pool_id`), Balancer weighted / stable (`getPoolTokens` on the Vault → `apply_balance_update_by_pool_id`). The fetch half composes the existing core choreography over `ConstructionIo`; the apply half drives the state owner's typed dispatchers, so a new family is a new arm here — never a new Python branch. |
| `rust/crates/engine/degenbot-bot/src/bot_core/mod.rs` | `pub mod refresh;` — the module's core door. |
| `rust/crates/foundation/degenbot-substrate/src/cl_orchestration.rs` | NEW `BotState::v4_state_view_for_pool(pool_id)`: the pool-keyed reader of the manager's registered V4 `StateView` (the live-scalar read target a refresh needs), and the removal of the now-fulfilled `#[expect(dead_code)]` on `ClOrchestration::state_view_for` (a Rust reader exists now). |
| `rust/crates/shells/degenbot-python/src/bot/pool.rs` | NEW `PyLiquidityPool.refresh_via(io, block_number=None)` pymethod — the FFI face of the core entry. It borrows the driver's `BotIo`, resolves its `ConstructionIo`, and drives `refresh_pool` under `py.detach`; it maps the typed refusals to the historical Python surfaces. |
| `rust/crates/shells/degenbot-python/src/bot/py_bot_io.rs` | `required_construction_io` widened `fn` → `pub(crate) fn` so the pool handle can reach the attached `ConstructionIo`. |
| `src/degenbot/bot/_bot.py` | The 87-line module-level `_update_pool` dispatcher is DELETED. `Bot.update()` collapses to `pool.refresh_via(self._io, block_number=...)`, and `Bot._builder_for_pool` (the `type(pool)` / MRO builder walk) is DELETED with it. The now-unused `Uniswap{V2,V3,V4}PoolExternalUpdate` imports are gone. |
| `src/degenbot/types/abstract/liquidity_pool.py` | NEW `AbstractLiquidityPool.refresh_via(io, *, block_number=None)` — the driver's one call onto the handle's core refresh; `_py_pool: Pool` annotated on the base. |
| `src/degenbot/builders/curve_pool_builder.py`, `src/degenbot/builders/balancer_builder.py`, `src/degenbot/builders/protocol.py` | The orphaned per-family Python `update()` statics (Curve, Balancer) and the `PoolBuilder.update` protocol member are DELETED — their fetch/compare/apply choreography is the core entry's now, and leaving a second copy in Python is exactly the duplication the collapse retires. Their now-unused imports are gone. |
| `src/degenbot/runner/build_paths.py` | `_POOL_VERSION_MAP` DELETED; the permutation filter now reads `degenbot.pathfinding.FAMILY_TAG_TO_POOL_KIND`, the tag → typed `PoolKind` map minted from the core's exported tag list. |
| `src/degenbot/_ffi/__init__.pyi` | regenerated stub (one def; see the self-check). |
| `tests/builders/test_pool_refresh_via.py` (NEW) / `tests/builders/test_builder_update_staticmethod.py` (DELETED) | The V2 refresh behavior tests now drive the real core entry through a real `BotIo` over a recorded `OfflineProvider` (recorded `getReserves()`), plus negative guards that the retired builder `update()` statics no longer exist. |

Behaviour is preserved by construction: the core refresh fetches the SAME calldata the retired
dispatchers fetched and applies through the SAME typed state dispatchers the companions'
`external_update` reached (`apply_sync_by_pool_id` / `apply_swap_by_pool_id` /
`apply_balance_update_by_pool_id`). The collapse removes Python's family branch, not any core work.

### One deliberate divergence (recorded, not hidden)

The historical Python companions' `external_update` RAISE `ExternalUpdateError` when an update's
block is older than the pool's own clock. The refresh entry instead returns
`RefreshOutcome::Unchanged` for that condition: the core apply dispatchers stamp the block they are
handed, so replaying an older observation would walk a pool's clock backwards (and a V3/V4
backward stamp outside a reorg panics). No default gate exercises a stale-block `Bot.update`, and
the companions' `external_update` raise contracts are untouched and still pinned by
`tests/uniswap/v2/test_uniswap_v2_liquidity_pool.py`, `tests/uniswap/v3/test_uniswap_v3_liquidity_pool.py`,
and `tests/aerodrome/test_aerodrome_pools.py`.

## Manifest / projection design (one-liner)

`degenbot-db::species` (`species.toml`, ADR-059 D3) stays the single source of fork /
manager-deployment identity; the Python permutation filter now consumes the core's exported family
taxonomy (`pool_family_tags` → `FAMILY_TAG_TO_POOL_KIND`, the tag → `PoolKind` map that the
manifest's family roster is parity-pinned against by
`degenbot-db::species::manifest_covers_exactly_the_graph_vocabulary`) instead of re-declaring
`_POOL_VERSION_MAP` by hand. `v2_v3_subclass_table` / `V3_FAMILIES` / `V3_VARIANT_TABLES` were
already manifest projections (`degenbot-db::schema::table::v2_v3_subclass_table` projects
`species::subclass_table_for_kind`) or already retired; no per-language species constant remains.

## Stub diff self-check (`just gen-stubs`)

`git diff src/degenbot/_ffi/__init__.pyi` adds exactly ONE def and removes none:

```
+    def refresh_via(self, /, io: BotIo, block_number: int | None = None) -> bool:
```

`just gen-stubs --check` → exit 0 (committed stubs match generator output; re-run after the
`PyRef` → `&Bound` signature change and the doc-markdown fix).

## Negative-probe records (parity guards)

- **Identity guard (V2/V3) — red path:** `tests/test_bot.py::TestBuildDelegatedIdentityReturnSurface::test_build_delegated_v2_parity_mismatch_raises` — exit 0 (re-run directly; also green inside the full-suite runs).
- **V4 identity guard — red path:** `tests/test_bot.py::TestBuildManagedPoolIdentityReturnSurface::test_build_v4_parity_mismatch_raises` — exit 0 (re-run directly; also green inside the full-suite runs).

Both guards are UNCHANGED by this chunk and their protection is intact. **Their relocation to
core-side fault-injection probes (task item 3) is NOT done — see PENDING.** No guard's protection
was weakened at any point.

## Scoped gate exits (captured DIRECTLY, `cmd; echo $?`, no pipes)

| gate | exit |
|---|---|
| `rustfmt --check --edition 2021 --config skip_children=true -- <5 touched .rs>` | **0** |
| `uv run --no-sync ruff format --check -- <7 touched .py>` | **0** |
| `uv run --no-sync ruff check -- <7 touched .py>` | **0** |
| `just lint-rust-check` (clippy; incl. the architecture subset) | **0** |
| `just gen-stubs --check` | **0** |
| `just rebuild-if-stale` (build receipt + extension) | **0** |
| `just test-rust-nextest` (FULL workspace) | **0** — 4721 passed / 29 skipped |
| `uv run --no-sync pytest tests/test_ffi_boundary.py` | **0** — 8 passed |
| `uv run --no-sync pytest tests/test_ffi_boundary.py tests/golden tests/pool_companion tests/builders tests/curve tests/uniswap tests/arbitrage` | **0** — 1010 passed / 5 skipped |
| `uv run --no-sync pytest tests/builders/test_pool_refresh_via.py` | **0** — 5 passed |
| `uv run --no-sync pytest` (FULL Python suite) | **0** — 3130 passed / 50 skipped |

Logs: `/tmp/iyl6qd-clippy.log`, `/tmp/iyl6qd-genstubs.log`, `/tmp/iyl6qd-rebuild.log`,
`/tmp/iyl6qd-nextest.log`, `/tmp/iyl6qd-pytest-scoped.log`, `/tmp/iyl6qd-pytest-full.log`,
`/tmp/iyl6qd-rustfmt.log`.

The full-suite Python count is 3130 vs the baseline's 3131: the deleted
`tests/builders/test_builder_update_staticmethod.py` carried 6 tests and its replacement
`tests/builders/test_pool_refresh_via.py` carries 5 (the three ``_update_pool`` behavior tests and
the two ``@staticmethod`` conformance tests became three `refresh_via` behavior tests plus two
"the retired statics are gone" guards). No test was lost silently.

## Counts delta (baseline table shape, re-observed @ HEAD)

| # | shrink target | baseline | observed | delta | attribution |
|---|---|---|---|---|---|
| C1 | `_update_pool|_builder_for_pool` lines under `src/degenbot` | 6 | **0** | **−6** | **THIS chunk** |
| C2 | `_POOL_VERSION_MAP|v2_v3_subclass_table|V3_VARIANT_TABLES|V3_FAMILIES` lines under `src/` | 5 | **0** | **−5** | **THIS chunk** |
| C3 | `issubclass` sites in `src/degenbot/bot/_bot.py` | 9 | 0 | −9 | FMPNVD `5de576784` (landed) |

Verification one-liners (read-only, foreground):

```bash
git grep -c '_update_pool\|_builder_for_pool' HEAD -- src/degenbot      # -> 6 lines (all _bot.py)
rg -n '_update_pool|_builder_for_pool' src/degenbot                     # -> EMPTY (exit 1)
git grep -c '_POOL_VERSION_MAP\|v2_v3_subclass_table\|V3_VARIANT_TABLES\|V3_FAMILIES' HEAD -- src/   # -> 5 lines (all _POOL_VERSION_MAP)
rg -n '_POOL_VERSION_MAP|v2_v3_subclass_table|V3_VARIANT_TABLES|V3_FAMILIES' src/                    # -> EMPTY (exit 1)
rg -c 'issubclass' src/degenbot/bot/_bot.py                             # -> EMPTY (exit 1)
```

## Acceptance

- `rg "_update_pool|_builder_for_pool" src/degenbot` — **EMPTY**. ✅
- `rg "_POOL_VERSION_MAP|v2_v3_subclass_table|V3_VARIANT_TABLES|V3_FAMILIES" src/` — **EMPTY**. ✅
- One construction entry serves every family including Curve. ❌ **PENDING** (Curve still routes through its Python discovery builder; the `construct_pool` Curve arm is not landed).
- Both parity guards' red paths demonstrated core-side (flips recorded). ❌ **PENDING** (both red paths are demonstrated, but still Python-side — the core-side relocation is not landed).

## PENDING (manager re-runs / next session)

1. **The Curve arm in `construct_pool`** (item 2). Curve exposes no `factory()`, so its identity is
   resolved by discovery, not by the resolver; and its construction needs the metapool base-pool
   recursion plus the LP/underlying ERC20 companion builds before `from_handle` resolves. The arm is
   therefore an entry + the recursion moving core-side together — the same residue PC5's own
   predecessor listed (FMPNVD result doc, PENDING #3). No dead arm was added in the meantime.
2. **Core-side parity probes** (item 3): fault-inject a wrong fee decode / slot layout / identity at
   the core construction path and observe both guards' red paths flip, replayable through the
   ADR-068 offline harness. The guards themselves are intact and their red paths green (above).
3. **Optional (item 5): the direct-pool zero-balance `raise NoLiquidity`** at
   `src/degenbot/curve/curve_stableswap_liquidity_pool.py:~771` moving core-side beside the metapool
   contract (PC4's note) — not attempted.
4. **Per-family refresh coverage.** Only the V2 arm of `refresh_pool` has a direct test
   (`tests/builders/test_pool_refresh_via.py`). The Aerodrome / V3 / V4 / Curve / Balancer arms are
   exercised by nothing that runs in a default gate — the families' `bot.update()` tests are
   `online_rpc`-marked and deselected. Scripting them through `OfflineProvider` (the harness already
   records V3 slot0 and V4 `getSlot0`/`getLiquidity` calls for build tests) is the obvious next step
   and was not affordable in this chunk's budget. **This is a real coverage gap, not a formality.**
5. **MANAGER LADDER: PENDING** — `just pre-push` whole-workspace lanes were not run by this worker
   (the scoped gates above, including the manager-mandated full `just test-rust-nextest` and
   `just lint-rust-check`, were).

## Tree / foreign edits

Files written by this chunk (13): `rust/crates/engine/degenbot-bot/src/bot_core/refresh.rs` (new),
`rust/crates/engine/degenbot-bot/src/bot_core/mod.rs`,
`rust/crates/foundation/degenbot-substrate/src/cl_orchestration.rs`,
`rust/crates/shells/degenbot-python/src/bot/pool.rs`,
`rust/crates/shells/degenbot-python/src/bot/py_bot_io.rs`, `src/degenbot/_ffi/__init__.pyi`
(regenerated), `src/degenbot/bot/_bot.py`,
`src/degenbot/types/abstract/liquidity_pool.py`,
`src/degenbot/builders/curve_pool_builder.py`, `src/degenbot/builders/balancer_builder.py`,
`src/degenbot/builders/protocol.py`, `src/degenbot/runner/build_paths.py`,
`tests/builders/test_pool_refresh_via.py` (new, replacing the deleted
`tests/builders/test_builder_update_staticmethod.py`), plus this doc.

No foreign (unowned) modifications were present in the working tree during the run. No commit; no
ergo task-state change; `.ergo/plans.jsonl` untouched.

---

# Continuation round — items 2 + 3 landed (Curve arm + core-side parity probes); item 5 deferred

STATUS: **GREEN on the continuation scope.** The ONE construction entry now serves EVERY family
including Curve, and the identity-parity guards are enforced CORE-SIDE (at the mint site) with both
red paths demonstrated. The optional Curve `NoLiquidity` relocation (item 5) is deferred. Items 1 +
4 (the prior worker's landed half) are untouched.

Baseline cited: `docs/ergo-results/hqylek-result.md` run 2 (green). Landed pre-existing chunks not
re-opened: `937153a29`, `4a03f5e04`, `231334a8f`, `ad035c48a`, `345a8c118`, `5de576784`.

## Shapes changed (diff, items 2 + 3)

| file | intent |
|---|---|
| `rust/crates/engine/degenbot-bot/src/bot_core/pool_builder/parity.rs` (NEW) | The construction identity-parity guard's core home. `IdentityDivergence` (typed refusal); `verify_address_identity(minted, registered)` for the V2/V3/Aerodrome `(token0, token1, address)` echo; `verify_v4_identity(minted, registered)` for the V4 `(currency0, currency1, fee, tick_spacing)` key; `registered_address_identity(bot, pool_id)` reads the registered identity back off shared state. Module unit tests fault-inject a swapped identity (address red path) and a wrong fee decode (V4 red path). |
| `rust/crates/engine/degenbot-bot/src/bot_core/pool_builder/mod.rs` | `pub mod parity;` — the guard's core door. |
| `rust/crates/engine/degenbot-bot/src/bot_core/build_register.rs` | `V2BuildError::IdentityDivergence` / `V4BuildError::IdentityDivergence` variants. `build_and_register_v2` verifies the minted echo against the registry of record's identity; `build_and_register_v4` captures the minted `V4Identity` before the move and verifies the registered key after registration. Both run at the mint site, so a mismatched construction is refused CORE-SIDE. |
| `rust/crates/engine/degenbot-bot/src/bot_core/pool_builder/tests.rs` | NEW core test: builds + registers a real V3 pool via the route, reads `parity::registered_address_identity` off shared state, proves the minted echo matches and a swapped echo is refused (address-keyed red path against REAL registered state). |
| `rust/crates/shells/degenbot-python/src/bot/errmap.rs` | `map_v2_build_err` / `map_v4_build_err` map the new `IdentityDivergence` variant onto `PoolRegistrationError` (a `ValueError` subclass). |
| `rust/crates/shells/degenbot-python/src/bot/mod.rs` | `construct_pool` gains a `registries: Option<Vec<String>>` per-family hint and a CURVE arm: the `"stableswap"` branch now splits on `variant == Some("balancer_stable")` (Balancer Vault arm) vs the Curve arm (drives `build_and_register_curve_pool` core-side). The ONE entry now serves all four families. |
| `src/degenbot/bot/_bot.py` | The two Curve special-case dispatches (the `except DegenbotValueError` fallback and the `STABLESWAP` builder branch) are DELETED. `build_pool` now resolves a discovery-only pool (resolver raised — Curve has no `factory()`) to the generic `STABLESWAP`/`None` descriptor and routes EVERY family through the ONE `construct_pool` call (passing `registries` for Curve). The Curve companions (metapool base-pool recursion + coin/underlying/LP ERC20 builds) run through `CurvePoolBuilder.finalize` after construction. `_CURVE_REGISTRIES` is built from `degenbot.curve.deployments`. |
| `src/degenbot/builders/curve_pool_builder.py` | `CurvePoolBuilder.build` no longer constructs: it delegates to the ONE entry (`self._py_bot.construct_pool(... family="stableswap" ... registries=...)`) then calls the NEW `finalize(handle, ...)` (the metapool recursion + companion builds the retired `build` body held). No second Curve construction implementation remains. |
| `src/degenbot/_ffi/__init__.pyi` | regenerated stub (see the self-check). |

Behaviour is preserved by construction: `construct_pool`'s Curve arm calls the SAME core
`build_and_register_curve_pool` (same detection choreography, same `RpcCurveDataProvider`, same
registration) the Python builder called before; the change removes Python's Curve branch, not any
core work.

## Item 2 — one construction entry serves every family incl. Curve

Core `construct_pool` family arms (all reachable, one match):

```
"concentrated_liquidity" => ...   (V3 route)
"constant_product"       => ...   (AerodromeV2 | V2)
"weighted"               => ...   (Balancer weighted)
"stableswap"             => ...   (Balancer stable | Curve)   <-- NEW Curve arm
other                    => ValueError
```

Acceptance greps (foreground):

```bash
rg -n 'construct_pool' rust/crates/shells/degenbot-python/src/bot/mod.rs   # the ONE entry + its arms
rg -n 'construct_pool\(' src/degenbot/bot/_bot.py                          # ONE call site (all families)
rg -n 'construct_pool' src/degenbot/builders/curve_pool_builder.py          # the builder DELEGATES to it
```

`src/degenbot/bot/_bot.py` has NO Curve construction branch: the Curve path is the shared
`construct_pool` call plus `CurvePoolBuilder.finalize` (companions only). `CurvePoolBuilder.build`
delegates to the same entry (kept solely as the recorded dual-driver parity harness's driver).

## Item 3 — parity guards core-side (red paths + flip record)

The enforcement now lives where the identity is minted (`bot_core/pool_builder/parity.rs`, wired into
`build_and_register_v2` / `build_and_register_v4`). The Python-side comparison guards are RETAINED as
a redundant cross-seam assertion — no guard is weaker; the core guard is strictly additional.

Core-side red-path records (fault injection):

- **Address-keyed identity red path** — unit test
  `bot_core::pool_builder::parity::tests::address_identity_parity_accepts_a_match_and_refuses_a_divergence`
  (swapped `(token0, token1)`) and the real-state test
  `bot_core::pool_builder::tests::construction_identity_parity_guard_reads_registered_identity_and_refuses_a_wrong_echo`
  (reads the registered identity off `BotState`, refuses a swapped echo). Green in the workspace run.
- **V4 identity red path** — unit test
  `bot_core::pool_builder::parity::tests::v4_identity_parity_accepts_a_match_and_refuses_a_wrong_fee`
  (wrong fee decode). Green in the workspace run.

**Flip demonstration (load-bearing):** the address red-path assertion was flipped once
(`.expect_err(..)` -> `.expect(..)`) and the workspace suite re-run:

```
FLIP_EXIT=100
FAIL degenbot-bot bot_core::pool_builder::parity::tests::address_identity_parity_accepts_a_match_and_refuses_a_divergence
Summary [10.445s] 4724 tests run: 4723 passed, 1 failed, 29 skipped
panicked at parity.rs:141: FLIP: guard must be Err: IdentityDivergence { address: 0x03.., detail: "minted AddressIdentity { token0: 0x02.., token1: 0x01.. } != registered AddressIdentity { token0: 0x01.., token1: 0x02.. }" }
```

The guard returned the typed `Err(IdentityDivergence { .. })` — proof the red path fires. The
assertion was reverted byte-for-byte; the suite re-ran green (4724 passed / 29 skipped, exit 0).

Python-side guards (UNCHANGED, still armed):
`tests/test_bot.py::TestBuildDelegatedIdentityReturnSurface::test_build_delegated_v2_parity_mismatch_raises`
and `::TestBuildManagedPoolIdentityReturnSurface::test_build_v4_parity_mismatch_raises` — both green
in the full-suite run (they may thin to delegation later; they are not weakened now).

## Stub diff self-check (`just gen-stubs`)

`git diff src/degenbot/_ffi/__init__.pyi` (vs HEAD) adds the item-1 `refresh_via` def plus this
round's ONE new parameter on `construct_pool`:

```
+        registries: Sequence[str] | None = None,
```

`just gen-stubs --check` -> exit 0 ("committed stubs match generator output").

## Scoped + manager-mandated gate exits (captured DIRECTLY, `cmd; echo $?`, no pipes)

| gate | exit |
|---|---|
| `rustfmt --check --edition 2021 --config skip_children=true -- <6 touched .rs>` | **0** |
| `uv run --no-sync ruff format --check -- <2 touched .py>` | **0** |
| `uv run --no-sync ruff check <2 touched .py>` | **0** |
| `just lint-rust-check` (full workspace clippy; incl. the architecture subset) | **0** |
| `just gen-stubs` (regen) | **0** |
| `just gen-stubs --check` | **0** |
| `just rebuild-if-stale` (build receipt + extension) | **0** |
| `just verify-build-fresh` | **0** ("extension is fresh") |
| `just test-rust-nextest` (FULL workspace) | **0** — 4724 passed / 29 skipped |
| `uv run --no-sync pytest tests/test_ffi_boundary.py` | **0** — 8 passed |
| `uv run --no-sync pytest tests/builders tests/curve tests/golden tests/pool_companion tests/uniswap tests/arbitrage` | **0** — 1002 passed / 5 skipped |
| `uv run --no-sync pytest` (FULL Python suite) | **0** — 3130 passed / 50 skipped |
| FLIP probe (temporary, reverted) `just test-rust-nextest` | **100** — 1 failed (the flipped assertion), then reverted -> 0 |

Logs: `/tmp/iyl6qd2/{genstubs,genstubs-check,rebuild,rebuild2,nextest,nextest2,flip,lint-rust,ffi,scoped,pytest-full}.log`.

## Counts delta (baseline table shape, re-observed @ HEAD)

| # | metric | baseline | observed | delta | attribution |
|---|---|---|---|---|---|
| C1 | `construct_pool` family arms in `degenbot-python/src/bot/mod.rs` | 3 families (no Curve) | **4 families (incl. Curve)** | **+1 (Curve)** | **THIS round (item 2)** |
| C2 | Curve construction call sites in `src/degenbot/bot/_bot.py` | 1 (Curve builder dispatch) | **1 (the shared `construct_pool` call)** | 0 | **THIS round (item 2)** |
| C3 | core-side parity-guard red-path probes | 0 | **3** (address unit + address real-state + V4 unit) | **+3** | **THIS round (item 3)** |
| C4 | `_update_pool|_builder_for_pool` lines under `src/degenbot` | 6 | 0 | -6 | item 1 (prior worker) |
| C5 | `_POOL_VERSION_MAP|v2_v3_subclass_table|V3_VARIANT_TABLES|V3_FAMILIES` lines under `src/` | 5 | 0 | -5 | item 4 (prior worker) |

Verification one-liners (read-only, foreground):

```bash
sed -n '926,1010p' rust/crates/shells/degenbot-python/src/bot/mod.rs | rg '"concentrated_liquidity"|"constant_product"|"weighted"|"stableswap"'   # 4 arms
rg -n 'construct_pool\(' src/degenbot/bot/_bot.py src/degenbot/builders/curve_pool_builder.py   # 1 + 1 (delegating)
rg -n 'IdentityDivergence|verify_address_identity|verify_v4_identity' rust/crates/engine/degenbot-bot/src/bot_core/pool_builder/parity.rs | wc -l
```

## Item 5 — DEFERRED (documented)

The direct-pool zero-balance `raise NoLiquidity` at
`src/degenbot/curve/curve_stableswap_liquidity_pool.py:~771` was NOT moved core-side beside the
metapool contract (`degenbot-pools::curve_dy_io::check_metapool_liquidity`, PC4's note). It is the
task's explicitly OPTIONAL item, and the budget went to items 2 + 3. No behaviour changed here; the
Python raise is intact.

## Acceptance

- `rg "_update_pool|_builder_for_pool" src/degenbot` — **EMPTY**. OK (item 1)
- `rg "_POOL_VERSION_MAP|v2_v3_subclass_table|V3_VARIANT_TABLES|V3_FAMILIES" src/` — **EMPTY**. OK (item 4)
- One construction entry serves every family including Curve. OK (item 2 — Curve arm + `_bot.py` routing)
- Both parity guards' red paths demonstrated core-side (flips recorded). OK (item 3)

## Tree / foreign edits

Files written by THIS round (10): `rust/crates/engine/degenbot-bot/src/bot_core/pool_builder/parity.rs`
(new), `.../pool_builder/mod.rs`, `.../pool_builder/tests.rs`, `.../bot_core/build_register.rs`,
`rust/crates/shells/degenbot-python/src/bot/errmap.rs`, `.../bot/mod.rs`,
`src/degenbot/_ffi/__init__.pyi` (regenerated), `src/degenbot/bot/_bot.py`,
`src/degenbot/builders/curve_pool_builder.py`, plus this doc.

No foreign (unowned) modifications were present in the working tree during the run. No commit; no
ergo task-state change; `.ergo/plans.jsonl` untouched.

## Continuation round 2 (manager finding)

The manager's full ladder came back RED on the `ty` type-checker inside
`just lint-python-check`. Both prior workers ran `ruff` but NOT `ty`, so two
`invalid-argument-type` diagnostics survived the items-1-4 landing. They are the
only two diagnostics; both are now fixed with the narrowest truthful change.

### The two diagnostics (verbatim)

```
error[invalid-argument-type]: Argument to bound method `CurvePoolBuilder.finalize` is incorrect
   --> src/degenbot/bot/_bot.py:651:17
    |
651 |                 state_block=block,
    |                 ^^^^^^^^^^^^^^^ Expected `int`, found `int | None`
info: element `None` of union `int | None` is not assignable to `int`
info: Method defined here --> src/degenbot/builders/curve_pool_builder.py:104
    (state_block: int)
```

```
error[invalid-argument-type]: Argument to bound method `Pool.refresh_via` is incorrect
  --> src/degenbot/types/abstract/liquidity_pool.py:38:46
   |
38 |         return self._py_pool.refresh_via(io, block_number)
   |                                              ^^^^^^^^^^^^ Expected `int | None`, found `int | str | None`
info: element `str` of union `int | str | None` is not assignable to `int | None`
info: Method defined here --> src/degenbot/_ffi/__init__.pyi:3499
    (block_number: int | None = None)
```

### Fix decisions

**1. `_bot.py` — resolve `block` to a concrete `int` at the construction seam.**
`BuildPoolRequest.state_block` is `int | None`; the resolve expression is
`request.state_block if request.state_block is not None else self._io.get_block_number()` —
i.e. the `else` arm is the live chain head, so `block` is NEVER `None` in practice
(the code's own comment already said a `None` "would otherwise degrade to `block=0`").
The annotation was simply loose (`int | None`). Chosen fix: declare `block: int`
(the truthful type) rather than widening `CurvePoolBuilder.finalize(state_block: int)`.
`finalize`'s `state_block` genuinely IS a required concrete block — it threads into the
metapool base-pool `BuildPoolRequest(state_block=state_block)` recursion and the companion
ERC20 builds, all of which need a real block — so an unresolved `None` is NOT semantically
legal there and widening would have been a lie. The declaration is now `int`, documented
with a comment naming the `int`-typed sinks (`construct_pool`, `finalize`).

**2. `types/abstract/liquidity_pool.py` — narrow the abstract's `block_number` to `int | None`.**
`BlockIdentifier = int | str`; the old signature was `BlockIdentifier | None` = `int | str | None`,
but the FFI boundary (`Pool.refresh_via`) takes `int | None` and the core `refresh_pool`
does not understand block-tag strings. The abstract layer is a pass-through — it adds no
resolution — so the honest fix is to narrow its parameter to match the FFI: `int | None = None`.
All callers already pass a resolved `int | None`: `Bot.update` resolves
`resolved_block_number = int(...) if block_number is not None and not isinstance(block_number, int)
else block_number` before calling, and `tests/builders/test_pool_refresh_via.py` passes
int literals / omits the arg. The resolution rule ("tags resolve at the driver seam that
owns the RPC clock; the abstract signature is numbers-only") is documented in the method
docstring, and block-tag support is explicitly marked PC10's future scope — NOT built here.
The now-unused `BlockIdentifier` TYPE_CHECKING import was removed.

### Scoped gate exits (DIRECT, `cmd; echo $?`, no pipes)

| gate | exit |
|---|---|
| `just lint-python-check` (ruff check + `ty check src/`) | **0** — both clean; "All checks passed!" |
| `just lint-rust-check` | **0** |
| `uv run --no-sync pytest tests/test_bot.py tests/builders tests/curve tests/golden` | **0** — 302 passed, 4 skipped |
| `uv run --no-sync pytest tests/test_ffi_boundary.py` | **0** — 8 passed |
| `just gen-stubs --check` | **0** — "committed stubs match generator output" |
| `uv run --no-sync ruff format --check -- src/degenbot/bot/_bot.py src/degenbot/types/abstract/liquidity_pool.py` | **0** — 2 files already formatted |

`rustfmt --check` was not run: this round touched NO `.rs` file (both edits are `.py`).
The abstract-layer edit is a Python source file, not a generated stub, so `gen-stubs --check`
stayed green with no diff.

### Files touched this round (2)

- `src/degenbot/bot/_bot.py` — `block` declared `int` (was `int | None`) + rationale comment.
- `src/degenbot/types/abstract/liquidity_pool.py` — `refresh_via` `block_number: int | None`;
  docstring resolution rule; unused `BlockIdentifier` import removed.
- `docs/ergo-results/iyl6qd-result.md` — this section.

No foreign (unowned) modifications were present in the working tree during the round. No
commit; no ergo task-state change; `.ergo/plans.jsonl` untouched.

## Manager review — verdict PASS (2026-10-10, after continuation + fix round 2)

Adversarially verified against the diff: item 1 — `rg '_update_pool|_builder_for_pool' src/degenbot` = 0, `refresh_via` -> `bot_core::refresh::refresh_pool` with per-family arms; item 4 — `_POOL_VERSION_MAP` gone (permutation filter reads the core-exported family taxonomy); item 2 — the Curve arm dispatches inside `construct_pool` (variant split `balancer_stable` vs Curve), `_bot.py` holds no Curve construction special-case (only the `CurvePoolBuilder` finalize helper); item 3 — `pool_builder/parity.rs` enforces both identity guards AT THE MINT SITE (`build_register.rs:193` V2, `:527` V4), Python guards retained as cross-seam redundancy, both red paths demonstrated (flip exit 100 with `Err(IdentityDivergence{..})` printed). Item 5 deferred per scope rules (recorded in this doc).

Fix round 2 (two `ty` diagnostics the scoped gates missed): `_bot.py` block resolved to a concrete `int` at the seam (widening `finalize(state_block)` was correctly rejected — the value threads into metapool recursion), and the abstract `refresh_via` signature narrowed to `int | None` to match the FFI truthfully (block-tag typing deferred to PC10 as planned). Lesson filed: scoped gates must include `just lint-python-check` (ruff AND ty).

Manager battery `/tmp/pc5c-gates/`: `just pre-push` **exit=0** + `just gen-stubs --check` **exit=0**. Green against the PC0 baseline (run 2).
