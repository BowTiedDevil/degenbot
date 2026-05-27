"""
Fake Uniswap V2 pair for testing.

Mimics the V2 swap interface:
- set_next_swap: pre-configure swap amounts and fund the output
- swap: execute a configured swap, invoke callback if data is non-empty

The V2 swap is an optimistic transfer: output tokens are sent to `to`
BEFORE any input is received. If data is non-empty, a callback
(uniswapV2Call) is invoked on `to`, giving the caller a chance to
pay the input tokens. After the callback (or immediately if no callback),
the pair checks that its balance has increased by the required input amount.

Unlike V3, there is no auto-pay of WETH — the executor must transfer
tokens to the pair before or during the callback.

The pair requires the output token to be pre-funded (via mint from
the token contract) before set_next_swap is called.
"""

from .interfaces.UniswapV2 import IUniswapV2Pair
from .interfaces.UniswapV2 import IUniswapV2Callee
from ethereum.ercs import IERC20

MAX_CALLDATA_LENGTH: constant(uint256) = 4096

implements: IUniswapV2Pair

OWNER: immutable(address)

token0: public(address)
token1: public(address)

amount_in: public(uint256)
amount_out: public(uint256)


@deploy
def __init__(token0: address, token1: address):
    OWNER = msg.sender
    self.token0 = token0
    self.token1 = token1


@external
@nonpayable
def set_next_swap(
    amount_in: uint256,
    amount_out: uint256,
    zero_for_one: bool,
):
    """Pre-configure the next swap and verify the pair holds enough output tokens."""
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


@external
@nonpayable
def swap(
    amount0Out: uint256,
    amount1Out: uint256,
    to: address,
    data: Bytes[MAX_CALLDATA_LENGTH],
):
    """
    Perform a fake swap.

    Transfers output tokens to `to`, invokes callback if data is non-empty,
    then asserts the input tokens were paid.
    """
    assert self.amount_in != 0 and self.amount_out != 0, "No swap configured"

    # Determine direction from which output is requested
    zero_for_one: bool = amount1Out > 0
    if zero_for_one:
        assert amount1Out == self.amount_out, "amount1Out != configured amount_out"
    else:
        assert amount0Out == self.amount_out, "amount0Out != configured amount_out"

    # Transfer output tokens to `to`
    if zero_for_one:
        extcall IERC20(self.token1).transfer(to, self.amount_out)
    else:
        extcall IERC20(self.token0).transfer(to, self.amount_out)

    # Invoke callback if data is non-empty
    if len(data) > 0:
        extcall IUniswapV2Callee(to).uniswapV2Call(
            msg.sender, amount0Out, amount1Out, data
        )

    # Verify input tokens were paid after callback
    if zero_for_one:
        assert staticcall IERC20(self.token0).balanceOf(self) >= self.amount_in, "token0 not paid"
    else:
        assert staticcall IERC20(self.token1).balanceOf(self) >= self.amount_in, "token1 not paid"

    self.amount_in = 0
    self.amount_out = 0


@external
@payable
def __default__():
    return
