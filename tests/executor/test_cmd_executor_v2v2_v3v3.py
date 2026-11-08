"""
Tests for cmd_executor V2-V2 and V3-V3 nested callback swaps.
"""

import pytest
from .conftest_shared import (
    WETH_DEPLOYMENT_WRAP_AMOUNT,
    V2_LIQUIDITY_WETH,
    V2_LIQUIDITY_USDC,
    enc_v2_swap_compact,
    enc_v3_swap_compact,
    enc_erc20_transfer,
    AddressTable,
    enc_preamble,
    v2_get_amount_out,
    _setup_v3,
)


@pytest.fixture
def v3a(project, owner_account, usdc, weth):
    token0, token1 = sorted([usdc.address, weth.address], key=lambda addr: addr.lower())
    return project.fake_uniswap_v3_pool.deploy(token0, token1, 0, 3000, sender=owner_account)


@pytest.fixture
def v3b(project, owner_account, usdc, weth):
    token0, token1 = sorted([usdc.address, weth.address], key=lambda addr: addr.lower())
    return project.fake_uniswap_v3_pool.deploy(token0, token1, 0, 3000, sender=owner_account)


@pytest.fixture
def v2a(project, owner_account, usdc, weth):
    token0, token1 = sorted([usdc.address, weth.address], key=lambda addr: addr.lower())
    return project.fake_uniswap_v2_pair.deploy(
        token0, token1, 0, 30, sender=owner_account
    )


@pytest.fixture
def v2b(project, owner_account, usdc, weth):
    token0, token1 = sorted([usdc.address, weth.address], key=lambda addr: addr.lower())
    return project.fake_uniswap_v2_pair.deploy(
        token0, token1, 0, 30, sender=owner_account
    )


AMOUNT_WETH = 1 * 10**18
AMOUNT_USDC = 2000 * 10**6


class TestV3V3:
    def test_v3_v3_nested_callback_double_autopay(
        self, usdc, weth, owner_account, executor, v3a, v3b
    ):
        """V3a swap (WETH→USDC) with forward_data containing V3b swap (USDC→WETH auto-pay).

        V3a uses forward_data to run V3b inside the callback.
        V3b uses auto-pay (empty forward_data) — callback auto-detects owed USDC.
        V3a callback also pays WETH to V3a pool explicitly (forward_data = pay V3a + V3b swap).
        """
        weth.mint(executor.address, AMOUNT_WETH, sender=owner_account)
        # V3a: WETH→USDC (input=WETH, zfo=input==token0)
        v3a_zfo, v3a_usdc_out = _setup_v3(v3a, weth, usdc, AMOUNT_WETH, AMOUNT_USDC, owner_account)

        # V3b: USDC→WETH (input=USDC, zfo=input==token0)
        # Use V3a's actual USDC output as V3b's input amount
        v3b_zfo, _ = _setup_v3(v3b, usdc, weth, v3a_usdc_out, AMOUNT_WETH * 2, owner_account)

        at = AddressTable()
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        v3a_idx = at.add(v3a.address)
        v3b_idx = at.add(v3b.address)

        # V3a callback: pay WETH to V3a, then V3b swap (auto-pay)
        v3a_callback_cmds = enc_erc20_transfer(weth_idx, v3a_idx, AMOUNT_WETH)
        v3a_callback_cmds += enc_v3_swap_compact(
            v3b_idx, v3b_zfo, v3a_usdc_out, executor_idx
        )

        commands = enc_v3_swap_compact(
            v3a_idx, v3a_zfo, AMOUNT_WETH, executor_idx, forward_data=v3a_callback_cmds
        )

        tx = executor.execute(
            enc_preamble(at) + commands, sender=owner_account, raise_on_revert=False
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")


class TestV2V2:
    def test_v2_v2_nested_callback(self, usdc, weth, owner_account, executor, v2a, v2b):
        """V2a: WETH→USDC (flash), V2b: USDC→WETH (direct, no callback).

        Flow: V2a flash sends USDC → callback → pay USDC to V2b → V2b sends WETH
        → pay WETH to V2a (flash repayment).
        """
        weth.mint(executor.address, AMOUNT_WETH, sender=owner_account)
        # V2a: WETH→USDC (input=WETH, zfo=input==token0)
        weth.mint(v2a.address, V2_LIQUIDITY_WETH, sender=owner_account)
        usdc.mint(v2a.address, V2_LIQUIDITY_USDC, sender=owner_account)
        v2a.sync(sender=owner_account)

        v2a_zfo = v2a.token0() == weth.address
        v2a_reserve_in = weth.balanceOf(v2a.address)
        v2a_reserve_out = usdc.balanceOf(v2a.address)
        v2a_amount_out = v2_get_amount_out(
            AMOUNT_WETH, v2a_reserve_in, v2a_reserve_out, fee=30
        )

        # V2b: USDC→WETH (input=USDC, zfo=input==token0)
        usdc.mint(v2b.address, V2_LIQUIDITY_USDC, sender=owner_account)
        weth.mint(v2b.address, V2_LIQUIDITY_WETH, sender=owner_account)
        v2b.sync(sender=owner_account)

        v2b_zfo = v2b.token0() == usdc.address
        v2b_reserve_in = usdc.balanceOf(v2b.address)
        v2b_reserve_out = weth.balanceOf(v2b.address)
        v2b_amount_out = v2_get_amount_out(
            v2a_amount_out, v2b_reserve_in, v2b_reserve_out, fee=30
        )

        at = AddressTable()
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        v2a_idx = at.add(v2a.address)
        v2b_idx = at.add(v2b.address)

        # V2a callback forward_data:
        # 1. Pay USDC to V2b (input for V2b swap)
        # 2. V2b swap (no callback) — sends WETH to executor
        # 3. Pay WETH to V2a (flash repayment)
        v2a_callback_cmds = enc_erc20_transfer(usdc_idx, v2b_idx, v2a_amount_out)
        v2a_callback_cmds += enc_v2_swap_compact(
            v2b_idx, v2b_zfo, v2b_amount_out, executor_idx
        )
        v2a_callback_cmds += enc_erc20_transfer(weth_idx, v2a_idx, AMOUNT_WETH)

        commands = enc_v2_swap_compact(
            v2a_idx,
            v2a_zfo,
            v2a_amount_out,
            executor_idx,
            forward_data=v2a_callback_cmds,
        )

        tx = executor.execute(
            # V2-V2 path; fake pool params don't guarantee profit in ETH+WETH terms
            enc_preamble(at, skip_profit=True) + commands,
            sender=owner_account,
            raise_on_revert=False,
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")
