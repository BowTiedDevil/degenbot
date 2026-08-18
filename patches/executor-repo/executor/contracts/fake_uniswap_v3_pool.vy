"""
Fake Uniswap V3 pool for testing.

Closely mirrors the real UniswapV3Pool.sol swap behavior:
- IIA invariant check matches the real contract (snapshot + delta verification)
- Callback is always invoked (matching real V3 behavior)
- SPL validation matches real V3
- Swap math uses real V3 compute_swap_step (sqrt-price/liquidity based)

Uses `initialize(sqrtPriceX96)` to set the starting price, mint tokens
to the pool, then call `add_liquidity()` to register them as liquidity.
The `swap()` function delegates to the `swap_math` library's
`compute_swap_step`, which computes outputs using the real V3 formulas
(SqrtPriceMath + FullMath) matching the on-chain UniswapV3Pool.

The pool maintains a single full-range liquidity position. sqrt_price_x96
is updated directly from compute_swap_step's returned next price (no
balance-derived recomputation). This matches the real V3 pool's approach.
"""

from .interfaces.UniswapV3 import IUniswapV3Pool
from .interfaces.UniswapV3 import IUniswapV3SwapCallback
from .interfaces.UniswapV3 import IPancakeV3SwapCallback
from ethereum.ercs import IERC20
from .libraries import swap_math

MAX_CALLDATA_LENGTH: constant(uint256) = 512

# Fixed-point constants (also defined in swap_math, re-declared for local use
# in add_liquidity where the library constant cannot be used in a storage write)
Q96: constant(uint256) = 79228162514264337593543950336  # 2^96

# Sqrt price ratio bounds (from TickMath)
MIN_SQRT_RATIO: constant(uint160) = 4295128739
MAX_SQRT_RATIO: constant(uint160) = 1461446703485210103287273052203988822378723970342

implements: IUniswapV3Pool

OWNER: immutable(address)

token0: public(address)
token1: public(address)
fee: public(uint24)

# Which callback selector to invoke: 0 = uniswapV3SwapCallback, 1 = pancakeV3SwapCallback
callback_variant: public(uint256)

# V3 state
sqrt_price_x96: public(uint160)
liquidity: public(uint128)

# Reentrancy guard — matches real V3's slot0.unlocked
unlocked: public(bool)


@deploy
def __init__(token0: address, token1: address, _callback_variant: uint256, _fee: uint24):
    OWNER = msg.sender
    self.token0 = token0
    self.token1 = token1
    self.callback_variant = _callback_variant
    self.fee = _fee
    self.unlocked = True


# ═══════════════════════════════════════════════════════════════════════════
# Setup
# ═══════════════════════════════════════════════════════════════════════════


@external
@nonpayable
def initialize(sqrt_price_x96: uint160):
    """Initialize the pool with a starting sqrt price (Q64.96).

    Must be called before `add_liquidity()` and `swap()`.
    Can only be called once. Analogous to the real V3's `initialize()`.
    """
    assert msg.sender == OWNER
    assert self.sqrt_price_x96 == 0, "AI"
    assert sqrt_price_x96 >= MIN_SQRT_RATIO and sqrt_price_x96 < MAX_SQRT_RATIO
    self.sqrt_price_x96 = sqrt_price_x96


@external
@nonpayable
def add_liquidity():
    """Register current token balances as full-range liquidity.

    Called after `initialize()` and after minting tokens to the pool.
    Computes liquidity from the current sqrt price and balances using
    the V3 full-range formula:
        L = min(balance0 * sqrtPrice / Q96, balance1 * Q96 / sqrtPrice)
    """
    assert msg.sender == OWNER
    assert self.sqrt_price_x96 > 0, "Not initialized"

    balance0: uint256 = staticcall IERC20(self.token0).balanceOf(self)
    balance1: uint256 = staticcall IERC20(self.token1).balanceOf(self)
    assert balance0 > 0 and balance1 > 0, "Need both tokens"

    # Compute full-range liquidity from current balances and sqrt price
    # L = min(balance0 * sqrtPrice / Q96, balance1 * Q96 / sqrtPrice)
    liq0: uint256 = (balance0 * convert(self.sqrt_price_x96, uint256)) // Q96
    liq1: uint256 = (balance1 * Q96) // convert(self.sqrt_price_x96, uint256)

    self.liquidity = convert(min(liq0, liq1), uint128)
    assert self.liquidity > 0, "Zero liquidity"


# ═══════════════════════════════════════════════════════════════════════════
# View helpers
# ═══════════════════════════════════════════════════════════════════════════


@external
@view
def get_amount_out(amount_in: uint256, zero_for_one: bool) -> uint256:
    """Quote the swap output for a given input amount.

    Delegates to swap_math.compute_swap_step with the pool's current
    sqrt price, liquidity, and fee. Returns the amount of output tokens
    the pool would send for `amount_in` input tokens.

    Reverts if the pool is not initialized (sqrt_price_x96 == 0).
    """
    assert self.sqrt_price_x96 > 0, "not initialized"
    assert self.liquidity > 0, "no liquidity"

    # Use the max-allowable target price (like the real V3 pool does for quotes)
    sqrt_price_target: uint160 = MIN_SQRT_RATIO + 1 if zero_for_one else MAX_SQRT_RATIO - 1

    result: (uint160, uint256, uint256, uint256) = swap_math.compute_swap_step(
        self.sqrt_price_x96, sqrt_price_target, self.liquidity,
        convert(amount_in, int256), self.fee
    )
    return result[2]  # amount_out


# ═══════════════════════════════════════════════════════════════════════════
# Main swap function
# ═══════════════════════════════════════════════════════════════════════════


@external
@nonpayable
def swap(
    recipient: address,
    zero_for_one: bool,
    amount_specified: int256,
    sqrt_price_limit_x96: uint160,
    data: Bytes[MAX_CALLDATA_LENGTH]
) -> (int256, int256):
    """
    Perform a V3-style swap.

    Delegates the amount computation to swap_math.compute_swap_step, then
    handles optimistic transfer, callback invocation, and IIA verification.

    Invariant checks match the real UniswapV3Pool.sol:
    1. Reentrancy guard (slot0.unlocked)
    2. amountSpecified != 0
    3. sqrtPriceLimitX96 must be in valid range for the swap direction
    4. Snapshot input-token balance before callback
    5. Verify balance delta after callback (not absolute balance!)
    """
    # ── Reentrancy guard (matches real V3's lock modifier) ──
    assert self.unlocked, "LOK"
    self.unlocked = False

    # ── Validate amountSpecified (real V3: require(amountSpecified != 0, 'AS')) ──
    assert amount_specified != 0, "AS"

    amount0: int256 = empty(int256)
    amount1: int256 = empty(int256)

    if self.sqrt_price_x96 > 0 and self.liquidity > 0:
        # ════════════════════════════════════════════════════════════════
        # V3 MATH MODE — real compute_swap_step (sqrt-price/liquidity)
        # ════════════════════════════════════════════════════════════════

        # ── Validate sqrtPriceLimitX96 (matches real V3's SPL check) ──
        if zero_for_one:
            assert sqrt_price_limit_x96 < self.sqrt_price_x96 and sqrt_price_limit_x96 > MIN_SQRT_RATIO, "SPL"
        else:
            assert sqrt_price_limit_x96 > self.sqrt_price_x96 and sqrt_price_limit_x96 < MAX_SQRT_RATIO, "SPL"

        # ── Compute swap step via real V3 SwapMath.computeSwapStep ──
        # Returns (sqrt_ratio_next_x96, amount_in, amount_out, fee_amount)
        step_result: (uint160, uint256, uint256, uint256) = swap_math.compute_swap_step(
            self.sqrt_price_x96, sqrt_price_limit_x96, self.liquidity,
            amount_specified, self.fee
        )
        sqrt_ratio_next_x96: uint160 = step_result[0]
        step_amount_in: uint256 = step_result[1]
        step_amount_out: uint256 = step_result[2]

        # ── Convert unsigned amounts to signed deltas (matches real V3) ──
        if zero_for_one:
            amount0 = convert(step_amount_in, int256)
            amount1 = -convert(step_amount_out, int256)
        else:
            amount0 = -convert(step_amount_out, int256)
            amount1 = convert(step_amount_in, int256)

        # ── Update sqrt_price_x96 from compute_swap_step's next price ──
        self.sqrt_price_x96 = sqrt_ratio_next_x96

        # ── Optimistic transfer of output tokens (matches real V3) ──
        balance_before: uint256 = empty(uint256)

        if zero_for_one:
            if amount1 < 0:
                extcall IERC20(self.token1).transfer(recipient, convert(-amount1, uint256))

            # ── IIA: snapshot + callback + verify (matches real V3) ──
            balance_before = staticcall IERC20(self.token0).balanceOf(self)
            if self.callback_variant == 0:
                extcall IUniswapV3SwapCallback(msg.sender).uniswapV3SwapCallback(amount0, amount1, data)
            else:
                extcall IPancakeV3SwapCallback(msg.sender).pancakeV3SwapCallback(amount0, amount1, data)
            assert balance_before + convert(amount0, uint256) <= staticcall IERC20(self.token0).balanceOf(self), "IIA"
        else:
            if amount0 < 0:
                extcall IERC20(self.token0).transfer(recipient, convert(-amount0, uint256))

            # ── IIA: snapshot + callback + verify (matches real V3) ──
            balance_before = staticcall IERC20(self.token1).balanceOf(self)
            if self.callback_variant == 0:
                extcall IUniswapV3SwapCallback(msg.sender).uniswapV3SwapCallback(amount0, amount1, data)
            else:
                extcall IPancakeV3SwapCallback(msg.sender).pancakeV3SwapCallback(amount0, amount1, data)
            assert balance_before + convert(amount1, uint256) <= staticcall IERC20(self.token1).balanceOf(self), "IIA"

        # ── Emit Swap event (matches real V3) ──
        log IUniswapV3Pool.Swap(msg.sender, recipient, amount0, amount1, self.sqrt_price_x96, self.liquidity, 0)

    assert self.sqrt_price_x96 > 0 and self.liquidity > 0, "Pool not initialized"

    self.unlocked = True
    return amount0, amount1


@external
@payable
def __default__():
    return
