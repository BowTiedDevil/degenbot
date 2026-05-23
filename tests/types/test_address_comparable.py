"""Tests verifying AddressComparable mixin provides address-based comparison and hashing.
"""

import pytest
from eth_typing import ChecksumAddress
from hexbytes import HexBytes

from degenbot.types.address_comparable import AddressComparable


class _FakeOnChainEntity(AddressComparable):
    def __init__(self, address: ChecksumAddress, name: str) -> None:
        self.address = address
        self.name = name


class TestAddressComparableEquality:
    def test_equal_when_same_address(self):
        """Two AddressComparable objects with the same address are equal."""
        a = _FakeOnChainEntity("0xAb5801a7D398351b8bE11C439e05C5B3259aeC9B", "A")
        b = _FakeOnChainEntity("0xAb5801a7D398351b8bE11C439e05C5B3259aeC9B", "B")
        assert a == b

    def test_not_equal_when_different_address(self):
        """Two AddressComparable objects with different addresses are not equal."""
        a = _FakeOnChainEntity("0xAb5801a7D398351b8bE11C439e05C5B3259aeC9B", "A")
        b = _FakeOnChainEntity("0x1f9840a85d5aF5bf1D1762F925BDADdC4201F984", "B")
        assert a != b

    def test_equal_to_string_of_address(self):
        """An AddressComparable object is equal to its address as a string."""
        addr = "0xAb5801a7D398351b8bE11C439e05C5B3259aeC9B"
        a = _FakeOnChainEntity(addr, "A")
        assert a == addr

    def test_equal_to_hexbytes_of_address(self):
        """An AddressComparable object is equal to its address as HexBytes."""
        addr = "0xAb5801a7D398351b8bE11C439e05C5B3259aeC9B"
        a = _FakeOnChainEntity(addr, "A")
        assert a == HexBytes(addr)

    def test_equal_to_bytes_of_address(self):
        """An AddressComparable object is equal to its address as bytes."""
        addr = "0xAb5801a7D398351b8bE11C439e05C5B3259aeC9B"
        a = _FakeOnChainEntity(addr, "A")
        assert a == bytes.fromhex(addr[2:])

    def test_not_equal_to_unrelated_type(self):
        """An AddressComparable is not equal to an unrelated type."""
        a = _FakeOnChainEntity("0xAb5801a7D398351b8bE11C439e05C5B3259aeC9B", "A")
        assert a != 42


class TestAddressComparableOrdering:
    def test_lt_by_address(self):
        """AddressComparable objects are ordered by address."""
        a = _FakeOnChainEntity("0x0000000000000000000000000000000000000001", "A")
        b = _FakeOnChainEntity("0x0000000000000000000000000000000000000002", "B")
        assert a < b
        assert not (b < a)

    def test_gt_by_address(self):
        """AddressComparable objects are ordered by address."""
        a = _FakeOnChainEntity("0x0000000000000000000000000000000000000001", "A")
        b = _FakeOnChainEntity("0x0000000000000000000000000000000000000002", "B")
        assert b > a
        assert not (a > b)

    def test_lt_compared_to_string(self):
        """AddressComparable objects can be compared to address strings."""
        a = _FakeOnChainEntity("0x0000000000000000000000000000000000000001", "A")
        assert a < "0x0000000000000000000000000000000000000002"

    def test_lt_unrelated_type_raises_type_error(self):
        """Ordering comparison with an unrelated type raises TypeError."""
        a = _FakeOnChainEntity("0x0000000000000000000000000000000000000001", "A")
        with pytest.raises(TypeError):
            _ = a < 42


class TestAddressComparableHashing:
    def test_hash_matches_address_hash(self):
        """Hash is based on address only."""
        addr = "0xAb5801a7D398351b8bE11C439e05C5B3259aeC9B"
        a = _FakeOnChainEntity(addr, "A")
        assert hash(a) == hash(addr)

    def test_same_address_same_hash(self):
        """Two objects with the same address have the same hash."""
        a = _FakeOnChainEntity("0xAb5801a7D398351b8bE11C439e05C5B3259aeC9B", "A")
        b = _FakeOnChainEntity("0xAb5801a7D398351b8bE11C439e05C5B3259aeC9B", "B")
        assert hash(a) == hash(b)
