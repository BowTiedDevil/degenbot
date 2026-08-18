"""
Tests for ERC6909 mint/burn on the fake PoolManager.

ERC6909 enables "internal balances" — assets held as accounting entries inside
the PoolManager without physical ERC-20 transfers.

Key optimization: replacing take+sync+transfer+settle with mint
  Old: take(USDC) + sync(USDC) + transfer(USDC→PM) + settle(USDC)  (4 ops, delta back to +amount)
  New: mint(USDC as ERC6909)                                        (1 op, delta goes to 0)

Uses cmd_executor with V4_MINT_COMPACT (0x19) and V4_BURN_COMPACT (0x1A).
"""

import pytest

from .conftest_shared import (
    ZERO_ADDRESS,
    WETH_DEPLOYMENT_WRAP_AMOUNT,
    _make_pool_key,
    _setup_v4_swap,
    AddressTable,
    enc_v4_swap_compact,
    enc_v4_take,
    enc_v4_take_compact,
    enc_v4_mint_compact,
    enc_v4_burn_compact,
    enc_v4_sync,
    enc_v4_settle,
    enc_v4_unlock,
    enc_erc20_transfer,
    enc_preamble,
)

AMOUNT_WETH = 1 * 10**18
AMOUNT_USDC = 2000 * 10**6
AMOUNT_WETH_PROFIT = 2 * 10**18


class TestERC6909ViewFunctions:
    """ERC6909 view function tests on the fake PoolManager."""

    def test_default_balances_are_zero(self, v4_pm, usdc, owner_account):
        pm = v4_pm
        usdc_id = int(usdc.address, 16)
        assert pm.balanceOf(owner_account.address, usdc_id) == 0
        assert pm.allowance(owner_account.address, owner_account.address, usdc_id) == 0
        assert pm.isOperator(owner_account.address, owner_account.address) is False

    def test_approve_and_set_operator(self, v4_pm, usdc, owner_account, accounts):
        pm = v4_pm
        other = accounts[1]
        usdc_id = int(usdc.address, 16)

        pm.approve(other.address, usdc_id, 1000, sender=owner_account)
        assert pm.allowance(owner_account.address, other.address, usdc_id) == 1000

        pm.setOperator(other.address, True, sender=owner_account)
        assert pm.isOperator(owner_account.address, other.address) is True


class TestERC6909Mint:
    """Test V4_MINT_COMPACT converts positive delta into ERC6909 balance."""

    def test_mint_creates_erc6909_balance(
        self, usdc, weth, owner_account, v4_pm, executor
    ):
        """
        V4 WETH→USDC swap, then mint USDC as ERC6909 instead of take.

        Mint converts the positive USDC delta into an ERC6909 balance entry.
        No physical token transfer — USDC stays inside the PoolManager.

        Commands: V4_UNLOCK(V4_SWAP_COMPACT + V4_MINT_COMPACT + V4_SYNC + V4_SETTLE)
        """
        weth.mint(executor.address, AMOUNT_WETH, sender=owner_account)
        pm = v4_pm

        pool_key = _make_pool_key(weth.address, usdc.address, fee=3000, tick_spacing=60)
        zfo = pool_key[0] == weth.address
        _setup_v4_swap(
            pm,
            owner_account,
            pool_key,
            AMOUNT_WETH,
            AMOUNT_USDC,
            zfo,
            output_token=usdc,
        )

        at = AddressTable()
        pm_idx = at.add(pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        zero_idx = at.add(ZERO_ADDRESS)

        usdc_id = int(usdc.address, 16)

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
            # mint USDC as ERC6909 (replaces take — USDC stays inside PM)
            + enc_v4_mint_compact(usdc_idx, executor_idx, AMOUNT_USDC)
            # settle WETH debt
            + enc_v4_sync(weth_idx)
            + enc_erc20_transfer(weth_idx, pm_idx, AMOUNT_WETH)
            + enc_v4_settle()
        )
        commands = enc_v4_unlock(inner)

        # ERC6909 mint/burn converts ETH/WETH to ERC6909, reducing combined balance
        tx = executor.execute(
            enc_preamble(at, skip_profit=True) + commands, sender=owner_account
        )
        assert tx.status == 1

        erc6909_bal = pm.balanceOf(executor.address, usdc_id)
        assert erc6909_bal == AMOUNT_USDC, (
            f"ERC6909 USDC balance should be {AMOUNT_USDC}, got {erc6909_bal}"
        )
        # No physical USDC was transferred out of PM
        assert usdc.balanceOf(executor.address) == 0

        print(f"\n  ✅ V4_MINT_COMPACT created ERC6909 balance of {AMOUNT_USDC} USDC")
        print("     (no physical USDC transfer — internal PM accounting)")


class TestERC6909Burn:
    """Test V4_BURN_COMPACT converts ERC6909 balance into a payable delta."""

    def test_burn_settles_from_erc6909(
        self, usdc, weth, owner_account, v4_pm, executor
    ):
        """
        Two-phase flow using cmd_executor:
        1. Swap WETH→USDC, mint USDC as ERC6909, settle WETH debt
        2. Swap USDC→WETH, burn ERC6909 USDC (settles USDC debt), take WETH
        """
        weth.mint(executor.address, AMOUNT_WETH, sender=owner_account)
        pm = v4_pm
        usdc_id = int(usdc.address, 16)

        at = AddressTable()
        pm_idx = at.add(pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        zero_idx = at.add(ZERO_ADDRESS)

        # ── Phase 1: WETH→USDC swap, mint USDC as ERC6909 ──
        pool_key_a = _make_pool_key(
            weth.address, usdc.address, fee=3000, tick_spacing=60
        )
        zfo_a = pool_key_a[0] == weth.address
        _setup_v4_swap(
            pm,
            owner_account,
            pool_key_a,
            AMOUNT_WETH,
            AMOUNT_USDC,
            zfo_a,
            output_token=usdc,
        )

        inner_1 = (
            enc_v4_swap_compact(
                weth_idx if pool_key_a[0] == weth.address else usdc_idx,
                usdc_idx if pool_key_a[1] == usdc.address else weth_idx,
                pool_key_a[2],
                pool_key_a[3],
                zero_idx,
                zfo_a,
                AMOUNT_WETH,
            )
            + enc_v4_mint_compact(usdc_idx, executor_idx, AMOUNT_USDC)
            + enc_v4_sync(weth_idx)
            + enc_erc20_transfer(weth_idx, pm_idx, AMOUNT_WETH)
            + enc_v4_settle()
        )
        commands_1 = enc_v4_unlock(inner_1)

        # ERC6909 mint/burn converts ETH/WETH to ERC6909, reducing combined balance
        tx1 = executor.execute(
            enc_preamble(at, skip_profit=True) + commands_1, sender=owner_account
        )
        assert tx1.status == 1
        assert pm.balanceOf(executor.address, usdc_id) == AMOUNT_USDC

        # ── Phase 2: USDC→WETH swap, burn ERC6909 USDC to pay ──
        pool_key_b = _make_pool_key(
            usdc.address, weth.address, fee=3000, tick_spacing=60
        )
        zfo_b = pool_key_b[0] == usdc.address
        _setup_v4_swap(
            pm,
            owner_account,
            pool_key_b,
            AMOUNT_USDC,
            AMOUNT_WETH_PROFIT,
            zfo_b,
            output_token=weth,
        )

        inner_2 = (
            enc_v4_swap_compact(
                usdc_idx if pool_key_b[0] == usdc.address else weth_idx,
                weth_idx if pool_key_b[1] == weth.address else usdc_idx,
                pool_key_b[2],
                pool_key_b[3],
                zero_idx,
                zfo_b,
                AMOUNT_USDC,
            )
            # burn ERC6909 USDC → adds positive delta for USDC (paying the debt)
            + enc_v4_burn_compact(usdc_idx, AMOUNT_USDC)
            # take WETH output
            + enc_v4_take_compact(weth_idx, executor_idx, AMOUNT_WETH_PROFIT)
        )
        commands_2 = enc_v4_unlock(inner_2)

        # ERC6909 mint/burn converts ETH/WETH to ERC6909, reducing combined balance
        tx2 = executor.execute(
            enc_preamble(at, skip_profit=True) + commands_2, sender=owner_account
        )
        assert tx2.status == 1

        assert pm.balanceOf(executor.address, usdc_id) == 0
        assert weth.balanceOf(executor.address) >= AMOUNT_WETH_PROFIT

        print(
            "\n  ✅ burn() settled USDC debt from ERC6909 balance (no physical transfer)"
        )
        print(f"     Executor received {AMOUNT_WETH_PROFIT} WETH via take()")


class TestERC6909GasSavings:
    """
    Gas comparison: mint vs explicit take for positive delta consumption.

    In V4, when a swap creates a positive delta (PM owes executor tokens),
    the executor can either:
      A) take(): physical transfer out of PM (delta → 0, executor gets ERC-20)
      B) mint(): convert to ERC6909 balance inside PM (delta → 0, no transfer)

    mint() saves gas by eliminating the ERC-20 transfer, but the token
    stays inside PM as an accounting entry (useful for V4→V4 paths where
    the intermediate token can be burned later for settlement).
    """

    def test_mint_vs_take_gas(self, usdc, weth, owner_account, v4_pm, executor):
        """
        Compare gas for consuming a positive USDC delta after a V4 swap.

        Take: V4_TAKE(USDC) — physical transfer out, delta goes to 0
        Mint: V4_MINT_COMPACT(USDC) — ERC6909 entry, delta goes to 0, no transfer

        Both: V4_SYNC(WETH) + ERC20_XFER(WETH→PM) + V4_SETTLE for input debt
        """
        weth.mint(executor.address, AMOUNT_WETH, sender=owner_account)
        pm = v4_pm

        pool_key = _make_pool_key(weth.address, usdc.address, fee=3000, tick_spacing=60)
        zfo = pool_key[0] == weth.address
        usdc_id = int(usdc.address, 16)

        at = AddressTable()
        pm_idx = at.add(pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        zero_idx = at.add(ZERO_ADDRESS)

        # ── Take: physical transfer of USDC out of PM ──
        _setup_v4_swap(
            pm,
            owner_account,
            pool_key,
            AMOUNT_WETH,
            AMOUNT_USDC,
            zfo,
            output_token=usdc,
        )

        inner_take = (
            enc_v4_swap_compact(
                weth_idx if pool_key[0] == weth.address else usdc_idx,
                usdc_idx if pool_key[1] == usdc.address else weth_idx,
                pool_key[2],
                pool_key[3],
                zero_idx,
                zfo,
                AMOUNT_WETH,
            )
            # USDC: take out of PM (physical transfer)
            + enc_v4_take(usdc_idx, executor_idx, AMOUNT_USDC)
            # WETH: settle input debt
            + enc_v4_sync(weth_idx)
            + enc_erc20_transfer(weth_idx, pm_idx, AMOUNT_WETH)
            + enc_v4_settle()
        )
        commands_take = enc_v4_unlock(inner_take)

        tx_take = executor.execute(
            # ERC6909 mint/burn converts ETH/WETH to ERC6909, reducing combined balance
            enc_preamble(at, skip_profit=True) + commands_take,
            sender=owner_account,
        )
        take_gas = tx_take.gas_used
        assert usdc.balanceOf(executor.address) == AMOUNT_USDC

        # ── Mint: ERC6909 entry inside PM ──
        weth.mint(executor.address, AMOUNT_WETH, sender=owner_account)
        _setup_v4_swap(
            pm,
            owner_account,
            pool_key,
            AMOUNT_WETH,
            AMOUNT_USDC,
            zfo,
            output_token=usdc,
        )

        inner_mint = (
            enc_v4_swap_compact(
                weth_idx if pool_key[0] == weth.address else usdc_idx,
                usdc_idx if pool_key[1] == usdc.address else weth_idx,
                pool_key[2],
                pool_key[3],
                zero_idx,
                zfo,
                AMOUNT_WETH,
            )
            # USDC: mint as ERC6909 (no physical transfer)
            + enc_v4_mint_compact(usdc_idx, executor_idx, AMOUNT_USDC)
            # WETH: settle input debt
            + enc_v4_sync(weth_idx)
            + enc_erc20_transfer(weth_idx, pm_idx, AMOUNT_WETH)
            + enc_v4_settle()
        )
        commands_mint = enc_v4_unlock(inner_mint)

        tx_mint = executor.execute(
            # ERC6909 mint/burn converts ETH/WETH to ERC6909, reducing combined balance
            enc_preamble(at, skip_profit=True) + commands_mint,
            sender=owner_account,
        )
        mint_gas = tx_mint.gas_used
        assert pm.balanceOf(executor.address, usdc_id) == AMOUNT_USDC

        gas_saved = take_gas - mint_gas
        pct = gas_saved / take_gas * 100
        print("\n  V4 swap: consuming positive USDC delta after swap")
        print(f"    V4_TAKE:               {take_gas:>8,} gas  (physical transfer out)")
        print(
            f"    V4_MINT_COMPACT:       {mint_gas:>8,} gas  (ERC6909 entry, {gas_saved:+,} = {pct:+.1f}%)"
        )

        # Mint should save gas because it avoids the ERC-20 transfer
        assert mint_gas < take_gas, (
            "mint should be cheaper than take (no ERC-20 transfer)"
        )
