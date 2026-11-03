# ADR-090: V4 hooked pools — the black-box finalist re-quote tier

## Status

Accepted (2026-11-02).

## Context

A quarter of all V4 liquidity is invisible to the solver. The live DB census
(182,733 V4 pools): 46,528 pools carry hooks (25.5%), of which **44,344 pools
across 10,628 distinct hooks** are amount-modifying (`AMOUNT_MODIFYING_HOOK_MASK
= 0xCC` — beforeSwap/afterSwap and their return-delta bits, per ADR-037). Since
ADR-037 these pools are admitted at registration with `Caveats::HOOKED_POOL` but
**excluded from solving at hop projection**: the white-box concentrated-liquidity
math cannot price an arbitrary hook's amount deltas, so the search tier never
generates a candidate through them.

Two established facts shape the remedy:

1. **The blindness is only in the search tier.** The dispatch pipeline is
   search (white-box ranking) → finalists → EVM verification frame → dispatch.
   The verification frame already executes the real PoolManager and hook
   contracts, so it is hook-exact today. What is missing is that search never
   proposes hooked candidates.
2. **Exact black-box quoting is already built and measured.** The evm2 pilot
   (ADR-089) executes real contracts with 0-wei parity against white-box on
   unhooked V2/V3/V4 legs at ~72µs/path warm (JIT, AOT store). The per-anchor
   prefetch (`sim/prefetch.rs`) reads a path's guaranteed slots in one RPC
   batch; the access-list capture (`sim/capture/access_list_slots.rs`) learns
   the slots a frame actually touched.

The correctness subtlety: a hook's internal state changes through arbitrary
storage writes invisible to the event stream, so hook state can never be
event-maintained the way pool state is. The tier's answer: cache the learned
slot *set* per hook, but read the slot *values* fresh from the node at the
solve's anchor on every quote — values are never cached across anchors.

## Decision

**The hybrid (Option C): approximate search, exact finalist re-quote.**

1. **Admission.** Hop projection stops excluding amount-modifying hooked V4
   pools. Search ranks their candidates by standard concentrated-liquidity
   math — an approximation that ignores the hook's deltas, which is exactly
   the mis-pricing `Caveats::HOOKED_POOL` already signals.
2. **The finalist re-quote.** Before a candidate enters the EVM verification
   frame, any leg through an amount-modifying hooked pool is re-quoted
   black-box: the real PoolManager and hook contracts execute under evm2 at
   the solve's anchor block (Quoter pattern — execute-then-revert), and the
   candidate's expected output is replaced with the exact figure. Candidates
   whose exact quote no longer clears the bar die here instead of wasting a
   verification slot.
3. **Hook state freshness.** The first black-box quote for a hook captures
   its touched-slot set (the access-list capture); the set persists keyed by
   hook (and spec) so steady-state re-quotes join the one-batch prefetch.
   Values come from the node at the anchor, every time.
4. **Caveat semantics.** `Caveats::HOOKED_POOL` remains on the search-tier
   approximation and on any path whose hooked legs were *not* black-box
   re-quoted (tier unavailable, hook not yet learned). A finalist whose
   hooked legs were all re-quoted at the anchor carries an exact quote; the
   caveat no longer applies to the quote (settlement risk is the
   verification frame's domain, unchanged).

Rejected alternatives: admission-only (A — ranking distortion wastes finalist
slots on hook-fee paths and misses hook-discount alpha), black-box in the
search fan-out (B — 72µs × candidate volume, mostly spent on candidates that
never final). The residual cost of C is missed alpha from approximately-ranked
hooked candidates that never reach the final — inherent to any
approximate-ranking scheme, bounded by the search breadth, and the bot's other
candidates still compete.

## Consequences

- ~24% of V4 liquidity becomes solvable (44,344 pools at the census date).
- The tier is the first production use of the ADR-089 fidelity machinery and
  creates the seam the dynamic-fee unlock (~489k path-facing pools, rejected
  at registration today) plugs into as a later phase: `beforeSwap`-computed
  fees are exact by construction under black-box execution, but admission
  requires the registration path to stop refusing `DynamicFee` — a separate
  decision, not made here.
- Cost is bounded by finalist volume: one black-box leg quote (~72µs warm at
  the pilot measurement) per hooked leg per finalist, plus a one-batch
  prefetch per anchor.

## Chunks

- **H1** — the black-box V4 leg primitive: `(pool_manager, pool_key,
  zero_for_one, amount_in, anchor)` → exact `amount_out` via evm2 execution
  of the real PoolManager through the Quoter pattern. Parity-tested against
  the on-chain V4 Quoter at the anchor for a census of real hooked pools
  (top hooks by pool count plus a long-tail sample).
- **H2** — hook slot learning: per-hook touched-slot capture → persistent
  prefetch facts; cold-start pays serial RPC fallback once per hook.
- **H3** — search/finalist integration: hop-projection admission, the
  finalist re-quote, the caveat semantics above.
- **H4** — performance: AOT coverage for PoolManager and the top hooks,
  re-quote p95 measured at production finalist rates, batch quoting only if
  the measurement demands it.
