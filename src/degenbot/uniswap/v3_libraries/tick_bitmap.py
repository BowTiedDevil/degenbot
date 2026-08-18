"""Uniswap V3 TickBitmap: compressed tick position lookup.

See: contract_reference/uniswap/V3/UniswapV3Factory.sol (TickBitmap library)
"""


# NOTE: Pydantic validation is applied to certain functions to enforce the built-in integer range
# guarantees from the Solidity contract. Pydantic's validation will copy mutable arguments when
# validating, which defeats the in-place mutation performed by certain functions. The
# `SkipValidation` type is applied so the original dict/list is referenced.


def position(tick: int) -> tuple[int, int]:
    """Compute the position in the mapping where the initialized bit for a tick is placed.

    Returns:
        A tuple of (word_pos, bit_pos).

    """
    return (
        tick >> 8,  # word_pos
        tick % 256,  # bit_pos
    )
