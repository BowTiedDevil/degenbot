# pragma version ~=0.4.0
# pragma evm-version cancun

"""
Tstore Executor — generic payload executor for Uniswap V2/V3/V4 arbitrage.

Supports all callback types from major Uniswap forks:
- V2: uniswapV2Call (Uniswap, SushiSwap), hook (Velodrome/Aerodrome), pancakeCall (PancakeSwap)
- V3: uniswapV3SwapCallback (Uniswap, SushiSwap), pancakeV3SwapCallback (PancakeSwap)
- V4: unlockCallback (PoolManager auto-settle by reading swap return values)

V4 settlement uses a HYBRID approach:
- V4 swaps are invoked directly by the unlockCallback (not via raw_call),
  so the contract can read the BalanceDelta return value.
- After all swaps, take() positive deltas and settle() negative deltas
  using ACTUAL on-chain amounts — eliminates CurrencyNotSettled rounding errors.
- Non-swap V4 operations (take/settle/sync) are NOT in the payload queue
  for V4 paths — they're handled automatically.
- V2/V3 payloads continue to use the generic queue + raw_call pattern.

Payload types:
- V2/V3: standard payload queue with raw_call delivery
- V4: PoolKey + SwapParams stored in transient storage, called directly
  with extcall to capture return values for delta tracking
"""

from ethereum.ercs import IERC20

interface IUniswapV3Pool:
    def token0() -> address: view
    def token1() -> address: view

interface IPoolManager:
    def settle() -> uint256: payable
    def sync(currency: address): nonpayable
    def take(currency: address, to: address, amount: uint256): nonpayable
    def swap(
        key: PoolKey,
        params: SwapParams,
        hook_data: Bytes[32],
    ) -> int256: nonpayable

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

struct V4SwapPayload:
    key: PoolKey
    params: SwapParams

struct Payload:
    target: address
    calldata: Bytes[MAX_PAYLOAD_BYTES]
    value: uint256
    will_callback: bool

OWNER_ADDR: immutable(address)
WETH_ADDR: immutable(address)
MAX_PAYLOADS: constant(uint256) = 16
MAX_V4_SWAPS: constant(uint256) = 4
MAX_PAYLOAD_BYTES: constant(uint256) = 832

# Encoding constants for BalanceDelta (int256 packed as amount0 || amount1)
# amount0 is in the upper 128 bits (bytes 0-15), amount1 in lower 128 bits (bytes 16-31)
SWAP_DELTA_AMOUNT0_OFFSET: constant(uint256) = 0
SWAP_DELTA_AMOUNT1_OFFSET: constant(uint256) = 16
SWAP_DELTA_VALUE_LENGTH: constant(uint256) = 16

# --- Transient state (cleared every transaction) ---

t_all_payloads_delivered: transient(bool)
t_allowed_callback_addresses: transient(HashMap[address, bool])
t_last_payload_index: transient(uint256)
t_payloads: transient(Payload[MAX_PAYLOADS])
t_queued_payload_index: transient(uint256)

# V4 swap params stored in transient storage for direct extcall
t_v4_swaps: transient(V4SwapPayload[MAX_V4_SWAPS])
t_v4_swap_count: transient(uint256)


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


# ──────────────────────────── Core ────────────────────────────


@internal
def deliver_queued_payload():
    """Deliver the next payload in the queue and advance the index."""
    payload_index: uint256 = self.t_queued_payload_index
    payload: Payload = self.t_payloads[payload_index]

    if payload.will_callback:
        self.t_allowed_callback_addresses[payload.target] = True

    if payload_index == self.t_last_payload_index:
        self.t_all_payloads_delivered = True
    else:
        self.t_queued_payload_index = unsafe_add(payload_index, 1)

    raw_call(
        payload.target,
        payload.calldata,
        value=payload.value,
    )


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
def execute_payloads(
    payloads: DynArray[Payload, MAX_PAYLOADS],
    v4_swaps: DynArray[V4SwapPayload, MAX_V4_SWAPS],
    bribe_bips: uint256 = 0,
):
    """
    Execute a queue of payloads with optional V4 swap auto-settlement.

    For V4 paths, pass PoolKey+SwapParams in v4_swaps. The unlockCallback
    will call PM.swap() directly and auto-settle based on actual deltas.
    For V2/V3 paths, pass all operations in payloads (v4_swaps=[]).

    After all payloads + V4 settlement, asserts combined balance did not decrease.

    If bribe_bips > 0, sends a coinbase bribe proportional to profit.
    """
    assert msg.sender == OWNER_ADDR, "!OWNER"

    # Store V4 swap params in transient storage
    for i: uint256 in range(MAX_V4_SWAPS):
        if i == len(v4_swaps):
            break
        self.t_v4_swaps[i] = v4_swaps[i]
    self.t_v4_swap_count = len(v4_swaps)

    # Store payload queue
    for i: uint256 in range(MAX_PAYLOADS):
        if i == len(payloads):
            self.t_last_payload_index = unsafe_sub(i, 1)
            break
        self.t_payloads[i] = payloads[i]

    combined_before: uint256 = staticcall IERC20(WETH_ADDR).balanceOf(self) + self.balance

    for i: uint256 in range(MAX_PAYLOADS):
        if self.t_all_payloads_delivered:
            break
        else:
            self.deliver_queued_payload()

    combined_after: uint256 = staticcall IERC20(WETH_ADDR).balanceOf(self) + self.balance
    assert combined_after >= combined_before, "combined balance reduction"

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


# ──────────────────────── V2 Callbacks ────────────────────────


@internal
def v2_swap_callback():
    """Resume payload delivery after a V2 flash borrow callback."""
    for i: uint256 in range(MAX_PAYLOADS):
        if self.t_all_payloads_delivered:
            break
        else:
            self.deliver_queued_payload()


@external
@payable
def uniswapV2Call(
    sender: address,
    amount0Out: uint256,
    amount1Out: uint256,
    data: Bytes[32],
):
    """Uniswap V2 & SushiSwap V2 flash borrow callback."""
    assert self.t_allowed_callback_addresses[msg.sender], "Unregistered V2 Callback Address"
    self.v2_swap_callback()


@external
@payable
def hook(
    sender: address,
    amount0Out: uint256,
    amount1Out: uint256,
    data: Bytes[32],
):
    """Velodrome/Aerodrome V2 flash borrow callback."""
    assert self.t_allowed_callback_addresses[msg.sender], "Unregistered V2 Callback Address"
    self.v2_swap_callback()


@external
@payable
def pancakeCall(
    sender: address,
    amount0Out: uint256,
    amount1Out: uint256,
    data: Bytes[32],
):
    """PancakeSwap V2 flash borrow callback."""
    assert self.t_allowed_callback_addresses[msg.sender], "Unregistered V2 Callback Address"
    self.v2_swap_callback()


# ──────────────────────── V3 Callbacks ────────────────────────


@internal
def v3_swap_callback(
    amount0_delta: int256,
    amount1_delta: int256,
):
    """
    Resume payload delivery after a V3 swap callback.

    If the calling pool is owed WETH, auto-transfer it.
    """
    for i: uint256 in range(MAX_PAYLOADS):
        if self.t_all_payloads_delivered:
            break
        else:
            self.deliver_queued_payload()

    # Auto-pay WETH to the calling V3 pool if it's owed
    if amount1_delta > 0:
        if staticcall IUniswapV3Pool(msg.sender).token1() == WETH_ADDR:
            extcall IERC20(WETH_ADDR).transfer(
                msg.sender,
                convert(amount1_delta, uint256),
                default_return_value=True,
            )
    elif amount0_delta > 0:
        if staticcall IUniswapV3Pool(msg.sender).token0() == WETH_ADDR:
            extcall IERC20(WETH_ADDR).transfer(
                msg.sender,
                convert(amount0_delta, uint256),
                default_return_value=True,
            )


@external
@payable
def uniswapV3SwapCallback(
    amount0_delta: int256,
    amount1_delta: int256,
    data: Bytes[32],
):
    """Uniswap V3 & SushiSwap V3 swap callback."""
    assert self.t_allowed_callback_addresses[msg.sender], "Unregistered V3 Callback Address"
    self.v3_swap_callback(amount0_delta, amount1_delta)


@external
@payable
def pancakeV3SwapCallback(
    amount0_delta: int256,
    amount1_delta: int256,
    data: Bytes[32],
):
    """PancakeSwap V3 swap callback."""
    assert self.t_allowed_callback_addresses[msg.sender], "Unregistered V3 Callback Address"
    self.v3_swap_callback(amount0_delta, amount1_delta)


# ──────────────────────── V4 Callback ─────────────────────────


@external
@payable
def unlockCallback(data: Bytes[32]) -> Bytes[32]:
    """
    PoolManager unlock callback — execute V4 swaps, then auto-settle.

    Reads V4 swap params from transient storage (set by execute_payloads).
    Calls PM.swap() directly to capture BalanceDelta return values.
    After all swaps, takes positive deltas and settles negative deltas
    using ACTUAL on-chain amounts — no rounding mismatches.

    For mixed V2-V4 / V3-V4 paths, non-V4 payloads run before the
    unlock call, V4 swaps run here, and settlement is automatic.

    Only ETH and WETH deltas are tracked for settlement, since those
    are the only currencies with open positions in typical arb paths.
    Intermediate ERC-20 deltas cancel exactly for fully-filled swaps.
    """
    assert self.t_allowed_callback_addresses[msg.sender], "Unregistered V4 Callback Address"
    pool_manager: address = msg.sender

    # ── Phase 1: Execute V4 swaps via extcall and tally deltas ──
    ether_delta: int128 = 0
    weth_delta: int128 = 0
    native: address = empty(address)

    swap_count: uint256 = self.t_v4_swap_count
    for i: uint256 in range(MAX_V4_SWAPS):
        if i >= swap_count:
            break
        swap_payload: V4SwapPayload = self.t_v4_swaps[i]
        key: PoolKey = swap_payload.key
        params: SwapParams = swap_payload.params

        # Call PM.swap() directly — captures BalanceDelta return value
        swap_delta: int256 = extcall IPoolManager(pool_manager).swap(
            key, params, b''
        )

        # Decode BalanceDelta: upper 128 bits = amount0, lower 128 bits = amount1
        delta_amount0: int128 = convert(
            slice(
                convert(swap_delta, bytes32),
                SWAP_DELTA_AMOUNT0_OFFSET,
                SWAP_DELTA_VALUE_LENGTH,
            ),
            int128,
        )
        delta_amount1: int128 = convert(
            slice(
                convert(swap_delta, bytes32),
                SWAP_DELTA_AMOUNT1_OFFSET,
                SWAP_DELTA_VALUE_LENGTH,
            ),
            int128,
        )

        # Tally deltas for ETH and WETH only
        # In V4, currencies are sorted: address(0) is always currency0
        if key.currency0 == native:
            ether_delta += delta_amount0
        elif key.currency0 == WETH_ADDR:
            weth_delta += delta_amount0

        if key.currency1 == native:
            ether_delta += delta_amount1
        elif key.currency1 == WETH_ADDR:
            weth_delta += delta_amount1

    # ── Phase 2: Deliver remaining queued payloads ──
    # For mixed V4+V2/V3 paths, there may be payloads queued after
    # the unlock that need to run before settlement.
    for i: uint256 in range(MAX_PAYLOADS):
        if self.t_all_payloads_delivered:
            break
        else:
            self.deliver_queued_payload()

    # ── Phase 3: Settle V4 deltas using ACTUAL amounts ──

    # Take positive deltas (credits: PM owes us)
    if ether_delta > 0:
        extcall IPoolManager(pool_manager).take(
            native, self, convert(ether_delta, uint256)
        )
    if weth_delta > 0:
        extcall IPoolManager(pool_manager).take(
            WETH_ADDR, self, convert(weth_delta, uint256)
        )

    # Settle negative deltas (debits: we owe PM)
    if ether_delta < 0:
        # Owe ETH to PM — settle with msg.value
        owed_eth: uint256 = convert(-ether_delta, uint256)
        if self.balance < owed_eth:
            extcall IWETH(WETH_ADDR).withdraw(
                owed_eth - self.balance,
                skip_contract_check=True,
            )
        extcall IPoolManager(pool_manager).settle(value=owed_eth)

    if weth_delta < 0:
        # Owe WETH to PM — transfer + sync + settle
        owed_weth: uint256 = convert(-weth_delta, uint256)
        weth_balance: uint256 = staticcall IERC20(WETH_ADDR).balanceOf(self)
        if weth_balance < owed_weth:
            extcall IWETH(WETH_ADDR).deposit(value=owed_weth - weth_balance)
        extcall IERC20(WETH_ADDR).transfer(
            pool_manager, owed_weth, default_return_value=True
        )
        extcall IPoolManager(pool_manager).sync(WETH_ADDR)
        extcall IPoolManager(pool_manager).settle()

    return b''


# ──────────────────────────── Fallback ────────────────────────────


@external
@payable
def __default__():
    """Accept plain ETH transfers, revert on unknown function calls."""
    if len(msg.data) == 0:
        return
    else:
        raise
