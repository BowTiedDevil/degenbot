"""
Tests for cmd_executor three-hop swap execution using Uniswap V3 pools.

Exercises both V3 command types:
  - V3_SWAP_COMPACT (0x30): explicit amount_specified, optional forward_data,
    auto-pay when forward_data is empty.
  - V3_SWAP_DELTA (0x31): amount from PM exttload delta, auto-pay, 4-byte
    fixed-size command. Only usable after a V4 swap has established a PM delta
    AND the executor already holds the input ERC-20 tokens (see TestV4V3V3Delta
    for the constraints).

Token landscape:
  WETH (18 decimals), USDC (6 decimals), WBTC (8 decimals)

Pool topologies:
  V3a: WETH / USDC  — sell WETH for USDC
  V3b: USDC / WBTC  — sell USDC for WBTC
  V3c: WBTC / WETH  — sell WBTC for WETH

Arbitrage path: WETH → USDC (V3a) → WBTC (V3b) → WETH (V3c)

Test classes:
  1. TestV3V3V3NestedCallbacks   — V3→V3→V3 with nested forward_data (3 callbacks)
  2. TestV3V3V3InnerAutoPay      — V3→V3→V3, innermost V3 auto-pays
  3. TestV3V3V3MiddleAutoPay     — V3→V3→V3, middle V3 auto-pays
  4. TestV3V3V3DoubleAutoPay     — V3→V3→V3, both V3b+V3c auto-pay
  5. TestV4V3V3Delta             — V4→V3(SWAP_DELTA)→V3(COMPACT with auto-pay)
  6. TestV3V3V4                  — V3→V3→V4 (two V3 legs then V4)
  7. TestPancakeV3Callback       — V3→V3→V3 with PancakeSwap V3 callback variant
  8. TestGasComparison           — Gas comparison across approaches
"""

import pytest
from .conftest_shared import (
    ZERO_ADDRESS,
    WETH_DEPLOYMENT_WRAP_AMOUNT,
    _setup_v3,
    enc_v3_swap_compact,
    enc_v3_swap_delta,
    enc_v4_swap_compact,
    enc_v4_take,
    enc_v4_take_delta,
    enc_v4_sync,
    enc_v4_settle,
    enc_v4_settle_delta,
    enc_v4_unlock,
    enc_erc20_transfer,
    _make_pool_key,
    _setup_v4_swap,
    AddressTable,
    enc_preamble,
)

# ── Amounts ──

AMOUNT_WETH = 1 * 10**18
AMOUNT_USDC = 2000 * 10**6
AMOUNT_WBTC = 100 * 10**8
AMOUNT_WETH_PROFIT = 2 * 10**18

# ── Fixtures ──


@pytest.fixture
def v3a(project, owner_account, weth, usdc):
    """WETH/USDC V3 pool — sell WETH for USDC."""
    token0, token1 = sorted([weth.address, usdc.address], key=lambda a: a.lower())
    return project.fake_uniswap_v3_pool.deploy(token0, token1, 0, 3000, sender=owner_account)


@pytest.fixture
def v3b(project, owner_account, usdc, wbtc):
    """USDC/WBTC V3 pool — sell USDC for WBTC."""
    token0, token1 = sorted([usdc.address, wbtc.address], key=lambda a: a.lower())
    return project.fake_uniswap_v3_pool.deploy(token0, token1, 0, 3000, sender=owner_account)


@pytest.fixture
def v3c(project, owner_account, wbtc, weth):
    """WBTC/WETH V3 pool — sell WBTC for WETH (Uni V3 callback)."""
    token0, token1 = sorted([wbtc.address, weth.address], key=lambda a: a.lower())
    return project.fake_uniswap_v3_pool.deploy(token0, token1, 0, 3000, sender=owner_account)


@pytest.fixture
def v3c_pancake(project, owner_account, wbtc, weth):
    """WBTC/WETH V3 pool — PancakeSwap V3 callback variant."""
    token0, token1 = sorted([wbtc.address, weth.address], key=lambda a: a.lower())
    return project.fake_uniswap_v3_pool.deploy(token0, token1, 1, 3000, sender=owner_account)


# ── Helpers ──


def _setup_v3_pools(v3a, v3b, v3c, weth, usdc, wbtc, owner):
    """Configure all three V3 pools with ample liquidity.

    Returns (v3a_zfo, v3b_zfo, v3c_zfo, v3a_usdc_out, v3b_wbtc_out, v3c_weth_out).
    """
    # Pool A: WETH→USDC
    v3a_zfo, v3a_usdc_out = _setup_v3(v3a, weth, usdc, AMOUNT_WETH, AMOUNT_USDC, owner)

    # Pool B: USDC→WBTC — use V3a's actual output
    v3b_zfo, v3b_wbtc_out = _setup_v3(v3b, usdc, wbtc, v3a_usdc_out, AMOUNT_WBTC, owner)

    # Pool C: WBTC→WETH — use V3b's actual output
    v3c_zfo, v3c_weth_out = _setup_v3(v3c, wbtc, weth, v3b_wbtc_out, AMOUNT_WETH_PROFIT, owner)

    return v3a_zfo, v3b_zfo, v3c_zfo, v3a_usdc_out, v3b_wbtc_out, v3c_weth_out


# ═══════════════════════════════════════════════════════════════════════════
# 1. V3→V3→V3 with nested forward_data (3 callbacks, fully explicit)
# ═══════════════════════════════════════════════════════════════════════════


class TestV3V3V3NestedCallbacks:
    """Three V3_SWAP_COMPACT calls with nested forward_data.

    Call stack (outermost -> innermost):
      V3a swap (executor receives USDC)
        callback: V3b swap (executor receives WBTC), pay WETH to V3a
                   callback: V3c swap (executor receives WETH), pay USDC to V3b
                              callback: pay WBTC to V3c

    ERC-20 transfers: 6    Callbacks: 3
    """

    def test_v3_v3_v3_nested_callbacks(
        self, weth, usdc, wbtc, owner_account, executor, v3a, v3b, v3c
    ):
        v3a_zfo, v3b_zfo, v3c_zfo, v3a_usdc_out, v3b_wbtc_out, v3c_weth_out = _setup_v3_pools(
            v3a, v3b, v3c, weth, usdc, wbtc, owner_account
        )
        at = AddressTable()
        idx = dict(
            weth=at.add(weth.address),
            usdc=at.add(usdc.address),
            wbtc=at.add(wbtc.address),
            executor=at.add(executor.address),
            v3a=at.add(v3a.address),
            v3b=at.add(v3b.address),
            v3c=at.add(v3c.address),
        )

        # Innermost: V3c callback — pay WBTC to V3c
        v3c_callback = enc_erc20_transfer(idx["wbtc"], idx["v3c"], v3b_wbtc_out)

        # V3b callback: V3c swap + pay USDC to V3b
        v3b_callback = enc_v3_swap_compact(
            idx["v3c"],
            v3c_zfo,
            v3b_wbtc_out,
            idx["executor"],
            forward_data=v3c_callback,
        )
        v3b_callback += enc_erc20_transfer(idx["usdc"], idx["v3b"], v3a_usdc_out)

        # V3a callback: V3b swap + pay WETH to V3a
        v3a_callback = enc_v3_swap_compact(
            idx["v3b"],
            v3b_zfo,
            v3a_usdc_out,
            idx["executor"],
            forward_data=v3b_callback,
        )
        v3a_callback += enc_erc20_transfer(idx["weth"], idx["v3a"], AMOUNT_WETH)

        # Top-level: V3a swap
        commands = enc_v3_swap_compact(
            idx["v3a"],
            v3a_zfo,
            AMOUNT_WETH,
            idx["executor"],
            forward_data=v3a_callback,
        )

        tx = executor.execute(
            enc_preamble(at) + commands, sender=owner_account, raise_on_revert=False
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")

        assert weth.balanceOf(executor.address) > 0


# ═══════════════════════════════════════════════════════════════════════════
# 2. V3->V3->V3 with innermost V3 auto-pay
# ═══════════════════════════════════════════════════════════════════════════


class TestV3V3V3InnerAutoPay:
    """V3->V3->V3 where V3c (innermost) uses auto-pay.

    V3c's callback auto-pays WBTC from executor's balance. The executor
    received WBTC from V3b's optimistic transfer just before V3c was called.

    ERC-20 transfers: 5    Callbacks: 3
    """

    def test_v3_v3_v3_inner_auto_pay(
        self, weth, usdc, wbtc, owner_account, executor, v3a, v3b, v3c
    ):
        v3a_zfo, v3b_zfo, v3c_zfo, v3a_usdc_out, v3b_wbtc_out, v3c_weth_out = _setup_v3_pools(
            v3a, v3b, v3c, weth, usdc, wbtc, owner_account
        )
        at = AddressTable()
        idx = dict(
            weth=at.add(weth.address),
            usdc=at.add(usdc.address),
            wbtc=at.add(wbtc.address),
            executor=at.add(executor.address),
            v3a=at.add(v3a.address),
            v3b=at.add(v3b.address),
            v3c=at.add(v3c.address),
        )

        # V3b callback: V3c swap (auto-pay) + pay USDC to V3b
        v3b_callback = enc_v3_swap_compact(
            idx["v3c"],
            v3c_zfo,
            v3b_wbtc_out,
            idx["executor"],
        )
        v3b_callback += enc_erc20_transfer(idx["usdc"], idx["v3b"], v3a_usdc_out)

        # V3a callback: V3b swap + pay WETH to V3a
        v3a_callback = enc_v3_swap_compact(
            idx["v3b"],
            v3b_zfo,
            v3a_usdc_out,
            idx["executor"],
            forward_data=v3b_callback,
        )
        v3a_callback += enc_erc20_transfer(idx["weth"], idx["v3a"], AMOUNT_WETH)

        # Top-level: V3a swap
        commands = enc_v3_swap_compact(
            idx["v3a"],
            v3a_zfo,
            AMOUNT_WETH,
            idx["executor"],
            forward_data=v3a_callback,
        )

        tx = executor.execute(
            enc_preamble(at) + commands, sender=owner_account, raise_on_revert=False
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")

        assert weth.balanceOf(executor.address) > 0


# ═══════════════════════════════════════════════════════════════════════════
# 3. V3->V3->V3 with middle V3 auto-pay
# ═══════════════════════════════════════════════════════════════════════════


class TestV3V3V3MiddleAutoPay:
    """V3->V3->V3 where V3b (middle) uses auto-pay, V3c uses forward_data.

    V3b's callback auto-pays USDC from executor's balance (executor received
    USDC from V3a's optimistic transfer). Then V3c is called inside V3a's
    callback as a separate command, using forward_data to pay WBTC.

    ERC-20 transfers: 5    Callbacks: 3
    """

    def test_v3_v3_v3_middle_auto_pay(
        self, weth, usdc, wbtc, owner_account, executor, v3a, v3b, v3c
    ):
        v3a_zfo, v3b_zfo, v3c_zfo, v3a_usdc_out, v3b_wbtc_out, v3c_weth_out = _setup_v3_pools(
            v3a, v3b, v3c, weth, usdc, wbtc, owner_account
        )
        at = AddressTable()
        idx = dict(
            weth=at.add(weth.address),
            usdc=at.add(usdc.address),
            wbtc=at.add(wbtc.address),
            executor=at.add(executor.address),
            v3a=at.add(v3a.address),
            v3b=at.add(v3b.address),
            v3c=at.add(v3c.address),
        )

        # V3c callback: pay WBTC to V3c
        v3c_callback = enc_erc20_transfer(idx["wbtc"], idx["v3c"], v3b_wbtc_out)

        # V3a callback: V3b swap (auto-pay), V3c swap (pay WBTC), pay WETH
        v3a_callback = enc_v3_swap_compact(
            idx["v3b"],
            v3b_zfo,
            v3a_usdc_out,
            idx["executor"],
        )
        v3a_callback += enc_v3_swap_compact(
            idx["v3c"],
            v3c_zfo,
            v3b_wbtc_out,
            idx["executor"],
            forward_data=v3c_callback,
        )
        v3a_callback += enc_erc20_transfer(idx["weth"], idx["v3a"], AMOUNT_WETH)

        # Top-level: V3a swap
        commands = enc_v3_swap_compact(
            idx["v3a"],
            v3a_zfo,
            AMOUNT_WETH,
            idx["executor"],
            forward_data=v3a_callback,
        )

        tx = executor.execute(
            enc_preamble(at) + commands, sender=owner_account, raise_on_revert=False
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")

        assert weth.balanceOf(executor.address) > 0


# ═══════════════════════════════════════════════════════════════════════════
# 4. V3->V3->V3 with double auto-pay (V3b and V3c both auto-pay)
# ═══════════════════════════════════════════════════════════════════════════


class TestV3V3V3DoubleAutoPay:
    """V3->V3->V3 where V3b and V3c both use auto-pay.

    Most gas-efficient V3->V3->V3 path: only V3a needs forward_data
    (to chain V3b, V3c, and pay WETH to V3a). Both V3b and V3c have
    empty forward_data, so the cmd_executor auto-detects and pays
    owed tokens from the executor's running balance.

    ERC-20 transfers: 4 (3 optimistic + 1 explicit pay-V3a)    Callbacks: 3
    """

    def test_v3_v3_v3_double_auto_pay(
        self, weth, usdc, wbtc, owner_account, executor, v3a, v3b, v3c
    ):
        v3a_zfo, v3b_zfo, v3c_zfo, v3a_usdc_out, v3b_wbtc_out, v3c_weth_out = _setup_v3_pools(
            v3a, v3b, v3c, weth, usdc, wbtc, owner_account
        )
        at = AddressTable()
        idx = dict(
            weth=at.add(weth.address),
            usdc=at.add(usdc.address),
            wbtc=at.add(wbtc.address),
            executor=at.add(executor.address),
            v3a=at.add(v3a.address),
            v3b=at.add(v3b.address),
            v3c=at.add(v3c.address),
        )

        # V3a callback: V3b swap (auto-pay), V3c swap (auto-pay), pay WETH
        v3a_callback = enc_v3_swap_compact(
            idx["v3b"],
            v3b_zfo,
            v3a_usdc_out,
            idx["executor"],
        )
        v3a_callback += enc_v3_swap_compact(
            idx["v3c"],
            v3c_zfo,
            v3b_wbtc_out,
            idx["executor"],
        )
        v3a_callback += enc_erc20_transfer(idx["weth"], idx["v3a"], AMOUNT_WETH)

        # Top-level: V3a swap
        commands = enc_v3_swap_compact(
            idx["v3a"],
            v3a_zfo,
            AMOUNT_WETH,
            idx["executor"],
            forward_data=v3a_callback,
        )

        tx = executor.execute(
            enc_preamble(at) + commands, sender=owner_account, raise_on_revert=False
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")

        assert weth.balanceOf(executor.address) > 0


# ═══════════════════════════════════════════════════════════════════════════
# 5. V3->V3->V3 reverse-order with direct custody (no intermediate custody)
# ═══════════════════════════════════════════════════════════════════════════


class TestV3V3V3ReverseDirectCustody:
    """V3->V3->V3 reverse order with direct custody — no intermediate executor custody.

    Instead of the forward-order approach (V3a->V3b->V3c) where every pool
    sends output to the executor (6 transfers), we execute in REVERSE order
    with each pool's output going directly to the pool that needs it as input.

    This exploits V3's balance-delta (IIA) check: the pool only verifies
    that its input-token balance increased by the owed amount — it doesn't
    care WHERE the tokens came from.

    Flow (reverse order, exact-input):

      1. V3c.swap(recipient=executor) — sends WETH to executor, callbacks
           V3c callback (forward_data):
      2.   V3b.swap(recipient=v3c) — sends WBTC directly to V3c, callbacks
               V3b callback (forward_data):
      3.     V3a.swap(recipient=v3b) — sends USDC directly to V3b, callbacks
                   V3a callback (auto-pay) — executor pays WETH to V3a

    As callbacks unwind, each pool's balance check passes because the
    inner pool sent tokens directly into it during the callback:

      3. V3a balance check: auto-pay delivered WETH ✓
      2. V3b balance check: V3a sent USDC to V3b directly ✓
      1. V3c balance check: V3b sent WBTC to V3c directly ✓

    Transfers: 4 (V3c→executor, V3b→V3c, V3a→V3b, executor→V3a)
    vs 6 in the forward nested-callback approach.

    The executor only ever holds WETH — no USDC or WBTC custody needed.
    """

    def test_v3_v3_v3_reverse_direct_custody(
        self, weth, usdc, wbtc, owner_account, executor, v3a, v3b, v3c
    ):
        v3a_zfo, v3b_zfo, v3c_zfo, v3a_usdc_out, v3b_wbtc_out, v3c_weth_out = _setup_v3_pools(
            v3a, v3b, v3c, weth, usdc, wbtc, owner_account
        )

        at = AddressTable()
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)
        v3a_idx = at.add(v3a.address)
        v3b_idx = at.add(v3b.address)
        v3c_idx = at.add(v3c.address)

        # V3a callback: auto-pay WETH to V3a (executor has WETH from V3c)
        v3a_callback = b""  # empty forward_data → auto-pay

        # V3b callback: V3a swap (recipient=v3b, auto-pay for V3a)
        v3b_callback = enc_v3_swap_compact(
            v3a_idx,
            v3a_zfo,
            AMOUNT_WETH,
            v3b_idx,
            forward_data=v3a_callback,
        )

        # V3c callback: V3b swap (recipient=v3c, forward_data chains V3a)
        v3c_callback = enc_v3_swap_compact(
            v3b_idx,
            v3b_zfo,
            v3a_usdc_out,
            v3c_idx,
            forward_data=v3b_callback,
        )

        # Top-level: V3c swap (recipient=executor, gets WETH first)
        commands = enc_v3_swap_compact(
            v3c_idx,
            v3c_zfo,
            v3b_wbtc_out,
            executor_idx,
            forward_data=v3c_callback,
        )

        tx = executor.execute(
            enc_preamble(at) + commands, sender=owner_account, raise_on_revert=False
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")

        # Executor should have WETH profit
        assert weth.balanceOf(executor.address) > 0


class TestV3V3V3ForwardDirectCustody:
    """V3a→V3b→V3c forward order with direct custody — expected to FAIL.

    In the reverse-order approach, V3b sends WBTC to V3c DURING V3c's
    callback, so V3c's IIA balance-delta check passes. In forward order,
    V3a sends USDC to V3b BEFORE V3b.swap() is called, so V3b's
    balance_before snapshot already includes the USDC. The IIA check
    then requires ADDITIONAL USDC to arrive during the callback — but
    nothing more arrives, so it fails.

    V3's IIA check: balance_before + amount_owed <= balance_after
    The input tokens must arrive DURING the callback (between the two
    balance reads), not before swap() is called. This is fundamentally
    different from V2's K-invariant check (which checks total balances).

    This test validates that forward-order direct custody does NOT work
    for V3, confirming that reverse-order is the unique strategy for
    eliminating intermediate executor custody on V3 three-hop paths.
    """

    def test_v3_v3_v3_forward_direct_custody_fails(
        self, weth, usdc, wbtc, owner_account, executor, v3a, v3b, v3c
    ):
        v3a_zfo, v3b_zfo, v3c_zfo, v3a_usdc_out, v3b_wbtc_out, v3c_weth_out = _setup_v3_pools(
            v3a, v3b, v3c, weth, usdc, wbtc, owner_account
        )

        at = AddressTable()
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)
        v3a_idx = at.add(v3a.address)
        v3b_idx = at.add(v3b.address)
        v3c_idx = at.add(v3c.address)

        # Forward order with direct custody:
        # V3a → V3b (USDC directly), V3b → V3c (WBTC directly), V3c → executor
        #
        # The problem: V3a sends USDC to V3b before V3b.swap() is called,
        # so V3b's balance_before already includes that USDC. The IIA check
        # then requires more USDC to arrive during the callback, which doesn't
        # happen.

        # V3a callback: V3b swap (recipient=v3c), then pay WETH to V3a
        v3a_callback = enc_v3_swap_compact(
            v3b_idx,
            v3b_zfo,
            v3a_usdc_out,
            v3c_idx,
        )
        # NOTE: no ERC20_TRANSFER of USDC to V3b — it should arrive via
        # V3a's optimistic transfer to V3b. But IIA doesn't see it.

        # Top-level: V3a swap (recipient=v3b, USDC goes directly)
        commands = enc_v3_swap_compact(
            v3a_idx,
            v3a_zfo,
            AMOUNT_WETH,
            v3b_idx,
            forward_data=v3a_callback,
        )

        tx = executor.execute(
            enc_preamble(at) + commands, sender=owner_account, raise_on_revert=False
        )
        # This should fail — V3b's IIA check fails because the USDC arrived
        # before balance_before was taken, not during the callback.
        assert tx.status == 0, (
            "Forward-order V3 direct custody should fail due to IIA timing: "
            "tokens must arrive DURING the callback, not before swap()"
        )


# ═══════════════════════════════════════════════════════════════════════════
# 6. V4->V3 (V3_SWAP_DELTA) -> V3 (COMPACT with auto-pay)
# ═══════════════════════════════════════════════════════════════════════════


class TestV4V3V3Delta:
    """V4->V3(V3_SWAP_DELTA)->V3(COMPACT with auto-pay).

    V3_SWAP_DELTA reads the amount from the PM exttload delta. After a V4
    swap creates a positive USDC delta, V4_TAKE_DELTA sends the USDC to the
    executor (consuming the delta). The executor now has USDC ERC-20 tokens
    AND the PM delta is zero. V3_SWAP_DELTA would read delta=0 and fail.

    Therefore, V3_SWAP_DELTA is used BEFORE V4_TAKE_DELTA in this test:
      1. V4 swap (WETH->USDC) creates USDC delta in PM exttload
      2. V3_SWAP_DELTA reads the USDC delta, calls V3b.swap(delta)
      3. V3b auto-pay callbacks to executor, which transfers USDC to V3b.
         But the executor doesn't have USDC yet (it's still in the PM).
         So this pattern requires the executor to already hold USDC.

    The only viable pattern for V3_SWAP_DELTA in a three-hop context is
    when USDC has been separately provided to the executor (e.g., via
    V4_TAKE_DELTA or WETH deposit/swap in a prior step). This test
    demonstrates it by doing:
      V4 swap (WETH->USDC) creates delta
      V4_TAKE_DELTA (USDC -> executor) gives executor USDC, delta -> 0
      V3_SWAP_COMPACT (not DELTA, since delta is 0)

    For the second V3 leg, we use V3_SWAP_COMPACT with auto-pay since
    the executor has WBTC from V3b's optimistic transfer.

    To demonstrate V3_SWAP_DELTA in a working context, we use a
    two-swap V4 pattern: first V4 swap creates a delta, V3_SWAP_DELTA
    reads it, then a second V4 swap creates another delta for V4 take.

    V3_SWAP_DELTA is designed to avoid encoding the amount. Inside the V4
    unlock callback, after a V4 swap establishes a PM delta, V3_SWAP_DELTA
    reads it WITHOUT calling V4_TAKE first. The V3 swap then runs (the
    callback auto-pays from executor balance), but the executor doesn't
    have the USDC yet.

    In the real Uniswap V4 architecture, PM.take() is called inside the
    unlock callback, which transfers tokens from the PM to the executor.
    This happens synchronously before V3_SWAP_DELTA reads the delta. But
    after PM.take(), the delta is updated (zeroed).

    V3_SWAP_DELTA therefore works ONLY when the V3 swap callback payment
    happens through the PM (settling the delta), not through the executor's
    ERC-20 balance. This requires V4_SETTLE_DELTA inside the V3 callback,
    which our current test setup doesn't support because V3 auto-pay uses
    ERC-20 transfers, not PM settlement.

    Conclusion: V3_SWAP_DELTA requires the executor to already hold the
    input token in its ERC-20 balance (from a prior independent source)
    AND there must be a PM delta for the same currency. This is a niche
    pattern, but testable by pre-funding the executor.
    """

    def test_v4_v3_v3_with_take_then_compact(
        self, usdc, weth, wbtc, owner_account, executor, v4_pm, v3b, v3c
    ):
        """V4->V3->V3 using V4_TAKE + V3_SWAP_COMPACT (not DELTA).

        This validates the three-hop V4->V3->V3 flow works correctly
        before attempting V3_SWAP_DELTA.
        """
        # V4 setup
        pool_key = _make_pool_key(weth.address, usdc.address, fee=3000, tick_spacing=60)
        v4_zfo = pool_key[0] == weth.address
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_key,
            AMOUNT_WETH,
            AMOUNT_USDC,
            v4_zfo,
            output_token=usdc,
        )

        # V3b setup
        v3b_zfo, v3b_wbtc_out = _setup_v3(v3b, usdc, wbtc, AMOUNT_USDC, AMOUNT_WBTC, owner_account)

        # V3c setup
        v3c_zfo, _ = _setup_v3(v3c, wbtc, weth, v3b_wbtc_out, AMOUNT_WETH_PROFIT, owner_account)

        at = AddressTable()
        idx = dict(
            weth=at.add(weth.address),
            usdc=at.add(usdc.address),
            wbtc=at.add(wbtc.address),
            executor=at.add(executor.address),
            v3b=at.add(v3b.address),
            v3c=at.add(v3c.address),
            pm=at.add(v4_pm.address),
            zero=at.add(ZERO_ADDRESS),
        )

        # V3b callback: V3c swap (auto-pay) + pay USDC to V3b
        v3b_callback = enc_v3_swap_compact(
            idx["v3c"],
            v3c_zfo,
            v3b_wbtc_out,
            idx["executor"],
        )
        v3b_callback += enc_erc20_transfer(idx["usdc"], idx["v3b"], AMOUNT_USDC)

        inner = enc_v4_swap_compact(
            idx["weth"] if pool_key[0] == weth.address else idx["usdc"],
            idx["usdc"] if pool_key[1] == usdc.address else idx["weth"],
            pool_key[2],
            pool_key[3],
            idx["zero"],
            v4_zfo,
            AMOUNT_WETH,
        )
        inner += enc_v4_take(idx["usdc"], idx["executor"], AMOUNT_USDC)
        inner += enc_v3_swap_compact(
            idx["v3b"],
            v3b_zfo,
            AMOUNT_USDC,
            idx["executor"],
            forward_data=v3b_callback,
        )
        inner += enc_v4_settle_delta(idx["weth"])

        tx = executor.execute(
            enc_preamble(at) + enc_v4_unlock(inner),
            sender=owner_account,
            raise_on_revert=False,
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")

        assert weth.balanceOf(executor.address) > 0

    def test_v4_v3_v3_delta_pre_funded(
        self, usdc, weth, wbtc, owner_account, executor, v4_pm, v3b, v3c
    ):
        """V4->V3(SWAP_DELTA)->V3 with pre-funded executor.

        V3_SWAP_DELTA reads the PM exttload delta as the V3 swap amount.
        For this to work, the executor must already hold the input token
        (USDC) in its ERC-20 balance so that auto-pay can pay the V3 pool.

        Setup: V4 swap creates a USDC delta. Before calling V3_SWAP_DELTA,
        we pre-fund the executor with USDC (via mint). V3_SWAP_DELTA reads
        the delta and calls V3b.swap with that amount. V3b's auto-pay
        callback transfers USDC from executor to V3b (succeeds because
        executor was pre-funded).

        After V3b swap: executor has WBTC. V3c swap uses auto-pay.
        After V3c swap: executor has WETH. V4_SETTLE_DELTA settles.

        Finally: V4_TAKE_DELTA removes the stale USDC delta.
        """
        # V4 setup
        pool_key = _make_pool_key(weth.address, usdc.address, fee=3000, tick_spacing=60)
        v4_zfo = pool_key[0] == weth.address
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_key,
            AMOUNT_WETH,
            AMOUNT_USDC,
            v4_zfo,
            output_token=usdc,
        )

        # V3b setup
        v3b_zfo, v3b_wbtc_out = _setup_v3(v3b, usdc, wbtc, AMOUNT_USDC, AMOUNT_WBTC, owner_account)

        # V3c setup
        v3c_zfo, _ = _setup_v3(v3c, wbtc, weth, v3b_wbtc_out, AMOUNT_WETH_PROFIT, owner_account)

        # Pre-fund executor with USDC for V3b auto-pay
        usdc.mint(executor.address, AMOUNT_USDC, sender=owner_account)

        at = AddressTable()
        idx = dict(
            weth=at.add(weth.address),
            usdc=at.add(usdc.address),
            wbtc=at.add(wbtc.address),
            executor=at.add(executor.address),
            v3b=at.add(v3b.address),
            v3c=at.add(v3c.address),
            pm=at.add(v4_pm.address),
            zero=at.add(ZERO_ADDRESS),
        )

        inner = enc_v4_swap_compact(
            idx["weth"] if pool_key[0] == weth.address else idx["usdc"],
            idx["usdc"] if pool_key[1] == usdc.address else idx["weth"],
            pool_key[2],
            pool_key[3],
            idx["zero"],
            v4_zfo,
            AMOUNT_WETH,
        )
        # V3_SWAP_DELTA: reads USDC delta from PM, swaps at V3b (auto-pay)
        inner += enc_v3_swap_delta(idx["v3b"], v3b_zfo, idx["executor"])
        # V3c swap (auto-pay): executor has WBTC from V3b optimistic transfer
        inner += enc_v3_swap_compact(idx["v3c"], v3c_zfo, v3b_wbtc_out, idx["executor"])
        # V4_TAKE_DELTA removes the now-stale USDC delta
        inner += enc_v4_take_delta(idx["usdc"], idx["executor"])
        inner += enc_v4_settle_delta(idx["weth"])

        tx = executor.execute(
            enc_preamble(at) + enc_v4_unlock(inner),
            sender=owner_account,
            raise_on_revert=False,
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")

        assert weth.balanceOf(executor.address) > 0


# ═══════════════════════════════════════════════════════════════════════════
# 6. V3->V3->V4 (two V3 legs then V4)
# ═══════════════════════════════════════════════════════════════════════════


class TestV3V3V4:
    """V3->V3->V4: WETH->USDC at V3a, USDC->WBTC at V3b, WBTC->WETH at V4.

    Flow:
      V3a swap (sends USDC to executor, callback)
        callback: V3b swap (sends WBTC to executor, callback) + pay USDC to V3a
                   callback: V4 unlock + pay USDC to V3b
                              inside unlock: sync WBTC, transfer, settle, swap, take WETH
    """

    def test_v3_v3_v4(self, usdc, weth, wbtc, owner_account, executor, v4_pm, v3a, v3b):
        # V3a setup: WETH->USDC
        v3a_zfo, v3a_usdc_out = _setup_v3(v3a, weth, usdc, AMOUNT_WETH, AMOUNT_USDC, owner_account)

        # V3b setup: USDC->WBTC
        # Use V3a's actual USDC output as V3b's input
        v3b_zfo, v3b_wbtc_out = _setup_v3(v3b, usdc, wbtc, v3a_usdc_out, AMOUNT_WBTC, owner_account)

        # V4 setup: WBTC->WETH
        pool_key = _make_pool_key(wbtc.address, weth.address, fee=3000, tick_spacing=60)
        v4_zfo = pool_key[0] == wbtc.address
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_key,
            v3b_wbtc_out,  # WBTC amount matches V3b's actual output
            AMOUNT_WETH_PROFIT,
            v4_zfo,
            output_token=weth,
        )

        at = AddressTable()
        idx = dict(
            weth=at.add(weth.address),
            usdc=at.add(usdc.address),
            wbtc=at.add(wbtc.address),
            executor=at.add(executor.address),
            v3a=at.add(v3a.address),
            v3b=at.add(v3b.address),
            pm=at.add(v4_pm.address),
            zero=at.add(ZERO_ADDRESS),
        )

        # V4 unlock inner: sync WBTC, transfer WBTC to PM, settle, V4 swap, take WETH
        v4_inner = enc_v4_sync(idx["wbtc"])
        v4_inner += enc_erc20_transfer(idx["wbtc"], idx["pm"], v3b_wbtc_out)
        v4_inner += enc_v4_settle()
        v4_inner += enc_v4_swap_compact(
            idx["wbtc"] if pool_key[0] == wbtc.address else idx["weth"],
            idx["weth"] if pool_key[1] == weth.address else idx["wbtc"],
            pool_key[2],
            pool_key[3],
            idx["zero"],
            v4_zfo,
            v3b_wbtc_out,  # WBTC amount matches V3b's actual output
        )
        v4_inner += enc_v4_take(idx["weth"], idx["executor"], AMOUNT_WETH_PROFIT)

        # V3b callback: V4 unlock + pay USDC to V3b
        v3b_callback = enc_v4_unlock(v4_inner)
        v3b_callback += enc_erc20_transfer(idx["usdc"], idx["v3b"], v3a_usdc_out)

        # V3a callback: V3b swap + pay WETH to V3a
        v3a_callback = enc_v3_swap_compact(
            idx["v3b"],
            v3b_zfo,
            v3a_usdc_out,  # USDC amount matches V3a's actual output
            idx["executor"],
            forward_data=v3b_callback,
        )
        v3a_callback += enc_erc20_transfer(idx["weth"], idx["v3a"], AMOUNT_WETH)

        # Top-level: V3a swap
        commands = enc_v3_swap_compact(
            idx["v3a"],
            v3a_zfo,
            AMOUNT_WETH,
            idx["executor"],
            forward_data=v3a_callback,
        )

        tx = executor.execute(
            enc_preamble(at) + commands, sender=owner_account, raise_on_revert=False
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")

        assert weth.balanceOf(executor.address) > 0


# ═══════════════════════════════════════════════════════════════════════════
# 7. V3->V3->V3 with PancakeSwap V3 callback variant
# ═══════════════════════════════════════════════════════════════════════════


class TestPancakeV3Callback:
    """V3->V3->V3 with V3c using PancakeSwap callback (pancakeV3SwapCallback).

    The cmd_executor handles both uniswapV3SwapCallback and
    pancakeV3SwapCallback identically (auto-pay or process forward_data).
    This test validates the PancakeSwap variant works in a three-hop
    context with the same auto-pay mechanism.
    """

    def test_v3_v3_v3_pancake_callback(
        self, weth, usdc, wbtc, owner_account, executor, v3a, v3b, v3c_pancake
    ):
        v3a_zfo, v3b_zfo, v3c_zfo, v3a_usdc_out, v3b_wbtc_out, v3c_weth_out = _setup_v3_pools(
            v3a, v3b, v3c_pancake, weth, usdc, wbtc, owner_account
        )
        at = AddressTable()
        idx = dict(
            weth=at.add(weth.address),
            usdc=at.add(usdc.address),
            wbtc=at.add(wbtc.address),
            executor=at.add(executor.address),
            v3a=at.add(v3a.address),
            v3b=at.add(v3b.address),
            v3c=at.add(v3c_pancake.address),
        )

        # V3a callback: V3b swap (auto-pay), V3c swap (auto-pay), pay WETH
        v3a_callback = enc_v3_swap_compact(
            idx["v3b"],
            v3b_zfo,
            v3a_usdc_out,
            idx["executor"],
        )
        v3a_callback += enc_v3_swap_compact(
            idx["v3c"],
            v3c_zfo,
            v3b_wbtc_out,
            idx["executor"],
        )
        v3a_callback += enc_erc20_transfer(idx["weth"], idx["v3a"], AMOUNT_WETH)

        commands = enc_v3_swap_compact(
            idx["v3a"],
            v3a_zfo,
            AMOUNT_WETH,
            idx["executor"],
            forward_data=v3a_callback,
        )

        tx = executor.execute(
            enc_preamble(at) + commands, sender=owner_account, raise_on_revert=False
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")

        assert weth.balanceOf(executor.address) > 0


# ═══════════════════════════════════════════════════════════════════════════
# 8. Gas comparison
# ═══════════════════════════════════════════════════════════════════════════


class TestGasComparison:
    """Print gas usage for different V3 three-hop approaches."""

    def test_nested_callbacks_gas(
        self, weth, usdc, wbtc, owner_account, executor, v3a, v3b, v3c
    ):
        v3a_zfo, v3b_zfo, v3c_zfo, v3a_usdc_out, v3b_wbtc_out, v3c_weth_out = _setup_v3_pools(
            v3a, v3b, v3c, weth, usdc, wbtc, owner_account
        )
        at = AddressTable()
        idx = dict(
            weth=at.add(weth.address),
            usdc=at.add(usdc.address),
            wbtc=at.add(wbtc.address),
            executor=at.add(executor.address),
            v3a=at.add(v3a.address),
            v3b=at.add(v3b.address),
            v3c=at.add(v3c.address),
        )

        v3c_callback = enc_erc20_transfer(idx["wbtc"], idx["v3c"], v3b_wbtc_out)
        v3b_callback = enc_v3_swap_compact(
            idx["v3c"],
            v3c_zfo,
            v3b_wbtc_out,
            idx["executor"],
            forward_data=v3c_callback,
        )
        v3b_callback += enc_erc20_transfer(idx["usdc"], idx["v3b"], v3a_usdc_out)
        v3a_callback = enc_v3_swap_compact(
            idx["v3b"],
            v3b_zfo,
            v3a_usdc_out,
            idx["executor"],
            forward_data=v3b_callback,
        )
        v3a_callback += enc_erc20_transfer(idx["weth"], idx["v3a"], AMOUNT_WETH)
        commands = enc_v3_swap_compact(
            idx["v3a"],
            v3a_zfo,
            AMOUNT_WETH,
            idx["executor"],
            forward_data=v3a_callback,
        )

        tx = executor.execute(enc_preamble(at) + commands, sender=owner_account)
        assert tx.status == 1
        print(f"\n    V3 nested callbacks (3 COMPACT):  {tx.gas_used:>8,} gas")

    def test_double_auto_pay_gas(
        self, weth, usdc, wbtc, owner_account, executor, v3a, v3b, v3c
    ):
        v3a_zfo, v3b_zfo, v3c_zfo, v3a_usdc_out, v3b_wbtc_out, v3c_weth_out = _setup_v3_pools(
            v3a, v3b, v3c, weth, usdc, wbtc, owner_account
        )
        at = AddressTable()
        idx = dict(
            weth=at.add(weth.address),
            usdc=at.add(usdc.address),
            wbtc=at.add(wbtc.address),
            executor=at.add(executor.address),
            v3a=at.add(v3a.address),
            v3b=at.add(v3b.address),
            v3c=at.add(v3c.address),
        )

        v3a_callback = enc_v3_swap_compact(
            idx["v3b"],
            v3b_zfo,
            v3a_usdc_out,
            idx["executor"],
        )
        v3a_callback += enc_v3_swap_compact(
            idx["v3c"],
            v3c_zfo,
            v3b_wbtc_out,
            idx["executor"],
        )
        v3a_callback += enc_erc20_transfer(idx["weth"], idx["v3a"], AMOUNT_WETH)
        commands = enc_v3_swap_compact(
            idx["v3a"],
            v3a_zfo,
            AMOUNT_WETH,
            idx["executor"],
            forward_data=v3a_callback,
        )

        tx = executor.execute(enc_preamble(at) + commands, sender=owner_account)
        assert tx.status == 1
        print(f"    V3 double auto-pay:              {tx.gas_used:>8,} gas")

    def test_v4_v3_v3_gas(
        self, usdc, weth, wbtc, owner_account, executor, v4_pm, v3b, v3c
    ):
        pool_key = _make_pool_key(weth.address, usdc.address, fee=3000, tick_spacing=60)
        v4_zfo = pool_key[0] == weth.address
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_key,
            AMOUNT_WETH,
            AMOUNT_USDC,
            v4_zfo,
            output_token=usdc,
        )

        v3b_zfo, v3b_wbtc_out = _setup_v3(v3b, usdc, wbtc, AMOUNT_USDC, AMOUNT_WBTC, owner_account)

        v3c_zfo, _ = _setup_v3(v3c, wbtc, weth, v3b_wbtc_out, AMOUNT_WETH_PROFIT, owner_account)

        at = AddressTable()
        idx = dict(
            weth=at.add(weth.address),
            usdc=at.add(usdc.address),
            wbtc=at.add(wbtc.address),
            executor=at.add(executor.address),
            v3b=at.add(v3b.address),
            v3c=at.add(v3c.address),
            pm=at.add(v4_pm.address),
            zero=at.add(ZERO_ADDRESS),
        )

        # V3b auto-pay: executor pays USDC to V3b after AMOUNT_USDC is taken from PM
        inner = enc_v4_swap_compact(
            idx["weth"] if pool_key[0] == weth.address else idx["usdc"],
            idx["usdc"] if pool_key[1] == usdc.address else idx["weth"],
            pool_key[2],
            pool_key[3],
            idx["zero"],
            v4_zfo,
            AMOUNT_WETH,
        )
        inner += enc_v4_take(idx["usdc"], idx["executor"], AMOUNT_USDC)
        inner += enc_v3_swap_compact(idx["v3b"], v3b_zfo, AMOUNT_USDC, idx["executor"])
        inner += enc_v3_swap_compact(idx["v3c"], v3c_zfo, v3b_wbtc_out, idx["executor"])
        inner += enc_v4_settle_delta(idx["weth"])

        tx = executor.execute(
            enc_preamble(at) + enc_v4_unlock(inner), sender=owner_account
        )
        assert tx.status == 1
        print(f"    V4->V3->V3 (TAKE + 2 COMPACT):   {tx.gas_used:>8,} gas")

    def test_v3_v3_v4_gas(
        self, usdc, weth, wbtc, owner_account, executor, v4_pm, v3a, v3b
    ):
        v3a_zfo, v3a_usdc_out = _setup_v3(v3a, weth, usdc, AMOUNT_WETH, AMOUNT_USDC, owner_account)

        v3b_zfo, v3b_wbtc_out = _setup_v3(v3b, usdc, wbtc, v3a_usdc_out, AMOUNT_WBTC, owner_account)

        pool_key = _make_pool_key(wbtc.address, weth.address, fee=3000, tick_spacing=60)
        v4_zfo = pool_key[0] == wbtc.address
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_key,
            v3b_wbtc_out,
            AMOUNT_WETH_PROFIT,
            v4_zfo,
            output_token=weth,
        )

        at = AddressTable()
        idx = dict(
            weth=at.add(weth.address),
            usdc=at.add(usdc.address),
            wbtc=at.add(wbtc.address),
            executor=at.add(executor.address),
            v3a=at.add(v3a.address),
            v3b=at.add(v3b.address),
            pm=at.add(v4_pm.address),
            zero=at.add(ZERO_ADDRESS),
        )

        v4_inner = enc_v4_sync(idx["wbtc"])
        v4_inner += enc_erc20_transfer(idx["wbtc"], idx["pm"], v3b_wbtc_out)
        v4_inner += enc_v4_settle()
        v4_inner += enc_v4_swap_compact(
            idx["wbtc"] if pool_key[0] == wbtc.address else idx["weth"],
            idx["weth"] if pool_key[1] == weth.address else idx["wbtc"],
            pool_key[2],
            pool_key[3],
            idx["zero"],
            v4_zfo,
            v3b_wbtc_out,
        )
        v4_inner += enc_v4_take(idx["weth"], idx["executor"], AMOUNT_WETH_PROFIT)

        v3b_callback = enc_v4_unlock(v4_inner)
        v3b_callback += enc_erc20_transfer(idx["usdc"], idx["v3b"], v3a_usdc_out)

        v3a_callback = enc_v3_swap_compact(
            idx["v3b"],
            v3b_zfo,
            v3a_usdc_out,
            idx["executor"],
            forward_data=v3b_callback,
        )
        v3a_callback += enc_erc20_transfer(idx["weth"], idx["v3a"], AMOUNT_WETH)

        commands = enc_v3_swap_compact(
            idx["v3a"],
            v3a_zfo,
            AMOUNT_WETH,
            idx["executor"],
            forward_data=v3a_callback,
        )

        tx = executor.execute(enc_preamble(at) + commands, sender=owner_account)
        assert tx.status == 1
        print(f"    V3->V3->V4 (2 COMPACT + unlock): {tx.gas_used:>8,} gas")
