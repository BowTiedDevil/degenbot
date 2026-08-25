Answers for FUWYUR (from the session that tripped the desync):

Q1 — Log paths:
- Gated run that tripped (launched 20:49 UTC via ./run_bot.sh start, aborted 20:51:09): logs/bot_run.log — LOST, truncated by the control-run restart at 21:04 (run_bot.sh does `: > $LOG`). Key excerpts preserved in FUWYUR body + below.
- Control run (ungated, started 21:04 UTC, still running as of handoff): logs/bot_run.log (live).
- Earlier gated runs: /tmp/bot_gate_run1.log (~15min clean window, gate ON but zero effective skips due to pre-fix `<` semantics) and /tmp/bot_gate_run.log (~10 min, post-fix, gate.skipped=558 in block 25834691; killed by my pkill before rebuild). Both died via sys.exit(3) sim-trap or pkill — NOT via desync.
- Preserved abort excerpt:
  `[SOLVER-STATE] ABORT: solver used desynced pool state at solve block 25834715 (path_idx=12, class=Unclassified, hops: hop V2 update_block=25834711 tick_data_block=25834711 stale_by=4, hop V4 update_block=25834711 tick_data_block=25477310 stale_by=4 cov=Tracked, lifecycle=Live, hop V3 update_block=25834714 tick_data_block=25834714 stale_by=1 cov=Tracked, lifecycle=Live) hop 2: tick-map fidelity probe at tick-data anchor 25834714: V3 pool 0x88e6...5640 block=25834714: tick 193370 liquidityGross mismatch — engine: 2441327690821017, on-chain: 2560076249428705`
  Note hop1 (V4): tick_data_block=25477310 — ~357k blocks older than head at launch; check whether that V4 pool is a second latent desync.

Q2 — WS completeness: implemented in block_pump.rs `assert_ws_block_complete` (~line 1822). Per-block eth_getLogs vs WS-delivered log_index sets, filtered client-side by RELEVANT_TOPICS; on any missing log_index it ABORTs loudly (`[WS-INVARIANT] LIVE WEBSOCKET LOG DROP`). No WS-INVARIANT line appeared in the tripped run's log before the desync ABORT — so either the check ran and passed (log WAS delivered → loss is downstream of delivery), or the check was skipped for that block. IMPORTANT: commit 6acdbbe30 added skipping the completeness check for backfill-owned blocks, and block 25834714 was only ~60–90s after bot launch — right at the backfill→live boundary. Also note pump_fsm.rs references DEGENBOT_WS_COMPLETENESS. This boundary classification is my top suspect for HOW the Mint went missing.

Q3 — dropped/quarantined counters for 25834714: I did not observe any such warnings/counters in what I captured. The tripped run's detailed log was lost to truncation, so this needs a fresh instrumented run.

NEW FINDING since filing (changes the picture):
- On-chain binary search: the Mint landed exactly between blocks 25834713/25834714 (cast ticks() probes; value 2441327690821017 → 2560076249428705). At snapshot block 25829786 on-chain matched what the engine loaded — snapshot/DB exonerated.
- Control run registered pool 88e6a0c2 AFTER block 25834750 with a FRESH pin (verify-dbg: tick_data_block=25834750, tick_count=1542) — so control never had the stale value, explaining its clean trip record. The gated run had registered it BEFORE the Mint block (engine held pre-Mint value at Live), then missed the live application.
- So the discriminating variable may be REGISTRATION TIMING relative to block 25834714, not gating itself. Suggest reproducing: restart gated, watch whether pools registering pre-Mint-moments miss live Mints while post-registration pins are correct.

Also flagged in SU7MAE: 73% of path-solves were gate-unsupported (Solidly/Curve/Balancer mixed paths) — separate follow-up.