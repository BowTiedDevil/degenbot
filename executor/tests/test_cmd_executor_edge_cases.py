"""
Tests for cmd_executor edge cases: native ETH settlement, V2 direct swap.
"""

import pytest
from .conftest_shared import (
    NATIVE_ADDRESS,
    ZERO_ADDRESS,
    enc_v4_swap_compact,
    enc_v4_take,
    enc_v4_settle_delta,
    enc_v4_unlock,
    enc_v2_swap_compact,
    enc_erc20_transfer,
    _make_pool_key,
    _setup_v4_swap,
    _setup_v2_pair,
    AddressTable,
    enc_preamble,
)


@pytest.fixture
def v2_pair(project, owner_account, usdc, weth):
    token0, token1 = sorted([usdc.address, weth.address], key=lambda addr: addr.lower())
    return project.fake_uniswap_v2_pair.deploy(
        token0, token1, 0, 30, sender=owner_account
    )


AMOUNT_WETH = AMOUNT_ETH = 1 * 10**18
AMOUNT_USDC = 2000 * 10**6


class TestV4NativeEthSettlement:
    def test_v4_native_eth_input(self, usdc, weth, owner_account, executor, v4_pm):
        """V4 swap with native ETH input, settled via V4_SETTLE_DELTA.

        ETH native is always currency0 (0x0000 sorts first).
        Direction: sell ETH (token0) for USDC (token1), zfo=True.
        After swap: delta[NATIVE] negative (owe ETH), delta[USDC] positive (take).
        V4_SETTLE_DELTA handles native ETH by calling settle(value=owed).
        """
        executor.balance += AMOUNT_ETH
        pool_key = _make_pool_key(
            NATIVE_ADDRESS, usdc.address, fee=500, tick_spacing=10
        )
        # NATIVE:ETH is always token0 (0x0000 sorts lowest), so zfo=True = sell ETH for USDC
        zfo = True
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_key,
            AMOUNT_ETH,
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
        native_idx = at.add(NATIVE_ADDRESS)

        inner = enc_v4_swap_compact(
            native_idx,  # currency0 (always NATIVE for native pools)
            usdc_idx,  # currency1
            pool_key[2],
            pool_key[3],
            zero_idx,
            zfo,
            AMOUNT_ETH,
        )
        inner += enc_v4_take(usdc_idx, executor_idx, AMOUNT_USDC)
        inner += enc_v4_settle_delta(native_idx)  # settles owed native ETH

        commands = enc_v4_unlock(inner)
        # test verifies mechanics, not profitability
        tx = executor.execute(
            enc_preamble(at, skip_profit=True) + commands,
            sender=owner_account,
            raise_on_revert=False,
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")


class TestV2DirectSwapNoCallback:
    def test_v2_direct_swap_no_callback(
        self, usdc, weth, owner_account, executor, v2_pair
    ):
        """V2 swap without flash borrow (no callback, empty forward_data).

        Must pre-fund the V2 pair with input tokens (WETH) before calling swap,
        since the pair checks balances after the swap.
        """
        weth.mint(executor.address, AMOUNT_WETH, sender=owner_account)
        v2_zfo, v2_amount_out = _setup_v2_pair(
            v2_pair, weth, usdc, owner_account, AMOUNT_WETH
        )

        at = AddressTable()
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        v2_idx = at.add(v2_pair.address)

        # Pre-fund V2 pair with input tokens, then swap (no callback)
        commands = enc_erc20_transfer(weth_idx, v2_idx, AMOUNT_WETH)
        commands += enc_v2_swap_compact(v2_idx, v2_zfo, v2_amount_out, executor_idx)

        # test verifies mechanics, not profitability
        tx = executor.execute(
            enc_preamble(at, skip_profit=True) + commands,
            sender=owner_account,
            raise_on_revert=False,
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")
