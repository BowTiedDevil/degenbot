# pragma version ~=0.4.0
# pragma evm-version cancun

"""
Cmd Executor — command-bytecode VM executor for Uniswap V2/V3/V4 arbitrage.

Uses a compact command stream instead of a generic payload queue.
Off-chain code pre-computes the entire execution plan and encodes
it as a sequence of commands. The contract is a pure VM — it decodes
and executes commands without on-chain decision-making.

Key differences from tstore_executor:
- No payload queue — command stream is the continuation mechanism
- No delta ledger — all amounts pre-computed off-chain
- No auto-pay — explicit ERC-20 transfers in command stream
- No Phase 0/1/2/3 — commands execute in encoded order
- No will_callback flag — targets auto-registered on swap commands
- Callback functions forward command stream through data parameter
  (using the full bytes passthrough that V2/V3/V4 support)

Command set:
  0x00  V2_SWAP           V2 pair swap with optional flash borrow
  0x01  V3_SWAP           V3 pool swap with callback
  0x02  V4_SWAP           V4 PoolManager swap (inside unlock)
  0x03  V4_TAKE           V4 take from PoolManager (inside unlock)
  0x04  V4_SYNC           V4 sync at PoolManager (inside unlock)
  0x05  V4_SETTLE         V4 settle (inside unlock, after sync+transfer)
  0x06  V4_SETTLE_NATIVE  V4 settle native ETH (inside unlock)
  0x07  ERC20_TRANSFER    ERC-20 token transfer (any context)
  0x08  WETH_DEPOSIT      Wrap ETH to WETH
  0x09  WETH_WITHDRAW     Unwrap WETH to ETH
  0x0A  V4_UNLOCK         Enter PoolManager unlock context
  0xFF  SEPARATOR         Command terminator

Design constraint: V4_SWAP, V4_TAKE, V4_SYNC, V4_SETTLE, and V4_SETTLE_NATIVE
must only appear inside an unlockCallback context (triggered by V4_UNLOCK or as
the first command when execute() detects a V4-first path). These commands use
msg.sender as the PoolManager address, which is correct inside unlockCallback
(where msg.sender = PM) but incorrect in the outer execute() context.

For V3→V4 paths: the V3 swap runs in the outer context, then V4_UNLOCK enters
the PM context. All sync/settle/swap/take commands go inside the inner stream.
ERC20_TRANSFER is context-agnostic and works in both outer and inner contexts.
"""

from ethereum.ercs import IERC20

interface IUniswapV2Pair:
    def swap(amount0Out: uint256, amount1Out: uint256, to: address, data: Bytes[MAX_COMMANDS_LENGTH]): nonpayable

interface IUniswapV3Pool:
    def token0() -> address: view
    def token1() -> address: view
    def swap(
        recipient: address,
        zero_for_one: bool,
        amount_specified: int256,
        sqrt_price_limit_x96: uint160,
        data: Bytes[MAX_COMMANDS_LENGTH],
    ) -> (int256, int256): nonpayable

interface IPoolManager:
    def settle() -> uint256: payable
    def sync(currency: address): nonpayable
    def take(currency: address, to: address, amount: uint256): nonpayable
    def swap(
        key: PoolKey,
        params: SwapParams,
        hook_data: Bytes[32],
    ) -> int256: nonpayable
    def unlock(data: Bytes[MAX_COMMANDS_LENGTH]) -> Bytes[MAX_COMMANDS_LENGTH]: nonpayable

interface IWETH:
    def deposit(): payable
    def withdraw(amount: uint256): nonpayable

struct PoolKey:
    currency0: address
    currency1: address
    fee: uint24
    tick_spacing: int24
    hooks: address

struct SwapParams:
    zero_for_one: bool
    amount_specified: int256
    sqrt_price_limit_x96: uint160

# ── Immutables ──

OWNER_ADDR: immutable(address)
WETH_ADDR: immutable(address)

# ── Constants ──

NATIVE_ADDRESS: constant(address) = empty(address)
MAX_INDEXED_ADDRESSES: constant(uint256) = 32
MAX_COMMANDS_LENGTH: constant(uint256) = 4096
MAX_COMMANDS: constant(uint256) = MAX_COMMANDS_LENGTH // 2

# Command opcodes
COMMAND_V2_SWAP: constant(bytes1) = 0x00
COMMAND_V3_SWAP: constant(bytes1) = 0x01
COMMAND_V4_SWAP: constant(bytes1) = 0x02
COMMAND_V4_TAKE: constant(bytes1) = 0x03
COMMAND_V4_SYNC: constant(bytes1) = 0x04
COMMAND_V4_SETTLE: constant(bytes1) = 0x05
COMMAND_V4_SETTLE_NATIVE: constant(bytes1) = 0x06
COMMAND_ERC20_TRANSFER: constant(bytes1) = 0x07
COMMAND_WETH_DEPOSIT: constant(bytes1) = 0x08
COMMAND_WETH_WITHDRAW: constant(bytes1) = 0x09
COMMAND_V4_UNLOCK: constant(bytes1) = 0x0A
COMMAND_SEPARATOR: constant(bytes1) = 0xFF

# V3 amount sign conventions (V3: positive = exact-input)
MIN_SQRT_PRICE_X96: constant(uint160) = 4295128739
MAX_SQRT_PRICE_X96: constant(uint160) = 1461446703485210103287273052203988822378723970342

# ── Transient state (cleared every transaction) ──

t_allowed_callback_addresses: transient(HashMap[address, bool])
t_addresses: transient(DynArray[address, MAX_INDEXED_ADDRESSES])


# ── Constructor ──


@payable
@deploy
def __init__(weth: address):
    OWNER_ADDR = msg.sender
    WETH_ADDR = weth

    if msg.value > 0:
        extcall IWETH(WETH_ADDR).deposit(
            value=msg.value,
            skip_contract_check=True,
        )


# ── Internal: Command Processor ──


@internal
def _process_commands(data: Bytes[MAX_COMMANDS_LENGTH]):
    """Process a command stream by iterating _execute_commands until exhausted."""
    remaining: Bytes[MAX_COMMANDS_LENGTH] = data
    for i: uint256 in range(MAX_COMMANDS):
        if len(remaining) == 0:
            break
        remaining = self._execute_commands(remaining)


@internal
def _execute_commands(data: Bytes[MAX_COMMANDS_LENGTH]) -> Bytes[MAX_COMMANDS_LENGTH]:
    """
    Decode and execute the first command in the data stream.

    Returns the remaining unprocessed bytes after the command's separator.
    For callback commands (V2_SWAP, V3_SWAP, V4_UNLOCK), the forward_data
    is extracted and passed to the external call. The remaining bytes after
    the separator belong to the calling context (not the callback).
    """
    command: bytes1 = convert(slice(data, 0, 1), bytes1)

    if command == COMMAND_V2_SWAP:
        # ── V2_SWAP ──
        # [0x00][pool_idx:1][zfo:1][amount_out:32][recipient_idx:1]
        # [forward_len:2][forward_data:N][0xFF]
        pool_idx: uint256 = convert(slice(data, 1, 1), uint256)
        zero_for_one: bool = convert(slice(data, 2, 1), bool)
        amount_out: uint256 = convert(slice(data, 3, 32), uint256)
        recipient_idx: uint256 = convert(slice(data, 35, 1), uint256)
        forward_len: uint256 = convert(slice(data, 36, 2), uint256)

        pool: address = self.t_addresses[pool_idx]
        recipient: address = self.t_addresses[recipient_idx]

        # Build V2 swap params
        amount0_out: uint256 = 0
        amount1_out: uint256 = 0
        if zero_for_one:
            amount1_out = amount_out
        else:
            amount0_out = amount_out

        # Forward data goes to V2 callback (triggers flash borrow if non-empty)
        forward_data: Bytes[MAX_COMMANDS_LENGTH] = slice(
            data, 38, forward_len
        ) if forward_len > 0 else b""

        terminator_index: uint256 = 38 + forward_len
        assert convert(slice(data, terminator_index, 1), bytes1) == COMMAND_SEPARATOR, "V2_SWAP: missing separator"

        # Register callback
        self.t_allowed_callback_addresses[pool] = True

        # Call V2 pair.swap()
        # swap(uint256,uint256,address,bytes)
        extcall IUniswapV2Pair(pool).swap(
            amount0_out, amount1_out, recipient, forward_data
        )

        # Return remaining bytes after separator
        remaining_start: uint256 = terminator_index + 1
        remaining_length: uint256 = len(data) - remaining_start
        if remaining_length > 0:
            return slice(data, remaining_start, remaining_length)
        else:
            return b''

    elif command == COMMAND_V3_SWAP:
        # ── V3_SWAP ──
        # [0x01][pool_idx:1][zfo:1][amount_specified:32][sqrt_limit:20]
        # [recipient_idx:1][forward_len:2][forward_data:N][0xFF]
        pool_idx: uint256 = convert(slice(data, 1, 1), uint256)
        zero_for_one: bool = convert(slice(data, 2, 1), bool)
        amount_specified: int256 = convert(slice(data, 3, 32), int256)
        sqrt_price_limit_x96: uint160 = convert(slice(data, 35, 20), uint160)
        recipient_idx: uint256 = convert(slice(data, 55, 1), uint256)
        forward_len: uint256 = convert(slice(data, 56, 2), uint256)

        pool: address = self.t_addresses[pool_idx]
        recipient: address = self.t_addresses[recipient_idx]

        forward_data: Bytes[MAX_COMMANDS_LENGTH] = slice(
            data, 58, forward_len
        ) if forward_len > 0 else b""

        terminator_index: uint256 = 58 + forward_len
        assert convert(slice(data, terminator_index, 1), bytes1) == COMMAND_SEPARATOR, "V3_SWAP: missing separator"

        # Register callback
        self.t_allowed_callback_addresses[pool] = True

        # Call V3 pool.swap()
        extcall IUniswapV3Pool(pool).swap(
            recipient,
            zero_for_one,
            amount_specified,
            sqrt_price_limit_x96,
            forward_data,
        )

        remaining_start: uint256 = terminator_index + 1
        remaining_length: uint256 = len(data) - remaining_start
        if remaining_length > 0:
            return slice(data, remaining_start, remaining_length)
        else:
            return b''

    elif command == COMMAND_V4_SWAP:
        # ── V4_SWAP ──
        # [0x02][c0_idx:1][c1_idx:1][fee:3][ts:3][hooks_idx:1][zfo:1]
        # [amount_specified:32][sqrt_limit:20][0xFF]
        c0_idx: uint256 = convert(slice(data, 1, 1), uint256)
        c1_idx: uint256 = convert(slice(data, 2, 1), uint256)
        fee: uint24 = convert(slice(data, 3, 3), uint24)
        tick_spacing: int24 = convert(slice(data, 6, 3), int24)
        hooks_idx: uint256 = convert(slice(data, 9, 1), uint256)
        zero_for_one: bool = convert(slice(data, 10, 1), bool)
        amount_specified: int256 = convert(slice(data, 11, 32), int256)
        sqrt_price_limit_x96: uint160 = convert(slice(data, 43, 20), uint160)

        c0: address = self.t_addresses[c0_idx]
        c1: address = self.t_addresses[c1_idx]
        hooks: address = self.t_addresses[hooks_idx]

        # Build PoolKey — ensure currency0 < currency1
        pool_key: PoolKey = PoolKey(
            currency0=c0 if convert(c0, uint256) < convert(c1, uint256) else c1,
            currency1=c0 if convert(c0, uint256) > convert(c1, uint256) else c1,
            fee=fee,
            tick_spacing=tick_spacing,
            hooks=hooks,
        )

        swap_params: SwapParams = SwapParams(
            zero_for_one=zero_for_one,
            amount_specified=amount_specified,
            sqrt_price_limit_x96=sqrt_price_limit_x96,
        )

        # Execute V4 swap — discard BalanceDelta return value
        # Off-chain code pre-computed all settlement amounts
        pool_manager: address = msg.sender
        extcall IPoolManager(pool_manager).swap(pool_key, swap_params, b'')

        terminator_index: uint256 = 63
        assert convert(slice(data, terminator_index, 1), bytes1) == COMMAND_SEPARATOR, "V4_SWAP: missing separator"

        remaining_start: uint256 = terminator_index + 1
        remaining_length: uint256 = len(data) - remaining_start
        if remaining_length > 0:
            return slice(data, remaining_start, remaining_length)
        else:
            return b''

    elif command == COMMAND_V4_TAKE:
        # ── V4_TAKE ──
        # [0x03][currency_idx:1][recipient_idx:1][amount:32][0xFF]
        currency_idx: uint256 = convert(slice(data, 1, 1), uint256)
        recipient_idx: uint256 = convert(slice(data, 2, 1), uint256)
        amount: uint256 = convert(slice(data, 3, 32), uint256)

        currency: address = self.t_addresses[currency_idx]
        recipient: address = self.t_addresses[recipient_idx]
        pool_manager: address = msg.sender

        extcall IPoolManager(pool_manager).take(currency, recipient, amount)

        terminator_index: uint256 = 35
        assert convert(slice(data, terminator_index, 1), bytes1) == COMMAND_SEPARATOR, "V4_TAKE: missing separator"

        remaining_start: uint256 = terminator_index + 1
        remaining_length: uint256 = len(data) - remaining_start
        if remaining_length > 0:
            return slice(data, remaining_start, remaining_length)
        else:
            return b''

    elif command == COMMAND_V4_SYNC:
        # ── V4_SYNC ──
        # [0x04][currency_idx:1][0xFF]
        currency_idx: uint256 = convert(slice(data, 1, 1), uint256)
        currency: address = self.t_addresses[currency_idx]
        pool_manager: address = msg.sender

        extcall IPoolManager(pool_manager).sync(currency)

        terminator_index: uint256 = 2
        assert convert(slice(data, terminator_index, 1), bytes1) == COMMAND_SEPARATOR, "V4_SYNC: missing separator"

        remaining_start: uint256 = terminator_index + 1
        remaining_length: uint256 = len(data) - remaining_start
        if remaining_length > 0:
            return slice(data, remaining_start, remaining_length)
        else:
            return b''

    elif command == COMMAND_V4_SETTLE:
        # ── V4_SETTLE ──
        # [0x05][0xFF]
        pool_manager: address = msg.sender
        extcall IPoolManager(pool_manager).settle()

        terminator_index: uint256 = 1
        assert convert(slice(data, terminator_index, 1), bytes1) == COMMAND_SEPARATOR, "V4_SETTLE: missing separator"

        remaining_start: uint256 = terminator_index + 1
        remaining_length: uint256 = len(data) - remaining_start
        if remaining_length > 0:
            return slice(data, remaining_start, remaining_length)
        else:
            return b''

    elif command == COMMAND_V4_SETTLE_NATIVE:
        # ── V4_SETTLE_NATIVE ──
        # [0x06][amount:32][0xFF]
        amount: uint256 = convert(slice(data, 1, 32), uint256)
        pool_manager: address = msg.sender

        extcall IPoolManager(pool_manager).settle(value=amount)

        terminator_index: uint256 = 33
        assert convert(slice(data, terminator_index, 1), bytes1) == COMMAND_SEPARATOR, "V4_SETTLE_NATIVE: missing separator"

        remaining_start: uint256 = terminator_index + 1
        remaining_length: uint256 = len(data) - remaining_start
        if remaining_length > 0:
            return slice(data, remaining_start, remaining_length)
        else:
            return b''

    elif command == COMMAND_ERC20_TRANSFER:
        # ── ERC20_TRANSFER ──
        # [0x07][token_idx:1][recipient_idx:1][amount:32][0xFF]
        token_idx: uint256 = convert(slice(data, 1, 1), uint256)
        recipient_idx: uint256 = convert(slice(data, 2, 1), uint256)
        amount: uint256 = convert(slice(data, 3, 32), uint256)

        token: address = self.t_addresses[token_idx]
        recipient: address = self.t_addresses[recipient_idx]

        extcall IERC20(token).transfer(recipient, amount, default_return_value=True)

        terminator_index: uint256 = 35
        assert convert(slice(data, terminator_index, 1), bytes1) == COMMAND_SEPARATOR, "ERC20_TRANSFER: missing separator"

        remaining_start: uint256 = terminator_index + 1
        remaining_length: uint256 = len(data) - remaining_start
        if remaining_length > 0:
            return slice(data, remaining_start, remaining_length)
        else:
            return b''

    elif command == COMMAND_WETH_DEPOSIT:
        # ── WETH_DEPOSIT ──
        # [0x08][amount:32][0xFF]
        amount: uint256 = convert(slice(data, 1, 32), uint256)

        extcall IWETH(WETH_ADDR).deposit(value=amount)

        terminator_index: uint256 = 33
        assert convert(slice(data, terminator_index, 1), bytes1) == COMMAND_SEPARATOR, "WETH_DEPOSIT: missing separator"

        remaining_start: uint256 = terminator_index + 1
        remaining_length: uint256 = len(data) - remaining_start
        if remaining_length > 0:
            return slice(data, remaining_start, remaining_length)
        else:
            return b''

    elif command == COMMAND_WETH_WITHDRAW:
        # ── WETH_WITHDRAW ──
        # [0x09][amount:32][0xFF]
        amount: uint256 = convert(slice(data, 1, 32), uint256)

        extcall IWETH(WETH_ADDR).withdraw(amount, skip_contract_check=True)

        terminator_index: uint256 = 33
        assert convert(slice(data, terminator_index, 1), bytes1) == COMMAND_SEPARATOR, "WETH_WITHDRAW: missing separator"

        remaining_start: uint256 = terminator_index + 1
        remaining_length: uint256 = len(data) - remaining_start
        if remaining_length > 0:
            return slice(data, remaining_start, remaining_length)
        else:
            return b''

    elif command == COMMAND_V4_UNLOCK:
        # ── V4_UNLOCK ──
        # [0x0A][pm_idx:1][forward_len:2][forward_data:N][0xFF]
        pm_idx: uint256 = convert(slice(data, 1, 1), uint256)
        forward_len: uint256 = convert(slice(data, 2, 2), uint256)

        pool_manager: address = self.t_addresses[pm_idx]
        forward_data: Bytes[MAX_COMMANDS_LENGTH] = slice(
            data, 4, forward_len
        ) if forward_len > 0 else b""

        terminator_index: uint256 = 4 + forward_len
        assert convert(slice(data, terminator_index, 1), bytes1) == COMMAND_SEPARATOR, "V4_UNLOCK: missing separator"

        # Register callback
        self.t_allowed_callback_addresses[pool_manager] = True

        extcall IPoolManager(pool_manager).unlock(forward_data)

        remaining_start: uint256 = terminator_index + 1
        remaining_length: uint256 = len(data) - remaining_start
        if remaining_length > 0:
            return slice(data, remaining_start, remaining_length)
        else:
            return b''

    else:
        err: String[64] = concat("Invalid command: 0x", self._hex_byte(convert(command, uint256)))
        raise err


@internal
@pure
def _hex_byte(val: uint256) -> String[2]:
    """Convert a uint256 (0-255) to a 2-character hex string."""
    nibbles: String[16] = "0123456789abcdef"
    return concat(
        slice(nibbles, val // 16, 1),
        slice(nibbles, val % 16, 1),
    )


# ── External Interface ──


@external
def withdraw(amount: uint256, destination: address):
    """Withdraw ETH or WETH to destination. Owner only."""
    assert msg.sender == OWNER_ADDR, "!OWNER"

    if amount > self.balance:
        extcall IWETH(WETH_ADDR).withdraw(
            amount - self.balance,
            skip_contract_check=True,
        )

    raw_call(
        destination,
        b'',
        value=amount,
    )


@external
@payable
def execute(
    addresses: DynArray[address, MAX_INDEXED_ADDRESSES],
    commands: Bytes[MAX_COMMANDS_LENGTH],
    bribe_bips: uint256 = 0,
    skip_profit_check: bool = False,
) -> uint256:
    """
    Execute a command stream for arbitrage.

    Off-chain code pre-computes the full execution plan, including
    all swap amounts, settlement order, and token transfers. The
    contract simply decodes and executes commands in sequence.

    Owner-only. Returns the profit (combined balance increase).
    """
    assert msg.sender == OWNER_ADDR, "!OWNER"

    # Store address lookup table
    self.t_addresses = addresses

    combined_before: uint256 = staticcall IERC20(WETH_ADDR).balanceOf(self) + self.balance

    # Process the command stream
    self._process_commands(commands)

    combined_after: uint256 = staticcall IERC20(WETH_ADDR).balanceOf(self) + self.balance

    if not skip_profit_check:
        assert combined_after >= combined_before, "balance reduction"

    if bribe_bips > 0:
        raw_call(
            block.coinbase,
            b'',
            value=min(
                msg.value,
                unsafe_mul(
                    bribe_bips,
                    unsafe_sub(combined_after, combined_before),
                ) // 10_000,
            ),
        )

    return combined_after - combined_before


# ── V2 Callbacks ──


@external
@payable
def uniswapV2Call(
    sender: address,
    amount0Out: uint256,
    amount1Out: uint256,
    data: Bytes[MAX_COMMANDS_LENGTH],
):
    """Uniswap V2 & SushiSwap V2 flash borrow callback."""
    assert self.t_allowed_callback_addresses[msg.sender], "Unregistered V2 Callback Address"
    self._process_commands(data)


@external
@payable
def hook(
    sender: address,
    amount0Out: uint256,
    amount1Out: uint256,
    data: Bytes[MAX_COMMANDS_LENGTH],
):
    """Velodrome/Aerodrome V2 flash borrow callback."""
    assert self.t_allowed_callback_addresses[msg.sender], "Unregistered V2 Callback Address"
    self._process_commands(data)


@external
@payable
def pancakeCall(
    sender: address,
    amount0Out: uint256,
    amount1Out: uint256,
    data: Bytes[MAX_COMMANDS_LENGTH],
):
    """PancakeSwap V2 flash borrow callback."""
    assert self.t_allowed_callback_addresses[msg.sender], "Unregistered V2 Callback Address"
    self._process_commands(data)


# ── V3 Callbacks ──


@external
@payable
def uniswapV3SwapCallback(
    amount0_delta: int256,
    amount1_delta: int256,
    data: Bytes[MAX_COMMANDS_LENGTH],
):
    """Uniswap V3 & SushiSwap V3 swap callback."""
    assert self.t_allowed_callback_addresses[msg.sender], "Unregistered V3 Callback Address"
    self._process_commands(data)


@external
@payable
def pancakeV3SwapCallback(
    amount0_delta: int256,
    amount1_delta: int256,
    data: Bytes[MAX_COMMANDS_LENGTH],
):
    """PancakeSwap V3 swap callback."""
    assert self.t_allowed_callback_addresses[msg.sender], "Unregistered V3 Callback Address"
    self._process_commands(data)


# ── V4 Callback ──


@external
@payable
def unlockCallback(data: Bytes[MAX_COMMANDS_LENGTH]) -> Bytes[MAX_COMMANDS_LENGTH]:
    """
    PoolManager unlock callback — process V4 commands.

    All V4 operations (swap, take, sync, settle) are encoded as
    commands in the data stream. The off-chain code pre-computes
    the exact settlement amounts and order. No delta ledger or
    phase logic needed.
    """
    assert self.t_allowed_callback_addresses[msg.sender], "Unregistered V4 Callback Address"
    self._process_commands(data)
    return b''


# ── Fallback ──


@external
@payable
def __default__():
    """Accept plain ETH transfers, revert on unknown function calls."""
    if len(msg.data) == 0:
        return
    else:
        raise
