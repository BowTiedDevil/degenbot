# The V4 dynamic-fee fee census — what the hooked zero-fee pools charge

> **What this is.** The measurement behind phase 2's one open design decision:
> the search-time fee policy for hooked V4 pools (F2). It changes no behavior —
> the deliverable is the rig, the committed census fixture, and the numbers
> below. Rig: `rust/crates/engine/degenbot-simulation/tests/evm2_v4_dynamic_fee_census.rs`.
> Fixture: `rust/crates/engine/degenbot-simulation/tests/fixtures/v4_dynamic_fee_census.tsv`
> (generator SQL + seed in its header).

## The population

Every hooked V4 pool that charges no LP fee — the **32,358** mainnet pools with
`fee_currency0 = fee_currency1 = 0` and a non-zero `hooks` address in
`~/.local/state/degenbot/db/degenbot.db`. A pool keyed at fee 0 has no LP fee for
a hook to override, so the hook IS the whole charge, carried as a `beforeSwap`
amount delta (every address in this population carries the amount-modifying
flags, `hooks & 0xCC != 0`). The population spans 4,478 hooks; 24,098 of its
pools (across 3,017 hooks) hold at least one recorded liquidity position, and the
rest have nothing to price.

The search tier models these pools as standard concentrated liquidity, i.e. as
if they were free. This census says how wrong that is.

## Method

All quotes run at archived block **26,320,000** (the H1/ADR-090 anchor).

- **Parity** — the H1 primitive (the deployed V4 `Quoter` executed over an
  anchor-pinned evm2 scratch) against the node's `eth_call` of the identical
  calldata at the identical block, both directions, three amounts scaled to the
  input currency's decimals, compared to the wei.
- **Implied fee** — `1 − out(as traded) / out(same walk, nothing charged)`,
  where the reference walk is the SAME pool at the SAME anchor with the hook's
  code replaced by a no-op that answers `BeforeSwapDelta(0)`/`lpFeeOverride(0)`
  and touches no hook storage (asserted). The key's own fee is zero, so that
  twin is the pool's own curve — same tick data, same price, same liquidity.
- **Scale** — every fee reading is taken at **1/10 of a 1-bip walk** of the
  pool's own state (derived from `slot0`/`liquidity` through the engine's swap
  math), because an absolute token scale is dust on one pool and a price shock
  on the next. The amount-dependence ladder is 1/100, 1/10 and 1 bip of the same
  measure — a 100× span in trade size.

### The derivation's own accuracy

The estimator is a ratio of two walks, so it carries the walk's curvature as a
bias. Four hookless static-fee control pools (one per tier) size that bias: the
fee-free reference is the engine's own `compute_swap_step_v4` curve, counted
only where the same math at the identified fee reproduces the chain's quote **to
the wei** (4 readings; the deeper rungs of the `tick_spacing = 1/10` tiers leave
the current tick range, which the control reports instead of pretending).

**Worst bias measured: 0.55 bps** (0.5% relative) on the deepest rung, and
−0.02/−0.06 bps on the shallow rung — so every reading below is good to well
under 1% of the fee itself.

Two calibration findings fell out of the controls:

- A static pool's charge is `calculateSwapFee(protocol_fee_dir, lpFee)` — the
  closed form identifies 3,499 pips for a 3,000-pip pool (protocol fee 500) and
  10,990 pips for a 10,000-pip pool (protocol fee 1,000). **`lpFee` is the pool's
  `slot0.lpFee` word, and on a static pool that word IS the key's `fee`**: the
  deployed manager writes it at `initialize` from `key.fee`, and only a
  dynamic-fee pool's hook can move it (`updateDynamicLPFee` is hook-only and
  reverts unless `key.fee.isDynamicFee()`). The engine's key-fee composition is
  therefore exact on a static pool, and a dynamic-fee pool's charge is its
  recorded black-box measurement.

  *Correction — an earlier revision of this note read the override experiment as
  "`slot0.lpFee` is inert". It is not; that reading was a dust artifact.*
  `slot0_lp_fee_word_is_the_static_pool_swap_fee` in the rig rewrites the word
  through a reth `stateDiff` `eth_call` **and** the evm2 twin, reads it back
  through the manager's own `extsload`, and gets the quote the composed fee over
  the overridden word predicts, to the wei, on every priced leg and both arms —
  the 3,000-pip control's zero-for-one leg at 3,499 pips quotes 22,572,904,399;
  at `lpFee` 0 (500 pips) 22,640,802,522; at `lpFee` 30,000 (30,485 pips)
  21,961,924,858. The earlier reading came from a dust walk: a step charges
  `ceil(amount·fee/1e6)`, so at ~1e6 wei of input every fee in `(0, 1e6]` charges
  the same single unit and the quote is byte-identical for a reason that has
  nothing to do with the word. Override probes must price at a depth the step
  prices.
- The deployed manager validates hook responses by length AND selector: a
  `beforeSwap` must answer exactly 96 bytes (`selector || delta || lpFee`) and a
  delta-returning `afterSwap` exactly 64 bytes (`selector || delta`), else
  `InvalidHookResponse` (`0x1e048e1d`). The no-op twin hook is therefore a
  four-branch stub, not an empty account.

## Results

### 1. Parity holds exactly

80 candidate pools across 44 hooks (the four most-populous hooks, ten pools
each, plus one pool for each of 40 seeded long-tail hooks):

| | |
|---|---|
| quote points | 396 (2 directions × 3 amounts per pool) |
| answered on both sides | 282 |
| refused on both sides | 114 |
| **mismatches** | **0** |
| **max \|difference\|** | **0 wei** |

66 of 80 pools have in-range liquidity at the anchor (14 are thin), and 29 of 44
hooks priced at least one leg. Cold reads: 7,191 over the run; wall 3.2 s.

The black-box tier prices a dynamic-fee pool exactly. That was expected by
construction — the tier executes the hook — and is now measured.

### 2. What the hooks charge (implied fee, 1/10-bip walks)

| hook | pools in population | sampled | implied fee | refused walks |
|---|---:|---:|---|---:|
| `0x322dcec4…68cc` | 5,833 | 10 | 99.48–99.99 bps, one pool at 298.55 | 8 of 20 |
| `0x0d625293…c0cc` | 5,112 | 10 | **149.22 bps, all ten pools** | 10 of 20 |
| `0x96b89368…e0cc` | 2,239 | 10 | 99.48–99.99 bps | 9 of 20 (1 thin) |
| `0x07f17023…a0cc` | 1,289 | 10 | **124.35 bps, all ten pools** | 10 of 20 |
| 40 long-tail hooks | 17,885 (55%) | 40 | 29.85 · 99.48–99.99 · 228.82–230.00 · 298.47–300.00 · 328.32–328.41 (12 hooks) · 497.51 · 999.99 · 1699.99 · 2988.98–3000.00 · 4199.99 · one at 0.00 | 23 of 54 walks |

Totals: 74 walks measured, 58 refused, 14 pools thin. Implied fee **min 0.00
bps, median 149.22 bps, max 4,199.99 bps** over 73 readings. The four most
populous hooks cover **44.7% of the population at 1.0–1.5%**; the long tail spans
**0% to 42%** — one hook family (`0x780237dc…40cc`) charges nothing on one side
and 42% on the other, the widest per-direction split in the sample.

**Mechanism:** on all 74 measured walks the pool's settled `slot0.lpFee` word is
**0**. These hooks never override the pool's LP fee — every basis point is a swap
delta. (The flags confirm it: every address in this population carries
amount-modifying bits — `hooks & 0xCC != 0` — and none is keyed at the
`0x800000` dynamic-fee sentinel, which is the flag a hook overrides a fee with.)

**Fee levels are per-pool configuration, not per-hook constants.** Within the two
biggest hooks the charge is identical on all ten sampled pools (149.22 every
time), which is exactly what a factory-set constant looks like. `0x322dce` shows
the same family behaving differently per pool (nine at ~1%, one at 3%), and the
long tail spreads over two orders of magnitude.

### 3. Amount dependence: the level is flat, the estimator drifts quadratically

| pool / direction (one per top hook) | 1-bip amount | 1/100 | 1/10 | 1 bip | spread |
|---|---:|---:|---:|---:|---:|
| `0x002d39c871a3` / buy | 5.3e16 | 124.93 | 124.35 | 118.82 | −6.11 |
| `0x000ed1d2ab65` / buy | 5.3e16 | 149.92 | 149.22 | 142.60 | −7.32 |
| `0x002fe5a5cb10` / sell | 7.8e16 | 299.85 | 298.55 | 286.12 | −13.73 |
| `0x5d6493176df9` / buy | 1.5e17 | 99.94 | 99.48 | 95.04 | −4.90 |
| `0x5d6493176df9` / sell | 1.8e25 | 99.99 | 99.99 | 99.99 | 0.00 |

The charge does not step with size — there is no tier edge in the 100× span.
What does move is the ratio's own bias, **quadratically**: −0.45% relative for
10× the amount, −4.8% for 100×, the same shape across four different hooks and
both directions, and 0.00 where the walk is deep enough to have no curvature.
That signature (smooth, ∝ impact², identical across hooks) is the estimator, not
a fee schedule. This is why the census reads fees at 1/10-bip walks.

Not exercised: a *value*-tiered fee schedule whose threshold sits above 1 bip of
walk (two hooks' bytecode carries `Tiers must be sorted ascending` /
`Threshold must be non-zero` strings, so tier tables exist). The measured span
crosses no tier.

### 4. Time dependence: flat

Five anchors over 300 blocks — timestamps 1793537051 → 1793540663, a **3,612-second
hour** — on one pool per top hook, both directions:

**5 series priced, 5 perfectly flat, 0 moved.** Every reading repeats to the
hundredth of a bip (149.22 five times, 124.35 five times, 298.55 five times,
99.99 and 99.48 five times each). The charge is a configuration, not a
per-block oracle; no intraday refresh is needed to keep a recorded fee true.

### 5. Refusals are direction-level facts, not noise

58 of the 132 pool-direction walks (44%) cannot be priced at all, and every one
of them is refused identically by evm2 and by the node (parity includes the
verdict, not just the answer):

- **42** — `UnexpectedRevertBytes(0x7a5ed734 || poolId)`: the hook family's own
  error, raised from a contract the hook calls — the selector appears in the
  runtime bytecode of **0 of the 44 census hooks**, so it is never the hook's own
  `revert` statement. One swap direction on each of 42 pools: the launch-pad
  "sell" side.
- **15** — `UnexpectedRevertBytes(WrappedError(hook, …))`: the hook itself
  reverted inside the PoolManager's hook call.
- **1** — a bare `UnexpectedRevertBytes` with no inner payload.

A pool that refuses one direction **cannot be a hop in that direction**, whatever
its fee is. That is a stronger constraint on admission than the fee.

## F2: the search-time fee policy

**Recommendation: record the fee per pool (and per direction), measured once by
the black-box twin and refreshed with pool state; do not use a per-hook constant
and do not search these pools at fee 0.**

The numbers behind it:

- **A plain hook-caveat approximation (search at fee 0) is wrong by the entire
  charge** — 1.0–1.5% for 45% of the population, 0–42% on the long tail. Arb
  edges here run a few bps; a 124-bp error is 40× the entire edge. Searching
  these pools "as if free" manufactures false positives by construction, so the
  caveat has to be priced, not flagged.
- **A conservative bound kills the population.** The only bound that is honest
  across the census is the long tail's maximum, 42% — at which no candidate
  clears any realistic bar, including the 45% of pools that actually charge
  1.0–1.5%. A bound set at the top hooks' 1.5% is wrong for nineteen long-tail
  hooks (2.3%–42%) and for `0x322dce`'s 3% pool: it understates by 1.5× to 28×.
- **A per-hook constant is nearly right where it matters and wrong where it
  doesn't generalize.** It reproduces the four big hooks exactly (10/10 pools
  identical; 44.7% of the population) but misses `0x322dce`'s within-family
  spread, and the long tail is one pool per hook — a constant per hook is just a
  per-pool measurement with the measurement thrown away.
- **A recorded last-measured fee is exact and cheap, and it does not rot.** The
  quote it comes from IS the chain's answer (0 wei over 282 parity answers); the
  charge is flat over an hour (0/5 series moved), so a recorded value needs no
  per-block refresh — only the refresh pool-state drift already triggers. The
  marginal cost is one extra black-box frame per pool at the anchor (the no-op
  twin), on top of the re-quote ADR-090 already takes for a hooked finalist; the
  distribution pass priced 74 walks and their twins in 2.3 s over 5,425 cold
  reads, so the twin is not a budget item.

Two riders, both from the data: record the **direction mask** with the fee (58 of
the 132 walks here refuse a direction outright — 42 of them a whole pool
direction — and a refused leg must never enter a path), and treat "not yet
measured" as *not searchable* rather than as fee 0 —
the first is a cache miss, the second is a systematic overestimate.

Caveat: the fee is the charge at `hookData = ""`, the call shape every quoter
uses. A hook that keys its fee off `hookData` is outside what any generic quoter
can price, and this census cannot see it.

## Reproducing

```sh
export TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI='http://host.containers.internal:8545/'
cargo test --manifest-path rust/Cargo.toml -p degenbot-simulation \
  --test evm2_v4_dynamic_fee_census -- --ignored --nocapture --test-threads=1
```

Five `#[ignore]`d tests, ~6 s total against a local archive node:

- `parity_census_dynamic_fee_hook_legs` — (1)
- `implied_fee_distribution_per_hook` — (2)
- `implied_fee_derivation_validated_on_static_fee_pools` — the estimator's bias
- `implied_fee_amount_dependence` — (3)
- `implied_fee_time_dependence` — (4)

Each is its own census over the same fixture (no state crosses between them), so
a partial run is a partial report, never a wrong one.
