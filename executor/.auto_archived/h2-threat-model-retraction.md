# H2 — RETRACTED as standalone fix (under stated operating model)

**Original spike verdict (EXPLOITABLE → full drain) was wrong.** Retracted after
re-examining the owner gate on `execute`.

## Why the original claim was wrong

1. `execute` is `OWNER_ADDR`-gated; `commands` + `config` are entirely the
   owner's input. A reentrant callback can only inject commands the owner
   could already have encoded directly. Reentrancy adds no new capability to
   an honest owner's path.

2. The profit check (`combined_after = WETH.balanceOf(self) + self.balance`,
   asserted `>= expected_value`) runs **after** the command loop, i.e. after
   every callback frame has unwound. Any reentrant theft (e.g.
   `ERC20_TRANSFER(WETH→pool)`) **lowers** `combined_after`, making the profit
   check fail *harder*, not softer. `execute` reverts; owner loses gas, not funds.

3. The bribe computation reads the same `combined_after`, so it is also bounded
   correctly — a reentrant send-out lowers `profit`, never inflates it.

## Residual risk (the only one that matters)

A malicious pool that *donates* WETH to the executor inside a reentrant
`ERC20_TRANSFER` could inflate `combined_after` and mask a losing path. This is
a **reentrancy-delivered variant of the H3 (donation) vector**, not a distinct
H2 attack. Canonical immutable Uniswap V2/V3 pairs never make inbound
`ERC20_TRANSFER`s to the executor during a callback — they call back only via
the documented flash-callback selectors. So under the stated model
(canonical pools only), even this residual is closed.

## Decision (under user's stated model: immutable canonical Uniswap V2/V3/V4)

**Do not ship the H2 hash pin.** It costs ~800-960 gas/path for defense-in-depth
against a vector that requires (a) a non-canonical/malicious pool AND (b) only
defeats the donation sub-case — both outside the stated model.

## Follow-up actions (transferred to other tasks)

1. **SECURITY_REVIEW.md**: document as a stated security invariant:
   "The executor assumes all swaps target immutable canonical Uniswap V2/V3/V4
   pools. The owner gate + post-loop profit check make reentrant callbacks
   wealth-neutral under an honest owner; pool-list poisoning yields a reverted
   execute (gas loss only)."
2. **H3**: extend the donation-hardening consideration to cover inbound
   ERC20_TRANSFER from any non-PM, non-callback-pool source (covers the
   reentrancy-delivered donation sub-case cheaply in one place, rather than
   via the H2 hash pin).

## Status

H2 task (WMYZ64): CLOSED — retracted. The H2 threat-model spike artifact is
retained for the attack-trace record, but its EXPLOITABLE verdict is superseded
by this retraction under the stated operating model.
