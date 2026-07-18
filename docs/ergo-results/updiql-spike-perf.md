# Spike Report: f64 bracket-narrow vs native U256 swap perf (UPDIQL)

## Goal
Decide whether a cheap f64 bracket-narrow pass adds meaningful value over running the native U256 swap leaves directly in a Brent/golden-section search over the U256 domain, for Curve / Balancer-weighted / Balancer-stable pairwise solve branches.

## Methodology
Added criterion micro-benchmarks under `rust/crates/{degenbot-v2-math,degenbot-curve-math,degenbot-balancer-math}/benches/`. Mainnet-like reserves at typical magnitudes (3pool ~$30M, 50/50 weighted ~$1M, 80/20 weighted ~$1M, 3-token Balancer stable ~$30M with A=200). V2 `IntHopState::swap` included as the Möbius baseline (the fast path Curve/Balancer branches are compared against).

## Results — per-call latency

| Leaf | Native U256 | f64 analog | Speedup of f64 |
|------|-------------|------------|----------------|
| V2 `IntHopState::swap` (Möbius baseline) | **63 ns** | — | — |
| Curve `stableswap_get_d` (3pool, A=2000) | **335 ns** | — | — |
| Curve `stableswap_get_y` (3pool swap) | **1.32 µs** | **16.8 ns** | 79× |
| Balancer weighted `calc_out_given_in` (50/50) | **138 ns** | **13.4 ns** | 10× |
| Balancer weighted `calc_out_given_in` (80/20) | **197 ns** | — | — |
| Balancer stable `calculate_invariant_deployed` (3-token, A=200) | **355 ns** | — | — |
| Balancer stable `calc_out_given_in` (3-token, precomputed D) | **1.19 µs** | — | — |

Notes:
- Curve `stableswap_get_y` calls `stableswap_get_d` once internally (the 335 ns is folded into the 1.32 µs).
- Balancer stable `calc_out_given_in` runs at ~1.19 µs *with precomputed D*; resolve_path computes D once per block (355 ns), so the per-iteration search cost is the 1.19 µs figure. Without precomputed D (per-iteration D recomputation) it would be ~1.55 µs.
- f64 analogs exist trivially for Curve `get_y` (same Newton recurrence, f64 ops) and Balancer weighted (stdlib `powf`). Balancer stable f64 analog was not measured but is structurally near-identical to Curve (Newton inversion of the same invariant shape) — expect ~20 ns like Curve.

## Projected solve latencies

Brent's method (scipy `minimize_scalar(method="bounded")`) typically does **~30–50 iterations** to converge to 1e-6 relative tolerance.

| Family | Native U256 per Brent solve (40 iters) | f64-narrow + U256 verify |
|--------|----------------------------------------|--------------------------|
| Curve | 1.32 µs × 40 ≈ **53 µs/solve** | ~20 ns × 40 (f64 probe) + 1.32 µs × 3 (verify) ≈ **4.4 µs/solve** |
| Balancer weighted | 138 ns × 40 ≈ **5.5 µs/solve** | (f64 ≈ 0.5 µs) ≈ **1 µs/solve** |
| Balancer stable | 1.19 µs × 40 ≈ **47 µs/solve** | ~20 ns × 40 + 3 × 1.19 µs ≈ **4.4 µs/solve** |

For comparison, the V2/V3/V4 Möbius closed-form solve is ~1 µs total (single closed-form computation, no iteration). So:
- **Balancer weighted over U256 directly** is already ~5 µs/solve — within 5× of Möbius, acceptable.
- **Curve and Balancer stable over U256 directly** are ~50 µs/solve — 50× slower than Möbius. Per affected-path-batch of, say, 20 paths, that's ~1 ms of pure Curve/stable solve time — still under the 50 ms debounce budget, but a noticeable fraction.

## Precision-loss risk in f64 (the f64 narrow's downside)

f64 has 52 bits of mantissa. Reserves at mainnet magnitudes (~1e24–1e26 wei for $1M–$10M pools at 18 dp) **exceed f64's exact-integer range (2^53 ≈ 9e15)**. Translating U256→f64 loses low-order wei precision:
- For an $1M 18dp pool (~1e24), f64 represents ~15 significant digits → ~9 wei of slop in the integer translation.
- For a $1M 6dp pool (~1e12), f64 is exact.

This precision loss is **safe for bracketing** (the f64 optimum only needs to land near the U256 optimum, not exactly on it — the ±3 integer verify sweep around the f64-derived optimum catches the true max). It is **unsafe for final verification**, which is why the U256 verify sweep is mandatory regardless.

## Recommendation

**Per family:**

| Family | Strategy | Rationale |
|--------|----------|-----------|
| **Balancer weighted** | **(a) Brent over U256 directly, no f64 narrow** | Native is 138 ns/call → ~5 µs/solve. f64 narrow saves <4 µs/solve. Not worth the precision-loss surface; weighted swaps are cheap. |
| **Curve stableswap** | **(b) f64-narrowed Brent + U256 verify** | Native is 1.32 µs/call → ~53 µs/solve. f64 narrow cuts to ~4 µs/solve (12× faster). Curve pools are common in arb paths; the latency matters. f64 precision loss is safe for bracketing; U256 verify catches the true optimum. |
| **Balancer stable** | **(b) f64-narrowed Brent + U256 verify** | Same shape as Curve (~1.2 µs/call, iterative Newton). f64 narrow cuts ~47 µs → ~4 µs. Same precision story. |

**Search algorithm: Brent's method, not golden-section.** Brent converges superlinearly (typically ~40 iters to 1e-6 tolerance vs ~60+ for golden-section), is the same algorithm Python `BrentSolver` uses (so cross-validation is apples-to-apples), and only needs `swap_fn` evaluations (no derivatives). The Solidly branch's golden-section + 25-iter template is preserved for Solidly specifically (Möbius precheck narrows its bracket so heavily that golden-section is fine); the new branches adopt Brent for the broader unbracketed case.

## Decision (Checkpoint answer)

- Balancer weighted → **(a) Brent over U256 directly**, no f64 precheck.
- Curve → **(b) f64-narrowed Brent + U256 verify**.
- Balancer stable → **(b) f64-narrowed Brent + U256 verify**.
- Search base → **Brent's method** for all three (superlinear convergence, matches Python oracle).

This strategy split gives the cheapest path where U256 is already cheap (weighted), and the f64 narrow where U256 is expensive (Curve, stable — both Newton-iterative). The U256 integer verify sweep (mirroring `solidly_brute_force_best`'s ±3 scan) is universal.

## Bench artifacts
Bench files added (not committed to the repo as production code — spike-only artifacts):
- `rust/crates/degenbot-v2-math/benches/swap.rs`
- `rust/crates/degenbot-curve-math/benches/stableswap.rs`
- `rust/crates/degenbot-balancer-math/benches/leaves.rs`

Run with: `cargo bench -p degenbot-{v2-math,curve-math,balancer-math} --bench {swap,stableswap,leaves}`
