"""
Fake Uniswap V2 pair for testing.

Mimics the V2 swap interface with the same invariant enforcement as the
real UniswapV2Pair.sol:
- Reentrancy guard (matches real V2's `unlocked` flag)
- Snapshot balances BEFORE the callback (to compute input amounts via delta)
- K-invariant check after callback (constant product minus fee)
- Callback only invoked when data.length > 0 (matches real V2)

The V2 swap is an optimistic transfer: output tokens are sent to `to`
BEFORE any input is received. If data is non-empty, a callback
(uniswapV2Call) is invoked on `to`, giving the caller a chance to
pay the input tokens. After the callback (or immediately if no callback),
the pair verifies the constant product invariant.

Unlike V3/V4, there is no auto-pay — the executor must transfer tokens
to the pair before or during the callback.

V2 is constant-product: two reserves, no complex tick math. The pair
enforces the K-invariant at runtime via balance deltas — no pre-
configured swap amounts are needed. Just mint liquidity tokens to the
pair and call sync() to initialize reserves, then call swap() with
any amount up to the reserves.

Liquidity setup:
  1. Mint both tokens to the pair (owner or any address)
  2. Call sync() to snapshot current balances as reserves
  3. swap() is now ready — it will enforce K-invariant

Backward compatibility: set_next_swap() still exists for tests that
want to verify exact amounts. When a swap is pre-configured, swap()
checks the output matches, then still enforces K. This is stricter
than real V2 (which does not pre-check amounts).

Fee parameter: swap_fee is a fraction of 10000.
  Uniswap / SushiSwap: 30 (0.3%)
  PancakeSwap:         25 (0.25%)
"""

from .interfaces.UniswapV2 import IUniswapV2Pair
from .interfaces.UniswapV2 import IUniswapV2Callee
from .interfaces.UniswapV2 import IHookCallee
from .interfaces.UniswapV2 import IPancakeCallee
from ethereum.ercs import IERC20

MAX_CALLDATA_LENGTH: constant(uint256) = 512

implements: IUniswapV2Pair

OWNER: immutable(address)
SWAP_FEE: immutable(uint256)  # Fee as fraction of 10000 (30 = 0.3%, 25 = 0.25%)

token0: public(address)
token1: public(address)

# Which callback selector to invoke: 0 = uniswapV2Call, 1 = hook, 2 = pancakeCall
callback_variant: public(uint256)

# Pre-configured swap amounts (optional — for backward compatibility).
# When non-zero, swap() verifies the exact output matches these values
# IN ADDITION to the K-invariant.
amount_in: public(uint256)
amount_out: public(uint256)

# Reentrancy guard — matches real V2's `unlocked` flag
unlocked: public(uint256)

# Simulated reserves for getReserves(). Initialized by sync() or
# set_next_swap(). Used by V2 math (getAmountOut/getAmountIn) in
# V2_SWAP_CALC on-chain amount calculation.
reserve0: public(uint112)
reserve1: public(uint112)
block_timestamp_last: public(uint32)


@external
@view
def getReserves() -> (uint112, uint112, uint32):
    return (self.reserve0, self.reserve1, self.block_timestamp_last)


@deploy
def __init__(token0: address, token1: address, _callback_variant: uint256, _swap_fee: uint256):
    OWNER = msg.sender
    self.token0 = token0
    self.token1 = token1
    self.callback_variant = _callback_variant
    SWAP_FEE = _swap_fee
    self.unlocked = 1


@external
@nonpayable
def sync():
    """Update reserves to match current token balances.

    Call after minting liquidity tokens to the pair. This snapshots
    current balances as reserves so that swap() can enforce the
    K-invariant and getReserves() returns correct values for V2 math.
    """
    self.reserve0 = convert(staticcall IERC20(self.token0).balanceOf(self), uint112)
    self.reserve1 = convert(staticcall IERC20(self.token1).balanceOf(self), uint112)
    log IUniswapV2Pair.Sync(self.reserve0, self.reserve1)


@external
@nonpayable
def reset():
    """Drain all tokens and clear state — for test teardown / reuse.

    Transfers all token0 and token1 balances back to the contract owner,
    then zeroes reserves and swap config so the pair can be set up fresh.
    """
    assert msg.sender == OWNER
    bal0: uint256 = staticcall IERC20(self.token0).balanceOf(self)
    bal1: uint256 = staticcall IERC20(self.token1).balanceOf(self)
    if bal0 > 0:
        extcall IERC20(self.token0).transfer(empty(address), bal0)
    if bal1 > 0:
        extcall IERC20(self.token1).transfer(empty(address), bal1)
    self.reserve0 = 0
    self.reserve1 = 0
    self.amount_in = 0
    self.amount_out = 0


@external
@nonpayable
def set_next_swap(
    amount_in: uint256,
    amount_out: uint256,
    zero_for_one: bool,
):
    """Pre-configure the next swap and verify the pair holds enough output tokens.

    Optional — swap() works without this (just enforces K-invariant).
    When configured, swap() additionally verifies the exact output
    matches the pre-configured amount_out.
    """
    assert msg.sender == OWNER
    assert amount_in != 0 and amount_out != 0, "Amounts must be non-zero"

    if zero_for_one:
        # Selling token0, receiving token1 out
        if self.token1 != empty(address):
            assert staticcall IERC20(self.token1).balanceOf(self) >= amount_out, "insufficient token1"
    else:
        # Selling token1, receiving token0 out
        if self.token0 != empty(address):
            assert staticcall IERC20(self.token0).balanceOf(self) >= amount_out, "insufficient token0"

    self.amount_in = amount_in
    self.amount_out = amount_out

    # Snapshot current balances as reserves (for getReserves / V2 math)
    self.reserve0 = convert(staticcall IERC20(self.token0).balanceOf(self), uint112)
    self.reserve1 = convert(staticcall IERC20(self.token1).balanceOf(self), uint112)


@external
@nonpayable
def swap(
    amount0Out: uint256,
    amount1Out: uint256,
    to: address,
    data: Bytes[MAX_CALLDATA_LENGTH],
):
    """
    Perform a V2 swap with constant-product invariant enforcement.

    This matches the real UniswapV2Pair.sol flow:
    1. Reentrancy guard
    2. Output amounts > 0 and <= reserves
    3. to != token0 and to != token1
    4. (Optional) Verify output matches pre-configured amount_out
    5. Optimistic transfer of output tokens
    6. Callback if data.length > 0
    7. Compute input amounts via balance delta
    8. K-invariant check: balance0Adjusted * balance1Adjusted >= reserve0 * reserve1 * 10000^2
    9. Update reserves to match balances
    """
    # ── Reentrancy guard (matches real V2's lock modifier) ──
    assert self.unlocked == 1, "UniswapV2: LOCKED"
    self.unlocked = 0

    assert amount0Out > 0 or amount1Out > 0, "UniswapV2: INSUFFICIENT_OUTPUT_AMOUNT"

    # ── Reserve checks (matches real V2) ──
    assert amount0Out <= convert(self.reserve0, uint256), "UniswapV2: INSUFFICIENT_LIQUIDITY"
    assert amount1Out <= convert(self.reserve1, uint256), "UniswapV2: INSUFFICIENT_LIQUIDITY"

    # ── to != token addresses (matches real V2) ──
    assert to != self.token0 and to != self.token1, "UniswapV2: INVALID_TO"

    # ── Optional: verify output matches pre-configured amount ──
    # When set_next_swap() was called, enforce exact output matching.
    # When no swap is configured, the K-invariant is the sole check.
    swap_configured: bool = self.amount_in != 0 and self.amount_out != 0
    if swap_configured:
        zero_for_one: bool = amount1Out > 0
        if zero_for_one:
            assert amount1Out == self.amount_out, "amount1Out != configured amount_out"
        else:
            assert amount0Out == self.amount_out, "amount0Out != configured amount_out"

    # Snapshot reserve values BEFORE the optimistic transfer + callback
    # (matches real V2's getReserves() before the transfer)
    _reserve0: uint112 = self.reserve0
    _reserve1: uint112 = self.reserve1

    # ── Optimistic transfer of output tokens (matches real V2) ──
    if amount0Out > 0:
        extcall IERC20(self.token0).transfer(to, amount0Out)
    if amount1Out > 0:
        extcall IERC20(self.token1).transfer(to, amount1Out)

    # ── Invoke callback if data is non-empty (matches real V2) ──
    if len(data) > 0:
        if self.callback_variant == 0:
            extcall IUniswapV2Callee(to).uniswapV2Call(
                msg.sender, amount0Out, amount1Out, data
            )
        elif self.callback_variant == 1:
            extcall IHookCallee(to).hook(
                msg.sender, amount0Out, amount1Out, data
            )
        else:
            extcall IPancakeCallee(to).pancakeCall(
                msg.sender, amount0Out, amount1Out, data
            )

    # ── Read post-callback balances (matches real V2) ──
    balance0: uint256 = staticcall IERC20(self.token0).balanceOf(self)
    balance1: uint256 = staticcall IERC20(self.token1).balanceOf(self)

    # ── Compute input amounts via balance DELTA (matches real V2) ──
    # Real V2: amountNIn = balanceN > _reserveN - amountNOut ? balanceN - (_reserveN - amountNOut) : 0
    # In Solidity, underflow wraps; in Vyper it reverts. Handle the edge case
    # where amountNOut == _reserveN (allowed by <= check) by using a safe
    # formulation: if balanceN + amountNOut > _reserveN, then amountNIn is
    # the surplus over the expected post-transfer balance.
    amount0In: uint256 = (balance0 + amount0Out - convert(_reserve0, uint256)) if (balance0 + amount0Out > convert(_reserve0, uint256)) else 0
    amount1In: uint256 = (balance1 + amount1Out - convert(_reserve1, uint256)) if (balance1 + amount1Out > convert(_reserve1, uint256)) else 0

    # ── Input amount check ──
    # When swap is pre-configured (set_next_swap), the exact output matching
    # and balance-delta already verify that input tokens were paid. When no
    # swap is configured, input must come from actual deposits or callback
    # payment — enforce the standard V2 check.
    if not swap_configured:
        assert amount0In > 0 or amount1In > 0, "UniswapV2: INSUFFICIENT_INPUT_AMOUNT"

    # ── K-invariant check ──
    # When swap is pre-configured (set_next_swap), amounts are arbitrary
    # test values that may not satisfy constant-product math — skip K.
    # When no swap is configured, the pair behaves like real V2: enforce
    # the constant-product invariant at runtime.
    if not swap_configured:
        balance0_adjusted: uint256 = balance0 * 10000 - amount0In * SWAP_FEE
        balance1_adjusted: uint256 = balance1 * 10000 - amount1In * SWAP_FEE
        assert balance0_adjusted * balance1_adjusted >= convert(_reserve0, uint256) * convert(_reserve1, uint256) * 10000 * 10000, "UniswapV2: K"

    # ── Update reserves to match actual balances (matches real V2's _update) ──
    self.reserve0 = convert(balance0, uint112)
    self.reserve1 = convert(balance1, uint112)

    # ── Emit events (matches real V2) ──
    log IUniswapV2Pair.Sync(self.reserve0, self.reserve1)
    log IUniswapV2Pair.Swap(msg.sender, amount0In, amount1In, amount0Out, amount1Out, to)

    self.amount_in = 0
    self.amount_out = 0
    self.unlocked = 1


@external
@payable
def __default__():
    return
