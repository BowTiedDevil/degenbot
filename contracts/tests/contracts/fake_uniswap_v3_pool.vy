"""
Fake Uniswap V3 pool for testing.

Mimics the V3 swap interface:
- set_next_swap: pre-configure swap parameters and fund the output
- swap: execute a configured swap, invoke callback on msg.sender

The pool requires the output token to be pre-funded (via mint from the
token contract) before set_next_swap is called.

After the swap, the callback is invoked and the pool then asserts that
the input token has been paid (balance == amount_in).
"""

from .interfaces.UniswapV3 import IUniswapV3Pool
from .interfaces.UniswapV3 import IUniswapV3SwapCallback
from .interfaces.UniswapV3 import IPancakeV3SwapCallback
from ethereum.ercs import IERC20

MAX_CALLDATA_LENGTH: constant(uint256) = 4096

implements: IUniswapV3Pool

OWNER: immutable(address)

token0: public(address)
token1: public(address)

# Which callback selector to invoke: 0 = uniswapV3SwapCallback, 1 = pancakeV3SwapCallback
callback_variant: public(uint256)

amount_in: public(uint256)
amount_out: public(uint256)


@deploy
def __init__(token0: address, token1: address, _callback_variant: uint256):
    OWNER = msg.sender
    self.token0 = token0
    self.token1 = token1
    self.callback_variant = _callback_variant


@external
@nonpayable
def set_next_swap(
    amount_in: uint256,
    amount_out: uint256,
    zero_for_one: bool,
):
    """Pre-configure the next swap and verify the pool holds enough output tokens."""
    assert msg.sender == OWNER
    assert amount_in != 0 and amount_out != 0, "Amounts must be non-zero"

    if zero_for_one:
        if self.token1 != empty(address):
            assert staticcall IERC20(self.token1).balanceOf(self) >= amount_out, "insufficient token1"
    else:
        if self.token0 != empty(address):
            assert staticcall IERC20(self.token0).balanceOf(self) >= amount_out, "insufficient token0"

    self.amount_in = amount_in
    self.amount_out = amount_out


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
    Perform a fake swap if the parameters match the configured swap.

    Transfers output tokens to recipient, then invokes the swap callback
    on msg.sender to request input tokens.
    """
    assert self.amount_in != 0 and self.amount_out != 0, "No swap configured"

    if amount_specified > 0:
        assert convert(amount_specified, uint256) == self.amount_in, "amount specified != amount_in"
    if amount_specified < 0:
        assert convert(-amount_specified, uint256) == self.amount_out, "amount specified != amount_out"

    # Transfer output to recipient
    if zero_for_one:
        extcall IERC20(self.token1).transfer(recipient, self.amount_out)
    else:
        extcall IERC20(self.token0).transfer(recipient, self.amount_out)

    # Compute delta values for callback
    amount0_delta: int256 = (
        convert(self.amount_in, int256)
        if zero_for_one
        else -convert(self.amount_out, int256)
    )
    amount1_delta: int256 = (
        -convert(self.amount_out, int256)
        if zero_for_one
        else convert(self.amount_in, int256)
    )

    # Invoke callback on the caller (the executor)
    if self.callback_variant == 0:
        extcall IUniswapV3SwapCallback(msg.sender).uniswapV3SwapCallback(
            amount0_delta, amount1_delta, data
        )
    else:
        extcall IPancakeV3SwapCallback(msg.sender).pancakeV3SwapCallback(
            amount0_delta, amount1_delta, data
        )

    # Verify input tokens were paid after callback
    if zero_for_one:
        assert staticcall IERC20(self.token0).balanceOf(self) >= self.amount_in, "token0 not paid"
    else:
        assert staticcall IERC20(self.token1).balanceOf(self) >= self.amount_in, "token1 not paid"

    self.amount_in = 0
    self.amount_out = 0

    return amount0_delta, amount1_delta


@external
@payable
def __default__():
    return
