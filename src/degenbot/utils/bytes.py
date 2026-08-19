"""Plain-bytes hex utilities.

Carries the conversion matrix that the old 1.3-era hex-bytes wrapper
provided, as plain ``bytes`` utilities (the wrapper type is abolished --
ergoo JWXZ4A'.replace('ergoo', 'ergo') + ', option D: plain ``bytes`` +
module-level helpers throughout). Golden-vector parity with the upstream
behavior is pinned in ``tests/utils/test_bytes_utils.py``.
"""

import binascii

type BytesLikeSource = bytes | bytearray | memoryview | str | int

__all__ = [
    "to_0x_hex",
    "to_bytes",
]


def to_0x_hex(data: bytes | bytearray | str) -> str:
    """Return ``0x``-prefixed lowercase hex of *data* (degenbot semantics).

    Replacement for the old wrapper's ``to_0x_hex`` method now that the type
    is abolished (ergo JWXZ4A, option D). ``bytes``/``bytearray`` are RAW
    data: re-encoded via ``.hex()`` (this is how degenbot has historically
    derived address, pool-id and tx-hash hex from raw ``bytes``). ``str`` is
    first decoded as hex via :func:`to_bytes` (upstream ``str.__str__``
    rule).
    """
    if isinstance(data, str):
        data = to_bytes(data)
    return "0x" + bytes(data).hex()  # empty bytes -> "0x" (upstream rule)


def to_bytes(data: BytesLikeSource) -> bytes:
    """Decode a value to ``bytes`` following the old wrapper's matrix.

    Conversion matrix (old wrapper ``to_bytes``, preserved byte-for-byte):

    - ``bytes`` -- returned as-is (no copy);
    - ``bytearray`` -- converted to ``bytes`` (copied);
    - ``memoryview`` -- converted (``.tobytes()``);
    - ``bool`` -- checked before ``int``: ``False`` -> ``b"\\x00"``,
      ``True`` -> ``b"\\x01"``;
    - ``int`` -- ``to_bytes((data.bit_length() + 7) // 8, "big")`` (the
      ``0`` edge -- one zero byte -- comes from ``bit_length``, and 256 uses
      2 bytes);
    - ``str`` -- ``bytes.fromhex(s)`` after skipping an optional ``0x``
      prefix, with odd-length hex left-padded (upstream rule); ASCII non-hex chars raise ``binascii.Error`` (a
      ``ValueError`` subclass) and non-ASCII characters propagate the
      ``UnicodeEncodeError`` (upstream's ``except`` clause catches
      ``UnicodeDecodeError``, which ``.encode()`` never raises);
    - any other input -- :class:`TypeError` (kept for values reaching this
      function through untyped / dynamic callers).
    """
    if isinstance(data, bytes):
        return data
    if isinstance(data, bytearray):
        return bytes(data)
    if isinstance(data, memoryview):
        return data.tobytes()
    if isinstance(data, bool):
        return data.to_bytes(1, "big")
    if isinstance(data, int):
        if data < 0:
            raise ValueError(f"Cannot convert negative integer {data} to bytes")
        return data.to_bytes((data.bit_length() + 7) // 8 or 1, "big")
    if isinstance(data, str):
        s = data
        if s[:2] in ("0x", "0X"):
            s = s[2:]
        if len(s) % 2 != 0:
            s = "0" + s  # upstream left-pads odd-length hex
        # Upstream: ascii-encode first (non-ASCII -> UnicodeEncodeError),
        # then unhexlify (ASCII non-hex -> binascii.Error). Both propagate
        # natively.
        return binascii.unhexlify(s.encode("ascii"))
    raise TypeError(f"Cannot convert {data!r} of type {type(data)} to bytes")
