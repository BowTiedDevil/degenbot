# Rust extension module for degenbot.
#
# This module provides high-performance Rust implementations of common operations
# used by the degenbot Python package.

# Converts a tick value to its corresponding sqrt price (X96 format).
#
# This function calculates the sqrt price for a given tick value using the
# Uniswap V3 tick math formula.
#
# Args:
#     tick: The tick value in range [-887272, 887272]
#
# Returns:
#     The sqrt price X96 value as an integer
#
# Raises:
#     ValueError: If the tick value is outside the valid range
def get_sqrt_ratio_at_tick(tick: int) -> int: ...

# Converts a sqrt price (X96 format) to its corresponding tick value.
#
# This function calculates the tick for a given sqrt price using the
# Uniswap V3 tick math formula.
#
# Args:
#     sqrt_price_x96: The sqrt price X96 value as an integer or bytes (max 20 bytes)
#
# Returns:
#     The tick value corresponding to the given sqrt price
#
# Raises:
#     ValueError: If the sqrt price is too large (exceeds 20 bytes)
#         or outside the valid [MIN_SQRT_RATIO, MAX_SQRT_RATIO) range
#     TypeError: If the input is not an int or bytes
def get_tick_at_sqrt_ratio(sqrt_price_x96: int | bytes) -> int: ...

# Generates an EIP-55 checksummed address from the input.
#
# Accepts either a hex string or a 20-byte sequence and returns
# a checksummed Ethereum address.
#
# Args:
#     address: A hex string (with or without '0x' prefix) or 20-byte
#         sequence representing an address
#
# Returns:
#     A checksummed Ethereum address string with proper uppercase/lowercase
#
# Raises:
#     ValueError: If the string is not a valid hex address, or if bytes
#         are not exactly 20 bytes long
#     TypeError: If the input is not a string or bytes
#
# Example:
#     >>> to_checksum_address("0x66f9664f97f2b50f62d13ea064982f936de76657")
#     '0x66F9664f97f2B50f62d13Ea064982f936de76657'
def to_checksum_address(address: str | bytes) -> str: ...
