# HQYLEK (PC0, epic GZPPS3): baseline artifacts captured — the deepening's shrink targets, counted live

STATUS: **GREEN BASELINE CAPTURED (run 2, 2026-10-10).** `just pre-push` all lanes +
`just gen-stubs --check` green; Python lane 3125 passed / 50 skipped in 124.49s. Counts
independently re-verified by the manager (every cell matches). Run 1 was RED on a foreign
untracked file — kept below as history: the baseline caught it, the owning lane fixed it within
minutes of the intercom relay. Pinned at HEAD `26e79c7f7` + the wave-5 untracked fixtures as found.

## GATE SLOTS (baseline run 2 — 2026-10-10, measured by the manager)

| slot | gate | command | status |
|---|---|---|---|
| G0 | pre-push lanes 1–6 | `just pre-push` → Object DB GC, commitlint, Rust clippy, Python lint, Stubtest drift, Rust build | **GREEN** (all six reached under fail-fast) |
| G1 | Rust workspace tests | `just test-rust-nextest` (lane 7 inside `just pre-push`) | **GREEN** (run 2; run-1 red — resolved history below) |
| G2 | Rust lint | `just lint-rust-check` (inside `just pre-push`) | **GREEN** |
| G3 | Python build + tests | `just dev` + `just test-python` (lanes 8–9 inside `just pre-push`) | **GREEN** — 3125 passed, 50 skipped in 124.49s |
| G4 | Stub drift | `just gen-stubs --check` | **GREEN** (exit 0 — "committed stubs match generator output") |

Runner evidence (logs kept, exits captured directly — no pipes): run 1 `/tmp/pc0-gates/`
(`pre-push.log` exit 101, `gen-stubs.log` exit 0); run 2 `/tmp/pc0-gates2/`
(`pre-push.log` exit 0, `gen-stubs.log` exit 0).

### Run-1 G1 failure — measured, attributed, resolved (history)

```
error: constant `PDP_CONFIGURATOR_IMPL` is never used
  --> crates/integrations/degenbot-aave/tests/wave5_config_span_replay.rs:81:7
error: constant `AS_POOL_IMPL` is never used
  --> crates/integrations/degenbot-aave/tests/wave5_config_span_replay.rs:84:7
error: could not compile `degenbot-aave` (test "wave5_config_span_replay") due to 2 previous errors
```

Attribution: `git log`/`git blame` report **"no such path in HEAD"** — the file is UNTRACKED, part of
a foreign in-flight stream (`generate_aave_config_windows.rs`, `tests/fixtures/cassettes/wave5/`,
`tests/fixtures/sql_goldens/wave5/` also untracked). The stream's files appeared in the tree
**during** this battery run. Not this epic's work; left untouched per tree-hygiene rules. Both live
implementation peers were notified via intercom 2026-10-10 (messages `7de8b356`, `f2aa2aa6`).

G3's ten corpus files (all tracked, all present):

```
tests/aerodrome/test_aerodrome_v2_onchain_parity.py
tests/aerodrome/test_aerodrome_v3_onchain_parity.py
tests/balancer/test_balancer_stable_onchain_parity.py
tests/balancer/test_balancer_v2_onchain_parity.py
tests/curve/test_curve_onchain_parity.py
tests/pancakeswap/test_pancakeswap_v2_onchain_parity.py
tests/uniswap/v2/test_camelot_v2_onchain_parity.py
tests/uniswap/v3/test_uniswap_v3_onchain_parity.py
tests/uniswap/v4/test_uniswap_v4_onchain_parity.py
tests/golden/test_oracle_replay_offline.py
```

No gate result is asserted here, and none may be inferred from this page — a later task's "behaviour
unchanged" claim is proved against the manager's G1/G2/G3/G4 run at review, not against this note.

## Counts table (observed @ HEAD `26e79c7f7`)

| # | shrink target | expected | observed | drift |
|---|---|---|---|---|
| C1 | `issubclass` sites in `src/degenbot/bot/_bot.py` | 9 | **9** | none |
| C2 | `tick_data` Python->Rust conversion sites under `rust/crates/shells/degenbot-python/src` | 7 | **7** | none |
| C3 | `AlloyProvider::new(rpc_url, ...)` sites under `rust/crates/shells/degenbot-python/src` | 7 | **7** | none |
| C4 | registration-lifecycle wrappers in `rust/crates/shells/degenbot-python/src/bot/pump.rs` | 6 | **6** | none |
| C5 | `py_bot` factory workarounds in `tests/helpers/*.py` | 7 | **7** | none |
| C6 | sign-mapping copies in `src/degenbot/uniswap/*.py` | (no expected value in the task body) | **4** | n/a |

Verification one-liners (all run read-only in the foreground):

```bash
rg -n 'issubclass' src/degenbot/bot/_bot.py | wc -l                                    # -> 9
rg -n 'tick_data must be|extract_tick_data|make_tick_info|TickInfo' \
   rust/crates/shells/degenbot-python/src                          # -> 27 hits / 5 files;
                                                                   #    the C2 table lists the 7 conversion bodies
rg -n 'AlloyProvider::new\(rpc_url' rust/crates/shells/degenbot-python/src | wc -l      # -> 7
rg -n 'pub\(crate\) fn run_.*registration' \
   rust/crates/shells/degenbot-python/src/bot/pump.rs | wc -l                            # -> 6
rg -n 'py_bot or Bot|py_bot if py_bot is not None else Bot' tests/helpers/ | wc -l       # -> 7
rg -n 'amount0_delta = rust_amount0' src/degenbot/uniswap/ | wc -l                       # -> 4
```

### C1 — the nine `issubclass` dispatch sites

`src/degenbot/bot/_bot.py` lines 656, 675, 684, 700, 706, 715, 759, 914, 921. Adjacent
(out of scope, do not fold into the shrink target): `src/degenbot/registry/pool_type.py` holds 2 more
— 11 `issubclass` lines tree-wide.

### C2 — the seven `tick_data` conversion sites

Counted as "a site that materializes Rust tick values from a Python tick container". Five are the
`{tick: (gross, net, block)}` 3-tuple form, two are the 2-tuple `make_tick_info` form:

| site | container shape |
|---|---|
| `bot/pool.rs:106` (`PyTickWordFetcher::fetch_missing_tick_word`) | dict -> `HashMap<i32, TickInfo>` (3-tuple) |
| `bot/pool.rs:1828` (`PyLiquidityPool.update_tick_data`) | dict -> `HashMap<i32, TickInfo>` (3-tuple) |
| `bot/pool.rs:3008` (`fn extract_tick_data`, the shared helper body) | `PyAny` -> `HashMap<i32, TickInfo>` (3-tuple) |
| `bot/mod.rs:1824` (`register_v3_pool`) | inline dict -> `HashMap<i32, TickInfo>` (3-tuple) |
| `bot/mod.rs:1978` (`register_v4_pool`) | inline dict -> `HashMap<i32, TickInfo>` (3-tuple) |
| `bot/engine/solve.rs:415` (`sync_v3_pool_states`) | dict -> `HashMap<i32, TickInfo>` via `make_tick_info` (2-tuple) |
| `bot/engine/solve.rs:481` (`sync_v4_pool_states`) | dict -> `HashMap<i32, TickInfo>` via `make_tick_info` (2-tuple) |

Counting-definition notes (so a later task's number is comparable):

- `make_tick_info`'s own definition (`bot/engine/mod.rs:172`) and the single call site of
  `extract_tick_data` (`bot/pool.rs:582`) are **helpers / callers**, not conversion sites — including
  them gives 9 and 8 respectively. The 7 above is the "one conversion body per boundary" count the
  task body means.
- The reverse direction (Rust -> Python tick dicts) is **not** counted: `db/snapshot.rs:192`,
  `bot/pool.rs:2921-2965`, `bot/pool.rs:3177` (`tick_data()` getter), `bot/pool.rs:3118`.
- `bot/result_channel.rs:339` and `bot/pool.rs:3006` are comments only; `pool/mod.rs:235/310` reads
  `tick_data` straight out of `degenbot_db` (already-Rust, no Python conversion).

### C3 — the seven `AlloyProvider::new(rpc_url, ...)` sites

`aave_updater.rs:173, 343, 449, 597` and `pool/mod.rs:154, 245, 320` — all seven use the literal two-arg
`(rpc_url, 5)` form (retries = 5). Breadth context: `AlloyProvider::new(` appears 11 times in the shell; the other
four take a `&rpc_url`/`&provider_url`/`&url` name (`bot/engine/verify.rs:173, 256`,
`rpc/async_contract.rs:40`, `rpc/contract.rs:41`), so a later task narrowing on the exact
`rpc_url,` (unborrowed) spelling will see 7 and a task narrowing on provider construction generally
will see 11.

### C4 — the six registration-lifecycle wrappers in `bot/pump.rs` (240 lines total)

Six `pub(crate) fn` definitions, one per (version x flavour) cell:

| line | wrapper |
|---|---|
| 87 | `run_v3_registration_lifecycle` |
| 111 | `run_v4_registration_lifecycle` |
| 138 | `run_v3_registration_lifecycle_blocking` |
| 161 | `run_v4_registration_lifecycle_blocking` |
| 191 | `run_v3_registration_lifecycle_with_retry_blocking` |
| 215 | `run_v4_registration_lifecycle_with_retry_blocking` |

**Pattern drift worth flagging for later tasks:** the body's literal pattern
`run_[a-zA-Z_]*registration` returns **0** hits here, because the `v3`/`v4` infixes carry digits the
character class excludes. `run_.*registration` (the task body's own spelling) is the pattern that
works: 15 matching lines, of which 6 are the wrapper definitions above (the rest are the wrappers'
calls into the core methods plus two doc cross-references).

### C5 — the seven `py_bot` factory workarounds in `tests/helpers/*.py`

```
tests/helpers/aerodrome_pool_factory.py:45   bot = py_bot or Bot()
tests/helpers/v4_pool_factory.py:68          bot = py_bot or Bot()
tests/helpers/curve_pool_factory.py:102      bot = py_bot if py_bot is not None else Bot()
tests/helpers/v3_pool_factory.py:111         bot = py_bot if py_bot is not None else Bot()
tests/helpers/v2_pool_factory.py:119         py_bot = py_bot if py_bot is not None else Bot()
tests/helpers/balancer_pool_factory.py:77    bot = py_bot if py_bot is not None else Bot()
tests/helpers/balancer_pool_factory.py:147   bot = py_bot if py_bot is not None else Bot()
```

Note the two spellings (`or`-fallback vs explicit `is not None`) and that `v2_pool_factory.py:119`
rebinds `py_bot` rather than `bot` — a single shared helper has to subsume both forms.

### C6 — the sign-mapping copies in `src/degenbot/uniswap/*.py`

4 sites, all in `v3_liquidity_pool.py` (340, 376, 448, 482), each a `zero_for_one` sign-mapping pair:

```python
amount0_delta = rust_amount0 if zero_for_one else -rust_amount0
amount1_delta = -rust_amount1 if zero_for_one else rust_amount1
```

The parallel Rust->delta mapping in `v4_liquidity_pool.py` (374-385, 411-416, 487-488, 514-517) uses
the same `rust_amount0/rust_amount1` ternaries but different output names, so it is *not* a copy of
the `amount0_delta =` shape. 30 `amount{0,1}_delta` lines exist under `src/degenbot/uniswap/`; only
these 4 are the duplicated sign-mapping block.

## Known skips (read-only survey of the G3 corpus)

- **Tier deselection is config, not per-test.** `pyproject.toml` `addopts` carries
  `-m "not slow and not base and not online_rpc"`, so those three markers are deselected from the
  default run before collection; `tests/conftest.py` `_NON_RUNNING_MARKERS` is
  `{online_rpc, base, slow, skip}` and `_check_fork_tier_tags` fails **collection** if a
  `fork_*`-fixture test carries none of them. `onchain_oracle` is deliberately *not* in that set —
  the corpus tests run offline in the default suite and read truth from `tests/golden/`.
- **Conditional skips inside the corpus (5 + 1):**
  - `tests/golden/test_oracle_replay_offline.py` skips at lines 594, 630, 655, 754, 782 when the
    recorded corpus file is absent. Reason constant (line 98): the `py_oracle` corpus was never
    recorded — no configured endpoint serves the pinned state (camelot at block 477785000: the
    reachable publicnode endpoint rejects archive state at `get_code` with "Archive requests require
    a personal token"; base: `mainnet.base.org` rate-limits bursty recording). A corpus or stale
    golden that *is* present fails rather than skips.
  - `tests/uniswap/v2/test_camelot_v2_onchain_parity.py:139` skips when the recorded on-chain
    `getAmountOut` reverted at record time (`oracle.exception_type`).
- **Out-of-corpus skips that can still fire in a G3 run's process:** `tests/conftest.py:159`
  (live RPC/anvil unavailable) and `:340` (anvil binary absent); `tests/golden/test_live_drift.py:151`
  (only when not selected via `-m live_drift`) and `:154` (cargo not on PATH). The `live_drift`
  marker is explicit-only and is never part of a default/CI/pre-push run.
- **No `xfail` in the corpus** — every `rg` for `pytest.mark.xfail` / `pytest.xfail` across the ten
  G3 files is empty. The only `skip` markers in the corpus are the conditional ones listed above.
- **Not verified here:** which of the conditional skips actually fire in the manager's run. That is a
  G3 observation; this page only records where they can fire and why.

## How later PC tasks use this page

- The counts table is the "structure changed" side of the M0 A/B pattern: a later task reports the
  same six numbers and shows them shrinking, so its "behaviour unchanged" claim has a fixed unit.
- G1-G4 are the "behaviour unchanged" side, and they are the manager's to run; a later task must
  cite the manager's run (with its SHA), never this page, as the pre-deepening green baseline.
- The counts are pinned to HEAD `26e79c7f7`. Anything that moves them before PC1 lands (the
  telemetry lane, the Aave DB-write lane, the hop-entry/solve-cycle work) must be re-counted rather
  than assumed — the task body's numbers were already three days older than the tree, and C4's
  pattern-vs-name drift shows how cheaply a stale pattern silently reads as zero.

## Tree / foreign edits

- Files written by this chunk: **`docs/ergo-results/hqylek-result.md` (this file) only.** No source,
  no test, no plan file, no `.ergo/plans.jsonl` entry, no ergo task-state change, no commit.
- Foreign modifications present in the working tree at start and left untouched (not mine, not
  reverted): `GLOSSARY.md`, `docs/adr/ADR-023-construction-io-retirement-disposition.md`,
  `docs/adr/index.md` (+64/-4). No other worker's edits were observed during the run.
- Untouched by instruction: `rust/crates/engine/degenbot-solvers/src/cl/` (telemetry lane) and
  `rust/crates/integrations/degenbot-aave/` (Aave DB-write lane). `rust/crates/shells/degenbot-python/`
  was read only, never written.

## CLOSED — this page is the green baseline

PC1–PC10 cite THIS run (run 2, 2026-10-10) as their pre-deepening green baseline: `just pre-push`
all lanes green + `just gen-stubs --check` green, with the counts table as the "structure changed"
unit. Nothing is pending. Run-1's foreign red stays on the page as evidence the battery has a
working red path.

No command was detached or left running past the harness bound; this run executed only read-only
foreground greps.
