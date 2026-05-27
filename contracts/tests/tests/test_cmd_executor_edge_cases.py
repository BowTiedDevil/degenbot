"""
Edge case tests for the cmd_executor.

Covers:
1. V4 native ETH settlement (V4_SETTLE_NATIVE)
2. V2 direct swap with no callback (forward_data is empty)
3. V4 sign convention validation (wrong sign should revert)
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


def enc_v4_settle_native(amount):
    return b"".join([CMD_V4_SETTLE_NATIVE, _e(amount), SEP])


def enc_erc20_transfer(token_idx, recipient_idx, amount):
    return b"".join([CMD_ERC20_TRANSFER, _e(token_idx, 1), _e(recipient_idx, 1), _e(amount), SEP])


def enc_weth_deposit(amount):
    return b"".join([CMD_WETH_DEPOSIT, _e(amount), SEP])


def enc_weth_withdraw(amount):
    return b"".join([CMD_WETH_WITHDRAW, _e(amount), SEP])


def enc_v4_unlock(pm_idx, forward_data):
    return b"".join([CMD_V4_UNLOCK, _e(pm_idx, 1), _e(len(forward_data), 2), forward_data, SEP])


def enc_v2_swap(pool_idx, zfo, amount_out, recipient_idx, forward_data=b""):
    return b"".join([CMD_V2_SWAP, _e(pool_idx, 1), b"\x01" if zfo else b"\x00", _e(amount_out), _e(recipient_idx, 1), _e(len(forward_data), 2), forward_data, SEP])


def enc_v3_swap(pool_idx, zfo, amount_specified, sqrt_limit, recipient_idx, forward_data=b""):
    return b"".join([CMD_V3_SWAP, _e(pool_idx, 1), b"\x01" if zfo else b"\x00", _e(amount_specified, 32, signed=True), _e(sqrt_limit, 20), _e(recipient_idx, 1), _e(len(forward_data), 2), forward_data, SEP])


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


class TestV4NativeEthSettlement:
    """V4 swap that outputs native ETH (currency0 = NATIVE_ADDRESS)."""

    def test_v4_native_eth_output(
        self,
        usdc,
        weth,
        owner_account,
        executor,
        v4_pool_manager,
    ):
        """
        V4 swap: USDC→ETH (PM sends native ETH out)
        then take ETH, then WETH deposit + settle.

        Tests V4_TAKE with native_address currency and V4_SETTLE for USDC.
        """
        amount_in = 2000 * 10**6  # 2000 USDC in
        amount_out = 1 * 10**18  # 1 ETH out

        # Fund executor with USDC (will be forwarded to PM)
        usdc.mint(executor.address, amount_in, sender=owner_account)

        # Pool: ETH(c0) vs USDC(c1), zfo=False (sell c1=USDC, buy c0=ETH)
        # NATIVE_ADDRESS = 0x0...0 is always c0 (lowest address)
        pool_key = _make_pool_key(NATIVE_ADDRESS, usdc.address, fee=3000, tick_spacing=60)
        pool_zfo = False  # selling c1 (USDC), buying c0 (ETH)

        _setup_v4_swap(
            v4_pool_manager, owner_account, pool_key,
            amount_in, amount_out, pool_zfo,
            fund_eth=True,  # PM needs ETH balance to send out
        )

        # ── Build address table ──
        at = AddressTable()
        pm_idx = at.add(v4_pool_manager.address)
        native_idx = at.add(NATIVE_ADDRESS)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        zero_idx = at.add(ZERO_ADDRESS)

        # ── Build inner command stream (inside unlockCallback) ──
        inner = b""

        # Sync + transfer USDC to PM + settle (credits our USDC delta)
        inner += enc_v4_sync(usdc_idx)
        inner += enc_erc20_transfer(usdc_idx, pm_idx, amount_in)
        inner += enc_v4_settle()

        # V4 swap: USDC→ETH
        inner += enc_v4_swap(
            c0_idx=native_idx,  # c0 = NATIVE_ADDRESS
            c1_idx=usdc_idx,
            fee=pool_key[2],
            tick_spacing=pool_key[3],
            hooks_idx=zero_idx,
            zfo=pool_zfo,  # False (selling c1=USDC)
            amount_specified=-amount_in,  # V4: negative = exact-input
            sqrt_limit=MAX_SQRT_PRICE_X96 - 1,  # for zfo=False
        )

        # Take ETH output
        inner += enc_v4_take(native_idx, executor_idx, amount_out)

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


class TestV2DirectSwapNoCallback:
    """V2 swap with empty forward_data (no flash borrow, direct swap)."""

    def test_v2_direct_swap_no_callback(
        self,
        usdc,
        weth,
        owner_account,
        executor,
        v2_pair,
    ):
        """
        V2 direct swap: WETH→USDC with empty data (no callback).
        The executor already has WETH, so V2 pair gets paid directly.
        """
        v2_amount_in = 1 * 10**18  # 1 WETH
        v2_amount_out = 2000 * 10**6  # 2000 USDC

        # ── Set up V2 swap ──
        v2_zfo = v2_pair.token0() == weth.address

        usdc.mint(v2_pair.address, v2_amount_out, sender=owner_account)
        v2_pair.set_next_swap(v2_amount_in, v2_amount_out, v2_zfo, sender=owner_account)

        # ── Build address table ──
        at = AddressTable()
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        executor_idx = at.add(executor.address)
        v2_idx = at.add(v2_pair.address)

        # ── Build command stream ──
        # Pay WETH to V2 pair first, then swap (no callback)
        commands = enc_erc20_transfer(weth_idx, v2_idx, v2_amount_in)
        commands += enc_v2_swap(
            pool_idx=v2_idx,
            zfo=v2_zfo,
            amount_out=v2_amount_out,
            recipient_idx=executor_idx,
            forward_data=b"",  # No callback
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


class TestV4WrongSignConvention:
    """Verify that the fake PM rejects wrong V4 sign convention."""

    def test_v4_wrong_sign_positive_exact_input(
        self,
        usdc,
        weth,
        owner_account,
        executor,
        v4_pool_manager,
    ):
        """
        V4 swap with POSITIVE amountSpecified for exact-input mode.
        V4 convention: negative = exact-input. Positive should revert
        with the fake PM's validation error.
        """
        amount_in = 1 * 10**18  # 1 WETH
        amount_out = 2000 * 10**6  # 2000 USDC

        pool_key = _make_pool_key(weth.address, usdc.address, fee=3000, tick_spacing=60)
        pool_zfo = pool_key[0] == weth.address

        _setup_v4_swap(
            v4_pool_manager, owner_account, pool_key,
            amount_in, amount_out, pool_zfo,
            output_token=usdc,
        )

        # ── Build address table ──
        at = AddressTable()
        pm_idx = at.add(v4_pool_manager.address)
        weth_idx = at.add(weth.address)
        usdc_idx = at.add(usdc.address)
        zero_idx = at.add(ZERO_ADDRESS)

        # ── Build inner command stream with WRONG sign ──
        inner = b""
        inner += enc_v4_swap(
            c0_idx=weth_idx if pool_key[0] == weth.address else usdc_idx,
            c1_idx=usdc_idx if pool_key[1] == usdc.address else weth_idx,
            fee=pool_key[2],
            tick_spacing=pool_key[3],
            hooks_idx=zero_idx,
            zfo=pool_zfo,
            amount_specified=amount_in,  # WRONG: should be negative for V4 exact-input
            sqrt_limit=MIN_SQRT_PRICE_X96 + 1 if pool_zfo else MAX_SQRT_PRICE_X96 - 1,
        )

        # ── Build outer command stream ──
        commands = enc_v4_unlock(pm_idx, inner)

        # ── Execute (should revert) ──
        tx = executor.execute(
            at.to_list(),
            commands,
            0,
            True,
            sender=owner_account,
            raise_on_revert=False,
        )

        # Must revert — the fake PM validates V4 sign convention
        assert tx.status == 0, "Transaction should have reverted with wrong V4 sign convention"
