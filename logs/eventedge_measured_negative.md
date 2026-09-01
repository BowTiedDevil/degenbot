## Closed-form edge localization (event-driven Mobius inverse) — measured NEGATIVE

Task lineage: loop-13 close-out noted right-edge bisection owns 84% of walk sims and is irreducible without changing the climb oracle. This was that oracle change.

**Design.** Replace the per-piece grow+bisection that finds the walk window right edge with the closed-form preimage: for tuple ks, the first input whose landing exits the piece = min over hops h of the Mobius prefix-chain inverse O_{h-1}^{-1}(T_h) at the hop's next boundary gross input T_h. Added compute_shifted_piece_mobius_prefixes (Fast I1024/Big fallback) and shifted_piece_min_input_reaching_output (x=(Dt-B)/(A-Ct), integer ceil) to mobius_shifted_piece. Validator variants tried: 2-probe verify (e,e+1), pre-positioned bracket [fs-W, fs+W], guarded rightward grow fs+64 doubling, and a symmetric leftward grow for quantizer-reversal pieces. 103-suite green; replay goldens 104/104 on all variants.

**Decisive measurement** (census counters per piece; 234-piece family 3671 [318,88,291]):
- The preimage itself lands above (localizes) in 1/233 pieces; 232/233 are quantizer-blocked: the realized chain floors down at each hop, so at the predicted x the hop input is 1-2 wei short and the landing index does NOT roll over. The crossing actually fires at the NEXT lattice preimage — a full piece-width away (1e16-1e19 wei).
- The walk's pieces are consecutive events of a quantized monotone chain; the right edge of piece k IS the (k+1)-th event; preimages pre-sort candidates but quantization can skip the leading candidate, so the search pays the full log-span anyway. Per-piece right-edge sims: event-driven ~69 vs baseline seeded ~68. Wall time: no change.

**Structure, stated:** the only algorithmic exit is a NEW oracle that reads crossing tables as an event queue (O(#boundaries) per hop with an amortized cascade watch list) — no seed-based scheme. Deferred, with scaffolding name walk_event_queue_climb, to the next climb-oracle rewrite.
