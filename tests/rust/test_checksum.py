import degenbot_rs
import pytest


def test_rust_checksum(random_addresses, checksummed_random_addresses):
    for address, checksum in zip(random_addresses, checksummed_random_addresses, strict=True):
        assert degenbot_rs.to_checksum_address(address) == checksum


def test_to_checksum_address_invalid_type():
    with pytest.raises(TypeError, match="Address must be string or bytes"):
        degenbot_rs.to_checksum_address(123)
    with pytest.raises(TypeError, match="Address must be string or bytes"):
        degenbot_rs.to_checksum_address(["0x" + "00" * 20])


def test_to_checksum_address_invalid_byte_length():
    for input_length in list(range(20)) + list(range(21, 32)):
        with pytest.raises(ValueError, match="Address must be 20 bytes"):
            degenbot_rs.to_checksum_address(b"\x00" * input_length)


def test_to_checksum_address_invalid_hex_string():
    with pytest.raises(ValueError, match="invalid character"):
        degenbot_rs.to_checksum_address("0x000000000000000000000000000000000000000g")


def test_checksum_known_addresses():
    assert (
        degenbot_rs.to_checksum_address("0x0000000000000000000000000000000000000000")
        == "0x0000000000000000000000000000000000000000"
    )
    assert (
        degenbot_rs.to_checksum_address("0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2")
        == "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
    )
    assert (
        degenbot_rs.to_checksum_address("0x000000000004444c5dc75cb358380d2e3de08a90")
        == "0x000000000004444c5dc75cB358380D2e3dE08A90"
    )
