"""
Tests for cmd_executor V4-V3 swap execution.

Verifies that:
1. V4→V3 paths settle without CurrencyNotSettled (forward token taken from PM, transferred to V3)
2. V3→V4 paths settle without CurrencyNotSettled (forward token transferred to PM, synced/settled)

Key difference from tstore_executor: No auto-pay — WETH payment to V3 pool
is an explicit ERC20_TRANSFER command in the forward_data for V3's callback.
"""

import eth_abi
import pytest
from ape.api.accounts import TestAccountAPI
from ape.contracts.base import ContractInstance
from ape.managers.project import ProjectManager
from ape_test.accounts import TestAccount
from eth_utils import keccak
from eth_utils.address import to_checksum_address
from hexbytes import HexBytes

NATIVE_ADDRESS = to_checksum_address("0x0000000000000000000000000000000000000000")
ZERO_ADDRESS = to_checksum_address("0x0000000000000000000000000000000000000000")

WETH_DEPLOYMENT_WRAP_AMOUNT = 10 * 10**18

MIN_SQRT_PRICE_X96 = 4295128739
MAX_SQRT_PRICE_X96 = 1461446703485210103287273052203988822378723970342

# Command opcodes (same as test_cmd_executor_v4v4)
CMD_V2_SWAP = b"\x00"
CMD_V3_SWAP = b"\x01"
CMD_V4_SWAP = b"\x02"
CMD_V4_TAKE = b"\x03"
CMD_V4_SYNC = b"\x04"
CMD_V4_SETTLE = b"\x05"
CMD_V4_SETTLE_NATIVE = b"\x06"
CMD_ERC20_TRANSFER = b"\x07"
CMD_WETH_DEPOSIT = b"\x08"
CMD_WETH_WITHDRAW = b"\x09"
CMD_V4_UNLOCK = b"\x0A"
SEP = b"\xff"


# ── Encoding helpers (duplicated from test_cmd_executor_v4v4 to keep files independent) ──


def _e(v: int, n: int = 32, signed: bool = False) -> bytes:
    """Encode an integer as n big-endian bytes."""
    return v.to_bytes(n, "big", signed=signed)


def enc_v4_swap(c0_idx, c1_idx, fee, tick_spacing, hooks_idx, zfo, amount_specified, sqrt_limit):
    return b"".join([CMD_V4_SWAP, _e(c0_idx, 1), _e(c1_idx, 1), _e(fee, 3), _e(tick_spacing, 3, signed=True), _e(hooks_idx, 1), b"\x01" if zfo else b"\x00", _e(amount_specified, 32, signed=True), _e(sqrt_limit, 20), SEP])


def enc_v4_take(currency_idx, recipient_idx, amount):
    return b"".join([CMD_V4_TAKE, _e(currency_idx, 1), _e(recipient_idx, 1), _e(amount), SEP])


def enc_v4_sync(currency_idx):
    return b"".join([CMD_V4_SYNC, _e(currency_idx, 1), SEP])


def enc_v4_settle():
    return b"".join([CMD_V4_SETTLE, SEP])


def enc_v4_settle_native(amount):
    return b"".join([CMD_V4_SETTLE_NATIVE, _e(amount), SEP])


def enc_erc20_transfer(token_idx, recipient_idx, amount):
    return b"".join([CMD_ERC20_TRANSFER, _e(token_idx, 1), _e(recipient_idx, 1), _e(amount), SEP])


def enc_weth_deposit(amount):
    return b"".join([CMD_WETH_DEPOSIT, _e(amount), SEP])


def enc_v4_unlock(pm_idx, forward_data):
    return b"".join([CMD_V4_UNLOCK, _e(pm_idx, 1), _e(len(forward_data), 2), forward_data, SEP])


def enc_v3_swap(pool_idx, zfo, amount_specified, sqrt_limit, recipient_idx, forward_data=b""):
    return b"".join([CMD_V3_SWAP, _e(pool_idx, 1), b"\x01" if zfo else b"\x00", _e(amount_specified, 32, signed=True), _e(sqrt_limit, 20), _e(recipient_idx, 1), _e(len(forward_data), 2), forward_data, SEP])


def enc_v2_swap(pool_idx, zfo, amount_out, recipient_idx, forward_data=b""):
    return b"".join([CMD_V2_SWAP, _e(pool_idx, 1), b"\x01" if zfo else b"\x00", _e(amount_out), _e(recipient_idx, 1), _e(len(forward_data), 2), forward_data, SEP])


def _make_pool_key(currency0, currency1, fee=0, tick_spacing=60, hooks=ZERO_ADDRESS):
    c0, c1 = sorted([currency0, currency1], key=lambda addr: addr.lower())
    return (c0, c1, fee, tick_spacing, hooks)


def _setup_v4_swap(pool_manager, owner, pool_key, amount_in, amount_out, zfo, output_token=None, fund_eth=False):
    if fund_eth:
        pool_manager.balance += amount_out
    elif output_token is not None:
        output_token.mint(pool_manager.address, amount_out, sender=owner)
    pool_manager.set_next_swap(pool_key, amount_in, amount_out, zfo, b"", sender=owner)


class AddressTable:
    def __init__(self):
        self._addresses = []
        self._index_map = {}

    def add(self, addr):
        if addr in self._index_map:
            return self._index_map[addr]
        idx = len(self._addresses)
        self._addresses.append(addr)
        self._index_map[addr] = idx
        return idx

    def to_list(self):
        return list(self._addresses)


# ── Fixtures ──


@pytest.fixture
def usdc(project, owner_account):
    return project.fake_erc20.deploy("Fake USD Coin", "USDC", 6, 100_000_000, sender=owner_account)


@pytest.fixture
def weth(project, owner_account):
    return project.fake_weth.deploy("Fake Wrapped Ether", "WETH", 18, 100_000_000, sender=owner_account)


@pytest.fixture
def owner_account(accounts):
    return accounts[0]


@pytest.fixture
def v4_pool_manager(project, owner_account, usdc, weth):
    token0, token1 = sorted([usdc.address, weth.address], key=lambda addr: addr.lower())
    return project.fake_uniswap_v4_pool_manager.deploy(token0, token1, sender=owner_account)


@pytest.fixture
def v3_pool(project, owner_account, usdc, weth):
    """Uniswap V3 pool (callback variant 0 = uniswapV3SwapCallback)."""
    token0, token1 = sorted([usdc.address, weth.address], key=lambda addr: addr.lower())
    return project.fake_uniswap_v3_pool.deploy(token0, token1, 0, sender=owner_account)


@pytest.fixture
def executor(project, owner_account, weth):
    contract = project.cmd_executor.deploy(weth.address, value=WETH_DEPLOYMENT_WRAP_AMOUNT, sender=owner_account)
    contract.balance = 1000 * 10**18
    return contract


# ── Tests ──


class TestV4ToV3:
    """V4→V3 paths: V4 swap then V3 swap within the unlock callback."""

    def test_v4_v3_usdc_to_weth(
        self,
        usdc,
        weth,
        owner_account,
        executor,
        v4_pool_manager,
        v3_pool,
    ):
        """
        V4 swap: WETH→USDC (PM sends USDC)
        then take USDC from PM → V3 swap: USDC→WETH (V3 sends WETH)
        then settle WETH to PM.

        V3 callback's forward_data: explicit ERC20_TRANSFER of USDC to V3 pool.
        After V3 swap returns: sync WETH, transfer WETH to PM, settle.
        """
        pool_a_amount_in = 1 * 10**18  # 1 WETH in to V4
        pool_a_amount_out = 2000 * 10**6  # 2000 USDC out from V4

        v3_amount_in = 2000 * 10**6  # 2000 USDC in to V3
        v3_amount_out = 1 * 10**18  # 1 WETH out from V3 (same as V4 input)

        # ── Set up V4 swap (WETH→USDC) ──
        pool_a_key = _make_pool_key(weth.address, usdc.address, fee=3000, tick_spacing=60)
        pool_a_zfo = pool_a_key[0] == weth.address

        _setup_v4_swap(
            v4_pool_manager, owner_account, pool_a_key,
            pool_a_amount_in, pool_a_amount_out, pool_a_zfo,
            output_token=usdc,
        )

        # ── Set up V3 swap (USDC→WETH) ──
        # USDC→WETH: selling USDC, buying WETH
        # If USDC is token0: zero_for_one = True (selling token0)
        # If WETH is token0: zero_for_one = False (selling token1)
        v3_zfo = v3_pool.token0() == usdc.address

        # Fund V3 pool with the output token (WETH for USDC→WETH path)
        # MUST mint before set_next_swap, which checks pool balance
        weth.mint(v3_pool.address, v3_amount_out, sender=owner_account)
        v3_pool.set_next_swap(
            v3_amount_in, v3_amount_out, v3_zfo, sender=owner_account
        )

        # ── Build address table ──
        at = AddressTable()
        pm_idx = at.add(v4_pool_manager.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        v3_idx = at.add(v3_pool.address)
        zero_idx = at.add(ZERO_ADDRESS)
        native_idx = at.add(NATIVE_ADDRESS)

        # ── Build inner command stream (inside unlockCallback) ──
        inner = b""

        # V4 swap: WETH→USDC
        inner += enc_v4_swap(
            c0_idx=weth_idx if pool_a_key[0] == weth.address else usdc_idx,
            c1_idx=usdc_idx if pool_a_key[1] == usdc.address else weth_idx,
            fee=pool_a_key[2],
            tick_spacing=pool_a_key[3],
            hooks_idx=zero_idx,
            zfo=pool_a_zfo,
            amount_specified=-pool_a_amount_in,
            sqrt_limit=MIN_SQRT_PRICE_X96 + 1 if pool_a_zfo else MAX_SQRT_PRICE_X96 - 1,
        )

        # Take USDC from PM to executor
        inner += enc_v4_take(usdc_idx, executor_idx, pool_a_amount_out)

        # V3 swap: USDC→WETH
        # V3 callback forward_data: transfer USDC to V3 pool (explicit payment)
        v3_callback_cmds = enc_erc20_transfer(usdc_idx, v3_idx, v3_amount_in)

        inner += enc_v3_swap(
            pool_idx=v3_idx,
            zfo=v3_zfo,
            amount_specified=v3_amount_in,  # V3: positive = exact-input
            sqrt_limit=MIN_SQRT_PRICE_X96 + 1 if v3_zfo else MAX_SQRT_PRICE_X96 - 1,
            recipient_idx=executor_idx,
            forward_data=v3_callback_cmds,
        )

        # After V3 swap returns: settle WETH to PM
        inner += enc_v4_sync(weth_idx)
        inner += enc_erc20_transfer(weth_idx, pm_idx, v3_amount_out)
        inner += enc_v4_settle()

        # ── Build outer command stream ──
        commands = enc_v4_unlock(pm_idx, inner)

        # ── Execute ──
        tx = executor.execute(
            at.to_list(),
            commands,
            0,
            True,
            sender=owner_account,
            raise_on_revert=False,
        )

        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")


class TestV3ToV4:
    """V3→V4 paths: V3 swap first, then forward token to PM + unlock + V4 swap."""

    def test_v3_v4_weth_to_usdc(
        self,
        usdc,
        weth,
        owner_account,
        executor,
        v4_pool_manager,
        v3_pool,
    ):
        """
        V3 swap: WETH→USDC (V3 sends USDC to executor)
        then sync USDC at PM, transfer USDC to PM, unlock PM
        inside unlockCallback: settle USDC, V4 swap USDC→WETH, take WETH profit.

        V3 callback's forward_data: explicit WETH transfer to V3 pool.
        """
        v3_amount_in = 1 * 10**18  # 1 WETH in to V3
        v3_amount_out = 2000 * 10**6  # 2000 USDC out from V3

        pool_b_amount_in = 2000 * 10**6  # 2000 USDC in to V4
        pool_b_amount_out = 2 * 10**18  # 2 WETH out from V4 (profitable)

        # ── Set up V3 swap (WETH→USDC) ──
        # WETH→USDC: selling WETH, buying USDC
        v3_zfo = v3_pool.token0() == weth.address  # True if WETH is token0

        # Fund V3 pool with output token BEFORE set_next_swap (which checks balance)
        usdc.mint(v3_pool.address, v3_amount_out, sender=owner_account)
        v3_pool.set_next_swap(
            v3_amount_in, v3_amount_out, v3_zfo, sender=owner_account
        )

        # ── Set up V4 swap (USDC→WETH) ──
        pool_b_key = _make_pool_key(weth.address, usdc.address, fee=3000, tick_spacing=60)
        pool_b_zfo = pool_b_key[0] == usdc.address  # True if USDC is c0 (selling c0)

        _setup_v4_swap(
            v4_pool_manager, owner_account, pool_b_key,
            pool_b_amount_in, pool_b_amount_out, pool_b_zfo,
            output_token=weth,
        )

        # ── Build address table ──
        at = AddressTable()
        pm_idx = at.add(v4_pool_manager.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        v3_idx = at.add(v3_pool.address)
        zero_idx = at.add(ZERO_ADDRESS)

        # ── Build outer command stream ──
        # V3 swap first, with forward_data for V3 callback (pay WETH to V3)
        v3_callback_cmds = enc_erc20_transfer(weth_idx, v3_idx, v3_amount_in)

        outer = enc_v3_swap(
            pool_idx=v3_idx,
            zfo=v3_zfo,
            amount_specified=v3_amount_in,  # V3: positive = exact-input
            sqrt_limit=MIN_SQRT_PRICE_X96 + 1 if v3_zfo else MAX_SQRT_PRICE_X96 - 1,
            recipient_idx=executor_idx,
            forward_data=v3_callback_cmds,
        )

        # After V3 swap: executor has 2000 USDC.
        # All V4 operations must happen inside unlockCallback where msg.sender = PM.
        # So sync, transfer, settle, swap, and take all go in the inner stream.

        # ── Build inner command stream (inside unlockCallback) ──
        inner = b""
        # sync USDC first, then transfer (PM will see balance increase), then settle
        inner += enc_v4_sync(usdc_idx)
        inner += enc_erc20_transfer(usdc_idx, pm_idx, pool_b_amount_in)
        inner += enc_v4_settle()

        # V4 swap: USDC→WETH
        inner += enc_v4_swap(
            c0_idx=weth_idx if pool_b_key[0] == weth.address else usdc_idx,
            c1_idx=usdc_idx if pool_b_key[1] == usdc.address else weth_idx,
            fee=pool_b_key[2],
            tick_spacing=pool_b_key[3],
            hooks_idx=zero_idx,
            zfo=pool_b_zfo,
            amount_specified=-pool_b_amount_in,  # V4: negative = exact-input
            sqrt_limit=MIN_SQRT_PRICE_X96 + 1 if pool_b_zfo else MAX_SQRT_PRICE_X96 - 1,
        )

        # Take all WETH from V4 swap (delta goes to zero)
        inner += enc_v4_take(weth_idx, executor_idx, pool_b_amount_out)

        # ── V4 unlock with inner commands ──
        outer += enc_v4_unlock(pm_idx, inner)

        # ── Execute ──
        tx = executor.execute(
            at.to_list(),
            outer,
            0,  # bribe_bips
            True,  # skip_profit_check
            sender=owner_account,
            raise_on_revert=False,
        )

        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")
