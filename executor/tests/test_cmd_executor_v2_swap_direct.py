"""
Tests for V2_SWAP_DIRECT (0x22) — explicit-amount, no-callback V2 swap.

V2_SWAP_DIRECT: [0x22][pool_idx:1][zfo:1][amount_out:16][recipient_idx:1]
= 20 bytes

Decouples callback routing from token destination: the V2 pair must
already hold input tokens (excess balance from pre-fund), and the
executor calls swap(data=b"") — no callback invoked.

Compared to V2_SWAP_CALC (0x21):
  + Saves ~4 staticcalls (~10K gas on cold slots): no getReserves,
    token0, token1, balanceOf, or getAmountOut computation
  + Calldata cost: 14 more bytes than CALC (20 vs 6)
  - Requires the caller to pre-compute amount_out off-chain

The V2 K-invariant check inside pair.swap() verifies correctness
(same safety guarantee as V2_SWAP_CALC).
"""

import pytest

from .conftest_shared import (
    ZERO_ADDRESS,
    WETH_DEPLOYMENT_WRAP_AMOUNT,
    enc_v2_swap_compact,
    enc_v2_swap_calc,
    enc_v2_swap_direct,
    enc_v3_swap_compact,
    enc_v4_swap_compact,
    enc_v4_take,
    enc_v4_sync,
    enc_v4_settle,
    enc_v4_settle_delta,
    enc_v4_unlock,
    enc_erc20_transfer,
    _make_pool_key,
    _setup_v4_swap,
    _setup_v2_pair,
    AddressTable,
    enc_preamble,
    v2_get_amount_out,
)
from .verify import count_transfers, snapshot_balances, diff_snapshots, NATIVE_ADDRESS

AMOUNT_WETH = 1 * 10**18
AMOUNT_USDC = 2000 * 10**6
AMOUNT_WBTC = 100 * 10**8
AMOUNT_WETH_PROFIT = 2 * 10**18
V2_FEE = 30

# ── Fixtures ──


@pytest.fixture
def v2_a(project, owner_account, weth, usdc):
    t0, t1 = sorted([weth.address, usdc.address], key=lambda a: a.lower())
    return project.fake_uniswap_v2_pair.deploy(t0, t1, 0, V2_FEE, sender=owner_account)


@pytest.fixture
def v2_b(project, owner_account, usdc, wbtc):
    t0, t1 = sorted([usdc.address, wbtc.address], key=lambda a: a.lower())
    return project.fake_uniswap_v2_pair.deploy(t0, t1, 0, V2_FEE, sender=owner_account)


@pytest.fixture
def v2_c(project, owner_account, wbtc, weth):
    t0, t1 = sorted([wbtc.address, weth.address], key=lambda a: a.lower())
    return project.fake_uniswap_v2_pair.deploy(t0, t1, 0, V2_FEE, sender=owner_account)


@pytest.fixture
def v3_c(project, owner_account, wbtc, weth):
    t0, t1 = sorted([wbtc.address, weth.address], key=lambda a: a.lower())
    return project.fake_uniswap_v3_pool.deploy(t0, t1, 0, 3000, sender=owner_account)


# ── Helpers ──


def _setup_v2_for_calc(
    pool, input_token, output_token, owner, amount_in, amount_out, fee=V2_FEE
):
    """Set up a V2 pair with ample liquidity at the correct price for V2_SWAP_CALC / V2_SWAP_DIRECT."""
    input_token.mint(pool.address, amount_in * 100, sender=owner)
    output_token.mint(pool.address, amount_out * 100, sender=owner)
    pool.sync(sender=owner)
    zfo = pool.token0() == input_token.address
    return zfo


def _setup_v2(pool, input_token, output_token, owner, amount_in, fee=V2_FEE):
    return _setup_v2_pair(pool, input_token, output_token, owner, amount_in, fee=fee)


def _setup_v3(pool, input_token, output_token, amount_in, amount_out, owner):
    zfo = pool.token0() == input_token.address
    output_token.mint(pool.address, amount_out, sender=owner)
    input_token.mint(pool.address, amount_in, sender=owner)
    pool.set_next_swap(amount_in, amount_out, zfo, sender=owner)
    return zfo


def _setup_v4_pool(
    pm,
    c0,
    c1,
    amount_in,
    amount_out,
    owner,
    fee=3000,
    tick_spacing=60,
    output_token=None,
):
    pool_key = _make_pool_key(c0, c1, fee=fee, tick_spacing=tick_spacing)
    zfo = pool_key[0] == c0
    _setup_v4_swap(
        pm, owner, pool_key, amount_in, amount_out, zfo, output_token=output_token
    )
    return pool_key, zfo


def _v4_swap(at, pool_key, zfo, amount, zero_idx):
    c0_idx = at.add(pool_key[0])
    c1_idx = at.add(pool_key[1])
    return enc_v4_swap_compact(
        c0_idx, c1_idx, pool_key[2], pool_key[3], zero_idx, zfo, amount
    )


def _v2_swap_direct(at, pool, zfo, amount_out, recipient_idx):
    return enc_v2_swap_direct(at.add(pool.address), zfo, amount_out, recipient_idx)


def _v2_swap_calc(at, pool, zfo, recipient_idx, fee=V2_FEE):
    return enc_v2_swap_calc(at.add(pool.address), zfo, recipient_idx, fee=fee)


def _v2_swap(at, pool, zfo, amount_out, recipient_idx, fee=30, forward_data=b""):
    return enc_v2_swap_compact(
        at.add(pool.address),
        zfo,
        amount_out,
        recipient_idx,
        fee=fee,
        forward_data=forward_data,
    )


def _v3_swap(at, pool, zfo, amount, recipient_idx, forward_data=b""):
    return enc_v3_swap_compact(
        at.add(pool.address), zfo, amount, recipient_idx, forward_data=forward_data
    )


def _erc20_xfer(at, token_idx, pool, amount):
    return enc_erc20_transfer(token_idx, at.add(pool.address), amount)


def _run_and_verify(
    executor,
    run_executor,
    at,
    commands,
    owner,
    tokens,
    accounts,
    expected_transfers,
    label,
    expected_weth_delta=None,
):
    """Execute a swap, then verify both transfer count and balance invariants."""
    before = snapshot_balances(tokens, accounts)
    tx = run_executor(at, commands, owner, skip_profit=True)
    actual = count_transfers(tx)
    assert actual == expected_transfers, (
        f"{label}: expected {expected_transfers} transfers, on-chain events show {actual}."
    )
    _verify_conservation(label, tokens, accounts, before, executor, expected_weth_delta)
    return tx


def _verify_conservation(
    label, tokens, accounts, before, executor, expected_weth_delta=None
):
    after = snapshot_balances(tokens, accounts)
    diffs = diff_snapshots(before, after)
    weth_addr = tokens[0].address
    executor_addr = executor.address if hasattr(executor, "address") else executor
    if expected_weth_delta is not None:
        weth_delta = diffs.get((weth_addr, executor_addr), 0)
        eth_delta = diffs.get((NATIVE_ADDRESS, executor_addr), 0)
        combined_delta = weth_delta + eth_delta
        assert combined_delta == expected_weth_delta, (
            f"{label}: executor WETH+ETH profit mismatch. "
            f"Expected {expected_weth_delta}, got {combined_delta}."
        )
    for token in tokens[1:]:
        token_addr = token.address if hasattr(token, "address") else token
        total = sum(v for (t, _), v in diffs.items() if t == token_addr)
        assert total == 0, f"{label}: {token_addr} conservation violated — sum={total}"
    weth_total = sum(v for (t, _), v in diffs.items() if t == weth_addr)
    eth_total = sum(v for (t, _), v in diffs.items() if t == NATIVE_ADDRESS)
    assert weth_total + eth_total == 0, f"{label}: WETH+ETH conservation violated"


# ═══════════════════════════════════════════════════════════════════════════
# Test: Basic V2_SWAP_DIRECT functionality
# ═══════════════════════════════════════════════════════════════════════════


class TestV2SwapDirectBasic:
    """Basic tests: V2_SWAP_DIRECT with pre-funded excess balance."""

    def test_v2_swap_direct_simple(
        self,
        run_executor, weth,
        usdc,
        owner_account,
        executor,
        v2_a,
    ):
        """V2_SWAP_DIRECT with executor pre-funding V2 pair via ERC20_TRANSFER.

        Flow:
        1. ERC20_TRANSFER WETH to V2a (creates excess balance)
        2. V2_SWAP_DIRECT at V2a (explicit amount_out, no callback)
        3. V2a K-invariant passes (excess = input, output from off-chain math)
        """
        weth.mint(executor.address, AMOUNT_WETH, sender=owner_account)
        a_zfo = _setup_v2_for_calc(
            v2_a, weth, usdc, owner_account, AMOUNT_WETH, AMOUNT_USDC
        )
        a_out = v2_get_amount_out(
            AMOUNT_WETH, weth.balanceOf(v2_a), usdc.balanceOf(v2_a), V2_FEE
        )

        at = AddressTable()
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        v2a_idx = at.add(v2_a.address)

        # Pre-fund V2a with WETH (creates excess balance), then V2_SWAP_DIRECT
        commands = enc_erc20_transfer(weth_idx, v2a_idx, AMOUNT_WETH)
        commands += enc_v2_swap_direct(v2a_idx, a_zfo, a_out, executor_idx)

        run_executor(at, commands, owner_account, skip_profit=True)

    def test_v2_swap_direct_v4_take_prefund(
        self,
        run_executor, weth,
        usdc,
        owner_account,
        executor,
        v4_pm,
        v2_a,
    ):
        """V2_SWAP_DIRECT with V4_TAKE pre-funding the V2 pair.

        This is the motivating use case: V4_TAKE sends tokens directly to
        a V2 pair, creating excess balance. V2_SWAP_DIRECT then swaps
        without on-chain computation (amounts pre-computed off-chain).

        Flow (V4→V2 single-hop):
        V4 unlock:
          1. V4 swap WETH→USDC
          2. V4_TAKE USDC→V2a (creates excess)
          3. V2_SWAP_DIRECT V2a (sends WETH back to executor)
          4. V4_SETTLE_DELTA WETH
        """
        weth.mint(executor.address, AMOUNT_WETH, sender=owner_account)
        a_zfo = _setup_v2_for_calc(
            v2_a, usdc, weth, owner_account, AMOUNT_USDC, AMOUNT_WETH
        )
        a_out = v2_get_amount_out(
            AMOUNT_USDC, usdc.balanceOf(v2_a), weth.balanceOf(v2_a), V2_FEE
        )

        pool_key, v4_zfo = _setup_v4_pool(
            v4_pm,
            weth.address,
            usdc.address,
            AMOUNT_WETH,
            AMOUNT_USDC,
            owner_account,
            fee=3000,
            tick_spacing=60,
            output_token=usdc,
        )

        at = AddressTable()
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        zero_idx = at.add(ZERO_ADDRESS)

        inner = _v4_swap(at, pool_key, v4_zfo, AMOUNT_WETH, zero_idx)
        inner += enc_v4_take(usdc_idx, at.add(v2_a.address), AMOUNT_USDC)
        inner += _v2_swap_direct(at, v2_a, a_zfo, a_out, executor_idx)
        inner += enc_v4_settle_delta(weth_idx)

        commands = enc_v4_unlock(inner)
        run_executor(at, commands, owner_account, skip_profit=True)


class TestV2SwapDirectVsCalc:
    """Compare V2_SWAP_DIRECT vs V2_SWAP_CALC — same results, different gas."""

    def test_v2_v2_v2_direct(
        self,
        run_executor, weth,
        usdc,
        wbtc,
        owner_account,
        executor,
        v2_a,
        v2_b,
        v2_c,
    ):
        """V2-V2-V2 with V2_SWAP_DIRECT instead of V2_SWAP_CALC.

        Same routing as the optimized TestV2V2V2 but using V2_SWAP_DIRECT
        for all V2 swaps (explicit amount, no on-chain computation).
        """
        a_zfo = _setup_v2_for_calc(
            v2_a, weth, usdc, owner_account, AMOUNT_WETH, AMOUNT_USDC
        )
        b_zfo = _setup_v2_for_calc(
            v2_b, usdc, wbtc, owner_account, AMOUNT_USDC, AMOUNT_WBTC
        )
        c_zfo = _setup_v2_for_calc(
            v2_c, wbtc, weth, owner_account, AMOUNT_WBTC, AMOUNT_WETH_PROFIT
        )

        # Compute chain amounts from V2 math (same as V2_SWAP_CALC would on-chain)
        a_out = v2_get_amount_out(
            AMOUNT_WETH, weth.balanceOf(v2_a), usdc.balanceOf(v2_a), V2_FEE
        )
        b_out = v2_get_amount_out(
            a_out, usdc.balanceOf(v2_b), wbtc.balanceOf(v2_b), V2_FEE
        )
        c_out = v2_get_amount_out(
            b_out, wbtc.balanceOf(v2_c), weth.balanceOf(v2_c), V2_FEE
        )

        at = AddressTable()
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)
        v2a_idx = at.add(v2_a.address)
        v2b_idx = at.add(v2_b.address)
        v2c_idx = at.add(v2_c.address)

        # Inside V2c callback: transfer WETH to V2a, then V2a→V2b, then V2b→V2c
        # Using V2_SWAP_DIRECT instead of V2_SWAP_CALC
        c_fwd = enc_erc20_transfer(weth_idx, v2a_idx, AMOUNT_WETH)
        c_fwd += enc_v2_swap_direct(v2a_idx, a_zfo, a_out, v2b_idx)
        c_fwd += enc_v2_swap_direct(v2b_idx, b_zfo, b_out, v2c_idx)

        # Flash borrow WETH from V2c — callback on executor
        commands = enc_v2_swap_compact(
            v2c_idx, c_zfo, c_out, executor_idx, forward_data=c_fwd
        )
        _run_and_verify(
            executor,
            run_executor,
            at,
            commands,
            owner_account,
            [weth, usdc, wbtc],
            [executor, v2_a, v2_b, v2_c],
            4,
            "TestV2V2V2.Direct",
        )


class TestV2SwapDirectThreeHop:
    """Three-hop paths using V2_SWAP_DIRECT — the motivating use case.

    These tests demonstrate the V2_SWAP_DIRECT command decoupling
    swap-amount specification from callback routing. When V4_TAKE
    pre-funds a V2 pair, the output amount is known off-chain —
    V2_SWAP_DIRECT avoids the on-chain computation that V2_SWAP_CALC
    performs (4 staticcalls: getReserves, token0, token1, balanceOf).
    """

    def test_v2_v4_v4_direct(
        self,
        run_executor, weth,
        usdc,
        wbtc,
        owner_account,
        executor,
        v4_pm,
        v2_a,
    ):
        """V2-V4-V4 with V2_SWAP_DIRECT (the path that motivated this command).

        Inside V4 unlock:
        1. V4_SYNC(USDC) — snapshot PM balance before V2a deposit
        2. V4_TAKE(WETH, V2a) — creates excess at V2a [1 xfer]
        3. V2_SWAP_DIRECT V2a→PM — sends USDC to PM (explicit amount, no callback) [1 xfer]
        4. V4_SETTLE() — credits +USDC delta
        5. V4b + V4c swaps (delta netting)
        6. V4_TAKE(WETH, executor, profit) [1 xfer]
        """
        a_zfo = _setup_v2_for_calc(
            v2_a, weth, usdc, owner_account, AMOUNT_WETH, AMOUNT_USDC
        )
        a_out = v2_get_amount_out(
            AMOUNT_WETH, weth.balanceOf(v2_a), usdc.balanceOf(v2_a), V2_FEE
        )

        b_pk, b_zfo = _setup_v4_pool(
            v4_pm,
            usdc.address,
            wbtc.address,
            a_out,
            AMOUNT_WBTC,
            owner_account,
            fee=500,
            tick_spacing=10,
            output_token=wbtc,
        )
        c_pk, c_zfo = _setup_v4_pool(
            v4_pm,
            wbtc.address,
            weth.address,
            AMOUNT_WBTC,
            AMOUNT_WETH_PROFIT,
            owner_account,
            fee=10000,
            tick_spacing=200,
            output_token=weth,
        )

        at = AddressTable()
        pm_idx = at.add(v4_pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)
        zero_idx = at.add(ZERO_ADDRESS)

        profit = AMOUNT_WETH_PROFIT - AMOUNT_WETH

        v4_inner = enc_v4_sync(usdc_idx)
        v4_inner += enc_v4_take(
            weth_idx, at.add(v2_a.address), AMOUNT_WETH
        )  # WETH→V2a (excess)
        v4_inner += _v2_swap_direct(at, v2_a, a_zfo, a_out, pm_idx)  # ← V2_SWAP_DIRECT
        v4_inner += enc_v4_settle()
        v4_inner += _v4_swap(at, b_pk, b_zfo, a_out, zero_idx)
        v4_inner += _v4_swap(at, c_pk, c_zfo, AMOUNT_WBTC, zero_idx)
        v4_inner += enc_v4_take(weth_idx, executor_idx, profit)

        commands = enc_v4_unlock(v4_inner)
        _run_and_verify(
            executor,
            run_executor,
            at,
            commands,
            owner_account,
            [weth, usdc, wbtc],
            [executor, v4_pm, v2_a],
            3,
            "TestV2V4V4.Direct",
        )

    def test_v4_v2_v4_direct(
        self,
        run_executor, weth,
        usdc,
        wbtc,
        owner_account,
        executor,
        v4_pm,
        v2_b,
    ):
        """V4-V2-V4 with V2_SWAP_DIRECT.

        V4_TAKE sends USDC directly to V2b (excess balance).
        V2_SWAP_DIRECT sends WBTC to executor (no callback, no on-chain computation).
        V4c swap consumes WBTC via delta.
        """
        a_pk, a_zfo = _setup_v4_pool(
            v4_pm,
            weth.address,
            usdc.address,
            AMOUNT_WETH,
            AMOUNT_USDC,
            owner_account,
            fee=3000,
            tick_spacing=60,
            output_token=usdc,
        )
        b_zfo = _setup_v2_for_calc(
            v2_b, usdc, wbtc, owner_account, AMOUNT_USDC, AMOUNT_WBTC
        )
        b_out = v2_get_amount_out(
            AMOUNT_USDC, usdc.balanceOf(v2_b), wbtc.balanceOf(v2_b), V2_FEE
        )

        c_pk, c_zfo = _setup_v4_pool(
            v4_pm,
            wbtc.address,
            weth.address,
            b_out,
            AMOUNT_WETH_PROFIT,
            owner_account,
            fee=10000,
            tick_spacing=200,
            output_token=weth,
        )

        at = AddressTable()
        pm_idx = at.add(v4_pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)
        zero_idx = at.add(ZERO_ADDRESS)

        inner = _v4_swap(at, a_pk, a_zfo, AMOUNT_WETH, zero_idx)
        inner += enc_v4_take(usdc_idx, at.add(v2_b.address), AMOUNT_USDC)
        inner += _v2_swap_direct(
            at, v2_b, b_zfo, b_out, executor_idx
        )  # ← V2_SWAP_DIRECT
        inner += _v4_swap(at, c_pk, c_zfo, b_out, zero_idx)
        inner += enc_v4_settle_delta(wbtc_idx)
        inner += enc_v4_settle_delta(weth_idx)

        commands = enc_v4_unlock(inner)
        _run_and_verify(
            executor,
            run_executor,
            at,
            commands,
            owner_account,
            [weth, usdc, wbtc],
            [executor, v4_pm, v2_b],
            4,
            "TestV4V2V4.Direct",
        )

    def test_v4_v2_v2_direct(
        self,
        run_executor, weth,
        usdc,
        wbtc,
        owner_account,
        executor,
        v4_pm,
        v2_b,
        v2_c,
    ):
        """V4-V2-V2 with V2_SWAP_DIRECT for both V2b and V2c.

        V4_TAKE sends USDC to V2b (excess), V2b sends WBTC to V2c (excess),
        V2c sends WETH to executor. All V2 swaps use DIRECT (no callbacks).
        """
        a_pk, a_zfo = _setup_v4_pool(
            v4_pm,
            weth.address,
            usdc.address,
            AMOUNT_WETH,
            AMOUNT_USDC,
            owner_account,
            fee=3000,
            tick_spacing=60,
            output_token=usdc,
        )
        b_zfo = _setup_v2_for_calc(
            v2_b, usdc, wbtc, owner_account, AMOUNT_USDC, AMOUNT_WBTC
        )
        b_out = v2_get_amount_out(
            AMOUNT_USDC, usdc.balanceOf(v2_b), wbtc.balanceOf(v2_b), V2_FEE
        )

        c_zfo = _setup_v2_for_calc(
            v2_c, wbtc, weth, owner_account, AMOUNT_WBTC, AMOUNT_WETH_PROFIT
        )
        c_out = v2_get_amount_out(
            b_out, wbtc.balanceOf(v2_c), weth.balanceOf(v2_c), V2_FEE
        )

        at = AddressTable()
        pm_idx = at.add(v4_pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)
        zero_idx = at.add(ZERO_ADDRESS)

        inner = _v4_swap(at, a_pk, a_zfo, AMOUNT_WETH, zero_idx)
        inner += enc_v4_take(usdc_idx, at.add(v2_b.address), AMOUNT_USDC)
        inner += _v2_swap_direct(
            at, v2_b, b_zfo, b_out, at.add(v2_c.address)
        )  # ← DIRECT
        inner += _v2_swap_direct(at, v2_c, c_zfo, c_out, executor_idx)  # ← DIRECT
        inner += enc_v4_settle_delta(weth_idx)

        commands = enc_v4_unlock(inner)
        _run_and_verify(
            executor,
            run_executor,
            at,
            commands,
            owner_account,
            [weth, usdc, wbtc],
            [executor, v4_pm, v2_b, v2_c],
            4,
            "TestV4V2V2.Direct",
        )

    def test_v2_v4_v2_direct(
        self,
        run_executor, weth,
        usdc,
        wbtc,
        owner_account,
        executor,
        v4_pm,
        v2_a,
        v2_c,
    ):
        """V2-V4-V2 with V2_SWAP_DIRECT for V2a→PM (delta netting).

        Same as the optimized three-hop V2-V4-V2 but using V2_SWAP_DIRECT
        for V2a→PM (explicit amount, no on-chain computation).
        """
        weth.mint(executor.address, AMOUNT_WETH, sender=owner_account)
        a_zfo = _setup_v2_for_calc(
            v2_a, weth, usdc, owner_account, AMOUNT_WETH, AMOUNT_USDC
        )
        a_out = v2_get_amount_out(
            AMOUNT_WETH, weth.balanceOf(v2_a), usdc.balanceOf(v2_a), V2_FEE
        )

        b_pk, b_zfo = _setup_v4_pool(
            v4_pm,
            usdc.address,
            wbtc.address,
            a_out,
            AMOUNT_WBTC,
            owner_account,
            fee=500,
            tick_spacing=10,
            output_token=wbtc,
        )
        c_zfo, c_out = _setup_v2(v2_c, wbtc, weth, owner_account, AMOUNT_WBTC)

        at = AddressTable()
        pm_idx = at.add(v4_pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)
        zero_idx = at.add(ZERO_ADDRESS)
        v2a_idx = at.add(v2_a.address)

        # V4 unlock: sync/settle USDC, V2a→PM (DIRECT), V4b swap, take WBTC→V2c
        v4_inner = enc_v4_sync(usdc_idx)
        v4_inner += _v2_swap_direct(
            at, v2_a, a_zfo, a_out, pm_idx
        )  # ← DIRECT instead of CALC
        v4_inner += enc_v4_settle()
        v4_inner += _v4_swap(at, b_pk, b_zfo, a_out, zero_idx)
        v4_inner += enc_v4_take(wbtc_idx, at.add(v2_c.address), AMOUNT_WBTC)

        # V2c fires first. Callback: WETH→V2a (excess), V4 unlock
        c_fwd = enc_erc20_transfer(weth_idx, v2a_idx, AMOUNT_WETH)
        c_fwd += enc_v4_unlock(v4_inner)

        commands = _v2_swap(at, v2_c, c_zfo, c_out, executor_idx, forward_data=c_fwd)
        _run_and_verify(
            executor,
            run_executor,
            at,
            commands,
            owner_account,
            [weth, usdc, wbtc],
            [executor, v4_pm, v2_a, v2_c],
            4,
            "TestV2V4V2.Direct",
        )


class TestV2SwapDirectGasComparison:
    """Gas comparison: V2_SWAP_DIRECT vs V2_SWAP_CALC.

    Both commands produce identical results but V2_SWAP_DIRECT saves
    ~10K gas on cold V2 pair slots by skipping on-chain amount computation.
    """

    def test_gas_comparison_v2_v4_v4(
        self,
        run_executor, weth,
        usdc,
        wbtc,
        owner_account,
        executor,
        v4_pm,
        v2_a,
    ):
        """Gas comparison: V2_SWAP_DIRECT vs V2_SWAP_CALC on V2-V4-V4 path."""
        # ── Setup shared state ──
        a_zfo = _setup_v2_for_calc(
            v2_a, weth, usdc, owner_account, AMOUNT_WETH, AMOUNT_USDC
        )
        a_out = v2_get_amount_out(
            AMOUNT_WETH, weth.balanceOf(v2_a), usdc.balanceOf(v2_a), V2_FEE
        )
        profit = AMOUNT_WETH_PROFIT - AMOUNT_WETH

        # ── V2_SWAP_CALC version ──
        b_pk, b_zfo = _setup_v4_pool(
            v4_pm,
            usdc.address,
            wbtc.address,
            a_out,
            AMOUNT_WBTC,
            owner_account,
            fee=500,
            tick_spacing=10,
            output_token=wbtc,
        )
        c_pk, c_zfo = _setup_v4_pool(
            v4_pm,
            wbtc.address,
            weth.address,
            AMOUNT_WBTC,
            AMOUNT_WETH_PROFIT,
            owner_account,
            fee=10000,
            tick_spacing=200,
            output_token=weth,
        )

        at_calc = AddressTable()
        pm_idx = at_calc.add(v4_pm.address)
        weth_idx = at_calc.add(weth.address)
        usdc_idx = at_calc.add(usdc.address)
        executor_idx = at_calc.add(executor.address)
        zero_idx = at_calc.add(ZERO_ADDRESS)

        v4_inner_calc = enc_v4_sync(usdc_idx)
        v4_inner_calc += enc_v4_take(weth_idx, at_calc.add(v2_a.address), AMOUNT_WETH)
        v4_inner_calc += _v2_swap_calc(at_calc, v2_a, a_zfo, pm_idx)  # V2_SWAP_CALC
        v4_inner_calc += enc_v4_settle()
        v4_inner_calc += _v4_swap(at_calc, b_pk, b_zfo, a_out, zero_idx)
        v4_inner_calc += _v4_swap(at_calc, c_pk, c_zfo, AMOUNT_WBTC, zero_idx)
        v4_inner_calc += enc_v4_take(weth_idx, executor_idx, profit)

        commands_calc = enc_v4_unlock(v4_inner_calc)
        tx_calc = executor.execute(
            enc_preamble(at_calc) + commands_calc, sender=owner_account
        )
        assert tx_calc.status == 1, "V2_SWAP_CALC version reverted"
        gas_calc = tx_calc.gas_used

        # ── Reset state ──
        v2_a.reset(sender=owner_account)

        # ── V2_SWAP_DIRECT version ──
        # Re-setup V2 pair (was reset)
        a_zfo2 = _setup_v2_for_calc(
            v2_a, weth, usdc, owner_account, AMOUNT_WETH, AMOUNT_USDC
        )
        a_out2 = v2_get_amount_out(
            AMOUNT_WETH, weth.balanceOf(v2_a), usdc.balanceOf(v2_a), V2_FEE
        )
        assert a_out2 == a_out, "V2 output amount should be identical after reset"

        # Re-setup V4 pools (set_next_swap was consumed)
        b_pk2, b_zfo2 = _setup_v4_pool(
            v4_pm,
            usdc.address,
            wbtc.address,
            a_out2,
            AMOUNT_WBTC,
            owner_account,
            fee=500,
            tick_spacing=10,
            output_token=wbtc,
        )
        c_pk2, c_zfo2 = _setup_v4_pool(
            v4_pm,
            wbtc.address,
            weth.address,
            AMOUNT_WBTC,
            AMOUNT_WETH_PROFIT,
            owner_account,
            fee=10000,
            tick_spacing=200,
            output_token=weth,
        )

        at_direct = AddressTable()
        pm_idx2 = at_direct.add(v4_pm.address)
        weth_idx2 = at_direct.add(weth.address)
        usdc_idx2 = at_direct.add(usdc.address)
        executor_idx2 = at_direct.add(executor.address)
        zero_idx2 = at_direct.add(ZERO_ADDRESS)

        v4_inner_direct = enc_v4_sync(usdc_idx2)
        v4_inner_direct += enc_v4_take(
            weth_idx2, at_direct.add(v2_a.address), AMOUNT_WETH
        )
        v4_inner_direct += _v2_swap_direct(
            at_direct, v2_a, a_zfo2, a_out2, pm_idx2
        )  # ← V2_SWAP_DIRECT
        v4_inner_direct += enc_v4_settle()
        v4_inner_direct += _v4_swap(at_direct, b_pk2, b_zfo2, a_out2, zero_idx2)
        v4_inner_direct += _v4_swap(at_direct, c_pk2, c_zfo2, AMOUNT_WBTC, zero_idx2)
        v4_inner_direct += enc_v4_take(weth_idx2, executor_idx2, profit)

        commands_direct = enc_v4_unlock(v4_inner_direct)
        tx_direct = executor.execute(
            enc_preamble(at_direct) + commands_direct, sender=owner_account
        )
        assert tx_direct.status == 1, "V2_SWAP_DIRECT version reverted"
        gas_direct = tx_direct.gas_used

        gas_saved = gas_calc - gas_direct
        print(f"\n  V2-V4-V4 gas comparison:")
        print(f"    V2_SWAP_CALC:   {gas_calc:>8,} gas")
        print(f"    V2_SWAP_DIRECT: {gas_direct:>8,} gas  (saves {gas_saved:+,})")

        # V2_SWAP_DIRECT should be cheaper (saves ~4 staticcalls minus calldata overhead)
        assert gas_direct < gas_calc, (
            f"V2_SWAP_DIRECT ({gas_direct} gas) should be cheaper than "
            f"V2_SWAP_CALC ({gas_calc} gas)"
        )
