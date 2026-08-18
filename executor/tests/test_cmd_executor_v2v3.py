"""
Tests for cmd_executor V2-V3 cross-protocol swaps.
"""

import pytest
from .conftest_shared import (
    V2_LIQUIDITY_WETH,
    V2_LIQUIDITY_USDC,
    enc_v3_swap_compact,
    enc_v2_swap_compact,
    enc_erc20_transfer,
    AddressTable,
    enc_preamble,
    v2_get_amount_out,
    _setup_v3,
)


@pytest.fixture
def v3_pool(project, owner_account, usdc, weth):
    token0, token1 = sorted([usdc.address, weth.address], key=lambda addr: addr.lower())
    return project.fake_uniswap_v3_pool.deploy(token0, token1, 0, 3000, sender=owner_account)


@pytest.fixture
def v2_pair(project, owner_account, usdc, weth):
    token0, token1 = sorted([usdc.address, weth.address], key=lambda addr: addr.lower())
    return project.fake_uniswap_v2_pair.deploy(
        token0, token1, 0, 30, sender=owner_account
    )


AMOUNT_WETH = 1 * 10**18
AMOUNT_USDC = 2000 * 10**6


class TestV2ToV3:
    def test_v2_v3_weth_usdc_to_weth(
        self, usdc, weth, owner_account, executor, v2_pair, v3_pool
    ):
        """V2→V3: WETH→USDC (V2 flash), USDC→WETH (V3 with auto-pay).

        Flow: V2 sends USDC to executor → callback → pay USDC to V3 → V3 sends WETH
        to executor → pay WETH to V2 pair.
        """
        # Executor needs starting WETH to cover V2 flash repayment
        # (V3 output is slightly less than AMOUNT_WETH due to 0.3% fee)
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

        v3_zfo, v3_amount_out = _setup_v3(
            v3_pool, usdc, weth, v2_amount_out, AMOUNT_WETH, owner_account
        )

        at = AddressTable()
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        v3_idx = at.add(v3_pool.address)
        v2_idx = at.add(v2_pair.address)

        # V3 callback forward_data: pay USDC to V3 pool (tokens must arrive DURING callback for IIA)
        v3_fwd = enc_erc20_transfer(usdc_idx, v3_idx, v2_amount_out)

        # V2 callback forward_data:
        # 1. V3 swap — V3 receives USDC during its callback (forward_data)
        # 2. Pay WETH to V2 pair (the borrowed amount)
        v2_fwd = enc_v3_swap_compact(
            v3_idx, v3_zfo, v2_amount_out, executor_idx, forward_data=v3_fwd
        )
        v2_fwd += enc_erc20_transfer(weth_idx, v2_idx, AMOUNT_WETH)

        commands = enc_v2_swap_compact(
            v2_idx, v2_zfo, v2_amount_out, executor_idx, forward_data=v2_fwd
        )

        tx = executor.execute(
            enc_preamble(at) + commands, sender=owner_account, raise_on_revert=False
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")


class TestV3ToV2:
    def test_v3_v2_weth_usdc_to_weth(
        self, usdc, weth, owner_account, executor, v2_pair, v3_pool
    ):
        """V3→V2: WETH→USDC (V3 auto-pay), USDC→WETH (V2 direct swap, no callback).

        Flow: V3 swap → callback pays WETH to V3 → V3 sends USDC to executor
        → transfer USDC to V2 pair → V2 swap (no callback).
        """
        weth.mint(executor.address, AMOUNT_WETH, sender=owner_account)
        v3_zfo, v3_amount_out = _setup_v3(
            v3_pool, weth, usdc, AMOUNT_WETH, AMOUNT_USDC, owner_account
        )

        # Set up V2 pair with ample liquidity for K-invariant
        usdc.mint(v2_pair.address, V2_LIQUIDITY_USDC, sender=owner_account)
        weth.mint(v2_pair.address, V2_LIQUIDITY_WETH, sender=owner_account)
        v2_pair.sync(sender=owner_account)

        v2_zfo = v2_pair.token0() == usdc.address
        v2_reserve_in = usdc.balanceOf(v2_pair.address)
        v2_reserve_out = weth.balanceOf(v2_pair.address)
        v2_amount_out = v2_get_amount_out(
            v3_amount_out, v2_reserve_in, v2_reserve_out, fee=30
        )

        at = AddressTable()
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        v3_idx = at.add(v3_pool.address)
        v2_idx = at.add(v2_pair.address)

        # V3 callback: pay WETH to V3 pool, then transfer USDC to V2 pair + V2 swap
        v3_callback_cmds = enc_erc20_transfer(weth_idx, v3_idx, AMOUNT_WETH)
        v3_callback_cmds += enc_erc20_transfer(usdc_idx, v2_idx, v3_amount_out)
        v3_callback_cmds += enc_v2_swap_compact(
            v2_idx, v2_zfo, v2_amount_out, executor_idx
        )

        commands = enc_v3_swap_compact(
            v3_idx, v3_zfo, AMOUNT_WETH, executor_idx, forward_data=v3_callback_cmds
        )

        tx = executor.execute(
            # V3-V2 path; V2 callback payment reduces combined balance
            enc_preamble(at, skip_profit=True) + commands,
            sender=owner_account,
            raise_on_revert=False,
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")
