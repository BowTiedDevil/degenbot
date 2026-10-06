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

## Predicted fix order (to be re-ranked against the baseline)

1. Hoist all RPC out of the SQLite transaction (pool verification; Aave discount/config
   facts) — a pre-lock fact-collection phase at pinned blocks.
2. Delta persist — write only event-touched ticks/words instead of full-map rewrite.
3. Batch `get_or_create` prefetch/insert + `prepare_cached` in Aave apply.
4. Multicall3-batch + memoize revision reads in config dispatch.
5. Overlap/pipeline chunk fetches.
6. Delete the dead `fetch_exchange` refresh loop.

## Glossary terms

`golden capture`, `cassette`, `statement ledger`, `replay bench` are defined in
`GLOSSARY.md` under "Capture and replay".
