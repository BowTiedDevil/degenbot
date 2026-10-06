# Updater RPC/SQL survey — golden captures, measurement, and the perf program

Survey of the pool updater (`rust/crates/integrations/degenbot-pool-updater`) and the
Aave updater (`rust/crates/integrations/degenbot-aave/src/updater`) for RPC- and
SQL-level inefficiencies and the testability gap around their coherence mechanism.
The decision this survey feeds is {doc}`adr/ADR-068-updater-golden-capture-replay`;
the work is filed as an ergo epic ("Updaters: golden-capture replay harness + RPC/SQL
perf program").

## The problem in one paragraph

Both updaters are chunk loops: fetch logs over a block range, decode, apply to SQLite
in one transaction, and advance a restart cursor. Their only end-to-end coherence
mechanism is the pre-commit chunk verification (computed map vs chain truth; Aave adds
touched-position and market-wide checks) — which runs against live RPC in production.
The SQL apply path is covered by synthetic-input contract tests; the fetch/decode/verify
path is covered almost nowhere. A regression in RPC shaping or in the apply loop is
discovered by the production gate rolling a chunk back, after the code has shipped.

## Findings — pool updater

- **Write lock held across RPC round-trips.** `apply_chunk_writes_on_conn`
  (`src/run.rs:389`) runs the verifier per pool with `vc.rt.block_on(verify_v3/v4_liquidity_map_on_chain…)`
  while the caller's `Transaction` is open (`run.rs` chunk body). Every verification
  round-trip holds the SQLite write lock.
- **O(map) SQL per touched pool per chunk.** `compute_v3_liquidity_update_on_conn`
  (`degenbot-db/src/liquidity_updater.rs:488`) re-reads the pool's FULL tick map +
  bitmap; `persist_v3` (`liquidity_updater.rs:1202`) rewrites it all —
  `DELETE … NOT IN (<all live ticks>)` plus an upsert of every live tick and word —
  even when one `Mint` moved one tick. Same shape for V4.
- **Marker write re-derives pool kind** with a per-pool `SELECT kind FROM pools`
  (`set_v3_liquidity_update_marker_on_conn`) although the compute stage already holds
  the state row.
- **Dead queries after every chunk.** A post-commit loop calls `db.fetch_exchange(spec.id)`
  and discards the result, then `load_active_exchange_specs` re-loads the full spec set
  each chunk (`run.rs` chunk tail).
- **RPC shaping.** V3 `Mint`/`Burn` is a whole-chain `eth_getLogs` per chunk (the fetch
doc notes pool-address filtering "can't be efficiently RPC-filtered"); the fetch phases
(pool creations → V3 scan → V4 per-manager) are sequential; verification re-verifies
each touched pool's FULL map every chunk regardless of delta size.

## Findings — Aave updater

- **`db.lock()` held across per-tx RPC `.await`s** — acknowledged in the
  `await_holding_lock` contract on `run_aave_update_driver`: the discount pre-pass and
  the config dispatch do substrate lookups and `eth_call`s while holding the write
  connection.
- **No RPC batching or memoization in config dispatch** (`updater/config_dispatch.rs`):
  per-event single `eth_call`s — `ATOKEN_REVISION()` / `DEBT_TOKEN_REVISION()` /
  `POOL_REVISION()` / `CONFIGURATOR_REVISION()` / `getSourceOfAsset` / `balanceOf` —
  sequential, with revision values re-read although `Upgraded` fires "once per market
  lifetime" (their own §4.2 note). `degenbot-rpc::multicall3` exists and is used only by
  the liquidity verifier.
- **N+1 SQL in the apply stage** (`updater/run/apply.rs`, ~2.6k lines): per-event
  `conn.execute`/`query_row` and `get_or_create_user/asset/position` SELECT-then-INSERT
  chains per entity per event; `updater/verify.rs` runs market-wide SELECTs.

## Findings — the coherence gap

- Pool updater: pre-commit per-pool map verification + market-wide verification at
  interval/completion (`verify.rs`). Aave: `verify_touched_positions_on_conn` per chunk
  + full four-check verification at interval/completion. All run against live RPC.
- The verification gate itself has **no negative probe**: nothing in CI demonstrates it
  catches a known-bad map or a known-wrong position. It is exercised for the first time
  in production, on real divergence.
- Existing coverage: `chunk_atomicity_contract.rs` / `no_duplicate_writer_contract.rs`
  drive the SQL apply with synthetic `ChunkInputs` (no RPC);
  `liquidity_verifier.rs` unit tests replay recorded call maps;
  `facade/.../investigation/chain_capture.rs` has a "pseudo-golden" replay harness for a
  different subsystem. The middle — the real chunk loop over realistic RPC traffic — is
  the untested span.

## Assets already in the tree

| Asset | Location | Gap |
|---|---|---|
| `OfflineProvider` — real `AlloyProvider` over recorded JSON | `degenbot-rpc/src/offline.rs`, `src/degenbot/provider/offline_provider.py` | serves `eth_call`/`getCode`/`getBlock` only — no `eth_getLogs`, the updaters' primary RPC |
| Pseudo-golden replay + committed captures | `investigation/chain_capture.rs`, `tests/fixtures/path*.json` | different subsystem |
| Cassette recorder precedent | `scripts/record_curve_tripool_cassette.py` → `tests/fixtures/chain_data/` | one-off; Python capture scripts are being retired (ergo `KSB4IW`) |
| SQL-path contract tests | `chunk_atomicity_contract.rs` etc. | synthetic inputs; fetch/decode/verify untouched |
| `prepare_cached` idiom, Criterion benches, tracing (`op_info!`), Jaeger/Prometheus/hotpath | scattered | none applied to updater loops |

## Approaches considered

**A. RPC cassette replay at the transport seam + SQLite statement-ledger goldens +
replay-driven bench (composite).** Record `(method, canonical params) → response`
ledgers with a recording transport; replay through a cassette transport injected as the
`AlloyProvider`; record SQL traffic via rusqlite trace/profile hooks; golden = canonical
DB dump + normalized statement ledger; the same cassette is the benchmark workload.
Pro: exercises fetch → decode → compute → verify → apply end-to-end, offline,
deterministically; one capture format serves tests, benchmarks, and coherence; matches
existing idioms. Con: needs `eth_getLogs` replay support and a provider-injection seam
(a localhost mock server over `rpc_url` avoids the seam but loses determinism).

**B. Typed-seam golden transcripts** (`ChunkInputs`/`AaveChunkEvent` → golden write-set
+ statement ledger). Cheap, wire-format-immune; complements A for SQL logic. Con: does
not cover fetch/decode/verify — the named blind spot. Complement only.

**C. Deterministic EVM oracle** (ScratchEvm/frame-replay executes real pool contracts
to generate captures + expected DB state). Unlimited adversarial cases, no node. Con:
substantial lift. Wave 2.

**D. Updater telemetry + hotpath profiling** (spans/metrics through the existing
Jaeger/Prometheus pipeline). Real-world numbers, cheap. Con: measures only what ships.
Complement.

**E. Forked-node replay** (anvil fork / archive reth). Highest fidelity; the right
capture-GENERATION environment. Con: nondeterministic, archive-dependent, anvil fails
closed on `eth_callMany` (`/skill:node-identity`). Keep out of CI.

**F. Differential testing vs the legacy Python updater.** Rejected: against the
"Python is a driver shell" architecture and the hard-cutover policy.

## Ranking

1. **A** — the one investment serving testability and measurement.
2. **B** — build the ledger/dump golden infra first (reused by A).
3. **D** — cheap; confirms fixes transfer to production.
4. **C** — wave 2: negative probes and edge-case generation.
5. **E** — capture generation only.
6. **F** — rejected.

## Measurement methodology

Per-chunk counters asserted as drift-gated goldens and fed by a replay bench: RPC
round-trips and bytes per chunk; SQL statement count; **write-lock hold wall time**;
chunks/sec. Gates follow the glossary idioms: machine-emitted captures are **drift
gates** (regenerate-and-diff with the same pipeline that writes them) and every gate
carries a **negative probe** (mutate a captured response or DB dump, observe red).

## Baseline (ergo NNCPXA — the measurement gate)

The replay bench's first baseline, recorded — where a chunk spends its time
and what one chunk costs in round trips and statements. The workload is the
replay bench (GLOSSARY “replay bench”): the REAL chunk loops
(`run_pool_update_on_db` / `run_aave_update_on_db`) over the committed seed
cassettes through the cassette replay transport (ADR-068 D5 injection, zero
network), each run writing a fresh temp SQLite DB wrapped in the statement
ledger.

**Reproduce (one command):** `just bench-updaters` (wraps
`cargo bench --locked --manifest-path rust/Cargo.toml -p degenbot --features
degenbot/sql-ledger --bench updater_replay_bench`; bench profile, median of 9
runs after 2 warmups, fresh transport/temp-DB/ledger per run).

**Machine note:** this devcontainer (8 cores, 62 GB RAM); the golden captures
are reth-recorded (each corpus file's `provenance.source`:
`reth/v2.7.0-3d592ec/x86_64-unknown-linux-gnu`) with the span pinned in the
corpus file. Run-to-run spread on this machine is ±10% on the timing
columns; the counters (round trips, bytes, statements) are deterministic and
asserted iteration-stable by the bench.

**Capture IDs (corpus file names):**
`pool_update_chunk_26102622-26102626.json` (verify gate OFF — the replay
suites' posture), `pool_verify_chunk_26102622-26102626.json` (the same chunk
replayed with the pre-commit verification gate ON — the gate's per-pool
tick/bitmap reads ride the recorded `eth_call`s),
`aave_update_chunk_26130440-26130445.json`.

| capture | rt | resp bytes | stmts | sql µs | fetch µs | dc+cmp µs | verify µs | apply µs | lock-hold µs | chunk µs | chunks/sec |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| pool_update_chunk_26102622-26102626 (gate OFF) | 2 | 4289 | 25 | 1000 | 130 | 34 | 0 | 207 | 1483 | 3997 | 250 |
| pool_verify_chunk_26102622-26102626 (gate ON) | 7 | 5510 | 25 | 1000 | 132 | 31 | 48 | 209 | 1527 | 4192 | 238 |
| aave_update_chunk_26130440-26130445 | 6 | 28274 | 150 | 2000 | 558 | 1327 | 0 | 169 | 2933 | 6138 | 162 |

Column semantics: `rt` = ledger entries served per run (the RPC round-trip
count; pool gate-ON adds the gate's five `eth_call`s to the two `eth_getLogs`
passes); `resp bytes` = the served answers' serialized payload bytes; `stmts`
= the statement ledger's record count per chunk apply (the drift-gate
literals: 25 / 150); stages are the run entries' `Instant` spans — fetch
(the RPC log fetches), dc+cmp (in-transaction decode+compute: pool per-pool
full-map read+compute; Aave per-tx discount pre-pass + config dispatch, both
under the write lock), verify (pre-commit on-chain gate), apply (remaining
in-transaction SQL); `lock-hold` = `transaction()` open → commit/drop.
`sql µs` sums the ledger's per-statement profile times — SQLite's legacy
`CurrentTimeInt64` profile path quantizes each statement to whole
milliseconds, so treat it as a coarse floor (the `Instant` stage spans carry
the fine timing).

What the numbers say (first read, to be re-ranked per fix):

- **The write lock is the pool chunk's center of gravity.** Gate OFF, the
  hold is 1.48 ms of a 4.0 ms chunk; the fetch+decode outside it is ~0.16 ms.
  With the gate ON the hold absorbs the verification RPC too (1.53 ms) —
  the structural shape Perf A removes (hoist all RPC out of the transaction).
  This corpus's gate reads are small (one brand-new pool, five calls), so the
  headline is the hold itself, not the verify delta; a corpus with real tick
  depth will widen it.
- **Aave's in-lock compute is the dominant in-transaction stage.**
  dc+cmp = 1.33 ms inside a 2.93 ms hold — the per-tx discount pre-pass +
  config-dispatch RPC reads while `db.lock()` is held (survey findings, Perf
  A/D), with apply's per-event SQL only ~0.17 ms measured (150 statements,
  mostly fast; Perf C's N+1 cost is statement COUNT before it is time).
- **Round-trip shapes are now pinned by gates**: pool 2 (`eth_getLogs`
  ×2), pool+gate 7 (+5 `eth_call`s), aave 6 (`eth_getLogs` ×6; no
  config events in this span → no `eth_call`s) — asserted as literals in
  the replay suites (`EXPECTED_RPC_ROUND_TRIPS`), alongside the statement
  counts (25/150) and the served-byte totals.
- **The dead-query tail is visible by subtraction**: pool chunk wall 4.0 ms
  vs fetch+hold 1.6 ms — the remainder is loop overhead + the post-commit
  `fetch_exchange` refresh loop (finding 6, Perf F).

## Predicted fix order (to be re-ranked against the baseline)

1. Hoist all RPC out of the SQLite transaction (pool verification; Aave discount/config
   facts) — a pre-lock fact-collection phase at pinned blocks.
2. Delta persist — write only event-touched ticks/words instead of full-map rewrite.
3. Batch `get_or_create` prefetch/insert + `prepare_cached` in Aave apply.
4. Multicall3-batch + memoize revision reads in config dispatch.
5. Overlap/pipeline chunk fetches.
6. Delete the dead `fetch_exchange` refresh loop.

### Post-Perf-A rows (ergo YN5QAF — hoist RPC out of the SQLite write transaction)

Same workload, same command (`just bench-updaters`), same devcontainer, median
of 9 after 2 warmups. Perf A moved the pool loop's per-pool O(map) compute and
the pre-commit per-pool + market-wide verification RPC into a pre-transaction
read pass (`compute_preverified_liquidity` + the hoisted gates): zero RPC
awaits — and none of the planned pools' map SELECTs — sit between
`transaction()` open and commit/drop. A verification RED now never opens the
transaction (the observable contract — `RunError::Verification`, zero rows,
stamp unadvanced — is unchanged; the probes assert the same triple).

| capture | rt | resp bytes | stmts | sql µs | fetch µs | dc+cmp µs | verify µs | apply µs | lock-hold µs | chunk µs | chunks/sec |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| pool_update_chunk_26102622-26102626 (gate OFF) | 2 | 4289 | 25 | 1000 | 131 | 74 | 0 | 190 | 1494 | 4241 | 235 |
| pool_verify_chunk_26102622-26102626 (gate ON) | 7 | 5510 | 25 | 1000 | 143 | 74 | 49 | 181 | 1576 | 4494 | 222 |
| aave_update_chunk_26130440-26130445 (Perf A NOT landed — see note) | 6 | 28274 | 150 | 1000 | 559 | 1396 | 0 | 169 | 2869 | 6060 | 165 |

Statement ledgers: the pool gate-OFF golden regenerated ORDER-ONLY — count
25 = 25, the DB dump BYTE-IDENTICAL, and exactly one statement TEXT changed:
the end-of-chunk stamp now carries the read pass's assumed marker in its
WHERE (`UPDATE exchanges SET last_update_block = ? WHERE chain_id = ? AND id = ?
AND last_update_block = ?`, arg_count 3 -> 4) — the structural restart-invariant
check. The two unknown-pool scope fetches moved pre-`BEGIN` (order-only). The
aave golden is untouched (no aave code landed in this task).

**Findings from the post-Perf-A measurement (re-ranking input):**

- **The pool lock-hold is now COMMIT-BOUND, not RPC/compute-bound.** The
  after rows show apply 210 -> 190 µs and dc+cmp absorbing the read pass;
  the hold moved 1502 -> 1494 µs (gate OFF). The probe evidence (temporary
  instrumentation, since removed): `tx.commit()` costs ~1100-1460 µs INSIDE
  the bench process for this corpus's 22-statement transaction, while an
  immediate empty `transaction()+commit()` control on the same connection
  costs 4-7 µs — and the IDENTICAL write shape commits in ~124 µs in a plain
  test process. The brief's ~200-300 µs (pool) / ~200-400 µs (aave)
  post-Perf-A expectations are therefore not reachable on this devcontainer
  by ANY hoist: the remaining ~1.25 ms is the chunk commit itself, not
  lock-held work. Attributing it (WAL checkpoint? fsync? bench-process
  context?) is the measurement gate's next finding; a WAL/synchronous
  PRAGMA change is production-semantics territory and was NOT touched.
- **The aave corpus has ZERO in-lock RPC to hoist.** The aave cassette
  records 6 `eth_getLogs` (the fetch surface) and no `eth_call`s — this span
  dispatches no config events and no GHO-discount path, so the
  discount pre-pass and config dispatch early-return. The baseline row's
  1.33 ms dc+cmp ("in-lock RPC" per the first read) is per-tx DECODE +
  substrate SQL reads + the operations parse, not RPC. Hoisting it is a
  different shape than the pool hoist: exact fact prediction requires either
  an in-memory overlay that mirrors the operations parser's substrate writes
  (user/position creation is log-driven but not topics-extractable), or a
  shadow-DB fact run whose recorded events replay against the real
  transaction. Ergo the aave half of Perf A needs its own task with that
  design settled; the pool half above is the template.

## Glossary terms

`golden capture`, `cassette`, `statement ledger`, `replay bench` are defined in
`GLOSSARY.md` under "Capture and replay".


### Post-Perf-C rows (ergo B2U4VL — Aave apply-stage SQL batching)

Same workload, same command (`just bench-updaters`), same devcontainer. Perf C
is the chunk-level substrate cache (`run/substrate.rs`): a handful of
set-shaped `SELECT ... IN` prefetches at chunk start (the topic- and
emitter-derived candidate users + their positions/configs, the market's asset
rows + contract revisions, the chain's GHO row), an in-memory write-overlay
consulted before those indexes, and ONE sorted multi-row `UPDATE ... FROM
(VALUES ...)` for the chunk's `ReserveDataUpdated` writes (the
`liquidity_updater.rs` deterministic-order idiom). The parser, the config
dispatch, and the apply arms all consult the cache; every cache-visible write
lands an overlay update.

| capture | rt | resp bytes | stmts | sql µs | fetch µs | dc+cmp µs | verify µs | apply µs | lock-hold µs | chunk µs | chunks/sec |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| aave_update_chunk_26130440-26130445 (post-Perf-C) | 6 | 28274 | 52 | 1000-2000 | 534 | 216-269 | 0 | 152-181 | 1758-2128 | 5484-5747 | 174-182 |

Statement ledger: 150 -> 52 (the drift gate's `EXPECTED_LEDGER_STATEMENTS`
updated with the measured literal; the golden regenerated through the same
writer). Class-by-class: the 51 per-event asset-resolution JOINs -> 1 lazy
probe (the candidate set pre-answers positives AND negatives — an address
absent from the market's asset rows is a proven SQL `Ok(None)`, cached with
zero statements); the 16 in-tx GHO re-resolves -> 0 (one prefetch + dirty-mark
re-query on any `aave_gho_tokens` write — none in this span); the 11
get-or-create-user probes -> 0 (candidate absence is proven by the prefetch —
the INSERT runs probe-less; row ids and order unchanged); the 9 position
by-key probes + 9 per-apply balance reads -> 0 (the overlay carries id +
`(balance, last_index)`); the 7 per-parse POOL-revision reads -> 1 prefetch;
the 9 per-event `ReserveDataUpdated` UPDATEs -> 1 multi-row statement (sorted
by asset id, last-event-wins per asset, loud short-count error). The DB dump
golden is BYTE-IDENTICAL (row ids, insertion order, and every value
unchanged — the §3.4 observable outcome). The RPC counters are untouched:
6 round trips / 28274 bytes.

**Findings from the post-Perf-C measurement (re-ranking input):**

- **The aave in-lock compute was statement-COUNT-bound, as ranked.** dc+cmp
  moved 1396 -> 216-269 µs (−81..−85%) and the lock-hold 2869 -> 1758-2128 µs
  with ZERO RPC change — the §3.4 read-your-own-writes ordering is preserved
  by the overlay (tx N+1 consults cache entries only where they reflect all
  writes of txs < N; every cache-visible write updates the overlay at the
  apply site, and the GHO row re-queries on any dirty mark).
- **The residual in-lock time is the chunk commit, not lock-held work.** The
  same commit-bound finding the pool lane measured applies: ~1.2 ms of the
  remaining hold is `tx.commit()` inside the bench process. No further
  statement-count lever moves it; a WAL/synchronous PRAGMA change is
  production-semantics territory and was NOT touched.
- **Perf D (config-dispatch RPC batching) re-rank: still corpus-blind, and
  the corpus now shows config dispatch is NOT RPC-free in general.** This
  span's config dispatch handled 9 discount-config events through
  pure-decode + substrate reads (no `eth_call` — the recorded 6 round trips
  are all `eth_getLogs`), so the revision-read multicall still has no
  measured weight HERE (its value is corpus-blind like the aave half of Perf
  A was). What Perf C removed is the config dispatch's per-event SQL: its
  handlers now ride the same substrate cache. The remaining measured lever
  for THIS corpus is the chunk commit (the measurement gate's next finding),
  then Perf F's dead-query tail.
