from .interfaces.UniswapV4 import IPoolManager
from .interfaces.UniswapV4 import IUnlockCallback
from ethereum.ercs import IERC20

import utility_functions

NATIVE_ADDRESS: constant(address) = empty(address)
MAX_CALLDATA_LENGTH: constant(uint256) = 4096
MAX_CURRENCIES: constant(uint256) = 256

implements: IPoolManager

OWNER: immutable(address)

swap_amounts: public(
    HashMap[
        bytes32, # Pool ID
        HashMap[address, uint256] # amounts used for the swap
    ]
)

# All currencies that the caller has interacted with during the transaction.
t_currencies_used: transient(DynArray[address, MAX_CURRENCIES])

# A map of the current delta for a user and currency.
# A negative value indicates a balance that must be paid with `settle`.
# A positive value indicates a balance that must be withdrawn with `take`.
t_deltas: transient(
    HashMap[
        address,
        HashMap[address, int128],
    ]
)

t_settle_currency: transient(address)
t_settle_currency_balance: transient(uint256)
t_unlocked: transient(bool)


@deploy
def __init__(token0: address, token1: address):
    OWNER = msg.sender


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


@external
def sync(currency: address):
    """Mark the next settlement currency and its pre-settlement balance."""
    if currency == NATIVE_ADDRESS:
        self.t_settle_currency = NATIVE_ADDRESS
    else:
        self.t_settle_currency = currency
        self.t_settle_currency_balance = staticcall IERC20(currency).balanceOf(self)


@internal
@payable
def _settle(account: address) -> uint256:
    amount_paid: uint256 = 0

    if self.t_settle_currency == NATIVE_ADDRESS:
        assert msg.value > 0, "ZERO VALUE SETTLE!"
        amount_paid = msg.value
    else:
        if msg.value > 0:
            raise "NonzeroNativeValue"
        amount_paid = staticcall IERC20(self.t_settle_currency).balanceOf(self) - self.t_settle_currency_balance

    self.t_deltas[account][self.t_settle_currency] += convert(amount_paid, int128)
    self.t_settle_currency = NATIVE_ADDRESS

    return amount_paid


@external
@payable
def settle() -> uint256:
    return self._settle(msg.sender)


@external
@payable
def settleFor(other: address) -> uint256:
    return self._settle(other)


@external
def take(currency: address, to: address, amount: uint256):
    self.t_currencies_used.append(currency)
    self.t_deltas[msg.sender][currency] -= convert(amount, int128)

    if currency == NATIVE_ADDRESS:
        raw_call(to, b'', value=amount)
    else:
        extcall IERC20(currency).transfer(to, amount)


@external
def unlock(data: Bytes[MAX_CALLDATA_LENGTH]) -> Bytes[MAX_CALLDATA_LENGTH]:
    assert not self.t_unlocked, "AlreadyUnlocked"

    self.t_unlocked = True
    result: Bytes[MAX_CALLDATA_LENGTH] = extcall IUnlockCallback(msg.sender).unlockCallback(data)
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

    return result


@external
def set_next_swap(
    pool_key: IPoolManager.PoolKey,
    amount_in: uint256,
    amount_out: uint256,
    zero_for_one: bool,
    hook_data: Bytes[MAX_CALLDATA_LENGTH]
):
    assert msg.sender == OWNER
    assert amount_in != 0 and amount_out != 0, "Amounts must be non-zero"

    currency_in: address = pool_key.currency0 if zero_for_one else pool_key.currency1
    currency_out: address = pool_key.currency1 if zero_for_one else pool_key.currency0

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
    Perform a fake swap if the parameters match the inputs provided in `set_next_swap`.

    Returns the swap delta, encoded as two closed-packed int128 values.
    """
    self.t_currencies_used.append(key.currency0)
    self.t_currencies_used.append(key.currency1)

    pool_id: bytes32 = self._to_pool_id(key)

    amount0_delta: int128 = convert(self.swap_amounts[pool_id][key.currency0], int128)
    amount1_delta: int128 = convert(self.swap_amounts[pool_id][key.currency1], int128)

    self.swap_amounts[pool_id][key.currency0] = 0
    self.swap_amounts[pool_id][key.currency1] = 0

    if params.zero_for_one:
        amount0_delta = -amount0_delta
    else:
        amount1_delta = -amount1_delta

    self.t_deltas[msg.sender][key.currency0] += amount0_delta
    self.t_deltas[msg.sender][key.currency1] += amount1_delta

    return convert(
        concat(
            convert(amount0_delta, bytes16),
            convert(amount1_delta, bytes16),
        ),
        int256
    )


@external
@payable
def __default__():
    return
