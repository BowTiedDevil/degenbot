"""
Tests for V2 swaps with configurable swap fees (fraction of 10000).

UniswapV2 and SushiSwapV2 use 30/10000 (0.3%).
PancakeSwapV2 uses 25/10000 (0.25%).

Validates that V2_SWAP_CALC correctly computes amounts from on-chain
excess balance (balanceOf(pair) - reserves) with fees in the sub-1%
range (10–99 / 10000).

V2_SWAP_CALC reads the "excess" tokens deposited to the V2 pair:
  excess = ERC20(input_token).balanceOf(pair) - reserves[input_index]
This equals tokens deposited to the pair but not yet reflected in
reserves. No callback is needed — the pair already holds the input.

With runtime K-invariant enforcement (no set_next_swap), the V2 pair
verifies the constant-product invariant after each swap, ensuring
correct fee handling.
"""

import pytest
from .conftest_shared import (
    WETH_DEPLOYMENT_WRAP_AMOUNT,
    enc_v2_swap_calc,
    AddressTable,
    enc_preamble,
    v2_get_amount_out,
)

AMOUNT_WETH = 1 * 10**18
AMOUNT_USDC = 2000 * 10**6  # Liquidity provision (not the swap output)
SWAP_INPUT = 1 * 10**18  # WETH deposited to create excess balance


def _deploy_v2_pair(project, weth, usdc, fee, owner_account, callback_variant=0):
    """Deploy a fake V2 pair with a given swap fee."""
    token0, token1 = sorted([usdc.address, weth.address], key=lambda a: a.lower())
    return project.fake_uniswap_v2_pair.deploy(
        token0, token1, callback_variant, fee, sender=owner_account
    )


def _deploy_executor(project, weth, v2_pair, owner_account):
    """Deploy cmd_executor (V2 pair used as pool_manager placeholder)."""
    contract = project.cmd_executor.deploy(
        weth.address,
        v2_pair.address,
        sender=owner_account,
    )
    # No initialize() — test uses v2_pair as dummy PM (no real PM.unlock)
    weth.deposit(value=WETH_DEPLOYMENT_WRAP_AMOUNT, sender=owner_account)
    weth.transfer(contract.address, WETH_DEPLOYMENT_WRAP_AMOUNT, sender=owner_account)
    contract.balance = 1000 * 10**18
    return contract


def _setup_pair_and_swap(
    v2, usdc, weth, owner_account, fee, zfo, swap_input=SWAP_INPUT
):
    """Fund a V2 pair with liquidity, then deposit input tokens to create
    excess balance. The pair enforces K-invariant at runtime (no
    set_next_swap needed).

    Flow:
    1. Add liquidity to pair (both sides) and sync()
    2. Read reserves after sync
    3. Compute expected output from V2 constant-product formula
    4. Deposit input tokens (creates excess balance = deposit amount)

    V2_SWAP_CALC reads: excess = balanceOf(pair) - reserves[input_index],
    then computes amount_out from reserves + fee + excess.
    """
    # 1. Provide liquidity to the pair and initialize reserves
    usdc.mint(v2.address, AMOUNT_USDC, sender=owner_account)
    weth.mint(v2.address, AMOUNT_WETH, sender=owner_account)
    v2.sync(sender=owner_account)

    # 2. Read reserves after liquidity provision
    reserve_in = weth.balanceOf(v2.address) if zfo else usdc.balanceOf(v2.address)
    reserve_out = usdc.balanceOf(v2.address) if zfo else weth.balanceOf(v2.address)

    # 3. Compute expected output from V2 constant-product formula
    expected_out = v2_get_amount_out(swap_input, reserve_in, reserve_out, fee)

    # 4. Deposit input tokens to create excess balance
    # This increases balanceOf(pair) but reserves are already snapshotted
    input_token = weth if zfo else usdc
    input_token.mint(v2.address, swap_input, sender=owner_account)

    return expected_out


class TestUniswapV2Fee:
    """Standard 30/10000 (0.3%) fee — UniswapV2 & SushiSwapV2."""

    def test_v2_swap_calc_uniswap_fee(self, project, usdc, weth, owner_account):
        v2 = _deploy_v2_pair(project, weth, usdc, 30, owner_account)
        executor = _deploy_executor(project, weth, v2, owner_account)

        v2_zfo = v2.token0() == weth.address
        expected_out = _setup_pair_and_swap(
            v2, usdc, weth, owner_account, fee=30, zfo=v2_zfo
        )

        at = AddressTable()
        v2_idx = at.add(v2.address)
        executor_idx = at.add(executor.address)

        commands = enc_preamble(at) + enc_v2_swap_calc(
            v2_idx, v2_zfo, executor_idx, fee=30
        )

        executor.execute(commands, sender=owner_account)
        assert usdc.balanceOf(executor.address) >= expected_out


class TestPancakeSwapV2Fee:
    """25/10000 (0.25%) fee — PancakeSwapV2."""

    def test_v2_swap_calc_pancake_fee(self, project, usdc, weth, owner_account):
        v2 = _deploy_v2_pair(project, weth, usdc, 25, owner_account)
        executor = _deploy_executor(project, weth, v2, owner_account)

        v2_zfo = v2.token0() == weth.address
        expected_out = _setup_pair_and_swap(
            v2, usdc, weth, owner_account, fee=25, zfo=v2_zfo
        )

        at = AddressTable()
        v2_idx = at.add(v2.address)
        executor_idx = at.add(executor.address)

        commands = enc_preamble(at) + enc_v2_swap_calc(
            v2_idx, v2_zfo, executor_idx, fee=25
        )

        executor.execute(commands, sender=owner_account)
        assert usdc.balanceOf(executor.address) >= expected_out


class TestLowFees:
    """Sub-1% fees: 10/10000 (0.1%) and 99/10000 (0.99%)."""

    def test_v2_swap_calc_fee_10(self, project, usdc, weth, owner_account):
        """0.1% fee — very low fee DEX."""
        v2 = _deploy_v2_pair(project, weth, usdc, 10, owner_account)
        executor = _deploy_executor(project, weth, v2, owner_account)

        v2_zfo = v2.token0() == weth.address
        expected_out = _setup_pair_and_swap(
            v2, usdc, weth, owner_account, fee=10, zfo=v2_zfo
        )

        at = AddressTable()
        v2_idx = at.add(v2.address)
        executor_idx = at.add(executor.address)

        commands = enc_preamble(at) + enc_v2_swap_calc(
            v2_idx, v2_zfo, executor_idx, fee=10
        )

        executor.execute(commands, sender=owner_account)
        assert usdc.balanceOf(executor.address) >= expected_out

    def test_v2_swap_calc_fee_50(self, project, usdc, weth, owner_account):
        """0.5% fee — mid-range."""
        v2 = _deploy_v2_pair(project, weth, usdc, 50, owner_account)
        executor = _deploy_executor(project, weth, v2, owner_account)

        v2_zfo = v2.token0() == weth.address
        expected_out = _setup_pair_and_swap(
            v2, usdc, weth, owner_account, fee=50, zfo=v2_zfo
        )

        at = AddressTable()
        v2_idx = at.add(v2.address)
        executor_idx = at.add(executor.address)

        commands = enc_preamble(at) + enc_v2_swap_calc(
            v2_idx, v2_zfo, executor_idx, fee=50
        )

        executor.execute(commands, sender=owner_account)
        assert usdc.balanceOf(executor.address) >= expected_out

    def test_v2_swap_calc_fee_99(self, project, usdc, weth, owner_account):
        """0.99% fee — near 1% ceiling of sub-1% range."""
        v2 = _deploy_v2_pair(project, weth, usdc, 99, owner_account)
        executor = _deploy_executor(project, weth, v2, owner_account)

        v2_zfo = v2.token0() == weth.address
        expected_out = _setup_pair_and_swap(
            v2, usdc, weth, owner_account, fee=99, zfo=v2_zfo
        )

        at = AddressTable()
        v2_idx = at.add(v2.address)
        executor_idx = at.add(executor.address)

        commands = enc_preamble(at) + enc_v2_swap_calc(
            v2_idx, v2_zfo, executor_idx, fee=99
        )

        executor.execute(commands, sender=owner_account)
        assert usdc.balanceOf(executor.address) >= expected_out


class TestFeeAffectsOutput:
    """Verify that different fees produce different output amounts.

    With identical reserves and input, lower fee → more output.
    """

    def test_lower_fee_produces_more_output(self, project, usdc, weth, owner_account):
        """0.1% fee should produce more output than 0.99% fee for same input."""
        v2_low = _deploy_v2_pair(project, weth, usdc, 10, owner_account)
        v2_high = _deploy_v2_pair(project, weth, usdc, 99, owner_account)

        v2_zfo_low = v2_low.token0() == weth.address
        v2_zfo_high = v2_high.token0() == weth.address

        # Deploy executors
        exec_low = _deploy_executor(project, weth, v2_low, owner_account)
        exec_high = _deploy_executor(project, weth, v2_high, owner_account)

        # Setup pairs with identical liquidity and swap input
        expected_low = _setup_pair_and_swap(
            v2_low, usdc, weth, owner_account, fee=10, zfo=v2_zfo_low
        )
        expected_high = _setup_pair_and_swap(
            v2_high, usdc, weth, owner_account, fee=99, zfo=v2_zfo_high
        )

        # Lower fee should compute more output (same input + reserves, less fee taken)
        assert expected_low > expected_high

        at_low = AddressTable()
        v2_low_idx = at_low.add(v2_low.address)
        exec_low_idx = at_low.add(exec_low.address)

        at_high = AddressTable()
        v2_high_idx = at_high.add(v2_high.address)
        exec_high_idx = at_high.add(exec_high.address)

        commands_low = enc_preamble(at_low) + enc_v2_swap_calc(
            v2_low_idx, v2_zfo_low, exec_low_idx, fee=10
        )
        commands_high = enc_preamble(at_high) + enc_v2_swap_calc(
            v2_high_idx, v2_zfo_high, exec_high_idx, fee=99
        )

        exec_low.execute(commands_low, sender=owner_account)
        exec_high.execute(commands_high, sender=owner_account)

        usdc_low = usdc.balanceOf(exec_low.address)
        usdc_high = usdc.balanceOf(exec_high.address)

        # Lower fee → more USDC output
        assert usdc_low > usdc_high


class TestPancakeCallback:
    """PancakeSwap callback variant with 25/10000 fee.

    V2_SWAP_CALC with excess balance does not invoke a callback (data=b""),
    so the callback_variant setting is irrelevant. This test verifies
    compatibility with PancakeSwap V2 pairs regardless.
    """

    def test_pancake_callback_with_fee_25(self, project, usdc, weth, owner_account):
        """PancakeSwap callback (variant=2) with 0.25% fee."""
        v2 = _deploy_v2_pair(project, weth, usdc, 25, owner_account, callback_variant=2)
        executor = _deploy_executor(project, weth, v2, owner_account)

        v2_zfo = v2.token0() == weth.address
        expected_out = _setup_pair_and_swap(
            v2, usdc, weth, owner_account, fee=25, zfo=v2_zfo
        )

        at = AddressTable()
        v2_idx = at.add(v2.address)
        executor_idx = at.add(executor.address)

        commands = enc_preamble(at) + enc_v2_swap_calc(
            v2_idx, v2_zfo, executor_idx, fee=25
        )

        executor.execute(commands, sender=owner_account)
        assert usdc.balanceOf(executor.address) >= expected_out
