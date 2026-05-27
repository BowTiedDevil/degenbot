"""
Tests for cmd_executor V2-V3 swap execution.

Verifies that:
1. V2→V3 paths: V2 sends intermediate token, V3 callback receives input token
2. V3→V2 paths: V3 sends intermediate token, V2 callback receives input token

No V4 involved — commands run in the outer context.
"""

import pytest
from ape.api.accounts import TestAccountAPI
from ape.contracts.base import ContractInstance
from ape.managers.project import ProjectManager
from ape_test.accounts import TestAccount
from eth_utils.address import to_checksum_address

NATIVE_ADDRESS = to_checksum_address("0x0000000000000000000000000000000000000000")
ZERO_ADDRESS = to_checksum_address("0x0000000000000000000000000000000000000000")

WETH_DEPLOYMENT_WRAP_AMOUNT = 10 * 10**18

MIN_SQRT_PRICE_X96 = 4295128739
MAX_SQRT_PRICE_X96 = 1461446703485210103287273052203988822378723970342

# Command opcodes
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


# ── Encoding helpers ──


def _e(v: int, n: int = 32, signed: bool = False) -> bytes:
    return v.to_bytes(n, "big", signed=signed)


def enc_v3_swap(pool_idx, zfo, amount_specified, sqrt_limit, recipient_idx, forward_data=b""):
    return b"".join([CMD_V3_SWAP, _e(pool_idx, 1), b"\x01" if zfo else b"\x00", _e(amount_specified, 32, signed=True), _e(sqrt_limit, 20), _e(recipient_idx, 1), _e(len(forward_data), 2), forward_data, SEP])


def enc_v2_swap(pool_idx, zfo, amount_out, recipient_idx, forward_data=b""):
    return b"".join([CMD_V2_SWAP, _e(pool_idx, 1), b"\x01" if zfo else b"\x00", _e(amount_out), _e(recipient_idx, 1), _e(len(forward_data), 2), forward_data, SEP])


def enc_erc20_transfer(token_idx, recipient_idx, amount):
    return b"".join([CMD_ERC20_TRANSFER, _e(token_idx, 1), _e(recipient_idx, 1), _e(amount), SEP])


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
def v3_pool(project, owner_account, usdc, weth):
    token0, token1 = sorted([usdc.address, weth.address], key=lambda addr: addr.lower())
    return project.fake_uniswap_v3_pool.deploy(token0, token1, 0, sender=owner_account)


@pytest.fixture
def v2_pair(project, owner_account, usdc, weth):
    token0, token1 = sorted([usdc.address, weth.address], key=lambda addr: addr.lower())
    return project.fake_uniswap_v2_pair.deploy(token0, token1, 0, sender=owner_account)


@pytest.fixture
def executor(project, owner_account, weth):
    contract = project.cmd_executor.deploy(weth.address, value=WETH_DEPLOYMENT_WRAP_AMOUNT, sender=owner_account)
    contract.balance = 1000 * 10**18
    return contract


# ── Tests ──


class TestV2ToV3:
    """V2→V3 paths: V2 flash borrow, then V3 swap in callback."""

    def test_v2_v3_weth_usdc_to_weth(
        self,
        usdc,
        weth,
        owner_account,
        executor,
        v2_pair,
        v3_pool,
    ):
        """
        V2 swap: WETH→USDC (V2 sends USDC to executor via flash borrow)
        then V3 swap: USDC→WETH (V3 sends WETH to executor, callback pays USDC)
        then pay WETH to V2 pair.

        V2 callback forward_data: V3 swap (with its own forward_data for V3 callback)
        V3 callback forward_data: ERC20_TRANSFER of USDC to V3 pool
        After V3 and V2 return: WETH transfer to V2 pair
        """
        v2_amount_in = 1 * 10**18  # 1 WETH in to V2
        v2_amount_out = 2000 * 10**6  # 2000 USDC out from V2

        v3_amount_in = 2000 * 10**6  # 2000 USDC in to V3
        v3_amount_out = 1 * 10**18  # 1 WETH out from V3 (same as V2 input)

        # ── Set up V2 swap (WETH→USDC) ──
        v2_zfo = v2_pair.token0() == weth.address

        usdc.mint(v2_pair.address, v2_amount_out, sender=owner_account)
        v2_pair.set_next_swap(v2_amount_in, v2_amount_out, v2_zfo, sender=owner_account)

        # ── Set up V3 swap (USDC→WETH) ──
        v3_zfo = v3_pool.token0() == usdc.address

        weth.mint(v3_pool.address, v3_amount_out, sender=owner_account)
        v3_pool.set_next_swap(v3_amount_in, v3_amount_out, v3_zfo, sender=owner_account)

        # ── Build address table ──
        at = AddressTable()
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        v2_idx = at.add(v2_pair.address)
        v3_idx = at.add(v3_pool.address)

        # ── Build command stream ──
        # V3 callback forward_data: pay USDC to V3 pool
        v3_callback_cmds = enc_erc20_transfer(usdc_idx, v3_idx, v3_amount_in)

        # V2 callback forward_data: V3 swap + WETH payment to V2 pair
        v2_callback_cmds = enc_v3_swap(
            pool_idx=v3_idx,
            zfo=v3_zfo,
            amount_specified=v3_amount_in,  # V3: positive = exact-input
            sqrt_limit=MIN_SQRT_PRICE_X96 + 1 if v3_zfo else MAX_SQRT_PRICE_X96 - 1,
            recipient_idx=executor_idx,
            forward_data=v3_callback_cmds,
        )
        # After V3 returns: pay WETH to V2 pair
        v2_callback_cmds += enc_erc20_transfer(weth_idx, v2_idx, v2_amount_in)

        # Outer: V2 swap with flash borrow
        commands = enc_v2_swap(
            pool_idx=v2_idx,
            zfo=v2_zfo,
            amount_out=v2_amount_out,
            recipient_idx=executor_idx,
            forward_data=v2_callback_cmds,
        )

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


class TestV3ToV2:
    """V3→V2 paths: V3 swap, then V2 swap in V3 callback."""

    def test_v3_v2_weth_usdc_to_weth(
        self,
        usdc,
        weth,
        owner_account,
        executor,
        v3_pool,
        v2_pair,
    ):
        """
        V3 swap: WETH→USDC (V3 sends USDC, callback pays WETH)
        then V2 swap: USDC→WETH (V2 sends WETH via flash borrow, callback pays USDC)
        then WETH payment to V3 pool (in V3 callback, after V2 returns).

        V3 callback forward_data: WETH transfer to V3 pool, then V2 swap
        V2 callback forward_data: USDC transfer to V2 pair
        """
        v3_amount_in = 1 * 10**18  # 1 WETH in to V3
        v3_amount_out = 2000 * 10**6  # 2000 USDC out from V3

        v2_amount_in = 2000 * 10**6  # 2000 USDC in to V2
        v2_amount_out = 1 * 10**18  # 1 WETH out from V2 (same as V3 input)

        # ── Set up V3 swap (WETH→USDC) ──
        v3_zfo = v3_pool.token0() == weth.address

        usdc.mint(v3_pool.address, v3_amount_out, sender=owner_account)
        v3_pool.set_next_swap(v3_amount_in, v3_amount_out, v3_zfo, sender=owner_account)

        # ── Set up V2 swap (USDC→WETH) ──
        v2_zfo = v2_pair.token0() == usdc.address

        weth.mint(v2_pair.address, v2_amount_out, sender=owner_account)
        v2_pair.set_next_swap(v2_amount_in, v2_amount_out, v2_zfo, sender=owner_account)

        # ── Build address table ──
        at = AddressTable()
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        v2_idx = at.add(v2_pair.address)
        v3_idx = at.add(v3_pool.address)

        # ── Build command stream ──
        # V2 callback forward_data: pay USDC to V2 pair
        v2_callback_cmds = enc_erc20_transfer(usdc_idx, v2_idx, v2_amount_in)

        # V3 callback forward_data: pay WETH to V3 pool, then V2 swap
        v3_callback_cmds = enc_erc20_transfer(weth_idx, v3_idx, v3_amount_in)
        v3_callback_cmds += enc_v2_swap(
            pool_idx=v2_idx,
            zfo=v2_zfo,
            amount_out=v2_amount_out,
            recipient_idx=executor_idx,
            forward_data=v2_callback_cmds,
        )

        # Outer: V3 swap
        commands = enc_v3_swap(
            pool_idx=v3_idx,
            zfo=v3_zfo,
            amount_specified=v3_amount_in,  # V3: positive = exact-input
            sqrt_limit=MIN_SQRT_PRICE_X96 + 1 if v3_zfo else MAX_SQRT_PRICE_X96 - 1,
            recipient_idx=executor_idx,
            forward_data=v3_callback_cmds,
        )

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
