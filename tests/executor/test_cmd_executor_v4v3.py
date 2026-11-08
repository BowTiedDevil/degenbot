"""
Tests for cmd_executor V4-V3 swap execution.
"""

import pytest
from .conftest_shared import (
    ZERO_ADDRESS,
    WETH_DEPLOYMENT_WRAP_AMOUNT,
    MIN_SQRT_PRICE_X96,
    MAX_SQRT_PRICE_X96,
    enc_v4_swap_compact,
    enc_v3_swap_compact,
    enc_v4_take,
    enc_v4_sync,
    enc_v4_settle,
    enc_v4_settle_delta,
    enc_erc20_transfer,
    enc_v4_unlock,
    enc_v4_take_delta,
    _make_pool_key,
    _setup_v4_swap,
    _setup_v3,
    AddressTable,
    enc_preamble,
)


@pytest.fixture
def v3_pool(project, owner_account, usdc, weth):
    token0, token1 = sorted([usdc.address, weth.address], key=lambda addr: addr.lower())
    return project.fake_uniswap_v3_pool.deploy(token0, token1, 0, 3000, sender=owner_account)


AMOUNT_WETH = 1 * 10**18
AMOUNT_USDC = 2000 * 10**6


class TestV4ToV3:
    """V4→V3: V4 swap first, take intermediate, V3 swap in callback, settle after."""

    def test_v4_v3_usdc_to_weth(
        self, usdc, weth, owner_account, executor, v4_pm, v3_pool
    ):
        """V4→V3 with explicit forward_data for V3 callback."""
        # Executor needs starting WETH to cover V4 settlement
        # (V3 output is slightly less than AMOUNT_WETH due to 0.3% fee)
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

        v3_zfo, v3_weth_out = _setup_v3(v3_pool, usdc, weth, AMOUNT_USDC, AMOUNT_WETH, owner_account)

        at = AddressTable()
        pm_idx = at.add(v4_pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        v3_idx = at.add(v3_pool.address)
        zero_idx = at.add(ZERO_ADDRESS)

        # V3 callback forward_data: pay USDC to V3 pool, then sync + transfer + settle WETH to PM
        v3_callback_cmds = enc_erc20_transfer(usdc_idx, v3_idx, AMOUNT_USDC)
        v3_callback_cmds += enc_v4_sync(weth_idx)
        v3_callback_cmds += enc_erc20_transfer(weth_idx, pm_idx, AMOUNT_WETH)
        v3_callback_cmds += enc_v4_settle()

        sqrt_limit = MIN_SQRT_PRICE_X96 + 1 if v3_zfo else MAX_SQRT_PRICE_X96 - 1
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
        inner += enc_v3_swap_compact(
            v3_idx, v3_zfo, AMOUNT_USDC, executor_idx, forward_data=v3_callback_cmds
        )

        commands = enc_v4_unlock(inner)
        tx = executor.execute(
            enc_preamble(at) + commands, sender=owner_account, raise_on_revert=False
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")


class TestV4ToV3AutoPay:
    """V4→V3 using auto-pay (empty forward_data in V3 callback)."""

    def test_v4_v3_auto_pay_usdc_to_weth(
        self, usdc, weth, owner_account, executor, v4_pm, v3_pool
    ):
        """V4→V3 with auto-pay — no forward_data needed."""
        # Executor needs starting WETH to cover V4 settlement
        # (V3 output is slightly less than AMOUNT_WETH due to 0.3% fee)
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

        v3_zfo, v3_weth_out = _setup_v3(v3_pool, usdc, weth, AMOUNT_USDC, AMOUNT_WETH, owner_account)

        at = AddressTable()
        pm_idx = at.add(v4_pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        v3_idx = at.add(v3_pool.address)
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
        # Auto-pay: no forward_data
        inner += enc_v3_swap_compact(v3_idx, v3_zfo, AMOUNT_USDC, executor_idx)
        inner += enc_v4_settle_delta(weth_idx)

        commands = enc_v4_unlock(inner)
        tx = executor.execute(
            enc_preamble(at) + commands, sender=owner_account, raise_on_revert=False
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")


class TestV3ToV4AutoPay:
    """V3→V4 with auto-pay in V3 callback."""

    def test_v3_v4_auto_pay_weth_to_usdc(
        self, usdc, weth, owner_account, executor, v4_pm, v3_pool
    ):
        """V3→V4: V3 swap with auto-pay callback, then V4 swap."""
        weth.mint(executor.address, AMOUNT_WETH, sender=owner_account)

        v3_zfo, v3_usdc_out = _setup_v3(v3_pool, weth, usdc, AMOUNT_WETH, AMOUNT_USDC, owner_account)

        pool_b_key = _make_pool_key(
            weth.address, usdc.address, fee=3000, tick_spacing=60
        )
        pool_b_zfo = pool_b_key[0] == usdc.address
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_b_key,
            v3_usdc_out,
            AMOUNT_WETH * 2,
            pool_b_zfo,
            output_token=weth,
        )

        at = AddressTable()
        pm_idx = at.add(v4_pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        v3_idx = at.add(v3_pool.address)
        zero_idx = at.add(ZERO_ADDRESS)

        # Outer: V3 swap (auto-pay callback)
        outer = enc_v3_swap_compact(v3_idx, v3_zfo, AMOUNT_WETH, executor_idx)

        # Inner (inside unlock): sync + transfer + settle (pre-settle for V4 swap)
        inner = enc_v4_sync(usdc_idx)
        inner += enc_erc20_transfer(usdc_idx, pm_idx, v3_usdc_out)
        inner += enc_v4_settle()
        inner += enc_v4_swap_compact(
            weth_idx if pool_b_key[0] == weth.address else usdc_idx,
            usdc_idx if pool_b_key[1] == usdc.address else weth_idx,
            pool_b_key[2],
            pool_b_key[3],
            zero_idx,
            pool_b_zfo,
            v3_usdc_out,
        )
        inner += enc_v4_take_delta(weth_idx, executor_idx)

        outer += enc_v4_unlock(inner)
        tx = executor.execute(
            enc_preamble(at) + outer, sender=owner_account, raise_on_revert=False
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")
