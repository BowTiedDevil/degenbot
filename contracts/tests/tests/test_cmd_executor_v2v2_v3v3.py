"""
Tests for cmd_executor same-protocol swap execution (V3-V3, V2-V2).

These paths don't involve V4 at all — commands run in the outer context.
The key challenge is nested callbacks (e.g., V3 calling V3 with its own
forward_data, which results in nested uniswapV3SwapCallback calls).
"""

import pytest
from eth_utils.address import to_checksum_address

NATIVE_ADDRESS = to_checksum_address("0x0000000000000000000000000000000000000000")
ZERO_ADDRESS = to_checksum_address("0x0000000000000000000000000000000000000000")

WETH_DEPLOYMENT_WRAP_AMOUNT = 10 * 10**18

MIN_SQRT_PRICE_X96 = 4295128739
MAX_SQRT_PRICE_X96 = 1461446703485210103287273052203988822378723970342

# Command opcodes
CMD_V2_SWAP = b"\x00"
CMD_V3_SWAP = b"\x01"
CMD_ERC20_TRANSFER = b"\x07"
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
def v3_pool_a(project, owner_account, usdc, weth):
    token0, token1 = sorted([usdc.address, weth.address], key=lambda addr: addr.lower())
    return project.fake_uniswap_v3_pool.deploy(token0, token1, 0, sender=owner_account)


@pytest.fixture
def v3_pool_b(project, owner_account, usdc, weth):
    """Second V3 pool for same-protocol two-hop."""
    token0, token1 = sorted([usdc.address, weth.address], key=lambda addr: addr.lower())
    return project.fake_uniswap_v3_pool.deploy(token0, token1, 1, sender=owner_account)  # callback variant 1


@pytest.fixture
def v2_pair_a(project, owner_account, usdc, weth):
    token0, token1 = sorted([usdc.address, weth.address], key=lambda addr: addr.lower())
    return project.fake_uniswap_v2_pair.deploy(token0, token1, 0, sender=owner_account)


@pytest.fixture
def v2_pair_b(project, owner_account, usdc, weth):
    """Second V2 pair for same-protocol two-hop."""
    token0, token1 = sorted([usdc.address, weth.address], key=lambda addr: addr.lower())
    return project.fake_uniswap_v2_pair.deploy(token0, token1, 1, sender=owner_account)  # callback variant 1 = hook


@pytest.fixture
def executor(project, owner_account, weth):
    contract = project.cmd_executor.deploy(weth.address, value=WETH_DEPLOYMENT_WRAP_AMOUNT, sender=owner_account)
    contract.balance = 1000 * 10**18
    return contract


# ── Tests ──


class TestV3V3:
    """V3→V3 same-protocol path with nested callback."""

    def test_v3_v3_nested_callback(
        self,
        usdc,
        weth,
        owner_account,
        executor,
        v3_pool_a,
        v3_pool_b,
    ):
        """
        V3 pool A swap: WETH→USDC
        then V3 pool B swap: USDC→WETH (in pool A callback, which pays WETH)
        V3 pool B callback: pays USDC to pool B.

        This tests that nested V3 callbacks work — pool A's callback calls
        pool B, whose callback processes its own forward_data, then control
        returns to pool A's callback which pays pool A the WETH it's owed.
        """
        pool_a_amount_in = 1 * 10**18  # 1 WETH in to pool A
        pool_a_amount_out = 2000 * 10**6  # 2000 USDC out from pool A

        pool_b_amount_in = 2000 * 10**6  # 2000 USDC in to pool B
        pool_b_amount_out = 1 * 10**18  # 1 WETH out from pool B

        # ── Set up V3 pool A (WETH→USDC) ──
        pool_a_zfo = v3_pool_a.token0() == weth.address

        usdc.mint(v3_pool_a.address, pool_a_amount_out, sender=owner_account)
        v3_pool_a.set_next_swap(pool_a_amount_in, pool_a_amount_out, pool_a_zfo, sender=owner_account)

        # ── Set up V3 pool B (USDC→WETH) ──
        pool_b_zfo = v3_pool_b.token0() == usdc.address

        weth.mint(v3_pool_b.address, pool_b_amount_out, sender=owner_account)
        v3_pool_b.set_next_swap(pool_b_amount_in, pool_b_amount_out, pool_b_zfo, sender=owner_account)

        # ── Build address table ──
        at = AddressTable()
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        v3a_idx = at.add(v3_pool_a.address)
        v3b_idx = at.add(v3_pool_b.address)

        # ── Build command stream ──
        # V3 pool B callback: pay USDC to pool B
        pool_b_callback_cmds = enc_erc20_transfer(usdc_idx, v3b_idx, pool_b_amount_in)

        # V3 pool A callback: pay WETH to pool A, then V3 pool B swap
        pool_a_callback_cmds = enc_erc20_transfer(weth_idx, v3a_idx, pool_a_amount_in)
        pool_a_callback_cmds += enc_v3_swap(
            pool_idx=v3b_idx,
            zfo=pool_b_zfo,
            amount_specified=pool_b_amount_in,  # V3: positive = exact-input
            sqrt_limit=MIN_SQRT_PRICE_X96 + 1 if pool_b_zfo else MAX_SQRT_PRICE_X96 - 1,
            recipient_idx=executor_idx,
            forward_data=pool_b_callback_cmds,
        )

        # Outer: V3 pool A swap
        commands = enc_v3_swap(
            pool_idx=v3a_idx,
            zfo=pool_a_zfo,
            amount_specified=pool_a_amount_in,
            sqrt_limit=MIN_SQRT_PRICE_X96 + 1 if pool_a_zfo else MAX_SQRT_PRICE_X96 - 1,
            recipient_idx=executor_idx,
            forward_data=pool_a_callback_cmds,
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


class TestV2V2:
    """V2→V2 same-protocol path with nested callback."""

    def test_v2_v2_nested_callback(
        self,
        usdc,
        weth,
        owner_account,
        executor,
        v2_pair_a,
        v2_pair_b,
    ):
        """
        V2 pair A swap: WETH→USDC (flash borrow)
        then V2 pair B swap: USDC→WETH (in pair A callback)
        V2 pair B callback: pay USDC to pair B
        V2 pair A callback (continued after pair B): pay WETH to pair A.

        Tests nested V2 callbacks (different callback variants: uniswapV2Call + hook).
        """
        pair_a_amount_in = 1 * 10**18  # 1 WETH in to pair A
        pair_a_amount_out = 2000 * 10**6  # 2000 USDC out from pair A

        pair_b_amount_in = 2000 * 10**6  # 2000 USDC in to pair B
        pair_b_amount_out = 1 * 10**18  # 1 WETH out from pair B

        # ── Set up V2 pair A (WETH→USDC) ──
        pair_a_zfo = v2_pair_a.token0() == weth.address

        usdc.mint(v2_pair_a.address, pair_a_amount_out, sender=owner_account)
        v2_pair_a.set_next_swap(pair_a_amount_in, pair_a_amount_out, pair_a_zfo, sender=owner_account)

        # ── Set up V2 pair B (USDC→WETH) ──
        pair_b_zfo = v2_pair_b.token0() == usdc.address

        weth.mint(v2_pair_b.address, pair_b_amount_out, sender=owner_account)
        v2_pair_b.set_next_swap(pair_b_amount_in, pair_b_amount_out, pair_b_zfo, sender=owner_account)

        # ── Build address table ──
        at = AddressTable()
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        v2a_idx = at.add(v2_pair_a.address)
        v2b_idx = at.add(v2_pair_b.address)

        # ── Build command stream ──
        # V2 pair B callback: pay USDC to pair B
        pair_b_callback_cmds = enc_erc20_transfer(usdc_idx, v2b_idx, pair_b_amount_in)

        # V2 pair A callback: V2 pair B swap, then pay WETH to pair A
        pair_a_callback_cmds = enc_v2_swap(
            pool_idx=v2b_idx,
            zfo=pair_b_zfo,
            amount_out=pair_b_amount_out,
            recipient_idx=executor_idx,
            forward_data=pair_b_callback_cmds,
        )
        # After pair B swap returns: pay WETH to pair A
        pair_a_callback_cmds += enc_erc20_transfer(weth_idx, v2a_idx, pair_a_amount_in)

        # Outer: V2 pair A swap with flash borrow
        commands = enc_v2_swap(
            pool_idx=v2a_idx,
            zfo=pair_a_zfo,
            amount_out=pair_a_amount_out,
            recipient_idx=executor_idx,
            forward_data=pair_a_callback_cmds,
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
