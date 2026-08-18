"""
H1 reproduction / regression guard for withdraw() reentrancy.

This test was written to *validate or refute* the H1 finding in the security
review: "withdraw() is reentrancy-vulnerable — ETH from WETH.withdraw lands in
self.balance before raw_call(destination), so a reentrant withdraw drains again."

Based on the control-flow trace (documented in `.auto/h1-withdraw-trace.md`),
the described exploit does **not** hold: by the time `destination`'s fallback
fires, `self.balance` has already been debited by the outer `raw_call`, and
`WETH.balanceOf(self)` is already burned. A reentrant `withdraw(amount)` fails
the `amount <= eth + weth` assertion.

This test encodes a malicious `destination` whose fallback reenters `withdraw`
and asserts the reentry reverts (and that no extra funds are drained). It is a
regression guard against any future change that might inadvertently open the
window (e.g. a codepath that credits `self.balance` *after* the raw_call).
"""

import pytest

from .conftest_shared import ZERO_ADDRESS

AMOUNT_WETH = 10 * 10**18


@pytest.fixture
def attacker_dest(project, owner_account, executor):
    """A malicious destination whose fallback reenters executor.withdraw()."""
    # Reserve slot 0 amount, slot 1 executor, slot 2 (bool) reentered flag
    contract = project.reentering_destination.deploy(
        executor.address, AMOUNT_WETH, sender=owner_account
    )
    return contract


class TestWithdrawReentrancyGuard:
    def test_withdraw_reentry_reverts(self, weth, owner_account, executor, attacker_dest):
        """
        Executor holds WETH only (no ETH). Owner calls withdraw(amount, attacker_dest).
        Expected flow:
          1. eth_balance=0, weth_balance=AMOUNT_WETH; assert passes.
          2. WETH.withdraw(AMOUNT_WETH) -> ETH credited to executor, WETH burned.
          3. raw_call(attacker_dest, value=AMOUNT_WETH) -> attacker's fallback runs.
             attacker reenters executor.withdraw(AMOUNT_WETH, attacker_dest).
             At reentry: self.balance = 0 (debited), weth = 0 (burned) -> assert reverts.
          4. Outer raw_call reverts -> whole withdraw() reverts.
        Pre-/post-fix, the reentry MUST revert. The executor MUST NOT lose more
        than AMOUNT_WETH (in fact it loses 0: the whole tx reverts).
        """
        # Fund the executor with WETH (no ETH).
        weth.mint(executor.address, AMOUNT_WETH, sender=owner_account)

        owner_weth_before = weth.balanceOf(owner_account)
        executor_weth_before = weth.balanceOf(executor.address)
        assert executor_weth_before == AMOUNT_WETH

        # The reentrant withdraw should revert the whole transaction.
        with pytest.raises(Exception):
            executor.withdraw(
                AMOUNT_WETH,
                attacker_dest.address,
                sender=owner_account,
            )

        # Post-conditions: executor's WETH untouched (tx reverted).
        assert weth.balanceOf(executor.address) == AMOUNT_WETH
        # Attacker got nothing.
        assert (
            weth.balanceOf(attacker_dest.address) == 0
        )
        # No extra WETH left the executor beyond the attempted amount (it didn't even leave).
        assert weth.balanceOf(owner_account) == owner_weth_before

    def test_withdraw_honest_path_unaffected(
        self, weth, owner_account, executor, accounts
    ):
        """
        Sanity: an honest destination (a plain EOA) can still withdraw.
        This guards against a fix that over-restricts the honest path.
        """
        weth.mint(executor.address, AMOUNT_WETH, sender=owner_account)
        honest_dest = accounts[5]
        bal_before = honest_dest.balance

        # initialize() mints 1 wei of WETH for ERC6909 slot warmup, so the
        # executor retains ~1 wei after withdrawing the AMOUNT_WETH it was
        # funded with. Tolerate that dust (see L2 finding).
        executor.withdraw(AMOUNT_WETH, honest_dest.address, sender=owner_account)

        assert weth.balanceOf(executor.address) <= 1
        assert honest_dest.balance == bal_before + AMOUNT_WETH
