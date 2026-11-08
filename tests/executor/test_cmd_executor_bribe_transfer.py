"""
Tests that bribe configuration actually transfers ETH to the right address.

Verifies:
- Bribe to coinbase (recipient_idx=0) sends profit*bips/10000 ETH to block.coinbase
- Bribe to address (recipient_idx>0) sends profit*bips/10000 ETH to the specified address
- No bribe is sent when bips == 0
- Fractional precision (e.g. 1% = 100 bips)

Strategy:
  Tx1: Deposit 1 ETH into the V4 PoolManager via a WETH→native ETH swap,
       then V4_MINT_COMPACT to hold it as ERC6909. The ETH stays in the PM.
       Skip profit check (combined balance drops by 1 ETH).

  Tx2: V4_BURN_COMPACT the ERC6909 balance + V4_TAKE_DELTA to pull the
       1 ETH back out. The returning ETH appears as "profit" (combined_after
       > combined_before by exactly 1 ETH). The bribe sends bips/10000 of
       that to the chosen recipient. We verify exact amounts.
"""

import pytest

from .conftest_shared import (
    NATIVE_ADDRESS,
    ZERO_ADDRESS,
    WETH_DEPLOYMENT_WRAP_AMOUNT,
    enc_v4_swap_compact,
    enc_v4_settle_delta,
    enc_v4_unlock,
    enc_v4_mint_compact,
    enc_v4_burn_compact,
    enc_v4_take_delta,
    make_config,
    _make_pool_key,
    _setup_v4_swap,
    AddressTable,
    enc_preamble,
)

AMOUNT_WETH = 1 * 10**18
AMOUNT_USDC = 2000 * 10**6
NATIVE_ID = 0  # uint160(NATIVE_ADDRESS)


@pytest.fixture
def bribe_recipient(accounts):
    """Dedicated account that receives BRIBE_ADDRESS payments."""
    return accounts[5]


def _setup_and_mint_native_eth(
    v4_pm, weth, owner_account, executor, at, amount=AMOUNT_WETH
):
    pool_key = _make_pool_key(NATIVE_ADDRESS, weth.address, fee=500, tick_spacing=10)
    zfo = pool_key[0] == weth.address  # False (NATIVE_ADDRESS < WETH)

    _setup_v4_swap(v4_pm, owner_account, pool_key, amount, amount, zfo, fund_eth=True)

    pm_idx = at.add(v4_pm.address)
    weth_idx = at.add(weth.address)
    executor_idx = at.add(executor.address)
    zero_idx = at.add(ZERO_ADDRESS)
    native_idx = at.add(NATIVE_ADDRESS)

    inner = (
        enc_v4_swap_compact(
            native_idx if pool_key[0] == NATIVE_ADDRESS else weth_idx,
            weth_idx if pool_key[1] == weth.address else native_idx,
            pool_key[2], pool_key[3], zero_idx, zfo, amount,
        )
        + enc_v4_mint_compact(native_idx, executor_idx, amount)
        + enc_v4_settle_delta(weth_idx)
    )
    return enc_v4_unlock(inner)


def _build_burn_and_take_native_eth(v4_pm, weth, executor, at, amount=AMOUNT_WETH):
    native_idx = at.add(NATIVE_ADDRESS)
    executor_idx = at.add(executor.address)

    inner = enc_v4_burn_compact(native_idx, amount) + enc_v4_take_delta(
        native_idx, executor_idx
    )
    return enc_v4_unlock(inner)


class TestBribeTransferBalances:
    """Verify bribes actually move ETH to the correct addresses."""

    def test_bribe_address_sends_to_recipient(
        self, usdc, weth, owner_account, executor, v4_pm, bribe_recipient
    ):
        """Bribe 50% to recipient via config param: burn 1 ETH of ERC6909, verify recipient
        receives exactly 0.5 ETH and executor retains 0.5 ETH profit."""
        PROFIT_ETH = AMOUNT_WETH
        BIPS = 5000

        at = AddressTable()

        commands_1 = _setup_and_mint_native_eth(
            v4_pm, weth, owner_account, executor, at
        )
        weth.mint(executor.address, PROFIT_ETH, sender=owner_account)
        tx1 = executor.execute(
            enc_preamble(at) + commands_1, sender=owner_account
        )
        assert tx1.status == 1

        assert v4_pm.balanceOf(executor.address, NATIVE_ID) == PROFIT_ETH

        recipient_idx = at.add(bribe_recipient.address)
        commands_2 = _build_burn_and_take_native_eth(v4_pm, weth, executor, at)

        combined_before = executor.balance + weth.balanceOf(executor.address)
        recipient_before = bribe_recipient.balance

        tx2 = executor.execute(
            enc_preamble(at) + commands_2,
            make_config(bribe_bips=BIPS, bribe_recipient_idx=recipient_idx),
            sender=owner_account,
        )

        combined_after = executor.balance + weth.balanceOf(executor.address)
        actual_bribe = bribe_recipient.balance - recipient_before

        profit = (combined_after - combined_before) + actual_bribe
        expected_bribe = profit * BIPS // 10000

        assert profit == PROFIT_ETH, (
            f"profit should be exactly {PROFIT_ETH}, got {profit}"
        )
        assert actual_bribe == expected_bribe, (
            f"recipient received {actual_bribe} wei, expected {expected_bribe} wei"
        )

    def test_bribe_coinbase_sends_eth(
        self, usdc, weth, owner_account, executor, v4_pm, chain
    ):
        """Bribe 50% to coinbase via config param."""
        PROFIT_ETH = AMOUNT_WETH
        BIPS = 5000

        at = AddressTable()

        commands_1 = _setup_and_mint_native_eth(
            v4_pm, weth, owner_account, executor, at
        )
        weth.mint(executor.address, PROFIT_ETH, sender=owner_account)
        tx1 = executor.execute(
            enc_preamble(at) + commands_1, sender=owner_account
        )
        assert tx1.status == 1

        commands_2 = _build_burn_and_take_native_eth(v4_pm, weth, executor, at)

        combined_before = executor.balance + weth.balanceOf(executor.address)

        tx2 = executor.execute(
            enc_preamble(at) + commands_2,
            make_config(bribe_bips=BIPS),  # recipient_idx=0 = coinbase
            sender=owner_account,
        )

        combined_after = executor.balance + weth.balanceOf(executor.address)

        net_gain = combined_after - combined_before
        expected_net_gain = PROFIT_ETH - (PROFIT_ETH * BIPS // 10000)
        assert net_gain == expected_net_gain, (
            f"executor net gain {net_gain} wei, expected {expected_net_gain} wei"
        )

    def test_no_bribe_sends_nothing(
        self, usdc, weth, owner_account, executor, v4_pm, bribe_recipient
    ):
        """No bribe: verify recipient gets nothing and executor keeps full profit."""
        PROFIT_ETH = AMOUNT_WETH

        at = AddressTable()

        commands_1 = _setup_and_mint_native_eth(
            v4_pm, weth, owner_account, executor, at
        )
        weth.mint(executor.address, PROFIT_ETH, sender=owner_account)
        tx1 = executor.execute(
            enc_preamble(at) + commands_1, sender=owner_account
        )
        assert tx1.status == 1

        commands_2 = _build_burn_and_take_native_eth(v4_pm, weth, executor, at)

        combined_before = executor.balance + weth.balanceOf(executor.address)
        recipient_before = bribe_recipient.balance

        tx2 = executor.execute(
            enc_preamble(at) + commands_2,
            sender=owner_account,
        )

        combined_after = executor.balance + weth.balanceOf(executor.address)

        assert bribe_recipient.balance == recipient_before
        assert combined_after - combined_before == PROFIT_ETH

    def test_bribe_fractional_precision(
        self, usdc, weth, owner_account, executor, v4_pm, bribe_recipient
    ):
        """Bribe 1% (100 bips) via config param."""
        PROFIT_ETH = AMOUNT_WETH
        BIPS = 100

        at = AddressTable()

        commands_1 = _setup_and_mint_native_eth(
            v4_pm, weth, owner_account, executor, at
        )
        weth.mint(executor.address, PROFIT_ETH, sender=owner_account)
        tx1 = executor.execute(
            enc_preamble(at) + commands_1, sender=owner_account
        )
        assert tx1.status == 1

        recipient_idx = at.add(bribe_recipient.address)
        commands_2 = _build_burn_and_take_native_eth(v4_pm, weth, executor, at)

        recipient_before = bribe_recipient.balance

        tx2 = executor.execute(
            enc_preamble(at) + commands_2,
            make_config(bribe_bips=BIPS, bribe_recipient_idx=recipient_idx),
            sender=owner_account,
        )

        actual_bribe = bribe_recipient.balance - recipient_before
        expected_bribe = PROFIT_ETH * BIPS // 10000  # 0.01 ETH
        assert actual_bribe == expected_bribe, (
            f"recipient got {actual_bribe} wei, expected {expected_bribe} wei "
            f"(profit={PROFIT_ETH}, bips={BIPS})"
        )
