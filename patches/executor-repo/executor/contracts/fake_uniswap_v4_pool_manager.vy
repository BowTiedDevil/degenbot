"""
Fake Uniswap V4 PoolManager for testing.

Mimics the real PoolManager interface and transient storage behavior:
- Delta accounting via transient storage (t_deltas) with exttload mirrors
- ERC6909 claims (mint/burn/transfer/transferFrom/balanceOf/allowance/isOperator)
- sync/settle with balance-delta snapshots (CurrencyReserves transient slots)
- take with physical token transfers
- swap with BalanceDelta return
- initialize / modifyLiquidity / donate / clear / updateDynamicLPFee
- exttload for reading transient slots (Lock, NonzeroDeltaCount, CurrencyDelta, CurrencyReserves)

The fake PM's transient slot values are verified against the real on-chain
PoolManager via the ExttloadComparator contract.
"""

from .interfaces.UniswapV4 import IPoolManager
from .interfaces.UniswapV4 import IPoolManagerExttload
from .interfaces.UniswapV4 import IUnlockCallback
from .interfaces.UniswapV4 import IERC6909Claims
from ethereum.ercs import IERC20

import utility_functions

NATIVE_ADDRESS: constant(address) = empty(address)
MAX_CALLDATA_LENGTH: constant(uint256) = 512
MAX_CURRENCIES: constant(uint256) = 256
MAX_EXTTLOAD_SLOTS: constant(uint256) = 32
MAX_HOOK_DATA_LENGTH: constant(uint256) = 32

# ── V4 transient storage slot constants ──
# These match the real PoolManager's raw tstore/tload slots exactly,
# so that exttload(slot) returns the same value as the real PM.
# Computed as bytes32(uint256(keccak256(label)) - 1).

# Lock.sol: bytes32(uint256(keccak256("Unlocked")) - 1)
T_LOCK_SLOT: constant(bytes32) = 0xc090fc4683624cfc3884e9d8de5eca132f2d0ec062aff75d43c0465d5ceeab23

# NonzeroDeltaCount.sol: bytes32(uint256(keccak256("NonzeroDeltaCount")) - 1)
T_NONZERO_DELTA_COUNT_SLOT: constant(bytes32) = 0x7d4b3164c6e45b97e7d87b7125a44c5828d005af88f9d751cfd78729c5d99a0b

# CurrencyReserves.sol: bytes32(uint256(keccak256("ReservesOf")) - 1)
T_RESERVES_OF_SLOT: constant(bytes32) = 0x1e0745a7db1623981f0b2a5d4232364c00787266eb75ad546f190e6cebe9bd95

# CurrencyReserves.sol: bytes32(uint256(keccak256("Currency")) - 1)
T_CURRENCY_SLOT: constant(bytes32) = 0x27e098c505d44ec3574004bca052aabf76bd35004c182099d8c575fb238593b9

# ── Tick spacing bounds (from TickMath) ──
MAX_TICK_SPACING: constant(int24) = 16384
MIN_TICK_SPACING: constant(int24) = -16384

# ── Dynamic fee sentinel (from LPFeeLibrary) ──
DYNAMIC_FEE_FLAG: constant(uint24) = 8388608  # top bit set = dynamic fee

implements: IPoolManager
implements: IPoolManagerExttload
implements: IERC6909Claims

OWNER: immutable(address)

# ── Pool registry ──
# Tracks which pools have been initialized (PoolId → sqrtPriceX96)
pools: public(HashMap[bytes32, uint160])

# ── Dynamic LP fee storage (PoolId → fee) ──
dynamic_lp_fees: public(HashMap[bytes32, uint24])

# ── Swap configuration ──
swap_amounts: public(
    HashMap[
        bytes32,  # Pool ID
        HashMap[address, uint256]  # amounts used for the swap
    ]
)

# ── ModifyLiquidity configuration ──
# Pre-configured delta amounts for a given pool + params
modify_liquidity_delta0: public(HashMap[bytes32, HashMap[bytes32, int128]])
modify_liquidity_delta1: public(HashMap[bytes32, HashMap[bytes32, int128]])

# ── Donate configuration ──
# Pre-configured donate amounts per pool
donate_amount0: public(HashMap[bytes32, uint256])
donate_amount1: public(HashMap[bytes32, uint256])

# ── ERC6909 storage (durable, not transient) ──
erc6909_balance_of: public(HashMap[address, HashMap[uint256, uint256]])
erc6909_allowance: public(HashMap[address, HashMap[address, HashMap[uint256, uint256]]])
erc6909_operator: public(HashMap[address, HashMap[address, bool]])

# ── Transient delta accounting ──
t_currencies_used: transient(DynArray[address, MAX_CURRENCIES])

t_deltas: transient(
    HashMap[
        address,
        HashMap[address, int128],
    ]
)

# ── CurrencyReserves transient state ──
# Mirrors the real PM's CurrencyReserves library:
#   tstore(CURRENCY_SLOT, currency) / tload(CURRENCY_SLOT)
#   tstore(RESERVES_OF_SLOT, reserves) / tload(RESERVES_OF_SLOT)
t_synced_currency: transient(address)
t_synced_reserves: transient(uint256)

t_unlocked: transient(bool)

# ── V4-style transient slot values, keyed by slot ──
t_ext_slot: transient(HashMap[bytes32, bytes32])


@deploy
def __init__(token0: address, token1: address):
    OWNER = msg.sender


# ══════════════════════════════════════════
# Internal helpers
# ══════════════════════════════════════════

# ── Delta slot computation: matches CurrencyDelta._computeSlot ──
# Uses the Solidity mstore convention: keccak256(abi.encode(target, currency))
# which left-pads each address to 32 bytes (12 zero bytes + 20 address bytes).

@internal
@view
def _compute_delta_slot(target: address, currency: address) -> bytes32:
    return keccak256(
        concat(
            convert(convert(target, uint160), bytes32),
            convert(convert(currency, uint160), bytes32),
        )
    )


# ── Delta accounting: matches CurrencyDelta.applyDelta + NonzeroDeltaCount ──

@internal
def _account_delta(target: address, currency: address, delta: int128):
    if delta == 0:
        return

    slot: bytes32 = self._compute_delta_slot(target, currency)
    previous: int256 = convert(self.t_deltas[target][currency], int256)
    next_val: int256 = previous + convert(delta, int256)

    self.t_deltas[target][currency] = convert(next_val, int128)
    self.t_ext_slot[slot] = convert(next_val, bytes32)

    # Mirror NonzeroDeltaCount
    nonzero_count: uint256 = convert(self.t_ext_slot[T_NONZERO_DELTA_COUNT_SLOT], uint256)
    if previous == 0 and next_val != 0:
        nonzero_count += 1
    elif previous != 0 and next_val == 0:
        nonzero_count -= 1
    self.t_ext_slot[T_NONZERO_DELTA_COUNT_SLOT] = convert(nonzero_count, bytes32)


# ── Pool ID computation: matches PoolKey.toId() ──

@internal
def _to_pool_id(pool_key: IPoolManager.PoolKey) -> bytes32:
    return keccak256(
        concat(
            convert(pool_key.currency0, bytes32),
            convert(pool_key.currency1, bytes32),
            convert(pool_key.fee, bytes32),
            convert(pool_key.tick_spacing, bytes32),
            convert(pool_key.hooks, bytes32),
        )
    )


@internal
def _input_currency(key: IPoolManager.PoolKey, zero_for_one: bool) -> address:
    return key.currency0 if zero_for_one else key.currency1


@internal
def _output_currency(key: IPoolManager.PoolKey, zero_for_one: bool) -> address:
    return key.currency1 if zero_for_one else key.currency0


# ── Currency → ERC6909 ID ──

@internal
@view
def _currency_to_id(currency: address) -> uint256:
    """Matches CurrencyLibrary.toId(): id = uint160(currency)."""
    return convert(convert(currency, uint160), uint256)


# ── CurrencyReserves helpers ──
# Mirror the real PM's CurrencyReserves library transient storage pattern.

@internal
def _sync_currency_and_reserves(currency: address, reserves: uint256):
    """Matches CurrencyReserves.syncCurrencyAndReserves()."""
    self.t_synced_currency = currency
    self.t_synced_reserves = reserves
    self.t_ext_slot[T_CURRENCY_SLOT] = convert(convert(currency, uint160), bytes32)
    self.t_ext_slot[T_RESERVES_OF_SLOT] = convert(reserves, bytes32)


@internal
def _reset_currency():
    """Matches CurrencyReserves.resetCurrency()."""
    self.t_synced_currency = NATIVE_ADDRESS
    self.t_ext_slot[T_CURRENCY_SLOT] = empty(bytes32)


# ══════════════════════════════════════════
# ERC6909 Claims interface
# ══════════════════════════════════════════

@external
@view
def balanceOf(owner: address, id: uint256) -> uint256:
    return self.erc6909_balance_of[owner][id]


@external
@view
def allowance(owner: address, spender: address, id: uint256) -> uint256:
    return self.erc6909_allowance[owner][spender][id]


@external
@view
def isOperator(owner: address, operator: address) -> bool:
    return self.erc6909_operator[owner][operator]


@external
def transfer(receiver: address, id: uint256, amount: uint256) -> bool:
    assert self.erc6909_balance_of[msg.sender][id] >= amount, "insufficient balance"
    self.erc6909_balance_of[msg.sender][id] -= amount
    self.erc6909_balance_of[receiver][id] += amount
    log IERC6909Claims.Transfer(msg.sender, msg.sender, receiver, id, amount)
    return True


@external
def transferFrom(sender: address, receiver: address, id: uint256, amount: uint256) -> bool:
    if msg.sender != sender and not self.erc6909_operator[sender][msg.sender]:
        allowed: uint256 = self.erc6909_allowance[sender][msg.sender][id]
        if allowed != max_value(uint256):
            assert allowed >= amount, "insufficient allowance"
            self.erc6909_allowance[sender][msg.sender][id] = allowed - amount

    assert self.erc6909_balance_of[sender][id] >= amount, "insufficient balance"
    self.erc6909_balance_of[sender][id] -= amount
    self.erc6909_balance_of[receiver][id] += amount
    log IERC6909Claims.Transfer(msg.sender, sender, receiver, id, amount)
    return True


@external
def approve(spender: address, id: uint256, amount: uint256) -> bool:
    self.erc6909_allowance[msg.sender][spender][id] = amount
    log IERC6909Claims.Approval(msg.sender, spender, id, amount)
    return True


@external
def setOperator(operator: address, approved: bool) -> bool:
    self.erc6909_operator[msg.sender][operator] = approved
    log IERC6909Claims.OperatorSet(msg.sender, operator, approved)
    return True


# ══════════════════════════════════════════
# V4 PoolManager: initialize
# ══════════════════════════════════════════

@external
def initialize(
    key: IPoolManager.PoolKey,
    sqrt_price_x96: uint160,
) -> int24:
    """
    Initialize a pool with a starting sqrt price.

    Mimics the real PoolManager.initialize():
    - Validates tick spacing bounds
    - Validates currency ordering (currency0 < currency1)
    - Stores the pool as initialized
    - Emits Initialize event
    - Returns tick (simplified: always 0 for fake PM)

    Unlike most PM functions, initialize() can be called outside unlock.
    """
    # Validate tick spacing bounds
    assert key.tick_spacing <= MAX_TICK_SPACING, "TickSpacingTooLarge"
    assert key.tick_spacing >= MIN_TICK_SPACING, "TickSpacingTooSmall"

    # Validate currency ordering (matches CurrenciesOutOfOrderOrEqual check)
    # In the real PM, Currency is comparable. In our fake PM, addresses are
    # compared as uint160 — address(0) < any non-zero address.
    assert convert(key.currency0, uint160) < convert(key.currency1, uint160), "CurrenciesOutOfOrderOrEqual"

    pool_id: bytes32 = self._to_pool_id(key)

    # Store pool as initialized (just track the sqrtPriceX96)
    self.pools[pool_id] = sqrt_price_x96

    log IPoolManager.Initialize(
        pool_id,
        key.currency0,
        key.currency1,
        key.fee,
        key.tick_spacing,
        key.hooks,
        sqrt_price_x96,
        0,  # tick (simplified: always 0)
    )

    # Hook calls skipped — fake PM doesn't call beforeInitialize/afterInitialize

    return 0  # tick


# ══════════════════════════════════════════
# V4 PoolManager: modifyLiquidity
# ══════════════════════════════════════════

@external
def set_next_modify_liquidity(
    pool_key: IPoolManager.PoolKey,
    params: IPoolManager.ModifyLiquidityParams,
    amount0: int128,
    amount1: int128,
):
    """Pre-configure the delta amounts for the next modifyLiquidity call on this pool+params."""
    assert msg.sender == OWNER
    pool_id: bytes32 = self._to_pool_id(pool_key)
    params_key: bytes32 = keccak256(
        concat(
            convert(params.tick_lower, bytes32),
            convert(params.tick_upper, bytes32),
            params.salt,
        )
    )
    self.modify_liquidity_delta0[pool_id][params_key] = amount0
    self.modify_liquidity_delta1[pool_id][params_key] = amount1


@external
def modifyLiquidity(
    key: IPoolManager.PoolKey,
    params: IPoolManager.ModifyLiquidityParams,
    hook_data: Bytes[MAX_HOOK_DATA_LENGTH],
) -> (int256, int256):
    """
    Modify liquidity in a pool. Uses pre-configured delta amounts.

    Returns (callerDelta, feesAccrued) encoded as two BalanceDelta values
    packed into int256. In the fake PM, feesAccrued is always 0.

    The real PM returns BalanceDelta (callerDelta) and BalanceDelta (feesAccrued).
    We encode each as int256 where upper 128 bits = amount0, lower 128 bits = amount1.
    For simplicity the fake PM returns the two BalanceDelta values as separate return values.
    """
    assert self.t_unlocked, "ManagerLocked"
    pool_id: bytes32 = self._to_pool_id(key)

    # Check pool is initialized
    assert self.pools[pool_id] != 0, "PoolNotInitialized"

    params_key: bytes32 = keccak256(
        concat(
            convert(params.tick_lower, bytes32),
            convert(params.tick_upper, bytes32),
            params.salt,
        )
    )

    amount0: int128 = self.modify_liquidity_delta0[pool_id][params_key]
    amount1: int128 = self.modify_liquidity_delta1[pool_id][params_key]

    # Clear the configured delta
    self.modify_liquidity_delta0[pool_id][params_key] = 0
    self.modify_liquidity_delta1[pool_id][params_key] = 0

    # Account deltas to caller
    self._account_delta(msg.sender, key.currency0, amount0)
    self._account_delta(msg.sender, key.currency1, amount1)
    self.t_currencies_used.append(key.currency0)
    self.t_currencies_used.append(key.currency1)

    # Encode as BalanceDelta: upper 128 = amount0, lower 128 = amount1
    caller_delta: int256 = convert(
        concat(convert(amount0, bytes16), convert(amount1, bytes16)),
        int256,
    )

    log IPoolManager.ModifyLiquidity(pool_id, msg.sender, params.tick_lower, params.tick_upper, params.liquidity_delta, params.salt)

    # Hook calls and feesAccrued skipped — fake PM returns 0 for fees
    return (caller_delta, 0)


# ══════════════════════════════════════════
# V4 PoolManager: donate
# ══════════════════════════════════════════

@external
def set_next_donate(
    pool_key: IPoolManager.PoolKey,
    amount0: uint256,
    amount1: uint256,
):
    """Pre-configure the amounts for the next donate call on this pool."""
    assert msg.sender == OWNER
    pool_id: bytes32 = self._to_pool_id(pool_key)
    self.donate_amount0[pool_id] = amount0
    self.donate_amount1[pool_id] = amount1


@external
def donate(
    key: IPoolManager.PoolKey,
    amount0: uint256,
    amount1: uint256,
    hook_data: Bytes[MAX_HOOK_DATA_LENGTH],
) -> int256:
    """
    Donate token amounts to a pool. Uses pre-configured amounts if set,
    otherwise uses the provided amount0/amount1 directly.

    Returns BalanceDelta encoded as int256.
    """
    assert self.t_unlocked, "ManagerLocked"
    pool_id: bytes32 = self._to_pool_id(key)

    # Check pool is initialized
    assert self.pools[pool_id] != 0, "PoolNotInitialized"

    # Use pre-configured amounts if available, otherwise use provided values
    actual_amount0: uint256 = amount0
    actual_amount1: uint256 = amount1

    configured_amount0: uint256 = self.donate_amount0[pool_id]
    configured_amount1: uint256 = self.donate_amount1[pool_id]

    if configured_amount0 != 0 or configured_amount1 != 0:
        actual_amount0 = configured_amount0
        actual_amount1 = configured_amount1
        self.donate_amount0[pool_id] = 0
        self.donate_amount1[pool_id] = 0

    # Account deltas: donate debits the caller
    self._account_delta(msg.sender, key.currency0, convert(actual_amount0, int128))
    self._account_delta(msg.sender, key.currency1, convert(actual_amount1, int128))
    self.t_currencies_used.append(key.currency0)
    self.t_currencies_used.append(key.currency1)

    log IPoolManager.Donate(pool_id, msg.sender, actual_amount0, actual_amount1)

    # Encode as BalanceDelta
    return convert(
        concat(convert(convert(actual_amount0, int128), bytes16), convert(convert(actual_amount1, int128), bytes16)),
        int256,
    )


# ══════════════════════════════════════════
# V4 PoolManager: mint / burn (ERC6909-aware)
# ══════════════════════════════════════════

@external
def mint(to: address, id: uint256, amount: uint256):
    """
    Convert a positive delta into an ERC6909 balance.
    Matches real PoolManager: _accountDelta(currency, -(amount), msg.sender); _mint(to, id, amount)
    """
    assert self.t_unlocked, "ManagerLocked"
    assert amount > 0, "zero mint"
    currency: address = convert(convert(id, uint160), address)

    self._account_delta(msg.sender, currency, -convert(amount, int128))
    self.t_currencies_used.append(currency)

    self.erc6909_balance_of[to][id] += amount
    log IERC6909Claims.Transfer(msg.sender, empty(address), to, id, amount)


@external
def burn(from_: address, id: uint256, amount: uint256):
    """
    Convert an ERC6909 balance into a payable delta.
    Matches real PoolManager: _accountDelta(currency, amount, msg.sender); _burnFrom(from, id, amount)
    """
    assert self.t_unlocked, "ManagerLocked"
    assert amount > 0, "zero burn"
    currency: address = convert(convert(id, uint160), address)

    self._account_delta(msg.sender, currency, convert(amount, int128))
    self.t_currencies_used.append(currency)

    if msg.sender != from_ and not self.erc6909_operator[from_][msg.sender]:
        allowed: uint256 = self.erc6909_allowance[from_][msg.sender][id]
        if allowed != max_value(uint256):
            assert allowed >= amount, "insufficient allowance"
            self.erc6909_allowance[from_][msg.sender][id] = allowed - amount

    assert self.erc6909_balance_of[from_][id] >= amount, "insufficient ERC6909 balance"
    self.erc6909_balance_of[from_][id] -= amount
    log IERC6909Claims.Transfer(msg.sender, from_, empty(address), id, amount)


# ══════════════════════════════════════════
# V4 PoolManager: sync / settle / take / clear
# ══════════════════════════════════════════

@external
def sync(currency: address):
    """
    Snapshot the current balance of a currency for the next settle().

    Unlike other V4 operations, sync() can be called outside unlock.
    Matches the real PM's sync() exactly:
    - Native: just resets the synced currency (no balance snapshot needed)
    - ERC-20: snapshots the current ERC-20 balance
    """
    if currency == NATIVE_ADDRESS:
        self._reset_currency()
    else:
        balance: uint256 = staticcall IERC20(currency).balanceOf(self)
        self._sync_currency_and_reserves(currency, balance)


@internal
@payable
def _settle(account: address) -> uint256:
    """
    Settle a previously synced currency.

    Matches the real PM's _settle() exactly:
    - If synced currency is address(0) (native or reset): paid = msg.value
    - If synced currency is an ERC-20: paid = balanceNow - balanceBefore,
      then reset the currency slot
    - Account the paid amount as a delta for `account`
    """
    currency: address = self.t_synced_currency
    paid: uint256 = 0

    if currency == NATIVE_ADDRESS:
        paid = msg.value
    else:
        if msg.value > 0:
            raise "NonzeroNativeValue"
        reserves_before: uint256 = self.t_synced_reserves
        reserves_now: uint256 = staticcall IERC20(currency).balanceOf(self)
        paid = reserves_now - reserves_before
        self._reset_currency()

    self._account_delta(account, currency, convert(paid, int128))
    self.t_currencies_used.append(currency)

    return paid


@external
@payable
def settle() -> uint256:
    assert self.t_unlocked, "ManagerLocked"
    return self._settle(msg.sender)


@external
@payable
def settleFor(recipient: address) -> uint256:
    assert self.t_unlocked, "ManagerLocked"
    return self._settle(recipient)


@external
def take(currency: address, to: address, amount: uint256):
    assert self.t_unlocked, "ManagerLocked"
    self._account_delta(msg.sender, currency, -convert(amount, int128))
    self.t_currencies_used.append(currency)

    if currency == NATIVE_ADDRESS:
        raw_call(to, b'', value=amount)
    else:
        extcall IERC20(currency).transfer(to, amount)


@external
def clear(currency: address, amount: uint256):
    """
    Clear an exact positive delta for a currency.

    Matches the real PM's clear():
    - Verifies the current delta exactly matches `amount` (as a positive value)
    - Subtracts the delta, reducing it to zero (and decrementing NonzeroDeltaCount)
    """
    assert self.t_unlocked, "ManagerLocked"
    current: int256 = convert(self.t_deltas[msg.sender][currency], int256)
    # amount is uint256, so amountDelta is always positive
    amount_delta: int128 = convert(amount, int128)
    assert amount_delta == convert(current, int128), "MustClearExactPositiveDelta"

    # Negate the delta to bring it to zero
    self._account_delta(msg.sender, currency, -amount_delta)
    # Don't need to append to t_currencies_used — delta is now 0


# ══════════════════════════════════════════
# V4 PoolManager: unlock
# ══════════════════════════════════════════

@external
def unlock(data: Bytes[MAX_CALLDATA_LENGTH]) -> Bytes[MAX_CALLDATA_LENGTH]:
    assert not self.t_unlocked, "AlreadyUnlocked"

    self.t_unlocked = True
    self.t_ext_slot[T_LOCK_SLOT] = convert(True, bytes32)

    result: Bytes[MAX_CALLDATA_LENGTH] = extcall IUnlockCallback(msg.sender).unlockCallback(data)

    self.t_ext_slot[T_LOCK_SLOT] = convert(False, bytes32)
    self.t_unlocked = False

    for currency: address in self.t_currencies_used:
        if self.t_deltas[msg.sender][currency] != 0:
            unsettled_amount: int256 = convert(
                self.t_deltas[msg.sender][currency],
                int256,
            )
            err: String[256] = concat(
                "CurrencyNotSettled",
                ":",
                utility_functions._convert_address_to_checksummed_addr_str(currency),
                ":",
                "-" if unsettled_amount < 0 else "+",
                uint2str(
                    convert(
                        abs(unsettled_amount),
                        uint256,
                    )
                )
            )
            raise err

    # Check NonzeroDeltaCount is zero after settlement
    if convert(self.t_ext_slot[T_NONZERO_DELTA_COUNT_SLOT], uint256) != 0:
        raise "CurrencyNotSettled"

    return result


# ══════════════════════════════════════════
# V4 PoolManager: swap
# ══════════════════════════════════════════

@external
def set_next_swap(
    pool_key: IPoolManager.PoolKey,
    amount_in: uint256,
    amount_out: uint256,
    zero_for_one: bool,
    hook_data: Bytes[MAX_HOOK_DATA_LENGTH]
):
    assert msg.sender == OWNER
    assert amount_in != 0 and amount_out != 0, "Amounts must be non-zero"

    currency_in: address = self._input_currency(pool_key, zero_for_one)
    currency_out: address = self._output_currency(pool_key, zero_for_one)

    assert (
        self.balance
        if currency_out == NATIVE_ADDRESS
        else staticcall IERC20(currency_out).balanceOf(self)
    ) >= amount_out, 'amount_out exceeds balance!'

    self.swap_amounts[self._to_pool_id(pool_key)][currency_in] = amount_in
    self.swap_amounts[self._to_pool_id(pool_key)][currency_out] = amount_out


@external
def swap(
    key: IPoolManager.PoolKey,
    params: IPoolManager.SwapParams,
    hookData: Bytes[MAX_CALLDATA_LENGTH]
) -> int256:
    """
    Perform a fake swap using pre-configured amounts.
    Validates the V4 sign convention for amountSpecified.
    Returns the swap delta, encoded as two close-packed int128 values.
    """
    assert self.t_unlocked, "ManagerLocked"
    if params.amount_specified == 0:
        raise "SwapAmountCannotBeZero"

    self.t_currencies_used.append(key.currency0)
    self.t_currencies_used.append(key.currency1)

    pool_id: bytes32 = self._to_pool_id(key)

    currency_in: address = self._input_currency(key, params.zero_for_one)
    currency_out: address = self._output_currency(key, params.zero_for_one)

    if params.amount_specified < 0:
        assert convert(-params.amount_specified, uint256) == self.swap_amounts[pool_id][currency_in], "V4 exact-input: |amountSpecified| != amount_in"
    elif params.amount_specified > 0:
        assert convert(params.amount_specified, uint256) == self.swap_amounts[pool_id][currency_out], "V4 exact-output: amountSpecified != amount_out"

    amount0_delta: int128 = convert(self.swap_amounts[pool_id][key.currency0], int128)
    amount1_delta: int128 = convert(self.swap_amounts[pool_id][key.currency1], int128)

    self.swap_amounts[pool_id][key.currency0] = 0
    self.swap_amounts[pool_id][key.currency1] = 0

    if params.zero_for_one:
        amount0_delta = -amount0_delta
    else:
        amount1_delta = -amount1_delta

    self._account_delta(msg.sender, key.currency0, amount0_delta)
    self._account_delta(msg.sender, key.currency1, amount1_delta)

    log IPoolManager.Swap(pool_id, msg.sender, amount0_delta, amount1_delta, 0, 0, 0, 0)

    return convert(
        concat(
            convert(amount0_delta, bytes16),
            convert(amount1_delta, bytes16),
        ),
        int256
    )


# ══════════════════════════════════════════
# V4 PoolManager: updateDynamicLPFee
# ══════════════════════════════════════════

@external
def updateDynamicLPFee(key: IPoolManager.PoolKey, new_dynamic_lp_fee: uint24):
    """
    Update the dynamic LP fee for a pool.

    Matches the real PM's updateDynamicLPFee():
    - Only callable by the hook address of a dynamic-fee pool
    - Validates that the fee is a valid LP fee
    - Stores the fee for the pool
    - Can be called outside unlock (like initialize)
    """
    # Validate: pool must have dynamic fee flag, and caller must be the hook
    assert (key.fee & DYNAMIC_FEE_FLAG) != 0, "UnauthorizedDynamicLPFeeUpdate"
    assert msg.sender == key.hooks, "UnauthorizedDynamicLPFeeUpdate"

    # Validate fee range (max valid LP fee = 1_000_000 = 100%)
    assert new_dynamic_lp_fee <= 1_000_000, "InvalidLPFee"

    pool_id: bytes32 = self._to_pool_id(key)
    self.dynamic_lp_fees[pool_id] = new_dynamic_lp_fee


# ══════════════════════════════════════════
# Exttload support
# ══════════════════════════════════════════

@external
@view
def exttload(slot: bytes32) -> bytes32:
    """
    Read a transient storage slot. Returns whatever is stored at the
    given slot in t_ext_slot, which mirrors the real PM's raw tload.

    The real PM's exttload does a raw tload — it can read ANY transient
    slot including CurrencyDelta, Lock, NonzeroDeltaCount, and
    CurrencyReserves slots.
    """
    return self.t_ext_slot[slot]


@external
@payable
def __default__():
    return
