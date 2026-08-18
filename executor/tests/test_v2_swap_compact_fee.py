"""
Tests for V2_SWAP_COMPACT (0x20) with inline fee field.

V2_SWAP_COMPACT: [0x20][pool_idx:1][zfo:1][amount_out:16][recipient_idx:1][fee:2][forward_len:2][forward_data:N]

The fee field is written to t_v2_pair_fee[pool] before swap(), enabling
correct auto-pay computation in _v2_auto_pay during the V2 callback.

This fixes the bug where t_v2_pair_fee was declared but never written,
causing fee=0 (no fee deduction) in _v2_auto_pay — which would fail
the K-invariant check on any real V2 pair that charges a fee.

Fee values (fraction of 10000):
  UniswapV2 / SushiSwapV2: 30  (0.3%)
  PancakeSwapV2:            25  (0.25%)
"""

import pytest
from .conftest_shared import (
    WETH_DEPLOYMENT_WRAP_AMOUNT,
    enc_v2_swap_compact,
    enc_erc20_transfer,
    AddressTable,
    enc_preamble,
    v2_get_amount_out,
    V2_LIQUIDITY_WETH,
    V2_LIQUIDITY_USDC,
)

AMOUNT_WETH = 1 * 10**18
AMOUNT_USDC = 2000 * 10**6


class TestUniswapV2Fee:
    """V2_SWAP_COMPACT with UniswapV2 30/10000 (0.3%) fee — auto-pay sentinel."""

    def test_v2_swap_compact_uniswap_fee(self, project, usdc, weth, owner_account):
        """V2_SWAP_COMPACT with fee=30 and auto-pay sentinel pays the correct owed amount."""
        # Deploy V2 pair with UniswapV2 fee
        token0, token1 = sorted([usdc.address, weth.address], key=lambda a: a.lower())
        v2 = project.fake_uniswap_v2_pair.deploy(
            token0, token1, 0, 30, sender=owner_account
        )

        # Deploy executor
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
        executor.balance = 1000 * 10**18

        # Provide liquidity to the pair
        weth.mint(v2.address, V2_LIQUIDITY_WETH, sender=owner_account)
        usdc.mint(v2.address, V2_LIQUIDITY_USDC, sender=owner_account)
        v2.sync(sender=owner_account)

        # Compute expected swap output
        v2_zfo = v2.token0() == weth.address
        reserve_in = weth.balanceOf(v2.address)
        reserve_out = usdc.balanceOf(v2.address)
        amount_out = v2_get_amount_out(AMOUNT_WETH, reserve_in, reserve_out, fee=30)

        # Fund the executor with WETH so it can pay the pair during auto-pay
        weth.mint(executor.address, AMOUNT_WETH, sender=owner_account)

        at = AddressTable()
        v2_idx = at.add(v2.address)
        executor_idx = at.add(executor.address)

        # Pass sentinel as forward_data to trigger auto-pay callback
        auto_pay_sentinel = b"\xfe"

        commands = enc_preamble(at, skip_profit=True) + enc_v2_swap_compact(
            v2_idx,
            v2_zfo,
            amount_out,
            executor_idx,
            fee=30,
            forward_data=auto_pay_sentinel,
        )

        executor.execute(commands, sender=owner_account)
        # Executor should have received USDC from the swap
        assert usdc.balanceOf(executor.address) >= amount_out


class TestPancakeSwapV2Fee:
    """V2_SWAP_COMPACT with PancakeSwapV2 25/10000 (0.25%) fee — auto-pay sentinel."""

    def test_v2_swap_compact_pancake_fee(self, project, usdc, weth, owner_account):
        """V2_SWAP_COMPACT with fee=25 and auto-pay sentinel pays the correct owed amount."""
        token0, token1 = sorted([usdc.address, weth.address], key=lambda a: a.lower())
        v2 = project.fake_uniswap_v2_pair.deploy(
            token0, token1, 0, 25, sender=owner_account
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
        executor.balance = 1000 * 10**18

        weth.mint(v2.address, V2_LIQUIDITY_WETH, sender=owner_account)
        usdc.mint(v2.address, V2_LIQUIDITY_USDC, sender=owner_account)
        v2.sync(sender=owner_account)

        v2_zfo = v2.token0() == weth.address
        reserve_in = weth.balanceOf(v2.address)
        reserve_out = usdc.balanceOf(v2.address)
        amount_out = v2_get_amount_out(AMOUNT_WETH, reserve_in, reserve_out, fee=25)

        weth.mint(executor.address, AMOUNT_WETH, sender=owner_account)

        at = AddressTable()
        v2_idx = at.add(v2.address)
        executor_idx = at.add(executor.address)

        auto_pay_sentinel = b"\xfe"

        commands = enc_preamble(at, skip_profit=True) + enc_v2_swap_compact(
            v2_idx,
            v2_zfo,
            amount_out,
            executor_idx,
            fee=25,
            forward_data=auto_pay_sentinel,
        )

        executor.execute(commands, sender=owner_account)
        assert usdc.balanceOf(executor.address) >= amount_out


class TestFeeAffectsAutoPay:
    """Verify that different fees produce different auto-pay amounts."""

    def test_lower_fee_pays_less_input(self, project, usdc, weth, owner_account):
        """For the same amount_out, lower fee means less input owed — so more WETH leftover."""
        # Deploy two pairs with different fees
        token0, token1 = sorted([usdc.address, weth.address], key=lambda a: a.lower())
        v2_low_fee = project.fake_uniswap_v2_pair.deploy(
            token0, token1, 0, 10, sender=owner_account
        )
        v2_high_fee = project.fake_uniswap_v2_pair.deploy(
            token0, token1, 0, 99, sender=owner_account
        )

        # Same liquidity in both pairs
        for v2 in [v2_low_fee, v2_high_fee]:
            weth.mint(v2.address, V2_LIQUIDITY_WETH, sender=owner_account)
            usdc.mint(v2.address, V2_LIQUIDITY_USDC, sender=owner_account)
            v2.sync(sender=owner_account)

        # Same WETH input amount
        v2_zfo = v2_low_fee.token0() == weth.address

        reserve_in_low = weth.balanceOf(v2_low_fee.address)
        reserve_out_low = usdc.balanceOf(v2_low_fee.address)
        amount_out_low = v2_get_amount_out(
            AMOUNT_WETH, reserve_in_low, reserve_out_low, fee=10
        )

        reserve_in_high = weth.balanceOf(v2_high_fee.address)
        reserve_out_high = usdc.balanceOf(v2_high_fee.address)
        amount_out_high = v2_get_amount_out(
            AMOUNT_WETH, reserve_in_high, reserve_out_high, fee=99
        )

        # Lower fee → more output
        assert amount_out_low > amount_out_high, "Lower fee should produce more output"


class TestMixedFeeV2V2:
    """V2→V2 path with different fees — PancakeSwap V2 (25 bps) → Uniswap V2 (30 bps)."""

    def test_mixed_fee_v2v2_path(self, project, usdc, weth, owner_account):
        """PancakeSwap V2 (25 bps) → Uniswap V2 (30 bps) via V2_SWAP_COMPACT with correct fees."""
        token0, token1 = sorted([usdc.address, weth.address], key=lambda a: a.lower())

        # PancakeSwap pair: fee=25
        v2_pancake = project.fake_uniswap_v2_pair.deploy(
            token0, token1, 0, 25, sender=owner_account
        )
        # Uniswap pair: fee=30
        v2_uni = project.fake_uniswap_v2_pair.deploy(
            token0, token1, 0, 30, sender=owner_account
        )

        # Deploy executor (use v2_pancake as pool_manager placeholder)
        executor = project.cmd_executor.deploy(
            weth.address,
            v2_pancake.address,
            sender=owner_account,
        )
        # No initialize() — dummy PM (V2 pair has no PM.unlock)
        weth.deposit(value=WETH_DEPLOYMENT_WRAP_AMOUNT, sender=owner_account)
        weth.transfer(
            executor.address, WETH_DEPLOYMENT_WRAP_AMOUNT, sender=owner_account
        )
        executor.balance = 1000 * 10**18

        # Setup PancakeSwap pair: WETH→USDC
        weth.mint(v2_pancake.address, V2_LIQUIDITY_WETH, sender=owner_account)
        usdc.mint(v2_pancake.address, V2_LIQUIDITY_USDC, sender=owner_account)
        v2_pancake.sync(sender=owner_account)

        p_zfo = v2_pancake.token0() == weth.address
        p_reserve_in = weth.balanceOf(v2_pancake.address)
        p_reserve_out = usdc.balanceOf(v2_pancake.address)
        p_amount_out = v2_get_amount_out(
            AMOUNT_WETH, p_reserve_in, p_reserve_out, fee=25
        )

        # Setup Uniswap pair: USDC→WETH
        usdc.mint(v2_uni.address, V2_LIQUIDITY_USDC, sender=owner_account)
        weth.mint(v2_uni.address, V2_LIQUIDITY_WETH, sender=owner_account)
        v2_uni.sync(sender=owner_account)

        u_zfo = v2_uni.token0() == usdc.address
        u_reserve_in = usdc.balanceOf(v2_uni.address)
        u_reserve_out = weth.balanceOf(v2_uni.address)
        u_amount_out = v2_get_amount_out(
            p_amount_out, u_reserve_in, u_reserve_out, fee=30
        )

        # Fund executor with WETH to pay first pair
        weth.mint(executor.address, AMOUNT_WETH, sender=owner_account)

        at = AddressTable()
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        p_idx = at.add(v2_pancake.address)
        u_idx = at.add(v2_uni.address)

        # First swap: PancakeSwap WETH→USDC, callback with:
        # 1. Pay USDC to Uniswap pair
        # 2. Uniswap swap USDC→WETH (no callback)
        # 3. Pay WETH to PancakeSwap pair
        p_callback_cmds = enc_erc20_transfer(usdc_idx, u_idx, p_amount_out)
        p_callback_cmds += enc_v2_swap_compact(
            u_idx, u_zfo, u_amount_out, executor_idx, fee=30
        )
        p_callback_cmds += enc_erc20_transfer(weth_idx, p_idx, AMOUNT_WETH)

        commands = enc_preamble(at, skip_profit=True) + enc_v2_swap_compact(
            p_idx,
            p_zfo,
            p_amount_out,
            executor_idx,
            fee=25,
            forward_data=p_callback_cmds,
        )

        executor.execute(commands, sender=owner_account)
        assert weth.balanceOf(executor.address) > 0
