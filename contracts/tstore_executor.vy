# pragma version ~=0.4.0
# pragma evm-version cancun

"""
Tstore Executor — generic payload executor for Uniswap V2/V3/V4 arbitrage.

Supports all callback types from major Uniswap forks:
- V2: uniswapV2Call (Uniswap, SushiSwap), hook (Velodrome/Aerodrome), pancakeCall (PancakeSwap)
- V3: uniswapV3SwapCallback (Uniswap, SushiSwap), pancakeV3SwapCallback (PancakeSwap)
- V4: unlockCallback (PoolManager unlock/settle/take pattern)

Usage:
    executor.execute_payloads([
        (POOL_MANAGER, unlock_calldata,                     False),  # V4 unlock entry
        # -- inside unlockCallback, queue resumes: --
        (POOL_MANAGER, swap_calldata,                        False),  # V4 swap A
        (POOL_MANAGER, swap_calldata,                        False),  # V4 swap B
        (WETH,          transfer_to_pool_manager_calldata,   False),  # settle debt
        (POOL_MANAGER, settle_calldata,                      False),  # settle()
        (POOL_MANAGER, take_calldata,                        False),  # take() profit
        # ... or for V2/V3 paths:
        (V2_POOL,  swap_calldata_with_data,  True),   # V2 flash borrow
        (TOKEN,    transfer_calldata,         False),  # ERC20 transfer
        (WETH,     transfer_calldata,         False),  # WETH repayment
    ])

V4 settlement pattern:
    V4 swaps happen inside unlockCallback (called by PoolManager.unlock).
    Python encodes all V4 operations (swap, sync, settle, take) as raw calldata.
    The executor treats them identically to V2/V3 payloads — just raw_calls.

Payload delivery uses a queue with transient storage:
- deliver_queued_payload() advances the queue index and raw_calls the target
- Callbacks (V2/V3/V4) resume queue delivery
- will_callback=True registers the target in t_allowed_callback_addresses
  so the callback's assert(msg.sender in allowed) passes
- t_all_payloads_delivered stops the queue

V3 auto-pay: When a V3 pool is owed WETH, the callback auto-transfers it.
For V3-V3 paths, the Python encoder should NOT include a separate WETH transfer
payload for V3 pools — the auto-pay handles it.
"""

from ethereum.ercs import IERC20

interface IUniswapV3Pool:
    def token0() -> address: view
    def token1() -> address: view

interface IWETH:
    def deposit(): payable
    def withdraw(amount: uint256): nonpayable

struct Payload:
    target: address
    calldata: Bytes[MAX_PAYLOAD_BYTES]
    value: uint256
    will_callback: bool

OWNER_ADDR: immutable(address)
WETH_ADDR: immutable(address)
MAX_PAYLOADS: constant(uint256) = 16
MAX_PAYLOAD_BYTES: constant(uint256) = 832  # PoolManager.swap(PoolKey,SwapParams,bytes32) ≈ 832

# --- Transient state (cleared every transaction) ---

t_all_payloads_delivered: transient(bool)
t_allowed_callback_addresses: transient(HashMap[address, bool])
t_last_payload_index: transient(uint256)
t_payloads: transient(Payload[MAX_PAYLOADS])
t_queued_payload_index: transient(uint256)


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
    bribe_bips: uint256 = 0,
):
    """
    Execute a queue of payloads sequentially.

    Payloads with will_callback=True register the target address
    so callbacks from that address are accepted.

    After all payloads are delivered, asserts WETH balance did not decrease.

    If bribe_bips > 0, sends a coinbase bribe proportional to WETH profit.
    """
    assert msg.sender == OWNER_ADDR, "!OWNER"

    for i: uint256 in range(MAX_PAYLOADS):
        if i == len(payloads):
            self.t_last_payload_index = unsafe_sub(i, 1)
            break
        self.t_payloads[i] = payloads[i]

    weth_balance_before: uint256 = staticcall IERC20(WETH_ADDR).balanceOf(self)

    for i: uint256 in range(MAX_PAYLOADS):
        if self.t_all_payloads_delivered:
            break
        else:
            self.deliver_queued_payload()

    weth_balance_after: uint256 = staticcall IERC20(WETH_ADDR).balanceOf(self)
    assert weth_balance_after >= weth_balance_before, "WETH balance reduction"

    if bribe_bips > 0:
        raw_call(
            block.coinbase,
            b'',
            value=min(
                msg.value,
                unsafe_mul(
                    bribe_bips,
                    unsafe_sub(weth_balance_after, weth_balance_before),
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
    This handles the common case where V3 sells WETH forward
    and the executor must pay the WETH debt.

    For V3→V2 paths where V3 buys the intermediate token:
    - V3 is owed the intermediate token (not WETH)
    - The V2 swap or explicit transfer delivers it
    - No auto-pay fires (amount0/amount1 delta is non-WETH)

    For V3→V3 paths:
    - Each V3 pool is owed WETH (or the intermediate token)
    - Auto-pay handles WETH debts; explicit payloads handle non-WETH debts
    - Python encoder must NOT include separate WETH transfer payloads
      for V3 pools where auto-pay fires (would cause double-payment)
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
    PoolManager unlock callback — resume payload delivery.

    Called by PoolManager.unlock(). Inside this callback, the executor
    delivers queued payloads that perform V4 operations:
    - PoolManager.swap (the V4 swap calls)
    - PoolManager.sync + ERC20.transfer + PoolManager.settle (settle debts)
    - PoolManager.take (receive profits)

    All V4 operations are pre-encoded by Python as raw calldata payloads.
    The executor does not decode swap deltas or manage settlement internally
    — Python pre-computes all amounts and encodes the settlement calldata.

    Only one unlockCallback is accepted per execute_payloads() call.
    The PoolManager address must be registered via will_callback=True on
    the unlock payload.
    """
    assert self.t_allowed_callback_addresses[msg.sender], "Unregistered V4 Callback Address"

    for i: uint256 in range(MAX_PAYLOADS):
        if self.t_all_payloads_delivered:
            break
        else:
            self.deliver_queued_payload()

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
