## Cross-block composition memo (walk_climb_fork follow-up) — built, measured, parked off by default

Mechanism: 128-bit content fingerprint of the path composition (per-range mix of liquidity/prices/gamma/fee in one lane, capacity folds in the other). Key is EXACT: crossing tables + word profiles are pure deterministic derivations of the sequence, so an identical key cannot serve a stale result.

Telemetry-first discipline:
- Zero-code log census first: 118/119 recurring slow path ids are sims-stable across blocks; median reappearance 10 blocks.
- Then stats-gated live census (DEGENBOT_SOLVER_WALK_MEMO_STATS=1), sims-weighted after round one showed raw counts.

Live measurement (2 sessions, ~5 min dry-run each):
- probes/block 288-1315, hits 3-50 = 0.9-10% (median ~4%).
- sims-weighted: hit_sims 2k-20k vs probes_sims 235k-795k = **1.6-7.5% of walk sims replayable cross-block**.
- Cache-on run confirmed replays fire exactly as the census predicted (cache_plays tracks probes).

Verdict: adoption NOT justified at this rate — the fingerprint + interlocked probe would offset a ~4% replay. The gate is OFF by default (env read once; disabled runs skip the fingerprint and the lock entirely — byte-identical hot path). Kept as gated measurement infra because the project pattern is env-gated diagnostics; the census can re-score adoption whenever the path-generation strategy changes (e.g. multi-block lookbacks or repeated-solve batches).

## Files
- rust/crates/degenbot-solvers/src/mobius_v3_int.rs: WalkMemoState + fingerprint + probe/store/note + gated entry (+ unit test: content stability/order sensitivity/field sensitivity).
- rust/crates/degenbot-bot/src/solvers/arb_engine/solver_dispatch.rs: epoch advance + memo.* fields in the solve-phase completion log.

## Verification
- rust solver suite green; 104/104 golden + deterministic replay green after restructure.
- Maturing rebuild path: uv run maturin develop -> uv sync --reinstall-package degenbot enforced before every live run (stale-.so trap).
