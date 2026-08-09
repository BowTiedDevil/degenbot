# OC34VZ — Generalized per-hop staleness reporter (TDD) — done

**Outcome:** added a generalized, non-aborting diagnostic that surfaces when any
**Tracked Live** CL (V3/V4) hop's `update_block` trails the promoted solve anchor
past `MAX_CL_STALENESS_BLOCKS` — the pre-condition the strict `SOLVER-STATE`
gate escalates to a fatal abort. Pool-agnostic: identity is read from each hop
(`v3:<address>` / `v4:0x<leading-hex>…`), never hardcoded, so lag is observable on
whatever pool misbehaves (the incident pool was `0fb0e40…`; a future pool may differ).

## Changes
- `rust/crates/degenbot-bot/src/bot_core/solver_state_verifier.rs`
  - `LaggingHop` struct + `lagging_tracked_hops(anchor, hops) -> Vec<LaggingHop>`
    (pure, `#[must_use]`). Scope: `cl_meta Some`, `cov == "Tracked"`,
    `lifecycle == "Live"`, `update_block > 0`, `anchor - update_block > MAX_CL_STALENESS_BLOCKS`.
    Skips Sparse/Quarantined (seeded/never-updated by design), never-updated, non-CL,
    and at/within-threshold hops.
  - 5 new unit tests (RED→GREEN): fires on lagging Tracked V3 and V4; ignores
    Sparse/Quarantined, never-updated, and <=3-block lag; identity is generalized
    (used verbatim, not a literal).
- `rust/crates/degenbot-bot/src/bot_core/block_pump.rs`
  - Imported `lagging_tracked_hops`; in `verify_solver_state_against_chain`, before
    the strict per-path gate, logs a `WARN` per lagging hop (generalized fields:
    path_idx, block, hop idx/type, coverage/lifecycle, both clocks, stale_by, pool).
    Observational only — does not weaken the UO3JM4 abort.

## Verification
- `cargo test -p degenbot-bot --lib`: 418 passed, 0 failed (rustc `warnings=deny` clean).
- All 14 `solver_state_verifier` tests pass, including the 5 new reporter tests.

## Environmental note
- `cargo clippy --deny warnings` cannot run to completion in this working tree:
  the in-progress executor-refactor WIP (`rust/crates/degenbot-executor/src/grammar.rs`,
  untracked; `composers.rs`/`encoders.rs` modified) fails clippy with ~22 errors
  (dead_code / must_use_candidate / get_first) under the workspace `warnings="deny"`,
  and clippy lints local path deps. This is pre-existing WIP from another task and was
  left untouched. The changed crate itself is rustc-clean (build+test passed).
