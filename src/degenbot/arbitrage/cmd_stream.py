"""Command-stream encoding for the cmd_executor contract.

Encodes swap operations into the compact command-bytecode format
consumed by the on-chain cmd_executor. Each command is:
  1-byte opcode + tightly-packed parameters

Addresses are referenced by index into a shared address table,
built during the preprocessing section of the command stream.

Command stream format:
  [0xFE][SET_ADDRESS commands][SKIP_PROFIT_CHECK][0xFF][execution commands]

See contracts/cmd_executor.vy for the full command set and encoding.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import TYPE_CHECKING

from eth_utils.address import to_checksum_address

from degenbot.arbitrage.encoding import EncodedCall
from degenbot.arbitrage.types import UniswapV4PoolSwapAmounts

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress

    from degenbot.arbitrage.types import (
        AbstractSwapAmounts,
        UniswapV2PoolSwapAmounts,
        UniswapV3PoolSwapAmounts,
        UniswapV4PoolSwapAmounts,
        V4PoolKey,
    )

# ── Sentinel addresses ──

NATIVE_ADDRESS: ChecksumAddress = to_checksum_address("0x0000000000000000000000000000000000000000")
ZERO_ADDRESS: ChecksumAddress = to_checksum_address("0x0000000000000000000000000000000000000000")

# ── Command opcodes ──

# Control / Preprocessing: 0x00–0x0F
CMD_SET_ADDRESS = b"\x00"
CMD_SKIP_PROFIT_CHECK = b"\x01"
CMD_BRIBE_COINBASE = b"\x02"
CMD_BRIBE_ADDRESS = b"\x03"

# ERC20 / ETH / Native: 0x10–0x1F
CMD_ERC20_TRANSFER = b"\x10"
CMD_ERC20_XFER_BALANCE = b"\x11"
CMD_WETH_DEPOSIT = b"\x12"
CMD_WETH_WITHDRAW = b"\x13"
CMD_WETH_DEPOSIT_ALL = b"\x14"
CMD_WETH_WITHDRAW_ALL = b"\x15"
CMD_SEND_ETH = b"\x16"
CMD_SEND_ETH_ALL = b"\x17"

# V2: 0x20–0x2F
CMD_V2_SWAP_COMPACT = b"\x20"
CMD_V2_SWAP_CALC = b"\x21"
CMD_V2_SWAP_DIRECT = b"\x22"

# V3: 0x30–0x3F
CMD_V3_SWAP_COMPACT = b"\x30"
CMD_V3_SWAP_DELTA = b"\x31"

# V4 Swaps: 0x40–0x4F
CMD_V4_SWAP_COMPACT = b"\x40"
CMD_V4_SWAP_DYNAMIC = b"\x41"
CMD_V4_BATCH = b"\x42"

# V4 Settlement / ERC6909: 0x50–0x5F
CMD_V4_UNLOCK = b"\x50"
CMD_V4_TAKE = b"\x51"
CMD_V4_TAKE_COMPACT = b"\x52"
CMD_V4_TAKE_DELTA = b"\x53"
CMD_V4_SYNC = b"\x54"
# 0x55 = V4_SETTLE
CMD_V4_SETTLE = b"\x55"
CMD_V4_SETTLE_DELTA = b"\x56"
CMD_V4_SETTLE_ALL = b"\x57"
CMD_V4_MINT_COMPACT = b"\x58"
CMD_V4_BURN_COMPACT = b"\x59"

# Stream separators
BEGIN_PREPROCESSING = b"\xfe"
BEGIN_EXECUTION = b"\xff"


# ── Encoding helpers ──


def _e(v: int, n: int = 32, signed: bool = False) -> bytes:
    """Encode an integer as n big-endian bytes."""
    return v.to_bytes(n, "big", signed=signed)


def _address_to_bytes(addr: str | ChecksumAddress) -> bytes:
    """Convert a checksummed address string to 20 raw bytes."""
    addr_str = addr if isinstance(addr, str) else addr
    addr_bytes = bytes.fromhex(addr_str[2:])
    msg = f"Invalid address length: {len(addr_bytes)}"
    assert len(addr_bytes) == 20, msg
    return addr_bytes


# ── Address table ──


class AddressTable:
    """Tracks addresses for compact index-based referencing in the command stream.

    Each address is assigned a sequential index in insertion order.
    The table is built during preprocessing and referenced during execution.
    """

    def __init__(self) -> None:
        self._addresses: list[ChecksumAddress] = []
        self._index_map: dict[ChecksumAddress, int] = {}

    def add(self, addr: str | ChecksumAddress) -> int:
        """Add an address, returning its index. Idempotent for duplicates."""
        if isinstance(addr, str):
            addr = to_checksum_address(addr)
        if addr in self._index_map:
            return self._index_map[addr]
        idx = len(self._addresses)
        self._addresses.append(addr)
        self._index_map[addr] = idx
        return idx

    def index_of(self, addr: str | ChecksumAddress) -> int:
        """Return the index for an address. Raises KeyError if not found."""
        if isinstance(addr, str):
            addr = to_checksum_address(addr)
        return self._index_map[addr]

    def __contains__(self, addr: str | ChecksumAddress) -> bool:
        if isinstance(addr, str):
            addr = to_checksum_address(addr)
        return addr in self._index_map

    @property
    def addresses(self) -> list[ChecksumAddress]:
        return list(self._addresses)


# ── Preprocessing commands ──


def enc_set_address(addr: str | ChecksumAddress) -> bytes:
    """SET_ADDRESS: [0x00][address:20] — 21 bytes."""
    return CMD_SET_ADDRESS + _address_to_bytes(addr)


def enc_set_addresses(address_table: AddressTable) -> bytes:
    """Encode SET_ADDRESS commands for all addresses in the table."""
    result = b""
    for addr in address_table.addresses:
        result += enc_set_address(addr)
    return result


def enc_skip_profit_check() -> bytes:
    """SKIP_PROFIT_CHECK: [0x01] — 1 byte."""
    return CMD_SKIP_PROFIT_CHECK


def enc_preamble(
    address_table: AddressTable,
    skip_profit: bool = True,
) -> bytes:
    """Encode the full preprocessing section + separator.

    Builds: [0xFE][SET_ADDRESS commands][SKIP_PROFIT_CHECK][0xFF]
    """
    preamble = BEGIN_PREPROCESSING + enc_set_addresses(address_table)
    if skip_profit:
        preamble += enc_skip_profit_check()
    preamble += BEGIN_EXECUTION
    return preamble


# ── ERC20 / ETH / Native commands ──


def enc_erc20_transfer(
    token_idx: int,
    recipient_idx: int,
    amount: int,
) -> bytes:
    """ERC20_TRANSFER: [0x10][token_idx:1][recipient_idx:1][amount:32] — 35 bytes."""
    return b"".join([
        CMD_ERC20_TRANSFER,
        _e(token_idx, 1),
        _e(recipient_idx, 1),
        _e(amount),
    ])


def enc_erc20_xfer_balance(
    token_idx: int,
    recipient_idx: int,
) -> bytes:
    """ERC20_XFER_BALANCE: [0x11][token_idx:1][recipient_idx:1] — 3 bytes."""
    return b"".join([
        CMD_ERC20_XFER_BALANCE,
        _e(token_idx, 1),
        _e(recipient_idx, 1),
    ])


def enc_weth_deposit(amount: int) -> bytes:
    """WETH_DEPOSIT: [0x12][amount:32] — 33 bytes."""
    return b"".join([CMD_WETH_DEPOSIT, _e(amount)])


def enc_weth_withdraw(amount: int) -> bytes:
    """WETH_WITHDRAW: [0x13][amount:32] — 33 bytes."""
    return b"".join([CMD_WETH_WITHDRAW, _e(amount)])


def enc_weth_deposit_all() -> bytes:
    """WETH_DEPOSIT_ALL: [0x14] — 1 byte."""
    return CMD_WETH_DEPOSIT_ALL


def enc_weth_withdraw_all() -> bytes:
    """WETH_WITHDRAW_ALL: [0x15] — 1 byte."""
    return CMD_WETH_WITHDRAW_ALL


def enc_send_eth(recipient_idx: int, amount: int) -> bytes:
    """SEND_ETH: [0x16][recipient_idx:1][amount:16] — 18 bytes."""
    return b"".join([CMD_SEND_ETH, _e(recipient_idx, 1), _e(amount, 16)])


def enc_send_eth_all(recipient_idx: int) -> bytes:
    """SEND_ETH_ALL: [0x17][recipient_idx:1] — 2 bytes."""
    return b"".join([CMD_SEND_ETH_ALL, _e(recipient_idx, 1)])


# ── V2 commands ──


def enc_v2_swap_compact(
    pool_idx: int,
    zfo: bool,
    amount_out: int,
    recipient_idx: int,
    fee: int = 30,
    forward_data: bytes = b"",
) -> bytes:
    """V2_SWAP_COMPACT: [0x20][pool_idx:1][zfo:1][amount_out:16]
    [recipient_idx:1][fee:2][fwd_len:2][fwd_data:N] = 24 + N bytes.

    fee is a fraction of 10000 (30 = 0.3% UniswapV2, 25 = 0.25% PancakeSwap).
    Written to t_v2_pair_fee[pool] before swap() for correct auto-pay.
    """
    return b"".join([
        CMD_V2_SWAP_COMPACT,
        _e(pool_idx, 1),
        b"\x01" if zfo else b"\x00",
        _e(amount_out, 16),
        _e(recipient_idx, 1),
        _e(fee, 2),
        _e(len(forward_data), 2),
        forward_data,
    ])


def enc_v2_swap_calc(
    pool_idx: int,
    zfo: bool,
    recipient_idx: int,
    fee: int = 30,
) -> bytes:
    """V2_SWAP_CALC: [0x21][pool_idx:1][zfo:1][recipient_idx:1][fee:2] — 6 bytes."""
    return b"".join([
        CMD_V2_SWAP_CALC,
        _e(pool_idx, 1),
        b"\x01" if zfo else b"\x00",
        _e(recipient_idx, 1),
        _e(fee, 2),
    ])


def enc_v2_swap_direct(
    pool_idx: int,
    zfo: bool,
    amount_out: int,
    recipient_idx: int,
) -> bytes:
    """V2_SWAP_DIRECT: [0x22][pool_idx:1][zfo:1][amount_out:16][recipient_idx:1]
    — 20 bytes. V2 swap with explicit amount and no callback."""
    return b"".join([
        CMD_V2_SWAP_DIRECT,
        _e(pool_idx, 1),
        b"\x01" if zfo else b"\x00",
        _e(amount_out, 16),
        _e(recipient_idx, 1),
    ])


# ── V3 commands ──


def enc_v3_swap_compact(
    pool_idx: int,
    zfo: bool,
    amount_specified: int,
    recipient_idx: int,
    forward_data: bytes = b"",
) -> bytes:
    """V3_SWAP_COMPACT: [0x30][pool_idx:1][zfo:1][amount_specified:16]
    [recipient_idx:1][fwd_len:2][fwd_data:N] = 22 + N bytes."""
    return b"".join([
        CMD_V3_SWAP_COMPACT,
        _e(pool_idx, 1),
        b"\x01" if zfo else b"\x00",
        _e(amount_specified, 16),
        _e(recipient_idx, 1),
        _e(len(forward_data), 2),
        forward_data,
    ])


def enc_v3_swap_delta(
    pool_idx: int,
    zfo: bool,
    recipient_idx: int,
) -> bytes:
    """V3_SWAP_DELTA: [0x31][pool_idx:1][zfo:1][recipient_idx:1] — 4 bytes."""
    return b"".join([
        CMD_V3_SWAP_DELTA,
        _e(pool_idx, 1),
        b"\x01" if zfo else b"\x00",
        _e(recipient_idx, 1),
    ])


# ── V4 swap commands ──


def enc_v4_swap_compact(
    c0_idx: int,
    c1_idx: int,
    fee: int,
    tick_spacing: int,
    hooks_idx: int,
    zfo: bool,
    amount_u128: int,
) -> bytes:
    """V4_SWAP_COMPACT: [0x40][c0_idx:1][c1_idx:1][fee:3][ts:3]
    [hooks_idx:1][zfo:1][amount:16] — 27 bytes."""
    return b"".join([
        CMD_V4_SWAP_COMPACT,
        _e(c0_idx, 1),
        _e(c1_idx, 1),
        _e(fee, 3),
        _e(tick_spacing, 3, signed=True),
        _e(hooks_idx, 1),
        b"\x01" if zfo else b"\x00",
        _e(amount_u128, 16),
    ])


def enc_v4_swap_dynamic(
    c0_idx: int,
    c1_idx: int,
    fee: int,
    tick_spacing: int,
    hooks_idx: int,
    zfo: bool,
) -> bytes:
    """V4_SWAP_DYNAMIC: [0x41][c0_idx:1][c1_idx:1][fee:3][ts:3]
    [hooks_idx:1][zfo:1] — 11 bytes. Amount from PM exttload."""
    return b"".join([
        CMD_V4_SWAP_DYNAMIC,
        _e(c0_idx, 1),
        _e(c1_idx, 1),
        _e(fee, 3),
        _e(tick_spacing, 3, signed=True),
        _e(hooks_idx, 1),
        b"\x01" if zfo else b"\x00",
    ])


def enc_v4_batch(swaps: list[tuple[int, int, int, int, int, bool, int]]) -> bytes:
    """V4_BATCH: [0x42][num_swaps:1][entry_1:26]...[entry_N:26]

    Each 26-byte entry: [c0_idx:1][c1_idx:1][fee:3][ts:3][hooks_idx:1]
    [zfo:1][amount_u128:16]. Amount=0 means dynamic (from PM exttload).
    After all swaps, auto-settles nonzero deltas.
    """
    inner = b"".join([CMD_V4_BATCH, _e(len(swaps), 1)])
    for c0_idx, c1_idx, fee, tick_spacing, hooks_idx, zfo, amount_u128 in swaps:
        inner += b"".join([
            _e(c0_idx, 1),
            _e(c1_idx, 1),
            _e(fee, 3),
            _e(tick_spacing, 3, signed=True),
            _e(hooks_idx, 1),
            b"\x01" if zfo else b"\x00",
            _e(amount_u128, 16),
        ])
    return inner


# ── V4 settlement / ERC6909 commands ──


def enc_v4_unlock(forward_data: bytes) -> bytes:
    """V4_UNLOCK: [0x50][len:2][data:N] — 3 + N bytes."""
    return b"".join([CMD_V4_UNLOCK, _e(len(forward_data), 2), forward_data])


def enc_v4_take(
    currency_idx: int,
    recipient_idx: int,
    amount: int,
) -> bytes:
    """V4_TAKE: [0x51][currency_idx:1][recipient_idx:1][amount:32] — 35 bytes."""
    return b"".join([
        CMD_V4_TAKE,
        _e(currency_idx, 1),
        _e(recipient_idx, 1),
        _e(amount),
    ])


def enc_v4_take_compact(
    currency_idx: int,
    recipient_idx: int,
    amount_u128: int,
) -> bytes:
    """V4_TAKE_COMPACT: [0x52][currency_idx:1][recipient_idx:1][amount:16] — 19 bytes."""
    return b"".join([
        CMD_V4_TAKE_COMPACT,
        _e(currency_idx, 1),
        _e(recipient_idx, 1),
        _e(amount_u128, 16),
    ])


def enc_v4_take_delta(
    currency_idx: int,
    recipient_idx: int,
) -> bytes:
    """V4_TAKE_DELTA: [0x53][currency_idx:1][recipient_idx:1] — 3 bytes."""
    return b"".join([
        CMD_V4_TAKE_DELTA,
        _e(currency_idx, 1),
        _e(recipient_idx, 1),
    ])


def enc_v4_sync(currency_idx: int) -> bytes:
    """V4_SYNC: [0x54][currency_idx:1] — 2 bytes."""
    return b"".join([CMD_V4_SYNC, _e(currency_idx, 1)])


def enc_v4_settle() -> bytes:
    """V4_SETTLE: [0x55] — 1 byte."""
    return b"\x55"


def enc_v4_settle_delta(currency_idx: int) -> bytes:
    """V4_SETTLE_DELTA: [0x56][currency_idx:1] — 2 bytes."""
    return b"".join([CMD_V4_SETTLE_DELTA, _e(currency_idx, 1)])


def enc_v4_settle_all() -> bytes:
    """V4_SETTLE_ALL: [0x57] — 1 byte."""
    return CMD_V4_SETTLE_ALL


def enc_v4_mint_compact(
    currency_idx: int,
    recipient_idx: int,
    amount_u128: int,
) -> bytes:
    """V4_MINT_COMPACT: [0x58][currency_idx:1][recipient_idx:1][amount:16] — 19 bytes."""
    return b"".join([
        CMD_V4_MINT_COMPACT,
        _e(currency_idx, 1),
        _e(recipient_idx, 1),
        _e(amount_u128, 16),
    ])


def enc_v4_burn_compact(
    currency_idx: int,
    amount_u128: int,
) -> bytes:
    """V4_BURN_COMPACT: [0x59][currency_idx:1][amount:16] — 18 bytes."""
    return b"".join([
        CMD_V4_BURN_COMPACT,
        _e(currency_idx, 1),
        _e(amount_u128, 16),
    ])


# ── Pool key helper ──


def make_pool_key(
    currency0: str | ChecksumAddress,
    currency1: str | ChecksumAddress,
    fee: int = 0,
    tick_spacing: int = 60,
    hooks: str | ChecksumAddress = ZERO_ADDRESS,
) -> tuple[ChecksumAddress, ChecksumAddress, int, int, ChecksumAddress]:
    """Create a V4 pool key with currencies sorted by address.

    Returns:
        (currency0, currency1, fee, tick_spacing, hooks) with
        currency0 < currency1 (lexicographic).
    """
    c0, c1 = sorted(
        [to_checksum_address(currency0), to_checksum_address(currency1)],
        key=lambda a: a.lower(),
    )
    return (c0, c1, fee, tick_spacing, to_checksum_address(hooks))


# ── Command-stream builder for 2-pool V4-V4 arbitrage ──


@dataclass
class V4V4ArbitragePayload:
    """Builds a complete command stream for a 2-pool V4→V4 arbitrage.

    The simplest arbitrage: two V4 swaps inside one unlock, delta netting
    eliminates intermediate tokens, take profit and settle input.

    Usage:
        payload = V4V4ArbitragePayload(
            pool_manager=pm_address,
            weth=weth_address,
            executor=executor_address,
        )
        payload.set_pool_a(
            currency0=weth_addr, currency1=usdc_addr,
            fee=3000, tick_spacing=60, hooks=ZERO_ADDRESS,
            amount_in=1*10**18, amount_out=2000*10**6,
        )
        payload.set_pool_b(
            currency0=usdc_addr, currency1=weth_addr,
            fee=500, tick_spacing=10, hooks=ZERO_ADDRESS,
            amount_in=2000*10**6, amount_out=2*10**18,
        )
        commands = payload.encode()
    """

    pool_manager: ChecksumAddress
    weth: ChecksumAddress
    executor: ChecksumAddress

    # Pool A (first swap)
    pool_a_key: tuple[ChecksumAddress, ChecksumAddress, int, int, ChecksumAddress] | None = None
    pool_a_amount_in: int = 0
    pool_a_amount_out: int = 0
    pool_a_zfo: bool = False

    # Pool B (second swap)
    pool_b_key: tuple[ChecksumAddress, ChecksumAddress, int, int, ChecksumAddress] | None = None
    pool_b_amount_in: int = 0
    pool_b_amount_out: int = 0
    pool_b_zfo: bool = False

    # Profit token
    profit_currency: ChecksumAddress | None = None

    _at: AddressTable = field(default_factory=AddressTable, init=False, repr=False)

    def _ensure_table(self) -> AddressTable:
        """Populate the address table with all needed addresses."""
        at = AddressTable()
        at.add(self.pool_manager)
        # Add currencies from pool keys
        if self.pool_a_key:
            at.add(self.pool_a_key[0])  # currency0
            at.add(self.pool_a_key[1])  # currency1
        if self.pool_b_key:
            at.add(self.pool_b_key[0])  # currency0
            at.add(self.pool_b_key[1])  # currency1
        at.add(self.weth)
        at.add(self.executor)
        at.add(ZERO_ADDRESS)  # No hooks
        if self.profit_currency and self.profit_currency != ZERO_ADDRESS:
            at.add(self.profit_currency)
        if self.profit_currency == NATIVE_ADDRESS:
            at.add(NATIVE_ADDRESS)
        self._at = at
        return at

    def set_pool_a(
        self,
        currency0: str | ChecksumAddress,
        currency1: str | ChecksumAddress,
        fee: int,
        tick_spacing: int,
        hooks: str | ChecksumAddress = ZERO_ADDRESS,
        amount_in: int = 0,
        amount_out: int = 0,
        zero_for_one: bool | None = None,
    ) -> None:
        """Configure pool A (first swap)."""
        self.pool_a_key = make_pool_key(currency0, currency1, fee, tick_spacing, hooks)
        self.pool_a_amount_in = amount_in
        self.pool_a_amount_out = amount_out
        if zero_for_one is not None:
            self.pool_a_zfo = zero_for_one
        else:
            self.pool_a_zfo = self.pool_a_key[0] == to_checksum_address(currency0)

    def set_pool_b(
        self,
        currency0: str | ChecksumAddress,
        currency1: str | ChecksumAddress,
        fee: int,
        tick_spacing: int,
        hooks: str | ChecksumAddress = ZERO_ADDRESS,
        amount_in: int = 0,
        amount_out: int = 0,
        zero_for_one: bool | None = None,
    ) -> None:
        """Configure pool B (second swap)."""
        self.pool_b_key = make_pool_key(currency0, currency1, fee, tick_spacing, hooks)
        self.pool_b_amount_in = amount_in
        self.pool_b_amount_out = amount_out
        if zero_for_one is not None:
            self.pool_b_zfo = zero_for_one
        else:
            self.pool_b_zfo = self.pool_b_key[0] == to_checksum_address(currency0)

    def encode(self, *, skip_profit: bool = True) -> bytes:
        """Encode the full command stream for V4→V4 arbitrage.

        Returns the complete bytes payload ready for executor.execute().
        Uses V4_SWAP_COMPACT for both swaps, V4_TAKE for profit,
        and V4_SETTLE_DELTA for remaining input debt.

        For same-currency paths (e.g., both WETH-denominated), use
        V4_TAKE for the net profit delta.
        For cross-currency paths (e.g., output is native ETH), use
        V4_TAKE for native + V4_SETTLE_DELTA for WETH input.
        """
        at = self._ensure_table()
        msg = "Pool A not configured"
        assert self.pool_a_key is not None, msg
        assert self.pool_b_key is not None, "Pool B not configured"

        pm_idx = at.index_of(self.pool_manager)
        weth_idx = at.index_of(self.weth)
        executor_idx = at.index_of(self.executor)
        zero_idx = at.index_of(ZERO_ADDRESS)

        # ── V4_SWAP_COMPACT for pool A ──
        inner = enc_v4_swap_compact(
            c0_idx=at.index_of(self.pool_a_key[0]),
            c1_idx=at.index_of(self.pool_a_key[1]),
            fee=self.pool_a_key[2],
            tick_spacing=self.pool_a_key[3],
            hooks_idx=zero_idx,
            zfo=self.pool_a_zfo,
            amount_u128=self.pool_a_amount_in,
        )

        # ── V4_SWAP_COMPACT for pool B ──
        inner += enc_v4_swap_compact(
            c0_idx=at.index_of(self.pool_b_key[0]),
            c1_idx=at.index_of(self.pool_b_key[1]),
            fee=self.pool_b_key[2],
            tick_spacing=self.pool_b_key[3],
            hooks_idx=zero_idx,
            zfo=self.pool_b_zfo,
            amount_u128=self.pool_b_amount_in,
        )

        # ── Settlement ──
        # Determine profit token and input token
        # V4 sign convention: amountSpecified < 0 means exact-input
        # Pool A: pays input_currency, receives output_currency
        # Pool B: pays pool_a's output, receives profit

        # Determine the output currency of pool B (the profit token)
        output_currency_b = self.pool_b_key[1] if self.pool_b_zfo else self.pool_b_key[0]

        if output_currency_b == NATIVE_ADDRESS or (
            self.profit_currency is not None and self.profit_currency == NATIVE_ADDRESS
        ):
            # Cross-currency: native ETH output
            native_idx = at.index_of(NATIVE_ADDRESS)
            inner += enc_v4_take(native_idx, executor_idx, self.pool_b_amount_out)
            inner += enc_v4_settle_delta(weth_idx)
        else:
            # Same-currency or ERC-20 output: take profit, settle input
            profit_amount = self.pool_b_amount_out - self.pool_a_amount_in
            if profit_amount > 0:
                inner += enc_v4_take(weth_idx, executor_idx, profit_amount)
            inner += enc_v4_settle_delta(weth_idx)

        commands = enc_v4_unlock(inner)
        return enc_preamble(at, skip_profit=skip_profit) + commands


# ── CmdExecutorComposer ────────────────────────────────────────────


class CmdExecutorComposer:
    """Compose swap amounts into a cmd_executor command stream.

    Implements the PayloadComposer protocol by converting a tuple of
    SwapAmounts objects into the compact command-bytecode format that
    the on-chain cmd_executor expects.

    The composer handles the full pipeline:
    1. Build an AddressTable from all referenced addresses
    2. Encode preprocessing section (SET_ADDRESS + SKIP_PROFIT_CHECK)
    3. Encode execution commands for each swap
    4. Wrap V4 swaps inside V4_UNLOCK / settlement

    Currently supports the simple 2-pool V4→V4 arbitrage path.
    Additional path types (V4→V3, V3→V4, V2, etc.) can be added
    by extending the _encode_path method.

    Usage:
        composer = CmdExecutorComposer(
            pool_manager=pm_address,
            weth=weth_address,
            executor=executor_address,
        )
        payloads = composer.compose(swap_amounts)
    """

    def __init__(
        self,
        pool_manager: ChecksumAddress,
        weth: ChecksumAddress,
        executor: ChecksumAddress,
    ) -> None:
        self.pool_manager = pool_manager
        self.weth = weth
        self.executor = executor

    def compose(
        self,
        swap_amounts: tuple[AbstractSwapAmounts, ...],
    ) -> list[EncodedCall]:
        """Compose swap amounts into a single cmd_executor execute() call.

        Args:
            swap_amounts: Ordered sequence of swap amounts from the solver.

        Returns:
            A single-element list containing the EncodedCall for
            executor.execute(commands_bytes).
        """

        # Route to path-specific encoder based on swap types
        if len(swap_amounts) == 2 and all(
            isinstance(s, UniswapV4PoolSwapAmounts) for s in swap_amounts
        ):
            commands = self._encode_v4_v4(swap_amounts)
        else:
            msg = f"Unsupported swap combination: {[type(s).__name__ for s in swap_amounts]}"
            raise NotImplementedError(msg)

        # Load ABI for encoding
        import json
        from web3 import Web3

        abi_path = "/home/ralph/code/degenbot/contracts/cmd_executor_abi.json"
        with open(abi_path) as f:
            abi = json.load(f)

        # Encode the execute(commands) call
        selector = Web3.keccak(text="execute(bytes)")[:4]
        from eth_abi import abi as eth_abi_module

        data = selector + eth_abi_module.encode(
            types=["bytes"],
            args=[commands],
        )

        return [
            EncodedCall(
                to=self.executor,
                data=data,
                value=0,
            )
        ]

    def _encode_v4_v4(
        self,
        swap_amounts: tuple[UniswapV4PoolSwapAmounts, ...],
    ) -> bytes:
        """Encode a 2-pool V4→V4 arbitrage into command stream.

        Uses V4_SWAP_COMPACT for both swaps, V4_TAKE for profit,
        and V4_SETTLE_DELTA for remaining WETH input debt.
        All operations happen inside a single V4_UNLOCK.
        """
        from degenbot.arbitrage.types import UniswapV4PoolSwapAmounts

        swap_a: UniswapV4PoolSwapAmounts = swap_amounts[0]
        swap_b: UniswapV4PoolSwapAmounts = swap_amounts[1]

        at = AddressTable()
        at.add(self.pool_manager)
        at.add(swap_a.pool_key.currency0)
        at.add(swap_a.pool_key.currency1)
        at.add(swap_b.pool_key.currency0)
        at.add(swap_b.pool_key.currency1)
        at.add(self.weth)
        at.add(self.executor)
        at.add(ZERO_ADDRESS)  # no hooks

        # Check if native ETH is involved
        has_native = NATIVE_ADDRESS in {
            swap_a.pool_key.currency0,
            swap_a.pool_key.currency1,
            swap_b.pool_key.currency0,
            swap_b.pool_key.currency1,
        }
        if has_native:
            at.add(NATIVE_ADDRESS)

        zero_idx = at.index_of(ZERO_ADDRESS)

        # V4_SWAP_COMPACT for pool A
        inner = enc_v4_swap_compact(
            c0_idx=at.index_of(swap_a.pool_key.currency0),
            c1_idx=at.index_of(swap_a.pool_key.currency1),
            fee=swap_a.pool_key.fee,
            tick_spacing=swap_a.pool_key.tick_spacing,
            hooks_idx=zero_idx,
            zfo=swap_a.zero_for_one,
            amount_u128=swap_a.amount_in,
        )

        # V4_SWAP_COMPACT for pool B
        inner += enc_v4_swap_compact(
            c0_idx=at.index_of(swap_b.pool_key.currency0),
            c1_idx=at.index_of(swap_b.pool_key.currency1),
            fee=swap_b.pool_key.fee,
            tick_spacing=swap_b.pool_key.tick_spacing,
            hooks_idx=zero_idx,
            zfo=swap_b.zero_for_one,
            amount_u128=swap_b.amount_in,
        )

        # Settlement: determine profit currency and amounts
        output_currency_b = (
            swap_b.pool_key.currency1 if swap_b.zero_for_one else swap_b.pool_key.currency0
        )
        input_currency_a = (
            swap_a.pool_key.currency0 if swap_a.zero_for_one else swap_a.pool_key.currency1
        )

        if output_currency_b == NATIVE_ADDRESS:
            # Cross-currency: native ETH profit
            native_idx = at.index_of(NATIVE_ADDRESS)
            inner += enc_v4_take(
                native_idx,
                at.index_of(self.executor),
                swap_b.amount_out,
            )
            inner += enc_v4_settle_delta(at.index_of(self.weth))
        elif input_currency_a == output_currency_b:
            # Same-currency loop (e.g., WETH→USDC→WETH)
            profit_amount = swap_b.amount_out - swap_a.amount_in
            if profit_amount > 0:
                inner += enc_v4_take(
                    at.index_of(self.weth),
                    at.index_of(self.executor),
                    profit_amount,
                )
            inner += enc_v4_settle_delta(at.index_of(self.weth))
        else:
            # Different ERC-20 output — take it, settle the input
            inner += enc_v4_take(
                at.index_of(output_currency_b),
                at.index_of(self.executor),
                swap_b.amount_out,
            )
            inner += enc_v4_settle_delta(at.index_of(input_currency_a))

        commands = enc_v4_unlock(inner)
        return enc_preamble(at, skip_profit=True) + commands

    def encode_batch(self, *, skip_profit: bool = True) -> bytes:
        """Encode using V4_BATCH for maximum compactness.

        V4_BATCH packs both swaps into a single command with auto-settle.
        The second swap uses dynamic amounts (amount=0) read from PM exttload.
        """
        at = self._ensure_table()
        msg = "Pool A not configured"
        assert self.pool_a_key is not None, msg
        assert self.pool_b_key is not None, "Pool B not configured"

        zero_idx = at.index_of(ZERO_ADDRESS)

        # V4_BATCH: first swap explicit, second dynamic
        batch = enc_v4_batch([
            # Swap 1: explicit amount
            (
                at.index_of(self.pool_a_key[0]),
                at.index_of(self.pool_a_key[1]),
                self.pool_a_key[2],
                self.pool_a_key[3],
                zero_idx,
                self.pool_a_zfo,
                self.pool_a_amount_in,
            ),
            # Swap 2: dynamic amount
            (
                at.index_of(self.pool_b_key[0]),
                at.index_of(self.pool_b_key[1]),
                self.pool_b_key[2],
                self.pool_b_key[3],
                zero_idx,
                self.pool_b_zfo,
                0,
            ),
        ])

        commands = enc_v4_unlock(batch)
        return enc_preamble(at, skip_profit=skip_profit) + commands


# ── Command-stream builder for 2-pool V4→V3 arbitrage ──


@dataclass
class V4V3ArbitragePayload:
    """Builds a complete command stream for a 2-pool V4→V3 arbitrage.

    Path: WETH→USDC at V4 (Pool A), USDC→WETH at V3 (Pool B).
    Uses auto-pay for V3 callback (no forward_data needed).
    After V3 swap, settle WETH input to PM via V4_SETTLE_DELTA.

    Usage:
        payload = V4V3ArbitragePayload(
            pool_manager=pm_address,
            weth=weth_address,
            executor=executor_address,
            v3_pool=v3_pool_address,
        )
        payload.set_v4_pool(
            currency0=weth_addr, currency1=usdc_addr,
            fee=3000, tick_spacing=60,
            amount_in=1*10**18, amount_out=2000*10**6,
        )
        payload.set_v3_pool(
            amount_in=2000*10**6, amount_out=2*10**18,
            zero_for_one=True,
        )
        commands = payload.encode()
    """

    pool_manager: ChecksumAddress
    weth: ChecksumAddress
    executor: ChecksumAddress
    v3_pool: ChecksumAddress
    intermediate_token: ChecksumAddress  # e.g., USDC

    # V4 pool
    v4_key: tuple[ChecksumAddress, ChecksumAddress, int, int, ChecksumAddress] | None = None
    v4_amount_in: int = 0
    v4_amount_out: int = 0
    v4_zfo: bool = False

    # V3 pool
    v3_amount_in: int = 0
    v3_amount_out: int = 0
    v3_zfo: bool = False

    _at: AddressTable = field(default_factory=AddressTable, init=False, repr=False)

    def _ensure_table(self) -> AddressTable:
        at = AddressTable()
        at.add(self.pool_manager)
        at.add(self.weth)
        at.add(self.intermediate_token)
        at.add(self.executor)
        at.add(self.v3_pool)
        at.add(ZERO_ADDRESS)
        if self.v4_key:
            at.add(self.v4_key[0])
            at.add(self.v4_key[1])
        self._at = at
        return at

    def set_v4_pool(
        self,
        currency0: str | ChecksumAddress,
        currency1: str | ChecksumAddress,
        fee: int,
        tick_spacing: int,
        hooks: str | ChecksumAddress = ZERO_ADDRESS,
        amount_in: int = 0,
        amount_out: int = 0,
        zero_for_one: bool | None = None,
    ) -> None:
        self.v4_key = make_pool_key(currency0, currency1, fee, tick_spacing, hooks)
        self.v4_amount_in = amount_in
        self.v4_amount_out = amount_out
        if zero_for_one is not None:
            self.v4_zfo = zero_for_one
        else:
            self.v4_zfo = self.v4_key[0] == to_checksum_address(currency0)

    def set_v3_pool(
        self,
        amount_in: int,
        amount_out: int,
        zero_for_one: bool,
    ) -> None:
        self.v3_amount_in = amount_in
        self.v3_amount_out = amount_out
        self.v3_zfo = zero_for_one

    def encode(self, *, skip_profit: bool = True) -> bytes:
        """Encode V4→V3 with auto-pay.

        Flow (all inside V4_UNLOCK):
        1. V4_SWAP_COMPACT (WETH→USDC)
        2. V4_TAKE USDC to executor
        3. V3_SWAP_COMPACT with auto-pay (USDC→WETH, no forward_data)
        4. V4_SETTLE_DELTA WETH (settle input debt)
        """
        at = self._ensure_table()
        msg = "V4 pool not configured"
        assert self.v4_key is not None, msg

        pm_idx = at.index_of(self.pool_manager)
        weth_idx = at.index_of(self.weth)
        usdc_idx = at.index_of(self.intermediate_token)
        executor_idx = at.index_of(self.executor)
        v3_idx = at.index_of(self.v3_pool)
        zero_idx = at.index_of(ZERO_ADDRESS)

        inner = enc_v4_swap_compact(
            c0_idx=at.index_of(self.v4_key[0]),
            c1_idx=at.index_of(self.v4_key[1]),
            fee=self.v4_key[2],
            tick_spacing=self.v4_key[3],
            hooks_idx=zero_idx,
            zfo=self.v4_zfo,
            amount_u128=self.v4_amount_in,
        )
        inner += enc_v4_take(usdc_idx, executor_idx, self.v4_amount_out)
        # Auto-pay: V3 callback auto-transfers owed tokens from executor
        inner += enc_v3_swap_compact(
            v3_idx,
            self.v3_zfo,
            self.v3_amount_in,
            executor_idx,
        )
        inner += enc_v4_settle_delta(weth_idx)

        commands = enc_v4_unlock(inner)
        return enc_preamble(at, skip_profit=skip_profit) + commands

    def encode_with_forward_data(
        self,
        *,
        skip_profit: bool = True,
    ) -> bytes:
        """Encode V4→V3 with explicit forward_data in V3 callback.

        Used when auto-pay is not possible (e.g., executor doesn't hold
        the required tokens, or V3 callback must perform additional
        operations before paying).

        The forward_data encodes:
        1. ERC20_TRANSFER USDC to V3 pool
        2. V4_SYNC WETH
        3. ERC20_TRANSFER WETH to PM
        4. V4_SETTLE
        """
        at = self._ensure_table()
        msg = "V4 pool not configured"
        assert self.v4_key is not None, msg

        pm_idx = at.index_of(self.pool_manager)
        weth_idx = at.index_of(self.weth)
        usdc_idx = at.index_of(self.intermediate_token)
        executor_idx = at.index_of(self.executor)
        v3_idx = at.index_of(self.v3_pool)
        zero_idx = at.index_of(ZERO_ADDRESS)

        # Build V3 callback forward_data
        v3_callback_cmds = enc_erc20_transfer(usdc_idx, v3_idx, self.v4_amount_out)
        v3_callback_cmds += enc_v4_sync(weth_idx)
        v3_callback_cmds += enc_erc20_transfer(weth_idx, pm_idx, self.v3_amount_out)
        v3_callback_cmds += enc_v4_settle()

        inner = enc_v4_swap_compact(
            c0_idx=at.index_of(self.v4_key[0]),
            c1_idx=at.index_of(self.v4_key[1]),
            fee=self.v4_key[2],
            tick_spacing=self.v4_key[3],
            hooks_idx=zero_idx,
            zfo=self.v4_zfo,
            amount_u128=self.v4_amount_in,
        )
        inner += enc_v4_take(usdc_idx, executor_idx, self.v4_amount_out)
        inner += enc_v3_swap_compact(
            v3_idx,
            self.v3_zfo,
            self.v3_amount_in,
            executor_idx,
            forward_data=v3_callback_cmds,
        )

        commands = enc_v4_unlock(inner)
        return enc_preamble(at, skip_profit=skip_profit) + commands
