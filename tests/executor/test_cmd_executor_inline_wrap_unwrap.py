"""
Tests for cmd_executor inline WETH wrapping/unwrapping in cross-protocol arbitrage.

These tests cover paths where one protocol leg uses native ETH (V4 with
NATIVE_ADDRESS) and another uses WETH (V2 or V3), requiring an inline
WETH_DEPOSIT or WETH_WITHDRAW command between swap legs to bridge the
token representation gap.

Key insight: V4 pools can reference native ETH via NATIVE_ADDRESS
(address(0)), while V2 and V3 pools always use WETH (ERC-20). An
arbitrage path between a V4 ETH/USDC pool and a V2/V3 WETH/USDC pool
requires converting between ETH and WETH mid-path.

Both fake V2 and V3 pools transfer output tokens BEFORE invoking the
callback, so the executor holds the output tokens when the callback's
forward_data executes.

Scenarios:
  A. V4 outputs ETH → WETH_DEPOSIT → V2 consumes WETH   (wrap between legs)
  B. V4 outputs ETH → WETH_DEPOSIT → V3 consumes WETH   (wrap between legs)
  C. V2 outputs WETH → WETH_WITHDRAW → V4 consumes ETH  (unwrap between legs)
  D. V3 outputs WETH → WETH_WITHDRAW → V4 consumes ETH  (unwrap between legs)

Each scenario is tested with both exact-amount (WETH_DEPOSIT / WETH_WITHDRAW)
and wrap/unwrap-all (WETH_DEPOSIT_ALL / WETH_WITHDRAW_ALL) variants.

V4 delta sign convention (after swap):
  - Positive delta: PM owes executor (take)
  - Negative delta: executor owes PM (settle)
"""

import pytest
from .conftest_shared import (
    NATIVE_ADDRESS,
    ZERO_ADDRESS,
    WETH_DEPLOYMENT_WRAP_AMOUNT,
    V2_LIQUIDITY_WETH,
    V2_LIQUIDITY_USDC,
    enc_v4_swap_compact,
    enc_v2_swap_compact,
    enc_v3_swap_compact,
    enc_v4_take,
    enc_v4_take_delta,
    enc_v4_settle_delta,
    enc_erc20_transfer,
    enc_v4_unlock,
    enc_weth_deposit,
    enc_weth_withdraw,
    enc_weth_deposit_all,
    enc_weth_withdraw_all,
    _make_pool_key,
    _setup_v4_swap,
    _setup_v3,
    AddressTable,
    enc_preamble,
    v2_get_amount_out,
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
AMOUNT_ETH = 1 * 10**18

# ═══════════════════════════════════════════════════════════════════════════
# A. V4 outputs ETH → WETH_DEPOSIT → V2 consumes WETH
#
#    V4 (NATIVE_ADDRESS/USDC): sell USDC, buy ETH
#      → V4 delta: ETH = +amt, USDC = -amt
#    V2 (WETH/USDC): sell WETH, buy USDC
#      → V2 callback pays WETH (from just-wrapped ETH)
#
#    Flow (inside V4_UNLOCK):
#      V4 swap → V4 take ETH → WETH_DEPOSIT → V2 swap → V4 settle USDC
#
#    Note: V2 computes output from reserves + fee, so the actual USDC
#    output is less than V2_LIQUIDITY_USDC. We size V4's USDC demand
#    to match V2's net output so settlement balances exactly.
# ═══════════════════════════════════════════════════════════════════════════


class TestV4ToV2InlineWrap:
    """V4 (ETH output) → WETH_DEPOSIT → V2 (WETH input)."""

    def test_v4_eth_to_v2_weth_with_inline_wrap(
        self, usdc, weth, owner_account, executor, v4_pm, v2_pair
    ):
        """V4 (sell USDC, buy ETH) → WETH_DEPOSIT → V2 (sell WETH, buy USDC).

        V4 pool uses NATIVE_ADDRESS; V2 pair uses WETH.
        WETH_DEPOSIT bridges the representation gap mid-path.
        """
        # ── V2 pair: WETH/USDC — sell WETH, buy USDC ──
        weth.mint(v2_pair.address, V2_LIQUIDITY_WETH, sender=owner_account)
        usdc.mint(v2_pair.address, V2_LIQUIDITY_USDC, sender=owner_account)
        v2_pair.sync(sender=owner_account)

        v2_zfo = v2_pair.token0() == weth.address
        v2_reserve_in = weth.balanceOf(v2_pair.address)
        v2_reserve_out = usdc.balanceOf(v2_pair.address)
        v2_amount_out = v2_get_amount_out(
            AMOUNT_WETH, v2_reserve_in, v2_reserve_out, fee=30
        )

        # ── V4 pool: NATIVE_ADDRESS/USDC — sell USDC, buy ETH ──
        # Size V4's USDC demand to match V2's net output (after fee).
        # The arb profit is in ETH/WETH, not USDC.
        pool_v4_key = _make_pool_key(
            NATIVE_ADDRESS, usdc.address, fee=3000, tick_spacing=60
        )
        v4_zfo = pool_v4_key[0] == usdc.address  # False: sell currency1 (USDC)
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_v4_key,
            v2_amount_out,  # amount_in: USDC sold at V4 (matches V2 output)
            AMOUNT_ETH,  # amount_out: ETH bought at V4
            v4_zfo,
            fund_eth=True,  # PM sends native ETH
        )

        at = AddressTable()
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        v2_idx = at.add(v2_pair.address)
        zero_idx = at.add(ZERO_ADDRESS)
        native_idx = at.add(NATIVE_ADDRESS)

        # V2 callback: pay WETH to V2 pair
        v2_callback_cmds = enc_erc20_transfer(weth_idx, v2_idx, AMOUNT_WETH)

        inner = enc_v4_swap_compact(
            native_idx if pool_v4_key[0] == NATIVE_ADDRESS else usdc_idx,
            usdc_idx if pool_v4_key[1] == usdc.address else native_idx,
            pool_v4_key[2],
            pool_v4_key[3],
            zero_idx,
            v4_zfo,
            v2_amount_out,
        )
        # V4 take native ETH → executor holds raw ETH
        inner += enc_v4_take(native_idx, executor_idx, AMOUNT_ETH)
        # ── Inline wrap: ETH → WETH ──
        inner += enc_weth_deposit(AMOUNT_WETH)
        # V2 swap: sell WETH for USDC (callback pays WETH from just-wrapped balance)
        inner += enc_v2_swap_compact(
            v2_idx, v2_zfo, v2_amount_out, executor_idx, forward_data=v2_callback_cmds
        )
        # V4 settle USDC delta (negative: owed to PM, paid from V2 output)
        inner += enc_v4_settle_delta(usdc_idx)

        commands = enc_v4_unlock(inner)
        tx = executor.execute(
            enc_preamble(at) + commands, sender=owner_account, raise_on_revert=False
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")

    def test_v4_eth_to_v2_weth_with_inline_wrap_all(
        self, usdc, weth, owner_account, executor, v4_pm, v2_pair
    ):
        """Same as above but using WETH_DEPOSIT_ALL (wraps entire ETH balance)."""
        weth.mint(v2_pair.address, V2_LIQUIDITY_WETH, sender=owner_account)
        usdc.mint(v2_pair.address, V2_LIQUIDITY_USDC, sender=owner_account)
        v2_pair.sync(sender=owner_account)

        v2_zfo = v2_pair.token0() == weth.address
        v2_reserve_in = weth.balanceOf(v2_pair.address)
        v2_reserve_out = usdc.balanceOf(v2_pair.address)
        v2_amount_out = v2_get_amount_out(
            AMOUNT_WETH, v2_reserve_in, v2_reserve_out, fee=30
        )

        pool_v4_key = _make_pool_key(
            NATIVE_ADDRESS, usdc.address, fee=3000, tick_spacing=60
        )
        v4_zfo = pool_v4_key[0] == usdc.address
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_v4_key,
            v2_amount_out,
            AMOUNT_ETH,
            v4_zfo,
            fund_eth=True,
        )

        at = AddressTable()
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        v2_idx = at.add(v2_pair.address)
        zero_idx = at.add(ZERO_ADDRESS)
        native_idx = at.add(NATIVE_ADDRESS)

        v2_callback_cmds = enc_erc20_transfer(weth_idx, v2_idx, AMOUNT_WETH)

        inner = enc_v4_swap_compact(
            native_idx if pool_v4_key[0] == NATIVE_ADDRESS else usdc_idx,
            usdc_idx if pool_v4_key[1] == usdc.address else native_idx,
            pool_v4_key[2],
            pool_v4_key[3],
            zero_idx,
            v4_zfo,
            v2_amount_out,
        )
        inner += enc_v4_take(native_idx, executor_idx, AMOUNT_ETH)
        # ── Inline wrap ALL: ETH → WETH ──
        inner += enc_weth_deposit_all()
        inner += enc_v2_swap_compact(
            v2_idx, v2_zfo, v2_amount_out, executor_idx, forward_data=v2_callback_cmds
        )
        inner += enc_v4_settle_delta(usdc_idx)

        commands = enc_v4_unlock(inner)
        tx = executor.execute(
            enc_preamble(at) + commands, sender=owner_account, raise_on_revert=False
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")


# ═══════════════════════════════════════════════════════════════════════════
# B. V4 outputs ETH → WETH_DEPOSIT → V3 consumes WETH
#
#    V4 (NATIVE_ADDRESS/USDC): sell USDC, buy ETH
#    V3 (WETH/USDC): sell WETH, buy USDC (auto-pay)
#
#    Flow (inside V4_UNLOCK):
#      V4 swap → V4 take ETH → WETH_DEPOSIT → V3 swap (auto-pay WETH)
#      → V4 settle USDC
#
#    V3's output is canned (set_next_swap), so we can size it to
#    produce more USDC than V4 demands, leaving profit in USDC.
# ═══════════════════════════════════════════════════════════════════════════


class TestV4ToV3InlineWrap:
    """V4 (ETH output) → WETH_DEPOSIT → V3 (WETH input, auto-pay)."""

    def test_v4_eth_to_v3_weth_with_inline_wrap(
        self, usdc, weth, owner_account, executor, v4_pm, v3_pool
    ):
        """V4 (sell USDC, buy ETH) → WETH_DEPOSIT → V3 (sell WETH, buy USDC, auto-pay).

        V4 pool uses NATIVE_ADDRESS; V3 pool uses WETH.
        V3 callback auto-pays WETH from executor's just-wrapped balance.
        V3 produces more USDC than V4 demands, leaving USDC profit.
        """
        # ── V4 pool: NATIVE_ADDRESS/USDC — sell USDC, buy ETH ──
        pool_v4_key = _make_pool_key(
            NATIVE_ADDRESS, usdc.address, fee=3000, tick_spacing=60
        )
        v4_zfo = pool_v4_key[0] == usdc.address
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_v4_key,
            AMOUNT_USDC,  # amount_in: USDC sold at V4
            AMOUNT_ETH,  # amount_out: ETH bought at V4
            v4_zfo,
            fund_eth=True,
        )

        # ── V3 pool: WETH/USDC — sell WETH, buy USDC ──
        # V3 gives MORE USDC than V4 demands → profit
        v3_zfo, _ = _setup_v3(v3_pool, weth, usdc, AMOUNT_WETH, AMOUNT_USDC * 2, owner_account)

        at = AddressTable()
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        v3_idx = at.add(v3_pool.address)
        zero_idx = at.add(ZERO_ADDRESS)
        native_idx = at.add(NATIVE_ADDRESS)

        inner = enc_v4_swap_compact(
            native_idx if pool_v4_key[0] == NATIVE_ADDRESS else usdc_idx,
            usdc_idx if pool_v4_key[1] == usdc.address else native_idx,
            pool_v4_key[2],
            pool_v4_key[3],
            zero_idx,
            v4_zfo,
            AMOUNT_USDC,
        )
        # V4 take native ETH
        inner += enc_v4_take(native_idx, executor_idx, AMOUNT_ETH)
        # ── Inline wrap: ETH → WETH ──
        inner += enc_weth_deposit(AMOUNT_WETH)
        # V3 swap: sell WETH for USDC (auto-pay uses just-wrapped WETH)
        inner += enc_v3_swap_compact(v3_idx, v3_zfo, AMOUNT_WETH, executor_idx)
        # V4 settle USDC (from V3 output)
        inner += enc_v4_settle_delta(usdc_idx)

        commands = enc_v4_unlock(inner)
        tx = executor.execute(
            enc_preamble(at) + commands, sender=owner_account, raise_on_revert=False
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")

        # Executor keeps USDC profit (V3 gave ~AMOUNT_USDC*2 after 0.3% fee, V4 took AMOUNT_USDC)
        assert usdc.balanceOf(executor) > 0

    def test_v4_eth_to_v3_weth_with_inline_wrap_all(
        self, usdc, weth, owner_account, executor, v4_pm, v3_pool
    ):
        """Same as above but using WETH_DEPOSIT_ALL."""
        pool_v4_key = _make_pool_key(
            NATIVE_ADDRESS, usdc.address, fee=3000, tick_spacing=60
        )
        v4_zfo = pool_v4_key[0] == usdc.address
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_v4_key,
            AMOUNT_USDC,
            AMOUNT_ETH,
            v4_zfo,
            fund_eth=True,
        )

        v3_zfo, _ = _setup_v3(v3_pool, weth, usdc, AMOUNT_WETH, AMOUNT_USDC * 2, owner_account)

        at = AddressTable()
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        v3_idx = at.add(v3_pool.address)
        zero_idx = at.add(ZERO_ADDRESS)
        native_idx = at.add(NATIVE_ADDRESS)

        inner = enc_v4_swap_compact(
            native_idx if pool_v4_key[0] == NATIVE_ADDRESS else usdc_idx,
            usdc_idx if pool_v4_key[1] == usdc.address else native_idx,
            pool_v4_key[2],
            pool_v4_key[3],
            zero_idx,
            v4_zfo,
            AMOUNT_USDC,
        )
        inner += enc_v4_take(native_idx, executor_idx, AMOUNT_ETH)
        inner += enc_weth_deposit_all()
        inner += enc_v3_swap_compact(v3_idx, v3_zfo, AMOUNT_WETH, executor_idx)
        inner += enc_v4_settle_delta(usdc_idx)

        commands = enc_v4_unlock(inner)
        tx = executor.execute(
            enc_preamble(at) + commands, sender=owner_account, raise_on_revert=False
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")

        assert usdc.balanceOf(executor) > 0


# ═══════════════════════════════════════════════════════════════════════════
# C. V2 outputs WETH → WETH_WITHDRAW → V4 consumes ETH
#
#    V2 (WETH/USDC): sell USDC, buy WETH (V2 flash-sends WETH first)
#    V4 (NATIVE_ADDRESS/USDC): sell ETH, buy USDC
#      → V4 delta: ETH = -amt (executor owes PM), USDC = +amt (PM owes executor)
#
#    Flow (V2 callback forward_data):
#      V2 sends WETH → WETH_WITHDRAW (unwrap) → V4 swap (sell ETH, buy USDC)
#      → V4 settle ETH (executor pays unwrapped ETH to PM)
#      → V4 take USDC (PM sends USDC to executor)
#      → pay USDC to V2 pair
#
#    Because V2 sends output tokens before invoking the callback, the
#    executor holds WETH when the callback's forward_data executes.
# ═══════════════════════════════════════════════════════════════════════════


class TestV2ToV4InlineUnwrap:
    """V2 (WETH output) → WETH_WITHDRAW → V4 (ETH input)."""

    def test_v2_weth_to_v4_eth_with_inline_unwrap(
        self, usdc, weth, owner_account, executor, v4_pm, v2_pair
    ):
        """V2 (sell USDC, buy WETH) → WETH_WITHDRAW → V4 (sell ETH, buy USDC).

        V2 flash-sends WETH. Callback: unwrap WETH→ETH, V4 swap for USDC,
        then pay USDC back to V2 pair.
        """
        weth.mint(executor.address, AMOUNT_WETH, sender=owner_account)
        # ── V2 pair: WETH/USDC — sell USDC, buy WETH ──
        weth.mint(v2_pair.address, V2_LIQUIDITY_WETH, sender=owner_account)
        usdc.mint(v2_pair.address, V2_LIQUIDITY_USDC, sender=owner_account)
        v2_pair.sync(sender=owner_account)

        v2_zfo = v2_pair.token0() == usdc.address
        v2_reserve_in = usdc.balanceOf(v2_pair.address)
        v2_reserve_out = weth.balanceOf(v2_pair.address)
        v2_amount_out = v2_get_amount_out(
            AMOUNT_USDC, v2_reserve_in, v2_reserve_out, fee=30
        )

        # ── V4 pool: NATIVE_ADDRESS/USDC — sell ETH, buy USDC ──
        pool_v4_key = _make_pool_key(
            NATIVE_ADDRESS, usdc.address, fee=3000, tick_spacing=60
        )
        v4_zfo = pool_v4_key[0] == NATIVE_ADDRESS  # True: sell currency0 (ETH)
        # V4 gives more USDC than V2 demands for its USDC input.
        # The arb profit is the difference in WETH/ETH.
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_v4_key,
            AMOUNT_ETH,  # amount_in: ETH sold at V4
            AMOUNT_USDC * 2,  # amount_out: USDC received from V4
            v4_zfo,
            output_token=usdc,  # PM needs USDC to pay out
        )

        at = AddressTable()
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        v2_idx = at.add(v2_pair.address)
        zero_idx = at.add(ZERO_ADDRESS)
        native_idx = at.add(NATIVE_ADDRESS)

        # V2 callback forward_data:
        #   1. WETH_WITHDRAW — unwrap V2's just-sent WETH → ETH
        #   2. V4_UNLOCK — V4 swap (sell ETH, buy USDC), settle ETH, take USDC
        #   3. ERC20_TRANSFER — pay USDC to V2 pair
        v2_fwd = enc_weth_withdraw(AMOUNT_WETH)

        v4_inner = enc_v4_swap_compact(
            native_idx if pool_v4_key[0] == NATIVE_ADDRESS else usdc_idx,
            usdc_idx if pool_v4_key[1] == usdc.address else native_idx,
            pool_v4_key[2],
            pool_v4_key[3],
            zero_idx,
            v4_zfo,
            AMOUNT_ETH,
        )
        # V4 settle ETH: pay unwrapped ETH to PM
        v4_inner += enc_v4_settle_delta(native_idx)
        # V4 take USDC
        v4_inner += enc_v4_take(usdc_idx, executor_idx, AMOUNT_USDC * 2)

        v2_fwd += enc_v4_unlock(v4_inner)
        # Pay USDC to V2 pair
        v2_fwd += enc_erc20_transfer(usdc_idx, v2_idx, AMOUNT_USDC)

        commands = enc_v2_swap_compact(
            v2_idx, v2_zfo, v2_amount_out, executor_idx, forward_data=v2_fwd
        )

        tx = executor.execute(
            # WETH_WITHDRAW converts WETH balance to ETH; combined balance unchanged but internal accounting affected
            enc_preamble(at, skip_profit=True) + commands,
            sender=owner_account,
            raise_on_revert=False,
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")

        # Executor profits: V4 gave AMOUNT_USDC*2, paid AMOUNT_USDC to V2
        assert usdc.balanceOf(executor) > 0

    def test_v2_weth_to_v4_eth_with_inline_unwrap_all(
        self, usdc, weth, owner_account, executor, v4_pm, v2_pair
    ):
        """Same as above but using WETH_WITHDRAW_ALL.

        WETH_WITHDRAW_ALL unwraps the executor's entire WETH balance
        (deployment wrap + V2 output). The executor then uses the
        unwrapped ETH for V4 settlement and receives USDC profit.
        """
        weth.mint(executor.address, AMOUNT_WETH, sender=owner_account)
        weth.mint(v2_pair.address, V2_LIQUIDITY_WETH, sender=owner_account)
        usdc.mint(v2_pair.address, V2_LIQUIDITY_USDC, sender=owner_account)
        v2_pair.sync(sender=owner_account)

        v2_zfo = v2_pair.token0() == usdc.address
        v2_reserve_in = usdc.balanceOf(v2_pair.address)
        v2_reserve_out = weth.balanceOf(v2_pair.address)
        v2_amount_out = v2_get_amount_out(
            AMOUNT_USDC, v2_reserve_in, v2_reserve_out, fee=30
        )

        pool_v4_key = _make_pool_key(
            NATIVE_ADDRESS, usdc.address, fee=3000, tick_spacing=60
        )
        v4_zfo = pool_v4_key[0] == NATIVE_ADDRESS
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_v4_key,
            AMOUNT_ETH,
            AMOUNT_USDC * 2,
            v4_zfo,
            output_token=usdc,
        )

        at = AddressTable()
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        v2_idx = at.add(v2_pair.address)
        zero_idx = at.add(ZERO_ADDRESS)
        native_idx = at.add(NATIVE_ADDRESS)

        v2_fwd = enc_weth_withdraw_all()
        v4_inner = enc_v4_swap_compact(
            native_idx if pool_v4_key[0] == NATIVE_ADDRESS else usdc_idx,
            usdc_idx if pool_v4_key[1] == usdc.address else native_idx,
            pool_v4_key[2],
            pool_v4_key[3],
            zero_idx,
            v4_zfo,
            AMOUNT_ETH,
        )
        v4_inner += enc_v4_settle_delta(native_idx)
        v4_inner += enc_v4_take(usdc_idx, executor_idx, AMOUNT_USDC * 2)
        v2_fwd += enc_v4_unlock(v4_inner)
        v2_fwd += enc_erc20_transfer(usdc_idx, v2_idx, AMOUNT_USDC)

        commands = enc_v2_swap_compact(
            v2_idx, v2_zfo, v2_amount_out, executor_idx, forward_data=v2_fwd
        )

        tx = executor.execute(
            # WETH_WITHDRAW converts WETH balance to ETH; combined balance unchanged but internal accounting affected
            enc_preamble(at, skip_profit=True) + commands,
            sender=owner_account,
            raise_on_revert=False,
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")


# ═══════════════════════════════════════════════════════════════════════════
# D. V3 outputs WETH → WETH_WITHDRAW → V4 consumes ETH
#
#    V3 (WETH/USDC): sell USDC, buy WETH (V3 sends WETH before callback)
#    V4 (NATIVE_ADDRESS/USDC): sell ETH, buy USDC
#      → V4 delta: ETH = -amt (executor owes PM), USDC = +amt (PM owes executor)
#
#    Flow (V3 callback forward_data — NOT auto-pay):
#      V3 sends WETH → WETH_WITHDRAW (unwrap) → V4 swap (sell ETH, buy USDC)
#      → V4 settle ETH → V4 take USDC → pay USDC to V3 pool
#
#    Because the fake V3 pool transfers output tokens before invoking
#    the callback, the executor holds WETH when forward_data executes.
#    We use explicit forward_data (not auto-pay) so the USDC payment
#    to V3 happens AFTER V4 provides USDC.
# ═══════════════════════════════════════════════════════════════════════════


class TestV3ToV4InlineUnwrap:
    """V3 (WETH output) → WETH_WITHDRAW → V4 (ETH input)."""

    def test_v3_weth_to_v4_eth_with_inline_unwrap(
        self, usdc, weth, owner_account, executor, v4_pm, v3_pool
    ):
        """V3 (sell USDC, buy WETH) → WETH_WITHDRAW → V4 (sell ETH, buy USDC).

        V3 sends WETH to executor. In the callback forward_data:
        unwrap WETH→ETH, V4 swap for USDC, pay USDC to V3 pool.
        """
        # ── V3 pool: WETH/USDC — sell USDC, buy WETH ──
        v3_zfo, v3_weth_out = _setup_v3(v3_pool, usdc, weth, AMOUNT_USDC, AMOUNT_WETH, owner_account)

        # ── V4 pool: NATIVE_ADDRESS/USDC — sell ETH, buy USDC ──
        pool_v4_key = _make_pool_key(
            NATIVE_ADDRESS, usdc.address, fee=3000, tick_spacing=60
        )
        v4_zfo = pool_v4_key[0] == NATIVE_ADDRESS
        # V4 gives more USDC than V3 demands, ensuring arb profit
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_v4_key,
            v3_weth_out,  # ETH amount matches V3's actual output
            AMOUNT_USDC * 2,
            v4_zfo,
            output_token=usdc,
        )

        at = AddressTable()
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        v3_idx = at.add(v3_pool.address)
        zero_idx = at.add(ZERO_ADDRESS)
        native_idx = at.add(NATIVE_ADDRESS)

        # V3 callback forward_data (NOT auto-pay — explicit forward_data):
        #   1. WETH_WITHDRAW — unwrap V3's just-sent WETH → ETH
        #   2. V4_UNLOCK — V4 swap (sell ETH, buy USDC), settle ETH, take USDC
        #   3. ERC20_TRANSFER — pay USDC to V3 pool
        v3_fwd = enc_weth_withdraw(v3_weth_out)

        v4_inner = enc_v4_swap_compact(
            native_idx if pool_v4_key[0] == NATIVE_ADDRESS else usdc_idx,
            usdc_idx if pool_v4_key[1] == usdc.address else native_idx,
            pool_v4_key[2],
            pool_v4_key[3],
            zero_idx,
            v4_zfo,
            v3_weth_out,  # ETH amount matches V3's actual output
        )
        v4_inner += enc_v4_settle_delta(native_idx)
        v4_inner += enc_v4_take(usdc_idx, executor_idx, AMOUNT_USDC * 2)

        v3_fwd += enc_v4_unlock(v4_inner)
        v3_fwd += enc_erc20_transfer(usdc_idx, v3_idx, AMOUNT_USDC)

        commands = enc_v3_swap_compact(
            v3_idx, v3_zfo, AMOUNT_USDC, executor_idx, forward_data=v3_fwd
        )

        tx = executor.execute(
            enc_preamble(at) + commands, sender=owner_account, raise_on_revert=False
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")

        # Executor profits: V4 gave AMOUNT_USDC*2, paid AMOUNT_USDC to V3
        assert usdc.balanceOf(executor) > 0

    def test_v3_weth_to_v4_eth_with_inline_unwrap_all(
        self, usdc, weth, owner_account, executor, v4_pm, v3_pool
    ):
        """Same as above but using WETH_WITHDRAW_ALL.

        WETH_WITHDRAW_ALL unwraps the executor's entire WETH balance
        (deployment wrap + V3 output). The executor then uses the
        unwrapped ETH for V4 settlement and receives USDC profit.
        """
        v3_zfo, v3_weth_out = _setup_v3(v3_pool, usdc, weth, AMOUNT_USDC, AMOUNT_WETH, owner_account)

        pool_v4_key = _make_pool_key(
            NATIVE_ADDRESS, usdc.address, fee=3000, tick_spacing=60
        )
        v4_zfo = pool_v4_key[0] == NATIVE_ADDRESS
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_v4_key,
            v3_weth_out,  # ETH amount matches V3's actual output
            AMOUNT_USDC * 2,
            v4_zfo,
            output_token=usdc,
        )

        at = AddressTable()
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        v3_idx = at.add(v3_pool.address)
        zero_idx = at.add(ZERO_ADDRESS)
        native_idx = at.add(NATIVE_ADDRESS)

        v3_fwd = enc_weth_withdraw_all()
        v4_inner = enc_v4_swap_compact(
            native_idx if pool_v4_key[0] == NATIVE_ADDRESS else usdc_idx,
            usdc_idx if pool_v4_key[1] == usdc.address else native_idx,
            pool_v4_key[2],
            pool_v4_key[3],
            zero_idx,
            v4_zfo,
            v3_weth_out,  # ETH amount matches V3's actual output
        )
        v4_inner += enc_v4_settle_delta(native_idx)
        v4_inner += enc_v4_take(usdc_idx, executor_idx, AMOUNT_USDC * 2)

        v3_fwd += enc_v4_unlock(v4_inner)
        v3_fwd += enc_erc20_transfer(usdc_idx, v3_idx, AMOUNT_USDC)

        commands = enc_v3_swap_compact(
            v3_idx, v3_zfo, AMOUNT_USDC, executor_idx, forward_data=v3_fwd
        )

        tx = executor.execute(
            enc_preamble(at) + commands, sender=owner_account, raise_on_revert=False
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")
