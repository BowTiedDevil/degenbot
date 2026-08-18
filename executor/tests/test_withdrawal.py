"""
Tests for withdrawing accumulated balances from the PoolManager and the executor.

Withdrawal flows:
  1. ERC6909 → physical tokens: V4_BURN_COMPACT + V4_TAKE_DELTA inside V4_UNLOCK
  2. ERC20 out of executor: ERC20_XFER_BALANCE
  3. Native ETH out of executor: SEND_ETH / SEND_ETH_ALL

These are the "exit" commands — they convert internal PM accounting entries
(ERC6909 balances) into physical tokens and move them to external addresses.
"""

import pytest
from .conftest_shared import (
    ZERO_ADDRESS,
    NATIVE_ADDRESS,
    WETH_DEPLOYMENT_WRAP_AMOUNT,
    _make_pool_key,
    _setup_v4_swap,
    AddressTable,
    enc_v4_swap_compact,
    enc_v4_take_delta,
    enc_v4_take_compact,
    enc_v4_mint_compact,
    enc_v4_burn_compact,
    enc_v4_sync,
    enc_v4_settle,
    enc_v4_settle_delta,
    enc_v4_unlock,
    enc_erc20_transfer,
    enc_erc20_xfer_balance,
    enc_send_eth,
    enc_send_eth_all,
    enc_preamble,
)

AMOUNT_WETH = 1 * 10**18
AMOUNT_USDC = 2000 * 10**6
AMOUNT_WETH_PROFIT = 2 * 10**18

# ═══════════════════════════════════════════════════════════════════
# 1. Withdraw ERC6909 USDC from PoolManager
# ═══════════════════════════════════════════════════════════════════


class TestWithdrawERC6909USDC:
    """Withdraw USDC held as ERC6909 inside the PoolManager."""

    def test_withdraw_erc6909_usdc_two_tx(
        self, usdc, weth, owner_account, executor, v4_pm
    ):
        """
        Two-transaction flow:
          Tx 1: V4 swap WETH→USDC, mint USDC as ERC6909 (stays inside PM)
          Tx 2: Burn ERC6909 USDC + take + transfer out to owner
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
        owner_idx = at.add(owner_account.address)

        # ── Tx 1: Swap + mint as ERC6909 ──
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

        inner_1 = (
            enc_v4_swap_compact(
                weth_idx if pool_key[0] == weth.address else usdc_idx,
                usdc_idx if pool_key[1] == usdc.address else weth_idx,
                pool_key[2],
                pool_key[3],
                zero_idx,
                zfo,
                AMOUNT_WETH,
            )
            + enc_v4_mint_compact(usdc_idx, executor_idx, AMOUNT_USDC)
            + enc_v4_sync(weth_idx)
            + enc_erc20_transfer(weth_idx, pm_idx, AMOUNT_WETH)
            + enc_v4_settle()
        )
        commands_1 = enc_v4_unlock(inner_1)
        # withdrawal intentionally moves value out of executor
        tx1 = executor.execute(
            enc_preamble(at, skip_profit=True) + commands_1, sender=owner_account
        )
        assert tx1.status == 1

        # Verify ERC6909 balance exists
        assert pm.balanceOf(executor.address, usdc_id) == AMOUNT_USDC
        # No physical USDC at executor
        assert usdc.balanceOf(executor.address) == 0

        # ── Tx 2: Burn ERC6909 + take + transfer to owner ──
        inner_2 = enc_v4_burn_compact(usdc_idx, AMOUNT_USDC) + enc_v4_take_delta(
            usdc_idx, executor_idx
        )
        commands_2 = (
            enc_send_eth_all(owner_idx)  # no-op (0 ETH balance during unlock)
            + enc_v4_unlock(inner_2)
            # After unlock: executor holds physical USDC, send it to owner
            + enc_erc20_xfer_balance(usdc_idx, owner_idx)
        )
        # withdrawal intentionally moves value out of executor
        tx2 = executor.execute(
            enc_preamble(at, skip_profit=True) + commands_2, sender=owner_account
        )
        assert tx2.status == 1

        # Verify withdrawal
        assert pm.balanceOf(executor.address, usdc_id) == 0
        assert usdc.balanceOf(executor.address) == 0
        assert usdc.balanceOf(owner_account.address) >= AMOUNT_USDC

    def test_withdraw_erc6909_usdc_single_tx(
        self, usdc, weth, owner_account, executor, v4_pm
    ):
        """
        Single-transaction flow: mint + burn + take + transfer, all in one call.
        This tests that burn+take works side-by-side with the original swap.
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
        owner_idx = at.add(owner_account.address)

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
            # Mint USDC as ERC6909
            + enc_v4_mint_compact(usdc_idx, executor_idx, AMOUNT_USDC)
            # Settle WETH debt
            + enc_v4_sync(weth_idx)
            + enc_erc20_transfer(weth_idx, pm_idx, AMOUNT_WETH)
            + enc_v4_settle()
            # Immediately burn ERC6909 + take (withdraw)
            + enc_v4_burn_compact(usdc_idx, AMOUNT_USDC)
            + enc_v4_take_delta(usdc_idx, executor_idx)
        )
        commands = enc_v4_unlock(inner) + enc_erc20_xfer_balance(usdc_idx, owner_idx)

        # withdrawal intentionally moves value out of executor
        tx = executor.execute(
            enc_preamble(at, skip_profit=True) + commands, sender=owner_account
        )
        assert tx.status == 1

        # ERC6909 balance should be 0 (minted then burned)
        assert pm.balanceOf(executor.address, usdc_id) == 0
        # Physical USDC sent to owner
        assert usdc.balanceOf(owner_account.address) >= AMOUNT_USDC
        # Executor holds no USDC
        assert usdc.balanceOf(executor.address) == 0


# ═══════════════════════════════════════════════════════════════════
# 2. Withdraw ERC6909 native ETH from PoolManager
# ═══════════════════════════════════════════════════════════════════


class TestWithdrawERC6909NativeETH:
    """Withdraw native ETH held as ERC6909 inside the PoolManager."""

    def test_withdraw_erc6909_native_eth(
        self, usdc, weth, owner_account, executor, v4_pm
    ):
        """
        Two-phase: swap WETH→native ETH, mint as ERC6909, then burn+take+send_eth_all.
        Uses a pool with native ETH as one of the currencies.
        """
        weth.mint(executor.address, AMOUNT_WETH, sender=owner_account)
        pm = v4_pm
        native_id = 0  # uint160(NATIVE_ADDRESS) = 0

        at = AddressTable()
        pm_idx = at.add(pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        zero_idx = at.add(ZERO_ADDRESS)
        native_idx = at.add(NATIVE_ADDRESS)
        owner_idx = at.add(owner_account.address)

        # ── Tx 1: Swap WETH→native ETH, mint ETH as ERC6909 ──
        # Pool: NATIVE_ADDRESS/WETH. zfo=True means WETH is input (sell WETH, buy native ETH).
        pool_key = _make_pool_key(
            NATIVE_ADDRESS, weth.address, fee=500, tick_spacing=10
        )
        zfo = pool_key[0] == weth.address  # False (NATIVE < WETH)
        _setup_v4_swap(
            pm, owner_account, pool_key, AMOUNT_WETH, AMOUNT_WETH, zfo, fund_eth=True
        )

        inner_1 = (
            enc_v4_swap_compact(
                native_idx if pool_key[0] == NATIVE_ADDRESS else weth_idx,
                weth_idx if pool_key[1] == weth.address else native_idx,
                pool_key[2],
                pool_key[3],
                zero_idx,
                zfo,
                AMOUNT_WETH,
            )
            # Mint native ETH as ERC6909 (stays inside PM, no physical ETH transfer)
            + enc_v4_mint_compact(native_idx, executor_idx, AMOUNT_WETH)
            # Settle WETH debt (we owe WETH to PM)
            + enc_v4_settle_delta(weth_idx)
        )
        commands_1 = enc_v4_unlock(inner_1)
        # withdrawal intentionally moves value out of executor
        tx1 = executor.execute(
            enc_preamble(at, skip_profit=True) + commands_1, sender=owner_account
        )
        assert tx1.status == 1

        # Verify ERC6909 balance for native ETH
        assert pm.balanceOf(executor.address, native_id) == AMOUNT_WETH

        # ── Tx 2: Burn ERC6909 ETH + take + send_eth_all to owner ──
        inner_2 = enc_v4_burn_compact(native_idx, AMOUNT_WETH) + enc_v4_take_delta(
            native_idx, executor_idx
        )
        commands_2 = enc_v4_unlock(inner_2) + enc_send_eth_all(owner_idx)

        # withdrawal intentionally moves value out of executor
        tx2 = executor.execute(
            enc_preamble(at, skip_profit=True) + commands_2, sender=owner_account
        )
        assert tx2.status == 1

        # Verify ERC6909 balance is 0
        assert pm.balanceOf(executor.address, native_id) == 0
        # Executor's ETH balance should be depleted by SEND_ETH_ALL
        assert executor.balance < 1 * 10**15


# ═══════════════════════════════════════════════════════════════════
# 3. Withdraw arbitrary ERC20 from executor
# ═══════════════════════════════════════════════════════════════════


class TestWithdrawERC20:
    """Withdraw ERC20 tokens held by the executor."""

    def test_withdraw_erc20_xfer_balance(
        self, usdc, weth, owner_account, executor, v4_pm
    ):
        """
        After a V4 swap that sends USDC to executor, withdraw all USDC
        to owner via ERC20_XFER_BALANCE.
        """
        weth.mint(executor.address, AMOUNT_WETH, sender=owner_account)
        pm = v4_pm

        at = AddressTable()
        pm_idx = at.add(pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        zero_idx = at.add(ZERO_ADDRESS)
        owner_idx = at.add(owner_account.address)

        # Swap WETH→USDC and take (executor receives physical USDC)
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

        # withdrawal intentionally moves value out of executor
        tx1 = executor.execute(
            enc_preamble(at, skip_profit=True) + commands, sender=owner_account
        )
        assert tx1.status == 1
        assert usdc.balanceOf(executor.address) == AMOUNT_USDC

        # Withdraw all USDC to owner
        usdc_before = usdc.balanceOf(owner_account.address)
        commands_withdraw = enc_erc20_xfer_balance(usdc_idx, owner_idx)
        # withdrawal intentionally moves value out of executor
        tx2 = executor.execute(
            enc_preamble(at, skip_profit=True) + commands_withdraw, sender=owner_account
        )
        assert tx2.status == 1

        assert usdc.balanceOf(executor.address) == 0
        assert usdc.balanceOf(owner_account.address) == usdc_before + AMOUNT_USDC

    def test_withdraw_erc20_transfer_specific_amount(
        self, usdc, weth, owner_account, executor
    ):
        """
        Transfer a specific ERC20 amount (not full balance) from executor.
        Uses ERC20_TRANSFER, not ERC20_XFER_BALANCE.
        """
        # Give executor some USDC directly
        usdc.mint(executor.address, AMOUNT_USDC * 3, sender=owner_account)
        assert usdc.balanceOf(executor.address) == AMOUNT_USDC * 3

        at = AddressTable()
        usdc_idx = at.add(usdc.address)
        owner_idx = at.add(owner_account.address)

        # Transfer only 1x AMOUNT_USDC (leaving 2x behind)
        commands = enc_erc20_transfer(usdc_idx, owner_idx, AMOUNT_USDC)
        tx = executor.execute(enc_preamble(at) + commands, sender=owner_account)
        assert tx.status == 1

        assert usdc.balanceOf(executor.address) == AMOUNT_USDC * 2
        assert usdc.balanceOf(owner_account.address) >= AMOUNT_USDC


# ═══════════════════════════════════════════════════════════════════
# 4. Withdraw native ETH from executor via SEND_ETH commands
# ═══════════════════════════════════════════════════════════════════


class TestWithdrawETH:
    """Withdraw native ETH from executor using SEND_ETH / SEND_ETH_ALL."""

    def test_send_eth_specific_amount(self, owner_account, executor):
        """SEND_ETH sends uint128 ETH to a specified address."""
        eth_amount = 5 * 10**18
        executor.balance = eth_amount

        at = AddressTable()
        owner_idx = at.add(owner_account.address)

        owner_eth_before = owner_account.balance
        commands = enc_send_eth(owner_idx, eth_amount)
        # withdrawal intentionally moves value out of executor
        tx = executor.execute(
            enc_preamble(at, skip_profit=True) + commands, sender=owner_account
        )
        assert tx.status == 1

        # Owner received the ETH (minus gas costs from the tx)
        assert owner_account.balance > owner_eth_before

    def test_send_eth_all(self, owner_account, executor):
        """SEND_ETH_ALL sends the executor's entire ETH balance."""
        eth_amount = 7 * 10**18
        executor.balance = eth_amount

        at = AddressTable()
        owner_idx = at.add(owner_account.address)

        owner_eth_before = owner_account.balance
        commands = enc_send_eth_all(owner_idx)
        # withdrawal intentionally moves value out of executor
        tx = executor.execute(
            enc_preamble(at, skip_profit=True) + commands, sender=owner_account
        )
        assert tx.status == 1

        # Executor should have near-zero ETH (just gas residue)
        assert executor.balance < 1 * 10**15  # less than 0.001 ETH
        # Owner received the ETH
        assert owner_account.balance > owner_eth_before


# ═══════════════════════════════════════════════════════════════════
# 5. Full round-trip: swap → mint → withdraw everything
# ═══════════════════════════════════════════════════════════════════


class TestFullRoundTrip:
    """
    Full lifecycle: profitable V4 swap, mint as ERC6909, then withdraw
    all accumulated balances (ERC6909 tokens + ETH + WETH).
    """

    def test_full_round_trip_usdc_via_erc6909(
        self, usdc, weth, owner_account, executor, v4_pm
    ):
        """
        1. V4 swap: WETH→USDC (profitable)
        2. V4_MINT_COMPACT: USDC as ERC6909 inside PM
        3. V4_BURN_COMPACT + V4_TAKE_DELTA: withdraw USDC from ERC6909
        4. ERC20_XFER_BALANCE: send USDC to owner
        5. WETH_WITHDRAW_ALL: unwrap remaining WETH
        6. SEND_ETH_ALL: send all ETH to owner
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
        owner_idx = at.add(owner_account.address)

        # ── Phase 1: Profitable swap + mint as ERC6909 ──
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
            + enc_v4_mint_compact(usdc_idx, executor_idx, AMOUNT_USDC)
            + enc_v4_sync(weth_idx)
            + enc_erc20_transfer(weth_idx, pm_idx, AMOUNT_WETH)
            + enc_v4_settle()
        )
        commands_1 = enc_v4_unlock(inner)
        # withdrawal intentionally moves value out of executor
        tx1 = executor.execute(
            enc_preamble(at, skip_profit=True) + commands_1, sender=owner_account
        )
        assert tx1.status == 1
        assert pm.balanceOf(executor.address, usdc_id) == AMOUNT_USDC

        # ── Phase 2: Withdraw everything ──
        # 2a: Burn ERC6909 USDC + take physical USDC
        # 2b: Transfer USDC to owner
        # 2c: Send all remaining ETH to owner
        inner_2 = enc_v4_burn_compact(usdc_idx, AMOUNT_USDC) + enc_v4_take_delta(
            usdc_idx, executor_idx
        )
        commands_2 = (
            enc_v4_unlock(inner_2)
            + enc_erc20_xfer_balance(usdc_idx, owner_idx)
            + enc_send_eth_all(owner_idx)
        )
        # withdrawal intentionally moves value out of executor
        tx2 = executor.execute(
            enc_preamble(at, skip_profit=True) + commands_2, sender=owner_account
        )
        assert tx2.status == 1

        # Verify clean state
        assert pm.balanceOf(executor.address, usdc_id) == 0
        assert usdc.balanceOf(executor.address) == 0
        assert usdc.balanceOf(owner_account.address) >= AMOUNT_USDC
        # Executor's ETH should be near zero
        assert executor.balance < 1 * 10**18  # well below the 1000 ETH deposit

    def test_full_round_trip_native_eth_via_erc6909(
        self, usdc, weth, owner_account, executor, v4_pm
    ):
        """
        1. V4 swap: WETH→native ETH (profitable)
        2. V4_MINT_COMPACT: ETH as ERC6909 inside PM
        3. V4_BURN_COMPACT + V4_TAKE_DELTA: withdraw ETH from ERC6909
        4. SEND_ETH_ALL: send all ETH to owner
        """
        weth.mint(executor.address, AMOUNT_WETH, sender=owner_account)
        pm = v4_pm
        native_id = 0

        at = AddressTable()
        pm_idx = at.add(pm.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        zero_idx = at.add(ZERO_ADDRESS)
        native_idx = at.add(NATIVE_ADDRESS)
        owner_idx = at.add(owner_account.address)

        # ── Phase 1: Swap WETH→native ETH, mint as ERC6909 ──
        pool_key = _make_pool_key(
            NATIVE_ADDRESS, weth.address, fee=500, tick_spacing=10
        )
        zfo = pool_key[0] == weth.address
        _setup_v4_swap(
            pm, owner_account, pool_key, AMOUNT_WETH, AMOUNT_WETH, zfo, fund_eth=True
        )

        inner_1 = (
            enc_v4_swap_compact(
                native_idx if pool_key[0] == NATIVE_ADDRESS else weth_idx,
                weth_idx if pool_key[1] == weth.address else native_idx,
                pool_key[2],
                pool_key[3],
                zero_idx,
                zfo,
                AMOUNT_WETH,
            )
            + enc_v4_mint_compact(native_idx, executor_idx, AMOUNT_WETH)
            + enc_v4_settle_delta(weth_idx)
        )
        commands_1 = enc_v4_unlock(inner_1)
        # withdrawal intentionally moves value out of executor
        tx1 = executor.execute(
            enc_preamble(at, skip_profit=True) + commands_1, sender=owner_account
        )
        assert tx1.status == 1
        assert pm.balanceOf(executor.address, native_id) == AMOUNT_WETH

        # ── Phase 2: Burn ERC6909 ETH + take + send to owner ──
        inner_2 = enc_v4_burn_compact(native_idx, AMOUNT_WETH) + enc_v4_take_delta(
            native_idx, executor_idx
        )
        commands_2 = enc_v4_unlock(inner_2) + enc_send_eth_all(owner_idx)

        # withdrawal intentionally moves value out of executor
        tx2 = executor.execute(
            enc_preamble(at, skip_profit=True) + commands_2, sender=owner_account
        )
        assert tx2.status == 1

        # Verify clean state
        assert pm.balanceOf(executor.address, native_id) == 0
        # Executor's ETH was sent out
        assert executor.balance < 1 * 10**15
