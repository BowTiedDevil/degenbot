"""
Tests for bribe functionality via the ABI config parameter.

Bribe configuration (bips + recipient) is packed into the config uint256
parameter of execute(), not in the command stream. The actual bribe is
executed by execute() after the profit check, sending profit * bips / 10000
ETH to the recipient.
"""

import pytest

from .conftest_shared import (
    ZERO_ADDRESS,
    WETH_DEPLOYMENT_WRAP_AMOUNT,
    enc_v4_swap_compact,
    enc_v4_take_compact,
    enc_v4_settle_delta,
    enc_v4_unlock,
    make_config,
    _make_pool_key,
    _setup_v4_swap,
    AddressTable,
    enc_preamble,
)

AMOUNT_WETH = 1 * 10**18
AMOUNT_USDC = 2000 * 10**6


class TestBribeCoinbase:
    """Bribe to coinbase: sends profit * bips / 10000 ETH to block.coinbase."""

    def test_bribe_coinbase_first_command(
        self, usdc, weth, owner_account, executor, v4_pm
    ):
        """Bribe via config param, bribe sent after profit check."""
        pool_key = _make_pool_key(weth.address, usdc.address, fee=0, tick_spacing=60)
        zfo = pool_key[0] == weth.address

        _setup_v4_swap(
            v4_pm, owner_account, pool_key,
            AMOUNT_WETH, AMOUNT_USDC, zfo, output_token=usdc,
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
                pool_key[2], pool_key[3], zero_idx, zfo, AMOUNT_WETH,
            )
            + enc_v4_take_compact(usdc_idx, executor_idx, AMOUNT_USDC)
            + enc_v4_settle_delta(weth_idx)
        )

        # 50% bribe to coinbase (5000 bips, recipient_idx=0) via config param
        commands = enc_v4_unlock(inner)

        weth.mint(executor.address, AMOUNT_WETH, sender=owner_account)
        tx = executor.execute(
            enc_preamble(at) + commands,
            make_config(bribe_bips=5000),  # bribe sends ETH out of executor
            sender=owner_account,
        )

        assert usdc.balanceOf(executor.address) >= AMOUNT_USDC

    def test_bribe_coinbase_zero_bips(self, usdc, weth, owner_account, executor, v4_pm):
        """0 bips in config sends no bribe."""
        pool_key = _make_pool_key(weth.address, usdc.address, fee=0, tick_spacing=60)
        zfo = pool_key[0] == weth.address

        _setup_v4_swap(
            v4_pm, owner_account, pool_key,
            AMOUNT_WETH, AMOUNT_USDC, zfo, output_token=usdc,
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
                pool_key[2], pool_key[3], zero_idx, zfo, AMOUNT_WETH,
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


class TestBribeAddress:
    """Bribe to address: sends profit * bips / 10000 ETH to address table entry."""

    def test_bribe_address_first_command(
        self, usdc, weth, owner_account, executor, v4_pm
    ):
        """Bribe to owner_account via config param."""
        pool_key = _make_pool_key(weth.address, usdc.address, fee=0, tick_spacing=60)
        zfo = pool_key[0] == weth.address

        _setup_v4_swap(
            v4_pm, owner_account, pool_key,
            AMOUNT_WETH, AMOUNT_USDC, zfo, output_token=usdc,
        )

        at = AddressTable()
        pm_idx = at.add(v4_pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        zero_idx = at.add(ZERO_ADDRESS)
        bribe_recipient_idx = at.add(owner_account.address)

        inner = (
            enc_v4_swap_compact(
                weth_idx if pool_key[0] == weth.address else usdc_idx,
                usdc_idx if pool_key[1] == usdc.address else weth_idx,
                pool_key[2], pool_key[3], zero_idx, zfo, AMOUNT_WETH,
            )
            + enc_v4_take_compact(usdc_idx, executor_idx, AMOUNT_USDC)
            + enc_v4_settle_delta(weth_idx)
        )

        commands = enc_v4_unlock(inner)

        weth.mint(executor.address, AMOUNT_WETH, sender=owner_account)
        tx = executor.execute(
            enc_preamble(at) + commands,
            make_config(bribe_bips=1000, bribe_recipient_idx=bribe_recipient_idx),
            sender=owner_account,
        )

        assert usdc.balanceOf(executor.address) >= AMOUNT_USDC

    def test_bribe_address_gas_overhead(
        self, usdc, weth, owner_account, executor, v4_pm
    ):
        """Measure gas overhead of bribe vs no bribe."""
        pool_key = _make_pool_key(weth.address, usdc.address, fee=0, tick_spacing=60)
        zfo = pool_key[0] == weth.address

        def setup():
            _setup_v4_swap(
                v4_pm, owner_account, pool_key,
                AMOUNT_WETH, AMOUNT_USDC, zfo, output_token=usdc,
            )

        at = AddressTable()
        pm_idx = at.add(v4_pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        zero_idx = at.add(ZERO_ADDRESS)
        bribe_recipient_idx = at.add(owner_account.address)

        inner = (
            enc_v4_swap_compact(
                weth_idx if pool_key[0] == weth.address else usdc_idx,
                usdc_idx if pool_key[1] == usdc.address else weth_idx,
                pool_key[2], pool_key[3], zero_idx, zfo, AMOUNT_WETH,
            )
            + enc_v4_take_compact(usdc_idx, executor_idx, AMOUNT_USDC)
            + enc_v4_settle_delta(weth_idx)
        )

        # Baseline: no bribe
        setup()
        weth.mint(executor.address, AMOUNT_WETH, sender=owner_account)
        commands_no_bribe = enc_v4_unlock(inner)
        tx_baseline = executor.execute(
            enc_preamble(at) + commands_no_bribe,
            sender=owner_account,
        )

        # With bribe
        setup()
        weth.mint(executor.address, AMOUNT_WETH, sender=owner_account)
        commands_bribe = enc_v4_unlock(inner)
        tx_bribe = executor.execute(
            enc_preamble(at) + commands_bribe,
            make_config(bribe_bips=1000, bribe_recipient_idx=bribe_recipient_idx),
            sender=owner_account,
        )

        baseline_gas = tx_baseline.gas_used
        bribe_gas = tx_bribe.gas_used
        overhead = bribe_gas - baseline_gas
        print(
            f"\n  Baseline: {baseline_gas:,} gas | With bribe: {bribe_gas:,} gas | Overhead: {overhead:+,}"
        )
        assert overhead < 5000, f"Bribe overhead too high: {overhead}"


class TestBribeWETHAutoWithdraw:
    """Verify bribe auto-withdraws WETH when ETH balance is insufficient."""

    def test_bribe_auto_withdraws_weth(
        self, usdc, weth, owner_account, executor, v4_pm
    ):
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
            v4_pm, owner_account, pool_key,
            AMOUNT_USDC, AMOUNT_WETH, zfo, output_token=weth,
        )

        inner = (
            enc_v4_swap_compact(
                weth_idx if pool_key[0] == weth.address else usdc_idx,
                usdc_idx if pool_key[1] == usdc.address else weth_idx,
                pool_key[2], pool_key[3], zero_idx, zfo, AMOUNT_USDC,
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
