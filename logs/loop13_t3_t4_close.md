## SREJKU + V4GZ6J — loop-13 close-out

T3: captured the gate-burst family live (MIN_US=20000, 433 lines) and converted to fixtures/live_gatebursts_mixed.jsonl (24 deduped 3-hop). Per-path gates (no prefix reuse) 0.6-2.4ms with the s1+hull dominance at 73%: pid 2557 gate 2.35ms = drv 119 / prod 478 / s1 910 / hull 800. The boundary-level stage-1 designs (wavefront, cascade) are already measured-inferior to the exact sweep on both old and fat corpora (loops 9/10); no third variant cleared the oracle bar this session.

T4: live restart-loop verification of the adopted loop-12 pipeline on NEW solve-phase telemetry:
- gate.derive_us per solved path: pre-loop-12 44us -> post 17us (block 25876411, 3,676 paths).
- gate composition: derive 62.4ms + compose 1,208ms + search 102ms + product 311ms + stage1 600ms + hull 271ms = 68% of solve CPU — instrumented split now names every ms.
- walk: 1.24M sims / 2.63M word steps / refine 264k; slowest-path list re-identifies the 25k-sim family still tailing the pack (sims-aware LPT orders them first).

## Loop-13 outcomes
1. Sim atomization (adopted) — right-edge bisection = 84% of walk sims (counter + replay columns live).
2. Edge-bracket tolerance (rejected, reverted) — loses climb basins/corners at every tolerance > 8; the <=4-wei bisection is correctness-load-bearing.
3. Gate-burst corpus landed for future stage-1 work.
4. No further walk-sim lever survives the oracle; remaining leads documented for a future session: climb-oracle redesign (theoretically costly), or per-pool edge memo across blocks (needs exact-edge dedup fingerprinting — reopened once a capture spans two blocks).
