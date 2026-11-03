# The pre-sim gas floor: a measured per-op decomposition

> **Measurement note.** Every number below comes from
> `tests/evm2_gas_decomposition.rs` — real pool bytecode on evm2 over archive
> mainnet state at block **26,335,759** (the phantom corpus's own anchor). Re-run
> it with
>
> ```sh
> TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI=http://host:8545 \
>   just test-rust-nextest -p degenbot-simulation \
>   -E 'test(gas_decomposition_census)' --run-ignored=ignored-only --no-capture
> ```
>
> The V4 bitmap-word sweep and the shared-account experiment run on their own
> too (seconds, no corpus):
>
> ```sh
> TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI=http://host:8545 \
>   just test-rust-nextest -p degenbot-simulation \
>   -E 'test(v4_bitmap_word_coefficient)' --run-ignored=ignored-only --no-capture
> ```
>
> The rig prints its whole ledger (per-pool `k → gas` tables, the fits, the
> error distributions). Numbers drift with the anchor; the *shape* is the
> finding.

A candidate's cost is not a constant. A swap's gas is dominated by **how far the
walk travels through the pool's tick map**, which the solver already knows: the
number of initialised ticks crossed and the number of bitmap words walked. This
page measures those coefficients and turns them into an admission floor that
under-predicts cost (a candidate that clears the floor and fails verification is
a cheap mistake; a wrongly suppressed candidate is lost alpha).

## The decomposition

```text
gas(leg)  =  a_family                       # the pool's own fixed work
          +  b_cross  · initialised_ticks_crossed
          +  b_word   · bitmap_words_walked
          +  [ b_transfer · token_transfers ]     # settlement only, per hop
```

Measured coefficients (`gas`, from the frames described below):

| family | fixed `a` | per crossing `b_cross` | per word `b_word` | notes |
|---|---|---|---|---|
| **V2** | 30,862 | — (no tick map) | — | flat: 30,850–31,026 over 10^15…10^21 in |
| **V3** | 118,594 | 24,824 | 7,373 | vanilla pools: `a` 116,464–125,236 |
| **V3 fork** (1 bp, fork bytecode) | 147,897–151,109 | 33,238–46,913 | — | degenerate sweep: its swaps saturate one range |
| **V4** (hookless, black-box quote) | 67,995 | 19,922 | **5,684** | the whole `unlock`/delta path, quoter frame (the corpus fit rests on 6 samples: the four hookless corpus legs) |

Per-pool V3 fits (`gas = a + b·k`, 9 samples each, both directions, 0–8
crossings):

```text
usdc/weth-500  zfo  a=117,421 b=24,590   ofz  a=117,072 b=24,986
dai/usdc-100   zfo  a=117,522 b=25,265   ofz  a=116,464 b=24,632
usdc/usdt-100  zfo  a=125,236 b=24,573   ofz  a=121,594 b=24,530
dai/usdc-500   zfo  a=118,259 b=24,684   ofz  a=118,011 b=24,300
```

Two facts fall out of the sweep:

- **The per-crossing marginal dominates a multi-crossing leg.** 24.8k per
  crossing is ~21% of a zero-crossing leg: eight crossings more than double the
  leg's cost. A static wei floor cannot see this.
- **The word term is the bigger cost on a sparse pool.** One empty bitmap word
  costs 7,373 gas and an `EMPTY-HALT` walk crosses hundreds of them: the phantom
  corpus's 1 bp fork leg (`69150 hop1`) burns **3.16 M gas with 12 crossings and
  351 words** — a crossings-only estimator predicts 0.4 M and lets it through.

### The V4 word term is its own measurement

V4's bitmap lives inside the singleton `PoolManager`'s storage (every pool's
`Pool.State` is a mapping entry), so a V4 word is one `SLOAD` of an account a
path's every V4 leg already touches. Borrowing V3's 7,373 was therefore an
untested assumption: the same loop shape walks the word (`swap`'s
`nextInitializedTickWithinOneWord`), but neither the storage account nor the
per-word work is V3's.
The rig sweeps it: four hookless pools × both directions, amounts **bisected on
the chain's own word count** — the contiguous run of the pool's bitmap-word
slots in the frame's touched set (one `SLOAD` per word marched) — 66 samples
over words 1…10, crossings read the same way (one `tickBitmap`-word slot per
word, one `ticks(tick)` slot per `cross()`).

```text
pool (both directions)              words marched   samples
dai/usdc-100-s1                          1…10          20
usdc/weth-500-s10 (dense, spacing 10)    1…10          12
eth/usdc-100-s1                          1…8           14
usdc/usdt-100-s1                         1…10          20
```

`b_word` is identified at **constant crossings** — the ladder's own engineered
shape: past the last initialized tick the amount barely moves while the walk
marches empty words, so each level's `gas = a + b_word·words` fit has no
crossing term to trade off against.

```text
per-level marginals   5,534  5,645  5,606  5,680  5,705  5,832   (6 levels)
pooled (demeaned)     b_word = 5,684 ± 25 gas/word   (n = 40, R² 0.9992)
                      residual |gas| p50 218  p90 568  max 921
cross-check           3-parameter within-group fit: b_cross 16,431 ± 40,
                      b_word 5,641 ± 74  (n = 64, R² 0.9998)
```

The `EMPTY-HALT` shape is in the ladder (the `dai/usdc-100-s1` up side): past
its fourth crossing the amount rises only 2% (4.5482e20 → 4.6585e20) for six
more words while gas climbs ~5.4k per word and **no further crossing** happens.
That is the shape the floor exists to price, and its slope is the coefficient.

**Measured against the borrowed 7,373:** a V4 word costs **5,684 ± 25**, i.e.
**77%** of V3's rate — the borrow over-charges by 1,689 gas/word (23%), which is
16.9k on a 10-word march (~23% of a zero-crossing V4 leg). The word's own
composition is the same as V3's (a cold `SLOAD` at 2,100 plus the swap loop and
fee accounting), but the V4 loop's per-word work is ~1,700 gas cheaper — the two
families must not share a `b_word`.

### Cold vs warm bitmap words

The first crossing inside a bitmap word also pays the word's cold `SLOAD` and
bit walk; each further crossing in that word does not. Measured by comparing the
0 → 1 increment against the same pool's fitted marginal (median over 9 pools):

```text
first crossing in a word   38,595 gas
marginal crossing          24,591 gas   ->  cold-word surcharge ~14,004 (57% of the marginal)
```

At admission precision this matters only for legs that cross ≥2 ticks inside one
word (dense 1 bp pools): the marginal is what the floor should use, with the
per-word term covering the cold first touch.

### Transfers and the settlement frame

The V3/V4 frames below are **quote frames**: they run the pool's real swap loop
and then revert before settlement, so no token moves. A settled path adds one
ERC-20 `transfer` per direction per hop. Measured on real USDC (the pool's own
`balanceOf` slot probed and self-checked, then a funded `transfer` frame):

| transfer shape | frame gas | marginal (frame − 21 000 intrinsic) |
|---|---|---|
| cold recipient (0 → nonzero SSTORE) | 62,056 | **41,056** |
| warm recipient (nonzero → nonzero) | 44,956 | **23,956** |

So a settled V3 hop is `118,594 + 24,824·k + 7,373·w + ~24,000…41,000` — the
transfer pair is worth a fifth of a zero-crossing leg and must not be dropped
when the floor prices a *settling* path. A V4 hop settles inside one `unlock`
(`sync`/`take`/`settle`), which the 72,232 fixed cost already contains for the
quoter frame; the per-transfer marginal is the same order.

### The shared account: two V4 legs in one frame

Every V4 leg sits on the same `PoolManager`, so a path's V4 hops could in
principle amortize the manager's warmth. Measured — a hand-assembled batch
caller that forwards two quote calldatas to the canonical quoter inside ONE
frame, against the same legs quoted in two frames:

```text
shape                                        frame gas
A alone (own frame, dai/usdc-100-s1)            103,909
B alone (own frame, usdc/weth-500-s10)           67,030
A alone (through the batch caller)              108,581   (its own plumbing: 4,672)
A + B (one frame)                               150,421
A + A (one frame)                               187,300
```

Netting the 21,000 intrinsic the second transaction pays and the caller's own
4,672 plumbing leaves the quoter-and-manager execution:

```text
execution, two frames A+B    128,939
execution, one frame A+B     124,749   -> 4,190 cheaper for the pair (2,095/leg, 3.2%)
the same pool twice          165,818 vs 161,628
                              -> the same 4,190, so storage-slot sharing adds 0
```

Only the manager's **account and code** access amortizes (the second leg's
`CALL` into it finds a warm account: 2,600 → 100). No storage slot does: two V4
pools index different `Pool.State` bases, and the same-pool pair proves even one
pool re-pays its `slot0`/liquidity/bitmap `SLOAD`s in the next leg — the per-leg
quote reverts its own swap, and the warmth it paid goes with the revert. A
settled path (one `unlock`, nothing reverted) would keep it, but the only slot
that aliases two V4 legs is a shared currency's transient `CurrencyDelta`
(100 gas a touch).

**For the floor: do not model it.** The amortization is ~2,100 gas per extra V4
leg — 3% of a 72,232-gas leg, below the (b) estimate's p50 residual (15.7k) — and
dropping it over-predicts, which is the safe direction. Charge every leg its own
fixed cost.

## Error on the phantom corpus

The corpus (`tests/fixtures/phantom_paths.tsv`, 26 hops over 14 paths at the
run's anchor) is the held-out test set: 120 measured legs (V2/V3/V4, amounts
scaled to each pool's in-range reserve, dead legs included).

| estimate source | signed p50 | p90 | min | max | `|err|` p50 | `|err|` p90 | under-predicts |
|---|---|---|---|---|---|---|---|
| **(b)** census / walk counts | +6.5% | +29.8% | −93.4% | +38.7% | 10.0% | 29.8% | 40/120 |
| **(c)** `|Δtick| / spacing` | +30.5% | +65,507% | −94.9% | +87,251% | 38.7% | 65,507% | 26/120 |

Per family with (b): V2 `|err|` p50 **0.04%** (the constant is exact), V3 p50
17.6% / p90 29.8%, V4 p50 **1.4%** / p90 85.5%. The V4 median is as tight as V2's
now that the walk's census is the chain's (see *Rig self-checks*); the V4 p90 is
one pool (`69354 hop1`) whose frame gas climbs to 1.32 M at the top of its own
ladder on 1 crossing and 1 walked word — a per-amount fixed cost outside the
two-term form, not an axis or parity error (its walk answers the chain exactly).

**Why V3 sits at +16%: the intercept is a pool property, not a family constant.**
The family fit's `a` (118,594) is the *median* pool; the cheapest corpus pool
runs at 96,909 and a 1 bp fork pool at 147,897. A median intercept
over-predicts the cheap pools — the expensive error. The floor variant keeps the
marginals and drops `a` to the 10th percentile of the corpus's per-pool
intercepts:

```text
floor variant (a = 81,947, b_cross = 24,824, b_word = 7,373 | V4 5,684)
    signed p50 −13.9%   max +33.7%   under-predicts 93/111 legs (V3+V4)
```

Two admissible choices, both wrong in the safe direction:

- **The low intercept** — family marginals, `a` at the p10 of the observed
  per-pool intercepts (81,947). Under-predicts 84% of the corpus's V3+V4 legs;
  the 16% it over-predicts are pools with a dearer fixed cost (fork bytecode,
  proxy tokens) and are over-predicted by ≤34%. The marginals are **per
  family**: `b_word` is 7,373 for a V3 leg and 5,684 for a V4 one (the corpus
  figures above were measured with V3's rate charged to both, so they
  over-predict a V4 leg by 1,689 gas per word walked — a few percent, and the
  safe direction).
- **Percentile floor** — `est = pred + q·residual`: at q10 of the (b) residual
  the estimate sits 6% below the fitted prediction and under-predicts ~90% of
  legs; at q05 it sits 9.1% below and under-predicts ~95%.

**Recommendation: floor with the p10 residual of the point estimate** (i.e. the
estimate is a 10th-percentile cost, not an expected cost). The measured
suppression cost is then ~10% of candidates that would have passed the sim — the
same class the static floor already lets through, and orders of magnitude cheaper
than the 322k wasted sims/7 h that motivated the floor.

## The crossing source: what admission should use

| option | what it costs | measured |
|---|---|---|
| **(a) carry the count on the walk's outcome** | zero extra work: the walk's landing index and its landing fill's word steps both exist; the range sequence's exit kinds turn them into the two counts (`SwapWalkCensus`), which `ConcentratedLiquidityPathSim::output` now surfaces | exact — it *is* the walk's loop count; validated against the chain census on every modeled sweep sample (59/59) |
| **(b) recompute from the in-memory tick map at admission** | O(initialised ticks between the hop's ticks) with the bitmap already in memory; needs the final tick, which the family sim returns | exact, but repeated work the walk already did |
| **(c) `|tick_final − tick_initial| / spacing`** | free (the solver has both ticks) | **rejected** |

Option (c) is unusable: on the corpus its ratio to the true initialised count is
**p50 273×, p90 6,587×, max 12,470×**. The physics is plain once measured — a
swap that exits the in-range position travels through *empty* tick space, so the
tick delta grows without bound while the number of ticks actually crossed does
not (`73502 hop1`: 27 crossings over 168,590 ticks of travel). At 24.8k per
crossing, (c) over-estimates a leg by ~4×10⁹ gas — every candidate would be
suppressed.

**Verdict: (a), with (b) as the fallback.** Both counts the formula needs are
already in the walk's hands, and `ConcentratedLiquidityPathSim::output` now
carries them: each hop's `Option<SwapWalkCensus>` states
`initialized_ticks_crossed` and `bitmap_words_walked` plus the `bounded_tail`
caveat. The rig reads them exactly that way:

```rust
let outcome = sim.output(amount);                 // ConcentratedLiquidityPathSim
let census = outcome.census[hop]                  // SwapWalkCensus
    .expect("a state-built hop carries range exit kinds");
let crossings = census.initialized_ticks_crossed; // == the chain's cross() count
let words = census.bitmap_words_walked;           // == the chain's word count
```

## The chain's own crossing count (`QuoterV2`)

`QuoterV2` returns `initializedTicksCrossed`; the rig compared it against its own
bitmap census on every canonical sweep sample:

```text
36 samples: the quoter's count == the census, 36 samples: == census + 1
            0 samples: anything else
```

The +1 is a **convention, not noise**: the quoter counts the tick the price
starts in when it is initialised, which the pool's `cross()` never runs on that
swap. So the chain figure *over-counts by exactly one* whenever the current tick
is a liquidity boundary — ~24.8k gas, a fifth of a zero-crossing leg. Any
estimator fed by the quoter's number inherits that bias; the walk's landing index
does not.

## Rig self-checks (what the numbers rest on)

- **The frame is the pool's own bytecode.** V3 legs execute `pool.swap` from a
  quote caller whose `uniswapV3SwapCallback` reverts with
  `(amount0Delta, amount1Delta, sqrtPriceX96After, tickAfter)`; the last two words
  come from a `slot0()` staticcall *inside* the callback, and the rig prints the
  `CALLER`/pool identity as a self-check on every pool (`ok=true` on all ten).
  Address-agnostic: it prices fork pools no canonical quoter routes to.
- **The census is the node's storage.** Crossings are counted from
  `tickBitmap(int16)` bits between the pre-swap and post-swap tick, not from the
  estimator under test.
- **The canonical quoter cross-checks the frame** on every pool it serves: the
  same point is quoted through `QuoterV2` and the pool's own `swap()` and the
  crossing counts/census are compared (the +1 finding above).
- **V4 legs are quoted through the black-box primitive** (`V4BlackBoxQuoter`),
  whose `gas_used` is the whole `unlock`/delta frame. Its walk answers the
  chain's `amountOut` exactly on every swept pool, the corpus's fee-82 pool
  (`69354 hop1`) included: re-measured at this commit that leg is exact on all
  five probes (`phantom_corpus_parity`: 26/26 hop parities, 0 findings). The V4
  error tail is therefore not a parity or axis problem — it is that same pool's
  frame gas jumping to 1.32 M at the top of its ladder (1 crossing, 1 word), a
  per-amount cost the two-term model cannot express.
- An `EMPTY-HALT` leg (a swap that leaves the liquidity and marches to the price
  limit) exhausts the 16 M frame cap; the rig records that as a finding rather
  than a sample, because its gas is bounded by the frame, not by the path.
- **V4's word and crossing axes are the frame's touched slot set**: one
  bitmap-word slot per word marched, one `ticks(tick)` slot per `cross()`. The
  white-box walk now reproduces both counts from its own outcome
  (`SwapWalkCensus`, `degenbot_pools`): on 59 of the 66 swept samples its
  `initialized_ticks_crossed` / `bitmap_words_walked` equal the frame's counts
  exactly, in both directions. The 7 exceptions are the walk's **bounded
  tail**: its range sequence stops at the last initialized tick, so a swap that
  leaves the book (the `EMPTY-HALT` march — corpus pool `usdc/weth-500-s10`,
  `zfo`) goes further than the walk can see. The outcome declares
  `census.bounded_tail` and both counts are lower bounds there, which the rig
  checks as bounds; the tail's true word count is only readable from the frame.
  The projected counts were previously wrong for a different reason (the range
  index is not the crossing count, and a landing range's whole word list is not
  the words walked — see the census's own docs in
  `state/int_v3_hop.rs`). The V4 coefficient above is fitted on the chain's
  counts.

## What the admission floor should consume

1. **Admission floor** (pre-sim, per path): sum over hops of
   `a_family + b_cross·crossings + b_word·words`, plus the per-transfer marginal
   when the path settles, times the block's effective gas price, compared against
   the solver's gross margin.
2. **Coefficients as data**, not constants in code: the family table above is the
   seed, and the per-pool intercept spread (96.9k–151.1k) is the reason the floor
   uses the low end.
3. **The crossing counts come from the walk** (option (a)): the path sim's
   outcome carries a per-hop `SwapWalkCensus` (initialized ticks crossed, bitmap
   words read), computed from the walk's own landing plus the range exit kinds —
   validated against the chain's touched set on 59 of 59 modeled V4 sweep
   samples. A `bounded_tail` census is a lower bound (the `EMPTY-HALT` march
   leaves the walk's modeled span), so the floor must treat it as the
   under-predicting side it already is.
4. **Never (c).** Tick-distance is not crossing count: 273× at the median, 12,470×
   at the tail.
