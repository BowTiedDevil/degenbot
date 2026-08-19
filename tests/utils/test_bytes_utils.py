"""Parity tests for degenbot.utils.bytes (to_bytes / to_0x_hex).

Golden vectors pinned from the old 1.3-era hex-bytes wrapper before its
removal (ergo JWXZ4A, option D: the wrapper is abolished; these functions
carry over its conversion matrix). Note the preserved upstream quirks:
- odd-length check happens on the ORIGINAL string (prefix included);
- non-ASCII chars propagate UnicodeEncodeError (upstream's except clause
  catches UnicodeDecodeError, which .encode() never raises);
- ASCII non-hex chars raise binascii.Error (a ValueError subclass).
"""

import binascii

import pytest

from degenbot.utils.bytes import to_0x_hex, to_bytes


def test_to_bytes_passthrough():
    b = b"ab"
    assert to_bytes(b) is b
    assert to_bytes(b"\x00\x0f\xff").hex() == "000fff"


def test_to_bytes_bytearray_and_memoryview():
    assert to_bytes(bytearray(b"\x01\x02")).hex() == "0102"
    assert to_bytes(memoryview(b"\x7f")).hex() == "7f"


def test_to_bytes_bool_before_int():
    assert to_bytes(True) == b"\x01"  # ruff: ignore[boolean-positional-value-in-call] - bool is the value under test
    assert to_bytes(False) == b"\x00"  # ruff: ignore[boolean-positional-value-in-call] - bool is the value under test


def test_to_bytes_int():
    assert to_bytes(0) == b"\x00"
    assert to_bytes(1) == b"\x01"
    assert to_bytes(255) == b"\xff"
    assert to_bytes(256) == b"\x01\x00"
    assert to_bytes(1 << 40).hex() == "010000000000"


def test_to_bytes_negative_int():
    with pytest.raises(ValueError, match="Cannot convert negative integer"):
        to_bytes(-1)


def test_to_bytes_hexstr():
    assert to_bytes("0x42069").hex() == "042069"  # odd -> left-padded
    assert to_bytes("42069").hex() == "042069"  # unprefixed, same rule
    assert to_bytes("0x042066").hex() == "042066"
    assert to_bytes("042066").hex() == "042066"
    assert to_bytes("0x0").hex() == "00"
    assert to_bytes("0").hex() == "00"
    assert to_bytes("") == b""
    assert to_bytes("0xdeadBEEF").hex() == "deadbeef"


def test_to_bytes_bad_hexstr_raises():
    with pytest.raises(binascii.Error):
        to_bytes("zz")
    with pytest.raises(binascii.Error):
        to_bytes("0xzz")
    with pytest.raises(UnicodeEncodeError):
        to_bytes("\u00e9")


def test_to_bytes_type_error():
    with pytest.raises(TypeError, match="Cannot convert"):
        to_bytes(None)


def test_to_0x_hex():
    # bytes are RAW data: re-encoded via .hex(), NOT decoded as hex text
    assert to_0x_hex(b"\x42\x06") == "0x4206"
    assert to_0x_hex(b"") == "0x"
    assert to_0x_hex(bytes([0, 15, 255])) == "0x000fff"
    assert to_0x_hex(bytearray(b"\xab\xcd")) == "0xabcd"
    assert to_0x_hex(b"V4POOLID" * 4) == "0x" + (b"V4POOLID" * 4).hex()


def test_to_0x_hex_string_input():
    # str is decoded as hex first, then re-encoded
    assert to_0x_hex("0x1122") == "0x1122"
    assert to_0x_hex("ab") == "0xab"
    assert to_0x_hex("0x42069") == "0x042069"  # left-padded like to_bytes
    assert to_0x_hex("") == "0x"
