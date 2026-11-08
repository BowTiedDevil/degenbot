"""
Tests for cmd_executor V4-V4 swap execution.
"""

import pytest

from .conftest_shared import (
    NATIVE_ADDRESS,
    ZERO_ADDRESS,
    enc_v4_swap_compact,
    enc_v4_take,
    enc_v4_unlock,
    enc_v4_settle_delta,
    enc_v4_batch,
    _make_pool_key,
    _setup_v4_swap,
    AddressTable,
    enc_preamble,
)


@pytest.fixture
def v4_pm(project, owner_account, wbtc, weth):
    token0, token1 = sorted([wbtc.address, weth.address], key=lambda addr: addr.lower())
    return project.fake_uniswap_v4_pool_manager.deploy(
        token0, token1, sender=owner_account
    )


class TestV4V4SameCurrency:
    def test_v4_v4_all_weth_pairs(self, wbtc, weth, owner_account, executor, v4_pm):
        """WETH→WBTC at Pool A, then WBTC→WETH at Pool B (profitable)."""
        pool_a_key = _make_pool_key(weth.address, wbtc.address, fee=0, tick_spacing=60)
        pool_a_amount_in = 10 * 10**18
        pool_a_amount_out = 1 * 10**8
        pool_a_zfo = pool_a_key[0] == weth.address
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_a_key,
            pool_a_amount_in,
            pool_a_amount_out,
            pool_a_zfo,
            output_token=wbtc,
        )

        pool_b_key = _make_pool_key(weth.address, wbtc.address, fee=0, tick_spacing=120)
        pool_b_amount_in = pool_a_amount_out
        pool_b_amount_out = 2 * pool_a_amount_in
        pool_b_zfo = pool_b_key[0] == wbtc.address
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_b_key,
            pool_b_amount_in,
            pool_b_amount_out,
            pool_b_zfo,
            output_token=weth,
        )

        at = AddressTable()
        pm_idx = at.add(v4_pm.address)
        weth_idx = at.add(weth.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)
        zero_idx = at.add(ZERO_ADDRESS)

        inner = b""
        inner += enc_v4_swap_compact(
            weth_idx if pool_a_key[0] == weth.address else wbtc_idx,
            wbtc_idx if pool_a_key[1] == wbtc.address else weth_idx,
            pool_a_key[2],
            pool_a_key[3],
            zero_idx,
            pool_a_zfo,
            pool_a_amount_in,
        )
        inner += enc_v4_swap_compact(
            weth_idx if pool_b_key[0] == weth.address else wbtc_idx,
            wbtc_idx if pool_b_key[1] == wbtc.address else weth_idx,
            pool_b_key[2],
            pool_b_key[3],
            zero_idx,
            pool_b_zfo,
            pool_b_amount_in,
        )
        profit_weth = pool_b_amount_out - pool_a_amount_in
        inner += enc_v4_take(weth_idx, executor_idx, profit_weth)

        commands = enc_v4_unlock(inner)
        tx = executor.execute(
            enc_preamble(at) + commands, sender=owner_account, raise_on_revert=False
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")


class TestV4Batch:
    """V4_BATCH: multi-swap + auto-settle in a tight loop."""

    def test_v4_batch_same_currency(self, wbtc, weth, owner_account, executor, v4_pm):
        """V4_BATCH: WETH→WBTC at Pool A, WBTC→WETH at Pool B (profitable)."""
        pool_a_key = _make_pool_key(weth.address, wbtc.address, fee=0, tick_spacing=60)
        pool_a_amount_in = 10 * 10**18
        pool_a_amount_out = 1 * 10**8
        pool_a_zfo = pool_a_key[0] == weth.address
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_a_key,
            pool_a_amount_in,
            pool_a_amount_out,
            pool_a_zfo,
            output_token=wbtc,
        )

        pool_b_key = _make_pool_key(weth.address, wbtc.address, fee=0, tick_spacing=120)
        pool_b_amount_in = pool_a_amount_out
        pool_b_amount_out = 2 * pool_a_amount_in
        pool_b_zfo = pool_b_key[0] == wbtc.address
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_b_key,
            pool_b_amount_in,
            pool_b_amount_out,
            pool_b_zfo,
            output_token=weth,
        )

        at = AddressTable()
        pm_idx = at.add(v4_pm.address)
        weth_idx = at.add(weth.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)
        zero_idx = at.add(ZERO_ADDRESS)

        # V4_BATCH: swap 1 explicit, swap 2 dynamic (amount=0 → read from PM delta)
        batch = enc_v4_batch(
            [
                # Swap 1: WETH→WBTC (explicit amount)
                (
                    weth_idx if pool_a_key[0] == weth.address else wbtc_idx,
                    wbtc_idx if pool_a_key[1] == wbtc.address else weth_idx,
                    pool_a_key[2],
                    pool_a_key[3],
                    zero_idx,
                    pool_a_zfo,
                    pool_a_amount_in,
                ),
                # Swap 2: WBTC→WETH (dynamic — amount derived from swap 1 delta)
                (
                    weth_idx if pool_b_key[0] == weth.address else wbtc_idx,
                    wbtc_idx if pool_b_key[1] == wbtc.address else weth_idx,
                    pool_b_key[2],
                    pool_b_key[3],
                    zero_idx,
                    pool_b_zfo,
                    0,  # dynamic amount
                ),
            ]
        )

        # V4_BATCH auto-settles, so no separate V4_TAKE / V4_SETTLE needed
        commands = enc_v4_unlock(batch)
        tx = executor.execute(
            enc_preamble(at) + commands, sender=owner_account, raise_on_revert=False
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")

    def test_v4_batch_cross_currency(self, wbtc, weth, owner_account, executor, v4_pm):
        """V4_BATCH: WETH→WBTC at Pool A, WBTC→ETH native at Pool B."""
        pool_a_key = _make_pool_key(
            weth.address, wbtc.address, fee=3000, tick_spacing=60
        )
        pool_a_amount_in = 10 * 10**18
        pool_a_amount_out = 1 * 10**8
        pool_a_zfo = pool_a_key[0] == weth.address
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_a_key,
            pool_a_amount_in,
            pool_a_amount_out,
            pool_a_zfo,
            output_token=wbtc,
        )

        pool_b_key = _make_pool_key(
            NATIVE_ADDRESS, wbtc.address, fee=500, tick_spacing=10
        )
        pool_b_amount_in = pool_a_amount_out
        pool_b_amount_out = 20 * 10**18
        pool_b_zfo = pool_b_key[0] == wbtc.address
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_b_key,
            pool_b_amount_in,
            pool_b_amount_out,
            pool_b_zfo,
            fund_eth=True,
        )

        at = AddressTable()
        pm_idx = at.add(v4_pm.address)
        weth_idx = at.add(weth.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)
        zero_idx = at.add(ZERO_ADDRESS)
        native_idx = at.add(NATIVE_ADDRESS)

        batch = enc_v4_batch(
            [
                # Swap 1: WETH→WBTC (explicit)
                (
                    weth_idx if pool_a_key[0] == weth.address else wbtc_idx,
                    wbtc_idx if pool_a_key[1] == wbtc.address else weth_idx,
                    pool_a_key[2],
                    pool_a_key[3],
                    zero_idx,
                    pool_a_zfo,
                    pool_a_amount_in,
                ),
                # Swap 2: WBTC→ETH native (dynamic)
                (
                    native_idx if pool_b_key[0] == NATIVE_ADDRESS else wbtc_idx,
                    wbtc_idx if pool_b_key[1] == wbtc.address else native_idx,
                    pool_b_key[2],
                    pool_b_key[3],
                    zero_idx,
                    pool_b_zfo,
                    0,  # dynamic
                ),
            ]
        )

        commands = enc_v4_unlock(batch)
        tx = executor.execute(
            enc_preamble(at) + commands, sender=owner_account, raise_on_revert=False
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")

    def test_v4_batch_usdc_intermediate(
        self, usdc, weth, owner_account, executor, v4_pm
    ):
        """V4_BATCH: WETH→USDC at Pool A, USDC→ETH at Pool B."""
        pool_a_key = _make_pool_key(
            weth.address, usdc.address, fee=3000, tick_spacing=60
        )
        pool_a_amount_in = 1 * 10**18
        pool_a_amount_out = 2000 * 10**6
        pool_a_zfo = pool_a_key[0] == weth.address
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_a_key,
            pool_a_amount_in,
            pool_a_amount_out,
            pool_a_zfo,
            output_token=usdc,
        )

        pool_b_key = _make_pool_key(
            NATIVE_ADDRESS, usdc.address, fee=500, tick_spacing=10
        )
        pool_b_amount_in = pool_a_amount_out
        pool_b_amount_out = 2 * 10**18
        pool_b_zfo = pool_b_key[0] == usdc.address
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_b_key,
            pool_b_amount_in,
            pool_b_amount_out,
            pool_b_zfo,
            fund_eth=True,
        )

        at = AddressTable()
        pm_idx = at.add(v4_pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        zero_idx = at.add(ZERO_ADDRESS)
        native_idx = at.add(NATIVE_ADDRESS)

        batch = enc_v4_batch(
            [
                # Swap 1: WETH→USDC (explicit)
                (
                    weth_idx if pool_a_key[0] == weth.address else usdc_idx,
                    usdc_idx if pool_a_key[1] == usdc.address else weth_idx,
                    pool_a_key[2],
                    pool_a_key[3],
                    zero_idx,
                    pool_a_zfo,
                    pool_a_amount_in,
                ),
                # Swap 2: USDC→ETH (dynamic)
                (
                    native_idx if pool_b_key[0] == NATIVE_ADDRESS else usdc_idx,
                    usdc_idx if pool_b_key[1] == usdc.address else native_idx,
                    pool_b_key[2],
                    pool_b_key[3],
                    zero_idx,
                    pool_b_zfo,
                    0,  # dynamic
                ),
            ]
        )

        commands = enc_v4_unlock(batch)
        tx = executor.execute(
            enc_preamble(at) + commands, sender=owner_account, raise_on_revert=False
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")


class TestV4V4DifferentCurrency:
    def test_v4_v4_weth_wbtc_eth(self, wbtc, weth, owner_account, executor, v4_pm):
        """WETH→WBTC at Pool A, then WBTC→ETH at Pool B."""
        weth.mint(executor.address, 10 * 10**18, sender=owner_account)

        pool_a_key = _make_pool_key(
            weth.address, wbtc.address, fee=3000, tick_spacing=60
        )
        pool_a_amount_in = 10 * 10**18
        pool_a_amount_out = 1 * 10**8
        pool_a_zfo = pool_a_key[0] == weth.address
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_a_key,
            pool_a_amount_in,
            pool_a_amount_out,
            pool_a_zfo,
            output_token=wbtc,
        )

        pool_b_key = _make_pool_key(
            NATIVE_ADDRESS, wbtc.address, fee=500, tick_spacing=10
        )
        pool_b_amount_in = pool_a_amount_out
        pool_b_amount_out = 20 * 10**18
        pool_b_zfo = pool_b_key[0] == wbtc.address
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_b_key,
            pool_b_amount_in,
            pool_b_amount_out,
            pool_b_zfo,
            fund_eth=True,
        )

        at = AddressTable()
        pm_idx = at.add(v4_pm.address)
        weth_idx = at.add(weth.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)
        zero_idx = at.add(ZERO_ADDRESS)
        native_idx = at.add(NATIVE_ADDRESS)

        inner = b""
        inner += enc_v4_swap_compact(
            weth_idx if pool_a_key[0] == weth.address else wbtc_idx,
            wbtc_idx if pool_a_key[1] == wbtc.address else weth_idx,
            pool_a_key[2],
            pool_a_key[3],
            zero_idx,
            pool_a_zfo,
            pool_a_amount_in,
        )
        inner += enc_v4_swap_compact(
            native_idx if pool_b_key[0] == NATIVE_ADDRESS else wbtc_idx,
            wbtc_idx if pool_b_key[1] == wbtc.address else native_idx,
            pool_b_key[2],
            pool_b_key[3],
            zero_idx,
            pool_b_zfo,
            pool_b_amount_in,
        )
        inner += enc_v4_take(native_idx, executor_idx, pool_b_amount_out)
        inner += enc_v4_settle_delta(weth_idx)

        commands = enc_v4_unlock(inner)
        tx = executor.execute(
            enc_preamble(at) + commands, sender=owner_account, raise_on_revert=False
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")


class TestV4V4V4ThreeHop:
    """V4-V4-V4 three-hop triangular arbitrage within a single PoolManager.

    Path: WETH→USDC (V4a) → USDC→WBTC (V4b) → WBTC→WETH (V3c).

    All swaps happen inside a single unlock() callback — no nested
    callbacks or intermediate custody. Deltas accumulate in the PM's
    transient storage and net out: USDC and WBTC cancel completely,
    leaving only a net WETH profit. This is the simplest three-hop
    because V4's delta accounting eliminates all intermediate transfers.
    """

    @pytest.fixture
    def v4_pm(self, project, owner_account, weth, usdc, wbtc):
        """Single PM that holds all three currencies."""
        # token0/token1 are just for constructor — PM works with any currencies
        token0, token1 = sorted(
            [weth.address, usdc.address], key=lambda addr: addr.lower()
        )
        pm = project.fake_uniswap_v4_pool_manager.deploy(
            token0, token1, sender=owner_account
        )
        return pm

    def test_v4_v4_v4_three_hop(self, weth, usdc, wbtc, owner_account, executor, v4_pm):
        """WETH→USDC→WBTC→WETH triangular arbitrage via three V4 pools."""
        AMOUNT_WETH = 10 * 10**18
        AMOUNT_USDC = 20000 * 10**6
        AMOUNT_WBTC = 100 * 10**8
        AMOUNT_WETH_PROFIT = 2 * 10**18  # more WETH out than in

        # V4a: WETH→USDC
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

        # V4b: USDC→WBTC
        pool_b_key = _make_pool_key(
            usdc.address, wbtc.address, fee=500, tick_spacing=10
        )
        pool_b_zfo = pool_b_key[0] == usdc.address
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_b_key,
            AMOUNT_USDC,
            AMOUNT_WBTC,
            pool_b_zfo,
            output_token=wbtc,
        )

        # V4c: WBTC→WETH
        pool_c_key = _make_pool_key(
            wbtc.address, weth.address, fee=10000, tick_spacing=200
        )
        pool_c_zfo = pool_c_key[0] == wbtc.address
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_c_key,
            AMOUNT_WBTC,
            AMOUNT_WETH + AMOUNT_WETH_PROFIT,
            pool_c_zfo,
            output_token=weth,
        )

        # Fund the PM with enough WETH for V4c's payout
        weth.mint(v4_pm.address, AMOUNT_WETH + AMOUNT_WETH_PROFIT, sender=owner_account)

        at = AddressTable()
        pm_idx = at.add(v4_pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)
        zero_idx = at.add(ZERO_ADDRESS)

        inner = b""
        # V4a: WETH→USDC
        inner += enc_v4_swap_compact(
            usdc_idx if pool_a_key[0] == usdc.address else weth_idx,
            weth_idx if pool_a_key[1] == weth.address else usdc_idx,
            pool_a_key[2],
            pool_a_key[3],
            zero_idx,
            pool_a_zfo,
            AMOUNT_WETH,
        )
        # V4b: USDC→WBTC
        inner += enc_v4_swap_compact(
            wbtc_idx if pool_b_key[0] == wbtc.address else usdc_idx,
            usdc_idx if pool_b_key[1] == usdc.address else wbtc_idx,
            pool_b_key[2],
            pool_b_key[3],
            zero_idx,
            pool_b_zfo,
            AMOUNT_USDC,
        )
        # V4c: WBTC→WETH
        inner += enc_v4_swap_compact(
            weth_idx if pool_c_key[0] == weth.address else wbtc_idx,
            wbtc_idx if pool_c_key[1] == wbtc.address else weth_idx,
            pool_c_key[2],
            pool_c_key[3],
            zero_idx,
            pool_c_zfo,
            AMOUNT_WBTC,
        )
        # Take WETH profit; USDC and WBTC net to zero
        inner += enc_v4_take(weth_idx, executor_idx, AMOUNT_WETH_PROFIT)
        # Settle remaining WETH delta (amount owed: AMOUNT_WETH, taken: AMOUNT_WETH_PROFIT)
        inner += enc_v4_settle_delta(weth_idx)

        commands = enc_v4_unlock(inner)
        tx = executor.execute(
            enc_preamble(at) + commands, sender=owner_account, raise_on_revert=False
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")


class TestV4V4DifferentCurrency2:
    """Moved test_v4_v4_usdc_intermediate_weth_eth back to its own class.

    This test was originally in TestV4V4DifferentCurrency but ended up
    inside TestV4V4V4ThreeHop after an edit. It uses module-level
    fixtures (executor, v4_pm) which are different from the
    three-hop fixtures (executor_three_hop, v4_pm).
    """

    def test_v4_v4_usdc_intermediate_weth_eth(
        self, usdc, weth, owner_account, executor, v4_pm
    ):
        """WETH→USDC at Pool A, then USDC→ETH at Pool B."""
        weth.mint(executor.address, 1 * 10**18, sender=owner_account)
        pool_a_key = _make_pool_key(
            weth.address, usdc.address, fee=3000, tick_spacing=60
        )
        pool_a_amount_in = 1 * 10**18
        pool_a_amount_out = 2000 * 10**6
        pool_a_zfo = pool_a_key[0] == weth.address
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_a_key,
            pool_a_amount_in,
            pool_a_amount_out,
            pool_a_zfo,
            output_token=usdc,
        )

        pool_b_key = _make_pool_key(
            NATIVE_ADDRESS, usdc.address, fee=500, tick_spacing=10
        )
        pool_b_amount_in = pool_a_amount_out
        pool_b_amount_out = 2 * 10**18
        pool_b_zfo = pool_b_key[0] == usdc.address
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_b_key,
            pool_b_amount_in,
            pool_b_amount_out,
            pool_b_zfo,
            fund_eth=True,
        )

        at = AddressTable()
        pm_idx = at.add(v4_pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        zero_idx = at.add(ZERO_ADDRESS)
        native_idx = at.add(NATIVE_ADDRESS)

        inner = b""
        inner += enc_v4_swap_compact(
            weth_idx if pool_a_key[0] == weth.address else usdc_idx,
            usdc_idx if pool_a_key[1] == usdc.address else weth_idx,
            pool_a_key[2],
            pool_a_key[3],
            zero_idx,
            pool_a_zfo,
            pool_a_amount_in,
        )
        inner += enc_v4_swap_compact(
            native_idx if pool_b_key[0] == NATIVE_ADDRESS else usdc_idx,
            usdc_idx if pool_b_key[1] == usdc.address else native_idx,
            pool_b_key[2],
            pool_b_key[3],
            zero_idx,
            pool_b_zfo,
            pool_b_amount_in,
        )
        inner += enc_v4_take(native_idx, executor_idx, pool_b_amount_out)
        inner += enc_v4_settle_delta(weth_idx)

        commands = enc_v4_unlock(inner)
        tx = executor.execute(
            enc_preamble(at) + commands, sender=owner_account, raise_on_revert=False
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")
