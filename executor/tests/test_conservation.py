"""
Conservation check negative tests: verifies _verify_conservation catches balance bugs.

These tests deliberately introduce errors into the command stream and assert
that the conservation check (token conservation across all tracked accounts)
fails. This validates that the check is not trivially passing and actually
catches real bugs:

1. V4_TAKE to untracked address (owner): tokens physically leave PM but go
   to an address not in snapshot_balances' tracking list → WETH+ETH
   conservation fails.

2. Extra ERC20_TRANSFER to untracked address: a stray transfer drains tokens
   from the executor to an untracked address inside the unlock callback →
   WETH+ETH conservation fails.

3. V4_TAKE to wrong tracked address (V2 pair instead of executor): tokens
   go to a pool that IS tracked, so conservation passes. But the executor
   profit check fails — the executor didn't get its profit. This shows that
   conservation alone is not sufficient: it prevents leaks but doesn't verify
   correct distribution.

4. Stray ERC20_TRANSFER to untracked address in V2 callback: a WETH transfer
   to owner_account appended after the V2 swap chain completes. The swap
   succeeds (the stray transfer is independent of the K-invariant), but
   conservation detects the leak.

Key insight: conservation can only fail when tokens move to an UNTRACKED
address. If all tokens stay between tracked accounts, conservation always
holds — no ERC20 token is ever created or destroyed by legitimate swap
operations. The conservation check catches "leaks" (tokens sent to addresses
the test wasn't monitoring), while the profit check catches "wrong
distribution" (tokens went to the wrong tracked account).
"""

import pytest

from .conftest_shared import (
    ZERO_ADDRESS,
    WETH_DEPLOYMENT_WRAP_AMOUNT,
    enc_v2_swap_compact,
    enc_v2_swap_calc,
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

from .verify import snapshot_balances, diff_snapshots, NATIVE_ADDRESS

AMOUNT_WETH = 1 * 10**18
AMOUNT_USDC = 2000 * 10**6
AMOUNT_WBTC = 100 * 10**8
AMOUNT_WETH_PROFIT = 2 * 10**18
PROFIT = AMOUNT_WETH_PROFIT - AMOUNT_WETH  # 1 WETH
V2_FEE = 30

# ── Helpers (duplicated from test_cmd_executor_three_hop_optimized) ──


def _v4_swap(at, pool_key, zfo, amount, zero_idx):
    c0_idx = at.add(pool_key[0])
    c1_idx = at.add(pool_key[1])
    return enc_v4_swap_compact(
        c0_idx, c1_idx, pool_key[2], pool_key[3], zero_idx, zfo, amount
    )


def _verify_conservation(
    label, tokens, accounts, before, executor, expected_weth_delta=None
):
    """Assert that balance changes satisfy arbitrage invariants.

    Simplified copy of _verify_conservation from the three-hop test file.
    """
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
            f"Expected {expected_weth_delta}, got {combined_delta} "
            f"(WETH={weth_delta}, ETH={eth_delta}).\n"
            f"  All diffs: {diffs}"
        )

    for token in tokens[1:]:
        token_addr = token.address if hasattr(token, "address") else token
        total = sum(v for (t, _), v in diffs.items() if t == token_addr)
        assert total == 0, (
            f"{label}: {token_addr} conservation violated — "
            f"sum of balance changes = {total} (should be 0).\n"
            f"  Diffs: "
            f"{dict((a, v) for (t, a), v in diffs.items() if t == token_addr)}"
        )

    weth_total = sum(v for (t, _), v in diffs.items() if t == weth_addr)
    eth_total = sum(v for (t, _), v in diffs.items() if t == NATIVE_ADDRESS)
    combined_total = weth_total + eth_total
    assert combined_total == 0, (
        f"{label}: WETH+ETH conservation violated — "
        f"WETH total={weth_total}, ETH total={eth_total}, "
        f"combined={combined_total} (should be 0).\n"
        f"  WETH diffs: "
        f"{dict((a, v) for (t, a), v in diffs.items() if t == weth_addr)}\n"
        f"  ETH diffs: "
        f"{dict((a, v) for (t, a), v in diffs.items() if t == NATIVE_ADDRESS)}"
    )


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


# ═══════════════════════════════════════════════════════════════════════════
# 1. V4_TAKE to untracked address: WETH+ETH conservation fails
# ═══════════════════════════════════════════════════════════════════════════


class TestConservationCatchesLeakedTokens:
    """V4_TAKE sends profit to an address NOT in the accounts list.

    The WETH physically leaves PM but goes to an untracked account.
    snapshot_balances doesn't see it, so the conservation check finds
    that tracked accounts lost WETH but none gained it.
    """

    def test_v4_take_to_untracked_address(
        self, run_executor, weth, usdc, wbtc, owner_account, executor, v4_pm
    ):
        """V4-V4-V4 where V4_TAKE sends profit to owner_account instead of executor.

        owner_account is in the AddressTable (needed for the command encoding)
        but NOT in the accounts list passed to snapshot_balances. The WETH
        leaves PM, arrives at owner_account (invisible to the snapshot), and
        conservation detects a -PROFIT imbalance.
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
        b_pk, b_zfo = _setup_v4_pool(
            v4_pm,
            usdc.address,
            wbtc.address,
            AMOUNT_USDC,
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
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)
        zero_idx = at.add(ZERO_ADDRESS)
        # BUG: add owner_account to address table for V4_TAKE, but DON'T track it
        owner_idx = at.add(owner_account.address)

        inner = _v4_swap(at, a_pk, a_zfo, AMOUNT_WETH, zero_idx)
        inner += _v4_swap(at, b_pk, b_zfo, AMOUNT_USDC, zero_idx)
        inner += _v4_swap(at, c_pk, c_zfo, AMOUNT_WBTC, zero_idx)
        # BUG: profit goes to owner_account instead of executor
        inner += enc_v4_take(weth_idx, owner_idx, PROFIT)

        commands = enc_v4_unlock(inner)
        before = snapshot_balances([weth, usdc, wbtc], [executor, v4_pm])

        tx = run_executor(at, commands, owner_account)
        assert tx.status == 1  # Transaction succeeds (deltas settle correctly)

        # Conservation should FAIL: PM lost WETH but no tracked account gained it
        with pytest.raises(AssertionError, match="WETH\\+ETH conservation violated"):
            _verify_conservation(
                "V4TakeToUntracked",
                [weth, usdc, wbtc],
                [executor, v4_pm],
                before,
                executor,
            )


# ═══════════════════════════════════════════════════════════════════════════
# 2. Extra ERC20_TRANSFER to untracked address: WETH+ETH conservation fails
# ═══════════════════════════════════════════════════════════════════════════


class TestConservationCatchesExtraTransfer:
    """A stray ERC20_TRANSFER inside the command stream sends tokens to an
    address that's not being tracked. The transaction succeeds (it's just a
    regular transfer — the executor has sufficient balance). But the
    conservation check sees the executor's WETH decrease with no corresponding
    increase at any tracked account.
    """

    def test_extra_erc20_transfer_to_untracked_address(
        self, run_executor, weth, usdc, wbtc, owner_account, executor, v4_pm
    ):
        """V4-V4-V4 with a stray WETH transfer to owner_account after V4_TAKE.

        The executor takes its profit (V4_TAKE to executor), then
        accidentally transfers half of it to owner_account. The transaction
        succeeds — but conservation detects the leak.
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
        b_pk, b_zfo = _setup_v4_pool(
            v4_pm,
            usdc.address,
            wbtc.address,
            AMOUNT_USDC,
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
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)
        zero_idx = at.add(ZERO_ADDRESS)
        owner_idx = at.add(owner_account.address)

        leak_amount = PROFIT // 2  # accidentally send half the profit to owner

        inner = _v4_swap(at, a_pk, a_zfo, AMOUNT_WETH, zero_idx)
        inner += _v4_swap(at, b_pk, b_zfo, AMOUNT_USDC, zero_idx)
        inner += _v4_swap(at, c_pk, c_zfo, AMOUNT_WBTC, zero_idx)
        inner += enc_v4_take(weth_idx, executor_idx, PROFIT)  # correct V4_TAKE
        # BUG: stray transfer that shouldn't be here
        inner += enc_erc20_transfer(weth_idx, owner_idx, leak_amount)

        commands = enc_v4_unlock(inner)
        before = snapshot_balances([weth, usdc, wbtc], [executor, v4_pm])

        tx = run_executor(at, commands, owner_account)
        assert tx.status == 1  # Transaction succeeds

        # Conservation should FAIL: executor leaked WETH to untracked address
        with pytest.raises(AssertionError, match="WETH\\+ETH conservation violated"):
            _verify_conservation(
                "ExtraTransferLeak",
                [weth, usdc, wbtc],
                [executor, v4_pm],
                before,
                executor,
            )


# ═══════════════════════════════════════════════════════════════════════════
# 3. V4_TAKE to wrong tracked address: conservation passes, profit check fails
# ═══════════════════════════════════════════════════════════════════════════


class TestConservationPassesButProfitCheckFails:
    """V4_TAKE sends profit to a tracked account (V2 pair) instead of executor.

    Conservation passes because the WETH went from PM to a tracked account —
    total balance changes sum to zero. But the executor didn't get its profit,
    so the explicit profit check (expected_weth_delta) catches it.

    This demonstrates that conservation alone is not sufficient:
    it verifies tokens aren't lost, but doesn't verify they end up
    at the RIGHT account. The profit check provides that assurance.
    """

    def test_v4_take_to_tracked_pool_profit_check_fails(
        self, run_executor, weth, usdc, wbtc, owner_account, executor, v4_pm, v2_a
    ):
        """V4-V4-V4 where V4_TAKE sends profit to V2_a (tracked) instead of executor.

        WETH physically leaves PM and arrives at V2_a — both PM and V2_a
        are in the accounts list, so conservation passes. But executor
        expected +PROFIT WETH and got 0.
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
        b_pk, b_zfo = _setup_v4_pool(
            v4_pm,
            usdc.address,
            wbtc.address,
            AMOUNT_USDC,
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
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        wbtc_idx = at.add(wbtc.address)
        executor_idx = at.add(executor.address)
        zero_idx = at.add(ZERO_ADDRESS)
        # Send V4_TAKE to V2_a instead of executor
        v2a_idx = at.add(v2_a.address)

        inner = _v4_swap(at, a_pk, a_zfo, AMOUNT_WETH, zero_idx)
        inner += _v4_swap(at, b_pk, b_zfo, AMOUNT_USDC, zero_idx)
        inner += _v4_swap(at, c_pk, c_zfo, AMOUNT_WBTC, zero_idx)
        # BUG: profit goes to V2_a (tracked) instead of executor
        inner += enc_v4_take(weth_idx, v2a_idx, PROFIT)

        commands = enc_v4_unlock(inner)
        accounts = [executor, v4_pm, v2_a]
        before = snapshot_balances([weth, usdc, wbtc], accounts)

        tx = run_executor(at, commands, owner_account)
        assert tx.status == 1

        # Conservation PASSES (tokens went to a tracked account)
        _verify_conservation(
            "V4TakeToTracked", [weth, usdc, wbtc], accounts, before, executor
        )

        # But the PROFIT CHECK fails — executor didn't get its profit
        with pytest.raises(AssertionError, match="executor WETH\\+ETH profit mismatch"):
            _verify_conservation(
                "V4TakeToTracked",
                [weth, usdc, wbtc],
                accounts,
                before,
                executor,
                expected_weth_delta=PROFIT,
            )


# ═══════════════════════════════════════════════════════════════════════════
# 4. Stray ERC20_TRANSFER to untracked address in V2-V2-V2 callback
# ═══════════════════════════════════════════════════════════════════════════


class TestConservationCatchesStrayTransferInCallback:
    """A stray ERC20_TRANSFER inside a V2 callback sends tokens to an
    untracked address. The swap still succeeds (the stray transfer is
    independent of the swap mechanics), but conservation detects the leak.

    Key insight: conservation only fails when tokens leave the set of
    tracked accounts. If all tokens stay between tracked accounts,
    conservation always holds (no tokens are created/destroyed).
    The conservation check catches "leaks" — tokens sent to wrong addresses
    that the test wasn't monitoring.
    """

    def test_stray_weth_transfer_after_v2_swap(
        self, run_executor, weth, usdc, wbtc, owner_account, executor, v2_a, v2_b, v2_c
    ):
        """V2-V2-V2 where a stray WETH transfer is appended AFTER the swap chain.

        The swap chain (WETH→V2a, V2a→V2b, V2b→V2c) executes correctly.
        After the chain completes, we add a stray WETH transfer to owner_account.
        The stray transfer happens inside V2c's callback — after V2b dispatches
        WBTC to V2c (satisfying K-invariant) but before the callback returns.
        Since the executor holds WETH profit from V2c's flash swap, the stray
        transfer has enough balance to succeed. Conservation catches the leak.
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
        owner_idx = at.add(owner_account.address)  # untracked in snapshot

        stray_amount = c_out // 4  # leak 25% of V2c's output

        # Normal swap chain, then BUG: stray WETH transfer at the end
        c_fwd = enc_erc20_transfer(weth_idx, v2a_idx, AMOUNT_WETH)
        c_fwd += enc_v2_swap_calc(at.add(v2_a.address), a_zfo, v2b_idx)
        c_fwd += enc_v2_swap_calc(at.add(v2_b.address), b_zfo, v2c_idx)
        # BUG: after swap chain completes, leak WETH to owner_account
        c_fwd += enc_erc20_transfer(weth_idx, owner_idx, stray_amount)

        commands = enc_v2_swap_compact(
            at.add(v2_c.address), c_zfo, c_out, executor_idx, forward_data=c_fwd
        )
        before = snapshot_balances([weth, usdc, wbtc], [executor, v2_a, v2_b, v2_c])

        tx = run_executor(at, commands, owner_account)
        assert tx.status == 1

        # WETH+ETH conservation should FAIL: WETH leaked to untracked address
        with pytest.raises(AssertionError, match="WETH\\+ETH conservation violated"):
            _verify_conservation(
                "StrayWethAfterSwap",
                [weth, usdc, wbtc],
                [executor, v2_a, v2_b, v2_c],
                before,
                executor,
            )


def _setup_v2_for_calc(
    pool, input_token, output_token, owner, amount_in, amount_out, fee=V2_FEE
):
    """Set up a V2 pair with ample liquidity at the correct price for V2_SWAP_CALC."""
    input_token.mint(pool.address, amount_in * 100, sender=owner)
    output_token.mint(pool.address, amount_out * 100, sender=owner)
    pool.sync(sender=owner)
    zfo = pool.token0() == input_token.address
    return zfo
