# BW0 / E5TK7W — Word-bitmap consumer audit (evidence matrix)

Task: `E5TK7W` (epic `U2XZSD`). Pure investigation, NO code changes.
Workspace HEAD at time of audit: `4c7b3431195d632542741e1f5e5bd749d78ea7ef` (`git status --porcelain` clean at start; no foreign edits observed during the run).
Result doc path per brief: `docs/ergo-results/e5tk7w-result.md` (the task body suggested a `bw0-<id>.md` name; the brief's explicit path wins).

Reference machine reads below are all Rust/Python source; every "count" is a command I ran, pasted verbatim.

---

## 0. TL;DR verdict

The user's hypothesis is **mostly confirmed with one precise correction**:

- The **in-memory word-map VALUES** (`BitmapAtWord.bitmap`) are **CONSISTENCY-CHECK-ONLY**
  (manager-corrected 2026-10-11 — see the Review Correction section): production code DOES
  branch on the bits — `verify_tracked_tick_map` (`tick_assembly.rs:435-466`, the Tracked
  intake gate: "a bit is set iff a gross>0 row exists") and the updater verifier
  (`verify.rs:131`). No ENGINE/SOLVER logic reads them. They are a *projection* of
  `tick_data` keys, recomputed on every read (FFI, Python) or write (DB). The **`block`
  stamp** is **LEGACY-DEAD** (writes + test asserts only — verified).
- The **DB table `initialization_maps` / `managed_pool_initialization_maps`** is
  **CONSISTENCY-CHECK-ONLY**: its only production readers are the on-chain-truth
  verifier and the intake reconciliation. No engine behaviour consumes the bits.
  It is **drop-eligible** once the verifier derives words in-process (which the live
  path already does).
- The **word-KEY machinery is LOAD-BEARING** and must NOT die with the values:
  `known_bitmap_words: HashSet<i32>` drives sparse miss-detection; `word_of` /
  `get_tick_word_and_bit_position` compute word positions; the on-chain
  `tinykeccak`-word bootstrap enumerates bits fetched fresh from chain.
- The **solver tick iteration (`compute_tick_ranges` / `gen_ticks_iter` /
  `V3TickRangeForSolver`) is LOAD-BEARING but does NOT read the word map at all** —
  it walks `tick_data` keys and computes word boundaries arithmetically. The
  manager's CAUTION ("this looks solver-facing") is correct about the *file* and
  wrong about the *dependency*: the solver reads zero bitmap words.

So the deletion wave can remove the word-map values + DB table + updater `tick_bitmap`
plumbing, but must preserve the word-key set, the word-position math, the on-chain
bootstrap, and the solver walk.

---

## 1. Decision matrix

Legend: **LB** = LOAD-BEARING · **CCO** = CONSISTENCY-CHECK-ONLY · **DEAD** = LEGACY-DEAD · **UNSURE**.

| # | Component | Verdict | Evidence (file:line + counts + command) |
|---|-----------|---------|------------------------------------------|
| (a1) | In-memory word-map **values** `HashMap<i32, BitmapAtWord>` produced by the apply path (`LiquidityMapUpdateResult.tick_bitmap`, `liquidity_mapping.rs:69-73`) | **DEAD** (as live state; a pure projection of `tick_data`) | Zero logic reads a bit. The only production sinks are (i) DB upsert and (ii) `known_bitmap_words` seeding from keys. `rg -n 'tick_bitmap' rust/crates/foundation/degenbot-pools/src/v3_state.rs rust/.../v4_state.rs` → **no `tick_bitmap` field**; those structs carry `known_bitmap_words: HashSet<i32>` only (`v3_state.rs:512`, `v4_state.rs:369`). FFI value is *re-derived*: `cl_snapshot_fields` builds bits from `state.tick_data().keys()` (`degenbot-python/src/bot/pool.rs:2863-2898`). |
| (a2) | `flip_tick` (`liquidity_mapping.rs:106`) + the bitmap half of `apply_liquidity_mapping_update` (`:186`) | **DEAD** | Sole callers are the bitmap-bearing apply path: `liquidity_updater.rs:1453` (apply_event_loop) and `pool_ingress.rs:890` (backfill), both feeding a DB write. The **engine** mutates ticks with a separate tick-only fn `apply_liquidity_to_tick_range` (`registry.rs:798,995`; `tick_bitmap.rs:640+`). No reader ever inspects the flipped bit. |
| (b) | `BitmapAtWord.block` stamp (`liquidity_mapping.rs:46`) | **DEAD** | Writes only: `flip_tick` `:123` (`entry.block = update_block`), apply seeds. Readers: **tests only**. `rg -n '\.block\b' rust/crates/foundation/degenbot-math/src/cl/liquidity_mapping.rs` → writes at `:123`; all other hits are `assert_eq!` in `#[cfg(test)]` (`:454,541,597,650,708`). `rg -n 'entry\.block|bw\.block|word\.block' rust -g '*.rs'` → no production read. DB `snapshot::BitmapAtWord` has **no `block` field at all** (`degenbot-db/src/snapshot.rs:33-36`). The FFI view stamps words with `state.update_block()`, not a stored row block (`pool.rs:2895`). |
| (c) | `tick_bitmap.rs`: `compute_tick_ranges` (`:419`), `gen_ticks_iter` (`:233`), `gen_ticks` (`:59`), `V3TickRangeForSolver` (`:608`) | **LOAD-BEARING**, but **zero word-map dependency** | `compute_tick_ranges(tick_data: &HashMap<i32, TickInfo>, …)` — reads `tick_data` keys/`liquidity_net` only. `gen_ticks_iter` collects `tick_data.keys()` (`:283-296`) and computes boundaries arithmetically from `tick_spacing` (`:266-276`). Consumers: `v3_state.rs:816-835`, `v4_state.rs:606-624`, `v3_state.rs:1350`, `v4_state.rs:920`, solver crates. |
| (d) | On-chain staged word fetch: `TickWordFetcher`/`FetchedTickWord`/`fetch_missing_tick_word`, `stage_word_fetch_by_pool_id`/`install_word_fetch`, `tick_fingerprint` | **LOAD-BEARING**; consumes word **key + ticks**, never a bitmap value | `FetchedTickWord { word: i32, ticks: HashMap<i32,TickInfo> }` — **no bitmap field** (`tick_fetch/miss.rs:52-59`). Merge inserts ticks + `known_bitmap_words.insert(fetched.word)` (`registry.rs:676-690`, `:881-894`). `tick_fingerprint` = `(update_block, tick_data.len(), Σ tick.block)` (`cl_orchestration.rs:1149-1171`) — tick-data only. |
| (d2) | On-chain single-word bootstrap `bootstrap_v3/v4_tick_word` / `fetch_tick_bitmap` / Python `_fetch_v3` | **LOAD-BEARING** (on-chain bitmap, fetched transiently) | The *chain* bitmap is read to enumerate set bits → fetch each tick: `builder.rs:945-968`, `tick_fetch/bootstrap.rs:167-191`, Python `builders/tick_data_fetcher.py:155-176`. Not the stored/persisted map. |
| (e1) | DB table `initialization_maps` / `managed_pool_initialization_maps` — **writes** | **DEAD-as-data** (projection of `tick_data`) | Writers: `persist_v3`/`persist_v4` (`liquidity_updater.rs:1482-1545`, upsert `:1371/:1400`), delta path (`:1872/:1874`), `upsert_init_maps_impl` (`:1921`). The value written is `result.tick_bitmap` from `apply_liquidity_mapping_update`, itself derived from `tick_data`. No other writer: `rg -n 'INITIALIZATION_MAPS|initialization_maps'` → only `liquidity_updater.rs` (44 hits) + schema/migrate + the dead row-reads. |
| (e2) | DB table — **reads** | **CONSISTENCY-CHECK-ONLY** | Readers: (1) `fetch_v3/v4_liquidity_map(_on_conn)` (`liquidity_updater.rs:402/:481`) → verify/intake; (2) `fetch_liquidity_map(_on_conn)` (`snapshot.rs:385/:463`) → tick_assembly chain arm; (3) `read.rs:428/:464` `fetch_initialization_map` — **zero production callers** (see §8); (4) FFI snapshot facade (`degenbot-python/src/db/snapshot.rs:184-189`) → Python `LiquidityMapSnapshot` (tests only in `src`). No read feeds engine behaviour. |
| (f) | Updater integration: the `tick_bitmap` half of `fetch_v3/v4_liquidity_map` results | **CONSISTENCY-CHECK-ONLY** | Consumers all verify: `pool-updater/src/verify.rs:246/283/367/407` → `shared_map()` (`:107-138`) → `verify_liquidity_map`; `degenbot-python/src/pool/mod.rs:262/340` and `degenbot-cli-core/src/pool.rs:426/460` (FFI/CLI verify) ; `degenbot-substrate/src/tick_assembly.rs:250` → `TickMapSeed.bitmaps` → `verify_staged`. `git grep`-class census: `rg -n 'fetch_v3_liquidity_map\|fetch_v4_liquidity_map'` shows no reader that *acts* on bits outside verify. |
| (g1) | Python `BitmapAtWord` TYPE + `tick_bitmap` dict **VALUES** (`concentrated/types.py:26`) | **DEAD** | Python `tick_bitmap` is a derived view: `cl_companion.py:88-104` property, docstring "derived from `tick_data` keys". `update_tick_data` doc (`cl_companion.py:164-199`): "the VALUES are NOT stored (the bitmap derives from the tick rows)". |
| (g2) | Python `tick_bitmap` dict **KEYS** | **LOAD-BEARING** (mirrors `known_bitmap_words`) | Sparse backfill gate reads **membership only**: `cl_companion.py:275` `if word not in self.tick_bitmap and not ensure_word_known(...)`. |
| (g3) | Python snapshot plumbing `snapshot_readers.py` / `v3_snapshot.py` / `v4_snapshot.py` `tick_bitmap` | **DEAD** for value consumers in `src` (facade + tests) | `snapshot_readers.py:28/72/165/263-269`; `v3_snapshot.py:116-150`; `v4_snapshot.py:128-171`. In `src`, no caller of `snapshot.tick_bitmap(...)` except the definitions; `get_all_liquidity_maps` drops the bitmap entirely (`snapshot_readers.py:171-178`; Rust `stream_liquidity_maps` SELECTs only `liquidity_positions`, `snapshot.rs:194-280`). Consumers are tests (`tests/uniswap/test_snapshot_facade_semantics.py`, `tests/database/test_snapshot_seam_parity.py`). |
| (h1) | `known_bitmap_words: HashSet<i32>` (V3/V4PoolState) + `word_of`/`seed_known_bitmap_words` | **LOAD-BEARING** | Sparse miss-detection: `v3_state.rs:1331-1341`, `:1437-1450`, `:1487-1500`; `v4_state.rs:905-1050`. |
| (h2) | `PoolTickCoverage` | **LOAD-BEARING** | Gates miss-detection (`coverage == Sparse`) and the seed/verify posture (`Tracked` complete). `tick_assembly.rs:23-31,415-422`; `v3_state.rs:501-512`. |
| (h3) | Intake reconciliation `verify_tracked_tick_map` (`tick_assembly.rs:435`) | **CONSISTENCY-CHECK-ONLY** (for DB data, near-tautological) | Cross-checks bitmap bits ⟺ `gross>0` rows (`:442-492`); raised as `TickMapAssemblyError::InconsistentTickMap` at registration (`builder.rs:113`, `:833`). |
| (h4) | On-chain-truth verifier `verify_liquidity_map` (`rpc/liquidity_verifier.rs:165`) | **CONSISTENCY-CHECK-ONLY** (real, chain-facing) | Reads supplied words + discovers on-chain-only ticks from them (`:196-227`, `:428-455`) and compares `BitmapWord` facts (`:376-405`). Wired in production: `pool-updater/src/run.rs:1591` (pre-commit), `verify.rs:234/446`, `pool_ingress.rs:181-201` (backrun driver, `driver_boot.rs:289`), CLI/FFI. |
| (i) | Parity/golden fixtures asserting bitmap shapes | Contain (not verdicts) | `degenbot-db/tests/parity.rs:77-88` + `fixtures/parity_expected.json` (2 `tick_bitmap` blocks); `degenbot-math/.../testdata/liquidity_mapping_fixtures.json`; `degenbot-db/tests/{liquidity_updater_parity,auto_heal_matrix}.rs`; `pool-updater/tests/{sql_golden_replay,wave2_capture_replay,verification_gate_probes}.rs`; `tests/database/test_snapshot_seam_parity.py`; `tests/uniswap/test_snapshot_facade_semantics.py`; `tests/golden/data/uniswap/{v3/wbtc_weth/17600000,v4/eth_usdc/25706703}.json`. These constrain the *shape* of any survivor; they do not create a consumer. |

Nothing landed in **UNSURE**: the word-map *values* have a clean zero-production-read story, and the *key* machinery has named consumers. The one judgement call (can the verifier keep working without the table?) is resolved below with a concrete rewire, so it is not "unsure" — but see §5 for the residual coverage loss that the manager must consciously accept.

---

## 2. Q1–Q5, explicit

### Q1 — Can the solver iterate initialized ticks WITHOUT bitmap words? What does `PoolTickCoverage` imply?

**Yes.** The solver walk reads `tick_data` directly:

- `compute_tick_ranges(tick_data: &HashMap<i32, TickInfo>, current_tick, tick_spacing, _liquidity, zero_for_one) -> Option<(Vec<V3TickRangeForSolver>, usize)>` (`tick_bitmap.rs:419-427`).
- `gen_ticks_iter` collects `tick_data.keys()` filtered by direction (`:283-296`), sorts them, and interleaves **arithmetically computed** word boundaries (`first_boundary = tick_spacing*256*word_pos`, `step = ±tick_spacing*256`, `:266-276`). No bitmap map is an argument anywhere in this module.
- Signature check: `rg -n 'compute_tick_ranges\|gen_ticks_iter' rust` → callers pass `&state.tick_data` only (`v3_state.rs:816`, `v4_state.rs:920`).

`PoolTickCoverage` (`v3_state.rs:500-512`, `tick_assembly.rs:23-31`):
- `Tracked` == the snapshot supplied **complete** tick data (Db arm requires a non-empty map, `tick_assembly.rs:415-422`; Sparse chain arm seeds only the single current word → always `Sparse`).
- `Sparse` == tick data is **partial**; the walk cannot trust a region whose word key is absent from `known_bitmap_words`, and raises `SimulateSwapError::MissingTickWord` to trigger a fetch (`v3_state.rs:1328-1341` and the two in-loop guards `:1437-1450`, `:1484-1500`).

So: for **Tracked** pools `tick_data` is complete and the walk is exact; for **Sparse** pools completeness is tracked by the word-**key** set, never by bitmap **values**.

### Q2 — If bitmap words disappeared, what breaks in sparse-tick pools? Name the path.

**Nothing breaks for the values; the key set must survive.** The sparse path is:

1. `v3_simulate_swap` (`v3_state.rs:1307+`): if `coverage==Sparse` and `word_of(tick)` ∉ `known_bitmap_words` → `MissingTickWord` (`:1331-1343`).
2. Same guard at each crossing (`:1437-1450`) and in the price-derived branch (`:1484-1500`).
3. `cl_orchestration.rs:1174-1260` `stage_word_fetch_by_pool_id`/`install_word_fetch` fetch the word (via the stored `TickWordFetcher`) and merge `fetched.ticks` + `known_bitmap_words.insert(fetched.word)` (`registry.rs:676-690`).
4. Python mirror gate: `cl_companion.py:270-279` (`word not in self.tick_bitmap`).

Every one of these is a **word-KEY membership** test plus the tick list. If the bitmap **value** vanished, the walk would be unchanged. If the word-**key** set vanished, sparse pools would silently skip unfetched words and return **short outputs** (the documented failure mode: `v3_state.rs:1483-1493`, "producing a short output"). The only place a *bitmap value* is genuinely needed is the **on-chain** bootstrap enumeration (`builder.rs:952-966`, `bootstrap.rs:176-188`), which fetches the word fresh from the chain and is unaffected by any stored-map deletion.

### Q3 — DB table full consumer trace; is the bitmap half acted on?

**Writers** (all project `tick_data`): `liquidity_updater.rs` `persist_v3` (`:1482`)/`persist_v4` (`:1515`), delta path (`:1872/:1874`), shared `upsert_init_maps_impl` (`:1921`). Schema: `schema.rs:55/60`, migrate check `migrate.rs:507/512`.

**Readers**:
- `liquidity_updater.rs:402/:481` `fetch_v3/v4_liquidity_map_on_conn` → `(tick_bitmap, tick_data)`. Callers: `verify.rs:246/283/367/407` (updater pre-commit + full verify), `pool/mod.rs:262/340` (FFI verify), `cli-core/src/pool.rs:426/460` (CLI verify), `liquidity_updater.rs:598/711` (reconstitute for compute/verify).
- `snapshot.rs:385/:463` `fetch_liquidity_map(_on_conn)` → `tick_assembly.rs:250` (`bitmaps_from_db`) → `TickMapSeed.bitmaps` → `pool_ingress.rs:632-638` `verify_staged` → on-chain verifier. **Only** for `Tracked` pools and only when a verifier + `VerifyLevel != Off`.
- `read.rs:428/:464` `fetch_initialization_map` — **no production caller** (only the `DbConstruction` trait impls in `construction_io/handle.rs:90`, `adapters.rs:206`, and tests).
- FFI `degenbot-python/src/db/snapshot.rs:184-189` → Python `LiquidityMapSnapshot.tick_bitmap()` (facade; consumers in `src` are none, tests only).

**Is the bitmap half read by anything that acts on it?** Yes, but *only to verify* — never to drive engine state. The strongest consumer is `verify_liquidity_map`, which uses the words for (a) on-chain word reads + discovery of on-chain-only ticks and (b) `BitmapWord` divergence facts. That is a consistency check. **The DB value is fully redundant with `liquidity_positions`** (it is written from the same apply), so the *engine* needs nothing from the table.

### Q4 — Where is the "consistency check"? Can it keep working without the word map?

Two distinct checks exist; name them precisely:

1. **Intake reconciliation** — `verify_tracked_tick_map` (`tick_assembly.rs:435-494`), called from `liquidity_map_to_tick_info` (`:410-423`). It asserts, per supplied word, that a set bit ⟺ a `liquidity_gross>0` row at that grid tick; a contradiction is `TickMapAssemblyError::InconsistentTickMap`, which rejects registration (`builder.rs:113/833`). **This is almost certainly the user's "useful for a consistency check but not load-bearing."**
   - **Why it is (near-)tautological today:** the persisted words are *written from the same apply that produced the rows* (see Q3 writers). It can only fire on physical DB corruption or a partial/interrupted write — not on any on-chain disagreement. If the table were dropped and the words derived from `tick_data`, the check becomes *identical by construction* and can never fire.
   - **Recommendation:** if the table dies, **drop `verify_tracked_tick_map` too** (it degenerates). Keep `InconsistentTickMap` only if you knowingly want corruption detection.

2. **On-chain-truth verifier** — `verify_liquidity_map` (`rpc/liquidity_verifier.rs:165`), adapters in `degenbot-bot/bot_core/liquidity_verifier.rs`, `pool-updater/verify.rs`.
   - **How it keeps working without the word map:** callers already construct the words from `tick_data` in-process:
     - live pool path: `map_from_ticks` (`bot_core/liquidity_verifier.rs:204-235`) derives each word's bits from `tick_data` keys + the active-tick neighbourhood words.
     - FFI/Python view: `cl_snapshot_fields` (`pool.rs:2863-2898`).
     - Python: `cl_companion.py:88-104`.
     `pool-updater/verify.rs`'s `shared_map` (`:107-138`) currently reads `computed.tick_bitmap`; switching it to the same derivation is mechanical (`derive_liquidity_delta_from_computed` already shows the local derivation idiom).
   - **Residual capability loss to accept consciously:** deriving words from local `tick_data` (a) cannot detect a stored word that disagrees on-chain but contains no local initialized tick (the `BitmapWord` fact would then be vacuous), and (b) discovers on-chain-only ticks only in words that local ticks already imply — the ±2 active-tick neighbourhood in `map_from_ticks` recovers the practically important case. This is the same posture the live-pool verifier already runs with today, so it is **not a regression** — it is a *unification* onto the already-shipping behaviour. **Verdict: keep the on-chain verifier (as check), drop the persisted word half, derive words at the call sites.**

### Q5 — Reference census (before counts)

Commands (per family), all run from repo root:

```
rg -c '<symbol>' rust/ -g '*.rs'
```

**`BitmapAtWord`** (rust, occurrences per file):
`degenbot-db/src/liquidity_updater.rs:22`, `substrate/pool_ingress.rs:16`, `math/cl/liquidity_mapping.rs:16`, `substrate/tick_assembly/tests.rs:9`, `degenbot-db/src/snapshot.rs:8`, `degenbot-python/src/bot/mod.rs:6`, `bot_core/pool_builder/tests.rs:4`, `substrate/tick_assembly.rs:3`, `strategy/tests/{v4_lane_admission,v3_anchor_tick_window,sequence_deficit_probe}.rs:3` each, `pool-updater/verify.rs:2`, `degenbot-db/src/lib.rs:2`, `degenbot-python/src/db/snapshot.rs:1`.

**`tick_bitmap`** (rust, top files): `degenbot-db/src/liquidity_updater.rs:64`, `rpc/src/abi.rs:28`, `math/cl/liquidity_mapping.rs:27`, `pools/src/v3_storage_slots.rs:16`, `substrate/pool_ingress.rs:15`, `pool-updater/verify.rs:14`, `substrate/tick_assembly/tests.rs:14`, `bot_core/pool_builder/tests.rs:14`, `degenbot-python/src/bot/pool.rs:12`, `pools/src/v4_storage_slots.rs:9`, `degenbot-db/tests/parity.rs:9`, `pools/src/lib.rs:8`, `degenbot-db/src/snapshot.rs:8`, `substrate/tick_assembly.rs:7`, `degenbot-python/src/bot/mod.rs:7`, `rpc/src/tick_bootstrap.rs:6`, `pools/src/v3_state.rs:5`, `rpc/src/liquidity_verifier.rs:5`, … (tail per `rg`).

**`INITIALIZATION_MAPS|initialization_maps`**: `degenbot-db/src/liquidity_updater.rs:44`, `degenbot-db/tests/liquidity_updater_parity.rs:8`, `degenbot-db/src/rows/liquidity.rs:5`, `pool-updater/tests/wave2_capture_replay.rs:4`, `substrate/tick_assembly/tests.rs:4`, `degenbot-db/src/read.rs:4`, `pool-updater/tests/sql_golden_replay.rs:3`, `degenbot-db/tests/auto_heal_matrix.rs:3`, `degenbot-python/src/bot/mod.rs:2`, `pool-updater/tests/verification_gate_probes.rs:2`, `degenbot-db/src/{snapshot,schema,migrate}.rs:2` each, `degenbot-python/src/pool/mod.rs:1`, `substrate/pool_ingress.rs:1`, benches/facade/tests each 1-2.

**`FetchedTickWord`**: `substrate/tests/word_fetch.rs:21`, `substrate/swap_simulation.rs:16`, `degenbot-python/src/bot/pool.rs:5`, `substrate/cl_orchestration.rs:5`, solvers tests/examples 4 each, `pools/src/tick_fetch/miss.rs:3`, `pools/src/registry.rs:3`, `pools/src/tick_fetch/mod.rs:2`, `pools/src/lib.rs:2`, `pools/src/v4_state.rs:1`, `pools/src/tick_fetch/bootstrap.rs:1`.

**`known_bitmap_words`**: `pools/src/v3_state.rs:29`, `pools/src/registry.rs:23`, `pools/src/v4_state.rs:13`, `substrate/tests/word_fetch.rs:10`, `substrate/swap_simulation.rs:4`, `substrate/cl_orchestration.rs:4`, `degenbot-python/src/bot/pool.rs:3`, several tests 1-3.

**`compute_tick_ranges`**: `pools/src/tick_bitmap.rs:26`, `pools/src/v3_state.rs:10`, solvers tests, `pools/src/v4_state.rs:3`, `pools/src/int_v3_hop.rs:2`.
**`gen_ticks_iter`**: `pools/src/tick_bitmap.rs:9`, `pools/src/{v3,v4}_state.rs:2` each.
**`V3TickRangeForSolver`**: `pools/src/v3_state.rs:7`, `pools/src/tick_bitmap.rs:7`, `pools/src/v4_state.rs:4`, `pools/src/lib.rs:1`, one solver test.
**`apply_liquidity_mapping_update`**: `math/cl/liquidity_mapping.rs:10`, `degenbot-db/src/liquidity_updater.rs:7`, `degenbot-python/src/db/liquidity_updater.rs:3`, `substrate/pool_ingress.rs:3`, `degenbot-python/src/concentrated_liquidity_math/lib.rs:1`, `pools/src/{v3_state,registry}.rs:1` each (doc refs).

Python (`rg -n 'BitmapAtWord|tick_bitmap' src/`): `BitmapAtWord` in `concentrated/types.py:26`, `cl_companion.py`, `v3/v4_snapshot.py`, `v3_types.py:61`, `v4_types.py:30`, `v3_liquidity_pool.py`, `snapshot_readers.py`, `builders/tick_data_fetcher.py`, `bot/_bot.py`. Python source contains **no `initialization_maps` reference at all** (`rg -n 'initialization_maps' src/ -g '*.py'` → empty): the table is Rust-only.

---

## 3. Deletion inventory (for BW3) — what dies together, what must NOT, what order

### Die together (one coherent cut)
- **DB**: table DDL + names (`schema.rs:55,60`), migration presence checks (`migrate.rs:507,512`), `snapshot::LiquidityMap.tick_bitmap` field (`snapshot.rs:50`) and its SELECT blocks (`snapshot.rs:410-426,490-508`), `liquidity_updater::fetch_v3/v4_liquidity_map` bitmap half (the SELECTs at `:409-427,488-507`), `read.rs` `fetch_initialization_map` / `fetch_managed_initialization_map` + `rows/liquidity.rs` `InitializationMapRow` / `ManagedPoolInitializationMapRow`, the `DbConstruction` trait methods (`construction_io/mod.rs:113,134`) + their impls, `upsert_*_initialization_maps*` + `delete_stale_*_init_maps*` + `upsert_init_maps_impl`, `LiquidityDelta.tick_bitmap/drained_words` (`liquidity_updater.rs:139-145`), `ComputedLiquidityUpdate.tick_bitmap` (`:202`).
- **Apply math**: `LiquidityMapUpdateResult.tick_bitmap` (`liquidity_mapping.rs:71`), `flip_tick` (`:106`), the bitmap half of `apply_liquidity_mapping_update` (`:186`), the `BitmapAtWord` math type (`:41`, incl. `block`), `LiquidityMapUpdateResult` if unused, and the `block`-stamp parity fixtures in `testdata/liquidity_mapping_fixtures.json`. Replace DB/ingress callers with the existing tick-only `apply_liquidity_to_tick_range` (`registry.rs:798/995`).
- **Ingress/updater plumbing**: `pool_ingress.rs` `db_bitmap_to_apply`/`apply_bitmap_to_db` (`:913,:953`) and the `tick_bitmap` field of `backfill_liquidity_map`'s return (`:905-908`), `pool-updater/run.rs:936/1005` `tick_bitmap` clones, `pool-updater/verify.rs:107-138` word half.
- **Type family (candidates)**: `degenbot-db::snapshot::BitmapAtWord`, `degenbot-math::cl::liquidity_mapping::BitmapAtWord`, the Python `BitmapAtWord` model (`concentrated/types.py:26`) + `_ffi` pyi signatures, `snapshot_readers._liquidity_map_from_raw` bitmap branch.
- **Verify rewire (same wave)**: `tick_assembly.rs:250,272-277,415-423,435-494` (`bitmaps_from_db`, the bitmap arm of `liquidity_map_to_tick_info`, `verify_tracked_tick_map`), `TickMapSeed.bitmaps` (`planning.rs:113`) + `verify_staged`'s use of it (`pool_ingress.rs:632-638`) → derive words from `seed.ticks` (mirror `bot_core/liquidity_verifier.rs:204-235`).

### Must NOT die together (named survivors)
- `known_bitmap_words` (`v3_state.rs:512`, `v4_state.rs:369`), `word_of` (`v3_state.rs:630`), `seed_known_bitmap_words` (`:638`), `mark_bitmap_words_known` (`registry.rs:564,692,896`), `merge_tick_word` (`registry.rs:676,881`).
- `get_tick_word_and_bit_position` **word half** (`math/…:89`) — used for word KEYS in `rpc/tick_bootstrap.rs:93/153`, `bot_core/pool_builder/builder.rs:863/905/936/1227`, `liquidity_updater.rs:1606`, `pool_ingress.rs:856`.
- On-chain bootstrap: `TickBootstrapRpc` (`tick_fetch/bootstrap.rs`), `fetch_tick_bitmap` (`choreography.rs:363,393`), Python `_fetch_v3/_fetch_v4` (`tick_data_fetcher.py:145,190`), `FetchedTickWord`, `TickWordFetcher`, `stage_word_fetch_by_pool_id`/`install_word_fetch`/`tick_fingerprint` (`cl_orchestration.rs:1149-1260`), storage-slot math `tick_bitmap_mapping_slot` (`v3_state.rs:195`) and `v3/v4_storage_slots.rs` constants — these are **on-chain slot/slotting math**, orthogonal to the word map.
- `compute_tick_ranges` / `gen_ticks_iter` / `gen_ticks` / `V3TickRangeForSolver` / `TickAlongPath` (`tick_bitmap.rs`) — the solver walk.
- `PoolTickCoverage`, `LiquidityAtTick`, `TickInfo`.

### Order
1. **BW1** (hardening, no behaviour change): make the reorg journal's full prior-row capture + rewind==replay property airtight. This is the prerequisite for touching anything near `tick_data` mutation; the bitmap removal edits the same region.
2. **BW2** (projection/stamp treatment for survivors): decide the fate of the word **stamp** (`BitmapAtWord.block`) and the **projection** discipline for whatever survives (derive-at-read), i.e. rewrite `verify_staged`/`shared_map` to derive words from ticks. Ship a `derived_words == old_words` differential across the parity corpus to prove the derivation is exact before deleting the source.
3. **BW3** (deletion): execute §3 "die together" once BW2's differential is green and the derived-verify coverage loss is accepted.

---

## 4. Shaped follow-up chunks

### BW1 — Exact rewind: journal full prior rows + `rewind == replay` + edge battery
- **Scope**: Ensure the reorg journal captures *full prior rows* (both tick rows and any surviving word stamp) so a rewind is byte-equal to never-having-applied. Assert the property `state(after rewind of [A..B]) == state(before any apply)` on a randomized event battery (in-range/out-of-range, gross→0 drain, re-init, upper/lower net signs, same-block net-zero churn, backfill replay `block <= initial_state_block`).
- **Why now**: BW3 edits `apply_liquidity_to_tick_range`/`merge_tick_word` — the same mutation surface the journal guards (`registry.rs:768-863`). Proving rewind==replay *first* makes the deletion a pure subtraction.
- **Risk note**: `apply_liquidity_update` deliberately does NOT journal `known_bitmap_words` (`registry.rs:561-564` "miss-detection fact, not a tick-map mutation"). If BW2/BW3 changes how known-words are derived, a reorg could leave the known set diverged. **BW1 must pin whether the known set participates in rewind; today it does not.**
- **Size**: ~M (one crate `degenbot-pools` + substrate tests; property tests, no production signature churn).

### BW2 — Word stamps / projection treatment for survivors
- **Scope**: (a) Delete or repurpose `BitmapAtWord.block` (zero production readers — matrix (b)); (b) rewrite the two verify call sites to **derive** words from `tick_data` (`pool-updater/verify.rs:shared_map`, `pool_ingress::verify_staged`) exactly as `bot_core/liquidity_verifier.rs:map_from_ticks` does, and add a differential test proving derived-words == DB-words over `fixtures/parity_expected.json` + the golden V3/V4 JSON.
- **Risk note**: the derived verifier loses the "stored word differs on-chain but no local tick" detection. That must be **explicitly accepted in the task's acceptance criteria**; if the manager wants that capability, the table must survive (verdict flips to keep-with-justification). Name the loss in the doc.
- **Size**: ~M (two crates + fixtures).

### BW3 — Deletion wave per the matrix
- **Scope**: execute §3 "die together"; update every parity/golden fixture in §1(i) and the census families in §2/Q5 to the new shapes.
- **Order guard**: only after BW2's differential is green; re-run `just test-rust-nextest` + `just test-python` whole.
- **Risk note**: the widest blast radius is the `degenbot-db` + `degenbot-substrate` + `degenbot-pool-updater` + `degenbot-python` schema/FFI seam. The **stale-parity-db fixtures** (`aave_parity.db`, `parity.db`, `liquidity_updater_*_initial.db`, `parity_expected.json`) encode the current table shape and MUST be regenerated in lock-step, or the parity gate fails on a *shape*, not a behaviour, diff.
- **Size**: ~L (schema + 6 crates + Python `_ffi` + fixtures).

---

## 5. The "consistency check" (Q4) answer in one line

The check the user means is almost certainly **`verify_tracked_tick_map` (`tick_assembly.rs:435`), the intake reconciliation**: a bitmap-bit ⟺ `gross>0`-row invariant at pool registration. For DB-written data it is (near-)tautological, so if the table dies it should die with it; the *real* on-chain consistency check (`verify_liquidity_map`) already has a derive-words-from-`tick_data` mode in production (`bot_core/liquidity_verifier.rs:204`), so the verifier keeps working once callers stop reading `computed.tick_bitmap` and start deriving. **Do not keep the word map "for the check": the check can be rebuilt from `tick_data`, at a named, accepted coverage cost.**

---

## 6. Found, not fixed

1. **`DbConstruction::fetch_initialization_map` / `fetch_managed_initialization_map`** (`construction_io/mod.rs:113,134`; impls `handle.rs:90/122`, `adapters.rs:206/235`) have **no production caller** — the builder lane now reads via `TickMapDb::fetch_liquidity_map`. Dead API surface; deletion-wave candidate.
2. **Two parallel liquidity-apply implementations**: engine `apply_liquidity_to_tick_range` (`tick_bitmap.rs:640`, tick-only) vs updater `apply_liquidity_mapping_update` (`liquidity_mapping.rs:186`, tick+bitmap). The bitmap catch is the only thing making them two; convergence is a BW3 outcome, flagged here.
3. **Delta-path verify coverage gap** (not a bug I will fix): `pool-updater/run.rs:936/1005` builds `ComputedLiquidityUpdate.tick_bitmap` from **`delta.tick_bitmap` (dirty words only)**. The pre-commit verifier's word reads/discovery therefore only cover the chunk's dirty words on the delta path, versus the full map on the full path (`run.rs:1591` → `verify.rs:348-423`). Recorded, not fixed.
4. **`BitmapAtWord.block` is stamped through the whole DB-updater path** (`flip_tick` `:123`) and carried/persisted nowhere useful; `snapshot::BitmapAtWord` doesn't even have the field. Pure dead weight; matrix (b). **Cross-reference:** prior task WOXT5Z (`docs/ergo-results/woxt5z-result.md`) *deliberately* hardened this stamp contract (removed the `= 0` defaults, filled real blocks, ruled `0` a legal stamp). That work is orthogonal to this verdict - it made the stamp *correct*, but "correct and never read" is still LEGACY-DEAD. BW2/BW3 must explicitly retire the WOXT5Z contract (and its `types.py:26` required-`block` decision) if the stamp dies, rather than silently dropping it.

---

## 7. Verification command appendix (the load-bearing ones)

```bash
# zero production readers of the block stamp
rg -n '\.block\b' rust/crates/foundation/degenbot-math/src/cl/liquidity_mapping.rs   # writes :123; rest are test asserts
rg -n 'entry\.block|bw\.block|word\.block' rust -g '*.rs'                             # no production bitmap-block read

# the live pool structs hold NO bitmap values
rg -n 'bitmap' rust/crates/foundation/degenbot-pools/src/v3_state.rs                  # known_bitmap_words only
rg -n 'bitmap' rust/crates/foundation/degenbot-pools/src/v4_state.rs                  # known_bitmap_words only

# solver reads tick_data only
rg -n 'pub fn compute_tick_ranges|pub fn gen_ticks_iter' rust/crates/foundation/degenbot-pools/src/tick_bitmap.rs

# FetchedTickWord has no bitmap field
sed -n '52,59p' rust/crates/foundation/degenbot-pools/src/tick_fetch/miss.rs

# writers of the table (all in liquidity_updater)
rg -n 'INITIALIZATION_MAPS|initialization_maps' rust

# DB table readers
rg -n 'fetch_v3_liquidity_map|fetch_v4_liquidity_map' rust -g '*.rs'

# the derivation already exists (proof the verifier can be rewired)
sed -n '204,235p' rust/crates/engine/degenbot-bot/src/bot_core/liquidity_verifier.rs
sed -n '2863,2898p' rust/crates/shells/degenbot-python/src/bot/pool.rs
```

---

## 8. Sign-off

**Matrix in brief**

- In-memory word-map **values** (`BitmapAtWord.bitmap`): **CONSISTENCY-CHECK-ONLY** (manager-
  corrected; was LEGACY-DEAD). Readers: `verify_tracked_tick_map` intake gate
  (`tick_assembly.rs:452/:466`), updater `verify.rs:131`, FFI export `db/snapshot.rs:186`,
  assembly passthroughs (`tick_assembly.rs:275/:320/:390`, `pool_ingress.rs:461`).
  Zero ENGINE/SOLVER logic readers. Derived projection of `tick_data`.
- `BitmapAtWord.block` stamp: **LEGACY-DEAD** (writes + test asserts only).
- DB `initialization_maps` / `managed_pool_initialization_maps` (writes + the `tick_bitmap` half of `fetch_*`): **CONSISTENCY-CHECK-ONLY**.
- Updater `tick_bitmap` plumbing (`LiquidityDelta`/`ComputedLiquidityUpdate`/`verify.rs`/`run.rs`): **CONSISTENCY-CHECK-ONLY**.
- Python `BitmapAtWord` type + `tick_bitmap` **values**: **CONSISTENCY-CHECK-ONLY** (manager-
  corrected; same readers via FFI — the intake gate consumes them); Python/engine
  `tick_bitmap` **keys** / `known_bitmap_words`: **LOAD-BEARING**.
- `tick_bitmap.rs` solver walk (`compute_tick_ranges`/`gen_ticks_iter`/`V3TickRangeForSolver`): **LOAD-BEARING**, zero word-map dependency.
- Staged word fetch + `tick_fingerprint` + on-chain bootstrap: **LOAD-BEARING** (word key + ticks; on-chain bitmap is fetched fresh).
- `verify_tracked_tick_map` intake reconciliation: **CONSISTENCY-CHECK-ONLY** (near-tautological for DB data); `verify_liquidity_map` on-chain truth: **CONSISTENCY-CHECK-ONLY** (real; rewireable to derived words).

**Top-3 riskiest deletion candidates / evidence gaps**

1. **DB table `initialization_maps`** — drop requires rewiring `verify.rs:shared_map` + `pool_ingress::verify_staged` onto derived words; the residual gap (on-chain-only word detection) is a *capability* decision, not a code fact. Evidence gap: **none on "no engine consumer"** — gap is the manager's appetite for the reduced verifier coverage.
2. **`tick_assembly::verify_tracked_tick_map`** — currently a *registration gate* (`builder.rs:833`). Dropping it removes a (weak) corruption tripwire. Evidence gap: whether any operator relies on the `InconsistentTickMap` alert in practice (I found only the code path, no telemetry consumer).
3. **`degenbot-math::cl::liquidity_mapping::BitmapAtWord` + `flip_tick`** — used by the DB-updater apply loop; deletion must re-point `liquidity_updater.rs:1453`/`pool_ingress.rs:890` to the tick-only `apply_liquidity_to_tick_range`. Evidence gap: parity byte-identity between the two apply impls is asserted per-module but there is **no single cross-implementation differential test**; BW2 must add one before BW3 deletes.

**Q1–Q5 one-liners**: Q1 yes — the walk reads `tick_data` keys and computes boundaries arithmetically; `Tracked` = complete tick_data, `Sparse` = partial gated by the word-key set. Q2 only the word-**key** set matters (`v3_simulate_swap` `MissingTickWord` gates, `merge_tick_word`); values are unused. Q3 writers project `tick_data`; readers are verify/intake/facade only; nothing acts on the bits for engine behaviour. Q4 the check is `verify_tracked_tick_map` (drop with the table) + `verify_liquidity_map` (keep, derive words in-process as production already does). Q5 per-crate/per-symbol counts in §2/Q5.

**Result doc**: `docs/ergo-results/e5tk7w-result.md`. No code/docs other than this file touched; no commits; no tree changes observed from the concurrent agent during the audit.

---

## Manager review correction (2026-10-11)

The audit's headline claimed the word-map VALUES have "zero logic readers" / "nothing in
production branches on them". Adversarial review found the counter-example:

```text
rust/crates/foundation/degenbot-substrate/src/tick_assembly.rs:452/:466
    if !entry.bitmap.bit(bit) { return Err(TickMapAssemblyError::InconsistentTickMap { .. }) }
```

`verify_tracked_tick_map` (the "Tracked intake reconciliation" this audit's Q4 correctly
identified) branches on the bits at the registration gate (`builder.rs:833`). The verdicts above
are corrected in place: values are CONSISTENCY-CHECK-ONLY, not LEGACY-DEAD.

Consequence for the waves: deleting the values requires an explicit decision about this gate.
Options, in the manager's preference order:
1. Rewire the gate to compare tick_data against CHAIN-fetched words only (that is already
   `verify_liquidity_map`'s job — "on-chain truth"), then delete the snapshot-internal
   bit<->row tautology check. The snapshot's internal consistency is then checked by
   schema/decode validation instead.
2. Keep the gate and keep the values solely for it until (1) lands — i.e. values die LAST.
3. Drop the gate outright (reject: silently losing an input-validation gate is not acceptable
   without the user's explicit sign-off — this is the "consistency check" the user called useful).

BW2/BW3 task bodies must carry this decision explicitly. Nothing in this correction changes the
LOAD-BEARING verdicts (word keys, solver walk, staged fetch) or the DB-table drop-eligibility.
