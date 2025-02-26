# @dev round towards negative infinity
def compress(tick: int, tick_spacing: int) -> int:
    """
    Compress the given tick by the spacing, rounding down towards negative infinity
    """
    # Use Python floor division directly, which matches rounding specified by the function
    return tick // tick_spacing


# @notice Computes the position in the mapping where the initialized bit for a tick lives
# @param tick The tick for which to compute the position
# @return wordPos The key in the mapping containing the word in which the bit is stored
# @return bitPos The bit position in the word where the flag is stored
def position(tick: int) -> tuple[int, int]:
    return tick >> 8, tick % 256
