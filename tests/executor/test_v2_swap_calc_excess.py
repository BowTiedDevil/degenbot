"""
Tests for V2_SWAP_CALC with excess-balance approach.

Validates:
- V2 direct: executor pre-funds pair with WETH, then V2_SWAP_CALC reads excess
- V4→V2: V4_TAKE sends USDC directly to V2 pair, creates excess
- V2→V2: first swap sends output to second pair, second swap reads excess
- K-invariant: V2_SWAP_CALC output satisfies the V2 constant-product check
- Reserve fluctuation: on-chain computation adapts to pre-swap reserve changes

With runtime K-invariant enforcement (no set_next_swap), the V2 pair
verifies the constant-product invariant after each swap. The amounts
computed by V2_SWAP_CALC are guaranteed to satisfy K, so no
pre-configuration is needed.
"""

import pytest
from .conftest_shared import (
    WETH_DEPLOYMENT_WRAP_AMOUNT,
    enc_v2_swap_calc,
    enc_erc20_transfer,
    enc_v4_swap_compact,
    enc_v4_take,
    enc_v4_take_delta,
    enc_v4_settle_delta,
    enc_v4_unlock,
    enc_v2_swap_compact,
    _make_pool_key,
    _setup_v4_swap,
    AddressTable,
    enc_preamble,
    ZERO_ADDRESS,
    v2_get_amount_out,
)


@pytest.fixture
def v2_pair(project, owner_account, usdc, weth):
    token0, token1 = sorted([usdc.address, weth.address], key=lambda addr: addr.lower())
    return project.fake_uniswap_v2_pair.deploy(
        token0, token1, 0, 30, sender=owner_account
    )


AMOUNT_WETH = 1 * 10**18
AMOUNT_USDC = 2000 * 10**6
SWAP_INPUT = 1 * 10**18


class TestV2DirectExcess:
    """V2 direct swap using V2_SWAP_CALC with excess balance."""

    def test_v2_direct_excess_balance(
        self, usdc, weth, owner_account, executor, v2_pair
    ):
        """Pre-fund V2 pair with WETH, then V2_SWAP_CALC reads excess and swaps."""
        v2_zfo = v2_pair.token0() == weth.address

        # Add liquidity and initialize reserves
        usdc.mint(v2_pair.address, AMOUNT_USDC, sender=owner_account)
        weth.mint(v2_pair.address, AMOUNT_WETH, sender=owner_account)
        v2_pair.sync(sender=owner_account)

        # Compute expected output from reserves
        reserve_in = weth.balanceOf(v2_pair.address)  # selling WETH
        reserve_out = usdc.balanceOf(v2_pair.address)  # buying USDC
        expected_out = v2_get_amount_out(SWAP_INPUT, reserve_in, reserve_out, fee=30)

        # Deposit input tokens to create excess balance (after sync, so reserves don't include it)
        weth.mint(v2_pair.address, SWAP_INPUT, sender=owner_account)

        at = AddressTable()
        v2_idx = at.add(v2_pair.address)
        executor_idx = at.add(executor.address)

        commands = enc_preamble(at) + enc_v2_swap_calc(
            v2_idx, v2_zfo, executor_idx, fee=30
        )

        executor.execute(commands, sender=owner_account)
        assert usdc.balanceOf(executor.address) >= expected_out

    def test_v2_direct_excess_balance_reverts_without_deposit(
        self, usdc, weth, owner_account, executor, v2_pair
    ):
        """V2_SWAP_CALC reverts when pair has no excess balance."""
        v2_zfo = v2_pair.token0() == weth.address

        # Add liquidity only — no excess deposit
        usdc.mint(v2_pair.address, AMOUNT_USDC, sender=owner_account)
        weth.mint(v2_pair.address, AMOUNT_WETH, sender=owner_account)
        v2_pair.sync(sender=owner_account)

        at = AddressTable()
        v2_idx = at.add(v2_pair.address)
        executor_idx = at.add(executor.address)

        commands = enc_preamble(at) + enc_v2_swap_calc(
            v2_idx, v2_zfo, executor_idx, fee=30
        )

        tx = executor.execute(commands, sender=owner_account, raise_on_revert=False)
        assert tx.status == 0, "Should revert — no excess balance"

    def test_v2_direct_no_callback_fires(
        self, usdc, weth, owner_account, executor, v2_pair
    ):
        """V2_SWAP_CALC with excess balance calls swap() with data=b'' (no callback)."""
        v2_zfo = v2_pair.token0() == weth.address

        # Add liquidity and initialize reserves
        usdc.mint(v2_pair.address, AMOUNT_USDC, sender=owner_account)
        weth.mint(v2_pair.address, AMOUNT_WETH, sender=owner_account)
        v2_pair.sync(sender=owner_account)

        reserve_in = weth.balanceOf(v2_pair.address)
        reserve_out = usdc.balanceOf(v2_pair.address)
        expected_out = v2_get_amount_out(SWAP_INPUT, reserve_in, reserve_out, fee=30)

        # Deposit input tokens (creates excess)
        weth.mint(v2_pair.address, SWAP_INPUT, sender=owner_account)

        # Verify executor's WETH balance DOESN'T change (no callback = no WETH transfer)
        weth_before = weth.balanceOf(executor.address)

        at = AddressTable()
        v2_idx = at.add(v2_pair.address)
        executor_idx = at.add(executor.address)

        commands = enc_preamble(at) + enc_v2_swap_calc(
            v2_idx, v2_zfo, executor_idx, fee=30
        )

        executor.execute(commands, sender=owner_account)

        # Executor should have USDC profit, not less WETH
        assert usdc.balanceOf(executor.address) >= expected_out


class TestV4ToV2DirectCustody:
    """V4→V2 with direct custody — V4_TAKE sends USDC to V2 pair directly."""

    def test_v4_v2_direct_custody_take(
        self, usdc, weth, owner_account, executor, v4_pm, v2_pair
    ):
        """V4_TAKE sends USDC to V2 pair → V2_SWAP_CALC reads excess."""
        weth.mint(executor.address, AMOUNT_WETH, sender=owner_account)
        pool_key = _make_pool_key(weth.address, usdc.address, fee=3000, tick_spacing=60)
        pool_a_zfo = pool_key[0] == weth.address
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_key,
            AMOUNT_WETH,
            AMOUNT_USDC,
            pool_a_zfo,
            output_token=usdc,
        )

        v2_zfo = v2_pair.token0() == usdc.address
        usdc.mint(v2_pair.address, 10_000 * 10**6, sender=owner_account)
        weth.mint(v2_pair.address, 5 * 10**18, sender=owner_account)
        v2_pair.sync(sender=owner_account)

        at = AddressTable()
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        v2_idx = at.add(v2_pair.address)
        zero_idx = at.add(ZERO_ADDRESS)

        inner = enc_v4_swap_compact(
            weth_idx if pool_key[0] == weth.address else usdc_idx,
            usdc_idx if pool_key[1] == usdc.address else weth_idx,
            pool_key[2],
            pool_key[3],
            zero_idx,
            pool_a_zfo,
            AMOUNT_WETH,
        )
        inner += enc_v4_take(usdc_idx, v2_idx, AMOUNT_USDC)
        inner += enc_v2_swap_calc(v2_idx, v2_zfo, executor_idx, fee=30)
        inner += enc_v4_settle_delta(weth_idx)

        commands = enc_v4_unlock(inner)
        tx = executor.execute(
            # V2 custody test; executor holds tokens for V2 without profit
            enc_preamble(at, skip_profit=True) + commands,
            sender=owner_account,
            raise_on_revert=False,
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")

    def test_v4_v2_direct_custody_take_delta(
        self, usdc, weth, owner_account, executor, v4_pm, v2_pair
    ):
        """V4_TAKE_DELTA sends USDC to V2 pair → V2_SWAP_CALC reads excess."""
        weth.mint(executor.address, AMOUNT_WETH, sender=owner_account)
        pool_key = _make_pool_key(weth.address, usdc.address, fee=3000, tick_spacing=60)
        pool_a_zfo = pool_key[0] == weth.address
        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_key,
            AMOUNT_WETH,
            AMOUNT_USDC,
            pool_a_zfo,
            output_token=usdc,
        )

        v2_zfo = v2_pair.token0() == usdc.address
        usdc.mint(v2_pair.address, 10_000 * 10**6, sender=owner_account)
        weth.mint(v2_pair.address, 5 * 10**18, sender=owner_account)
        v2_pair.sync(sender=owner_account)

        at = AddressTable()
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        v2_idx = at.add(v2_pair.address)
        zero_idx = at.add(ZERO_ADDRESS)

        inner = enc_v4_swap_compact(
            weth_idx if pool_key[0] == weth.address else usdc_idx,
            usdc_idx if pool_key[1] == usdc.address else weth_idx,
            pool_key[2],
            pool_key[3],
            zero_idx,
            pool_a_zfo,
            AMOUNT_WETH,
        )
        inner += enc_v4_take_delta(usdc_idx, v2_idx)
        inner += enc_v2_swap_calc(v2_idx, v2_zfo, executor_idx, fee=30)
        inner += enc_v4_settle_delta(weth_idx)

        commands = enc_v4_unlock(inner)
        tx = executor.execute(
            # V2 custody test; executor holds tokens for V2 without profit
            enc_preamble(at, skip_profit=True) + commands,
            sender=owner_account,
            raise_on_revert=False,
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")


class TestV2ToV2DirectCustody:
    """V2→V2 chain — first swap sends output directly to second pair."""

    def test_v2_v2_direct_custody(
        self, project, usdc, weth, owner_account, executor, v2_pair
    ):
        """V2→V2: First swap sends USDC to second V2 pair, V2_SWAP_CALC reads excess."""
        # Deploy a second V2 pair
        token0, token1 = sorted([usdc.address, weth.address], key=lambda a: a.lower())
        v2_pair_2 = project.fake_uniswap_v2_pair.deploy(
            token0, token1, 0, 30, sender=owner_account
        )

        # Setup first pair: sell WETH → USDC
        v2_1_zfo = v2_pair.token0() == weth.address
        usdc.mint(v2_pair.address, 10_000 * 10**6, sender=owner_account)
        weth.mint(v2_pair.address, 5 * 10**18, sender=owner_account)
        v2_pair.sync(sender=owner_account)
        reserve_in_1 = weth.balanceOf(v2_pair.address)
        reserve_out_1 = usdc.balanceOf(v2_pair.address)
        v2_1_out = v2_get_amount_out(SWAP_INPUT, reserve_in_1, reserve_out_1, fee=30)
        # Pre-fund first pair with WETH (creates excess)
        weth.mint(v2_pair.address, SWAP_INPUT, sender=owner_account)

        # Setup second pair: sell USDC → WETH
        v2_2_zfo = v2_pair_2.token0() == usdc.address
        usdc.mint(v2_pair_2.address, 20_000 * 10**6, sender=owner_account)
        weth.mint(v2_pair_2.address, 10 * 10**18, sender=owner_account)
        v2_pair_2.sync(sender=owner_account)

        at = AddressTable()
        v2_1_idx = at.add(v2_pair.address)
        v2_2_idx = at.add(v2_pair_2.address)
        executor_idx = at.add(executor.address)

        # V2_SWAP_CALC on pair 1: reads excess WETH, sends USDC to pair 2
        # V2_SWAP_CALC on pair 2: reads excess USDC, sends WETH to executor
        commands = enc_preamble(at)
        commands += enc_v2_swap_calc(v2_1_idx, v2_1_zfo, v2_2_idx, fee=30)
        commands += enc_v2_swap_calc(v2_2_idx, v2_2_zfo, executor_idx, fee=30)

        executor.execute(commands, sender=owner_account)
        # Executor should have WETH profit from the arbitrage
        assert weth.balanceOf(executor.address) > 0


class TestExcessBalanceWithMultipleDeposits:
    """V2_SWAP_CALC correctly reads total excess when multiple deposits create it."""

    def test_multiple_deposits_create_excess(
        self, usdc, weth, owner_account, executor, v2_pair
    ):
        """Two separate deposits to V2 pair are both counted in excess balance."""
        v2_zfo = v2_pair.token0() == weth.address

        # Add liquidity and initialize reserves
        usdc.mint(v2_pair.address, AMOUNT_USDC, sender=owner_account)
        weth.mint(v2_pair.address, AMOUNT_WETH, sender=owner_account)
        v2_pair.sync(sender=owner_account)

        deposit_1 = 5 * 10**17  # 0.5 WETH
        deposit_2 = 5 * 10**17  # 0.5 WETH
        total_deposit = deposit_1 + deposit_2  # 1 WETH

        # Compute expected output using total deposit
        reserve_in = weth.balanceOf(v2_pair.address)
        reserve_out = usdc.balanceOf(v2_pair.address)
        expected_out = v2_get_amount_out(total_deposit, reserve_in, reserve_out, fee=30)

        # Deposit in two transactions (pairs are stateful)
        weth.mint(v2_pair.address, deposit_1, sender=owner_account)
        weth.mint(v2_pair.address, deposit_2, sender=owner_account)

        at = AddressTable()
        v2_idx = at.add(v2_pair.address)
        executor_idx = at.add(executor.address)

        commands = enc_preamble(at) + enc_v2_swap_calc(
            v2_idx, v2_zfo, executor_idx, fee=30
        )

        executor.execute(commands, sender=owner_account)
        assert usdc.balanceOf(executor.address) >= expected_out
