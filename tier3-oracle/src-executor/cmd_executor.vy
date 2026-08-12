"""
Cmd Executor — command-bytecode VM executor for Uniswap V2/V3/V4 arbitrage.

Uses a compact command stream instead of a generic payload queue.
Two modes of operation:

1. Explicit mode: off-chain code pre-computes all amounts and encodes
   V4_TAKE / V4_SYNC / V4_SETTLE / ERC20_TRANSFER commands explicitly.

2. Dynamic mode: V4 deltas are read from the PoolManager's exttload,
   V4_TAKE_DELTA / V4_SETTLE_DELTA / V4_SETTLE_ALL use the on-chain
   delta tracker to derive amounts. This eliminates calldata for amounts
   and replaces explicit V4_SYNC + ERC20_TRANSFER + V4_SETTLE sequences
   with single auto-settle commands.

Both modes can be freely mixed in the same command stream. All V4
operations update the delta tracker, so V4_SETTLE_ALL always reflects
the true state.

Key differences from tstore_executor:
- No payload queue — command stream is the continuation mechanism
- V3/V4 auto-pay — V3 callback auto-detects owed amounts, V4 reads from exttload
- V2_SWAP_CALC reads excess balance (direct custody, no callback needed)
- No Phase 0/1/2/3 — commands execute in encoded order
- No will_callback flag — targets auto-registered on swap commands
- Callback functions forward command stream through data parameter
  (using the full bytes passthrough that V2/V3/V4 support)

Command set (grouped by protocol at 0x10 boundaries):

  Control / Preprocessing (0x00-0x0F):
    0x00  SET_ADDRESS                     Append address to lookup table
    0x01  (reserved — was SET_EXPECTED_BALANCE, now in config param)
    0x02  (reserved — was BRIBE_COINBASE, now in config param)
    0x03  (reserved — was BRIBE_ADDRESS, now in config param)

  ERC20 / ETH / Native (0x10-0x1F):
    0x10  ERC20_TRANSFER    ERC-20 token transfer (any context)
    0x11  ERC20_XFER_BALANCE Transfer entire token balance (warm read)
    0x12  WETH_DEPOSIT      Wrap ETH to WETH
    0x13  WETH_WITHDRAW     Unwrap WETH to ETH
    0x14  WETH_DEPOSIT_ALL  Wrap all ETH to WETH
    0x15  WETH_WITHDRAW_ALL Unwrap all WETH to ETH
    0x16  SEND_ETH          Send uint128 ETH to address
    0x17  SEND_ETH_ALL      Send all ETH to address

  V2 (0x20-0x2F):
    0x20  V2_SWAP_COMPACT   V2 swap (uint128 amount_out + optional forward_data)
    0x21  V2_SWAP_CALC      V2 swap with on-chain amount calc from excess balance
    0x22  V2_SWAP_DIRECT    V2 swap with explicit amount_out, no callback

  V3 (0x30-0x3F):
    0x30  V3_SWAP_COMPACT   V3 swap (uint128 amount + default sqrt + auto-pay)
    0x31  V3_SWAP_DELTA     V3 swap (amount from PM exttload + default sqrt + auto-pay)

  V4 Swaps (0x40-0x4F):
    0x40  V4_SWAP_COMPACT   V4 swap (uint128 amount + default sqrt_limit)
    0x41  V4_SWAP_DYNAMIC   V4 swap (amount from PM exttload)
    0x42  V4_BATCH          V4 multi-swap + auto-settle (tight loop)

  V4 Settlement / ERC6909 (0x50-0x5F):
    0x50  V4_UNLOCK         Enter PoolManager unlock context
    0x51  V4_TAKE           V4 take from PoolManager (explicit amount)
    0x52  V4_TAKE_COMPACT   V4 take with uint128 amount
    0x53  V4_TAKE_DELTA     V4 take using PM exttload delta
    0x54  V4_SYNC           V4 sync at PoolManager (anytime)
    0x55  V4_SETTLE         V4 settle (after unlock, post sync+transfer)
    0x56  V4_SETTLE_DELTA   V4 auto-settle one currency from PM exttload delta
    0x57  V4_SETTLE_ALL     V4 auto-settle all nonzero deltas
    0x58  V4_MINT_COMPACT   V4 mint as ERC6909 (uint128 amount, no transfer)
    0x59  V4_BURN_COMPACT   V4 burn from ERC6909 (uint128 amount, no transfer)

  Stream separators:
    0xFF  BEGIN_EXECUTION      Marks end of preprocessing / start of execution

    (There is no 0xFE preprocessing prefix in the stream — _preprocess runs
    unconditionally from offset 0, reading SET_ADDRESS commands until 0xFF or
    the first non-preprocessing opcode. The 0xFE byte is reused as the V2
    auto-pay sentinel and the V4_WETH_SENTINEL constant; these are different
    contexts and not a stream prefix.)

PoolManager address: V4 commands reference the PoolManager via an immutable
set at deployment time. V4 operations can be placed in any context (outer,
V2 callback, V3 callback, or unlockCallback) as long as the PoolManager
has been unlocked.

Delta tracker: V4 deltas are read from the PoolManager's own authoritative
transient storage via exttload() instead of maintaining a local HashMap.
This eliminates tracker drift risk and saves tstore writes on every swap.
t_v4_currencies_touched is no longer used — V4_SETTLE_ALL iterates
t_addresses directly instead, saving 200-300 gas of TSTOREs per V4
swap/take when V4_SETTLE_ALL is not called (all optimal paths).
"""

#pragma version ^0.5.0a3
#pragma experimental-codegen
#pragma optimize gas

from ethereum.ercs import IERC20

from .interfaces.UniswapV2 import IUniswapV2Pair
from .interfaces.UniswapV3 import IUniswapV3Pool
from .interfaces.UniswapV4 import IPoolManager
from .interfaces.UniswapV4 import IPoolManagerExttload
from .interfaces.UniswapV4 import IERC6909Claims
from .interfaces import IWETH

# ── Immutables ──

OWNER_ADDR: immutable(address)
WETH_ADDR: immutable(address)
POOL_MANAGER_ADDR: immutable(address)

# Precomputed V4 delta slots for WETH and NATIVE (saves keccak256 on hot paths).
# Computed in __init__ as keccak256(abi.encodePacked(self, currency)) per v4-core CurrencyDelta._computeSlot.
WETH_DELTA_SLOT: immutable(bytes32)
NATIVE_DELTA_SLOT: immutable(bytes32)

# ── Constants ──

NATIVE_ADDRESS: constant(address) = empty(address)
MAX_INDEXED_ADDRESSES: constant(uint256) = 32
# Max command-stream byte length. Bounds every Bytes[] command parameter and
# every for-loop cap (execute, all callbacks, unlockCallback, _preprocess).
# Sized to fit the largest known 3-hop path with headroom. The loop cap of
# MAX_COMMANDS_LENGTH iterations is sufficient because the smallest command is
# 1 byte (V4_SETTLE etc.), so a full 288-byte stream of 1-byte commands = 288
# commands = exactly the loop cap — no silent truncation for well-typed input
# (see .auto/h3-donation-truncation.md). Exceeding this length is rejected by
# the Bytes[] type at the ABI boundary.
MAX_COMMANDS_LENGTH: constant(uint256) = 288
MAX_V4_BATCH_SWAPS: constant(uint256) = 8

# Command opcodes — grouped by protocol at 0x10 boundaries
#
# 0x00–0x0F  Control / Preprocessing
# 0x10–0x1F  ERC20 / ETH / Native
# 0x20–0x2F  V2
# 0x30–0x3F  V3
# 0x40–0x4F  V4 Swaps
# 0x50–0x5F  V4 Settlement / ERC6909
# Separators / preprocessing:
#   0x00  SET_ADDRESS (only preprocessing opcode still in the stream)
#   0xFF  BEGIN_EXECUTION (preprocessing end / execution start)
# (There is no 0xFE prefix; _preprocess starts at offset 0 unconditionally.)

# ── Control / Preprocessing: 0x00–0x0F ──
COMMAND_SET_ADDRESS: constant(uint256) = 0
# Opcode 0x01 reserved (was SET_EXPECTED_BALANCE, now in config param)
# Opcode 0x02 reserved (was BRIBE_COINBASE, now in config param)
# Opcode 0x03 reserved (was BRIBE_ADDRESS, now in config param)

# ── ERC20 / ETH / Native: 0x10–0x1F ──
COMMAND_ERC20_TRANSFER: constant(bytes1) = 0x10
COMMAND_ERC20_XFER_BALANCE: constant(bytes1) = 0x11
COMMAND_WETH_DEPOSIT: constant(bytes1) = 0x12
COMMAND_WETH_WITHDRAW: constant(bytes1) = 0x13
COMMAND_WETH_DEPOSIT_ALL: constant(bytes1) = 0x14
COMMAND_WETH_WITHDRAW_ALL: constant(bytes1) = 0x15
COMMAND_SEND_ETH: constant(bytes1) = 0x16
COMMAND_SEND_ETH_ALL: constant(bytes1) = 0x17

# ── V2: 0x20–0x2F ──
COMMAND_V2_SWAP_COMPACT: constant(bytes1) = 0x20
COMMAND_V2_SWAP_CALC: constant(bytes1) = 0x21
COMMAND_V2_SWAP_DIRECT: constant(bytes1) = 0x22

# ── V3: 0x30–0x3F ──
COMMAND_V3_SWAP_COMPACT: constant(bytes1) = 0x30
COMMAND_V3_SWAP_DELTA: constant(bytes1) = 0x31

# ── V4 Swaps: 0x40–0x4F ──
COMMAND_V4_SWAP_COMPACT: constant(bytes1) = 0x40
COMMAND_V4_SWAP_DYNAMIC: constant(bytes1) = 0x41
COMMAND_V4_BATCH: constant(bytes1) = 0x42

# ── Sentinel bytes: only protocol roles (0xFC–0xFF). ──
# No path-specific tokens are baked into the contract.
# idx < SENTINEL_THRESHOLD → t_addresses table lookup (populated per-tx via SET_ADDRESS).
# idx >= SENTINEL_THRESHOLD (0xFC) → one of the 4 protocol sentinels below.
# Any byte >= 0xFC not matching one of these reverts (InvalidCommand) rather
# than silently mis-resolving — see _lookup_address and inline blocks.
V4_WETH_SENTINEL:   constant(uint256) = 254  # 0xFE
V4_SELF_SENTINEL:   constant(uint256) = 253  # 0xFD
V4_PM_SENTINEL:     constant(uint256) = 252  # 0xFC
V4_NATIVE_SENTINEL: constant(uint256) = 255  # 0xFF
SENTINEL_THRESHOLD: constant(uint256) = 252  # 0xFC — idx >= this is a protocol sentinel

# L11 sentinel-branch ordering rule: within each handler's inline sentinel
# chain, order by PROTOCOL-ROLE frequency (e.g. WETH first for currency fields,
# SELF first for recipient fields), NOT by which address a particular benchmark
# path happens to use. Per-handler tuning to benchmark token frequencies is
# OVERFITTING (see AGENTS.md OVERFITTING). User tokens are never sentinels —
# they go through t_addresses via SET_ADDRESS. Keeping this rule prevents the
# latent mis-resolution / benchmark-artifact issues that motivated removing
# the old USER0/USER1 sentinels (commit 8c75fa6).


# ── V4 Settlement / ERC6909: 0x50–0x5F ──
COMMAND_V4_UNLOCK: constant(bytes1) = 0x50
COMMAND_V4_TAKE: constant(bytes1) = 0x51
COMMAND_V4_TAKE_COMPACT: constant(bytes1) = 0x52
COMMAND_V4_TAKE_DELTA: constant(bytes1) = 0x53
COMMAND_V4_SYNC: constant(bytes1) = 0x54
COMMAND_V4_SETTLE: constant(bytes1) = 0x55
COMMAND_V4_SETTLE_DELTA: constant(bytes1) = 0x56
COMMAND_V4_SETTLE_ALL: constant(bytes1) = 0x57
COMMAND_V4_MINT_COMPACT: constant(bytes1) = 0x58
COMMAND_V4_BURN_COMPACT: constant(bytes1) = 0x59

# ── Stream separator ──
# Marks the boundary between preprocessing commands and execution commands.
# If omitted, the entire stream is execution (backward compatible).
# Stream format: [preprocessing cmds (SET_ADDRESS...)][0xFF][execution cmds]
# If the first byte is not 0x00/0xFF, no preprocessing runs and the entire
# stream is execution. There is no 0xFE prefix — _preprocess starts at
# offset 0 unconditionally.
BEGIN_EXECUTION: constant(uint256) = 255      # End of preprocessing / start of execution


# V2 auto-pay sentinel: 1-byte data payload that triggers the auto-pay
# path in the V2 callback handler. Real V2 pairs only invoke the callback
# when data.length > 0, so we pass this 1-byte sentinel instead of empty bytes.
# (Historical note: 0xFE was once a BEGIN_PREPROCESSING prefix byte. It was
# removed (see AGENTS.md item 11). 0xFE is now only the V2 auto-pay sentinel
# byte and the V4_WETH_SENTINEL constant, used in callback data / currency
# indices — never as a stream prefix. _preprocess starts at offset 0.

# V3 amount sign conventions (V3: positive = exact-input)
MIN_SQRT_PRICE_X96: constant(uint160) = 4295128739
MAX_SQRT_PRICE_X96: constant(uint160) = 1461446703485210103287273052203988822378723970342
# Precomputed: MIN+1 and MAX-1 to avoid runtime arithmetic in V4 swap paths
MIN_SQRT_PRICE_PLUS1: constant(uint160) = 4295128740
MAX_SQRT_PRICE_MINUS1: constant(uint160) = 1461446703485210103287273052203988822378723970341

# ── Field encoding widths (bytes) ──
# Used in slice() calls to make field widths self-documenting.
WIDTH_BOOL: constant(uint256) = 1          # bool
WIDTH_UINT8: constant(uint256) = 1         # uint8 (commands, indices)
WIDTH_UINT16: constant(uint256) = 2        # uint16 (bips only — forward_len/tstore_len now use uint8)
WIDTH_UINT24: constant(uint256) = 3        # fee, tick_spacing
WIDTH_UINT96: constant(uint256) = 12       # uint96 amount (covers all practical token amounts up to 7.9e28)
WIDTH_ADDRESS: constant(uint256) = 20      # address
WIDTH_UINT256: constant(uint256) = 32      # uint256 amount


# ── Preprocessing command sizes (total bytes including opcode) ──
SIZE_SET_ADDRESS: constant(uint256) = 21       # [0x00][address:20]
# SIZE_SET_EXPECTED_BALANCE removed — now in config param
# SIZE_BRIBE_COINBASE removed — now in config param
# SIZE_BRIBE_ADDRESS removed — now in config param

# ── Execution command sizes (total bytes including opcode, fixed-size only) ──
# Variable-size commands (V2/V3_SWAP_COMPACT with forward_data, V4_UNLOCK,
# V4_BATCH) have dynamic sizes from embedded length fields.
SIZE_V4_SETTLE: constant(uint256) = 1
SIZE_V4_SETTLE_ALL: constant(uint256) = 1
SIZE_WETH_DEPOSIT_ALL: constant(uint256) = 1
SIZE_WETH_WITHDRAW_ALL: constant(uint256) = 1
SIZE_SEND_ETH_ALL: constant(uint256) = 2
SIZE_V4_SYNC: constant(uint256) = 2
SIZE_V4_SETTLE_DELTA: constant(uint256) = 2
SIZE_ERC20_XFER_BALANCE: constant(uint256) = 3
SIZE_V4_TAKE_DELTA: constant(uint256) = 3
SIZE_V2_SWAP_CALC: constant(uint256) = 6
SIZE_V2_SWAP_DIRECT: constant(uint256) = 16
SIZE_V3_SWAP_DELTA: constant(uint256) = 4
SIZE_V4_SWAP_DYNAMIC: constant(uint256) = 9
SIZE_SEND_ETH: constant(uint256) = 14
SIZE_V4_BURN_COMPACT: constant(uint256) = 14
SIZE_V4_TAKE_COMPACT: constant(uint256) = 15
SIZE_V4_MINT_COMPACT: constant(uint256) = 15
SIZE_V4_SWAP_COMPACT: constant(uint256) = 21
SIZE_WETH_DEPOSIT: constant(uint256) = 33
SIZE_WETH_WITHDRAW: constant(uint256) = 33
SIZE_ERC20_TRANSFER: constant(uint256) = 15
SIZE_V4_TAKE: constant(uint256) = 35

# ── V2/V3 swap compact field offsets (from opcode byte) ──
# V3 value: [opcode][pool_idx][zfo][amount][recipient_idx][fwd_len][fwd_data]
# V3 width: [1     ][1       ][1  ][12    ][1            ][1      ][N       ]
# V3 off:   [0     ][1       ][2  ][3..14 ][15           ][16     ][17+     ]
OFF_SWAP_RECIPIENT: constant(uint256) = 19
OFF_V3_FWD_LEN: constant(uint256)    = 15
OFF_V3_FWD_DATA: constant(uint256)   = 17

# V2 value: [opcode][pool_idx][zfo][amount][recipient_idx][fee][fwd_len][fwd_data]
# V2 width: [1     ][1       ][1  ][12    ][1            ][2  ][1      ][N       ]
# V2 off:   [0     ][1       ][2  ][3..14 ][15           ][16..17][18     ][19+     ]
OFF_V2_SWAP_FEE: constant(uint256)   = 15
OFF_V2_FWD_LEN: constant(uint256)    = 17
OFF_V2_FWD_DATA: constant(uint256)   = 19

# ── Length-prefixed command header size ──
# Used by V4_UNLOCK: [opcode:1][len:1][data:N]
SIZE_LEN_PREFIXED_HEADER: constant(uint256) = 2

# ── V4 pool key / swap field offsets (from opcode byte) ──
# Value:  [opcode][c0][c1][fee ][ts  ][hooks][zfo][amount]
# Width:  [1     ][1  ][1  ][2   ][2   ][1    ][1  ][16    ]
# Offset: [0     ][1  ][2  ][3..4][5..6][7    ][8  ][9..24]
# Used by V4_SWAP_COMPACT and V4_SWAP_DYNAMIC. Amount field is COMPACT-only.
V4_PK_C0: constant(uint256) = 1
V4_PK_C1: constant(uint256) = 2
V4_PK_FEE: constant(uint256) = 3
V4_PK_TS: constant(uint256) = 5            # past currency0_index + currency1_index + fee
V4_PK_HOOKS: constant(uint256) = 7         # past currency0_index + currency1_index + fee + tick_spacing
V4_PK_ZFO: constant(uint256) = 8           # past all pool key + swap direction fields
V4_PK_AMOUNT: constant(uint256) = 9         # past all header fields (swap compact only)

# ── V4 batch entry field offsets (from entry start, NO opcode prefix) ──
# Value:  [currency0][currency1][fee ][tick_spacing][hook_address_index][zero_for_one][amount]
# Width:  [1        ][1        ][2   ][2           ][1                 ][1           ][16    ]
# Offset: [0        ][1        ][2..3][4..5        ][6                 ][7           ][8..23]
V4_BATCH_ENTRY_SIZE: constant(uint256) = 20
V4_BATCH_ENTRY_CURRENCY0_OFFSET: constant(uint256) = 0
V4_BATCH_ENTRY_CURRENCY1_OFFSET: constant(uint256) = 1
V4_BATCH_ENTRY_FEE_OFFSET: constant(uint256) = 2
V4_BATCH_ENTRY_TICK_SPACING_OFFSET: constant(uint256) = 4   
V4_BATCH_ENTRY_HOOKS_ADDRESS_INDEX_OFFSET: constant(uint256) = 6          
V4_BATCH_ENTRY_ZERO_FOR_ONE_OFFSET: constant(uint256) = 9   
V4_BATCH_ENTRY_AMOUNT_OFFSET: constant(uint256) = 8        

# ── Custom errors ──
# Zero-arg errors where the condition is self-explanatory;
# parameterized errors for diagnostics on the failure path only.
error Unauthorized:
    caller: address

error InvalidCallback:
    caller: address

error InsufficientBalance:
    amount: uint256
    available: uint256

error InsufficientProfit:
    actual: uint256
    expected: uint256

error InvalidCommand:
    opcode: uint256

error BipsTooHigh:
    bips: uint256

error InvalidMsgValue:
    value: uint256

error NotPlainEthTransfer:
    pass


# ── Transient state (cleared every transaction) ──

# Callback registration + V2 fee — packed into a single uint256 transient store.
# Low 160 bits = callback address, bits 160-175 = V2 fee.
# V2 swaps: pack pool address + fee into one TSTORE (saves 1 TSTORE per swap).
# V3 swaps: pack pool address only (fee = 0, upper bits zero).
# External callbacks: pack sender address (fee = 0).
# Callbacks read address via convert(packed, address) + fee via packed >> 160.
# Saves 1 TSTORE + 1 TLOAD per V2 swap compared to separate transient vars.
# Single address TLOAD (~100 gas) instead of HashMap keccak256+TLOAD (~142 gas).
# Packed callback registration + V2 fee: address in low 160 bits, fee in bits 160-175.
# Saves 1 TSTORE + 1 TLOAD per V2 swap (write packed once, read packed once).
# For V2: packed = convert(pool, uint256) | unsafe_mul(fee, CALLBACK_FEE_SHIFT)
# For V3: packed = convert(pool, uint256) (fee = 0)
CALLBACK_FEE_SHIFT: constant(uint256) = 2 ** 160
# L10 invariant: V2 packs address + fee into the low 160 + bits 160-175.
# V3 swaps MUST overwrite the FULL word (write `convert(pool, uint256)` with
# high bits = 0) so stale V2 fee bits never bleed into a subsequent V2 auto-pay
# (`fee = packed >> 160`). V3 writes the full uint256; do not change V3 to a
# partial write. V4 swaps do not use callbacks (unlockCallback is auth'd by
# POOL_MANAGER_ADDR, not t_callback_packed).
t_callback_packed: transient(uint256)
t_addresses: transient(address[MAX_INDEXED_ADDRESSES])

# V4 delta tracking: instead of maintaining a local transient HashMap, we read
# the PoolManager's own authoritative deltas via exttload(). This eliminates
# tracker drift risk and saves tstore writes on every V4 swap.
# V4_SETTLE_ALL iterates t_addresses directly — no touched tracking needed.




@deploy
def __init__(weth: address, pool_manager: address):
    OWNER_ADDR = msg.sender
    WETH_ADDR = weth
    POOL_MANAGER_ADDR = pool_manager

    # Precompute V4 delta slots for hot protocol currencies.
    # Matches v4-core CurrencyDelta._computeSlot: keccak256(abi.encodePacked(target, currency)).
    # Solidity left-pads addresses (12 zero bytes + 20 addr bytes), matching
    # convert(convert(addr, uint160), bytes32) in Vyper.
    WETH_DELTA_SLOT = keccak256(concat(
        convert(self, bytes32),
        convert(convert(WETH_ADDR, uint160), bytes32),
    ))
    NATIVE_DELTA_SLOT = keccak256(concat(
        convert(self, bytes32),
        convert(convert(NATIVE_ADDRESS, uint160), bytes32),
    ))


@external
@payable
def initialize():
    """Pre-warm the ERC6909 slot for WETH by minting 1 wei as ERC6909.

    The PM's sync/settle/mint must run inside PM.unlock(), so we call
    PM.unlock() with a warmup command stream that the executor's
    unlockCallback handler will process.

    Built from the same command opcodes and sentinel indices used by
    _execute_command_at, so this stays aligned with any refactoring.

    Owner-only (one-time setup at deployment). Requires exactly 1 wei: it is
    consumed by WETH.deposit(value=1) to mint 1 wei WETH that then flows
    WETH->PM and becomes 1 wei ERC6909. No ETH is stranded (the warmup stream
    is WETH-only).
    """
    assert msg.sender == OWNER_ADDR, Unauthorized(caller=msg.sender)
    assert msg.value == 1, InvalidMsgValue(value=msg.value)  # need 1 wei for WETH.deposit

    # Initialize the WETH storage slot
    # msg.value == 1 wei is consumed here to mint 1 wei WETH (no ETH stranded).
    extcall IWETH(WETH_ADDR).deposit(value=1, skip_contract_check=True)

    # Sentinel byte values: uint256 constants need double-convert to bytes1.
    _weth: bytes1 = convert(convert(V4_WETH_SENTINEL, uint8), bytes1)   # 0xFE
    _pm: bytes1 = convert(convert(V4_PM_SENTINEL, uint8), bytes1)       # 0xFC
    _self: bytes1 = convert(convert(V4_SELF_SENTINEL, uint8), bytes1)   # 0xFD

    warmup: Bytes[512] = concat(
        # V4_SYNC(WETH) -- tell PM to start tracking WETH reserves
        COMMAND_V4_SYNC, _weth,
        # ERC20_TRANSFER(WETH, PM, 1) -- send 1 wei WETH to PoolManager (uint96 amount)
        COMMAND_ERC20_TRANSFER, _weth, _pm, slice(convert(1, bytes32), 20, 12),
        # V4_SETTLE -- account the 1 wei as +1 WETH delta
        COMMAND_V4_SETTLE,
        # V4_MINT_COMPACT(WETH, self, 1) -- convert +1 delta to ERC6909
        # (slot goes 0->nonzero = warm for all future mints)
        COMMAND_V4_MINT_COMPACT, _weth, _self,
        slice(convert(1, bytes32), 20, 12),  # uint96 amount = 1
    )
    extcall IPoolManager(POOL_MANAGER_ADDR).unlock(warmup, skip_contract_check=True)


@internal
def _v2_auto_pay(pool: address, amount0_out: uint256, amount1_out: uint256):
    """
    Auto-pay the V2 pair from callback parameters.

    V2's callback provides unsigned output amounts (what the pair sent us).
    We need to compute what we OWE using the constant-product formula + fee.

    Reads reserves from the pair (warm during callback), computes the owed
    input amount, and transfers it to the pair. The pair's post-callback K
    invariant check verifies correctness.
    """
    # Determine which token we received (the output) and which we owe (the input)
    fee: uint256 = self.t_callback_packed >> 160  # Extract fee from packed callback
    reserve0: uint112 = 0
    reserve1: uint112 = 0
    _: uint32 = 0
    (reserve0, reserve1, _) = staticcall IUniswapV2Pair(pool).getReserves()

    if amount0_out > 0:
        # We received token0, so we owe token1 (sold token1 to get token0)
        owed: uint256 = self._v2_get_amount_in(amount0_out, convert(reserve1, uint256), convert(reserve0, uint256), fee)
        token1: address = staticcall IUniswapV2Pair(pool).token1()
        extcall IERC20(token1).transfer(
            pool,
            owed,
            default_return_value=True,
            skip_contract_check=True,
        )
    else:
        # We received token1, so we owe token0 (sold token0 to get token1)
        owed2: uint256 = self._v2_get_amount_in(amount1_out, convert(reserve0, uint256), convert(reserve1, uint256), fee)
        token0_addr: address = staticcall IUniswapV2Pair(pool).token0()
        extcall IERC20(token0_addr).transfer(
            pool, 
            owed2, 
            default_return_value=True, 
            skip_contract_check=True,
        )


@internal
def _lookup_address(idx: uint256) -> address:
    """Resolve an address index with sentinel support.

    idx < SENTINEL_THRESHOLD (0xFC) → t_addresses table lookup (per-tx SET_ADDRESS).
    idx >= 0xFC → one of 4 protocol sentinels:
        0xFE = WETH_ADDR, 0xFD = self, 0xFC = POOL_MANAGER_ADDR, 0xFF = NATIVE_ADDRESS.
    Any other byte >= 0xFC reverts (InvalidCommand) — no silent catch-all,
    no path-specific tokens are baked into the contract.
    """
    if idx >= SENTINEL_THRESHOLD:
        if idx == V4_WETH_SENTINEL:
            return WETH_ADDR
        if idx == V4_SELF_SENTINEL:
            return self
        if idx == V4_PM_SENTINEL:
            return POOL_MANAGER_ADDR
        if idx == V4_NATIVE_SENTINEL:
            return NATIVE_ADDRESS
        raise InvalidCommand(opcode=idx)
    return self.t_addresses[idx]


@internal
def _read_pm_delta(currency: address) -> int256:
    """
    Read the executor's delta for a currency from the PoolManager via exttload.

    Uses the PM's own authoritative transient storage instead of a local tracker.
    Positive = PM owes us (take), Negative = we owe PM (settle).
    After a PM.take() or PM.settle() call, the delta is automatically updated
    inside the PM — a subsequent exttload returns the correct post-operation value.

    For WETH and NATIVE, uses precomputed delta slots (immutable) to avoid
    keccak256 + concat overhead on hot paths. All other currencies (including
    any path-specific token) compute the slot via keccak256.
    """
    slot: bytes32 = empty(bytes32)
    if currency == WETH_ADDR:
        slot = WETH_DELTA_SLOT
    elif currency == NATIVE_ADDRESS:
        slot = NATIVE_DELTA_SLOT
    else:
        slot = keccak256(concat(
            convert(self, bytes32),
            convert(currency, bytes32),
        ))
    raw: bytes32 = staticcall IPoolManagerExttload(POOL_MANAGER_ADDR).exttload(slot)
    return convert(raw, int256)



@internal
@pure
def _v2_get_amount_out(amount_in: uint256, reserve_in: uint256, reserve_out: uint256, fee: uint256) -> uint256:
    """
    Compute the output amount for a V2 swap given input amount and reserves.

    Formula (fee as fraction of 10000, e.g. 30 = 0.3%):
        feeMultiplier = 10000 - fee
        amountOut = (amountIn * feeMultiplier * reserveOut) / (reserveIn * 10000 + amountIn * feeMultiplier)

    For UniswapV2 (fee=30): feeMultiplier = 9970.
    For PancakeSwap (fee=25): feeMultiplier = 9975.
    """
    fee_multiplier: uint256 = unsafe_sub(10000, fee)
    # L6: use CHECKED mul on the amount_in-carrying products. amount_in in the
    # V2_SWAP_CALC caller is `pair_balance - reserve_in`, and pair_balance is
    # attacker-donatable; a giant donation could wrap the triple product to a
    # wrong (small) amount_out, griefing the executor into under-outputting.
    # Checked mul converts that into a clean revert. amount_out is NOT
    # attacker-inflatable in _v2_get_amount_in (pool-controlled, reserve-bounded),
    # so it keeps unsafe_mul. V2_SWAP_CALC is cold (not in the 27-path benchmark).
    numerator: uint256 = unsafe_mul(amount_in, fee_multiplier) * reserve_out
    denominator: uint256 = unsafe_add(unsafe_mul(reserve_in, 10000), unsafe_mul(amount_in, fee_multiplier))
    return numerator // denominator


@internal
@pure
def _v2_get_amount_in(amount_out: uint256, reserve_in: uint256, reserve_out: uint256, fee: uint256) -> uint256:
    """
    Compute the required input amount for a V2 swap given desired output and reserves.

    Formula (fee as fraction of 10000):
        amountIn = (reserveIn * amountOut * 10000) / ((reserveOut - amountOut) * feeMultiplier) + 1

    The +1 handles integer division rounding to ensure the K invariant is satisfied.
    """
    # Note: removed amount_out < reserve_out assertion — the subtraction
    # below would underflow (revert) if amount_out >= reserve_out.
    fee_multiplier: uint256 = unsafe_sub(10000, fee)
    numerator: uint256 = unsafe_mul(unsafe_mul(reserve_in, amount_out), 10000)
    denominator: uint256 = unsafe_mul(unsafe_sub(reserve_out, amount_out), fee_multiplier)
    return unsafe_add(numerator // denominator, 1)


@internal
def _v4_settle_currency(currency: address, delta: int256):
    """
    Settle a single V4 currency delta against the PoolManager.

    - Positive delta: take() — PM owes us tokens
    - Negative delta: settle() — we owe PM tokens
      - Native ETH: settle with msg.value
      - WETH: sync, deposit if needed, transfer, settle
      - ERC-20: sync, transfer, settle

    After settlement, the PM's delta is automatically zeroed by _accountDelta,
    so no local tracking update is needed.
    """
    if delta < 0:
        owed: uint256 = convert(unsafe_sub(0, delta), uint256)

        if currency == WETH_ADDR:
            extcall IPoolManager(POOL_MANAGER_ADDR).sync(
                WETH_ADDR,
                skip_contract_check=True,
            )
            weth_balance: uint256 = staticcall IERC20(WETH_ADDR).balanceOf(self)
            if weth_balance < owed:
                extcall IWETH(WETH_ADDR).deposit(
                    value=unsafe_sub(owed, weth_balance),
                    skip_contract_check=True,
                )
            extcall IERC20(WETH_ADDR).transfer(
                POOL_MANAGER_ADDR, 
                owed, 
                default_return_value=True,
                skip_contract_check=True,
            )
            extcall IPoolManager(POOL_MANAGER_ADDR).settle(
                skip_contract_check=True,
            )

        elif currency == NATIVE_ADDRESS:
            extcall IPoolManager(POOL_MANAGER_ADDR).settle(
                value=owed,
                skip_contract_check=True,
            )

        else:
            extcall IPoolManager(POOL_MANAGER_ADDR).sync(
                currency,
                skip_contract_check=True,
            )
            extcall IERC20(currency).transfer(
                POOL_MANAGER_ADDR, 
                owed, 
                default_return_value=True,
                skip_contract_check=True,
            )
            extcall IPoolManager(POOL_MANAGER_ADDR).settle(
                skip_contract_check=True,
            )

    elif delta > 0:
        extcall IPoolManager(POOL_MANAGER_ADDR).take(
            currency, self, convert(delta, uint256),
            skip_contract_check=True,
        )


@internal
def _auto_settle_touched():
    """
    Settle all nonzero V4 deltas for currencies touched by swaps.

    Reads delta amounts from the PM's exttload (authoritative source).
    After each _v4_settle_currency call, the PM's delta is automatically
    zeroed by _accountDelta inside take()/settle().

    Shared by V4_SETTLE_ALL.
    """
    # ── Settle native ETH ──
    native_delta: int256 = convert(staticcall IPoolManagerExttload(POOL_MANAGER_ADDR).exttload(NATIVE_DELTA_SLOT), int256)
    self._v4_settle_currency(NATIVE_ADDRESS, native_delta)

    # ── Settle WETH ──
    weth_delta: int256 = convert(staticcall IPoolManagerExttload(POOL_MANAGER_ADDR).exttload(WETH_DELTA_SLOT), int256)
    self._v4_settle_currency(WETH_ADDR, weth_delta)

    # ── Settle other currencies from the address table ──
    # Iterates all 32 address slots; empty slots (address(0)) have delta=0 → no-op.
    # WETH and NATIVE are handled above (they use sentinels, not the table).
    # NOTE: t_addr_count is not read here — it saves a TSTORE in _preprocess.
    for i: uint256 in range(MAX_INDEXED_ADDRESSES):
        addr: address = self.t_addresses[i]
        if addr == empty(address):
            continue
        delta: int256 = self._read_pm_delta(addr)
        self._v4_settle_currency(addr, delta)


@internal
def _preprocess(data: Bytes[MAX_COMMANDS_LENGTH]) -> uint256:
    """
    Parse the preprocessing section of a command stream.

    Only handles SET_ADDRESS (0x00) commands to populate the address
    table. All other configuration (profit check, bribes) comes from
    the ABI config parameter, avoiding elif dispatch + slice/convert
    overhead in this loop.

    Returns the byte offset where execution should begin.
    """
    offset: uint256 = 0  # start at offset 0 (no 0xFE prefix — see stream-separator comment)

    for _: uint256 in range(MAX_COMMANDS_LENGTH):
        op: uint256 = convert(slice(data, offset, WIDTH_UINT8), uint256)

        # SET_ADDRESS: [0x00][address:20] — the only preprocessing command
        # still in the stream. Checked first because it's the most common.
        # Table index = offset // 21 (SET_ADDRESS is fixed 21B, packed from 0).
        # This is safe because: (1) opcodes 0x01-0x03 are reserved (no other
        # fixed-size preprocessing opcode can shift the offset//21 mapping),
        # and (2) the Bytes[288] type bound limits the stream to ≤13
        # SET_ADDRESS commands (13×21=273 < 288 < 14×21=294), well under the
        # 32-slot table — so an out-of-range index is unreachable for
        # well-typed input (the ABI rejects oversized streams). Tested by
        # tests/test_preprocess_addr_count.py.
        if op == COMMAND_SET_ADDRESS:
            addr: address = convert(slice(data, unsafe_add(offset, WIDTH_UINT8), WIDTH_ADDRESS), address)
            self.t_addresses[offset // SIZE_SET_ADDRESS] = addr
            offset = unsafe_add(offset, SIZE_SET_ADDRESS)

        # 0xFF separator: end of preprocessing
        elif op == BEGIN_EXECUTION:
            return unsafe_add(offset, WIDTH_UINT8)

        # Any other opcode: start execution
        else:
            break

    return offset


@internal
def _cmd_v2_swap_compact(data: Bytes[MAX_COMMANDS_LENGTH], offset: uint256) -> uint256:
    """V2_SWAP_COMPACT: [0x20][pool_idx:1][zfo:1][amount_out:12][recipient_idx:1][fee:2][forward_len:1][forward_data:N]"""
    # Read all fixed fields as a single 18-byte slice (saves 1 bounds check vs 14+5)
    # Layout: [pool_idx:1][zfo:1][amount_out:12][recipient_idx:1][fee:2][forward_len:1]
    all: uint256 = convert(slice(data, unsafe_add(offset, WIDTH_UINT8), 18), uint256)
    pool: address = self.t_addresses[all >> 136]
    zero_for_one: bool = convert((all >> 128) & 255, bool)
    amount_out: uint256 = (all >> 32) & 79228162514264337593543950335  # bits 32-127, 96 bits
    recipient_idx: uint256 = (all >> 24) & 255
    fee: uint256 = (all >> 8) & 65535
    # Note: fee validation (>0, <10000) is not checked here — invalid fees
    # (>=10000) revert via `unsafe_sub(10000, fee)` underflow inside
    # _v2_get_amount_in/_out; fee==0 means a fee-free swap (valid, just unusual).
    # Covered by tests/test_v2_fee_bounds.py. See L5 analysis.

    # Always register callback — V2_SWAP_COMPACT always has forward_data (at minimum
    # the auto-pay byte 0xFE). The TSTORE is harmless if forward_len=0 (never in practice).
    # Pack callback address + fee into single TSTORE (saves 1 TSTORE = 100 gas)
    self.t_callback_packed = convert(pool, uint256) | unsafe_mul(fee, CALLBACK_FEE_SHIFT)

    # Inline sentinel resolution for recipient (saves function call overhead)
    _v2c_r: address = empty(address)
    if recipient_idx >= SENTINEL_THRESHOLD:
        if recipient_idx == V4_SELF_SENTINEL:
            _v2c_r = self
        elif recipient_idx == V4_WETH_SENTINEL:
            _v2c_r = WETH_ADDR
        elif recipient_idx == V4_PM_SENTINEL:
            _v2c_r = POOL_MANAGER_ADDR
        elif recipient_idx == V4_NATIVE_SENTINEL:
            _v2c_r = NATIVE_ADDRESS
        else:
            raise InvalidCommand(opcode=recipient_idx)
    else:
        _v2c_r = self.t_addresses[recipient_idx]

    extcall IUniswapV2Pair(pool).swap(
        amount_out if not zero_for_one else 0,
        amount_out if zero_for_one else 0,
        _v2c_r,
        slice(data, unsafe_add(offset, OFF_V2_FWD_DATA), all & 255),
        skip_contract_check=True,
    )

    return unsafe_add(unsafe_add(offset, OFF_V2_FWD_DATA), all & 255)


@internal
def _cmd_v4_batch(data: Bytes[MAX_COMMANDS_LENGTH], offset: uint256) -> uint256:
    """V4_BATCH: [0x42][num_swaps:1][entry_1:24]...[entry_N:24]

    PRECONDITION: all batch swaps' NET input/output currencies must resolve to
    WETH or native ETH. Settlement at batch end only reads the WETH and NATIVE
    delta slots; any other ERC20 currency with a nonzero delta is NOT settled
    here and would leave a CurrencyNotSettled revert at PM.unlock exit. The
    operator must handle non-WETH/native intermediate currencies with a
    trailing V4_SETTLE_DELTA / V4_TAKE elsewhere.

    Dynamic-entry (amount_u128 == 0) semantics (M9):
      - Only the FIRST dynamic entry encountered may seed its input amount from
        PM credit (reads input-currency delta via _read_pm_delta). This is the
        pm_credit_consumed flag below: True until the first dynamic entry
        consumes the PM credit, then False forever.
      - Subsequent dynamic entries chain off the immediately-prior swap's
        returned delta (prev_swap_delta), extracting the relevant currency's
        leg via prev_output_is_currency0. This requires the prior swap's output
        currency to be the current swap's input currency (reverse-order chain).
      - Unsupported: a dynamic entry needing PM credit at a position other
        than the first dynamic entry. Encode an explicit amount_u128 instead.
    """
    num_batch_swaps: uint256 = convert(slice(data, unsafe_add(offset, WIDTH_UINT8), WIDTH_UINT8), uint256)

    batch_offset: uint256 = unsafe_add(offset, 2)

    prev_swap_delta: int256 = 0
    prev_output_is_currency0: bool = False
    # M9: pm_credit_consumed — True until the first dynamic entry (amount_u128
    # == 0) reads the PM input-currency delta to seed its amount; then False.
    # Only the first dynamic entry may seed from PM credit; later dynamic
    # entries must chain off prev_swap_delta from the immediately-prior swap.
    pm_credit_consumed: bool = False

    for batch_idx: uint256 in range(num_batch_swaps, bound=MAX_V4_BATCH_SWAPS):
        # Read currency0 + currency1 indices as a single 2-byte slice (saves 1 bounds check)
        c_indices: uint256 = convert(slice(data, batch_offset, 2), uint256)
        # Read fee(2) + ts(2) + hooks_idx(1) + zfo(1) as a single 6-byte slice
        # Layout: [fee_hi|fee_lo|ts_hi|ts_lo|hooks|zfo]
        fthz: uint256 = convert(slice(data, unsafe_add(batch_offset, 2), 6), uint256)
        _hooks_idx: uint256 = (fthz >> 8) & 255

        # 0xFF sentinel = no hooks (skips TLOAD for address(0) lookup)
        pool_key: IPoolManager.PoolKey = IPoolManager.PoolKey(
            currency0=self._lookup_address((c_indices >> 8)),
            currency1=self._lookup_address((c_indices & 255)),
            fee=convert(fthz >> 32, uint24),
            tick_spacing=convert((fthz >> 16) & 65535, int24),
            hooks=self.t_addresses[_hooks_idx] if _hooks_idx != V4_NATIVE_SENTINEL else empty(address),
        )

        zero_for_one: bool = convert((fthz & 255), bool)
        amount_u128: uint128 = convert(slice(data, unsafe_add(batch_offset, V4_BATCH_ENTRY_AMOUNT_OFFSET), 12), uint128)

        amount_specified: int256 = 0
        if amount_u128 == 0:
            if not pm_credit_consumed:
                # First dynamic entry: seed input amount from PM credit for the
                # input currency. pm_credit_consumed flips to True below.
                input_delta: int256 = self._read_pm_delta(
                    pool_key.currency0 if zero_for_one else pool_key.currency1
                )
                # input_delta > 0 checked implicitly: swap would fail with 0 amount
                # assert removed: delta=0 means no credit, PM.swap reverts
                amount_specified = unsafe_sub(empty(int256), input_delta)
                pm_credit_consumed = True
            else:
                received: int128 = convert(
                    slice(
                        convert(prev_swap_delta, bytes32),
                        0 if prev_output_is_currency0 else 16,
                        16,
                    ),
                    int128,
                )
                # received > 0 checked implicitly: swap would fail with 0 amount
                # assert removed: prev_swap_delta of 0 means no output from prior swap
                amount_specified = unsafe_sub(empty(int256), convert(received, int256))
        else:
            amount_specified = unsafe_sub(empty(int256), convert(amount_u128, int256))

        prev_swap_delta = extcall IPoolManager(POOL_MANAGER_ADDR).swap(
            pool_key,
            IPoolManager.SwapParams(
                zero_for_one=zero_for_one,
                # L7 sign convention (V4): all batch `amount_specified` are
                # NEGATIVE (exact-output). input_delta / received / amount_u128
                # are magnitudes; unsafe_sub(empty(int256), x) negates them.
                amount_specified=amount_specified,
                sqrt_price_limit_x96=MIN_SQRT_PRICE_PLUS1 if zero_for_one else MAX_SQRT_PRICE_MINUS1,
            ),
            b"",
            skip_contract_check=True,
        )
        prev_output_is_currency0 = not zero_for_one
        batch_offset = unsafe_add(batch_offset, V4_BATCH_ENTRY_SIZE)

    # Settle only native ETH and WETH deltas after a V4 batch.
    # V4-only paths: intermediate ERC-20 deltas always cancel (swap 1 gives
    # +X, swap 2 takes -X). Only the input/output currencies (WETH or native)
    # have nonzero deltas. Skipping the ERC-20 iteration saves ~2,000 gas
    # and avoids unnecessary exttload calls.
    native_delta: int256 = convert(staticcall IPoolManagerExttload(POOL_MANAGER_ADDR).exttload(NATIVE_DELTA_SLOT), int256)
    self._v4_settle_currency(NATIVE_ADDRESS, native_delta)
    weth_delta: int256 = convert(staticcall IPoolManagerExttload(POOL_MANAGER_ADDR).exttload(WETH_DELTA_SLOT), int256)
    self._v4_settle_currency(WETH_ADDR, weth_delta)

    return batch_offset


@internal
def _cmd_v2_swap_calc(data: Bytes[MAX_COMMANDS_LENGTH], offset: uint256) -> uint256:
    """V2_SWAP_CALC: [0x21][pool_idx:1][zfo:1][recipient_idx:1][fee:2]

    Computes swap output on-chain from getReserves() + excess balance.

    The input amount is determined by reading the excess balance of the
    input token in the V2 pair: balanceOf(pair) - reserves[input_index].
    This equals tokens deposited to the pair but not yet reflected in
    reserves — e.g., from a V4_TAKE with the pair as recipient.

    Since the pair already holds the input tokens, swap() is called with
    empty data (no callback). The V2 K-invariant will pass because
    _v2_get_amount_out produces amounts that satisfy the K-check formula.

    The fee accumulation approximation (excess balance may include tiny
    accumulated V2 fees from other swappers between the last sync()
    and this call) is negligible for same-block arbitrage.
    """
    # Read pool_idx + zfo + recipient_idx + fee as a single 5-byte slice (saves 1 bounds check)
    pzrf: uint256 = convert(slice(data, unsafe_add(offset, WIDTH_UINT8), 5), uint256)
    pool: address = self.t_addresses[pzrf >> 32]
    zero_for_one: bool = convert((pzrf >> 24) & 255, bool)
    recipient_idx: uint256 = (pzrf >> 16) & 255
    fee: uint256 = pzrf & 65535
    # Note: fee validation removed — invalid fees (>=10000) revert via
    # `unsafe_sub(10000, fee)` underflow inside _v2_get_amount_out; fee==0
    # is a valid fee-free swap. Covered by tests/test_v2_fee_bounds.py (L5).

    # Read reserves from the pair
    reserve0: uint112 = 0
    reserve1: uint112 = 0
    _: uint32 = 0
    (reserve0, reserve1, _) = staticcall IUniswapV2Pair(pool).getReserves()

    token0: address = staticcall IUniswapV2Pair(pool).token0()
    token1: address = staticcall IUniswapV2Pair(pool).token1()

    # Compute excess balance of the input token in the V2 pair.
    # Excess = balanceOf(pair) - reserve for that token.
    # This is tokens deposited to the pair but not yet reflected in
    # reserves — our swap input amount. For direct-custody paths
    # (V4_TAKE sent tokens directly to the pair), this is the exact
    # deposited amount. For fee accumulation, the approximation is
    # negligible for same-block arbitrage.
    input_token: address = token0 if zero_for_one else token1
    reserve_in: uint256 = convert(reserve0 if zero_for_one else reserve1, uint256)
    reserve_out: uint256 = convert(reserve1 if zero_for_one else reserve0, uint256)
    pair_balance: uint256 = staticcall IERC20(input_token).balanceOf(pool)
    # Note: pair_balance >= reserve_in check removed — the subtraction
    # below would underflow (revert) if pair_balance < reserve_in.
    amount_in: uint256 = unsafe_sub(pair_balance, reserve_in)
    # Note: amount_in > 0 check removed — zero amount causes V2 pair K-invariant failure

    # Compute output from on-chain reserves + fee + excess input
    amount_out: uint256 = self._v2_get_amount_out(amount_in, reserve_in, reserve_out, fee)

    # Pair already has the input tokens (excess balance) — no callback needed.
    # The K-invariant will pass because our _v2_get_amount_out formula
    # produces amounts that satisfy the V2 K-check with equality (or
    # slightly below due to integer rounding, which also passes >=).
    extcall IUniswapV2Pair(pool).swap(
        amount_out if not zero_for_one else 0,
        amount_out if zero_for_one else 0,
        self._lookup_address(recipient_idx),
        b"",
        skip_contract_check=True,
    )

    return unsafe_add(offset, SIZE_V2_SWAP_CALC)


@internal
def _cmd_v2_swap_direct(data: Bytes[MAX_COMMANDS_LENGTH], offset: uint256) -> uint256:
    """V2_SWAP_DIRECT: [0x22][pool_idx:1][zfo:1][amount_out:12][recipient_idx:1]

    V2 swap with explicit amount_out and no callback (data=b"").

    The V2 pair must already hold the input tokens (excess balance)
    from a prior ERC20_TRANSFER or V4_TAKE. The K-invariant check
    inside the pair's swap() function verifies correctness.

    Unlike V2_SWAP_CALC which computes amount_out on-chain from
    excess balance (4 staticcalls: getReserves, token0, token1,
    balanceOf + getAmountOut), this command uses the explicit
    amount_out from calldata — saving ~4 staticcalls (~10K gas on
    cold slots) at the cost of 14 extra calldata bytes vs CALC.

    Use when: the caller has pre-computed the exact swap amounts
    off-chain and has pre-funded the V2 pair with input tokens.
    """
    # Read pool_idx + zfo + amount_out + recipient_idx as a single 15-byte slice (saves 1 bounds check vs 14+1)
    # Layout: [pool_idx:1][zfo:1][amount_out:12][recipient_idx:1]
    all: uint256 = convert(slice(data, unsafe_add(offset, WIDTH_UINT8), 15), uint256)
    pool: address = self.t_addresses[all >> 112]
    zero_for_one: bool = convert((all >> 104) & 255, bool)
    amount_out: uint256 = (all >> 8) & 79228162514264337593543950335  # 96 bits
    recipient_idx: uint256 = all & 255

    # Inline sentinel resolution for recipient (saves function call overhead)
    _v2d_r: address = empty(address)
    if recipient_idx >= SENTINEL_THRESHOLD:
        if recipient_idx == V4_SELF_SENTINEL:
            _v2d_r = self
        elif recipient_idx == V4_WETH_SENTINEL:
            _v2d_r = WETH_ADDR
        elif recipient_idx == V4_PM_SENTINEL:
            _v2d_r = POOL_MANAGER_ADDR
        elif recipient_idx == V4_NATIVE_SENTINEL:
            _v2d_r = NATIVE_ADDRESS
        else:
            raise InvalidCommand(opcode=recipient_idx)
    else:
        _v2d_r = self.t_addresses[recipient_idx]

    # No callback (data=b""). The pair already has input tokens from
    # a pre-fund operation (ERC20_TRANSFER or V4_TAKE to the pair).
    # V2's K-invariant will verify the swap is valid.
    extcall IUniswapV2Pair(pool).swap(
        amount_out if not zero_for_one else 0,
        amount_out if zero_for_one else 0,
        _v2d_r,
        b"",
        skip_contract_check=True,
    )

    return unsafe_add(offset, SIZE_V2_SWAP_DIRECT)


@internal
def _cmd_v3_swap_compact(data: Bytes[MAX_COMMANDS_LENGTH], offset: uint256) -> uint256:
    """V3_SWAP_COMPACT: [0x30][pool_idx:1][zfo:1][amount_specified:12][recipient_idx:1][forward_len:1][forward_data:N]"""
    # Read pool_idx + zfo + amount + recipient_idx + forward_len as a single 16-byte slice (saves 1 bounds check)
    # Layout: [pool_idx:1][zfo:1][amount:12][recipient_idx:1][forward_len:1]
    all: uint256 = convert(slice(data, unsafe_add(offset, WIDTH_UINT8), 16), uint256)
    pool: address = self.t_addresses[all >> 120]
    zero_for_one: bool = convert((all >> 112) & 255, bool)

    # Inline sentinel resolution for recipient (saves function call overhead)
    _r_idx: uint256 = (all >> 8) & 255
    _recipient: address = empty(address)
    if _r_idx >= SENTINEL_THRESHOLD:
        if _r_idx == V4_WETH_SENTINEL:
            _recipient = WETH_ADDR
        elif _r_idx == V4_PM_SENTINEL:
            _recipient = POOL_MANAGER_ADDR
        elif _r_idx == V4_SELF_SENTINEL:
            _recipient = self
        elif _r_idx == V4_NATIVE_SENTINEL:
            _recipient = NATIVE_ADDRESS
        else:
            raise InvalidCommand(opcode=_r_idx)
    else:
        _recipient = self.t_addresses[_r_idx]

    self.t_callback_packed = convert(pool, uint256)  # V3: fee = 0 (upper bits zero) — full word write; see t_callback_packed invariant (L10).
    # L7 sign convention: V3 amount_specified is POSITIVE (exact-input).
    # Contrast with V4 (_cmd_v4_swap_compact etc.) where amount_specified is
    # NEGATIVE (exact-output, via `unsafe_sub(empty(int256), amount)`).
    extcall IUniswapV3Pool(pool).swap(
        _recipient,
        zero_for_one,
        convert((all >> 16) & 79228162514264337593543950335, int256),
        MIN_SQRT_PRICE_PLUS1 if zero_for_one else MAX_SQRT_PRICE_MINUS1,
        slice(data, unsafe_add(offset, OFF_V3_FWD_DATA), all & 255),
        skip_contract_check=True,
    )

    return unsafe_add(unsafe_add(offset, OFF_V3_FWD_DATA), all & 255)


@internal
def _cmd_v3_swap_delta(data: Bytes[MAX_COMMANDS_LENGTH], offset: uint256) -> uint256:
    """V3_SWAP_DELTA: [0x31][pool_idx:1][zfo:1][recipient_idx:1]"""
    # Read pool_idx + zfo + recipient_idx as a single 3-byte slice (saves 1 bounds check)
    pzf: uint256 = convert(slice(data, unsafe_add(offset, WIDTH_UINT8), 3), uint256)
    pool: address = self.t_addresses[pzf >> 16]
    zero_for_one: bool = convert((pzf >> 8) & 255, bool)
    recipient_idx: uint256 = pzf & 255

    input_delta: int256 = self._read_pm_delta(
        staticcall IUniswapV3Pool(pool).token0() if zero_for_one else staticcall IUniswapV3Pool(pool).token1()
    )
    # Note: input_delta > 0 implicitly checked — swap with 0 amount reverts
    self.t_callback_packed = convert(pool, uint256)  # V3: fee = 0 (upper bits zero) — full word write (L10).
    # L7: V3 amount_specified POSITIVE (exact-input, here = input_delta).
    extcall IUniswapV3Pool(pool).swap(
        self._lookup_address(recipient_idx),
        zero_for_one,
        input_delta,
        MIN_SQRT_PRICE_PLUS1 if zero_for_one else MAX_SQRT_PRICE_MINUS1,
        b"",
        skip_contract_check=True,
    )

    return unsafe_add(offset, SIZE_V3_SWAP_DELTA)


@internal
def _cmd_v4_swap_compact(data: Bytes[MAX_COMMANDS_LENGTH], offset: uint256) -> uint256:
    """V4_SWAP_COMPACT: [0x40][currency0_index:1][currency1_index:1][fee:2][ts:2][hooks_idx:1][zfo:1][amount_specified:12]"""
    # Read all pool key + swap direction + amount as a single 20-byte slice (saves 2 bounds checks
    # vs separate reads). Layout: [c0][c1][fee:2][ts:2][hooks][zfo][amount:12]
    # fee is uint16 (max 65535 > 10000), ts is int16 (max 32767 > 200) — 2 bytes each instead of 3.
    # amount is uint96 (max 7.9e28) — covers all practical token amounts — 12 bytes instead of 16.
    all: uint256 = convert(slice(data, unsafe_add(offset, V4_PK_C0), 20), uint256)
    hooks_idx: uint256 = (all >> 104) & 255
    zero_for_one: bool = convert((all >> 96) & 255, bool)

    # 0xFF sentinel = no hooks (skips TLOAD for address(0) lookup)
    # Inline sentinel resolution for currency0 (saves function call overhead)
    c0_idx: uint256 = all >> 152
    c0: address = empty(address)
    if c0_idx >= SENTINEL_THRESHOLD:
        if c0_idx == V4_WETH_SENTINEL:
            c0 = WETH_ADDR
        elif c0_idx == V4_SELF_SENTINEL:
            c0 = self
        elif c0_idx == V4_NATIVE_SENTINEL:
            c0 = NATIVE_ADDRESS
        elif c0_idx == V4_PM_SENTINEL:
            c0 = POOL_MANAGER_ADDR
        else:
            raise InvalidCommand(opcode=c0_idx)
    else:
        c0 = self.t_addresses[c0_idx]

    # Inline sentinel resolution for currency1
    c1_idx: uint256 = (all >> 144) & 255
    c1: address = empty(address)
    if c1_idx >= SENTINEL_THRESHOLD:
        if c1_idx == V4_WETH_SENTINEL:
            c1 = WETH_ADDR
        elif c1_idx == V4_SELF_SENTINEL:
            c1 = self
        elif c1_idx == V4_NATIVE_SENTINEL:
            c1 = NATIVE_ADDRESS
        elif c1_idx == V4_PM_SENTINEL:
            c1 = POOL_MANAGER_ADDR
        else:
            raise InvalidCommand(opcode=c1_idx)
    else:
        c1 = self.t_addresses[c1_idx]

    extcall IPoolManager(POOL_MANAGER_ADDR).swap(
        IPoolManager.PoolKey(
            currency0=c0,
            currency1=c1,
            fee=convert((all >> 128) & 65535, uint24),
            tick_spacing=convert((all >> 112) & 65535, int24),
            hooks=self.t_addresses[hooks_idx] if hooks_idx != V4_NATIVE_SENTINEL else empty(address),
        ),
        IPoolManager.SwapParams(
            zero_for_one=zero_for_one,
            amount_specified=unsafe_sub(empty(int256), convert(all & 79228162514264337593543950335, int256)),
            sqrt_price_limit_x96=MIN_SQRT_PRICE_PLUS1 if zero_for_one else MAX_SQRT_PRICE_MINUS1,
        ),
        b"",
        skip_contract_check=True,
    )
    # No t_v4_currencies_touched tracking — saves 2 TSTOREs (200 gas) per swap.
    # V4_SETTLE_ALL iterates t_addresses directly instead.

    return unsafe_add(offset, SIZE_V4_SWAP_COMPACT)


@internal
def _cmd_erc20_transfer(data: Bytes[MAX_COMMANDS_LENGTH], offset: uint256) -> uint256:
    """ERC20_TRANSFER: [0x10][token_idx:1][recipient_idx:1][amount:12]

    Transfer uint96 amount of ERC20 tokens. Amount fits in uint96
    (max 7.9e28) which covers all practical token amounts.
    """
    # Read token_idx + recipient_idx + amount as a single 14-byte slice (saves 1 bounds check)
    # Layout: [token_idx:1][recipient_idx:1][amount:12]
    all: uint256 = convert(slice(data, unsafe_add(offset, WIDTH_UINT8), 14), uint256)
    amount: uint256 = all & 79228162514264337593543950335

    # Inline sentinel resolution for token (saves function call overhead)
    t_idx: uint256 = all >> 104
    token: address = empty(address)
    if t_idx >= SENTINEL_THRESHOLD:
        if t_idx == V4_WETH_SENTINEL:
            token = WETH_ADDR
        elif t_idx == V4_SELF_SENTINEL:
            token = self
        elif t_idx == V4_NATIVE_SENTINEL:
            token = NATIVE_ADDRESS
        elif t_idx == V4_PM_SENTINEL:
            token = POOL_MANAGER_ADDR
        else:
            raise InvalidCommand(opcode=t_idx)
    else:
        token = self.t_addresses[t_idx]

    # Inline sentinel resolution for recipient
    r_idx: uint256 = (all >> 96) & 255
    recipient: address = empty(address)
    if r_idx >= SENTINEL_THRESHOLD:
        if r_idx == V4_PM_SENTINEL:
            recipient = POOL_MANAGER_ADDR
        elif r_idx == V4_SELF_SENTINEL:
            recipient = self
        elif r_idx == V4_WETH_SENTINEL:
            recipient = WETH_ADDR
        elif r_idx == V4_NATIVE_SENTINEL:
            recipient = NATIVE_ADDRESS
        else:
            raise InvalidCommand(opcode=r_idx)
    else:
        recipient = self.t_addresses[r_idx]

    extcall IERC20(token).transfer(
        recipient,
        amount,
        default_return_value=True,
        skip_contract_check=True,
    )

    return unsafe_add(offset, SIZE_ERC20_TRANSFER)


@internal
def _cmd_weth_deposit(data: Bytes[MAX_COMMANDS_LENGTH], offset: uint256) -> uint256:
    """WETH_DEPOSIT: [0x12][amount:32]"""
    amount: uint256 = convert(slice(data, unsafe_add(offset, WIDTH_UINT8), WIDTH_UINT256), uint256)

    extcall IWETH(WETH_ADDR).deposit(
        value=amount,
        skip_contract_check=True,
    )

    return unsafe_add(offset, SIZE_WETH_DEPOSIT)


@internal
def _cmd_weth_withdraw(data: Bytes[MAX_COMMANDS_LENGTH], offset: uint256) -> uint256:
    """WETH_WITHDRAW: [0x13][amount:32]"""
    amount: uint256 = convert(slice(data, unsafe_add(offset, WIDTH_UINT8), WIDTH_UINT256), uint256)

    extcall IWETH(WETH_ADDR).withdraw(
        amount,
        skip_contract_check=True,
    )

    return unsafe_add(offset, SIZE_WETH_WITHDRAW)


@internal
def _cmd_erc20_xfer_balance(data: Bytes[MAX_COMMANDS_LENGTH], offset: uint256) -> uint256:
    """ERC20_XFER_BALANCE: [0x11][token_idx:1][recipient_idx:1]"""
    indices: uint256 = convert(slice(data, unsafe_add(offset, WIDTH_UINT8), 2), uint256)
    token: address = self._lookup_address((indices >> 8))

    extcall IERC20(token).transfer(
        self._lookup_address((indices & 255)),
        staticcall IERC20(token).balanceOf(self),
        default_return_value=True,
        skip_contract_check=True,
    )

    return unsafe_add(offset, SIZE_ERC20_XFER_BALANCE)


@internal
def _cmd_weth_deposit_all(data: Bytes[MAX_COMMANDS_LENGTH], offset: uint256) -> uint256:
    """WETH_DEPOSIT_ALL: [0x14]"""
    extcall IWETH(WETH_ADDR).deposit(
        value=self.balance,
        skip_contract_check=True,
    )
    return unsafe_add(offset, SIZE_WETH_DEPOSIT_ALL)


@internal
def _cmd_weth_withdraw_all(data: Bytes[MAX_COMMANDS_LENGTH], offset: uint256) -> uint256:
    """WETH_WITHDRAW_ALL: [0x15]"""
    extcall IWETH(WETH_ADDR).withdraw(
        staticcall IERC20(WETH_ADDR).balanceOf(self),
        skip_contract_check=True,
    )
    return unsafe_add(offset, SIZE_WETH_WITHDRAW_ALL)


@internal
def _cmd_send_eth(data: Bytes[MAX_COMMANDS_LENGTH], offset: uint256) -> uint256:
    """SEND_ETH: [0x16][recipient_idx:1][amount:12]

    Send a uint96 amount of native ETH from the executor to a recipient.
    Used to withdraw ETH received from the PoolManager via V4_TAKE_DELTA
    on the NATIVE_ADDRESS currency after burning ERC6909 entries.
    """
    # Read recipient_idx + amount as a single 13-byte slice (saves 1 bounds check)
    # Layout: [recipient_idx:1][amount:12]
    ra: uint256 = convert(slice(data, unsafe_add(offset, WIDTH_UINT8), 13), uint256)
    raw_call(self.t_addresses[ra >> 96], b"", value=ra & 79228162514264337593543950335)
    return unsafe_add(offset, SIZE_SEND_ETH)


@internal
def _cmd_send_eth_all(data: Bytes[MAX_COMMANDS_LENGTH], offset: uint256) -> uint256:
    """SEND_ETH_ALL: [0x17][recipient_idx:1]

    Send the executor's entire ETH balance to a recipient.
    Used to withdraw all ETH after a V4 burn+take withdrawal.
    The balance is read warm (self.balance is ~3 gas after any prior operation).
    """
    raw_call(self.t_addresses[convert(slice(data, unsafe_add(offset, WIDTH_UINT8), WIDTH_UINT8), uint256)], b"", value=self.balance)
    return unsafe_add(offset, SIZE_SEND_ETH_ALL)


@internal
def _cmd_v4_swap_dynamic(data: Bytes[MAX_COMMANDS_LENGTH], offset: uint256) -> uint256:
    """V4_SWAP_DYNAMIC: [0x41][currency0_index:1][currency1_index:1][fee:2][ts:2][hooks_idx:1][zfo:1]"""
    # Read all pool key + swap direction as a single 8-byte slice (saves 1 bounds check)
    # Layout: [c0][c1][fee:2][ts:2][hooks][zfo]
    pkh: uint256 = convert(slice(data, unsafe_add(offset, V4_PK_C0), 8), uint256)
    hooks_idx: uint256 = (pkh >> 8) & 255
    zero_for_one: bool = convert((pkh & 255), bool)

    # 0xFF sentinel = no hooks (skips TLOAD for address(0) lookup)
    pool_key: IPoolManager.PoolKey = IPoolManager.PoolKey(
        currency0=self._lookup_address(pkh >> 56),
        currency1=self._lookup_address((pkh >> 48) & 255),
        fee=convert((pkh >> 32) & 65535, uint24),
        tick_spacing=convert((pkh >> 16) & 65535, int24),
        hooks=self.t_addresses[hooks_idx] if hooks_idx != V4_NATIVE_SENTINEL else empty(address),
    )

    input_delta: int256 = self._read_pm_delta(
        pool_key.currency0 if zero_for_one else pool_key.currency1
    )
    # Note: input_delta > 0 implicitly checked — swap with 0 amount reverts

    extcall IPoolManager(POOL_MANAGER_ADDR).swap(
        pool_key,
        IPoolManager.SwapParams(
            zero_for_one=zero_for_one,
            amount_specified=unsafe_sub(empty(int256), input_delta),  # L7: V4 NEGATIVE.
            sqrt_price_limit_x96=MIN_SQRT_PRICE_PLUS1 if zero_for_one else MAX_SQRT_PRICE_MINUS1,
        ),
        b"",
        skip_contract_check=True,
    )
    # No t_v4_currencies_touched tracking — saves 2 TSTOREs (200 gas) per swap.
    # V4_SETTLE_ALL iterates t_addresses directly instead.

    return unsafe_add(offset, SIZE_V4_SWAP_DYNAMIC)


@internal
def _cmd_v4_take(data: Bytes[MAX_COMMANDS_LENGTH], offset: uint256) -> uint256:
    """V4_TAKE: [0x51][currency_idx:1][recipient_idx:1][amount:32]"""
    indices: uint256 = convert(slice(data, unsafe_add(offset, WIDTH_UINT8), 2), uint256)
    amount: uint256 = convert(slice(data, unsafe_add(offset, 3), WIDTH_UINT256), uint256)

    extcall IPoolManager(POOL_MANAGER_ADDR).take(
        self._lookup_address((indices >> 8)),
        self._lookup_address((indices & 255)),
        amount,
        skip_contract_check=True,
    )

    # No t_v4_currencies_touched tracking.

    return unsafe_add(offset, SIZE_V4_TAKE)


@internal
def _cmd_v4_sync(data: Bytes[MAX_COMMANDS_LENGTH], offset: uint256) -> uint256:
    """V4_SYNC: [0x54][currency_idx:1]"""
    # Inline sentinel resolution for currency (saves function call overhead)
    _currency_idx: uint256 = convert(slice(data, unsafe_add(offset, WIDTH_UINT8), WIDTH_UINT8), uint256)
    _currency: address = empty(address)
    if _currency_idx >= SENTINEL_THRESHOLD:
        if _currency_idx == V4_WETH_SENTINEL:
            _currency = WETH_ADDR
        elif _currency_idx == V4_SELF_SENTINEL:
            _currency = self
        elif _currency_idx == V4_NATIVE_SENTINEL:
            _currency = NATIVE_ADDRESS
        elif _currency_idx == V4_PM_SENTINEL:
            _currency = POOL_MANAGER_ADDR
        else:
            raise InvalidCommand(opcode=_currency_idx)
    else:
        _currency = self.t_addresses[_currency_idx]

    extcall IPoolManager(POOL_MANAGER_ADDR).sync(
        _currency,
        skip_contract_check=True,
    )

    return unsafe_add(offset, SIZE_V4_SYNC)


@internal
def _cmd_v4_settle(data: Bytes[MAX_COMMANDS_LENGTH], offset: uint256) -> uint256:
    """V4_SETTLE: [0x55]"""
    extcall IPoolManager(POOL_MANAGER_ADDR).settle(
        skip_contract_check=True,
    )
    return unsafe_add(offset, SIZE_V4_SETTLE)


@internal
def _cmd_v4_unlock(data: Bytes[MAX_COMMANDS_LENGTH], offset: uint256) -> uint256:
    """V4_UNLOCK: [0x50][forward_len:1][forward_data:N]"""
    forward_len: uint256 = convert(slice(data, unsafe_add(offset, WIDTH_UINT8), WIDTH_UINT8), uint256)

    # No need to register PM in t_callback_packed —
    # unlockCallback checks immutable POOL_MANAGER_ADDR directly.
    # This saves a TSTORE (100 gas) per V4_UNLOCK invocation.

    extcall IPoolManager(POOL_MANAGER_ADDR).unlock(
        slice(data, unsafe_add(offset, SIZE_LEN_PREFIXED_HEADER), forward_len),
        skip_contract_check=True,
    )

    return unsafe_add(unsafe_add(offset, SIZE_LEN_PREFIXED_HEADER), forward_len)


@internal
def _cmd_v4_take_delta(data: Bytes[MAX_COMMANDS_LENGTH], offset: uint256) -> uint256:
    """V4_TAKE_DELTA: [0x53][currency_idx:1][recipient_idx:1]

    Takes the full positive PM delta for the specified currency.
    No amount encoding needed — reads delta from PM's transient storage.
    12 bytes shorter than V4_TAKE_COMPACT per take.
    """
    indices: uint256 = convert(slice(data, unsafe_add(offset, WIDTH_UINT8), 2), uint256)
    currency: address = self._lookup_address((indices >> 8))
    delta: int256 = self._read_pm_delta(currency)

    # Guard: take only makes sense for a positive delta (PM owes us). A zero
    # delta is a no-op; a negative delta means we OWE the currency (settle's
    # job, not take's) and convert(negative, uint256) would wrap to ~2^255
    # causing an opaque PM revert. Fail-closed: skip both non-positive cases
    # so the operator gets a clean no-op rather than a confusing downstream
    # revert (consistent with _cmd_v4_settle_delta's delta > 0 branch).
    if delta > 0:
        extcall IPoolManager(POOL_MANAGER_ADDR).take(
            currency,
            self._lookup_address((indices & 255)),
            convert(delta, uint256),
            skip_contract_check=True,
        )

    return unsafe_add(offset, SIZE_V4_TAKE_DELTA)


@internal
def _cmd_v4_take_compact(data: Bytes[MAX_COMMANDS_LENGTH], offset: uint256) -> uint256:
    """V4_TAKE_COMPACT: [0x52][currency_idx:1][recipient_idx:1][amount:12]"""
    # Read currency + recipient indices + amount as a single 14-byte slice (saves 1 bounds check)
    ira: uint256 = convert(slice(data, unsafe_add(offset, WIDTH_UINT8), 14), uint256)

    # Inline sentinel resolution for currency (saves function call overhead)
    c_idx: uint256 = ira >> 104
    currency: address = empty(address)
    if c_idx >= SENTINEL_THRESHOLD:
        if c_idx == V4_WETH_SENTINEL:
            currency = WETH_ADDR
        elif c_idx == V4_SELF_SENTINEL:
            currency = self
        elif c_idx == V4_NATIVE_SENTINEL:
            currency = NATIVE_ADDRESS
        elif c_idx == V4_PM_SENTINEL:
            currency = POOL_MANAGER_ADDR
        else:
            raise InvalidCommand(opcode=c_idx)
    else:
        currency = self.t_addresses[c_idx]

    # Inline sentinel resolution for recipient
    r_idx: uint256 = (ira >> 96) & 255
    recipient: address = empty(address)
    if r_idx >= SENTINEL_THRESHOLD:
        if r_idx == V4_SELF_SENTINEL:
            recipient = self
        elif r_idx == V4_WETH_SENTINEL:
            recipient = WETH_ADDR
        elif r_idx == V4_PM_SENTINEL:
            recipient = POOL_MANAGER_ADDR
        elif r_idx == V4_NATIVE_SENTINEL:
            recipient = NATIVE_ADDRESS
        else:
            raise InvalidCommand(opcode=r_idx)
    else:
        recipient = self.t_addresses[r_idx]

    amount: uint256 = ira & 79228162514264337593543950335  # lower 96 bits

    extcall IPoolManager(POOL_MANAGER_ADDR).take(
        currency,
        recipient,
        amount,
        skip_contract_check=True,
    )

    # No t_v4_currencies_touched tracking.

    return unsafe_add(offset, SIZE_V4_TAKE_COMPACT)


@internal
def _cmd_v4_settle_delta(data: Bytes[MAX_COMMANDS_LENGTH], offset: uint256) -> uint256:
    """V4_SETTLE_DELTA: [0x56][currency_idx:1]"""
    currency_idx: uint256 = convert(slice(data, unsafe_add(offset, WIDTH_UINT8), WIDTH_UINT8), uint256)
    # Fast path: use sentinel-aware settle to avoid redundant currency comparisons
    # in _v4_settle_currency. For WETH/NATIVE sentinels, we skip the address
    # comparison branches in _v4_settle_currency.
    if currency_idx >= SENTINEL_THRESHOLD:
        # Sentinel: inline the settlement logic directly, skipping _v4_settle_currency's
        # redundant address comparisons. Also skips _read_pm_delta's keccak256.
        if currency_idx == V4_WETH_SENTINEL:
            delta: int256 = convert(staticcall IPoolManagerExttload(POOL_MANAGER_ADDR).exttload(WETH_DELTA_SLOT), int256)
            if delta < 0:
                owed_w: uint256 = convert(unsafe_sub(0, delta), uint256)
                extcall IPoolManager(POOL_MANAGER_ADDR).sync(WETH_ADDR, skip_contract_check=True)
                # No balanceOf+deposit fallback here (V4_SETTLE_DELTA): the operator
                # must ensure executor holds enough WETH (swap output or pre-fund).
                # _v4_settle_currency (V4_SETTLE_ALL) keeps the ETH→WETH deposit fallback.
                extcall IERC20(WETH_ADDR).transfer(POOL_MANAGER_ADDR, owed_w, default_return_value=True, skip_contract_check=True)
                extcall IPoolManager(POOL_MANAGER_ADDR).settle(skip_contract_check=True)
            elif delta > 0:
                extcall IPoolManager(POOL_MANAGER_ADDR).take(WETH_ADDR, self, convert(delta, uint256), skip_contract_check=True)
        elif currency_idx == V4_NATIVE_SENTINEL:
            delta2: int256 = convert(staticcall IPoolManagerExttload(POOL_MANAGER_ADDR).exttload(NATIVE_DELTA_SLOT), int256)
            if delta2 < 0:
                extcall IPoolManager(POOL_MANAGER_ADDR).settle(value=convert(unsafe_sub(0, delta2), uint256), skip_contract_check=True)
            elif delta2 > 0:
                extcall IPoolManager(POOL_MANAGER_ADDR).take(NATIVE_ADDRESS, self, convert(delta2, uint256), skip_contract_check=True)
        else:
            # PM (0xFC) and SELF (0xFD) sentinels are not valid currency inputs
            # to SETTLE — fail closed with InvalidCommand rather than silently
            # no-op'ing and leaving a delta to be caught later by PM.unlock.
            raise InvalidCommand(opcode=currency_idx)
    else:
        # Table index — inline settle logic to skip _v4_settle_currency's
        # redundant WETH/NATIVE comparisons (table indices are always ERC-20).
        # Also inline _read_pm_delta to skip function call overhead + prune
        # WETH/NATIVE slot branches (table addresses always use keccak256 slot).
        currency: address = self.t_addresses[currency_idx]
        _slot4: bytes32 = keccak256(concat(
            convert(self, bytes32),
            convert(currency, bytes32),
        ))
        delta4: int256 = convert(staticcall IPoolManagerExttload(POOL_MANAGER_ADDR).exttload(_slot4), int256)
        if delta4 < 0:
            owed4: uint256 = convert(unsafe_sub(0, delta4), uint256)
            extcall IPoolManager(POOL_MANAGER_ADDR).sync(currency, skip_contract_check=True)
            extcall IERC20(currency).transfer(POOL_MANAGER_ADDR, owed4, default_return_value=True, skip_contract_check=True)
            extcall IPoolManager(POOL_MANAGER_ADDR).settle(skip_contract_check=True)
        elif delta4 > 0:
            extcall IPoolManager(POOL_MANAGER_ADDR).take(currency, self, convert(delta4, uint256), skip_contract_check=True)

    return unsafe_add(offset, SIZE_V4_SETTLE_DELTA)


@internal
def _cmd_v4_settle_all(data: Bytes[MAX_COMMANDS_LENGTH], offset: uint256) -> uint256:
    """V4_SETTLE_ALL: [0x57]"""
    self._auto_settle_touched()
    return unsafe_add(offset, SIZE_V4_SETTLE_ALL)


@internal
def _cmd_v4_mint_compact(data: Bytes[MAX_COMMANDS_LENGTH], offset: uint256) -> uint256:
    """
    V4_MINT_COMPACT: [0x58][currency_idx:1][recipient_idx:1][amount:12]

    Convert a positive PM delta into ERC6909 balance for `recipient`.
    No physical token transfer — the asset stays inside the PoolManager
    as an accounting entry (ERC6909 share).

    Replaces: take + sync + transfer + settle (4 ops → 1 op).
    Must be called inside the unlock callback (after V4_SWAP).
    """
    # Read indices + amount as a single 14-byte slice (saves 1 bounds check)
    # Layout: [currency_idx:1][recipient_idx:1][amount:12]
    ira: uint256 = convert(slice(data, unsafe_add(offset, WIDTH_UINT8), 14), uint256)

    # Inline sentinel resolution for currency (saves function call overhead)
    c_idx: uint256 = ira >> 104
    currency: address = empty(address)
    if c_idx >= SENTINEL_THRESHOLD:
        if c_idx == V4_WETH_SENTINEL:
            currency = WETH_ADDR
        elif c_idx == V4_SELF_SENTINEL:
            currency = self
        elif c_idx == V4_NATIVE_SENTINEL:
            currency = NATIVE_ADDRESS
        elif c_idx == V4_PM_SENTINEL:
            currency = POOL_MANAGER_ADDR
        else:
            raise InvalidCommand(opcode=c_idx)
    else:
        currency = self.t_addresses[c_idx]

    amount: uint256 = ira & 79228162514264337593543950335

    # Inline sentinel resolution for recipient
    r_idx: uint256 = (ira >> 96) & 255
    recipient: address = empty(address)
    if r_idx >= SENTINEL_THRESHOLD:
        if r_idx == V4_SELF_SENTINEL:
            recipient = self
        elif r_idx == V4_WETH_SENTINEL:
            recipient = WETH_ADDR
        elif r_idx == V4_PM_SENTINEL:
            recipient = POOL_MANAGER_ADDR
        elif r_idx == V4_NATIVE_SENTINEL:
            recipient = NATIVE_ADDRESS
        else:
            raise InvalidCommand(opcode=r_idx)
    else:
        recipient = self.t_addresses[r_idx]

    # ERC6909 id = uint160(currency_address), per CurrencyLibrary.toId()
    extcall IPoolManager(POOL_MANAGER_ADDR).mint(
        recipient,
        convert(convert(currency, uint160), uint256),
        amount,
        skip_contract_check=True,
    )

    # No t_v4_currencies_touched tracking.

    return unsafe_add(offset, SIZE_V4_MINT_COMPACT)


@internal
def _cmd_v4_burn_compact(data: Bytes[MAX_COMMANDS_LENGTH], offset: uint256) -> uint256:
    """
    V4_BURN_COMPACT: [0x59][currency_idx:1][amount:12]

    Convert ERC6909 balance into a payable PM delta.
    The executor always burns its OWN ERC6909 tokens, which adds to
    the PM's delta for `currency`. The PM then owes the executor that
    currency (retrievable via take).

    Replaces: sync + transfer + settle (3 ops → 1 op) for deep-settled tokens.
    """
    # Read currency_idx + amount as a single 13-byte slice (saves 1 bounds check)
    # Layout: [currency_idx:1][amount:12]
    ca: uint256 = convert(slice(data, unsafe_add(offset, WIDTH_UINT8), 13), uint256)
    currency: address = self._lookup_address(ca >> 96)
    amount: uint256 = ca & 79228162514264337593543950335

    # Always burn from self — the executor only holds its own ERC6909 tokens.
    # Hardcoding self eliminates the risk of accidentally burning another
    # account's tokens and saves 1 byte of calldata per burn command.
    extcall IPoolManager(POOL_MANAGER_ADDR).burn(
        self,
        convert(convert(currency, uint160), uint256),
        amount,
        skip_contract_check=True,
    )

    # No t_v4_currencies_touched tracking.

    return unsafe_add(offset, SIZE_V4_BURN_COMPACT)


@internal
def _execute_command_at(data: Bytes[MAX_COMMANDS_LENGTH], offset: uint256) -> uint256:
    """
    Decode and dispatch one command at the given offset in the data stream.

    Each command is handled by a dedicated @internal function. This thin
    dispatch function only reads the opcode byte and delegates, keeping
    its own memory footprint minimal. Function extraction enables Venom's
    liveness analysis to see that different command handlers' alloca regions
    are mutually exclusive, potentially allowing memory reuse.

    Returns the offset of the next command (i.e., offset + command_size).
    """
    # Read opcode as uint256 to enable range-based dispatch via shift.
    # command_hi = command >> 4 gives the high nibble (5, 4, 3, etc.).
    command: uint256 = convert(slice(data, offset, WIDTH_UINT8), uint256)
    command_hi: uint256 = command >> 4

    # Two-level dispatch: match high nibble first (1 comparison), then
    # exact match within group. Opcodes are grouped by protocol at 0x10
    # boundaries. This reduces average comparison count from ~5.3 (flat
    # dispatch) to ~3.8, saving dispatch overhead gas.
    #
    # High nibbles: 5=V4 settlement, 4=V4 swap, 3=V3 swap, 2=V2 swap,
    #               1=ERC20/ETH, 0=Control

    if command_hi == 5:  # 0x50-0x5F: V4 settlement (most frequently dispatched)
        # Sub-order by frequency: V4_TAKE_COMPACT(26) > V4_UNLOCK(20) >
        # V4_SYNC(14) > V4_SETTLE(14) > V4_SETTLE_DELTA(10)
        if command == 82:  # 0x52 V4_TAKE_COMPACT
            return self._cmd_v4_take_compact(data, offset)
        elif command == 80:  # 0x50 V4_UNLOCK
            return self._cmd_v4_unlock(data, offset)
        elif command == 84:  # 0x54 V4_SYNC
            return self._cmd_v4_sync(data, offset)
        elif command == 85:  # 0x55 V4_SETTLE
            return self._cmd_v4_settle(data, offset)
        elif command == 86:  # 0x56 V4_SETTLE_DELTA
            return self._cmd_v4_settle_delta(data, offset)
        elif command == 83:  # 0x53 V4_TAKE_DELTA
            return self._cmd_v4_take_delta(data, offset)
        elif command == 88:  # 0x58 V4_MINT_COMPACT
            return self._cmd_v4_mint_compact(data, offset)
        elif command == 87:  # 0x57 V4_SETTLE_ALL
            return self._cmd_v4_settle_all(data, offset)
        elif command == 81:  # 0x51 V4_TAKE
            return self._cmd_v4_take(data, offset)
        elif command == 89:  # 0x59 V4_BURN_COMPACT
            return self._cmd_v4_burn_compact(data, offset)
    elif command_hi == 4:  # 0x40-0x4F: V4 swap group
        if command == 64:  # 0x40 V4_SWAP_COMPACT
            return self._cmd_v4_swap_compact(data, offset)
        elif command == 65:  # 0x41 V4_SWAP_DYNAMIC
            return self._cmd_v4_swap_dynamic(data, offset)
        elif command == 66:  # 0x42 V4_BATCH
            return self._cmd_v4_batch(data, offset)
    elif command_hi == 3:  # 0x30-0x3F: V3 swap group
        if command == 48:  # 0x30 V3_SWAP_COMPACT
            return self._cmd_v3_swap_compact(data, offset)
        elif command == 49:  # 0x31 V3_SWAP_DELTA
            return self._cmd_v3_swap_delta(data, offset)
    elif command_hi == 2:  # 0x20-0x2F: V2 swap group
        if command == 34:  # 0x22 V2_SWAP_DIRECT
            return self._cmd_v2_swap_direct(data, offset)
        elif command == 32:  # 0x20 V2_SWAP_COMPACT
            return self._cmd_v2_swap_compact(data, offset)
        elif command == 33:  # 0x21 V2_SWAP_CALC
            return self._cmd_v2_swap_calc(data, offset)
    elif command == 16:  # 0x10 ERC20_TRANSFER
        return self._cmd_erc20_transfer(data, offset)
    elif command == 17:  # 0x11 ERC20_XFER_BALANCE
        return self._cmd_erc20_xfer_balance(data, offset)
    elif command == 18:  # 0x12 WETH_DEPOSIT
        return self._cmd_weth_deposit(data, offset)
    elif command == 19:  # 0x13 WETH_WITHDRAW
        return self._cmd_weth_withdraw(data, offset)
    elif command == 20:  # 0x14 WETH_DEPOSIT_ALL
        return self._cmd_weth_deposit_all(data, offset)
    elif command == 21:  # 0x15 WETH_WITHDRAW_ALL
        return self._cmd_weth_withdraw_all(data, offset)
    elif command == 22:  # 0x16 SEND_ETH
        return self._cmd_send_eth(data, offset)
    elif command == 23:  # 0x17 SEND_ETH_ALL
        return self._cmd_send_eth_all(data, offset)

    raise InvalidCommand(opcode=command)  # Invalid command opcode


@external
def withdraw(amount: uint256, destination: address):
    """Withdraw ETH or WETH to destination. Owner only."""
    assert msg.sender == OWNER_ADDR, Unauthorized(caller=msg.sender)

    eth_balance: uint256 = self.balance
    weth_balance: uint256 = staticcall IERC20(WETH_ADDR).balanceOf(self)
    assert amount <= unsafe_add(eth_balance, weth_balance), InsufficientBalance(amount=amount, available=unsafe_add(eth_balance, weth_balance))

    if amount > eth_balance:
        extcall IWETH(WETH_ADDR).withdraw(
            unsafe_sub(amount, eth_balance),
            skip_contract_check=True,
        )

    raw_call(
        destination,
        b"",
        value=amount,
    )


@internal
def _combined_balance(check_mode: uint256) -> uint256:
    """Read the executor's combined WETH+ETH (or ERC6909 WETH) balance.

    Used at BOTH the start (combined_before) and end (combined_after) of the
    execute() slow path so the profit assert and bribe compute on the TRUE
    delta — never on an operator-supplied `expected_value` that may be
    misconfigured (the U3WVLL defect: `expected_value=0` silently skipped the
    profit check and over-bribed). Reading the on-chain balance at the start
    costs one cold balanceOf (~2600 gas) the first time; the end read is warm
    (the stream touches WETH/ERC6909) at ~100 gas.

    Flash paths start at 0 (no-prefund architecture) so combined_before=0 —
    the assert `combined_after >= 0` is trivially true (no regression vs the
    M1 retraction); self-fund paths start >0 so the assert is the active
    protection (a money-losing self-fund tx reverts).
    """
    if check_mode == 2:
        # ERC6909 WETH held in the PoolManager.
        return staticcall IERC6909Claims(POOL_MANAGER_ADDR).balanceOf(
            self, convert(convert(WETH_ADDR, uint160), uint256)
        )
    # WETH + ETH combined (check_mode == 1).
    return unsafe_add(staticcall IERC20(WETH_ADDR).balanceOf(self), self.balance)


@external
@payable
def execute(commands: Bytes[MAX_COMMANDS_LENGTH], config: uint256 = 0) -> uint256:
    """
    Execute a command stream for arbitrage.

    The stream starts with a preprocessing section (SET_ADDRESS commands)
    followed by 0xFF separator, then execution commands:

        [SET_ADDRESS cmds][0xFF][execution cmds]

    If the first byte is an execution opcode (not 0x00/0xFF), no
    preprocessing runs and the entire stream is execution.

    Preprocessing opcodes:
      0x00  SET_ADDRESS  [address:20]  Append address to lookup table
      0xFF  BEGIN_EXECUTION            End preprocessing / start execution

    All other configuration (profit check, bribes) is packed into the
    ABI config parameter — decoded for free without command-stream overhead:

      bits 0-7:    check_mode
                    0 = skip (no balance check)
                    1 = check WETH + ETH combined balance
                    2 = check ERC6909 WETH balance (PM.balanceOf(self, weth_id));
                        intended for pure-V4 paths that end with V4_MINT_COMPACT
                        profit capture (the ERC6909 slot is warm from the MINT,
                        ~3,500 gas cheaper than a cold WETH.balanceOf).
                        NOTE (L8): mode-2 reads the ERC6909 balance — the command
                        stream MUST mint the profit to self via V4_MINT_COMPACT
                        (or already hold ERC6909 WETH) for the check to be
                        meaningful. Reading mode-2 on a path that ends with
                        physical WETH TAKE will see a stale/zero ERC6909 balance
                        and fail. Mode-1 is the default for mixed V2/V3/V4 paths.
      bits 8-23:   bribe_bips (0 = no bribe, 1-10000 = basis points; >10000 reverts BipsTooHigh)
      bits 24-31:  bribe_recipient_idx (0 = block.coinbase / builder, 1-31 = address table index)
      bits 32-255: expected_value (IGNORED — kept for config-ABI compatibility;
                   the contract reads its own combined balance at start+end)

    Note on the profit check (U3WVLL defect fix): the contract reads its OWN
    combined balance at the start (combined_before) and end (combined_after)
    of the slow path — NOT an operator-supplied `expected_value`. The profit
    assert `combined_after >= combined_before` is UNCONDITIONAL (no
    `expected_value > 0` guard): a money-losing self-fund tx reverts to
    protect the operator. For flash paths combined_before=0 (no-prefund
    architecture) so the assert is trivially true; a losing flash path reverts
    at the protocol layer (flash-loan repayment) before reaching the check.
    Bribes compute on the TRUE profit (combined_after - combined_before),
    eliminating the `expected_value=0` over-bribe footgun. The rare "sweep
    accumulated profit to another address" case (which requires the assert
    defeated) is a deferred explicit opt-in, NOT `expected_value=0`.

    Examples:
      0                                              → fast path: skip check, no bribe
      1                                              → WETH+ETH profit check, no bribe
      (500 << 8) | 2                                 → ERC6909 check, 5% coinbase bribe
      (500 << 8) | (3 << 24) | 1                     → WETH+ETH check, 5% bribe to addr[3]

    Owner-only. Returns the profit (balance increase).
    """
    assert msg.sender == OWNER_ADDR, Unauthorized(caller=msg.sender)

    # Unpack config: ABI decoding is free — no slice/convert/dispatch overhead.
    # check_mode: 0=skip, 1=WETH+ETH, 2=ERC6909 WETH.
    check_mode: uint256 = config & 255

    offset: uint256 = self._preprocess(commands)

    # Fast path: when no balance check needed AND no bribe, avoid all
    # balanceOf reads and post-processing. check_mode=0 and bips=0 means
    # operator confirms starting balance externally (no on-chain check).
    bribe_bips: uint256 = (config >> 8) & 65535
    if check_mode == 0 and bribe_bips == 0:
        for _: uint256 in range(MAX_COMMANDS_LENGTH):
            offset = self._execute_command_at(commands, offset)
            if offset >= len(commands):
                break
        return 0

    # Slow path: balance check or bribe needed.
    # (Fast path already returned when check_mode==0 and bribe_bips==0.)
    # U3WVLL defect fix: the contract reads its OWN combined balance at the
    # start (combined_before) and end (combined_after) of the slow path, so
    # the profit assert + bribe compute on the TRUE delta. The operator's
    # `expected_value` (config bits 32+) is IGNORED — it was the footgun
    # (expected_value=0 silently skipped the assert and over-bribed; the
    # contract couldn't distinguish a 0-balance flash path from a
    # misconfigured 0). For flash paths combined_before=0 (no-prefund
    # architecture); for self-fund paths combined_before>0 (the funded entry
    # capital) so the assert is the active money-loss protection. A losing
    # flash path reverts at the protocol layer (flash-loan repayment) before
    # reaching here. See .auto_archived/m1-profit-guard-retraction.md (the M1
    # retraction opposed forcing expected_value>0, which broke flash paths;
    # reading on-chain instead does NOT break them — combined_before=0 is
    # correct for flash paths).
    combined_before: uint256 = self._combined_balance(check_mode)
    for _: uint256 in range(MAX_COMMANDS_LENGTH):
        offset = self._execute_command_at(commands, offset)
        if offset >= len(commands):
            break
    combined_after: uint256 = self._combined_balance(check_mode)
    # Unconditional assert (no `if expected_value > 0` guard): a money-losing
    # tx reverts to protect the operator. For flash paths (combined_before=0)
    # this is `combined_after >= 0` (trivially true for uint); for self-fund
    # paths it is the active floor. The rare "send accumulated profit to
    # another address" sweep case needs this defeated — that is an explicit
    # opt-in (see the deferred sweep mode, not `expected_value=0`).
    assert combined_after >= combined_before, InsufficientProfit(actual=combined_after, expected=combined_before)
    
    # Bribes send a portion of this transaction's profit: profit * bips / 10000.
    # If the executor's ETH balance is insufficient, withdraws WETH (up to
    # current WETH balance) to cover the shortfall. If total ETH+WETH still
    # doesn't cover the desired bribe, sends whatever is available (never reverts).
    if bribe_bips > 0:
        # Bound bribe_bips: only matters when a bribe was actually requested.
        # Without this, bribe_bips > 10000 would compute bribe_amount = bips*profit/10000
        # > profit, over-bribing (draining more than the actual profit).
        assert bribe_bips <= 10_000, BipsTooHigh(bips=bribe_bips)
        # Resolve bribe recipient: idx=0 = block.coinbase (builder bribe),
        # idx>0 = address table lookup. TLOAD is 100 gas (warm) — t_addresses
        # was just populated by _preprocess.
        # Field is 8 bits but only 0..31 are valid (0=coinbase, 1..31=table);
        # bytes 32..255 (incl. sentinels 0xFC..0xFF) are not meaningful bribe
        # targets and would otherwise bounds-revert opaquely. Fail closed here.
        # (M2: idx=0 previously sent ETH to address(0) — a burn, NOT a builder
        # bribe. block.coinbase pays the actual block builder.)
        bribe_recipient_idx: uint256 = (config >> 24) & 255
        assert bribe_recipient_idx < MAX_INDEXED_ADDRESSES, InvalidCommand(opcode=bribe_recipient_idx)
        bribe_recipient: address = block.coinbase
        if bribe_recipient_idx > 0:
            bribe_recipient = self.t_addresses[bribe_recipient_idx]
        # idx=0 → bribe_recipient stays block.coinbase → raw_call pays the builder

        profit: uint256 = 0
        if combined_after >= combined_before:
            profit = unsafe_sub(combined_after, combined_before)

        if profit > 0:
            bribe_amount: uint256 = unsafe_mul(bribe_bips, profit) // 10_000
            if bribe_amount > 0:
                if bribe_amount > self.balance:
                    weth_available: uint256 = staticcall IERC20(WETH_ADDR).balanceOf(self)
                    if weth_available > 0:
                        extcall IWETH(WETH_ADDR).withdraw(
                            min(weth_available, unsafe_sub(bribe_amount, self.balance)),
                            skip_contract_check=True,
                        )
                if bribe_amount > self.balance:
                    bribe_amount = self.balance
                if bribe_amount > 0:
                    raw_call(
                        bribe_recipient, 
                        b"", 
                        value=bribe_amount,
                    )

    return unsafe_sub(combined_after, combined_before)


@external
@payable
def uniswapV2Call(
    sender: address,
    amount0Out: uint256,
    amount1Out: uint256,
    data: Bytes[MAX_COMMANDS_LENGTH],
):
    """Uniswap V2 & SushiSwap V2 flash borrow callback.
    
    If data is exactly 1 byte (auto-pay sentinel), computes owed amount
    from reserves + fee and auto-pays. Otherwise, processes command stream.
    """
    assert convert(msg.sender, uint256) == self.t_callback_packed % CALLBACK_FEE_SHIFT, InvalidCallback(caller=msg.sender)
    # Inline _v2_callback_handler to save INVOKE overhead per V2 callback.
    if len(data) == WIDTH_UINT8:
        self._v2_auto_pay(msg.sender, amount0Out, amount1Out)
    else:
        offset: uint256 = 0
        for _: uint256 in range(MAX_COMMANDS_LENGTH):
            offset = self._execute_command_at(data, offset)
            if offset >= len(data):
                break


@external
@payable
def hook(
    sender: address,
    amount0Out: uint256,
    amount1Out: uint256,
    data: Bytes[MAX_COMMANDS_LENGTH],
):
    """Hook callback (same as uniswapV2Call, used by some V2 forks)."""
    assert convert(msg.sender, uint256) == self.t_callback_packed % CALLBACK_FEE_SHIFT, InvalidCallback(caller=msg.sender)
    if len(data) == WIDTH_UINT8:
        self._v2_auto_pay(msg.sender, amount0Out, amount1Out)
    else:
        offset: uint256 = 0
        for _: uint256 in range(MAX_COMMANDS_LENGTH):
            offset = self._execute_command_at(data, offset)
            if offset >= len(data):
                break


@external
@payable
def pancakeCall(
    sender: address,
    amount0Out: uint256,
    amount1Out: uint256,
    data: Bytes[MAX_COMMANDS_LENGTH],
):
    """PancakeSwap V2 flash borrow callback."""
    assert convert(msg.sender, uint256) == self.t_callback_packed % CALLBACK_FEE_SHIFT, InvalidCallback(caller=msg.sender)
    if len(data) == WIDTH_UINT8:
        self._v2_auto_pay(msg.sender, amount0Out, amount1Out)
    else:
        offset: uint256 = 0
        for _: uint256 in range(MAX_COMMANDS_LENGTH):
            offset = self._execute_command_at(data, offset)
            if offset >= len(data):
                break


@external
@payable
def uniswapV3SwapCallback(
    amount0_delta: int256,
    amount1_delta: int256,
    data: Bytes[MAX_COMMANDS_LENGTH],
):
    """Uniswap V3 & SushiSwap V3 swap callback.
    
    If data is empty (V3_SWAP called with forward_len=0), auto-pays the pool
    using the callback's amount parameters — no forward_data encoding needed.
    Otherwise, processes the command stream in data (legacy behavior).
    """
    assert convert(msg.sender, uint256) == self.t_callback_packed % CALLBACK_FEE_SHIFT, InvalidCallback(caller=msg.sender)
    if len(data) == 0:
        # Inline _v3_auto_pay: pays the V3 pool from callback deltas
        if amount0_delta > 0:
            extcall IERC20(staticcall IUniswapV3Pool(msg.sender).token0()).transfer(
                msg.sender, convert(amount0_delta, uint256), default_return_value=True, skip_contract_check=True,
            )
        else:
            extcall IERC20(staticcall IUniswapV3Pool(msg.sender).token1()).transfer(
                msg.sender, convert(amount1_delta, uint256), default_return_value=True, skip_contract_check=True,
            )
    else:
        offset: uint256 = 0
        for _: uint256 in range(MAX_COMMANDS_LENGTH):
            offset = self._execute_command_at(data, offset)
            if offset >= len(data):
                break


@external
@payable
def pancakeV3SwapCallback(
    amount0_delta: int256,
    amount1_delta: int256,
    data: Bytes[MAX_COMMANDS_LENGTH],
):
    """PancakeSwap V3 swap callback. See uniswapV3SwapCallback for auto-pay."""
    assert convert(msg.sender, uint256) == self.t_callback_packed % CALLBACK_FEE_SHIFT, InvalidCallback(caller=msg.sender)
    if len(data) == 0:
        # Inline _v3_auto_pay: pays the V3 pool from callback deltas
        if amount0_delta > 0:
            extcall IERC20(staticcall IUniswapV3Pool(msg.sender).token0()).transfer(
                msg.sender, convert(amount0_delta, uint256), default_return_value=True, skip_contract_check=True,
            )
        else:
            extcall IERC20(staticcall IUniswapV3Pool(msg.sender).token1()).transfer(
                msg.sender, convert(amount1_delta, uint256), default_return_value=True, skip_contract_check=True,
            )
    else:
        offset: uint256 = 0
        for _: uint256 in range(MAX_COMMANDS_LENGTH):
            offset = self._execute_command_at(data, offset)
            if offset >= len(data):
                break


@external
@payable
def unlockCallback(data: Bytes[MAX_COMMANDS_LENGTH]) -> Bytes[MAX_COMMANDS_LENGTH]:
    """
    Uniswap V4 PoolManager unlock callback
    """
    assert msg.sender == POOL_MANAGER_ADDR, InvalidCallback(caller=msg.sender)
    offset: uint256 = 0
    for _: uint256 in range(MAX_COMMANDS_LENGTH):
        offset = self._execute_command_at(data, offset)
        if offset >= len(data):
            break
    return b""


@external
@payable
def __default__():
    """Accept plain ETH transfers from trusted callers; revert otherwise.

    Two legitimate callers deliver ETH to the executor via raw_call (which
    hits this fallback):
      1. PoolManager `take(NATIVE_ADDRESS, executor, amt)` — native-take path.
      2. WETH `withdraw` — credits ETH to the withdrawer via raw_call.

    Any other plain-ETH transfer is a donation that would inflate the mode-1
    profit check (`combined_after = WETH.balanceOf(self) + self.balance`) and
    could mask a losing arbitrage or trigger a fake bribe payout. Reject those.
    Unknown-function calls (msg.data != empty) are rejected as before.
    """
    assert len(msg.data) == 0, NotPlainEthTransfer()
    assert msg.sender == POOL_MANAGER_ADDR or msg.sender == WETH_ADDR, Unauthorized(caller=msg.sender)
