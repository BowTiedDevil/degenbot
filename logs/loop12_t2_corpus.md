## 4EG7P3 — done: synthetic giant-liquidity corpus (loop-12)

Generator: `rust/crates/degenbot-solvers/examples/synth_corpus_gen.rs` (re-runnable: emits both replay schemas from one geometry set).

Families (fixtures/synth_giant_{cl,mixed}.jsonl):
| family | range layout | envelope | replay wall | walk |
|---|---|---|---|---|
| W (27817-style shallow climb) | [1, 390, 3] | 555us | 689us | pieces 7 (does NOT reproduce live 323 — live climb stickiness needs more slope shaping) |
| G (gate fat) | [2500, 4000, 2500] = 9k | 6.4ms | 6.0ms | pieces 2 |
| G2 (generic climb) | [1200, 3000, 1500] = 5.7k | 3.7ms | 22.1ms | sims 13,550 / pieces 298 |

G2 reproduces the live WALK-heavy magnitude (26k sims / 323 pieces live); G reproduces the gate-burst magnitude at single-path scale (6.4ms before per-block prefix reuse).

gate_bench on synth_giant_mixed:
```
71001 [2500,4000,2500] gate=5.24ms (prod 0.55 / s1 0.87 / hull 0.52, ~3.3ms unaccounted = compose pruning loops)
71002 [1200,3000,1500] gate=4.31ms
71000 [1,390,3]       gate=0.41ms
```
A/B surface for T3 established (deterministic 3/3; golden-null = informational only).
