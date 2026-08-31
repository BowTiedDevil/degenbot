## Loop-12 BY7BLS — solver p90 makespan + fat-family envelopes: close-out

| Task | Change | Evidence | Verdict |
|---|---|---|---|
| KUKHMX | sims-aware LPT cost (max(proxy, previous-block sims), recorder per path) | 7/7 lpt tests, bot suite 6/6 green; solutions byte-identical by construction (pure scheduling) | **adopted** (live observation channels: [solve-phase] phase_us + slowest max) |
| 4EG7P3 | synthetic giant-liquidity corpus + generator | G family 5.1ms gate / G2 13.5k sims reproduces live walk scale; generator committed | adopted |
| PVOPYP | early-select tangent derivation on fat tables | derive owned 60% of the fat gate; gates −21/−29% on synth, heavy-corpus floor lines byte-identical (420/420/465), 692/701 golden, mixed divergent=0; cross-crate suites 11/11 + 6/6 green | **adopted** |
| IBFXZP | per-pool walk memo | synthetic corpus cannot reproduce live 27817 stickiness (pieces=7 in W/W2) → no measurement surface | **canceled with rationale** |
| ZIDOAO | audit | bot 6/6 + solvers 11/11 result files green; replay parity re-verified post-move | done |

## Expected live effect (restart-verified)

- Gate derive on fat-pool paths loses the per-range coefficient derivation for un-sampled tangents (the live paths pass caller-built crossing tables, so the saving is the tangent-loop term; typical blocks show gate.derive_us ~126ms → expect a chunk of that to vanish, biggest on the 24–153ms gate-burst paths).
- Makespan: heavy repeats (path 27817 26k sims, 56–244ms) now bin first everywhere they appear, so block max drops toward the balanced bound.

## Left for loop-13
1. Live capture of path 27817's real crossing tables (one DEGENBOT_SOLVER_CAPTURE block) to reopen the walk-memo and W-family A/Bs.
2. The remaining fat-family derive floor = owned crossings build in bench mode; live already avoids most of it.
3. Walk-heavy paths (~29% of CPU) — sim reduction candidates on real table shapes only.
