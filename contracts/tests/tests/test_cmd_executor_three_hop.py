"""
Tests for cmd_executor three-hop swap execution.

Three-hop paths involve three pools across two unlocked contexts
(V4 and outer), with callbacks chaining through V2, V3, and V4.
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


def enc_v4_swap(c0_idx, c1_idx, fee, tick_spacing, hooks_idx, zfo, amount_specified, sqrt_limit):
    return b"".join([CMD_V4_SWAP, _e(c0_idx, 1), _e(c1_idx, 1), _e(fee, 3), _e(tick_spacing, 3, signed=True), _e(hooks_idx, 1), b"\x01" if zfo else b"\x00", _e(amount_specified, 32, signed=True), _e(sqrt_limit, 20), SEP])


def enc_v4_take(currency_idx, recipient_idx, amount):
    return b"".join([CMD_V4_TAKE, _e(currency_idx, 1), _e(recipient_idx, 1), _e(amount), SEP])


def enc_v4_sync(currency_idx):
    return b"".join([CMD_V4_SYNC, _e(currency_idx, 1), SEP])


def enc_v4_settle():
    return b"".join([CMD_V4_SETTLE, SEP])


def enc_erc20_transfer(token_idx, recipient_idx, amount):
    return b"".join([CMD_ERC20_TRANSFER, _e(token_idx, 1), _e(recipient_idx, 1), _e(amount), SEP])


def enc_v4_unlock(pm_idx, forward_data):
    return b"".join([CMD_V4_UNLOCK, _e(pm_idx, 1), _e(len(forward_data), 2), forward_data, SEP])


def enc_v3_swap(pool_idx, zfo, amount_specified, sqrt_limit, recipient_idx, forward_data=b""):
    return b"".join([CMD_V3_SWAP, _e(pool_idx, 1), b"\x01" if zfo else b"\x00", _e(amount_specified, 32, signed=True), _e(sqrt_limit, 20), _e(recipient_idx, 1), _e(len(forward_data), 2), forward_data, SEP])


def enc_v2_swap(pool_idx, zfo, amount_out, recipient_idx, forward_data=b""):
    return b"".join([CMD_V2_SWAP, _e(pool_idx, 1), b"\x01" if zfo else b"\x00", _e(amount_out), _e(recipient_idx, 1), _e(len(forward_data), 2), forward_data, SEP])


def _make_pool_key(currency0, currency1, fee=0, tick_spacing=60, hooks=ZERO_ADDRESS):
    c0, c1 = sorted([currency0, currency1], key=lambda addr: addr.lower())
    return (c0, c1, fee, tick_spacing, hooks)


def _setup_v4_swap(pool_manager, owner, pool_key, amount_in, amount_out, zfo, output_token=None):
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


class TestThreeHopHybrid:
    """Three-hop paths involving V2, V3, and V4."""

    def test_v4_v3_v2_three_hop(
        self,
        usdc,
        weth,
        owner_account,
        executor,
        v4_pool_manager,
        v3_pool,
        v2_pair,
    ):
        """
        Three-hop: V4 (WETH→USDC) → V3 (USDC→WETH) → V2 (WETH→USDC)

        V4 swap inside unlock, take USDC, V3 swap (callback pays USDC),
        V2 swap (callback pays WETH), then settle WETH to PM.

        Flow:
        1. V4 swap: WETH→USDC
        2. V4 take USDC to executor
        3. V3 swap: USDC→WETH (callback: pay USDC to V3)
        4. V2 swap: WETH→USDC (callback: pay WETH to V2)
        5. V4 sync WETH, transfer WETH to PM, settle
        """
        v4_amount_in = 1 * 10**18  # 1 WETH in to V4
        v4_amount_out = 2000 * 10**6  # 2000 USDC out from V4

        v3_amount_in = 2000 * 10**6  # 2000 USDC in to V3
        v3_amount_out = 1 * 10**18  # 1 WETH out from V3

        v2_amount_in = 1 * 10**18  # 1 WETH in to V2
        v2_amount_out = 2000 * 10**6  # 2000 USDC out from V2 (profitable)

        # ── Set up V4 swap (WETH→USDC) ──
        pool_a_key = _make_pool_key(weth.address, usdc.address, fee=3000, tick_spacing=60)
        pool_a_zfo = pool_a_key[0] == weth.address

        _setup_v4_swap(
            v4_pool_manager, owner_account, pool_a_key,
            v4_amount_in, v4_amount_out, pool_a_zfo,
            output_token=usdc,
        )

        # ── Set up V3 swap (USDC→WETH) ──
        v3_zfo = v3_pool.token0() == usdc.address

        weth.mint(v3_pool.address, v3_amount_out, sender=owner_account)
        v3_pool.set_next_swap(v3_amount_in, v3_amount_out, v3_zfo, sender=owner_account)

        # ── Set up V2 swap (WETH→USDC) ──
        v2_zfo = v2_pair.token0() == weth.address

        usdc.mint(v2_pair.address, v2_amount_out, sender=owner_account)
        v2_pair.set_next_swap(v2_amount_in, v2_amount_out, v2_zfo, sender=owner_account)

        # ── Build address table ──
        at = AddressTable()
        pm_idx = at.add(v4_pool_manager.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        v3_idx = at.add(v3_pool.address)
        v2_idx = at.add(v2_pair.address)
        zero_idx = at.add(ZERO_ADDRESS)

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
            amount_specified=-v4_amount_in,
            sqrt_limit=MIN_SQRT_PRICE_X96 + 1 if pool_a_zfo else MAX_SQRT_PRICE_X96 - 1,
        )

        # Take USDC from PM to executor
        inner += enc_v4_take(usdc_idx, executor_idx, v4_amount_out)

        # V2 callback: pay WETH to V2 pair
        v2_callback_cmds = enc_erc20_transfer(weth_idx, v2_idx, v2_amount_in)

        # V3 callback: pay USDC to V3 pool, then V2 swap
        v3_callback_cmds = enc_erc20_transfer(usdc_idx, v3_idx, v3_amount_in)
        v3_callback_cmds += enc_v2_swap(
            pool_idx=v2_idx,
            zfo=v2_zfo,
            amount_out=v2_amount_out,
            recipient_idx=executor_idx,
            forward_data=v2_callback_cmds,
        )

        # V3 swap: USDC→WETH
        inner += enc_v3_swap(
            pool_idx=v3_idx,
            zfo=v3_zfo,
            amount_specified=v3_amount_in,
            sqrt_limit=MIN_SQRT_PRICE_X96 + 1 if v3_zfo else MAX_SQRT_PRICE_X96 - 1,
            recipient_idx=executor_idx,
            forward_data=v3_callback_cmds,
        )

        # After V3+V2 swaps return: settle WETH to PM
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

    def test_v2_v3_v4_three_hop(
        self,
        usdc,
        weth,
        owner_account,
        executor,
        v2_pair,
        v3_pool,
        v4_pool_manager,
    ):
        """
        Three-hop: V2 (WETH→USDC) → V3 (USDC→WETH) → V4 (WETH→USDC)

        Flow:
        1. V2 swap: WETH→USDC (flash borrow, callback: V3 + V2 payment)
        2. V3 swap: USDC→WETH (callback: pay USDC to V3)
        3. After V3 returns: pay WETH to V2 pair
        4. After V2 returns: sync+transfer+settle at PM, V4 swap, take profit
        """
        v2_amount_in = 1 * 10**18  # 1 WETH in to V2
        v2_amount_out = 2000 * 10**6  # 2000 USDC out from V2

        v3_amount_in = 2000 * 10**6  # 2000 USDC in to V3
        v3_amount_out = 1 * 10**18  # 1 WETH out from V3

        v4_amount_in = 1 * 10**18  # 1 WETH in to V4
        v4_amount_out = 2 * 10**18  # 2 WETH out from V4 (profitable)

        # ── Set up V2 swap (WETH→USDC) ──
        v2_zfo = v2_pair.token0() == weth.address

        usdc.mint(v2_pair.address, v2_amount_out, sender=owner_account)
        v2_pair.set_next_swap(v2_amount_in, v2_amount_out, v2_zfo, sender=owner_account)

        # ── Set up V3 swap (USDC→WETH) ──
        v3_zfo = v3_pool.token0() == usdc.address

        weth.mint(v3_pool.address, v3_amount_out, sender=owner_account)
        v3_pool.set_next_swap(v3_amount_in, v3_amount_out, v3_zfo, sender=owner_account)

        # ── Set up V4 swap (WETH→USDC) ──
        pool_key = _make_pool_key(weth.address, usdc.address, fee=3000, tick_spacing=60)
        pool_zfo = pool_key[0] == weth.address  # Selling WETH (c0 if WETH < USDC)

        _setup_v4_swap(
            v4_pool_manager, owner_account, pool_key,
            v4_amount_in, v4_amount_out, pool_zfo,
            output_token=usdc,
        )

        # ── Build address table ──
        at = AddressTable()
        pm_idx = at.add(v4_pool_manager.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        v2_idx = at.add(v2_pair.address)
        v3_idx = at.add(v3_pool.address)
        zero_idx = at.add(ZERO_ADDRESS)

        # ── Build inner command stream (inside unlockCallback) ──
        inner = b""
        # Sync WETH, transfer WETH to PM, settle (credits our delta)
        inner += enc_v4_sync(weth_idx)
        inner += enc_erc20_transfer(weth_idx, pm_idx, v4_amount_in)
        inner += enc_v4_settle()

        # V4 swap: WETH→USDC
        inner += enc_v4_swap(
            c0_idx=weth_idx if pool_key[0] == weth.address else usdc_idx,
            c1_idx=usdc_idx if pool_key[1] == usdc.address else weth_idx,
            fee=pool_key[2],
            tick_spacing=pool_key[3],
            hooks_idx=zero_idx,
            zfo=pool_zfo,
            amount_specified=-v4_amount_in,
            sqrt_limit=MIN_SQRT_PRICE_X96 + 1 if pool_zfo else MAX_SQRT_PRICE_X96 - 1,
        )

        # Take USDC from V4 swap
        inner += enc_v4_take(usdc_idx, executor_idx, v4_amount_out)

        # ── Build V3 callback forward_data ──
        v3_callback_cmds = enc_erc20_transfer(usdc_idx, v3_idx, v3_amount_in)

        # ── Build V2 callback forward_data ──
        # V3 swap + WETH payment to V2 pair + V4 unlock
        v2_callback_cmds = enc_v3_swap(
            pool_idx=v3_idx,
            zfo=v3_zfo,
            amount_specified=v3_amount_in,
            sqrt_limit=MIN_SQRT_PRICE_X96 + 1 if v3_zfo else MAX_SQRT_PRICE_X96 - 1,
            recipient_idx=executor_idx,
            forward_data=v3_callback_cmds,
        )
        # After V3 returns: pay WETH to V2 pair
        v2_callback_cmds += enc_erc20_transfer(weth_idx, v2_idx, v2_amount_in)

        # ── Build outer command stream ──
        outer = enc_v2_swap(
            pool_idx=v2_idx,
            zfo=v2_zfo,
            amount_out=v2_amount_out,
            recipient_idx=executor_idx,
            forward_data=v2_callback_cmds,
        )

        # After V2 returns: V4 unlock with inner commands
        outer += enc_v4_unlock(pm_idx, inner)

        # ── Execute ──
        tx = executor.execute(
            at.to_list(),
            outer,
            0,
            True,
            sender=owner_account,
            raise_on_revert=False,
        )

        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")
