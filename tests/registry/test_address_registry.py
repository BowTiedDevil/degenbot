"""Tests for the generic AddressRegistry base classes."""

import pytest

from degenbot.exceptions import DegenbotValueError
from degenbot.registry.base import AddressRegistry, MultiKeyAddressRegistry
from hexbytes import HexBytes


class FakeItem:
    """A simple item for testing registry storage."""

    def __init__(self, address: str):
        self.address = address


# Valid test addresses (20 bytes / 40 hex chars)
TEST_ADDRESS_1 = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
TEST_ADDRESS_2 = "0x6B175474E89094C44Da98b954EedeAC495271d0F"
TEST_MANAGER_ADDRESS = "0x1234567890123456789012345678901234567890"
TEST_POOL_ID = "0xabcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890"


class TestAddressRegistry:
    def test_get_missing_returns_none(self):
        registry = AddressRegistry()
        assert registry._get(chain_id=1, address=TEST_ADDRESS_1) is None

    def test_add_and_retrieve(self):
        registry = AddressRegistry()
        item = FakeItem(TEST_ADDRESS_1)

        registry._add(item=item, chain_id=1, address=TEST_ADDRESS_1)

        assert registry._get(chain_id=1, address=TEST_ADDRESS_1) is item
        assert len(registry) == 1

    def test_add_duplicate_raises_by_default(self):
        registry = AddressRegistry()
        item1 = FakeItem(TEST_ADDRESS_1)
        item2 = FakeItem(TEST_ADDRESS_2)

        registry._add(item=item1, chain_id=1, address=TEST_ADDRESS_1)

        with pytest.raises(DegenbotValueError, match="is already registered"):
            registry._add(item=item2, chain_id=1, address=TEST_ADDRESS_1)

    def test_add_duplicate_with_ignore(self):
        registry = AddressRegistry(on_duplicate="ignore")
        item1 = FakeItem(TEST_ADDRESS_1)
        item2 = FakeItem(TEST_ADDRESS_2)

        registry._add(item=item1, chain_id=1, address=TEST_ADDRESS_1)
        registry._add(item=item2, chain_id=1, address=TEST_ADDRESS_1)

        assert len(registry) == 1
        assert registry._get(chain_id=1, address=TEST_ADDRESS_1) is item1

    def test_add_duplicate_with_replace(self):
        registry = AddressRegistry(on_duplicate="replace")
        item1 = FakeItem(TEST_ADDRESS_1)
        item2 = FakeItem(TEST_ADDRESS_2)

        registry._add(item=item1, chain_id=1, address=TEST_ADDRESS_1)
        registry._add(item=item2, chain_id=1, address=TEST_ADDRESS_1)

        assert len(registry) == 1
        assert registry._get(chain_id=1, address=TEST_ADDRESS_1) is item2

    def test_remove_pool(self):
        registry = AddressRegistry()
        item = FakeItem(TEST_ADDRESS_1)

        registry._add(item=item, chain_id=1, address=TEST_ADDRESS_1)
        registry._remove(chain_id=1, address=TEST_ADDRESS_1)

        assert registry._get(chain_id=1, address=TEST_ADDRESS_1) is None
        assert len(registry) == 0

    def test_remove_missing_is_noop(self) -> None:
        registry = AddressRegistry()

        registry._remove(chain_id=1, address=TEST_ADDRESS_1)

        assert len(registry) == 0

    def test_list_all_yields_items(self):
        registry = AddressRegistry()
        item1 = FakeItem(TEST_ADDRESS_1)
        item2 = FakeItem(TEST_ADDRESS_2)

        registry._add(item=item1, chain_id=1, address=TEST_ADDRESS_1)
        registry._add(item=item2, chain_id=1, address=TEST_ADDRESS_2)

        items = list(registry.list_all())
        assert set(items) == {item1, item2}

    def test_checksumming_applied_to_addresses(self):
        registry = AddressRegistry()
        item = FakeItem(TEST_ADDRESS_1)

        # Add with lowercase, retrieve with checksummed
        registry._add(item=item, chain_id=1, address=TEST_ADDRESS_1.lower())
        # Use the same address - checksum function should handle it
        retrieved = registry._get(chain_id=1, address=TEST_ADDRESS_1)

        assert retrieved is item

    def test_name_in_error_message(self):
        registry = AddressRegistry(name="TestRegistry")
        item = FakeItem(TEST_ADDRESS_1)

        registry._add(item=item, chain_id=1, address=TEST_ADDRESS_1)

        with pytest.raises(DegenbotValueError, match="TestRegistry is already registered"):
            registry._add(item=item, chain_id=1, address=TEST_ADDRESS_1)


class TestMultiKeyAddressRegistry:
    def test_get_missing_returns_none(self):
        registry = MultiKeyAddressRegistry(
            address_fields=("pool_manager_address", "pool_id"),
        )
        assert (
            registry._get(
                chain_id=1,
                pool_manager_address=TEST_MANAGER_ADDRESS,
                pool_id=TEST_POOL_ID,
            )
            is None
        )

    def test_add_and_retrieve_with_two_keys(self):
        registry = MultiKeyAddressRegistry(
            address_fields=("pool_manager_address", "pool_id"),
        )
        item = FakeItem(TEST_ADDRESS_1)

        registry._add(
            item=item,
            chain_id=1,
            pool_manager_address=TEST_MANAGER_ADDRESS,
            pool_id=TEST_POOL_ID,
        )

        retrieved = registry._get(
            chain_id=1,
            pool_manager_address=TEST_MANAGER_ADDRESS,
            pool_id=TEST_POOL_ID,
        )
        assert retrieved is item
        assert len(registry) == 1

    def test_add_duplicate_raises_by_default(self):
        registry = MultiKeyAddressRegistry(
            address_fields=("pool_manager_address", "pool_id"),
        )
        item1 = FakeItem(TEST_ADDRESS_1)
        item2 = FakeItem(TEST_ADDRESS_2)

        registry._add(
            item=item1,
            chain_id=1,
            pool_manager_address=TEST_MANAGER_ADDRESS,
            pool_id=TEST_POOL_ID,
        )

        with pytest.raises(DegenbotValueError, match="is already registered"):
            registry._add(
                item=item2,
                chain_id=1,
                pool_manager_address=TEST_MANAGER_ADDRESS,
                pool_id=TEST_POOL_ID,
            )

    def test_remove_pool(self):
        registry = MultiKeyAddressRegistry(
            address_fields=("pool_manager_address", "pool_id"),
        )
        item = FakeItem(TEST_ADDRESS_1)

        registry._add(
            item=item,
            chain_id=1,
            pool_manager_address=TEST_MANAGER_ADDRESS,
            pool_id=TEST_POOL_ID,
        )
        registry._remove(
            chain_id=1,
            pool_manager_address=TEST_MANAGER_ADDRESS,
            pool_id=TEST_POOL_ID,
        )

        assert (
            registry._get(
                chain_id=1,
                pool_manager_address=TEST_MANAGER_ADDRESS,
                pool_id=TEST_POOL_ID,
            )
            is None
        )

    def test_remove_missing_is_noop(self) -> None:
        registry = MultiKeyAddressRegistry(
            address_fields=("pool_manager_address", "pool_id"),
        )

        registry._remove(
            chain_id=1,
            pool_manager_address=TEST_MANAGER_ADDRESS,
            pool_id=TEST_POOL_ID,
        )

        assert len(registry) == 0

    def test_missing_address_field_raises_error(self):
        registry = MultiKeyAddressRegistry(
            address_fields=("pool_manager_address", "pool_id"),
        )
        item = FakeItem(TEST_ADDRESS_1)

        with pytest.raises(ValueError, match="Missing required address field"):
            registry._add(
                item=item,
                chain_id=1,
                pool_manager_address=TEST_MANAGER_ADDRESS,  # Missing pool_id
            )

    def test_pool_id_not_checksummed(self):
        """Verify pool_id is kept as HexBytes, not checksummed."""

        registry = MultiKeyAddressRegistry(
            address_fields=("pool_manager_address", "pool_id"),
        )
        item = FakeItem(TEST_ADDRESS_1)

        pool_id_bytes = HexBytes(TEST_POOL_ID)

        registry._add(
            item=item,
            chain_id=1,
            pool_manager_address=TEST_MANAGER_ADDRESS,
            pool_id=pool_id_bytes,
        )

        # Should retrieve correctly (pool_id is kept as HexBytes)
        retrieved = registry._get(
            chain_id=1,
            pool_manager_address=TEST_MANAGER_ADDRESS,
            pool_id=pool_id_bytes,
        )
        assert retrieved is item
