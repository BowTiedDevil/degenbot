"""
Bytes normalization utilities.

Provides utilities for normalizing between bytes and HexBytes
types when working with web3 providers.
"""

from hexbytes import HexBytes

# Type alias for any bytes-like type that can be returned by web3 providers
type HexBytesLike = bytes | HexBytes


def to_bytes(data: HexBytesLike) -> bytes:
    """
    Normalize any hex-bytes type to plain bytes.

    Use this when you need to pass data to libraries that require plain bytes
    (e.g., eth_abi which has hard-coded isinstance checks).

    Args:
        data: Any of bytes or HexBytes

    Returns:
        Plain bytes object

    Example:
        >>> from degenbot.provider import AlloyProvider
        >>> provider = AlloyProvider("https://...")
        >>> result = provider.call("0x...", calldata)  # Returns HexBytes
        >>> raw_bytes = to_bytes(result)  # Normalize to bytes
    """
    return bytes(data)


def to_hex(data: HexBytesLike) -> str:
    """
    Normalize any hex-bytes type to 0x-prefixed lowercase hex string.

    Args:
        data: Any of bytes or HexBytes

    Returns:
        0x-prefixed lowercase hex string

    Example:
        >>> from degenbot.provider import AlloyProvider
        >>> provider = AlloyProvider("https://...")
        >>> result = provider.call("0x...", calldata)
        >>> hex_str = to_hex(result)  # "0x..."
    """
    if isinstance(data, bytes | bytearray | memoryview):
        return "0x" + data.hex()
    # HexBytes.hex() returns unprefixed (same as bytes.hex())
    hex_result = data.hex()
    if not hex_result.startswith("0x"):
        return "0x" + hex_result
    return hex_result


__all__ = [
    "HexBytesLike",
    "to_bytes",
    "to_hex",
]
