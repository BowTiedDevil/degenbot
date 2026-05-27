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

unlockCallback phases:
- Phase 0: Pre-settle input ERC-20 for V3→V4/V2→V4 paths
- Phase 1: Execute V4 swaps and tally deltas in t_v4_deltas
- Phase 2: Deliver remaining queued payloads (take/transfer for V4→V3)
- Phase 3: Settle all nonzero deltas via _v4_settle_currency
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
    dynamic_amount: bool

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
NATIVE_ADDRESS: constant(address) = empty(address)

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

# V4 delta ledger: tracks actual currency deltas from V4 swaps.
# Replaces the old ether_delta/weth_delta pair so intermediate ERC-20
# tokens (e.g. USDC in a WETH→USDC→ETH path) are properly settled.
t_v4_deltas: transient(HashMap[address, int128])


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


# ──────────────────── V4 Internal Helpers ─────────────────────


@internal
def _decode_swap_delta(swap_delta: int256, byte_offset: uint256) -> int128:
    """Extract a component from a packed BalanceDelta at the given byte offset."""
    return convert(
        slice(
            convert(swap_delta, bytes32),
            byte_offset,
            SWAP_DELTA_VALUE_LENGTH,
        ),
        int128,
    )


@internal
def _v4_settle_currency(pool_manager: address, currency: address, delta: int128):
    """
    Settle a single V4 currency delta against the PoolManager.

    - Positive delta: take() — PM owes us tokens
    - Negative delta: settle() — we owe PM tokens
      - Native ETH: unwrap WETH if needed, settle with msg.value
      - WETH: sync, deposit if needed, transfer, settle
      - ERC-20: sync, transfer, settle

    Called by Phase 3 for each currency in the delta ledger.
    Zeros the delta after settling to prevent double-settlement when the
    same ERC-20 appears in multiple V4 swap pool keys.
    """

    if delta > 0:
        # PM owes us — take the tokens
        extcall IPoolManager(pool_manager).take(
            currency, self, convert(delta, uint256)
        )
    elif delta < 0:
        owed: uint256 = convert(-delta, uint256)

        if currency == NATIVE_ADDRESS:
            # Settle native ETH — unwrap WETH if insufficient balance
            if self.balance < owed:
                extcall IWETH(WETH_ADDR).withdraw(
                    owed - self.balance,
                    skip_contract_check=True,
                )
            extcall IPoolManager(pool_manager).settle(value=owed)

        elif currency == WETH_ADDR:
            # Settle WETH — sync before transfer, deposit if needed
            extcall IPoolManager(pool_manager).sync(WETH_ADDR)
            weth_balance: uint256 = staticcall IERC20(WETH_ADDR).balanceOf(self)
            if weth_balance < owed:
                extcall IWETH(WETH_ADDR).deposit(value=owed - weth_balance)
            extcall IERC20(WETH_ADDR).transfer(
                pool_manager, owed, default_return_value=True
            )
            extcall IPoolManager(pool_manager).settle()

        else:
            # Settle ERC-20 — sync before transfer
            extcall IPoolManager(pool_manager).sync(currency)
            extcall IERC20(currency).transfer(
                pool_manager, owed, default_return_value=True
            )
            extcall IPoolManager(pool_manager).settle()

    # Zero the delta to prevent double-settlement in Phase 3
    self.t_v4_deltas[currency] = 0


@internal
def _zero_intermediate_deltas(swap_count: uint256):
    """
    Zero out intermediate ERC-20 deltas in t_v4_deltas.

    Called after Phase 2 when queued payloads (take/transfer) have
    explicitly handled intermediate ERC-20 tokens. This prevents
    Phase 3 from double-taking or double-settling those currencies.
    Native ETH and WETH deltas are never zeroed — Phase 3 handles them.
    """
    for i: uint256 in range(MAX_V4_SWAPS):
        if i >= swap_count:
            break
        key: PoolKey = self.t_v4_swaps[i].key
        for currency: address in [key.currency0, key.currency1]:
            if currency == NATIVE_ADDRESS or currency == WETH_ADDR:
                continue
            self.t_v4_deltas[currency] = 0


@internal
def _deliver_remaining_payloads():
    """Deliver all remaining payloads in the queue."""
    for i: uint256 in range(MAX_PAYLOADS):
        if self.t_all_payloads_delivered:
            break
        self.deliver_queued_payload()


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
    skip_profit_check: bool = False,
):
    """
    Execute a queue of payloads with optional V4 swap auto-settlement.

    For V4 paths, pass PoolKey+SwapParams in v4_swaps. The unlockCallback
    will call PM.swap() directly and auto-settle based on actual deltas.
    For V2/V3 paths, pass all operations in payloads (v4_swaps=[]).

    After all payloads + V4 settlement, asserts combined balance did not decrease
    (unless skip_profit_check=True for testing).

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

    self._deliver_remaining_payloads()

    combined_after: uint256 = staticcall IERC20(WETH_ADDR).balanceOf(self) + self.balance
    if not skip_profit_check:
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
    self._deliver_remaining_payloads()


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
    self._deliver_remaining_payloads()


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
    self._deliver_remaining_payloads()


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
    self._deliver_remaining_payloads()

    # Auto-pay WETH to the calling V3 pool if it's owed
    owed_token: address = NATIVE_ADDRESS
    owed_amount: uint256 = 0
    if amount1_delta > 0:
        owed_token = staticcall IUniswapV3Pool(msg.sender).token1()
        owed_amount = convert(amount1_delta, uint256)
    elif amount0_delta > 0:
        owed_token = staticcall IUniswapV3Pool(msg.sender).token0()
        owed_amount = convert(amount0_delta, uint256)

    if owed_token == WETH_ADDR and owed_amount > 0:
        extcall IERC20(WETH_ADDR).transfer(
            msg.sender, owed_amount, default_return_value=True,
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

    Four phases:
      Phase 0: Pre-settle input ERC-20 currencies for V3→V4/V2→V4 paths.
               Only runs when payloads were delivered before unlock.
      Phase 1: Execute V4 swaps, tally deltas in t_v4_deltas.
               Handles dynamic_amount derivation from ledger.
      Phase 2: Deliver remaining queued payloads (take/transfer for V4→V3).
               Zeros intermediate ERC-20 deltas if payloads were delivered.
      Phase 3: Settle all nonzero deltas via _v4_settle_currency.

    All currencies tracked in t_v4_deltas (not just ETH/WETH) so
    intermediate ERC-20 tokens are properly settled.
    """
    assert self.t_allowed_callback_addresses[msg.sender], "Unregistered V4 Callback Address"
    pool_manager: address = msg.sender
    swap_count: uint256 = self.t_v4_swap_count

    # ── Phase 0: Pre-settle input ERC-20 for V3→V4/V2→V4 ──
    # When forward tokens were transferred+synced before unlock(),
    # call settle() to credit them BEFORE the V4 swap consumes them.
    # Only runs when payloads were already delivered (non-V4-first paths).
    # Skips dynamic_amount swaps (input from ledger) and WETH/native
    # (handled by Phase 3's WETH/ETH settlement).
    if self.t_queued_payload_index > 0:
        for i: uint256 in range(MAX_V4_SWAPS):
            if i >= swap_count:
                break
            swap_payload: V4SwapPayload = self.t_v4_swaps[i]
            params: SwapParams = swap_payload.params
            if swap_payload.dynamic_amount:
                continue
            input_currency: address = swap_payload.key.currency0 if params.zero_for_one else swap_payload.key.currency1
            if input_currency != NATIVE_ADDRESS and input_currency != WETH_ADDR:
                # Skip if already credited (same input currency across multiple swaps)
                if self.t_v4_deltas[input_currency] > 0:
                    continue
                amount_settled: uint256 = extcall IPoolManager(pool_manager).settle()
                self.t_v4_deltas[input_currency] += convert(amount_settled, int128)

    # ── Phase 1: Execute V4 swaps and tally deltas ──
    for i: uint256 in range(MAX_V4_SWAPS):
        if i >= swap_count:
            break
        swap_payload: V4SwapPayload = self.t_v4_swaps[i]
        key: PoolKey = swap_payload.key
        params: SwapParams = swap_payload.params

        # Dynamic amount: derive from accumulated delta
        if params.amount_specified == 0 and swap_payload.dynamic_amount:
            input_currency: address = key.currency0 if params.zero_for_one else key.currency1
            input_delta: int128 = self.t_v4_deltas[input_currency]
            assert input_delta > 0, "dynamic: no input credit"
            params = SwapParams(
                zero_for_one=params.zero_for_one,
                amount_specified=-convert(input_delta, int256),
                sqrt_price_limit_x96=params.sqrt_price_limit_x96,
            )

        swap_delta: int256 = extcall IPoolManager(pool_manager).swap(key, params, b'')

        # Tally deltas from BalanceDelta return value
        self.t_v4_deltas[key.currency0] += self._decode_swap_delta(swap_delta, SWAP_DELTA_AMOUNT0_OFFSET)
        self.t_v4_deltas[key.currency1] += self._decode_swap_delta(swap_delta, SWAP_DELTA_AMOUNT1_OFFSET)

    # ── Phase 2: Deliver remaining queued payloads ──
    payloads_delivered_in_phase2: bool = not self.t_all_payloads_delivered
    self._deliver_remaining_payloads()

    # Zero intermediate ERC-20 deltas that were handled by payloads
    if payloads_delivered_in_phase2:
        self._zero_intermediate_deltas(swap_count)

    # ── Phase 3: Settle all nonzero deltas ──
    native_delta: int128 = self.t_v4_deltas[NATIVE_ADDRESS]
    weth_delta: int128 = self.t_v4_deltas[WETH_ADDR]

    if native_delta != 0:
        self._v4_settle_currency(pool_manager, NATIVE_ADDRESS, native_delta)
    if weth_delta != 0:
        self._v4_settle_currency(pool_manager, WETH_ADDR, weth_delta)

    # Settle intermediate ERC-20 tokens from V4 swap PoolKeys
    for i: uint256 in range(MAX_V4_SWAPS):
        if i >= swap_count:
            break
        key: PoolKey = self.t_v4_swaps[i].key
        for currency: address in [key.currency0, key.currency1]:
            if currency == NATIVE_ADDRESS or currency == WETH_ADDR:
                continue
            delta: int128 = self.t_v4_deltas[currency]
            if delta != 0:
                self._v4_settle_currency(pool_manager, currency, delta)

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
