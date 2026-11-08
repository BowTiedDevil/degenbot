# Arithmetic Profit Tracking — Implementation Plan

> **Conclusion**: Do not implement arithmetic profit tracking. See [`user-guide.md`](user-guide.md)
> §14 for the current profit-check implementation using the `expected_balance` parameter.
> The analysis below explains why arithmetic tracking regresses gas.

## Motivation

The current profit check reads `WETH.balanceOf(self) + self.balance` at start
and end of execution to compute profit. Each `balanceOf` is ~185 gas when
WARM (4,700 when COLD). The `expected_balance` parameter already eliminates
the "before" read, but the "after" read still costs ~185 gas on most paths.

**Proposal**: Replace the `balanceOf` read with an arithmetic counter
accumulated in transient storage. On every WETH/ETH inflow and outflow,
add/subtract the amount to a running `t_profit` counter. At the end, the
counter holds the net WETH+ETH change — which IS the profit.

**This works because of the WETH/Ether-only custody invariant**: the
executor never takes custody of any other ERC20 token. All intermediate
flows are WETH or ETH. (See AGENTS.md and docs/pool-mechanics.md.)

## Where WETH/ETH Flows Occur

### Inflows (add to counter)

| Source | Command(s) | Amount available |
|--------|-----------|-----------------|
| V2 callback | `uniswapV2Call`, `hook`, `pancakeCall` | `amount0 + amount1` (one is zero, the other is WETH) |
| V3 callback | `uniswapV3SwapCallback`, `pancakeV3SwapCallback` | `amount` param (always WETH by invariant) |
| V4 TAKE (WETH) | `V4_TAKE`, `V4_TAKE_COMPACT`, `V4_TAKE_DELTA` | `amount` when `currency == WETH_ADDR` |
| V4 TAKE (ETH) | `V4_TAKE`, `V4_TAKE_COMPACT`, `V4_TAKE_DELTA` | `amount` when `currency == NATIVE_ADDRESS` |
| V4 MINT (WETH) | `V4_MINT_COMPACT` | `amount` when `currency == WETH_ADDR` |
| WETH deposit | `WETH_DEPOSIT`, `WETH_DEPOSIT_ALL` | net-zero (ETH→WETH, no combined change) |
| WETH received via `ERC20_TRANSFER` | `ERC20_TRANSFER` | amount when `token == WETH_ADDR` |
| ETH received | `__default__` | `msg.value` |

### Outflows (subtract from counter)

| Sink | Command(s) | Amount available |
|------|-----------|-----------------|
| V2 auto-pay | `_v2_auto_pay` | `owed` (WETH transferred to pair) |
| V3 auto-pay | `uniswapV3SwapCallback`, `pancakeV3SwapCallback` (auto-pay path) | `amount_owed` (WETH transferred to pool) |
| V4 settle (WETH) | `V4_SETTLE`, `V4_SETTLE_DELTA`, `V4_SETTLE_ALL` | WETH amount owed to PM |
| V4 settle (ETH) | `V4_SETTLE`, `V4_SETTLE_DELTA`, `V4_SETTLE_ALL` | ETH amount owed to PM |
| V4 BURN (WETH) | `V4_BURN_COMPACT` | amount when `currency == WETH_ADDR` |
| WETH withdraw | `WETH_WITHDRAW`, `WETH_WITHDRAW_ALL` | net-zero (WETH→ETH, no combined change) |
| WETH sent via `ERC20_TRANSFER` | `ERC20_TRANSFER` | amount when `token == WETH_ADDR` |
| ETH sent | `SEND_ETH`, `SEND_ETH_ALL` | ETH amount |
| Bribe | `execute()` post-processing | bribe_amount (ETH but handled separately) |
| WETH deposit for V4 settle | Inside `_v4_settle_currency` | net-zero (ETH→WETH→PM, no combined change) |

### Net-zero operations (skip)

These move value between WETH and ETH within the executor — the combined
WETH+ETH balance doesn't change:

- `WETH_DEPOSIT` / `WETH_DEPOSIT_ALL` — ETH becomes WETH
- `WETH_WITHDRAW` / `WETH_WITHDRAW_ALL` — WETH becomes ETH
- `WETH.deposit()` inside `_v4_settle_currency` — ETH becomes WETH to pay PM

## State Design

```vyper
# One transient uint256 for the running WETH+ETH counter
t_profit: transient(uint256)
```

Only one transient slot. 100 gas per TSTORE, 100 gas per TLOAD. We expect
~5–10 accumulations per path (inflows + outflows), so ~500–1,000 gas in
tracking overhead vs ~185 gas saved on the removed `balanceOf` read.

**Wait — this doesn't add up.** 500 gas in TSTOREs vs 185 gas saved from
one `balanceOf` removal. We need the counter to save gas overall.

## Rethinking: Where Does the Saving Come From?

The real question is: what does `t_profit` replace?

**Current slow path** (when `expected_balance > 0` or `bribe_bips > 0`):
```
combined_after = WETH.balanceOf(self) + self.balance  // ~185 gas when warm
assert combined_after >= expected_balance  // reverts with InsufficientProfit(actual, expected)
profit = combined_after - combined_before
```

**With t_profit**:
```
profit = t_profit  // 100 gas TLOAD
assert profit + combined_before >= expected_balance  // reverts with InsufficientProfit(actual, expected)
```

The saving is only ~85 gas (one `balanceOf` read minus one `TLOAD`). But
we added ~5–10 TSTOREs (500–1,000 gas) throughout the path. **Net regression.**

**However**, the slow path currently runs for EVERY path when `bribe_bips > 0`
or `expected_balance > 0`. In the fast path (`expected_balance == 0 and
bribe_bips == 0`), there is NO balanceOf read — just the command loop and
return. The `t_profit` TSTOREs would ALSO run on the fast path, adding
~500–1,000 gas to every path for no benefit.

**Conclusion: Arithmetic tracking is a net regression for the current
architecture.** The TSTORE overhead exceeds the `balanceOf` savings on every
path — and on the fast path (majority of production calls), it's pure
overhead with zero benefit.

## When Would This Be Worth It?

Arithmetic tracking would win if:

1. **The fast path is eliminated** — i.e., every call needs a profit check.
   Then the `balanceOf` saving is realized on every path (~185 gas), but
   the TSTOREs still cost ~500–1,000 gas. Still a net regression.

2. **WETH/ETH is cold** — On V4V4V4, the first `balanceOf` costs ~4,700 gas.
   Arithmetic tracking would save ~4,600 gas on that path. But V4V4V4 only
   has 3–4 WETH/ETH flows, so ~300–400 gas in TSTOREs. Net saving: ~4,200
   gas. But only for V4V4V4, and only when WETH is cold (first tx in block).

3. **We track fewer flows** — Instead of tracking every inflow/outflow, we
   could track ONLY the net WETH+ETH delta. But the net delta IS the profit,
   which is what we're trying to compute. We can't know it in advance.

## Alternative: Conditional Tracking

Only accumulate `t_profit` when `need_balance == True` (i.e., on the slow
path). This means the fast path has zero overhead. On the slow path:
- Add ~500–1,000 gas in TSTOREs
- Save ~185 gas from `balanceOf`
- Net: +315 to +815 gas regression per slow-path call

Still not worth it.

## Alternative: Track Only in Callbacks (Skip Commands)

Another angle: the V2/V3 callbacks ALREADY know the WETH amounts. What if
we accumulate ONLY in the callback handlers (3 TSTOREs for V2, 2 for V3)
and skip tracking in the command handlers?

Problem: outflows from commands like `ERC20_TRANSFER(WETH, ...)` and
`SEND_ETH(...)` would be missed. The counter would overcount.

## Final Verdict

**Do not implement arithmetic profit tracking.** The WETH/Ether-only
custody invariant makes it theoretically possible, but the gas economics
don't work: TSTORE overhead exceeds the `balanceOf` savings on every
path configuration. The `expected_balance` function parameter already
eliminates one of the two `balanceOf` reads, and the remaining read is
cheap (~185 gas when warm, which is almost always the case in multi-hop
paths that touch WETH).

The one scenario where it would win — V4V4V4 with cold WETH — is a
minority path (6.3% of the benchmark total) and only cold for the first
tx in a block. Not worth the complexity.

---

## Appendix: Full Flow Analysis Per Path Type

For reference, here's how many WETH/ETH TSTOREs each path type would
require, and the current `balanceOf` cost on the slow path:

| Path type | WETH/ETH inflows | WETH/ETH outflows | Net-zero ops | TSTORE count | balanceOf cost (warm) | Net delta |
|-----------|-----------------|-------------------|--------------|-------------|----------------------|-----------|
| V2V2V2    | 3 (V2 callbacks) | 3 (V2 auto-pay)   | 0            | 6           | ~185                 | −415      |
| V2V2V3    | 2 (V2) + 1 (V3) | 2 (V2) + 1 (V3)   | 0            | 6           | ~185                 | −415      |
| V2V2V4    | 2 (V2) + 1 (V4) | 2 (V2) + 1 (V4)   | 0            | 6           | ~185                 | −415      |
| V2V3V2    | 2 (V2) + 1 (V3) | 2 (V2) + 1 (V3)   | 0            | 6           | ~185                 | −415      |
| V3V3V3    | 3 (V3 callbacks) | 3 (V3 auto-pay)   | 0            | 6           | ~185                 | −415      |
| V4V4V4    | 1 (V4 take)     | 1 (V4 settle)     | 0            | 2           | ~4,700 (cold)        | +4,415*   |

\* V4V4V4 is the only path where arithmetic tracking wins, but it's only
cold on the first tx in a block, and the path represents 6.3% of the
benchmark. Average saving across all paths: negative.

**The `expected_balance` parameter is the right solution** — it eliminates
one `balanceOf` read with zero runtime overhead on any path. The remaining
`balanceOf` read (end-of-execution) is already cheap in the common case.
