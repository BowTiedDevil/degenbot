"""
Tests for cmd_executor V4-V2 swap execution.
"""

import pytest
from .conftest_shared import (
    ZERO_ADDRESS,
    WETH_DEPLOYMENT_WRAP_AMOUNT,
    V2_LIQUIDITY_WETH,
    V2_LIQUIDITY_USDC,
    enc_v4_swap_compact,
    enc_v2_swap_compact,
    enc_v2_swap_calc,
    enc_v4_take,
    enc_v4_sync,
    enc_v4_settle,
    enc_v4_settle_delta,
    enc_erc20_transfer,
    enc_v4_unlock,
    _make_pool_key,
    _setup_v4_swap,
    AddressTable,
    enc_preamble,
    v2_get_amount_out,
)


@pytest.fixture
def v2_pair(project, owner_account, usdc, weth):
    token0, token1 = sorted([usdc.address, weth.address], key=lambda addr: addr.lower())
    return project.fake_uniswap_v2_pair.deploy(
        token0, token1, 0, 30, sender=owner_account
    )


AMOUNT_WETH = 1 * 10**18
AMOUNT_USDC = 2000 * 10**6


class TestV4ToV2:
    def test_v4_v2_usdc_to_weth(
        self, usdc, weth, owner_account, executor, v4_pm, v2_pair
    ):
        """V4→V2: V4 swap, take USDC, V2 swap with explicit callback payment."""
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

        # Set up V2 pair with ample liquidity for K-invariant
        usdc.mint(v2_pair.address, V2_LIQUIDITY_USDC, sender=owner_account)
        weth.mint(v2_pair.address, V2_LIQUIDITY_WETH, sender=owner_account)
        v2_pair.sync(sender=owner_account)

        v2_zfo = v2_pair.token0() == usdc.address
        v2_reserve_in = usdc.balanceOf(v2_pair.address)
        v2_reserve_out = weth.balanceOf(v2_pair.address)
        v2_amount_out = v2_get_amount_out(
            AMOUNT_USDC, v2_reserve_in, v2_reserve_out, fee=30
        )

        at = AddressTable()
        pm_idx = at.add(v4_pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        v2_idx = at.add(v2_pair.address)
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

        v2_callback_cmds = enc_erc20_transfer(usdc_idx, v2_idx, AMOUNT_USDC)
        # V2_SWAP_COMPACT uses uint128 amount_out
        inner += enc_v2_swap_compact(
            v2_idx, v2_zfo, v2_amount_out, executor_idx, forward_data=v2_callback_cmds
        )
        inner += enc_v4_settle_delta(weth_idx)

        commands = enc_v4_unlock(inner)
        tx = executor.execute(
            # V4-V2 path; V2 callback payment reduces combined balance
            enc_preamble(at, skip_profit=True) + commands,
            sender=owner_account,
            raise_on_revert=False,
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")


class TestV2ToV4:
    def test_v2_v4_weth_to_usdc(
        self, usdc, weth, owner_account, executor, v4_pm, v2_pair
    ):
        """V2→V4: V2 swap first, then unlock + V4 swap."""
        weth.mint(executor.address, AMOUNT_WETH, sender=owner_account)
        # Set up V2 pair with ample liquidity for K-invariant
        weth.mint(v2_pair.address, V2_LIQUIDITY_WETH, sender=owner_account)
        usdc.mint(v2_pair.address, V2_LIQUIDITY_USDC, sender=owner_account)
        v2_pair.sync(sender=owner_account)

        v2_zfo = v2_pair.token0() == weth.address
        v2_reserve_in = weth.balanceOf(v2_pair.address)
        v2_reserve_out = usdc.balanceOf(v2_pair.address)
        v2_amount_out = v2_get_amount_out(
            AMOUNT_WETH, v2_reserve_in, v2_reserve_out, fee=30
        )

        pool_b_key = _make_pool_key(
            weth.address, usdc.address, fee=3000, tick_spacing=60
        )
        pool_b_zfo = pool_b_key[0] == usdc.address
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_b_key,
            v2_amount_out,
            AMOUNT_WETH * 2,
            pool_b_zfo,
            output_token=weth,
        )

        at = AddressTable()
        pm_idx = at.add(v4_pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        v2_idx = at.add(v2_pair.address)
        zero_idx = at.add(ZERO_ADDRESS)

        v2_callback_cmds = enc_erc20_transfer(weth_idx, v2_idx, AMOUNT_WETH)
        outer = enc_v2_swap_compact(
            v2_idx, v2_zfo, v2_amount_out, executor_idx, forward_data=v2_callback_cmds
        )

        inner = enc_v4_sync(usdc_idx)
        inner += enc_erc20_transfer(usdc_idx, pm_idx, v2_amount_out)
        inner += enc_v4_settle()
        inner += enc_v4_swap_compact(
            weth_idx if pool_b_key[0] == weth.address else usdc_idx,
            usdc_idx if pool_b_key[1] == usdc.address else weth_idx,
            pool_b_key[2],
            pool_b_key[3],
            zero_idx,
            pool_b_zfo,
            v2_amount_out,
        )
        inner += enc_v4_take(weth_idx, executor_idx, AMOUNT_WETH * 2)

        outer += enc_v4_unlock(inner)
        tx = executor.execute(
            enc_preamble(at) + outer, sender=owner_account, raise_on_revert=False
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")


class TestV4ToV2SwapCalc:
    def test_v4_v2_swap_calc(self, usdc, weth, owner_account, executor, v4_pm, v2_pair):
        """V4→V2 using V2_SWAP_CALC with excess balance.

        V4_TAKE sends USDC directly to the V2 pair (direct custody).
        V2_SWAP_CALC reads excess balance (tokens deposited but not
        yet in reserves), computes output on-chain, swaps with no callback.
        """
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

        # Add liquidity to V2 pair and initialize reserves
        usdc.mint(v2_pair.address, 10_000 * 10**6, sender=owner_account)
        weth.mint(v2_pair.address, 5 * 10**18, sender=owner_account)
        v2_pair.sync(sender=owner_account)

        # V4→V2 path: selling USDC to V2, buying WETH from V2
        v2_zfo = v2_pair.token0() == usdc.address

        at = AddressTable()
        pm_idx = at.add(v4_pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        v2_idx = at.add(v2_pair.address)
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
        # V4_TAKE sends USDC directly to V2 pair (creates excess balance)
        inner += enc_v4_take(usdc_idx, v2_idx, AMOUNT_USDC)
        # V2_SWAP_CALC reads excess balance, computes output, no callback needed
        inner += enc_v2_swap_calc(v2_idx, v2_zfo, executor_idx, fee=30)
        inner += enc_v4_settle_delta(weth_idx)

        commands = enc_v4_unlock(inner)
        tx = executor.execute(
            # V4-V2 path; V2 callback payment reduces combined balance
            enc_preamble(at, skip_profit=True) + commands,
            sender=owner_account,
            raise_on_revert=False,
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")
