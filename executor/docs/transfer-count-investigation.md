# Transfer Count Investigation: Why the "4 transfer" Claim Was Wrong

> **Updated**: All three 5-transfer paths have been optimized to 4 transfers.
> See [`user-guide.md`](user-guide.md) §12 for the full 3-hop encyclopedia with
> updated transfer counts and [`pool-mechanics.md`](pool-mechanics.md) §8 for
> the summary table.

## Executive Summary

The previous session claimed all 27 three-hop paths reached ≤4 transfers. On-chain
event counting reveals 3 paths actually require 5 transfers:

| Path | Claimed | Actual | Root Cause |
|------|---------|--------|------------|
| V2-V3-V3 | 4 | **5** | V3 IIA requires physical USDC input transfer |
| V2-V3-V4 | 4 | **5** | V3 IIA requires physical USDC input transfer |
| V4-V3-V3 | 4 | **5** | V4 settle_delta produces a 2nd WETH→PM transfer |

Two separate bugs caused the false claim:

1. **No on-chain verification**: Transfer counts were derived from static analysis
   of the command byte stream, not from actual execution events. The static count
   missed implicit transfers (V3 callback payments, V4 settlement transfers).

2. **Theoretical reasoning error**: The analysis assumed V3's IIA could be
   satisfied without a physical transfer (like V2's excess-balance trick or V4's
   delta accounting). In reality, V3's `balance_before + amount_owed <= balance_after`
   check requires tokens to arrive *during* the callback, which can't be done
   via excess balance — it requires a physical transfer.

## Detailed Analysis

### Bug #1: V3's IIA Cannot Be Satisfied by Excess Balance

**V2's approach** (0 extra transfers): V2 checks K-invariant on *final balances*.
If tokens are already at the pair (excess balance from a prior V4_TAKE or
V2_SWAP_CALC), the K-invariant passes without a separate input transfer. Timing
doesn't matter — only the final state matters.

**V3's approach** (1 extra transfer): V3 checks `balance_before + amount_owed <=
balance_after` — a *delta* check. Tokens arriving before `balance_before` is
snapshotted (i.e., before `swap()` starts) are already counted in `balance_before`,
so they don't help satisfy IIA. Tokens must arrive *during the callback* between
the two snapshots.

This means V3 always requires a physical ERC20 transfer during its callback,
adding 1 unavoidable transfer per V3 middle leg.

**V4's approach** (0 extra transfers for same-pool-manager): V4 uses delta
accounting — no physical transfers at all. But cross-protocol hops (V4_TAKE to
a V3 pool) can satisfy V3's IIA during the callback, avoiding the extra transfer.

### Bug #2: V4_SETTLE_DELTA Requires a Physical Transfer

When the executor owes tokens to the PM (negative delta), `_v4_settle_currency()`
does `IERC20(WETH).transfer(PM, owed)` — a physical transfer that emits a
Transfer event. The previous analysis counted V4_SETTLE_DELTA as "0 transfers"
(delta accounting only), but it actually produces 1 transfer per negative delta.

In paths with V4 legs (e.g., V4-V3-V3), the profit capture (`ERC20_TRANSFER
WETH→PM`) and the delta settlement (`V4_SETTLE_DELTA WETH`) are two separate
transfers to the same recipient (PM). These *could* be merged into one transfer
of the full amount, reducing 5→4. This is an **implementation optimization**,
not a theoretical minimum.

### Per-Path Breakdown

**Paths at 5 transfers** (theoretical minimum is 5 for V2-V3-V3; 4 is achievable
for V4-V3-V3 and V2-V3-V4 with the settle merge optimization):

#### V2-V3-V3 (minimum = 5, no optimization possible)

| # | Token | From | To | Purpose |
|---|-------|------|----|---------|
| 1 | USDC | V2a | executor | V2 swap output (unavoidable) |
| 2 | WETH | V3c | V2a | V3c output (direct custody ✓) |
| 3 | WBTC | V3b | V3c | V3b output (reverse-order ✓) |
| 4 | USDC | executor | V3b | V3 IIA payment (unavoidable) |
| 5 | WETH | executor | V2a | Flash-loan repayment |

Transfer #4 is structurally unavoidable because:
- USDC must arrive *during* V3b's callback (IIA requirement)
- V2a already sent its USDC to executor (can't send to V3b directly due to
  timing: it would arrive before V3b.swap() starts, so balance_before would
  include it)
- No other source of USDC exists that can transfer during V3b's callback

**5 is the true theoretical minimum for V2-V3-V3.**

#### V2-V3-V4 (actual = 5, achievable = 4 with settle merge)

| # | Token | From | To | Purpose |
|---|-------|------|----|---------|
| 1 | USDC | V2a | executor | V2 swap output |
| 2 | WBTC | V3b | PM | V3b output (direct to PM ✓) |
| 3 | WETH | PM | V2a | V4_TAKE direct custody ✓ |
| 4 | WETH | PM | executor | V4_TAKE profit |
| 5 | USDC | executor | V3b | V3 IIA payment |

Transfer #5 is an unavoidable V3 IIA payment. #1 and #5 both move USDC
(V2a→executor then executor→V3b) but cannot be merged due to the same
timing issue as V2-V3-V3: V2a must send USDC to executor (the callback
recipient), and then executor sends it to V3b during V3b's callback.

#3 and #4 are both V4_TAKE WETH: PM→V2a and PM→executor. Merging them
into one TAKE to the executor (then executor sends to V2a) yields no
savings (2 transfers instead of 2). But if V2a uses the
WETH as excess balance, #1 (USDC V2a→executor) and #5 might be mergeable.

**5 is likely the true minimum for V2-V3-V4 as well** due to V3 IIA.

#### V4-V3-V3 (actual = 5, achievable = 4 with settle merge)

| # | Token | From | To | Purpose |
|---|-------|------|----|---------|
| 1 | WETH | V3b | executor | V3c swap output |
| 2 | WBTC | V3c | V3b | V3b swap output (reverse-order) |
| 3 | USDC | PM | V3b | V4_TAKE during V3b callback (satisfies IIA ✓) |
| 4 | WETH | executor | PM | Profit capture (ERC20_TRANSFER + sync + settle) |
| 5 | WETH | executor | PM | Delta settlement (V4_SETTLE_DELTA) |

Transfers #4 and #5 can be **merged into one** by sending the full AMOUNT_WETH
upfront (profit + principal) and skipping V4_SETTLE_DELTA. This is a test
implementation issue, not a theoretical limitation.

**4 is achievable for V4-V3-V3 with the settle-merge optimization.**

### Path Summary with Corrected Counts

| Path | Actual | Theoretical Min | Gap | Status |
|------|--------|----------------|-----|--------|
| V2V2V2 | 4 | 4 | 0 | ✓ optimal |
| V2V2V3 | 4 | 4 | 0 | ✓ optimal |
| V2V2V4 | 4 | 4 | 0 | ✓ optimal |
| V2V3V2 | 4 | 4 | 0 | ✓ optimal |
| **V2V3V3** | **5** | **5** | **0** | ✓ optimal (5 is the minimum) |
| **V2V3V4** | **5** | **5** | **0** | ✓ optimal — V4 pool is WETH\/WBTC, no USDC to TAKE for IIA |
| V2V4V2 | 4 | 4 | 0 | ✓ optimal |
| V2V4V3 | 4 | 4 | 0 | ✓ optimal |
| V2V4V4 | 3 | 3 | 0 | ✓ optimal |
| V3V2V2 | 4 | 4 | 0 | ✓ optimal |
| V3V2V3 | 4 | 4 | 0 | ✓ optimal |
| V3V2V4 | 4 | 4 | 0 | ✓ optimal |
| V3V3V2 | 4 | 4 | 0 | ✓ optimal |
| V3V3V3 | 4 | 4 | 0 | ✓ optimal |
| V3V3V4 | 4 | 4 | 0 | ✓ optimal |
| V3V4V2 | 4 | 4 | 0 | ✓ optimal |
| V3V4V3 | 4 | 4 | 0 | ✓ optimal |
| V3V4V4 | 3 | 3 | 0 | ✓ optimal |
| V4V2V2 | 4 | 4 | 0 | ✓ optimal |
| V4V2V3 | 4 | 4 | 0 | ✓ optimal |
| V4V2V4 | 4 | 4 | 0 | ✓ optimal |
| V4V3V2 | 4 | 4 | 0 | ✓ optimal |
| **V4V3V3** | **5** | **4** | **1** | ✗ fixable: merge settle_delta into profit transfer |
| V4V3V4 | 4 | 4 | 0 | ✓ optimal |
| V4V4V2 | 3 | 3 | 0 | ✓ optimal |
| V4V4V3 | 4 | 4 | 0 | ✓ optimal |
| V4V4V4 | 1 | 1 | 0 | ✓ optimal |

## Root Cause of the Previous Session's Error

1. **Static analysis instead of on-chain verification**: The previous session
   counted transfers by parsing the command byte stream (opcodes), not by counting
   on-chain events. This missed:
   - V4 settle_delta's implicit `IERC20.transfer()`
   - V3 callback's implicit IIA payment transfer

2. **Over-optimistic theoretical reasoning**: The claim "V3 IIA satisfied by
   tokens arriving during callback" was applied without distinguishing between
   tokens that arrive via V4_TAKE (which works — the transfer happens during
   the callback window) vs. tokens that arrive via excess balance (which doesn't
   work — they appear in balance_before). The analysis conflated these two cases.

3. **No assertion enforcement**: The test suite had no `_verify_transfers` calls
   at the time, so the claimed counts were never validated against actual
   execution.

## Action Items

- [ ] Apply V4-V3-V3 settle-merge optimization (5→4 transfers)
- [ ] Update docs/pool-mechanics.md with corrected counts
- [ ] Confirm V2-V3-V4 theoretical minimum (5 or 4?)

## Resolution (2026-05-31)

All three 5-transfer paths have been optimized to 4 transfers:

### V2-V3-V3 (5→4): V3c outermost, V2a inside V3b callback

**Missed optimization**: V2 called with `to=V3b` INSIDE V3b's callback.
V2a's optimistic USDC transfer hits V3b between the `balance_before`
and `balance_after` snapshots → IIA satisfied. No separate
executor→V3b transfer needed.

V2a uses `V2_SWAP_CALC` (no callback) because excess WETH was
pre-deposited by the executor before the V2 swap.

Prior approach: V2a was the outermost swap, sending USDC to executor,
requiring a separate executor→V3b transfer (5 transfers total).

### V2-V3-V4 (5→4): V3b outermost, V2a inside V3b callback

Same pattern: V3b as outermost swap, V4 unlock inside V3b callback
(providing WETH to V2a as excess), then V2a.swap(to=V3b) satisfies IIA.

### V4-V3-V3 (5→4): Merged WETH profit+settle into single transfer

The executor made TWO separate WETH→PM transfers:
1. `enc_erc20_transfer(weth, pm, AMOUNT_WETH_PROFIT)` (profit capture)
2. `V4_SETTLE_DELTA(weth)` → internally `IERC20.transfer(pm, remaining)` (settlement)

Merged into one: `enc_erc20_transfer(weth, pm, AMOUNT_WETH)` + sync + settle.
Saves 1 transfer with no functional change.

### Root Cause of the Missed Optimization

The previous session always started from V2 (the flash-loan source).
When V3 is a middle leg, V2's output goes to executor (not V3b),
necessitating a separate executor→V3b transfer for IIA.

**The fix**: start from V3 instead. V3 as the outermost swap means V2
can be called from inside V3's callback, with V2's `to` parameter
pointing to V3. V2's optimistic transfer then satisfies V3's IIA
during the callback window — the same "direct custody" trick already
used for V3→V3 and V3→V2 paths.

### Updated Transfer Count Table

ALL 27 paths now at ≤4 transfers. Total savings: 56 (35.9% from 156 naive).

> **Note**: Subsequent optimization rounds have further reduced some paths
> below the counts listed above. Notably V4-V3-V4 (4→3 via V4_TAKE_DELTA
> and tighter delta netting), V4-V4-V3 (4→3), and V4-V4-V4 (1→0–1 with
> V4_MINT). See [`user-guide.md`](user-guide.md) §12 and Appendix D for
> the current authoritative counts.
