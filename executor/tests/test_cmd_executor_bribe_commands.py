"""
Tests for bribe functionality via the ABI config parameter.

Bribe configuration (bips + recipient) is packed into the config uint256
parameter of execute(), not in the command stream. The actual bribe is
executed by execute() after the profit check, sending profit * bips / 10000
ETH to the recipient.

U3WVLL: any slow-path tx (check_mode != 0, or bribe requested) asserts
combined-after >= combined-before on the on-chain WETH+ETH balance. A
self-funded bribe tx must therefore end with MORE WETH+ETH than it started.
The bribe flows below are 2-hop WETH→USDC→WETH routes whose terminal profit
is WETH. (The old single WETH→USDC route predates U3WVLL and now correctly
reverts as a WETH+ETH-losing self-fund path.)
"""

import pytest

from .conftest_shared import (
    ZERO_ADDRESS,
    AddressTable,
    _make_pool_key,
    _setup_v4_swap,
    enc_preamble,
    enc_v4_settle_delta,
    enc_v4_swap_compact,
    enc_v4_take,
    enc_v4_take_compact,
    enc_v4_unlock,
    make_config,
)

AMOUNT_WETH = 1 * 10**18
AMOUNT_USDC = 2000 * 10**6

# 2-hop bribe routes: 10 WETH in, 20 WETH out — terminal profit denominated
# in WETH (required by the U3WVLL on-chain profit floor on self-funded
# slow-path txs; see the file docstring).
IN_WETH = 10 * 10**18
MID_USDC = 2000 * 10**6
OUT_WETH = 20 * 10**18


def _two_hop_weth_usdc_weth(
    v4_pm,
    owner,
    weth,
    usdc,
    weth_idx,
    usdc_idx,
    executor_idx,
    zero_idx,
    in_weth,
    mid_usdc,
    out_weth,
):
    """Two-hop WETH→USDC→WETH arbitrage on the fake PM (fee 0).

    Pool A: WETH→USDC, Pool B: USDC→WETH (distinct tick spacings). After
    both swaps the WETH delta nets to +profit; the trailing take captures
    it, so all PM deltas net to zero (no settle command needed). Returns
    the inner command stream.
    """
    pool_a_key = _make_pool_key(weth.address, usdc.address, fee=0, tick_spacing=60)
    pool_a_zfo = pool_a_key[0] == weth.address
    _setup_v4_swap(v4_pm, owner, pool_a_key, in_weth, mid_usdc, pool_a_zfo, output_token=usdc)

    pool_b_key = _make_pool_key(weth.address, usdc.address, fee=0, tick_spacing=120)
    pool_b_zfo = pool_b_key[0] == usdc.address
    _setup_v4_swap(v4_pm, owner, pool_b_key, mid_usdc, out_weth, pool_b_zfo, output_token=weth)

    c0a, c1a = (weth_idx, usdc_idx) if pool_a_key[0] == weth.address else (usdc_idx, weth_idx)
    c0b, c1b = (usdc_idx, weth_idx) if pool_b_key[0] == usdc.address else (weth_idx, usdc_idx)

    return (
        enc_v4_swap_compact(c0a, c1a, pool_a_key[2], pool_a_key[3], zero_idx, pool_a_zfo, in_weth)
        + enc_v4_swap_compact(
            c0b, c1b, pool_b_key[2], pool_b_key[3], zero_idx, pool_b_zfo, mid_usdc
        )
        + enc_v4_take(weth_idx, executor_idx, out_weth - in_weth)
    )


class TestBribeCoinbase:
    """Bribe to coinbase: sends profit * bips / 10000 ETH to block.coinbase."""

    def test_bribe_coinbase_first_command(self, usdc, weth, owner_account, executor, v4_pm):
        """50% bribe to block.coinbase via config param, paid from WETH profit."""
        at = AddressTable()
        pm_idx = at.add(v4_pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        zero_idx = at.add(ZERO_ADDRESS)

        inner = _two_hop_weth_usdc_weth(
            v4_pm,
            owner_account,
            weth,
            usdc,
            weth_idx,
            usdc_idx,
            executor_idx,
            zero_idx,
            IN_WETH,
            MID_USDC,
            OUT_WETH,
        )
        commands = enc_v4_unlock(inner)

        weth.mint(executor.address, IN_WETH, sender=owner_account)
        combined_before = executor.balance + weth.balanceOf(executor.address)
        tx = executor.execute(
            enc_preamble(at) + commands,
            make_config(check_mode=1, bribe_bips=5000),  # recipient_idx=0 → coinbase
            sender=owner_account,
        )
        combined_after = executor.balance + weth.balanceOf(executor.address)

        profit = OUT_WETH - IN_WETH
        assert combined_after - combined_before == profit - (profit * 5000 // 10_000)

    def test_bribe_coinbase_zero_bips(self, usdc, weth, owner_account, executor, v4_pm):
        """0 bips in config sends no bribe."""
        pool_key = _make_pool_key(weth.address, usdc.address, fee=0, tick_spacing=60)
        zfo = pool_key[0] == weth.address

        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_key,
            AMOUNT_WETH,
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

        inner = (
            enc_v4_swap_compact(
                weth_idx if pool_key[0] == weth.address else usdc_idx,
                usdc_idx if pool_key[1] == usdc.address else weth_idx,
                pool_key[2],
                pool_key[3],
                zero_idx,
                zfo,
                AMOUNT_WETH,
            )
            + enc_v4_take_compact(usdc_idx, executor_idx, AMOUNT_USDC)
            + enc_v4_settle_delta(weth_idx)
        )

        commands = enc_v4_unlock(inner)

        weth.mint(executor.address, AMOUNT_WETH, sender=owner_account)
        tx = executor.execute(
            enc_preamble(at) + commands,
            make_config(bribe_bips=0),  # 0 bips = no bribe
            sender=owner_account,
        )

        assert usdc.balanceOf(executor.address) >= AMOUNT_USDC


@pytest.fixture
def bribe_recipient(accounts):
    """Dedicated account that receives BRIBE_ADDRESS payments."""
    return accounts[5]


class TestBribeAddress:
    """Bribe to address: sends profit * bips / 10000 ETH to address table entry."""

    def test_bribe_address_first_command(
        self, usdc, weth, owner_account, executor, v4_pm, bribe_recipient
    ):
        """10% bribe to an address-table entry via config param, paid from WETH profit."""
        at = AddressTable()
        pm_idx = at.add(v4_pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        zero_idx = at.add(ZERO_ADDRESS)
        bribe_recipient_idx = at.add(bribe_recipient.address)

        inner = _two_hop_weth_usdc_weth(
            v4_pm,
            owner_account,
            weth,
            usdc,
            weth_idx,
            usdc_idx,
            executor_idx,
            zero_idx,
            IN_WETH,
            MID_USDC,
            OUT_WETH,
        )
        commands = enc_v4_unlock(inner)

        weth.mint(executor.address, IN_WETH, sender=owner_account)
        recipient_before = bribe_recipient.balance
        combined_before = executor.balance + weth.balanceOf(executor.address)
        tx = executor.execute(
            enc_preamble(at) + commands,
            make_config(check_mode=1, bribe_bips=1000, bribe_recipient_idx=bribe_recipient_idx),
            sender=owner_account,
        )
        combined_after = executor.balance + weth.balanceOf(executor.address)

        profit = OUT_WETH - IN_WETH
        bribe = profit * 1000 // 10_000
        assert bribe_recipient.balance - recipient_before == bribe
        assert combined_after - combined_before == profit - bribe

    def test_bribe_address_gas_overhead(self, usdc, weth, owner_account, executor, v4_pm):
        """Measure gas overhead of bribe vs no bribe (same 2-hop flow)."""
        at = AddressTable()
        pm_idx = at.add(v4_pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        zero_idx = at.add(ZERO_ADDRESS)
        bribe_recipient_idx = at.add(owner_account.address)

        def setup():
            # (Re)sets both pools — idempotent; the repeat only mints idle
            # output tokens into the fake PM (harmless).
            return _two_hop_weth_usdc_weth(
                v4_pm,
                owner_account,
                weth,
                usdc,
                weth_idx,
                usdc_idx,
                executor_idx,
                zero_idx,
                IN_WETH,
                MID_USDC,
                OUT_WETH,
            )

        # Baseline: no bribe → config=0 fast path (no balance reads, no
        # assert, no payment).
        setup()
        weth.mint(executor.address, IN_WETH, sender=owner_account)
        tx_baseline = executor.execute(
            enc_preamble(at) + enc_v4_unlock(setup()),
            sender=owner_account,
        )

        # With bribe → slow path: balance reads + profit assert + WETH
        # withdraw + ETH transfer to the owner.
        setup()
        weth.mint(executor.address, IN_WETH, sender=owner_account)
        tx_bribe = executor.execute(
            enc_preamble(at) + enc_v4_unlock(setup()),
            make_config(check_mode=1, bribe_bips=1000, bribe_recipient_idx=bribe_recipient_idx),
            sender=owner_account,
        )

        baseline_gas = tx_baseline.gas_used
        bribe_gas = tx_bribe.gas_used
        overhead = bribe_gas - baseline_gas
        print(
            f"\n  Baseline: {baseline_gas:,} gas | With bribe: {bribe_gas:,} gas | Overhead: {overhead:+,}"
        )
        assert overhead > 0, "bribe path must cost more than the no-bribe fast path"
        assert overhead < 60_000, f"Bribe overhead too high: {overhead}"


class TestBribeWETHAutoWithdraw:
    """Verify bribe auto-withdraws WETH when ETH balance is insufficient."""

    def test_bribe_auto_withdraws_weth(self, usdc, weth, owner_account, executor, v4_pm):
        """
        When profit is in WETH, the bribe auto-withdraws WETH to cover
        the ETH shortfall.
        """
        pool_key = _make_pool_key(weth.address, usdc.address, fee=0, tick_spacing=60)
        zfo = pool_key[0] == usdc.address  # selling USDC, buying WETH

        at = AddressTable()
        pm_idx = at.add(v4_pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        zero_idx = at.add(ZERO_ADDRESS)

        executor.withdraw(executor.balance, owner_account.address, sender=owner_account)
        assert executor.balance == 0

        usdc.mint(executor.address, AMOUNT_USDC, sender=owner_account)

        _setup_v4_swap(
            v4_pm,
            owner_account,
            pool_key,
            AMOUNT_USDC,
            AMOUNT_WETH,
            zfo,
            output_token=weth,
        )

        inner = (
            enc_v4_swap_compact(
                weth_idx if pool_key[0] == weth.address else usdc_idx,
                usdc_idx if pool_key[1] == usdc.address else weth_idx,
                pool_key[2],
                pool_key[3],
                zero_idx,
                zfo,
                AMOUNT_USDC,
            )
            + enc_v4_take_compact(weth_idx, executor_idx, AMOUNT_WETH)
            + enc_v4_settle_delta(usdc_idx)
        )

        # 50% bribe via config
        commands = enc_v4_unlock(inner)
        weth_bal_before = weth.balanceOf(executor.address)

        tx = executor.execute(
            enc_preamble(at) + commands,
            make_config(bribe_bips=5000),  # bribe sends ETH out of executor
            sender=owner_account,
        )
        assert tx.status == 1

        weth_bal_after = weth.balanceOf(executor.address)
        assert weth_bal_after < weth_bal_before + AMOUNT_WETH, (
            "WETH should have been unwrapped for bribe"
        )
        assert executor.balance < AMOUNT_WETH // 10
