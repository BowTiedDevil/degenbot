# FKEN7K (BW1, epic U2XZSD): exact rewind — journal full prior rows; `rewind == replay`

STATUS: **GREEN.** `restore_before_block(target)` now leaves V3/V4 pool state bit-identical to a
fresh replay of every event strictly before `target` — values, `block` stamps, scalars, both
clocks. The `restored_block` re-stamp convention is retired; `rg restored_block rust/crates`
returns nothing. Tree left dirty, no commits, `.ergo/plans.jsonl` untouched.

## Shape (from the diff)

`TickBefore` (`degenbot-pools/src/state_history.rs`) grew from values-only to the full prior row:

```rust
pub struct TickBefore {
    pub liquidity_gross_before: Option<alloy::primitives::U128>,   // None => tick did not exist before
    pub liquidity_net_before: i128,
    pub block_before: u64,   // the prior row's own last-mutation stamp (read only when gross is Some)
}
```

Capture (4 sites, `degenbot-pools/src/registry.rs` — the V3 and V4 `apply_swap` /
`apply_liquidity_update` `journaled_priors` builders, one added line each):

```rust
block_before: prior.as_ref().map_or(0, |p| p.block),
```

Restore twin (`v3_state.rs` ~1084, `v4_state.rs` ~790) — the `restored_block` capture, its
rationale comment, and the re-stamp die; the row's own stamp comes back:

```rust
// was: let restored_block = self.tick_data_block;  ...  block: restored_block,
block: tick_before.block_before,
```

The clock rewind (`update_block_before` / `tick_data_block_before`), the scalar-priors write-back
(ADR-004 tick-only skip), the `state_nonce` bump, and the tick-range cache invalidation are
unchanged. `stateview_feasibility_probe.rs` (the only other constructor) follows the shape.

## The product: `rewind == replay`

`v3_state::exact_rewind_tests::rewind_equals_replay_v3` and its V4 twin take a randomized script
of 1–5 events on strictly-increasing blocks (`Swap` with/without a tick prior — including tick
*creation* — and Mint/Burn over in-range, out-of-range and absent-boundary ranges), apply it to a
genesis-anchored pool, then for **every legal target** (each event block, each block+1, the
registration block, `newest+1`) compare against a fresh replay of the events strictly before that
target.

The oracle comparison is ordered so the *stamp* is checked first and names itself:

```
assert_same_state: per-tick STAMP mismatch -> per-tick VALUE mismatch -> row presence ->
                   sqrt_price_x96 -> liquidity -> tick -> update_block -> tick_data_block
```

Failure detail carries `target=` and the full script, and proptest shrinks to the minimal kind
sequence.

## Edge-case battery (both twins where applicable)

| case | test (V3; V4 twin is byte-identical modulo the struct) | what it pins |
|---|---|---|
| a. multi-block popped range | `multi_block_popped_range_rewinds_to_the_pre_target_state` | events 97..100, rewind to 98: rows + both clocks land on 97 |
| b. tick created inside the popped range | `tick_created_inside_the_popped_range_is_removed_on_rollback` | `None` prior => removed (not re-stamped) |
| c. same tick in several popped blocks | `tick_touched_in_multiple_popped_blocks_restores_the_exact_pre_range_row` | oldest prior wins, seed row AND seed stamp (0) return |
| d. tick-only popped range | `tick_only_popped_range_skips_the_scalar_write_back_and_keeps_the_price_clock` | ADR-004 scalar skip preserved; slot0 head untouched |
| e. clock priors `None` | (same test as d) | `update_block` keeps its post-97 value; only `tick_data_block` rewinds |
| f. rollback then new events | `rollback_then_new_events_apply_cleanly_and_move_stamps_forward` | 99/100 apply cleanly, stamps move forward again |
| g. double rollback | `double_rollback_equals_a_single_rollback_to_the_earlier_target` | 99 then 97 == 97 once (and == replay) |
| h. registration boundary | `rewind_below_the_registration_boundary_is_a_hard_error` + `rewind_at_registration_and_one_past_it_are_legal` | empty journal = the coordinator's `NoStatePriorToBlock` boundary (pool-level refusal pinned); registration+1 and AT-boundary rewinds are legal |
| i. backward stamp outside reorg | `backward_price_clock_stamp_outside_reorg_still_panics` + `backward_liquidity_clock_stamp_outside_reorg_still_panics` | both monotonicity guards intact (panic messages asserted) |
| j. nonce + cache | `rollback_bumps_nonce_and_invalidates_the_tick_range_cache` | exactly one nonce bump; both cache directions reset |

`replace_tick_data_does_not_rewind_block` and the pre-existing
`restore_before_block_stamps_restored_ticks_with_the_liquidity_clock` (both twins) stay green —
the latter still holds because in its two-event scenario the true prior stamp equals the rewound
liquidity clock.

## Probe record (verbatim)

Probe applied to both twins (byte-for-byte backup + md5 first), reintroducing the re-stamp:

```rust
// v3_state.rs / v4_state.rs restore loop, probe only
let restored_block = self.tick_data_block;
...
block: restored_block,
```

`just test-rust-nextest -p degenbot-pools` -> **exit 100**, both properties red:

```
FAIL [   0.005s] (2824/4764) degenbot-pools v3_state::exact_rewind_tests::rewind_equals_replay_v3

    thread 'v3_state::exact_rewind_tests::rewind_equals_replay_v3' (351599) panicked at crates/foundation/degenbot-pools/src/v3_state.rs:3574:5:
    Test failed: rewind==replay: tick -60 STAMP mismatch - rewind says 51 but replay says 0 (target=52 script=[(51, Swap { sqrt: 79228162514264337593543950336, liq: 1000000, tick: 0, new_tick: None }), (52, Liq { lower: -60, upper: 60, delta: 7000 })]).
    minimal failing input: kinds = [
        0,
        3,
    ]

FAIL [   0.005s] (2884/4764) degenbot-pools v4_state::exact_rewind_tests::rewind_equals_replay_v4

    thread 'v4_state::exact_rewind_tests::rewind_equals_replay_v4' (351722) panicked at crates/foundation/degenbot-pools/src/v4_state.rs:1818:25:
    Test failed: rewind==replay: tick -60 STAMP mismatch - rewind says 51 but replay says 0 (target=52 script=[(51, Swap { sqrt: 79228162514264337593543950335, liq: 999998, tick: 500, new_tick: Some((180, 250)) }), (52, Liq { lower: 120, upper: 240, delta: -3000 }), (53, Swap { sqrt: 79228162514264337593543950337, liq: 1000001, tick: 60, new_tick: Some((120, 500)) }), (54, Liq { lower: -60, upper: 60, delta: 7000 })]).
```

Reverted from the byte-for-byte backups; `md5sum` before == after for both files:

```
bc553adf6f301cec903823ead3225c6c  v3_state.rs   (backup == restored)
96fc438e2d2120ac7199aceeea8d42ab  v4_state.rs   (backup == restored)
```

The probe also made proptest write `degenbot-pools/proptest-regressions/{v3,v4}_state.txt`; that
artifact of the probe was removed (it recorded a case that no longer fails).

## Gate exits (captured directly, no pipes)

| gate | command | exit |
|---|---|---|
| whole-workspace Rust tests | `just test-rust-nextest` | **0** — 4764 passed, 29 skipped |
| Rust lint | `just lint-rust-check` | **0** |
| Python lint (ruff AND ty) | `just lint-python-check` | **0** — `ruff check`: All checks passed; `ty check`: All checks passed |
| Stub drift | `just gen-stubs --check` | **0** — "committed stubs match generator output" |
| Comment hygiene | `just lint-comment-hygiene` | **0** |
| fmt authority (Rust) | `rustfmt --check --edition 2021 --config skip_children=true -- state_history.rs v3_state.rs v4_state.rs registry.rs stateview_feasibility_probe.rs` | **0** |
| fmt authority (Python) | `uv run --no-sync ruff format --check -- <touched .py>` | n/a — **no Python touched** |
| scope check | `rg -n restored_block rust/crates` | **1** (no match) |
| negative probe | `just test-rust-nextest -p degenbot-pools` with the re-stamp | **100** (both properties red, see above) |

Note on the recipe's `-p <crate>` narrowing: `just test-rust-nextest -p degenbot-pools` reported
the same 4764-test workspace total as the unfiltered run, so both runs exercised the whole
workspace suite; the plain `just test-rust-nextest` above is the reportable one.

One scoped run (the cold first build, 204s) exceeded the ~120s foreground bound and completed in
the background; its exit 0 and summary were read from the captured log file, not inferred. Every
other gate above ran in the foreground. Nothing is left PENDING.

## Counts delta

| unit | before | after |
|---|---|---|
| `TickBefore` fields | 2 (`liquidity_gross_before`, `liquidity_net_before`) | 3 (+ `block_before: u64`) |
| `size_of::<TickBefore>()` | 48 B (align 16) | **48 B** — the `u64` lands in the 8-byte hole between the 24-byte `Option<U128>` and the 16-byte-aligned `i128` |
| journal `Vec` element `(i32, TickBefore)` | 64 B | 64 B |
| journal bytes/entry | — | unchanged 64 B/tick-prior element; a 2-boundary Mint/Burn still journals 128 B, a Swap with one crossed tick 64 B |
| degenbot-pools tests | 4740 (`-p` run at session start) | 4764 (+24: 12 per twin) |

The stamp is therefore free in memory: the journal grows by nothing, and the accuracy gain is the
information (a real prior stamp) rather than bytes.

## Deliberately left

- **Word map / `BitmapAtWord` / `tick_bitmap` maps** — untouched (BW2/BW3 own them).
- **WOXT5Z's bulk-sync fills** — not reopened; a different arm of the stamp contract.
- **`V3RestoreResult.block`** — still returned (the oldest popped delta's block); nothing now
  stamps with it. Not removed (public shape, out of this chunk's radius).
- **`state_nonce` / `initial_state_block` / `registration_lifecycle` / snapshot fields** — not part
  of the equality property; the nonce is pinned by battery (j).
- **Python surface** — no Python files touched, so no Python fmt/stub change beyond
  `gen-stubs --check` staying green.
- Probe artifact `proptest-regressions/` removed (recorded above).

## PENDING (manager decides)

1. **Manager whole-tree ladder at review** — this chunk ran the full `just test-rust-nextest`,
   `just lint-rust-check`, `just lint-python-check`, `just gen-stubs --check`,
   `just lint-comment-hygiene` and the Rust fmt authority; nothing is left un-run, and no gate was
   detached or deferred.

## Tree / foreign edits

Written by this chunk: the 5 files above plus this page. Nothing else was modified, reverted, or
"cleaned up". `git status` showed only those 5 paths (plus the probe's `proptest-regressions/`
directory, since removed). No foreign edits were observed; the fovea "shared literal" notices
(`ReorgJournal`, `V3BlockDelta`, `build_int_v4_sequence`) are cross-file literal-sharing
heuristics on unchanged code, not edits by another agent.

One process note: the two `rustfmt` *apply* invocations on `v3_state.rs`/`v4_state.rs` are
reported by the workspace watcher as an "unattributed mutation path" — they are this chunk's own
formatter runs.
