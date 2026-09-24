# V2-as-degenerate-CL swap-step parity probe

Probe harness: `rust/crates/engine/degenbot-solvers/examples/v2_cl_parity_probe.rs`
Run: `cargo run -p degenbot-solvers --example v2_cl_parity_probe`

This probe answers the decision question in ADR-060 ("Collapse the V2 walk
dispatch — representation vs interface"): can a V2 constant-product hop be
represented as a degenerate single-range CL tick sequence without changing the
per-step arithmetic? It measures the **swap-step** level, which is the site that
flips the ADR decision; the full-path returned-tuple bar is a downstream
concern.

The probe runs the production code paths unchanged: the baseline is
`degenbot_math::v2::IntHopState::swap`, the candidate is
`degenbot_solvers::cl::simulate_v3_range_swap` over the projected range. No
special rounding, no configuration switch.

## Mapping construction

For a V2 hop `(R_in, R_out, gamma_numer, fee_denom)` and a chosen direction:

- **Liquidity**: `L = floor(sqrt(R_in * R_out))` — the integer geometric mean.
  The real geometric mean is irrational for any non-square `R_in * R_out`, so
  this floor is lossy by construction and is the first expected parity break.
- **Spot price**: `sqrt_price_x96 = floor(L * 2^96 / R_in)`, placing the
  projected spot at the reserve ratio. Also generally non-integral.
- **Range span**: one range with **empty interior word boundaries**, so the CL
  step degenerates to a single `compute_swap_step_v3` to the exit boundary.
  The span is widened by `2^32` on the swap side
  (`[sqrtP >> 32, sqrtP]` for zero-for-one, `[sqrtP, sqrtP << 32]` for
  one-for-zero). Every tested input is orders of magnitude below the resulting
  capacity, so the step stays in the target-unreachable (open-AMM) branch — the
  only branch with a V2 analogue.
- **Fee**: the V2 fraction is mapped onto the identical CL fraction,
  `fee_pips = 1e6 * (fee_denom - gamma_numer) / fee_denom`, giving
  `gamma_CL = 1e6 - fee_pips` over `fee_denom_CL = 1e6`. Exact for
  997/1000, 9975/10000, 9995/10000 and 99/100.
- **Direction**: both projections are measured. Zero-for-one projects
  `R_in -> token0`, one-for-zero projects `R_out -> token0`. The two CL
  rounding paths differ (zero-for-one floors `amount1`; one-for-zero floors
  `amount0`), so a representation must clear both.

The candidate's net input is `floor(x * gamma/fee_denom)` (the CL
`amount_remaining_less_fee`); the baseline keeps the fee inside the rational
`floor(x * gamma * R_out / (fee_denom * R_in + x * gamma))`.

## Corpus inventory

| Source | V2 hop states | Notes |
|---|---|---|
| `tests/fixtures/heavy_mixed_solve_captures.jsonl.zst` | 27 unique (369 occurrences) | real mixed V2+CL solver captures |
| `tests/fixtures/path5000_v2v4v3_block25704509.json` | 1 | real UniV2 hop with recorded input |
| `tests/fixtures/path110302_v3v4v2_block25711761.json` | 1 | real UniV2 hop, recorded 58,233,015 input |
| `tests/fixtures/path182449_v4v4v2_block25731019.json` | 1 | real SushiV2 hop, recorded 9,085,365 input |
| **real total** | **30** | fee tiers 0.30% and 0.25% |
| synthetic grid | 36 | fees {0.30%, 1%, 0.05%} × reserve ratios {~1x, 10x, 1000x} × magnitudes {1e18, 1e21, 1e24, 1e27} |

Each state is swept over an 8-point log-spaced input grid up to the order of
magnitude of `R_in`, plus the 3 recorded hotspot inputs, in both directions:
**1236 comparisons**.

Coverage gaps (methodology limitations):

- No boundary-exact cases. The range is deliberately widened past every tested
  input to keep the step target-unreachable; a synthetic range-boundary hit is a
  projection artifact, not a V2 behaviour. `range_boundary_hits = 0`.
- No input beyond ~`R_in` and no multi-hop composition; the probe is a
  per-step comparison by scope.
- Real captures cover only 0.30%/0.25% fee tiers and two ratio regimes
  (~2.5e-9 and ~2e2..5e2). The 1% and 0.05% tiers are synthetic only.
- Per-hop input amounts are not captured in the mixed JSONL, so the input grid
  plus the recorded single-path hotspots stand in for solved amounts.

## Divergence distribution (1236 comparisons)

| Bucket | n | divergent | % | negative | positive | min delta | median (non-zero) | max delta |
|---|---|---|---|---|---|---|---|---|
| **overall** | 1236 | 225 | 18.20% | 192 | 33 | -530,594,942 | -11 | +7,255 |
| real | 660 | 66 | 10.00% | 58 | 8 | -530,594,942 | -211 | +7,255 |
| synthetic | 576 | 159 | 27.60% | 134 | 25 | -999 | -9 | +148 |
| zero-for-one | 618 | 103 | 16.67% | 103 | 0 | -530,594,942 | -14 | -1 |
| one-for-zero | 618 | 122 | 19.74% | 89 | 33 | -530,594,942 | -9 | +7,255 |
| ratio >=100x | 110 | 66 | 60.00% | 58 | 8 | -530,594,942 | -211 | +7,255 |
| ratio 1000x | 192 | 85 | 44.27% | 72 | 13 | -999 | -208 | +148 |
| ratio 10x | 192 | 64 | 33.33% | 52 | 12 | -10 | -5 | +2 |
| ratio ~1x | 192 | 10 | 5.21% | 10 | 0 | -1 | -1 | -1 |
| ratio <1e-3 | 550 | 0 | 0.00% | 0 | 0 | 0 | 0 | 0 |
| 0.30% (synthetic) | 192 | 48 | 25.00% | 40 | 8 | -996 | -9 | +138 |
| 1.00% (synthetic) | 192 | 46 | 23.96% | 38 | 8 | -989 | -9 | +148 |
| 0.05% (synthetic) | 192 | 65 | 33.85% | 56 | 9 | -999 | -9 | +134 |
| 997/1000 (real) | 594 | 66 | 11.11% | 58 | 8 | -530,594,942 | -211 | +7,255 |
| 9975/10000 (real) | 66 | 0 | 0.00% | 0 | 0 | 0 | 0 | 0 |

`consumed_input` matched on every comparison (`consumed_mismatch = 0`): with the
range never reached, both simulators consume the full `x`. Divergence is
therefore entirely in `output`. It is **not one-sided overall**: zero-for-one is
strictly non-positive, one-for-zero can overshoot.

Representative real divergences (all `consumed_v2 == consumed_cl`):

```
x=1            R_in=7456337076646   R_out=3886685670626625517819   V2=519695605   CL=0
x=1            R_in=272235829501    R_out=144881599035718159565    V2=530594942   CL=0
x=9085365      R_in=1835250620298287096280  R_out=4569594589161887004337777
               V2=22553805448 CL=22553803195 delta=-2253
x=58233015     R_in=7456337076646   R_out=3886685670626625517819
               V2=144559529641 CL=144559527263 delta=-2378
```

## Structural reasons for the divergence

1. **Post-fee net input is floored before the invariant.** This is the dominant
   and decisive break. V2's `getAmountOut` keeps the fee as an exact rational:
   `out = floor(x·gamma·R_out / (fee_denom·R_in + x·gamma))`. The CL exact-in
   step first computes `amount_remaining_less_fee = floor(x·gamma/fee_denom)`
   (a `muldiv`), then derives the price move from that floored quantity. For a
   non-unit `gamma`, small inputs floor to zero: at `x = 1` and 0.3% the CL step
   sees `floor(0.997) = 0` and returns zero output while V2 returns
   hundreds of millions of wei. This produces the `-530,594,942` real-fixture
   miss and the zero-for-one one-sidedness.
2. **Geometric-mean liquidity floor.** `L = floor(sqrt(R_in·R_out))` equals the
   real geometric mean only for perfect squares; the projected constant-product
   invariant therefore differs by up to one unit of `L`. The output effect
   scales with `R_out` and with the reserve ratio.
3. **Spot-price floor.** `floor(L·2^96/R_in)` sits off the exact reserve ratio
   by up to one Q96 unit, shifting the projected price.
4. **Per-step rounding shape.** Even with exact `L` and sqrt price, the CL step
   rounds `sqrt_price_next` up and floors the output delta in two stages, while
   V2 performs a single terminal `DIV`. That asymmetry is why the zero-for-one
   arm is one-sided (candidate ≤ baseline) but the one-for-zero arm can exceed
   the baseline (the `amount1` input path rounds differently), yielding the
   +7,255 maximum overshoot.

The states that matched everywhere (`ratio < 1e-3`, including the three real
0.25% pools and the deep-ratio 0.3% pools) do so because their outputs are small
relative to the reserve magnitude: the `L`, sqrt-price and net-input floors all
fall below the 1-wei output quantum at the tested inputs. The projection is
arithmetically identical there by magnitude, not by construction. Divergence
concentrates where `R_out >= 100·R_in` (60% of comparisons).

## Verdict

**B — keep the interface collapse; the representation collapse fails the
zero-tolerance bar.**

ADR-060 clears option A only if every compared step is byte-identical. The probe
found 225 of 1236 comparisons (18.2%) with a non-zero output delta, including a
real captured hop whose `x = 1` output differs by 530,594,942 wei and a real
`x = 58,233,015` comparison differing by 2,378 wei. No wei tolerance applies:
the structural cause — flooring the post-fee input before the invariant, plus
the geometric-mean and spot-price floors — is inherent to the CL step, not an
accumulation a probe could erase. A representation swap would therefore change
landed range geometry and any candidate sitting on a `crossing_gross_input`
boundary, which is an observable result change dressed as a refactor.

Collapsing the V2 walk dispatch at the **piece-view seam** (interface collapse)
removes the enum match sites without touching either production simulator and
remains byte-identical by construction. This probe is the evidence record for
that choice; it does not modify any production source.
