# 7DSMFQ (BW2, epic U2XZSD): snapshot intake gate rewired to CHAIN truth

STATUS: **GREEN for the assigned chunk; one gate PENDING on a FOREIGN in-flight edit.**

The snapshot-internal bit<->row tautology (`verify_tracked_tick_map`'s old body) is retired and
replaced by a chain-word reconciliation (consolidating with `verify_liquidity_map`'s
on-chain-truth role). Snapshot internal consistency is no longer an intake gate. The typed-error
contract is preserved and extended (`ChainUnavailable`); every error-mapping site is updated. The
last LOGIC reader of `BitmapAtWord.bitmap` is gone. Values/tables untouched (BW3's job). No
commits, no `.ergo` state changes; tree left dirty for the manager's ladder.

`just lint-rust-check` is the one red gate — **both** its errors are in
`rust/crates/engine/degenbot-bot/src/hosted_sources.rs`, which a concurrent worker was rewriting
into a pubsub-runtime implementation *during this run*. See §7/§8. Every file I touched is
individually clippy- and rustfmt-clean, and the gate was **EXIT=0** on my snapshot before that
foreign rewrite landed.

---

## 1. What the inherited (uncommitted) diff actually contains

Verified in the tree with `git diff` (the brief's summary was accurate). Shapes + `file:line`
(final, post-rustfmt):

| site | file:line | shape |
|---|---|---|
| new typed error | `degenbot-substrate/src/tick_assembly.rs:111` | `TickMapAssemblyError::ChainUnavailable` ("...requires a chain transport for on-chain word reconciliation, but none is wired") |
| reshaped error | `tick_assembly.rs:120` | `InconsistentTickMap` retained; semantics now "snapshot rows disagree with CHAIN words" |
| chain reconciliation | `tick_assembly.rs:413` / `:437` | `reconcile_db_seed_v3/v4(seed, …, block, chain)`; `ChainUnavailable` at `:424` / `:449` |
| compare core | `tick_assembly.rs:497` | `verify_tracked_tick_map(chain_words: &HashMap<i64,U256>, tick_data: &HashMap<i32,TickInfo>, spacing)` — row side + bitmap side, both against CHAIN words |
| helpers | `tick_assembly.rs:559/572/588/604/615/626` | `row_words`, `fetch_chain_words_for_rows`, `reconcile_tracked_seed_with_chain`, `pub seed_row_words`, `pub reconcile_tracked_rows`, `word_representative_tick` |
| sync wrappers | `tick_assembly.rs:319/385` | `assemble_v3/v4_tick_map` Db arm now calls the reconcile before returning the seed |
| tautology removed | `tick_assembly.rs:472` | `liquidity_map_to_tick_info` no longer calls `verify_tracked_tick_map`; now returns `Option<…>` |
| registration gate | `bot_core/pool_builder/builder.rs:859/902` (defs `:932/:958`, `tick_word_i16` `:987`) | the pipeline position at `:833`-area is kept; what it validates is now chain words via `ConstructionIo` |
| python error map | `shells/degenbot-python/src/db/mod.rs:176` | `ChainUnavailable → PyRuntimeError` |
| ingress error map | `pool_ingress.rs:259-266` (`:265`) | `From<TickMapAssemblyError> for IngressDecline`: `ChainUnavailable | InconsistentTickMap → Self::Db` |
| ingress test renamed | `pool_ingress.rs:1551` | `internal_inconsistency_is_no_longer_an_intake_gate` (was `off_never_proceeds_past_a_self_contradictory_tracked_map`) |
| separate concern | `hosted_sources.rs` + `bot/engine/{mod,register,strategy}.rs` | engine-lifetime `HostedSources` ownership + clean head-edge shutdown — **not** the intake-gate rewire (§8) |

## 2. Completeness assessment of the inherited work

The inherited design is sound and, for the core rewire, complete: the chain comparison, the typed
`ChainUnavailable` refusal, the error-mapping arms, and the tautology removal were all present and
correct. What it had **never** been through is a gate run — and it did not survive one. Missing
pieces I closed:

1. **Two substrate tests contradicted acceptance (c).** `v3_store_miss_db_hit_non_empty_returns_tracked_ticks`
   and its V4 twin drove a non-empty `Tracked` Db seed with `chain=None`; under the new gate they
   must be refused, so they panicked. Fixed by wiring a chain word (V3) and a new word-aware fake
   `WordMapChainRpc` (V4 — the `-10`/`10` row set spans words `-1` and `0`, which a single-word
   fake cannot answer).
2. **Two shell tests + one builder test-pair + one pytest case assumed the retired behaviour.**
   `assemble_v3_tick_map_returns_tracked_after_snapshot_load` and the T3 rejection test called
   `assemble_v3_tick_map(..., io=None)` and could no longer pass / no longer rejcted for the
   asserted reason. Reshaped (see §3). The two `build_v3_db_hit_*` tests seeded no `tick_bitmap`
   response ("if the chain arm were reached it would error") — the gate now *does* reach chain.
3. **Two clippy errors** from the inherited edits (`liquidity_map_to_tick_info`'s now-dead `Result`
   wrapper; an always-`Some` helper), **one comment-hygiene token** (`AGREES`), and rustfmt drift in
   `tick_assembly.rs`.
4. **Stale claims** in `pool_ingress.rs` (`:220`, `:383-389`, `:431`) and `builder.rs` (`:115-119`,
   `:835`) that still said a self-contradictory Tracked map aborts. Reworded to chain truth.

Also repaired (inherited, production): `bot/engine/strategy.rs` — `boot_hosted_arms` /
`boot_hosted_head_feed` returned `Option<HostedSources>` but only ever returned `Some`
(clippy `unnecessary_wraps`); the wrapper is dropped and the sole call site wraps in `Some`.

## 3. Edge battery

Substrate (`degenbot-substrate/src/tick_assembly/tests.rs`):

| case | test (:line) | expected | result |
|---|---|---|---|
| corrupt: chain bit without row | `v3_tracked_chain_bit_without_row_rejected_at_intake` (:740) | typed `InconsistentTickMap` | PASS |
| corrupt: row without chain bit | `v3_tracked_row_without_chain_bit_rejected_at_intake` (:755) | typed `InconsistentTickMap` | PASS |
| consistent snapshot ⇔ chain | `v3_tracked_consistent_snapshot_accepted` (:770) | `Some`, `Tracked` | PASS |
| gross=0 row ignored | `v3_tracked_gross_zero_row_is_ignored_by_chain_truth` (:797) | `Some`, `Tracked` | PASS |
| chain bit over a gross=0 row | `v3_tracked_chain_bit_over_gross_zero_row_rejected` (:817) | typed `InconsistentTickMap` | PASS |
| chain-fetch unavailable | `v3_tracked_chain_unavailable_is_a_typed_failure` (:833) | `ChainUnavailable` | PASS |
| chain transport errors | `v3_tracked_chain_fetch_error_is_a_typed_failure` (:847) | `Chain(Rpc)` | PASS |
| V4 corrupt (bit without row) | `v4_tracked_chain_bit_without_row_rejected_at_intake` (:865) | typed `InconsistentTickMap` | PASS |
| V4 consistent | `v4_tracked_consistent_snapshot_accepted` (:894) | `Some`, `Tracked` | PASS |
| internal inconsistency is not a gate | `pool_ingress.rs:1551` | stages (no refusal) | PASS |

Shell boundary (`degenbot_rs bot::tests`, via new offline `io` helper
`offline_v3_tick_word_io` at `bot/mod.rs:2958`):

| case | test (:line) | result |
|---|---|---|
| Db hit, snapshot bitmap contradicts rows, chain agrees → admitted | `assemble_v3_tick_map_chain_truth_gate` case 3 (:3311) | PASS, `tracked` |
| chain disagrees (empty word 0) → typed `ValueError` | same test, case 2 | PASS |
| no chain transport → typed `RuntimeError` | same test, case 1 | PASS |
| Db hit consistent → `tracked` | `assemble_v3_tick_map_returns_tracked_after_snapshot_load` (:3186) | PASS |
| builder registration gate (Db hit, chain word agrees) | `build_v3_db_hit_yields_tracked` (`pool_builder/tests.rs:928`) | PASS |
| builder stamps the DB liquidity clock | `build_v3_db_hit_stamps_tick_data_block_at_db_liquidity_update_block` | PASS |

Python boundary (`tests/test_ffi_checked_words.py:157`,
`test_assemble_v3_tracked_snapshot_without_chain_transport_refused`): an internally-inconsistent
`Tracked` snapshot **and** a self-consistent one both refuse with a typed `RuntimeError` naming
the missing chain transport — never a `ValueError` claiming a snapshot contradiction, never a
silent pass.

## 4. Negative probe (verbatim)

Flip (one edit, in `reconcile_tracked_seed_with_chain`, `tick_assembly.rs`):

```rust
    let _chain_words = fetch_chain_words_for_rows(&seed.ticks, tick_spacing, fetch)?;
    // NEGATIVE PROBE (temporary): the chain comparison is disabled.
    Ok(())
```

Run (direct exit, `cargo nextest run -p degenbot-substrate -E 'test(v3_tracked_row_without_chain_bit_rejected_at_intake)'`):

```
PROBE_EXIT=100
        FAIL [   0.008s] (1/1) degenbot-substrate tick_assembly::tests::v3_tracked_row_without_chain_bit_rejected_at_intake
  stderr ───
    thread 'tick_assembly::tests::v3_tracked_row_without_chain_bit_rejected_at_intake' (539089) panicked at crates/foundation/degenbot-substrate/src/tick_assembly/tests.rs:762:10:
    a gross>0 row without its chain bit must be rejected at intake: Some(({10: TickInfo { liquidity_gross: 1000000, liquidity_net: 1000000, block: 0 }}, Tracked))
```

Reverted byte-for-byte from a pre-probe copy: `sha256sum` restored to
`746bb9b88787826f143413a82dc0747e927db3a4e1f8444db0af5ffb7f7b63c4`, marker count 0,
`rustfmt --check` clean. (A first probe attempt — an early `return Ok(())` inside
`verify_tracked_tick_map` — was abandoned because this workspace builds with `-D warnings` and the
early return tripped `unused_variables`; it was reverted before the recorded probe.)

## 5. Gate exits (each captured directly)

| gate | exit | note |
|---|---|---|
| `just test-rust-nextest` | **0** | 4767 run: 4767 passed, 29 skipped (fp `4b82f0d5`) |
| `just lint-rust-check` | **101** | 2 errors, both FOREIGN (`hosted_sources.rs`) — see §7. EXIT=0 on my snapshot pre-rewrite |
| `just lint-python-check` | **0** | ruff + ty both "All checks passed!" |
| `pytest tests/pool_companion tests/database tests/uniswap tests/test_ffi_checked_words.py` | **0** | 247 passed, 1 skipped |
| `just gen-stubs --check` | **0** | committed stubs match generator output |
| `just lint-comment-hygiene` | **0** | |
| `rustfmt --check --edition 2021 --config skip_children=true -- <7 files I touched>` | **0** | |
| `rustfmt --check … -- <all dirty .rs>` | **1** | single diff in `hosted_sources.rs:285` (foreign region) |
| `ruff format --check -- tests/test_ffi_checked_words.py` | **1** | file already unformatted **at HEAD**; `tests/` is outside the repo's `ruff format src/` scope — see §7 |
| `just rebuild-if-stale` | **0** | build 2260 (`17f057f1a3befe43`); receipt fresh |

## 6. Counts delta

| metric | HEAD | now |
|---|---|---|
| `.bitmap.bit(` logic-reader sites (`git grep -n '\\.bitmap\\.bit(' HEAD -- '*.rs'`) | 2 — both in `verify_tracked_tick_map`, both reading the `BitmapAtWord.bitmap` VALUE | **0** |
| `InconsistentTickMap` occurrence-lines | 16 | 18 |

The two retired readers were the tautology's row side and bitmap side. The `+2` is the *variant
being repurposed*, not new snapshot-internal logic: `tick_assembly.rs` gains the
`word_representative_tick` defensive `ok_or` (`:626`) and `builder.rs` gains `tick_word_i16`
(`:987`) — both pure width checks, both reachable only for an out-of-range word position.

Value-flow note (honesty, not a logic reader): `BitmapAtWord.bitmap` is still *mirrored* by
`bitmaps_from_db` into `seed.bitmaps`, and `pool_ingress`'s chain-SAMPLE verifier derives ticks
from those values (`degenbot-rpc::liquidity_verifier::bitmap_ticks` — its locals are plain `U256`,
never a `.bitmap` field access). That is the pre-existing on-chain-truth sample path, distinct
from the retired tautology. No `BitmapAtWord.bitmap` VALUE is read for logic anywhere.

## 7. Deliberately left / PENDING

- **`just lint-rust-check` — PENDING (FOREIGN).** All three errors are in
  `rust/crates/engine/degenbot-bot/src/hosted_sources.rs`, written by a concurrent worker during
  this run: `mint` (:151) now calls `build_pubsub_runtime().expect(...)` without a `# Panics` doc
  section, `spawn_head_task` (:314) is 107 lines, and its test-only `capture::lock` (:1059) carries
  a `double_must_use`. Retried 5× over ~4 min; unchanged. Isolated proof they are not mine:
  `cargo clippy -p degenbot-substrate --all-targets -- --deny warnings` = **EXIT 0**, and every
  clippy error under `cargo clippy -p degenbot-bot --all-targets` resolves to `hosted_sources.rs`
  (none in `builder.rs` / `pool_builder/tests.rs`).
  I did **not** touch that file (its fingerprint kept moving; a `#[expect(too_many_lines)]` I add
  today could become an *unfulfilled* expectation the moment its author splits the fn, which this
  workspace's `-D warnings` would then reject).
- **`rustfmt` authority — one foreign diff.** `hosted_sources.rs:285` (the new
  `build_pubsub_runtime`). Every file I touched passes. I ran `rustfmt` (write mode) once early on
  `hosted_sources.rs` to clear inherited drift; the concurrent rewrite has since superseded most of
  that region.
- **`ruff format --check` on `tests/test_ffi_checked_words.py`** fails identically at `HEAD`
  (pre-existing long lines + function-local imports; `tests/` is not in `just fmt-check-python`,
  which is `ruff format --check src/`). I did not reformat the file — that would be an unrelated
  whole-file diff. My added lines introduce no new class of finding.
- **`pool_ingress` sample policy hole (pre-existing, noted not fixed).** With
  `VerifyLevel::Off`, `verify_staged` returns early, and with no verifier wired even `Bootstrap`
  short-circuits — so a `Tracked` map can stage unverified. That is the documented
  operator-declaration policy (`Set the chain-sample verification policy…`), unchanged by this
  chunk; the brief's acceptance (c) targets the intake gate, which is now `ChainUnavailable`-loud.
- **Values untouched** (BW3 deletes them) — `BitmapAtWord` / the DB tables are intact.
- `just test-python` (whole suite) and `just fmt-check` (whole-tree) were **not** run — not mine.

## 8. Foreign tree activity (manager's eyes)

- `rust/crates/engine/degenbot-bot/src/hosted_sources.rs` — rewritten mid-run into a dedicated
  `tokio` single-worker pubsub runtime (`build_pubsub_runtime`, `head_ws_provider`,
  `pubsub_runtime: Option<Runtime>`), plus a matching `Option<Arc<dyn Provider>>` return from
  `spawn_head_task`. Likewise `rust/crates/foundation/degenbot-rpc/src/head_watch.rs`
  (`HeadWatch::subscribe` now takes a `tokio::runtime::Handle`). At 18:50-18:52 the tree was
  **temporarily uncompilable** (`E0061`/`E0308` in `hosted_sources.rs`); it settled by 18:52:57.
  These two files are *not* part of the BW2 intake-gate rewire.
- `rust/crates/shells/degenbot-python/src/bot/engine/{mod,register,strategy}.rs` +
  `hosted_sources.rs` carry an **engine-lifetime `HostedSources` ownership** change (Drop order,
  `watch` shutdown, `head_edge_stopped` flag, two teardown tests). This was present in the 9-file
  inherited diff; it is unrelated to the snapshot gate. I repaired one clippy error in
  `strategy.rs` (§2) and left the rest alone.
- No other foreign files observed. I added no files.

---

### Files changed by me this round

`tick_assembly.rs`, `tick_assembly/tests.rs`, `pool_ingress.rs`,
`bot_core/pool_builder/builder.rs`, `bot_core/pool_builder/tests.rs`,
`shells/degenbot-python/src/bot/mod.rs`, `shells/degenbot-python/src/bot/engine/strategy.rs`,
`shells/degenbot-python/src/db/mod.rs` (touched), `tests/test_ffi_checked_words.py`.
No commits. `.ergo/plans.jsonl` untouched.
