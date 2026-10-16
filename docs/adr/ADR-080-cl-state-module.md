# ADR-080: The CL state module — the Concentrated Liquidity Kernel's state-side sibling

**Status: accepted (2026-10-16).** Drafted at the architecture-review grilling (candidate C6; rounds 1–3, Q8/Q17/Q24). The state-side analogue of ADR-071 (the kernel unified the solve side) and ADR-073 (the family read module unified the read side); ADR-059 D3/D6 stand unchanged.

## Context

ADR-071 gave the CL family one solve home (the kernel); ADR-073 gave it one read home (`families/concentrated_liquidity/`, whose tree already owns the read-side state slices — `ConcentratedLiquidityState`/`Mut`, the V3/V4 match compiler-exhaustive inside the tree). The **full state structs** remain hand-kept twins: `v3_state.rs` (3,597 lines, ~1,259 test) and `v4_state.rs` (2,298 lines, ~1,198 test) carry parallel method sets — `swap_is_viable`, `word_of`, `seed_known_bitmap_words`, `advance_update_block`, `advance_tick_data_block`, `seed_genesis`, `invalidate_tick_range_cache`, `get_cached_tick_ranges`, the `build_int_*_sequence` walks, `restore_before_block`, `discard_before_block`, `journal_len`, `newest_block` — and parallel test suites. V4 already imports V3's shared types (`PoolTickCoverage`, `RegistrationLifecycle`, `SimulateSwapError`, `V3SwapOutcome`): the shared half is acknowledged, not extracted. Every CL state fix is applied twice; drift is caught only per file.

The V3/V4 divergence is real and stays: storage slots (slot0 vs state-view), event shapes, hooks hashed into the PoolId. That is adapter material, not shared-logic material — the same split ADR-071 drew for the solve side.

## Decision

**D1 — The shared half extracts into the CL family tree.** Journals (discard/restore/len), bitmap-word seeding and coverage, viability, and the tick-range cache become a data-generic state module inside `degenbot-pools/src/families/concentrated_liquidity/`, beside the read door — generic over the storage data, never a trait per pool, hot loops monomorphic (ADR-059 D6).

**D2 — `v3_state.rs` / `v4_state.rs` become the two storage adapters.** Slot0-keyed and state-view/hook-keyed storage, event application, and identity stay in the per-design files; the shared sequence/journal/viability behavior crosses the one state-module interface.

**D3 — Extraction order: journals → bitmap words → viability/cache.** Byte-identical moves per step (the ADR-071/073 playbook); no arithmetic changes in the same change.

**D4 — Replace, don't layer.** The two parallel test suites merge at the state module's interface; adapter-specific tests (slot0 vs state-view storage) stay thin on the adapters. Per-file duplicate tests are deleted, not kept.

## Consequences

The CL family's story becomes one tree: kernel (solve), read door (reads), state module (state), two thin design adapters. A CL state bug has one home; a fix lands once. The family read module's stated ownership of "the state slices" becomes literally true.
