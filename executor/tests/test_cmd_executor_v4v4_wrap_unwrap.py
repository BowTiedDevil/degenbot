"""
Tests for cmd_executor inline WETH wrapping/unwrapping within V4-V4 paths.

These tests cover paths where one V4 pool uses native ETH (NATIVE_ADDRESS)
and another uses WETH, requiring an inline WETH_DEPOSIT or WETH_WITHDRAW
command between V4 swaps — all inside a single V4_UNLOCK.

Unlike the cross-protocol tests (V4→V2, V4→V3) where the wrap/unwrap
bridges a V4 callback into a V2/V3 callback, these tests keep everything
inside one V4 unlock session. The executor takes tokens out of V4 (via
V4_TAKE), converts between ETH and WETH, then sells the converted token
at the second V4 pool and settles the remaining deltas.

Scenarios:
  A. V4 take WETH → WETH_WITHDRAW → V4 sell ETH   (unwrap between V4 legs)
  B. V4 take ETH  → WETH_DEPOSIT  → V4 sell WETH   (wrap between V4 legs)

Each scenario is tested with both exact-amount and _ALL variants.
"""

import pytest
from .conftest_shared import (
    NATIVE_ADDRESS,
    ZERO_ADDRESS,
    WETH_DEPLOYMENT_WRAP_AMOUNT,
    enc_v4_swap_compact,
    enc_v4_take,
    enc_v4_take_delta,
    enc_v4_settle_delta,
    enc_v4_unlock,
    enc_weth_deposit,
    enc_weth_withdraw,
    enc_weth_deposit_all,
    enc_weth_withdraw_all,
    _make_pool_key,
    _setup_v4_swap,
    AddressTable,
    enc_preamble,
)

AMOUNT_WETH = 1 * 10**18
AMOUNT_USDC = 2000 * 10**6
AMOUNT_ETH = 1 * 10**18

# ═══════════════════════════════════════════════════════════════════════════
# A. V4 take WETH → WETH_WITHDRAW → V4 sell ETH
#
#    Pool A (WETH/USDC): sell USDC, buy WETH
#    V4_TAKE(WETH): receive WETH from PM
#    WETH_WITHDRAW: unwrap WETH → ETH
#    Pool B (NATIVE_ADDRESS/USDC): sell ETH, buy USDC
#    V4 settle: pay ETH to PM, take USDC profit
# ═══════════════════════════════════════════════════════════════════════════


class TestV4WethToV4Eth:
    """V4 (WETH output) → WETH_WITHDRAW → V4 (ETH input)."""

    def test_v4_weth_to_v4_eth_with_inline_unwrap(
        self, usdc, weth, owner_account, executor, v4_pm
    ):
        """V4 (sell USDC, buy WETH) → WETH_WITHDRAW → V4 (sell ETH, buy USDC)."""
        # Pool A: WETH/USDC — sell USDC, buy WETH
        pool_a_key = _make_pool_key(
            weth.address, usdc.address, fee=3000, tick_spacing=60
        )
        pool_a_zfo = pool_a_key[0] == usdc.address
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_a_key,
            AMOUNT_USDC,
            AMOUNT_WETH,
            pool_a_zfo,
            output_token=weth,
        )

        # Pool B: NATIVE_ADDRESS/USDC — sell ETH, buy USDC
        pool_b_key = _make_pool_key(
            NATIVE_ADDRESS, usdc.address, fee=500, tick_spacing=10
        )
        pool_b_zfo = pool_b_key[0] == NATIVE_ADDRESS
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_b_key,
            AMOUNT_ETH,
            AMOUNT_USDC * 2,
            pool_b_zfo,
            output_token=usdc,
        )

        at = AddressTable()
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
            AMOUNT_USDC,
        )
        inner += enc_v4_take(weth_idx, executor_idx, AMOUNT_WETH)
        inner += enc_weth_withdraw(AMOUNT_WETH)
        inner += enc_v4_swap_compact(
            native_idx if pool_b_key[0] == NATIVE_ADDRESS else usdc_idx,
            usdc_idx if pool_b_key[1] == usdc.address else native_idx,
            pool_b_key[2],
            pool_b_key[3],
            zero_idx,
            pool_b_zfo,
            AMOUNT_ETH,
        )
        inner += enc_v4_settle_delta(native_idx)
        inner += enc_v4_take_delta(usdc_idx, executor_idx)

        commands = enc_v4_unlock(inner)
        tx = executor.execute(
            enc_preamble(at) + commands, sender=owner_account, raise_on_revert=False
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")

        assert usdc.balanceOf(executor) >= AMOUNT_USDC

    def test_v4_weth_to_v4_eth_with_inline_unwrap_all(
        self, usdc, weth, owner_account, executor, v4_pm
    ):
        """Same but using WETH_WITHDRAW_ALL."""
        pool_a_key = _make_pool_key(
            weth.address, usdc.address, fee=3000, tick_spacing=60
        )
        pool_a_zfo = pool_a_key[0] == usdc.address
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_a_key,
            AMOUNT_USDC,
            AMOUNT_WETH,
            pool_a_zfo,
            output_token=weth,
        )

        pool_b_key = _make_pool_key(
            NATIVE_ADDRESS, usdc.address, fee=500, tick_spacing=10
        )
        pool_b_zfo = pool_b_key[0] == NATIVE_ADDRESS
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_b_key,
            AMOUNT_ETH,
            AMOUNT_USDC * 2,
            pool_b_zfo,
            output_token=usdc,
        )

        at = AddressTable()
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
            AMOUNT_USDC,
        )
        inner += enc_v4_take(weth_idx, executor_idx, AMOUNT_WETH)
        inner += enc_weth_withdraw_all()
        inner += enc_v4_swap_compact(
            native_idx if pool_b_key[0] == NATIVE_ADDRESS else usdc_idx,
            usdc_idx if pool_b_key[1] == usdc.address else native_idx,
            pool_b_key[2],
            pool_b_key[3],
            zero_idx,
            pool_b_zfo,
            AMOUNT_ETH,
        )
        inner += enc_v4_settle_delta(native_idx)
        inner += enc_v4_take_delta(usdc_idx, executor_idx)

        commands = enc_v4_unlock(inner)
        tx = executor.execute(
            enc_preamble(at) + commands, sender=owner_account, raise_on_revert=False
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")


# ═══════════════════════════════════════════════════════════════════════════
# B. V4 take ETH → WETH_DEPOSIT → V4 sell WETH
#
#    Pool A (NATIVE_ADDRESS/USDC): sell USDC, buy ETH
#    V4_TAKE(ETH): receive ETH from PM
#    WETH_DEPOSIT: wrap ETH → WETH
#    Pool B (WETH/USDC): sell WETH, buy USDC
#    V4 settle: pay WETH to PM, take USDC profit
# ═══════════════════════════════════════════════════════════════════════════


class TestV4EthToV4Weth:
    """V4 (ETH output) → WETH_DEPOSIT → V4 (WETH input)."""

    def test_v4_eth_to_v4_weth_with_inline_wrap(
        self, usdc, weth, owner_account, executor, v4_pm
    ):
        """V4 (sell USDC, buy ETH) → WETH_DEPOSIT → V4 (sell WETH, buy USDC)."""
        # Pool A: NATIVE_ADDRESS/USDC — sell USDC, buy ETH
        pool_a_key = _make_pool_key(
            NATIVE_ADDRESS, usdc.address, fee=3000, tick_spacing=60
        )
        pool_a_zfo = pool_a_key[0] == usdc.address
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_a_key,
            AMOUNT_USDC,
            AMOUNT_ETH,
            pool_a_zfo,
            fund_eth=True,
        )

        # Pool B: WETH/USDC — sell WETH, buy USDC
        pool_b_key = _make_pool_key(
            weth.address, usdc.address, fee=500, tick_spacing=10
        )
        pool_b_zfo = pool_b_key[0] == weth.address
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_b_key,
            AMOUNT_WETH,
            AMOUNT_USDC * 2,
            pool_b_zfo,
            output_token=usdc,
        )

        at = AddressTable()
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        zero_idx = at.add(ZERO_ADDRESS)
        native_idx = at.add(NATIVE_ADDRESS)

        inner = enc_v4_swap_compact(
            native_idx if pool_a_key[0] == NATIVE_ADDRESS else usdc_idx,
            usdc_idx if pool_a_key[1] == usdc.address else native_idx,
            pool_a_key[2],
            pool_a_key[3],
            zero_idx,
            pool_a_zfo,
            AMOUNT_USDC,
        )
        inner += enc_v4_take(native_idx, executor_idx, AMOUNT_ETH)
        inner += enc_weth_deposit(AMOUNT_WETH)
        inner += enc_v4_swap_compact(
            weth_idx if pool_b_key[0] == weth.address else usdc_idx,
            usdc_idx if pool_b_key[1] == usdc.address else weth_idx,
            pool_b_key[2],
            pool_b_key[3],
            zero_idx,
            pool_b_zfo,
            AMOUNT_WETH,
        )
        inner += enc_v4_settle_delta(weth_idx)
        inner += enc_v4_take_delta(usdc_idx, executor_idx)

        commands = enc_v4_unlock(inner)
        tx = executor.execute(
            enc_preamble(at) + commands, sender=owner_account, raise_on_revert=False
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")

        assert usdc.balanceOf(executor) >= AMOUNT_USDC

    def test_v4_eth_to_v4_weth_with_inline_wrap_all(
        self, usdc, weth, owner_account, executor, v4_pm
    ):
        """Same but using WETH_DEPOSIT_ALL."""
        pool_a_key = _make_pool_key(
            NATIVE_ADDRESS, usdc.address, fee=3000, tick_spacing=60
        )
        pool_a_zfo = pool_a_key[0] == usdc.address
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_a_key,
            AMOUNT_USDC,
            AMOUNT_ETH,
            pool_a_zfo,
            fund_eth=True,
        )

        pool_b_key = _make_pool_key(
            weth.address, usdc.address, fee=500, tick_spacing=10
        )
        pool_b_zfo = pool_b_key[0] == weth.address
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_b_key,
            AMOUNT_WETH,
            AMOUNT_USDC * 2,
            pool_b_zfo,
            output_token=usdc,
        )

        at = AddressTable()
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        zero_idx = at.add(ZERO_ADDRESS)
        native_idx = at.add(NATIVE_ADDRESS)

        inner = enc_v4_swap_compact(
            native_idx if pool_a_key[0] == NATIVE_ADDRESS else usdc_idx,
            usdc_idx if pool_a_key[1] == usdc.address else native_idx,
            pool_a_key[2],
            pool_a_key[3],
            zero_idx,
            pool_a_zfo,
            AMOUNT_USDC,
        )
        inner += enc_v4_take(native_idx, executor_idx, AMOUNT_ETH)
        inner += enc_weth_deposit_all()
        inner += enc_v4_swap_compact(
            weth_idx if pool_b_key[0] == weth.address else usdc_idx,
            usdc_idx if pool_b_key[1] == usdc.address else weth_idx,
            pool_b_key[2],
            pool_b_key[3],
            zero_idx,
            pool_b_zfo,
            AMOUNT_WETH,
        )
        inner += enc_v4_settle_delta(weth_idx)
        inner += enc_v4_take_delta(usdc_idx, executor_idx)

        commands = enc_v4_unlock(inner)
        tx = executor.execute(
            enc_preamble(at) + commands, sender=owner_account, raise_on_revert=False
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")
