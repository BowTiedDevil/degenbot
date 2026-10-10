# FMPNVD (PC5, epic GZPPS3): the pool-construction switchboard gets its one entry — Python half landed

STATUS: **PARTIAL — GREEN ON THE LANDED HALF; refresh walk + core-side negative probes PENDING.**
The build-path switchboard (the centerpiece's Python half) is collapsed onto ONE core
construction entry. The `update()`/`refresh_pool` walk and the core-side negative probes are
NOT landed (see *PENDING*). Scoped gates + the manager-mandated full battery are all green
against the landed diff; exits captured directly below.

Baseline cited: `docs/ergo-results/hqylek-result.md` run 2 (green). Landed pre-existing chunks
not re-opened: `937153a29`, `4a03f5e04`, `231334a8f`, `ad035c48a`, `345a8c118`.

## Shapes changed (diff)

| file | intent |
|---|---|
| `rust/crates/shells/degenbot-python/src/bot/mod.rs` | NEW `PyBot.construct_pool` pymethod — the ONE construction entry's FFI face. It dispatches on `(family, variant)` (ADR-059 D6: dispatch at the tier boundary then monomorphic) to the already-shipped per-family adapters (`build_v3_pool` / `build_v2_pool` / `build_aerodrome_v2_pool` / `build_balancer_weighted_pool` / `build_balancer_stable_pool`) and returns the normalized echo `(pool_id, token0, token1, address, variant)`. A new family is a new arm HERE, not a Python branch. |
| `src/degenbot/bot/_bot.py` | `build_pool`'s 90-line `issubclass` chain is deleted. The delegated path is now: `resolve_pool_type` → `pool_type.family`/`pool_type.variant` (DATA) → `self._py_bot.construct_pool(...)` → parity guard → `_register_delegated_tokens(py_pool, variant=...)` → `pool_class_for_descriptor(pool_type)` → `from_handle`. The PancakeSwap slot-layout branch (`slot_layout = "pancakeswap" if issubclass(...)`) is deleted — the V3 fork storage layout resolves CORE-side from the deployment table (`builder::build_v3` → `is_pancakeswap_v3_factory`), which is the species manifest's storage-layout-id projection. `_register_delegated_tokens` now keys the Balancer N-token branch on `variant` (data) instead of `issubclass`. |
| `src/degenbot/_ffi/__init__.pyi` | regenerated stub (one def; see the self-check). |
| `tests/test_bot.py` | `_ScriptedEngine` double: `build_v2_pool` field → `construct_pool`; the V2 parity-mismatch red-path test now scripts `construct_pool`. |

Behaviour is preserved by construction: `construct_pool` dispatches to the SAME core
`build_and_register_*` methods (same single-flight keys, same lock sites, same telemetry spans)
the facade called before — the collapse removes Python's family branch, not any core work.

## Manifest design (one-liner) + its two consumers

The species manifest (`degenbot-db::species`, `species.toml`, ADR-059 D3) is a typed Rust record
single-sourced from one TOML file; it is ALREADY the first home for the per-species
`slot_layout` id. PC5's construction path now consumes it INDIRECTLY and removes the last Python
re-encoding of that fact: the core `build_v3` resolves `ClSlotLayout` from the factory via the
deployment table (`is_pancakeswap_v3_factory`), so the Python `issubclass(PancakeswapV3Pool)`
branch is gone. The manifest has TWO consumers: the Rust core (schema routing + the V3 layout-id
projection) and the Python driver (which no longer names a fork for layout). No new
manifest row was required — the entry existed (`pancakeswap_v3` → `slot_layout = "pancake_v3"`).

## Stub diff self-check (`just gen-stubs`)

`git diff src/degenbot/_ffi/__init__.pyi` adds exactly ONE def and removes none:

```
+    def construct_pool(
+        self, /, address: str, family: str, variant: str | None = None,
+        block: int | None = None, db: bool = True,
+        tick_data_fetcher: Any | None = None,
+        factories: Sequence[str] | None = None,
+        vault: str | None = None, invariant_version: int | None = None,
+    ) -> tuple[int, str, str, str, str]: ...
```

`just gen-stubs --check` → exit 0 (committed stubs match generator output).

## Negative-probe records (both parity guards' red paths)

The two return-surface parity guards survive the collapse and their red paths are still
demonstrated by existing, GREEN tests (both exercised through the collapsed `build_pool`):

- **Identity guard (V2/V3) — red path:** `tests/test_bot.py::TestBuildDelegatedIdentityReturnSurface::test_build_delegated_v2_parity_mismatch_raises`.
  The `construct_pool` echo (`token0=0xC…`, `token1=0xD…`) diverges from the registered handle
  (`0xa…`/`0xb…`) → `build_pool` raises `DegenbotValueError`. `pytest tests/test_bot.py` exit 0.
- **V4 identity guard — red path:** `tests/test_bot.py::TestBuildManagedPoolIdentityReturnSurface::test_build_v4_parity_mismatch_raises`
  (unchanged; still green in the full-suite run).

**PENDING (brief's settled shape):** relocating these guards to CORE-side NEGATIVE PROBES
(fault-inject a wrong fee decode / slot layout / identity at the construction path, replayable
through the ADR-068 harness). The guards themselves are intact and their red paths demonstrated —
only the relocation to core-side probes is outstanding.

## Scoped gate exits (captured DIRECTLY, `cmd; echo $?`, no pipes)

| gate | exit |
|---|---|
| `rustfmt --check --edition 2021 --config skip_children=true -- rust/crates/shells/degenbot-python/src/bot/mod.rs` | **0** |
| `uv run --no-sync ruff format --check -- src/degenbot/bot/_bot.py` | **0** |
| `uv run --no-sync ruff format --check -- tests/test_bot.py` | **0** |
| `uv run --no-sync ruff check src/degenbot/bot/_bot.py` | **0** |
| `just lint-rust-check` (clippy; includes `check-engine-impl-blocks`) | **0** |
| `just gen-stubs --check` | **0** |
| `just rebuild-if-stale` (build receipt + extension) | **0** |
| `just test-rust-nextest` (FULL workspace, manager-mandated) | **0** — 4721 passed / 29 skipped |
| `uv run --no-sync pytest tests/golden tests/pool_companion tests/builders tests/test_ffi_boundary.py` | **0** — 310 passed / 4 skipped |
| `uv run --no-sync pytest` (FULL Python suite) | **0** — 3131 passed / 50 skipped |

Logs: `/tmp/fmpnvd-lint-rust.log`, `/tmp/fmpnvd-genstubs.log`, `/tmp/fmpnvd-rebuild.log`,
`/tmp/fmpnvd-nextest.log`, `/tmp/fmpnvd-pytest-scoped.log`, `/tmp/fmpnvd-pytest-full.log`.

Nothing detached; every command ran in the foreground.

## Counts delta (PC0 baseline table shape, re-observed @ HEAD `345a8c118` + this diff)

| # | shrink target | baseline | observed | delta | attribution |
|---|---|---|---|---|---|
| C1 | `issubclass` sites in `src/degenbot/bot/_bot.py` | 9 | **0** | **−9** | **THIS chunk (PC5)** |
| C2 | `liquidity_gross, liquidity_net, block` arity assertions under `rust/crates/shells/degenbot-python/src` | 7 | 6 | −1 | PC1 `4QBFGW` (landed) |
| C3 | `AlloyProvider::new(rpc_url, ...)` sites under the shell | 7 | 7 | 0 | untouched (PC7) |
| C4 | `pub(crate) fn run_.*registration` wrappers in `bot/pump.rs` | 6 | 0 | −6 | PC3 `231334a8f` (landed) |
| C5 | `py_bot or Bot` / `py_bot if ...` factory workarounds in `tests/helpers/` | 7 | 0 | −7 | PC2 `4a03f5e04` (landed) |
| C6 | `amount0_delta = rust_amount0` sign-mapping copies in `src/degenbot/uniswap/` | 4 | 4 | 0 | untouched (PC8) |

(Observed @ the current tree; C2/C4/C5 moved under earlier landed chunks, re-counted read-only —
PC5's own delta is C1 9 → 0.)

## Acceptance

- `rg "issubclass" src/degenbot/bot/_bot.py` — **EMPTY** (exit 1). ✅
- `rg "_update_pool|_builder_for_pool" src/degenbot` — **NOT empty** (exit 0; sites at
  `_bot.py:84,112,1132,1137,1142,1145`). ❌ **PENDING** (refresh walk).
- The construction entry serves V2 / V3 / V4(managed) / AerodromeV2 / Balancer weighted / Balancer
  stable through ONE `construct_pool` call; Curve still routes through its Python discovery
  builder. ➖ **PARTIAL**.
- The negative probes demonstrate both parity guards' red paths. ➖ **PARTIAL** (red paths
  demonstrated via existing tests; core-side relocation PENDING).

## PENDING (manager re-runs / next session)

1. **The refresh walk** — `_update_pool` / `update()` / `_builder_for_pool` collapse onto
   `pool.refresh_via(io)` → a core `refresh_pool(bot, pool_id, io, block) -> RefreshOutcome`.
   This needs a core refresh entry for the Aerodrome/Curve/Balancer families that today refresh
   through their Python builders' `update()` (no core refresh exists for them), so it is a
   separate, sizeable sub-chunk.
2. **Core-side negative probes** for the two parity guards (fee decode / slot layout / identity),
   replayable through the ADR-068 harness.
3. **Curve + `stays-python` residue collapse** — `builders/{erc20,curve_pool,balancer}_builder*.py`
   onto the shipped shell entries (ADR-023 Post-adoption A2). Curve's `factory()`-less discovery +
   metapool recursion + LP-token companion builds are why Curve still keeps its Python builder.
4. **MANAGER LADDER: PENDING** — `just pre-push` whole-workspace lanes were not run by this worker
   (scoped gates + the manager-mandated `just test-rust-nextest` / `just lint-rust-check` /
   `just gen-stubs --check` / `test_ffi_boundary.py` were).

## Tree / foreign edits

Files written by this chunk (4): `rust/crates/shells/degenbot-python/src/bot/mod.rs`,
`src/degenbot/bot/_bot.py`, `src/degenbot/_ffi/__init__.pyi` (regenerated), `tests/test_bot.py`,
plus this doc. No foreign (unowned) modifications were present in the working tree during the run.
No commit; no ergo task-state change; `.ergo/plans.jsonl` untouched.

## Manager review — verdict PASS for the landed half (2026-10-10)

Independently verified: `rg issubclass src/degenbot/bot/_bot.py` = **ZERO** (C1 9 -> 0),
`construct_pool` + data-driven family/variant dispatch in place, stub delta exactly +1,
`degenbot-db::species`/`species.toml` confirmed pre-existing at HEAD `85bdea4fa` (this chunk wired the
last Python re-encoding — the PancakeSwap slot-layout id — into it). Manager battery
`/tmp/pc5-gates/`: `just pre-push` **exit=0** + `just gen-stubs --check` **exit=0**.

The PENDING half (update-path `refresh_via` collapse + the Curve arm + core-side parity-guard
probes + `_POOL_VERSION_MAP`/species-constant consumers) is split out as its own chain task and
carries FMPNVD's remainder by ID. FMPNVD closes on the landed half.
