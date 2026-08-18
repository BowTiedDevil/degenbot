"""
Tests for cmd_executor dynamic value discovery commands.
"""

import pytest
from .conftest_shared import (
    NATIVE_ADDRESS,
    ZERO_ADDRESS,
    WETH_DEPLOYMENT_WRAP_AMOUNT,
    MIN_SQRT_PRICE_X96,
    MAX_SQRT_PRICE_X96,
    enc_v4_swap_compact,
    enc_v4_swap_dynamic,
    enc_v4_take,
    enc_v4_take_delta,
    enc_v4_settle_delta,
    enc_v4_settle_all,
    enc_v4_unlock,
    _make_pool_key,
    _setup_v4_swap,
    AddressTable,
    enc_preamble,
)


class TestV4TakeDelta:
    def test_v4_take_delta_weth_profit(
        self, usdc, weth, owner_account, executor, v4_pm
    ):
        """V4→V4 same-currency using V4_TAKE_DELTA instead of V4_TAKE."""
        pool_a_amount_in = 1 * 10**18
        pool_a_amount_out = 2000 * 10**6
        pool_b_amount_in = 2000 * 10**6
        pool_b_amount_out = 2 * 10**18

        pool_a_key = _make_pool_key(
            weth.address, usdc.address, fee=3000, tick_spacing=60
        )
        pool_a_zfo = pool_a_key[0] == weth.address
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_a_key,
            pool_a_amount_in,
            pool_a_amount_out,
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
            pool_b_amount_in,
            pool_b_amount_out,
            pool_b_zfo,
            output_token=weth,
        )

        at = AddressTable()
        pm_idx = at.add(v4_pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        zero_idx = at.add(ZERO_ADDRESS)

        inner = b""
        inner += enc_v4_swap_compact(
            weth_idx if pool_a_key[0] == weth.address else usdc_idx,
            usdc_idx if pool_a_key[1] == usdc.address else weth_idx,
            pool_a_key[2],
            pool_a_key[3],
            zero_idx,
            pool_a_zfo,
            pool_a_amount_in,
        )
        inner += enc_v4_swap_compact(
            weth_idx if pool_b_key[0] == weth.address else usdc_idx,
            usdc_idx if pool_b_key[1] == usdc.address else weth_idx,
            pool_b_key[2],
            pool_b_key[3],
            zero_idx,
            pool_b_zfo,
            pool_b_amount_in,
        )
        inner += enc_v4_take_delta(weth_idx, executor_idx)

        commands = enc_v4_unlock(inner)
        tx = executor.execute(
            enc_preamble(at) + commands, sender=owner_account, raise_on_revert=False
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")


class TestV4SettleDelta:
    def test_v4_settle_delta_weth(self, usdc, weth, owner_account, executor, v4_pm):
        """V4→V4 cross-currency using V4_SETTLE_DELTA."""
        pool_a_amount_in = 1 * 10**18
        pool_a_amount_out = 2000 * 10**6
        pool_b_amount_in = 2000 * 10**6
        pool_b_amount_out = 2 * 10**18

        pool_a_key = _make_pool_key(
            weth.address, usdc.address, fee=3000, tick_spacing=60
        )
        pool_a_zfo = pool_a_key[0] == weth.address
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_a_key,
            pool_a_amount_in,
            pool_a_amount_out,
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
            pool_b_amount_in,
            pool_b_amount_out,
            pool_b_zfo,
            output_token=weth,
        )

        at = AddressTable()
        pm_idx = at.add(v4_pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        zero_idx = at.add(ZERO_ADDRESS)

        inner = b""
        inner += enc_v4_swap_compact(
            weth_idx if pool_a_key[0] == weth.address else usdc_idx,
            usdc_idx if pool_a_key[1] == usdc.address else weth_idx,
            pool_a_key[2],
            pool_a_key[3],
            zero_idx,
            pool_a_zfo,
            pool_a_amount_in,
        )
        inner += enc_v4_swap_compact(
            weth_idx if pool_b_key[0] == weth.address else usdc_idx,
            usdc_idx if pool_b_key[1] == usdc.address else weth_idx,
            pool_b_key[2],
            pool_b_key[3],
            zero_idx,
            pool_b_zfo,
            pool_b_amount_in,
        )
        inner += enc_v4_settle_delta(weth_idx)

        commands = enc_v4_unlock(inner)
        tx = executor.execute(
            enc_preamble(at) + commands, sender=owner_account, raise_on_revert=False
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")


class TestV4SettleAll:
    def test_v4_settle_all_v4v4_cross_currency(
        self, usdc, weth, owner_account, executor, v4_pm
    ):
        """V4→V4 cross-currency using V4_SETTLE_ALL."""
        pool_a_amount_in = 1 * 10**18
        pool_a_amount_out = 2000 * 10**6
        pool_b_amount_in = 2000 * 10**6
        pool_b_amount_out = 2 * 10**18

        pool_a_key = _make_pool_key(
            weth.address, usdc.address, fee=3000, tick_spacing=60
        )
        pool_a_zfo = pool_a_key[0] == weth.address
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_a_key,
            pool_a_amount_in,
            pool_a_amount_out,
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
            pool_b_amount_in,
            pool_b_amount_out,
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

        inner = b""
        inner += enc_v4_swap_compact(
            weth_idx if pool_a_key[0] == weth.address else usdc_idx,
            usdc_idx if pool_a_key[1] == usdc.address else weth_idx,
            pool_a_key[2],
            pool_a_key[3],
            zero_idx,
            pool_a_zfo,
            pool_a_amount_in,
        )
        inner += enc_v4_swap_compact(
            native_idx if pool_b_key[0] == NATIVE_ADDRESS else usdc_idx,
            usdc_idx if pool_b_key[1] == usdc.address else native_idx,
            pool_b_key[2],
            pool_b_key[3],
            zero_idx,
            pool_b_zfo,
            pool_b_amount_in,
        )
        inner += enc_v4_settle_all()

        commands = enc_v4_unlock(inner)
        tx = executor.execute(
            enc_preamble(at) + commands, sender=owner_account, raise_on_revert=False
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")


class TestV4SwapDynamic:
    def test_v4_swap_dynamic_v4v4(self, usdc, weth, owner_account, executor, v4_pm):
        """V4→V4 using V4_SWAP_DYNAMIC for the second swap."""
        pool_a_amount_in = 1 * 10**18
        pool_a_amount_out = 2000 * 10**6
        pool_b_amount_in = 2000 * 10**6
        pool_b_amount_out = 2 * 10**18

        pool_a_key = _make_pool_key(
            weth.address, usdc.address, fee=3000, tick_spacing=60
        )
        pool_a_zfo = pool_a_key[0] == weth.address
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_a_key,
            pool_a_amount_in,
            pool_a_amount_out,
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
            pool_b_amount_in,
            pool_b_amount_out,
            pool_b_zfo,
            output_token=weth,
        )

        at = AddressTable()
        pm_idx = at.add(v4_pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        zero_idx = at.add(ZERO_ADDRESS)

        inner = b""
        # First swap: explicit amount
        inner += enc_v4_swap_compact(
            weth_idx if pool_a_key[0] == weth.address else usdc_idx,
            usdc_idx if pool_a_key[1] == usdc.address else weth_idx,
            pool_a_key[2],
            pool_a_key[3],
            zero_idx,
            pool_a_zfo,
            pool_a_amount_in,
        )
        # Second swap: dynamic amount from exttload
        inner += enc_v4_swap_dynamic(
            weth_idx if pool_b_key[0] == weth.address else usdc_idx,
            usdc_idx if pool_b_key[1] == usdc.address else weth_idx,
            pool_b_key[2],
            pool_b_key[3],
            zero_idx,
            pool_b_zfo,
        )
        inner += enc_v4_take_delta(weth_idx, executor_idx)

        commands = enc_v4_unlock(inner)
        tx = executor.execute(
            enc_preamble(at) + commands, sender=owner_account, raise_on_revert=False
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")


class TestMixedExplicitDynamic:
    def test_mixed_v4_take_and_take_delta(
        self, usdc, weth, owner_account, executor, v4_pm
    ):
        """Mix explicit and dynamic commands."""
        pool_a_amount_in = 1 * 10**18
        pool_a_amount_out = 2000 * 10**6
        pool_b_amount_in = 2000 * 10**6
        pool_b_amount_out = 2 * 10**18

        pool_a_key = _make_pool_key(
            weth.address, usdc.address, fee=3000, tick_spacing=60
        )
        pool_a_zfo = pool_a_key[0] == weth.address
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_a_key,
            pool_a_amount_in,
            pool_a_amount_out,
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
            pool_b_amount_in,
            pool_b_amount_out,
            pool_b_zfo,
            output_token=weth,
        )

        at = AddressTable()
        pm_idx = at.add(v4_pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        zero_idx = at.add(ZERO_ADDRESS)

        inner = b""
        inner += enc_v4_swap_compact(
            weth_idx if pool_a_key[0] == weth.address else usdc_idx,
            usdc_idx if pool_a_key[1] == usdc.address else weth_idx,
            pool_a_key[2],
            pool_a_key[3],
            zero_idx,
            pool_a_zfo,
            pool_a_amount_in,
        )
        inner += enc_v4_swap_compact(
            weth_idx if pool_b_key[0] == weth.address else usdc_idx,
            usdc_idx if pool_b_key[1] == usdc.address else weth_idx,
            pool_b_key[2],
            pool_b_key[3],
            zero_idx,
            pool_b_zfo,
            pool_b_amount_in,
        )
        inner += enc_v4_take_delta(weth_idx, executor_idx)

        commands = enc_v4_unlock(inner)
        tx = executor.execute(
            enc_preamble(at) + commands, sender=owner_account, raise_on_revert=False
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")

    def test_settle_then_settle_all(self, usdc, weth, owner_account, executor, v4_pm):
        """V4_SETTLE_DELTA for WETH, then V4_SETTLE_ALL for the rest."""
        weth.mint(executor.address, 1 * 10**18, sender=owner_account)
        pool_a_amount_in = 1 * 10**18
        pool_a_amount_out = 2000 * 10**6
        pool_b_amount_in = 2000 * 10**6
        pool_b_amount_out = 2 * 10**18

        pool_a_key = _make_pool_key(
            weth.address, usdc.address, fee=3000, tick_spacing=60
        )
        pool_a_zfo = pool_a_key[0] == weth.address
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_a_key,
            pool_a_amount_in,
            pool_a_amount_out,
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
            pool_b_amount_in,
            pool_b_amount_out,
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

        inner = b""
        inner += enc_v4_swap_compact(
            weth_idx if pool_a_key[0] == weth.address else usdc_idx,
            usdc_idx if pool_a_key[1] == usdc.address else weth_idx,
            pool_a_key[2],
            pool_a_key[3],
            zero_idx,
            pool_a_zfo,
            pool_a_amount_in,
        )
        inner += enc_v4_swap_compact(
            native_idx if pool_b_key[0] == NATIVE_ADDRESS else usdc_idx,
            usdc_idx if pool_b_key[1] == usdc.address else native_idx,
            pool_b_key[2],
            pool_b_key[3],
            zero_idx,
            pool_b_zfo,
            pool_b_amount_in,
        )
        inner += enc_v4_settle_delta(weth_idx)
        inner += enc_v4_settle_all()

        commands = enc_v4_unlock(inner)
        tx = executor.execute(
            enc_preamble(at) + commands, sender=owner_account, raise_on_revert=False
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")
