"""
Reentering destination — TEST-ONLY contract for the H1 reentrancy regression
guard (tests/test_withdraw_reentrancy.py). NOT a fake Uniswap contract; not in
.auto/baseline_checksums.json.

Its only purpose: reenter `executor.withdraw(amount, destination)` from its
fallback when it receives ETH via raw_call (i.e. at the moment the executor's
`withdraw` does `raw_call(destination, value=amount)`).

Expected control flow (documented in .auto/h1-withdraw-trace.md):
  - Before raw_call: `WETH.withdraw(amount)` has burned executor's WETH and
    credited `self.balance` (call frame nested inside WETH.withdraw).
  - `raw_call(destination, value=amount)` debits `self.balance` BEFORE running
    destination's fallback. So at the reentry moment the executor's
    `self.balance == original_eth` (the transferred ETH has left) and
    `WETH.balanceOf(self) == 0` (burned).
  - The reentrant `withdraw(amount, ...)` reads both as low/zero -> the
    `amount <= eth + weth` assertion reverts -> the outer raw_call reverts ->
    the whole `withdraw()` reverts.

The reentrancy is therefore NOT exploitable for fund theft under the current
code. This test guards the invariant against future regressions.
"""

# Test-only interface to reenter executor.withdraw().
interface ReenterTarget:
    def withdraw(amount: uint256, destination: address): nonpayable

target: address
amount: uint256
reentered: bool


@deploy
def __init__(_target: address, _amount: uint256):
    self.target = _target
    self.amount = _amount
    self.reentered = False


@external
@payable
def __default__():
    if not self.reentered:
        # One-shot reentry: guard against infinite recursion if a future bug
        # ever allows the reentrant call to proceed.
        self.reentered = True
        extcall ReenterTarget(self.target).withdraw(self.amount, self)
