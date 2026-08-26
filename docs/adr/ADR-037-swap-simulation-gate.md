# ADR-037: The swap-simulation gate — one deep swap-read interface

**Status:** Accepted
**Date:** 2026-08-25
**Task:** NHNNIZ / A7T56B (epic "Swap-simulation gate")

## Context

Every simulation read on `BotState` shipped in two shapes: a
`*_miss_aware` variant returning `Result<_, SimulateSwapError>` and a
`*_with_fetch` twin that re-implemented the identical fetch → merge →
retry loop three times (tokens-out, exact-input, exact-output), each with
its own dedup set and give-up-to-`U256::ZERO` semantics. Two further reads
carried the same disease in worse forms: `calculate_tokens_in` silently
swallowed `NotComputable` *and* `MissingTickWord` for V3/V4 pools behind a
docstring claiming constant-product math, and `simulate_swap_with_override`
carried two contradictory doc blocks over its fourth copy of the retry
loop, shaped as a 10-argument parameter bag.

The consequence was a shallow interface: callers had to know which shape
they held, failure modes were invisible (`ZERO`/`None` meant at least four
distinct things), and any change to give-up semantics required three
synchronized edits plus three parity suites. An architecture review
(ergo epic `NHNNIZ`) settled the replacement design; this ADR records it
so future reviews do not re-suggest the twins.

## Decision

1. **One public swap-read interface.**
   `BotState::swap_simulation(pool_id, SwapRequest) -> SwapRead`
   (module `bot_core/swap_simulation.rs`) replaces all seven methods: the
   three `*_miss_aware` twins, the three `*_with_fetch` twins, and
   `calculate_tokens_in`. The pure family cores stay put
   (`degenbot_pools::simulate_swap::simulate_swap`,
   `v3_state::v3_simulate_swap`, `v4_state::v4_simulate_swap`); the module
   owns request/outcome types and the single fetch→merge→retry policy,
   generic over compute closure + merge target so the override path reuses
   it without touching registered state.

2. **Signed request, user perspective.**
   `SwapRequest { zero_for_one, amount_specified: I256, sqrt_price_limit }`:
   positive `amount_specified` = exact-output (the pool delivers that
   magnitude to the user); negative = exact-input (the user sends it).
   This follows the **V4/user perspective** deliberately — neither
   engine's internal convention is canonical at this seam. The mapping
   lives in exactly one place inside the module:
   - V3 engine: negates both directions vs canonical (V3 exact-in `>0`,
     exact-out `<0`);
   - V4 engine: identity (exact-in `<0`, exact-out `>0`).
   Outcome deltas are reported in the same user perspective (positive =
   received). The mapping is pinned by table tests.

3. **Typed outcome, hard cutover.** No backwards-compatibility layer
   (per AGENTS.md): every former silent-`ZERO`/silent-`None` failure mode
   becomes an observable variant — `NotComputable`, `FetchFailed { word }`,
   `FetchExhausted { word }` (repeated miss on an attempted word, or no
   fetcher registered). The information is never destroyed; a convenience
   flatten may exist for callers that want today's ergonomics.

4. **Trust = caveat set, not bool.** Outcomes carry an additive,
   `#[non_exhaustive]` flag set whose EMPTY value means "this number is
   exact". First variant `SparseCoverage`, derived from registration-time
   `PoolTickCoverage` (glossary Tracked/Sparse). Per-family payload
   variants keep invalid states unrepresentable (V2 has no per-hop detail;
   CL payloads carry consumed/delivered/end-state/`fetched_words`).

5. **Hooked pools: caveat, not refusal.** `RegisterV4PoolError::DynamicFee`
   and `FeeExceedsEncoderLimit` rejections stay (un-encodable fee / fee-flag
   ambiguity). Amount-modifying hooks are ADMITTED since ergo task `X4EU3J`:
   their simulations carry `Caveats::HOOKED_POOL` (derived from
   `hook_flags & 0xCC`, the low 16 bits of the hook address per v4-core
   `Hooks.sol`) and hop projection excludes them from solving, so the net
   trading behavior is unchanged while the pools become queryable. The
   archived Python pattern (`archive/main-20260721` `v4_liquidity_pool.py`
   `Hooks` enum; `PossibleInaccurateResult` carrying the approximate
   amounts) is the model for the Python-side surfacing in the tail task.
   ADR-012's spec-width admission contract is unaffected (storage widths,
   not hooks).

## Consequences

- The miss policy has ONE home: dedup set, repeated-miss give-up, missing
  fetcher handling, and reentrancy discipline (fetcher cloned off the
  entry before looping) concentrate where they are unit-tested once.
- Both consumers benefit equally — the standalone Rust bot and the PyO3
  driver shell sit on the same seam; Python-side retry/sign-shuffling
  becomes deletable after the FFI stabilizes (tail task `3XGX4A`).
- Parity tests that pinned silent-ZERO shapes now assert explicit variants.
- Non-goals: no third bespoke pump channel or bus unification (ADR-027);
  no window-guard changes (ADR-036); no solve-seam reshaping (ADR-015);
  no family-trait work (ADR-014/016/017). Domain vocabulary lives in
  CONTEXT.md under "Swap simulation".