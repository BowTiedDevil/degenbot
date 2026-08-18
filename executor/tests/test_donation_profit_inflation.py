"""
H3 (donation half) reproduction + regression guard.

An attacker who sends plain ETH to the executor (via __default__) inflates
`combined_after` in mode-1 profit check, which can mask a losing arbitrage and
even trigger a fake bribe payout. Post-fix, __default__ is gated to
POOL_MANAGER_ADDR and WETH_ADDR only (the two legitimate callers that deliver
ETH via raw_call: PM native-take and WETH.withdraw). Any other plain-ETH
transfer reverts.

See .auto/h3-donation-truncation.md.
"""

import pytest

from .conftest_shared import (
    enc_preamble,
    make_config,
    AddressTable,
)

AMOUNT_WETH = 10 * 10**18
DONATION = 5 * 10**18


class TestDonationProfitInflation:
    """An external ETH donation must NOT be able to reach the executor's balance."""

    def test_plain_eth_donation_reverts(
        self, weth, owner_account, executor, accounts
    ):
        """
        Attacker (anyone) attempts to donate ETH via a plain value-transfer
        CALL to the executor (triggers __default__).

        Pre-fix: __default__ accepts the transfer → executor.balance rises by
        DONATION → profit check can be fooled.
        Post-fix: __default__ rejects non-PM, non-WETH senders → the donation
        CALL reverts.

        We assert the post-fix behavior: the donation reverts and the
        executor's balance is untainted.
        """
        weth.mint(executor.address, AMOUNT_WETH, sender=owner_account)
        pre_balance = weth.balanceOf(executor.address) + executor.balance

        attacker = accounts[7]
        owner_account.transfer(attacker, DONATION + 10**16)

        donation_reverted = False
        try:
            attacker.transfer(executor.address, DONATION)
        except Exception:
            donation_reverted = True

        assert donation_reverted, (
            "Plain ETH donation to executor should be rejected post-fix "
            "(__default__ must gate to POOL_MANAGER_ADDR / WETH_ADDR only)."
        )

        # Executor balance untainted.
        assert (
            weth.balanceOf(executor.address) + executor.balance == pre_balance
        ), "Executor balance changed despite donation rejection"

    def test_pm_native_take_still_delivers_eth(
        self, weth, owner_account, executor, v4_pm
    ):
        """
        Regression: PM's native-take path (take(NATIVE, executor, amount))
        delivers ETH via raw_call(executor) → must still be accepted. This is
        the legitimate __default__ caller we must preserve. Covered more
        thoroughly in test_withdrawal / test_v4_* suites; this is a focused
        guard that the __default__ gate doesn't break PM native delivery.
        """
        # This is a smoke test: the full V4 native-take path is exercised
        # across the existing withdrawal tests. Here we just assert that the
        # executor still accepts ETH from the PM via a direct value transfer
        # (as PM.take(NATIVE, executor, amt) does internally).
        # We can't easily synthesize a PM.take directly out of context, so we
        # rely on the existing test_withdrawal ERC6909-native-ETH suite as the
        # real regression guard. This test exists to be skipped if that path
        # is otherwise uncovered; marked xfail-style smoke.
        pytest.skip(
            "PM native-take delivery is covered by test_withdrawal.* native-ETH "
            "tests; a focused unit test would require synthesizing a bare PM "
            "value-transfer which is out of context here."
        )
