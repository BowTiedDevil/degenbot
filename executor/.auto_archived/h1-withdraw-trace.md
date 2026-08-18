# H1 — `withdraw()` reentrancy: NOT EXPLOITABLE (validated empirically)

**Original finding (H1):** "Reentrancy: ETH delivered before `raw_call`, no guard —
`WETH.withdraw` delivers ETH to `self.balance` before `raw_call(destination)`
hands it to `destination`. A malicious `destination` reenters `withdraw` while
`self.balance` is inflated and `weth_balance` is lower → drains again."

**Verdict: NOT EXPLOITABLE.** The original reasoning made the same mistake as
the H2 spike: it stopped at "ETH is credited before the callback" without tracing
*when* `self.balance` is debited relative to the callback frame.

## Corrected control-flow trace

`withdraw(amount=150)` with `eth_balance=50, weth_balance=100`:

1. `assert 150 <= 50 + 100` ✓
2. `WETH.withdraw(100)`:
   - Burns 100 executor-WETH (`balanceOf[self] -= 100` → 0).
   - `raw_call(msg.sender=executor, value=100)` → `CALL` credits 100 ETH to
     the executor → `self.balance` rises to 150. The executor's `__default__`
     accepts it (no callback fires here — `destination` is NOT involved).
   - Frame returns; `self.balance == 150`.
3. `raw_call(destination, value=150)`:
   - `CALL destination` with value=150. The EVM **debits `self.balance` by 150
     before execution of `destination`'s code begins** — this is fundamental
     CALL semantics, not a Vypyism.
   - So at the moment `destination`'s fallback runs: `self.balance == 0`
     (debited), `WETH.balanceOf(self) == 0` (burned in step 2).
4. `destination`'s fallback reenters `executor.withdraw(150, destination)`:
   - `eth_balance = self.balance = 0`
   - `weth_balance = balanceOf(self) = 0`
   - `assert 150 <= 0 + 0` → **reverts (InsufficientBalance)**.
5. The revert unwinds the inner `CALL`, killing the outer `raw_call`, which
   reverts the whole `withdraw()` transaction. **Zero funds drained.**

The "donate ETH back to inflate on reentry" variant is also net-zero: any ETH
the attacker sends back to make the assert pass is the attacker's own ETH, and
the reentrant `raw_call(destination)` sends it right back to the attacker. Net
theft = 0.

## Why the original framing was wrong

It conflated "ETH is credited to `self.balance` during `WETH.withdraw`" (true —
step 2) with "ETH is credited to `self.balance` at the moment the `destination`
fallback runs" (false — step 3 debits it first). The two are separated by the
outer `raw_call`, whose CALL semantics debit value *before* executing the callee.

This is the same error class as the H2 spike: stopping the trace one frame short
of the decisive moment (in H2: forgetting the profit check reads *after* the
loop; in H1: forgetting CALL debits value before fallback execution).

## Empirical confirmation

`tests/test_withdraw_reentrancy.py` deploys a malicious `destination`
(`contracts/reentering_destination.vy`) whose fallback reenters
`executor.withdraw(amount, self)`. The test asserts:
- The reentry reverts (the whole `withdraw()` tx reverts).
- The executor's WETH balance is untouched post-tx (tx reverted).
- The attacker destination received nothing.

Test passes against the **unmodified** contract — confirming no fix is needed.

## Decision

**No code change in `withdraw()`.** The function is correct as written; the
"checks-effects-interactions violation" appearance is illusory because the
effect (ETH debit) and the interaction (callback) are atomic within a single
CALL.

The regression test + malicious-destination contract are kept as guards against
a future refactor that might split the WETH-withdraw from the ETH-send in a way
that *does* open the window (e.g. caching `amount` and forwarding it in a
deferred step).

## Artifacts

- `tests/test_withdraw_reentrancy.py` — regression test (passes on unmodified code).
- `contracts/reentering_destination.vy` — test-only malicious destination
  (NOT in `.auto/baseline_checksums.json`; not a fake Uniswap contract).
- This document.

## Status

H1 task (IDAXRF): CLOSED — retracted, no fix; regression guard retained.
