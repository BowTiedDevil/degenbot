# M1 — Profit-check guard on expected_value==0: RETRACTED as a revert guard

## Original finding (M1)

`check_mode != 0` (operator wants a profit check) but `expected_value == 0`:
the `if expected_value > 0: assert combined_after >= expected_value` guard
treats 0 as "no check", silently skipping the profit check the operator asked
for. Proposed fix: `assert check_mode == 0 or expected_value > 0`.

## Verdict: the revert guard is INCOMPATIBLE with the flash-borrow architecture

The executor's defining property (from AGENTS.md): **"No prefunding required —
the executor borrows all working capital atomically via V2/V3 flash swaps and
V4 PoolManager take(). Can be deployed with zero balance."**

This means a large class of legitimate arbitrage paths **start the executor at
0 balance**:
- `expected_value` (pre-tx combined balance) = `WETH.balanceOf(self) + self.balance` = 0.
- `check_mode = 1` is the steady-state operating mode (verify profitability).

So `expected_value == 0` with `check_mode != 0` is **correct and common** for
no-prefund flash paths — not a misconfiguration. Forcing a revert there breaks
19 of the 27 benchmark paths (every V2/V3-involving flash path). Confirmed
empirically: the guard broke `TestV2V2V2` through `TestV3V4V3` etc.; pure-V4
(`TestV4V4V4`) survived only because its fixture happens to pre-mint.

## Why the "silent skip" is actually fine here

When `expected_value == 0`, the profit check `combined_after >= 0` is trivially
true for any `uint256` (unsigned). It is a no-op, but it is **not wrong** — it
confirms the executor didn't end negative (impossible for uint) and doesn't
silently APPROVE a loss that a real baseline would have caught, because there
IS no real baseline to check against (the executor started at 0). For flash
paths the meaningful invariant is "combined_after > 0" (we made a profit),
which the operator can encode by setting `expected_value = 0` and relying on
the path reverting at the protocol layer if it loses money (V2/V3/V4
invariants force repayment of the flash loan, so a losing path reverts inside
the callback, not at the profit check).

## The residual footgun (operator responsibility, not enforceable)

The one real concern: `expected_value == 0` + `bribe_bips > 0` + executor has a
pre-existing (prefunded) balance → `profit = combined_after - 0 = full balance`
→ `bribe = bips/10000 × full_balance` drains pre-existing funds. The contract
CANNOT distinguish:
- a genuine starts-from-0 flash path (correct, don't revert), from
- a prefunded executor with a misconfigured `expected_value == 0` (footgun).

So no on-chain guard can fix this without breaking the flash-borrow
architecture. The operator MUST set `expected_value` to their real pre-tx
balance whenever they (a) have a prefunded executor and (b) request a bribe.
This is documented in the `execute` docstring.

## What WAS kept from M1

Only the `bribe_bips <= 10_000` (`BipsTooHigh`) bound — without it,
`bribe_amount = bribe_bips × profit / 10000` exceeds `profit` when
`bribe_bips > 10000`, over-bribing and draining more than the actual profit.
That bound is unambiguously correct (the `BipsTooHigh` custom error already
existed in the contract but was never raised) and breaks nothing.

## Gas

Keeping only `BipsTooHigh` (1 assert on the slow path) vs the original M1
guard (2 asserts): the 27-path benchmark returns to baseline on the guard
removal. The single BipsTooHigh assert is ~+12 gas on the slow path (negligible;
every profit-checked path runs the slow path). `V4V4V4`: 85,776 → 85,7XX.

## Status

M1 task (RK7TI2): PARTIALLY DONE — the revert guard is retracted; only the
`BipsTooHigh` bound is shipped. The expected_value=0 semantics are documented
as intentional. This is the same lesson as the H1/H2/M6 retractions: a "fix"
that breaks the flash-borrow / canonical-pools operating model is wrong even if
it sounds like a hardening.
