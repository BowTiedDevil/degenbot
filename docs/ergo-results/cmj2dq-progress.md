Implemented via worker + adversarial review + supervisor fixes.

Landed:
- 48007b083 feat(sidecar): finality-based frame liveness (CMJ2DQ) - Tentative{NonceConsumed::MinedAt|SlotTakenAt} replaces stale/TTL death; reorg revival by canonical hash mismatch; finality deaths gated ONLY on the node's finalized tag; journal tentative records + boot fold + compaction; gate_mined_target replaces the stale gate; dry-run fixtures age-neutralized. 7 FSM pins (no-age, no-pool-eviction, revival, boot fold, finality).
- d1218a864 fix(sidecar): evidence-only liveness lanes (adversarial review D1/D2). D1: failed head-nonce read no longer fabricates u64::MAX consumption (evidence-gated nonce lane, unit-pinned). D2: classify_consumption no longer anchors SlotTakenAt to the observed head (self-consistent fabricated evidence the reorg check verified against itself); block+hash must come from the node's records or the frame holds for retry.

Verified live (session 20260918T174510Z-2440665, bid mode): boot fold reloaded 13 parked + 13 tentative; finality deaths flowing for legacy tentatives (mined=true and mined=false at block <= finalized).

Follow-ups (non-blocking, tracked here - now recorded as OPEN ergo tasks, created 2026-09-18 after the done-record hid them from ergo list):
- KYW2IQ: gap-journal versioned record migration (old schemas fold as Tracked; corruption never destroys evidence)
- BRV2XI: quarantine rescue funnel re-entry (frontier + pool-pred rescues re-enter the funnel - the rescue_event_unsupported stub violates the regime)
- WLZNMN: classification retry cadence decay (unresolvable evidence must not probe every head forever; no clocks, no fabricated death)
- 3S7JR6: by-nonce by-identity backfill when the lane supports it
Legacy notes preserved below.
- Journal schema migration cliff: reload skipped 2149 old-schema lines then compacted them away. Regime-correct fold of unknown-schema park records as Tracked needs a versioned record migration.
- by-nonce identity on lanes without connector-index support.
