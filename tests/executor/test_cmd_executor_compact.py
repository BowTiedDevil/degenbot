"""
Tests for cmd_executor compact and warm-balance commands.
"""

import pytest
from .conftest_shared import (
    NATIVE_ADDRESS,
    ZERO_ADDRESS,
    WETH_DEPLOYMENT_WRAP_AMOUNT,
    enc_v4_swap_compact,
    enc_v4_take,
    enc_v4_take_delta,
    enc_v4_take_compact,
    enc_v4_sync,
    enc_v4_settle,
    enc_v4_settle_delta,
    enc_erc20_xfer_balance,
    enc_weth_deposit_all,
    enc_weth_withdraw_all,
    enc_v4_unlock,
    _make_pool_key,
    _setup_v4_swap,
    AddressTable,
    enc_preamble,
)

AMOUNT_WETH = 1 * 10**18
AMOUNT_USDC = 2000 * 10**6


class TestV4SwapCompact:
    def test_v4_swap_compact_weth_usdc(
        self, usdc, weth, owner_account, executor, v4_pm
    ):
        """V4→V4 using V4_SWAP_COMPACT (uint128 amount + default sqrt)."""
        pool_a_key = _make_pool_key(
            weth.address, usdc.address, fee=3000, tick_spacing=60
        )
        pool_a_zfo = pool_a_key[0] == weth.address
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_a_key,
            AMOUNT_WETH,
            AMOUNT_USDC,
            pool_a_zfo,
            output_token=usdc,
        )

        pool_b_key = _make_pool_key(
            weth.address, usdc.address, fee=3000, tick_spacing=120
        )
        pool_b_zfo = pool_b_key[0] == usdc.address
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_b_key,
            AMOUNT_USDC,
            AMOUNT_WETH * 2,
            pool_b_zfo,
            output_token=weth,
        )

        at = AddressTable()
        pm_idx = at.add(v4_pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        zero_idx = at.add(ZERO_ADDRESS)

        inner = enc_v4_swap_compact(
            weth_idx if pool_a_key[0] == weth.address else usdc_idx,
            usdc_idx if pool_a_key[1] == usdc.address else weth_idx,
            pool_a_key[2],
            pool_a_key[3],
            zero_idx,
            pool_a_zfo,
            AMOUNT_WETH,
        )
        inner += enc_v4_swap_compact(
            weth_idx if pool_b_key[0] == weth.address else usdc_idx,
            usdc_idx if pool_b_key[1] == usdc.address else weth_idx,
            pool_b_key[2],
            pool_b_key[3],
            zero_idx,
            pool_b_zfo,
            AMOUNT_USDC,
        )
        inner += enc_v4_take_delta(weth_idx, executor_idx)

        commands = enc_v4_unlock(inner)
        tx = executor.execute(
            enc_preamble(at) + commands, sender=owner_account, raise_on_revert=False
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")


class TestErc20XferBalance:
    def test_xfer_balance_after_v4_take(
        self, usdc, weth, owner_account, executor, v4_pm
    ):
        """ERC20_XFER_BALANCE after V4 take."""
        pool_a_key = _make_pool_key(
            weth.address, usdc.address, fee=3000, tick_spacing=60
        )
        pool_a_zfo = pool_a_key[0] == weth.address
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_a_key,
            AMOUNT_WETH,
            AMOUNT_USDC,
            pool_a_zfo,
            output_token=usdc,
        )

        pool_b_key = _make_pool_key(
            weth.address, usdc.address, fee=3000, tick_spacing=120
        )
        pool_b_zfo = pool_b_key[0] == usdc.address
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_b_key,
            AMOUNT_USDC,
            AMOUNT_WETH * 2,
            pool_b_zfo,
            output_token=weth,
        )

        at = AddressTable()
        pm_idx = at.add(v4_pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        zero_idx = at.add(ZERO_ADDRESS)

        inner = enc_v4_swap_compact(
            weth_idx if pool_a_key[0] == weth.address else usdc_idx,
            usdc_idx if pool_a_key[1] == usdc.address else weth_idx,
            pool_a_key[2],
            pool_a_key[3],
            zero_idx,
            pool_a_zfo,
            AMOUNT_WETH,
        )
        inner += enc_v4_take(usdc_idx, executor_idx, AMOUNT_USDC)
        inner += enc_v4_sync(usdc_idx)
        inner += enc_erc20_xfer_balance(usdc_idx, pm_idx)
        inner += enc_v4_settle()
        inner += enc_v4_swap_compact(
            weth_idx if pool_b_key[0] == weth.address else usdc_idx,
            usdc_idx if pool_b_key[1] == usdc.address else weth_idx,
            pool_b_key[2],
            pool_b_key[3],
            zero_idx,
            pool_b_zfo,
            AMOUNT_USDC,
        )
        inner += enc_v4_take_delta(weth_idx, executor_idx)

        commands = enc_v4_unlock(inner)
        tx = executor.execute(
            enc_preamble(at) + commands, sender=owner_account, raise_on_revert=False
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")


class TestV4TakeCompact:
    def test_v4_take_compact(self, usdc, weth, owner_account, executor, v4_pm):
        """V4_TAKE_COMPACT: uint128 amount (16 bytes instead of 32)."""
        weth.mint(executor.address, AMOUNT_WETH, sender=owner_account)
        pool_key = _make_pool_key(weth.address, usdc.address, fee=3000, tick_spacing=60)
        zfo = pool_key[0] == weth.address
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_key,
            AMOUNT_WETH,
            AMOUNT_USDC,
            zfo,
            output_token=usdc,
        )

        at = AddressTable()
        pm_idx = at.add(v4_pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        zero_idx = at.add(ZERO_ADDRESS)

        inner = enc_v4_swap_compact(
            weth_idx if pool_key[0] == weth.address else usdc_idx,
            usdc_idx if pool_key[1] == usdc.address else weth_idx,
            pool_key[2],
            pool_key[3],
            zero_idx,
            zfo,
            AMOUNT_WETH,
        )
        inner += enc_v4_take_compact(usdc_idx, executor_idx, AMOUNT_USDC)
        inner += enc_v4_settle_delta(weth_idx)
        commands = enc_v4_unlock(inner)

        # fake pool parameters don't guarantee profit in ETH+WETH terms
        tx = executor.execute(
            enc_preamble(at, skip_profit=True) + commands, sender=owner_account
        )
        assert usdc.balanceOf(executor) == AMOUNT_USDC


class TestWethDepositAll:
    def test_deposit_all_eth(self, usdc, weth, owner_account, executor, v4_pm):
        """WETH_DEPOSIT_ALL wraps all ETH."""
        weth.mint(executor.address, AMOUNT_WETH, sender=owner_account)
        pool_a_key = _make_pool_key(
            weth.address, usdc.address, fee=3000, tick_spacing=60
        )
        pool_a_zfo = pool_a_key[0] == weth.address
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_a_key,
            AMOUNT_WETH,
            AMOUNT_USDC,
            pool_a_zfo,
            output_token=usdc,
        )

        pool_b_key = _make_pool_key(
            NATIVE_ADDRESS, usdc.address, fee=500, tick_spacing=10
        )
        pool_b_zfo = pool_b_key[0] == usdc.address
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_b_key,
            AMOUNT_USDC,
            AMOUNT_WETH * 2,
            pool_b_zfo,
            fund_eth=True,
        )

        at = AddressTable()
        pm_idx = at.add(v4_pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        zero_idx = at.add(ZERO_ADDRESS)
        native_idx = at.add(NATIVE_ADDRESS)

        inner = enc_v4_swap_compact(
            weth_idx if pool_a_key[0] == weth.address else usdc_idx,
            usdc_idx if pool_a_key[1] == usdc.address else weth_idx,
            pool_a_key[2],
            pool_a_key[3],
            zero_idx,
            pool_a_zfo,
            AMOUNT_WETH,
        )
        inner += enc_v4_swap_compact(
            native_idx if pool_b_key[0] == NATIVE_ADDRESS else usdc_idx,
            usdc_idx if pool_b_key[1] == usdc.address else native_idx,
            pool_b_key[2],
            pool_b_key[3],
            zero_idx,
            pool_b_zfo,
            AMOUNT_USDC,
        )
        inner += enc_v4_settle_delta(weth_idx)
        inner += enc_v4_take_delta(native_idx, executor_idx)
        inner += enc_weth_deposit_all()

        commands = enc_v4_unlock(inner)
        tx = executor.execute(
            enc_preamble(at) + commands, sender=owner_account, raise_on_revert=False
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")


class TestWethWithdrawAll:
    def test_withdraw_all_weth(self, usdc, weth, owner_account, executor, v4_pm):
        """WETH_WITHDRAW_ALL unwraps all WETH."""
        weth.mint(executor.address, AMOUNT_WETH, sender=owner_account)
        pool_a_key = _make_pool_key(
            weth.address, usdc.address, fee=3000, tick_spacing=60
        )
        pool_a_zfo = pool_a_key[0] == weth.address
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_a_key,
            AMOUNT_WETH,
            AMOUNT_USDC,
            pool_a_zfo,
            output_token=usdc,
        )

        pool_b_key = _make_pool_key(
            NATIVE_ADDRESS, usdc.address, fee=500, tick_spacing=10
        )
        pool_b_zfo = pool_b_key[0] == usdc.address
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_b_key,
            AMOUNT_USDC,
            AMOUNT_WETH * 2,
            pool_b_zfo,
            fund_eth=True,
        )

        at = AddressTable()
        pm_idx = at.add(v4_pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        zero_idx = at.add(ZERO_ADDRESS)
        native_idx = at.add(NATIVE_ADDRESS)

        inner = enc_v4_swap_compact(
            weth_idx if pool_a_key[0] == weth.address else usdc_idx,
            usdc_idx if pool_a_key[1] == usdc.address else weth_idx,
            pool_a_key[2],
            pool_a_key[3],
            zero_idx,
            pool_a_zfo,
            AMOUNT_WETH,
        )
        inner += enc_v4_swap_compact(
            native_idx if pool_b_key[0] == NATIVE_ADDRESS else usdc_idx,
            usdc_idx if pool_b_key[1] == usdc.address else native_idx,
            pool_b_key[2],
            pool_b_key[3],
            zero_idx,
            pool_b_zfo,
            AMOUNT_USDC,
        )
        inner += enc_v4_settle_delta(weth_idx)
        inner += enc_v4_take_delta(native_idx, executor_idx)
        inner += enc_weth_deposit_all()
        inner += enc_weth_withdraw_all()

        commands = enc_v4_unlock(inner)
        tx = executor.execute(
            enc_preamble(at) + commands, sender=owner_account, raise_on_revert=False
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")

        weth_bal = weth.balanceOf(executor)
        assert weth_bal == 0, "All WETH should be unwrapped"
