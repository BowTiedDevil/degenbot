# FUWYUR — root cause & fix: missed live V3 Mint application (UO3JM4 desync class)

Date: 2026-08-25 · Implementer: ox-alpha@degenbot

## TL;DR

Live-WS liquidity events (V3 Mint/Burn, V4 ModifyLiquidity) for pools that are
**not yet registered** were silently dropped by `LogDispatcher::dispatch`'s
APPLY-MISS funnel **before** ever reaching `apply_v3/v4_liquidity_update`'s
unregistered-buffering arm — making that buffering arm dead code on the live
path. When crawl registered the pool AFTER the event's block completed, the
engine pinned pre-event DB data and went Live permanently missing the delta,
until ADR-021 tripped.

## Root cause chain (matches every observed fact)

1. Crawl mid-flight: pool `0x88e6A0c2…5640` not yet in `BotState` when block
   25834714's logs arrived (~60 s after launch; control-run timing shows this
   pool registers late in the crawl).
2. The Mint@25834714 decoded fine, but `dispatch()`'s funnel
   (`resolve_pool_id == None`) early-returned with only a DEBUG "APPLY MISS".
   The dual buffer (`v3_buffer.buffer_pump`) was never reached.
3. Registration later loaded the DB row stamped `update_block = 25834714`
   (the DB-aware liquidity updater refreshes rows concurrently with the
   crawl; its stamp raced the Mint), drained buffers that did NOT contain the
   Mint, verified against an anchor sharing the blind spot, and went Live with
   stale tick 193370 gross = 2441327690821017 vs on-chain 2560076249428705.
4. No `[WS-INVARIANT]` fired because WS delivery was fine — the loss was
   *after* delivery, inside dispatch. `ws_delivered` recorded the log index;
   the tombstone cross-check passed vacuously.

Backfill-window events (S+1..W) were never affected: `process_backfill_logs`outes Mints/Burns into the backfill buffer regardless of registration. The
unprotected window is exactly `(W, registration]` on the live stream.

## Fix (degenbot-bot/src/bot_core/log_dispatcher.rs)

Extracted the funnel miss handling into `handle_apply_miss`:

- CL-family liquidity events (`V3Liquidity | V4Liquidity`) now fall through to
  the write-lock apply path even when unregistered — the existing
  `apply_v3/v4_liquidity_update` unregistered arms buffer them into the pump
  buffer, so the registration drain+pin seam (`apply_pump_buffer_v3`,
  cutoff-gated) and `set_*_pool_live` tail flush capture them with no new
  machinery.
- Swap/Sync events keep the cheap no-write-lock drop: their scalar payload is
  re-seeded wholesale from the DB row at registration, so buffering would be
  dead weight. Hot-path cost added is one write-lock acquisition per
  unregistered-pool Mint/Burn (rare vs swaps).

Telemetry stays honest: still counted as `log_apply_missed` from the solve
path's perspective (no subscriber notify), with a distinct trace message
naming the buffer routing.

## Tests (red→green)

- `bot_core::block_pump::tests::fuwyur_live_mint_for_unregistered_pool_survives_late_registration`
  drives the REAL pump: live Mint@N for an unregistered pool + Swap@N+1
  tombstone → late Tracked/Quarantined registration → drain+pin+set_live.
  Assert tick gross lands at the exact production numbers
  (244_132_769_082_101_7 → 256_007_624_942_870_5). RED before the fix with
  precisely the tripwire symptom (stale seed value left-pinned).
- `bot_core::log_dispatcher::tests::fuwyur_unregistered_mint_is_buffered_not_dropped`
  pins the dispatcher-level contract (`buffered_v3_event_count > 0`).
- Full crate suite green (568 passed), clippy `-D warnings` clean, fmt applied.

## Follow-ups (out of scope here, do NOT lose)

1. **DB row stamp honesty**: the updater wrote `update_block = 25834714` while
   the row content pre-dated the Mint (fetch/stamp race inside the updater).
   With the fix the buffered event re-pins honestly, but the updater's stamp
   semantics deserve their own task — a lying stamp can still mask gaps for
   pools whose events never arrive post-registration.
2. **V4 symmetric unit test**: the fix routes `V4ModifyLiquidity` identically,
   but a dedicated dispatcher/pump test with a real ModifyLiquidity log wasn't
   built here.
3. **Peer's step-2 probe**: on Live transition, fetch the pool's full tick
   bitmap anchor ticks on-chain and diff (closes the off-range blind spot
   ADR-021 admits) — still worth building as an independent guardrail.
4. Peer-flagged second latent desync: V4 hop carried `tick_data_block =
   25477310` (~357k blocks stale at launch) — investigate separately.
