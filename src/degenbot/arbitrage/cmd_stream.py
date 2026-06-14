"""Command-stream encoding for the cmd_executor contract.

Encodes swap operations into the compact command-bytecode format
consumed by the on-chain cmd_executor. Each command is:
  1-byte opcode + tightly-packed parameters

Addresses are referenced by index into a shared address table,
built during the preprocessing section of the command stream.
Sentinel indices (0xF0-0xFF) resolve to common addresses without
SET_ADDRESS or TLOAD, saving ~476 gas per use.

Command stream format:
  [SET_ADDRESS commands][BRIBE commands][0xFF][execution commands]

The contract's _preprocess function reads opcodes from offset 0.
Preprocessing opcodes: 0x00 (SET_ADDRESS), 0x02/0x03 (BRIBE).
0xFF marks the end of preprocessing / start of execution.
The stream does NOT start with 0xFE — the contract no longer
consumes a 0xFE prefix.

Profit check is controlled by the expected_balance parameter on
execute() — expected_balance=0 skips the check. The old 0x01
(SKIP_PROFIT_CHECK) opcode is no longer recognized by the contract.

See contracts/cmd_executor.vy for the full command set and encoding.
"""

from __future__ import annotations

import json
import pathlib
from dataclasses import dataclass, field
from typing import TYPE_CHECKING

from eth_abi import abi as eth_abi_module
from eth_utils.address import to_checksum_address
from web3 import Web3

from degenbot.arbitrage.encoding import EncodedCall
from degenbot.arbitrage.types import AbstractSwapAmounts, UniswapV4PoolSwapAmounts

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress


# ── Sentinel addresses ──

NATIVE_ADDRESS: ChecksumAddress = to_checksum_address("0x0000000000000000000000000000000000000000")
ZERO_ADDRESS: ChecksumAddress = to_checksum_address("0x0000000000000000000000000000000000000000")

# ── Command opcodes ──

# Control / Preprocessing: 0x00-0x0F
CMD_SET_ADDRESS = b"\x00"
CMD_SKIP_PROFIT_CHECK = b"\x01"  # Deprecated — contract no longer handles 0x01 in preprocessing
CMD_BRIBE_COINBASE = b"\x02"
CMD_BRIBE_ADDRESS = b"\x03"

# ERC20 / ETH / Native: 0x10-0x1F
CMD_ERC20_TRANSFER = b"\x10"
CMD_ERC20_XFER_BALANCE = b"\x11"
CMD_WETH_DEPOSIT = b"\x12"
CMD_WETH_WITHDRAW = b"\x13"
CMD_WETH_DEPOSIT_ALL = b"\x14"
CMD_WETH_WITHDRAW_ALL = b"\x15"
CMD_SEND_ETH = b"\x16"
CMD_SEND_ETH_ALL = b"\x17"

# V2: 0x20-0x2F
CMD_V2_SWAP_COMPACT = b"\x20"
CMD_V2_SWAP_CALC = b"\x21"
CMD_V2_SWAP_DIRECT = b"\x22"

# V3: 0x30-0x3F
CMD_V3_SWAP_COMPACT = b"\x30"
CMD_V3_SWAP_DELTA = b"\x31"

# V4 Swaps: 0x40-0x4F
CMD_V4_SWAP_COMPACT = b"\x40"
CMD_V4_SWAP_DYNAMIC = b"\x41"
CMD_V4_BATCH = b"\x42"

# V4 Settlement / ERC6909: 0x50-0x5F
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
BEGIN_PREPROCESSING = b"\xfe"  # Deprecated — contract no longer reads 0xFE prefix
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


# ── Sentinel address indices ──
# Resolved by the contract without SET_ADDRESS or TLOAD.

SENTINEL_PM = 0xFC  # PoolManager (immutable, set at deploy)
SENTINEL_SELF = 0xFD  # self / executor address
SENTINEL_WETH = 0xFE  # WETH (immutable, set at deploy)
SENTINEL_NATIVE = 0xFF  # address(0) / NATIVE_ADDRESS — also "no hooks" flag
SENTINEL_USER0 = 0xF0  # USER0 (deploy-time, typically USDC)
SENTINEL_USER1 = 0xF1  # USER1 (deploy-time, typically WBTC)
SENTINEL_THRESHOLD = 0xF0  # Index >= this value is a sentinel, not a table index


def pack_expected_balance(check_mode: int, expected_value: int) -> int:
    """Pack check_mode and expected_value into the expected_balance uint256.

    Format: (expected_value << 8) | check_mode

    check_mode: 0=skip, 1=WETH+ETH, 2=ERC6909
    """
    return (expected_value << 8) | check_mode


# ── Address table ──


class AddressTable:
    """Tracks addresses for compact index-based referencing in the command stream.

    Each address is assigned a sequential index in insertion order (0-239).
    Sentinel indices (0xF0-0xFF) resolve to common addresses without
    SET_ADDRESS or TLOAD, saving ~476 gas per use.

    The table is built during preprocessing and referenced during execution.
    """

    def __init__(
        self,
        *,
        weth_address: str | ChecksumAddress | None = None,
        executor_address: str | ChecksumAddress | None = None,
        pool_manager_address: str | ChecksumAddress | None = None,
        user0_address: str | ChecksumAddress | None = None,
        user1_address: str | ChecksumAddress | None = None,
    ) -> None:
        self._addresses: list[ChecksumAddress] = []
        self._index_map: dict[ChecksumAddress, int] = {}

        # Build sentinel mapping — these never need SET_ADDRESS
        self._sentinel_map: dict[ChecksumAddress, int] = {}
        # NATIVE_ADDRESS always maps to SENTINEL_NATIVE
        self._sentinel_map[NATIVE_ADDRESS] = SENTINEL_NATIVE
        # ZERO_ADDRESS always maps to SENTINEL_NATIVE (same as native for "no address")
        self._sentinel_map[ZERO_ADDRESS] = SENTINEL_NATIVE
        if pool_manager_address:
            self._sentinel_map[to_checksum_address(pool_manager_address)] = SENTINEL_PM
        if executor_address:
            self._sentinel_map[to_checksum_address(executor_address)] = SENTINEL_SELF
        if weth_address:
            self._sentinel_map[to_checksum_address(weth_address)] = SENTINEL_WETH
        if user0_address:
            self._sentinel_map[to_checksum_address(user0_address)] = SENTINEL_USER0
        if user1_address:
            self._sentinel_map[to_checksum_address(user1_address)] = SENTINEL_USER1

    def add(self, addr: str | ChecksumAddress) -> int:
        """Add an address, returning its index. Idempotent for duplicates.

        Sentinel addresses (WETH, PM, executor, USDC, WBTC, NATIVE)
        return their fixed sentinel index without adding to the table.
        """
        if isinstance(addr, str):
            addr = to_checksum_address(addr)
        # Check sentinel first
        if addr in self._sentinel_map:
            return self._sentinel_map[addr]
        if addr in self._index_map:
            return self._index_map[addr]
        idx = len(self._addresses)
        msg = f"Address table full: {idx} (max 239)"
        assert idx < SENTINEL_THRESHOLD, msg
        self._addresses.append(addr)
        self._index_map[addr] = idx
        return idx

    def index_of(self, addr: str | ChecksumAddress) -> int:
        """Return the index for an address. Raises KeyError if not found."""
        if isinstance(addr, str):
            addr = to_checksum_address(addr)
        if addr in self._sentinel_map:
            return self._sentinel_map[addr]
        return self._index_map[addr]

    def __contains__(self, addr: str | ChecksumAddress) -> bool:
        if isinstance(addr, str):
            addr = to_checksum_address(addr)
        return addr in self._sentinel_map or addr in self._index_map

    @property
    def addresses(self) -> list[ChecksumAddress]:
        """Return only table addresses (not sentinels) for SET_ADDRESS encoding."""
        return list(self._addresses)


# ── Preprocessing commands ──


def enc_set_address(addr: str | ChecksumAddress) -> bytes:
    """SET_ADDRESS: [0x00][address:20] — 21 bytes."""
    return CMD_SET_ADDRESS + _address_to_bytes(addr)


def enc_set_addresses(address_table: AddressTable) -> bytes:
    """Encode SET_ADDRESS commands for all table addresses (skip sentinels)."""
    result = b""
    for addr in address_table.addresses:  # .addresses excludes sentinels
        result += enc_set_address(addr)
    return result


def enc_skip_profit_check() -> bytes:
    """SKIP_PROFIT_CHECK: [0x01] — deprecated, emits empty bytes.

    The contract no longer handles opcode 0x01 in preprocessing.
    Profit check is controlled by the expected_balance parameter on
    execute() — passing expected_balance=0 skips the check.
    This function is kept as a no-op for backwards compatibility with
    callers that unconditionally include it.
    """
    return b""


def enc_bribe_coinbase(bips: int) -> bytes:
    """BRIBE_COINBASE: [0x02][bips:2] — 3 bytes (preprocessing).

    Sends profit * bips // 10000 ETH to block.coinbase.
    Max bips: 10000 (= 100% of profit). Never reverts on shortfall.
    """
    msg = f"bips must be 0-10000, got {bips}"
    assert 0 <= bips <= 10_000, msg
    return CMD_BRIBE_COINBASE + _e(bips, 2)


def enc_bribe_address(recipient_idx: int, bips: int) -> bytes:
    """BRIBE_ADDRESS: [0x03][recipient_idx:1][bips:2] — 4 bytes (preprocessing).

    Sends profit × bips / 10000 ETH to an arbitrary address.
    Max bips: 10000 (= 100% of profit). Never reverts on shortfall.
    """
    msg = f"bips must be 0-10000, got {bips}"
    assert 0 <= bips <= 10_000, msg
    return CMD_BRIBE_ADDRESS + _e(recipient_idx, 1) + _e(bips, 2)


def enc_preamble(
    address_table: AddressTable,
    skip_profit: bool = True,
    bribe_bips: int = 0,
    bribe_address_idx: int | None = None,
) -> bytes:
    """Encode the full preprocessing section + separator.

    Builds: [SET_ADDRESS commands][BRIBE commands][0xFF]

    The stream starts directly with SET_ADDRESS commands — no 0xFE prefix.
    The contract's _preprocess function reads opcodes from offset 0 and
    processes 0x00 (SET_ADDRESS), 0x02/0x03 (BRIBE), 0xFF (BEGIN_EXECUTION).
    Any unrecognized opcode (including the old 0xFE prefix and 0x01
    skip-profit-check) causes _preprocess to break, then execution
    dispatches that opcode as an invalid command → revert.

    Profit check is controlled by the expected_balance parameter on
    execute() — passing expected_balance=0 skips the check. The old
    0x01 (SKIP_PROFIT_CHECK) opcode is no longer recognized.

    Args:
        address_table: Address table with sentinel support.
        skip_profit: Deprecated — profit check is now controlled by
            expected_balance parameter on execute(). Kept for API compat.
        bribe_bips: Profit fraction (in bips, 10000=100%) to send as bribe.
        bribe_address_idx: If set, bribe to this address index instead of coinbase.
    """
    preamble = enc_set_addresses(address_table)
    if bribe_bips > 0:
        if bribe_address_idx is not None:
            preamble += enc_bribe_address(bribe_address_idx, bribe_bips)
        else:
            preamble += enc_bribe_coinbase(bribe_bips)
    preamble += BEGIN_EXECUTION
    return preamble


# ── ERC20 / ETH / Native commands ──


def enc_erc20_transfer(
    token_idx: int,
    recipient_idx: int,
    amount: int,
) -> bytes:
    """ERC20_TRANSFER: [0x10][token_idx:1][recipient_idx:1][amount:12] — 15 bytes.

    Amount is uint96 (max 7.9×10²⁸ — covers all practical token amounts).
    """
    return b"".join([
        CMD_ERC20_TRANSFER,
        _e(token_idx, 1),
        _e(recipient_idx, 1),
        _e(amount, 12),
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
    """SEND_ETH: [0x16][recipient_idx:1][amount:12] — 14 bytes."""
    return b"".join([CMD_SEND_ETH, _e(recipient_idx, 1), _e(amount, 12)])


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
    """V2_SWAP_COMPACT: [0x20][pool_idx:1][zfo:1][amount_out:12]
    [recipient_idx:1][fee:2][fwd_len:1][fwd_data:N] = 19 + N bytes.

    fee is a fraction of 10000 (30 = 0.3% UniswapV2, 25 = 0.25% PancakeSwap).
    Written to t_v2_pair_fee[pool] before swap() for correct auto-pay.
    Amount is uint96. Forward data max 255 bytes.
    """
    return b"".join([
        CMD_V2_SWAP_COMPACT,
        _e(pool_idx, 1),
        b"\x01" if zfo else b"\x00",
        _e(amount_out, 12),
        _e(recipient_idx, 1),
        _e(fee, 2),
        _e(len(forward_data), 1),
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
    """V2_SWAP_DIRECT: [0x22][pool_idx:1][zfo:1][amount_out:12][recipient_idx:1]
    — 16 bytes. V2 swap with explicit amount and no callback.

    Amount is uint96. No fee field — the pair applies its stored fee.
    """
    return b"".join([
        CMD_V2_SWAP_DIRECT,
        _e(pool_idx, 1),
        b"\x01" if zfo else b"\x00",
        _e(amount_out, 12),
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
    """V3_SWAP_COMPACT: [0x30][pool_idx:1][zfo:1][amount_specified:12]
    [recipient_idx:1][fwd_len:1][fwd_data:N] = 17 + N bytes.

    Amount is uint96 (positive exact-input — contract negates internally).
    Sqrt price limit auto-set to widest range. Forward data max 255 bytes.
    """
    return b"".join([
        CMD_V3_SWAP_COMPACT,
        _e(pool_idx, 1),
        b"\x01" if zfo else b"\x00",
        _e(amount_specified, 12),
        _e(recipient_idx, 1),
        _e(len(forward_data), 1),
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
    amount_u96: int,
) -> bytes:
    """V4_SWAP_COMPACT: [0x40][c0_idx:1][c1_idx:1][fee:2][ts:2]
    [hooks_idx:1][zfo:1][amount:12] — 21 bytes.

    Fee is uint16 (e.g., 3000 = 0.3%). Tick spacing is int16 encoded as unsigned.
    Amount is uint96 (positive exact-input — contract negates internally).
    Use hooks_idx=0xFF (SENTINEL_NATIVE) for "no hooks".
    """
    return b"".join([
        CMD_V4_SWAP_COMPACT,
        _e(c0_idx, 1),
        _e(c1_idx, 1),
        _e(fee, 2),
        _e(tick_spacing, 2, signed=True),
        _e(hooks_idx, 1),
        b"\x01" if zfo else b"\x00",
        _e(amount_u96, 12),
    ])


def enc_v4_swap_dynamic(
    c0_idx: int,
    c1_idx: int,
    fee: int,
    tick_spacing: int,
    hooks_idx: int,
    zfo: bool,
) -> bytes:
    """V4_SWAP_DYNAMIC: [0x41][c0_idx:1][c1_idx:1][fee:2][ts:2]
    [hooks_idx:1][zfo:1] — 9 bytes. Amount from PM exttload.

    Fee is uint16. Tick spacing is int16 encoded as unsigned.
    Use hooks_idx=0xFF (SENTINEL_NATIVE) for "no hooks".
    """
    return b"".join([
        CMD_V4_SWAP_DYNAMIC,
        _e(c0_idx, 1),
        _e(c1_idx, 1),
        _e(fee, 2),
        _e(tick_spacing, 2, signed=True),
        _e(hooks_idx, 1),
        b"\x01" if zfo else b"\x00",
    ])


def enc_v4_batch(swaps: list[tuple[int, int, int, int, int, bool, int]]) -> bytes:
    """V4_BATCH: [0x42][num_swaps:1][entry_1:20]...[entry_N:20]

    Each 20-byte entry: [c0_idx:1][c1_idx:1][fee:2][ts:2][hooks_idx:1]
    [zfo:1][amount:12]. Amount=0 means dynamic (from PM exttload).
    After all swaps, auto-settles native ETH and WETH deltas.
    Max swaps: 8 (contract limit).
    """
    msg = f"V4_BATCH max 8 swaps, got {len(swaps)}"
    assert len(swaps) <= 8, msg
    inner = b"".join([CMD_V4_BATCH, _e(len(swaps), 1)])
    for c0_idx, c1_idx, fee, tick_spacing, hooks_idx, zfo, amount_u96 in swaps:
        inner += b"".join([
            _e(c0_idx, 1),
            _e(c1_idx, 1),
            _e(fee, 2),
            _e(tick_spacing, 2, signed=True),
            _e(hooks_idx, 1),
            b"\x01" if zfo else b"\x00",
            _e(amount_u96, 12),
        ])
    return inner


# ── V4 settlement / ERC6909 commands ──


def enc_v4_unlock(forward_data: bytes) -> bytes:
    """V4_UNLOCK: [0x50][len:1][data:N] — 2 + N bytes.

    Forward data max 255 bytes. Enters the PoolManager unlock context.
    """
    return b"".join([CMD_V4_UNLOCK, _e(len(forward_data), 1), forward_data])


def enc_v4_take(
    currency_idx: int,
    recipient_idx: int,
    amount: int,
) -> bytes:
    """V4_TAKE: [0x51][currency_idx:1][recipient_idx:1][amount:32] — 35 bytes.

    Rarely used — prefer V4_TAKE_COMPACT (15 bytes) or V4_TAKE_DELTA (3 bytes).
    """
    return b"".join([
        CMD_V4_TAKE,
        _e(currency_idx, 1),
        _e(recipient_idx, 1),
        _e(amount),
    ])


def enc_v4_take_compact(
    currency_idx: int,
    recipient_idx: int,
    amount_u96: int,
) -> bytes:
    """V4_TAKE_COMPACT: [0x52][currency_idx:1][recipient_idx:1][amount:12] — 15 bytes.

    Amount is uint96. Preferred over enc_v4_take for all known amounts.
    """
    return b"".join([
        CMD_V4_TAKE_COMPACT,
        _e(currency_idx, 1),
        _e(recipient_idx, 1),
        _e(amount_u96, 12),
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
    amount_u96: int,
) -> bytes:
    """V4_MINT_COMPACT: [0x58][currency_idx:1][recipient_idx:1][amount:12] — 15 bytes.

    Convert positive PM delta into ERC6909 balance for recipient.
    No physical token transfer — asset stays inside PoolManager.
    Amount is uint96.
    """
    return b"".join([
        CMD_V4_MINT_COMPACT,
        _e(currency_idx, 1),
        _e(recipient_idx, 1),
        _e(amount_u96, 12),
    ])


def enc_v4_burn_compact(
    currency_idx: int,
    amount_u96: int,
) -> bytes:
    """V4_BURN_COMPACT: [0x59][currency_idx:1][amount:12] — 14 bytes.

    Convert ERC6909 balance into a payable PM delta (offsets a debt).
    Amount is uint96.
    """
    return b"".join([
        CMD_V4_BURN_COMPACT,
        _e(currency_idx, 1),
        _e(amount_u96, 12),
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


# ── Simulation warmup slots ──


def _keccak256(data: bytes) -> bytes:
    """Keccak-256 hash."""
    from web3 import Web3
    return Web3.keccak(data)


def _mapping_slot(base_slot: int, key: int) -> int:
    """Compute Solidity mapping storage slot: keccak256(key . base_slot).

    ```. is concatenation, both key and base_slot are 32-byte big-endian.
    """
    return int.from_bytes(
        _keccak256(key.to_bytes(32, "big") + base_slot.to_bytes(32, "big")),
        "big",
    )


def _nested_mapping_slot(base_slot: int, key1: int, key2: int) -> int:
    """Compute storage slot for mapping[key1][key2] at base_slot."""
    return _mapping_slot(_mapping_slot(base_slot, key1), key2)


def compute_simulation_warmup_slots(
    executor_address: str | ChecksumAddress,
    weth_address: str | ChecksumAddress,
    pool_manager_address: str | ChecksumAddress,
) -> dict[str, dict]:
    """Compute the eth_simulateV1 stateDiff overrides that replicate
    the effect of cmd_executor.initialize().

    The cmd_executor's ``initialize()`` warms 3 storage slots to avoid
    cold SSTORE costs (~22,100 gas per 0→nonzero write) during arbitrage:

    1. **WETH.balanceOf(executor)** — Warms the executor's WETH ERC20
       balance slot. Set to 1 wei (non-zero = slot is "warm").
    2. **PM.erc6909_balanceOf(executor, weth_id)** — The executor's
       ERC6909 WETH balance inside PoolManager. Set to 1 wei by
       V4_MINT_COMPACT in ``initialize()``. This is the PRIMARY
       warmup slot — it enables gas-efficient V4_MINT profit capture.
    3. **PM.erc6909_balanceOf(executor, native_id)** — The executor's
       ERC6909 native ETH balance inside PoolManager. NOT warmed by
       ``initialize()`` but included for paths that mint native ETH
       as ERC6909. Set to 1 wei to pre-warm.

    For injected runtime bytecode (no ``initialize()`` call), these slots
    are cold. ``eth_simulateV1``'s ``stateDiff`` parameter can set them
    to non-zero values before execution, accurately replicating the
    production gas costs.

    Storage slot formulas (Solidity mapping keccak256 layout):

    - WETH9: ``balanceOf`` at mapping slot 3 (``name`` at slot 0, ``symbol``
      at slot 1, ``decimals`` at slot 2 — not ``constant`` in the deployed
      WETH9, they occupy storage slots)
    - ERC6909: ``balanceOf`` at mapping slot 4 (``owner`` at slot 0,
      ``pendingOwner`` at slot 1, ``isOperator`` at slot 2,
      ``protocolFeesAccrued`` at slot 3 — C3 linearization of
      ``PoolManager is ProtocolFees, ERC6909Claims, ...``)
    - ERC6909 id: ``uint160(currency_address)`` per CurrencyLibrary.toId()

    Args:
        executor_address: The cmd_executor contract address.
        weth_address: The WETH contract address.
        pool_manager_address: The Uniswap V4 PoolManager address.

    Returns:
        A dict keyed by checksummed contract addresses, compatible with
        the ``stateDiff`` parameter of ``eth_simulateV1``.

    Example::

        overrides = compute_simulation_warmup_slots(
            executor_address="0x...",
            weth_address="0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
            pool_manager_address="0x000000000004444c5dc75cB358380D2e3dE08A90",
        )
        # Pass as stateDiff in eth_simulateV1:
        # {"stateDiff": overrides}
    """
    executor = to_checksum_address(executor_address)
    weth = to_checksum_address(weth_address)
    pm = to_checksum_address(pool_manager_address)

    executor_int = int(executor, 16)
    weth_int = int(weth, 16)

    # ERC6909 token id: uint160(currency_address) per CurrencyLibrary.toId()
    weth_id = weth_int & ((1 << 160) - 1)  # uint160
    native_id = 0  # uint160(address(0)) = 0

    ONE_WEI = "0x0000000000000000000000000000000000000000000000000000000000000001"

    # WETH9 storage layout: name(slot 0, short string), symbol(slot 1, short
    # string), decimals(slot 2, uint8), balanceOf(slot 3, mapping).
    # Note: WETH9 does NOT use 'constant' for name/symbol — they occupy slots.
    WETH9_BALANCEOF_SLOT = 3
    weth_balance_slot = _mapping_slot(WETH9_BALANCEOF_SLOT, executor_int)

    # PoolManager ERC6909 storage layout (verified on mainnet via C3 linearization):
    #   slot 0: owner (Owned)
    #   slot 1: pendingOwner (Owned)
    #   slot 2: isOperator (ERC6909)
    #   slot 3: protocolFeesAccrued (ProtocolFees)
    #   slot 4: balanceOf (ERC6909)
    #   slot 5: allowance (ERC6909)
    # balanceOf is NOT at slot 1 — Owned and ProtocolFees come before ERC6909
    # in the C3 linearization, and protocolFeesAccrued interleaves between
    # isOperator and balanceOf.
    PM_ERC6909_BALANCEOF_SLOT = 4
    erc6909_weth_slot = _nested_mapping_slot(PM_ERC6909_BALANCEOF_SLOT, executor_int, weth_id)
    erc6909_native_slot = _nested_mapping_slot(PM_ERC6909_BALANCEOF_SLOT, executor_int, native_id)

    return {
        # WETH contract: pre-warm executor's WETH balance slot
        weth: {
            "stateDiff": {
                f"0x{weth_balance_slot:064x}": ONE_WEI,
            }
        },
        # PoolManager: pre-warm executor's ERC6909 slots
        pm: {
            "stateDiff": {
                f"0x{erc6909_weth_slot:064x}": ONE_WEI,
                f"0x{erc6909_native_slot:064x}": ONE_WEI,
            }
        },
        # Executor: 1 wei ETH remaining from initialize() (msg.value=2, deposit=1)
        executor: {
            "balance": "0x1",
        },
    }


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
        at = AddressTable(
            weth_address=self.weth,
            executor_address=self.executor,
            pool_manager_address=self.pool_manager,
        )
        # Add currencies from pool keys
        if self.pool_a_key:
            at.add(self.pool_a_key[0])  # currency0
            at.add(self.pool_a_key[1])  # currency1
        if self.pool_b_key:
            at.add(self.pool_b_key[0])  # currency0
            at.add(self.pool_b_key[1])  # currency1
        # No need to explicitly add WETH, executor, PM — they're sentinels
        # No need to explicitly add ZERO_ADDRESS — it maps to SENTINEL_NATIVE (0xFF)
        if self.profit_currency and self.profit_currency not in (ZERO_ADDRESS, NATIVE_ADDRESS):
            at.add(self.profit_currency)
        if self.profit_currency == NATIVE_ADDRESS:
            at.add(NATIVE_ADDRESS)  # Will resolve to SENTINEL_NATIVE
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
        zero_idx = SENTINEL_NATIVE  # "No hooks" — always 0xFF

        # ── V4_SWAP_COMPACT for pool A ──
        inner = enc_v4_swap_compact(
            c0_idx=at.index_of(self.pool_a_key[0]),
            c1_idx=at.index_of(self.pool_a_key[1]),
            fee=self.pool_a_key[2],
            tick_spacing=self.pool_a_key[3],
            hooks_idx=zero_idx,
            zfo=self.pool_a_zfo,
            amount_u96=self.pool_a_amount_in,
        )

        # ── V4_SWAP_COMPACT for pool B ──
        inner += enc_v4_swap_compact(
            c0_idx=at.index_of(self.pool_b_key[0]),
            c1_idx=at.index_of(self.pool_b_key[1]),
            fee=self.pool_b_key[2],
            tick_spacing=self.pool_b_key[3],
            hooks_idx=zero_idx,
            zfo=self.pool_b_zfo,
            amount_u96=self.pool_b_amount_in,
        )

        # ── Settlement ──
        # Determine the output currency of pool B (the profit token)
        output_currency_b = self.pool_b_key[1] if self.pool_b_zfo else self.pool_b_key[0]

        if output_currency_b == NATIVE_ADDRESS or (
            self.profit_currency is not None and self.profit_currency == NATIVE_ADDRESS
        ):
            # Cross-currency: native ETH output
            native_idx = at.index_of(NATIVE_ADDRESS)
            inner += enc_v4_take_compact(native_idx, executor_idx, self.pool_b_amount_out)
            inner += enc_v4_settle_delta(weth_idx)
        else:
            # Same-currency or ERC-20 output: take profit, settle input
            profit_amount = self.pool_b_amount_out - self.pool_a_amount_in
            if profit_amount > 0:
                inner += enc_v4_take_compact(weth_idx, executor_idx, profit_amount)
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
            executor.execute(commands_bytes, expected_balance).
        """

        # Route to path-specific encoder based on swap types
        if len(swap_amounts) == 2 and all(
            isinstance(s, UniswapV4PoolSwapAmounts) for s in swap_amounts
        ):
            commands = self._encode_v4_v4(swap_amounts)
        else:
            msg = f"Unsupported swap combination: {[type(s).__name__ for s in swap_amounts]}"
            raise NotImplementedError(msg)

        # Encode the execute(commands, expected_balance) call
        # Use expected_balance=0 (skip on-chain check; operator verifies off-chain)
        selector = Web3.keccak(text="execute(bytes,uint256)")[:4]

        data = selector + eth_abi_module.encode(
            types=["bytes", "uint256"],
            args=[commands, 0],
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

        swap_a: UniswapV4PoolSwapAmounts = swap_amounts[0]
        swap_b: UniswapV4PoolSwapAmounts = swap_amounts[1]

        at = AddressTable(
            weth_address=self.weth,
            executor_address=self.executor,
            pool_manager_address=self.pool_manager,
        )
        at.add(swap_a.pool_key.currency0)
        at.add(swap_a.pool_key.currency1)
        at.add(swap_b.pool_key.currency0)
        at.add(swap_b.pool_key.currency1)
        # No need to add WETH, executor, PM — they're sentinels
        # No need to add ZERO_ADDRESS — maps to SENTINEL_NATIVE (0xFF) for "no hooks"

        # Check if native ETH is involved
        has_native = NATIVE_ADDRESS in {
            swap_a.pool_key.currency0,
            swap_a.pool_key.currency1,
            swap_b.pool_key.currency0,
            swap_b.pool_key.currency1,
        }
        if has_native:
            at.add(NATIVE_ADDRESS)  # Resolves to SENTINEL_NATIVE

        zero_idx = SENTINEL_NATIVE  # "No hooks" — always 0xFF

        # V4_SWAP_COMPACT for pool A
        inner = enc_v4_swap_compact(
            c0_idx=at.index_of(swap_a.pool_key.currency0),
            c1_idx=at.index_of(swap_a.pool_key.currency1),
            fee=swap_a.pool_key.fee,
            tick_spacing=swap_a.pool_key.tick_spacing,
            hooks_idx=zero_idx,
            zfo=swap_a.zero_for_one,
            amount_u96=swap_a.amount_in,
        )

        # V4_SWAP_COMPACT for pool B
        inner += enc_v4_swap_compact(
            c0_idx=at.index_of(swap_b.pool_key.currency0),
            c1_idx=at.index_of(swap_b.pool_key.currency1),
            fee=swap_b.pool_key.fee,
            tick_spacing=swap_b.pool_key.tick_spacing,
            hooks_idx=zero_idx,
            zfo=swap_b.zero_for_one,
            amount_u96=swap_b.amount_in,
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
            inner += enc_v4_take_compact(
                native_idx,
                at.index_of(self.executor),
                swap_b.amount_out,
            )
            inner += enc_v4_settle_delta(at.index_of(self.weth))
        elif input_currency_a == output_currency_b:
            # Same-currency loop (e.g., WETH→USDC→WETH)
            profit_amount = swap_b.amount_out - swap_a.amount_in
            if profit_amount > 0:
                inner += enc_v4_take_compact(
                    at.index_of(self.weth),
                    at.index_of(self.executor),
                    profit_amount,
                )
            inner += enc_v4_settle_delta(at.index_of(self.weth))
        else:
            # Different ERC-20 output — take it, settle the input
            inner += enc_v4_take_compact(
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

        zero_idx = SENTINEL_NATIVE  # "No hooks" — always 0xFF

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
        at = AddressTable(
            weth_address=self.weth,
            executor_address=self.executor,
            pool_manager_address=self.pool_manager,
        )
        at.add(self.intermediate_token)
        at.add(self.v3_pool)
        # ZERO_ADDRESS maps to SENTINEL_NATIVE — no explicit add needed
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
        zero_idx = SENTINEL_NATIVE  # "No hooks" — always 0xFF

        inner = enc_v4_swap_compact(
            c0_idx=at.index_of(self.v4_key[0]),
            c1_idx=at.index_of(self.v4_key[1]),
            fee=self.v4_key[2],
            tick_spacing=self.v4_key[3],
            hooks_idx=zero_idx,
            zfo=self.v4_zfo,
            amount_u96=self.v4_amount_in,
        )
        inner += enc_v4_take_compact(usdc_idx, executor_idx, self.v4_amount_out)
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
        zero_idx = SENTINEL_NATIVE  # "No hooks" — always 0xFF

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
            amount_u96=self.v4_amount_in,
        )
        inner += enc_v4_take_compact(usdc_idx, executor_idx, self.v4_amount_out)
        inner += enc_v3_swap_compact(
            v3_idx,
            self.v3_zfo,
            self.v3_amount_in,
            executor_idx,
            forward_data=v3_callback_cmds,
        )

        commands = enc_v4_unlock(inner)
        return enc_preamble(at, skip_profit=skip_profit) + commands
