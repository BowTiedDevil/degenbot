"""
Tests for V2 fee bounds validation.

V2 fee is a fraction of 10000. Values >= 10000 are invalid because
10000 - fee would underflow (0 or negative). The executor should
revert with a clear error message rather than a cryptic underflow.
"""

import pytest
from .conftest_shared import (
    WETH_DEPLOYMENT_WRAP_AMOUNT,
    enc_v2_swap_compact,
    enc_v2_swap_calc,
    AddressTable,
    enc_preamble,
    V2_LIQUIDITY_WETH,
    V2_LIQUIDITY_USDC,
)


class TestFeeBoundsV2SwapCalc:
    """V2_SWAP_CALC should revert on invalid fee values."""

    def test_fee_exactly_10000_reverts(self, project, usdc, weth, owner_account):
        """fee=10000 means 100% fee — all output consumed, nonsense."""
        token0, token1 = sorted([usdc.address, weth.address], key=lambda a: a.lower())
        v2 = project.fake_uniswap_v2_pair.deploy(
            token0, token1, 0, 30, sender=owner_account
        )
        executor = project.cmd_executor.deploy(
            weth.address,
            v2.address,
            sender=owner_account,
        )
        # No initialize() — dummy PM (V2 pair has no PM.unlock)
        weth.deposit(value=WETH_DEPLOYMENT_WRAP_AMOUNT, sender=owner_account)
        weth.transfer(
            executor.address, WETH_DEPLOYMENT_WRAP_AMOUNT, sender=owner_account
        )

        at = AddressTable()
        v2_idx = at.add(v2.address)
        executor_idx = at.add(executor.address)

        commands = enc_preamble(at, skip_profit=True) + enc_v2_swap_calc(
            v2_idx, True, executor_idx, fee=10000
        )

        tx = executor.execute(commands, sender=owner_account, raise_on_revert=False)
        assert tx.status == 0, "Should revert — fee=10000 is invalid"

    def test_fee_above_10000_reverts(self, project, usdc, weth, owner_account):
        """fee=20000 > 10000 — would cause underflow in fee_multiplier."""
        token0, token1 = sorted([usdc.address, weth.address], key=lambda a: a.lower())
        v2 = project.fake_uniswap_v2_pair.deploy(
            token0, token1, 0, 30, sender=owner_account
        )
        executor = project.cmd_executor.deploy(
            weth.address,
            v2.address,
            sender=owner_account,
        )
        # No initialize() — dummy PM (V2 pair has no PM.unlock)
        weth.deposit(value=WETH_DEPLOYMENT_WRAP_AMOUNT, sender=owner_account)
        weth.transfer(
            executor.address, WETH_DEPLOYMENT_WRAP_AMOUNT, sender=owner_account
        )

        at = AddressTable()
        v2_idx = at.add(v2.address)
        executor_idx = at.add(executor.address)

        commands = enc_preamble(at, skip_profit=True) + enc_v2_swap_calc(
            v2_idx, True, executor_idx, fee=20000
        )

        tx = executor.execute(commands, sender=owner_account, raise_on_revert=False)
        assert tx.status == 0, "Should revert — fee > 10000 is invalid"

    def test_fee_zero_reverts(self, project, usdc, weth, owner_account):
        """fee=0 is suspicious — no known V2 pair has zero fee."""
        token0, token1 = sorted([usdc.address, weth.address], key=lambda a: a.lower())
        v2 = project.fake_uniswap_v2_pair.deploy(
            token0, token1, 0, 30, sender=owner_account
        )
        executor = project.cmd_executor.deploy(
            weth.address,
            v2.address,
            sender=owner_account,
        )
        # No initialize() — dummy PM (V2 pair has no PM.unlock)
        weth.deposit(value=WETH_DEPLOYMENT_WRAP_AMOUNT, sender=owner_account)
        weth.transfer(
            executor.address, WETH_DEPLOYMENT_WRAP_AMOUNT, sender=owner_account
        )

        at = AddressTable()
        v2_idx = at.add(v2.address)
        executor_idx = at.add(executor.address)

        commands = enc_preamble(at, skip_profit=True) + enc_v2_swap_calc(
            v2_idx, True, executor_idx, fee=0
        )

        tx = executor.execute(commands, sender=owner_account, raise_on_revert=False)
        assert tx.status == 0, "Should revert — fee=0 is invalid"


class TestFeeBoundsV2SwapCompact:
    """V2_SWAP_COMPACT should revert on invalid fee values."""

    def test_fee_above_10000_reverts(self, project, usdc, weth, owner_account):
        """fee > 10000 in V2_SWAP_COMPACT should revert clearly."""
        token0, token1 = sorted([usdc.address, weth.address], key=lambda a: a.lower())
        v2 = project.fake_uniswap_v2_pair.deploy(
            token0, token1, 0, 30, sender=owner_account
        )
        executor = project.cmd_executor.deploy(
            weth.address,
            v2.address,
            sender=owner_account,
        )
        # No initialize() — dummy PM (V2 pair has no PM.unlock)
        weth.deposit(value=WETH_DEPLOYMENT_WRAP_AMOUNT, sender=owner_account)
        weth.transfer(
            executor.address, WETH_DEPLOYMENT_WRAP_AMOUNT, sender=owner_account
        )

        at = AddressTable()
        v2_idx = at.add(v2.address)
        executor_idx = at.add(executor.address)

        commands = enc_preamble(at, skip_profit=True) + enc_v2_swap_compact(
            v2_idx, True, 1000, executor_idx, fee=20000
        )

        tx = executor.execute(commands, sender=owner_account, raise_on_revert=False)
        assert tx.status == 0, "Should revert — fee > 10000 is invalid"
