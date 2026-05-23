"""Generic address-based registry base classes.

This module provides abstract base classes for registries that index items by
chain ID and address(es). The common pattern (checksumming, deduplication, key
construction, removal) is implemented once, then reused by concrete registries.

The base classes provide protected methods (_get, _add, _remove) that handle
the shared machinery (key construction, deduplication, storage). Concrete
subclasses define their own publicly-typed get/add/remove methods with
domain-specific parameter names that delegate to the protected methods.

Usage:
    class TokenRegistry(AddressRegistry[Erc20Token]):
        def get(self, token_address: str, chain_id: int) -> Erc20Token | None:
            return self._get(chain_id=chain_id, address=token_address)

        def add(self, token: Erc20Token, chain_id: int) -> None:
            self._add(item=token, chain_id=chain_id, address=token.address)

    class ManagedPoolRegistry(MultiKeyAddressRegistry[AbstractLiquidityPool]):
        def __init__(self) -> None:
            super().__init__(
                address_fields=("pool_manager_address", "pool_id"),
                name="ManagedPool",
            )

        def get(self, chain_id: int, pool_manager_address: str, pool_id: bytes) -> ...:
            return self._get(
                chain_id=chain_id,
                pool_manager_address=pool_manager_address,
                pool_id=pool_id,
            )
"""

from __future__ import annotations

from abc import ABC, abstractmethod
from typing import TYPE_CHECKING, Any, Protocol

from hexbytes import HexBytes

from degenbot.checksum_cache import get_checksum_address
from degenbot.exceptions import DegenbotValueError
from degenbot.types.abstract import AbstractRegistry

if TYPE_CHECKING:
    from collections.abc import Iterable

    from eth_typing import ChecksumAddress


class AddressFunction(Protocol):
    """Protocol for address checksumming functions."""

    def __call__(self, address: str | bytes) -> ChecksumAddress:
        """Checksum the given address."""
        ...


class AbstractAddressRegistry[T](AbstractRegistry, ABC):
    """Abstract base for address-based registries.

    Provides shared functionality for:
    - Address checksumming
    - Key construction (chain_id + addresses)
    - Deduplication checks
    - Storage access

    Concrete implementations must implement:
    - _build_key(): Build the lookup key for storage
    - _storage: Property returning the backing dict

    Subclasses define their own typed public methods (get, add, remove)
    with domain-specific parameter names that delegate to the protected
    _get, _add, _remove methods.
    """

    def __init__(
        self,
        *,
        checksum_fn: AddressFunction = get_checksum_address,
        on_duplicate: str = "error",
        name: str = "AbstractAddressRegistry",
    ) -> None:
        """Initialize the registry.

        Args:
            checksum_fn: Function to convert addresses to checksummed form.
            on_duplicate: Behavior when adding an item that already exists:
                - "error": Raise DegenbotValueError (default, maintains existing behavior)
                - "ignore": Silently skip the duplicate (no-op)
                - "replace": Overwrite the existing item with the new one
            name: Registry name for debugging/error messages.

        Raises:
            ValueError: If on_duplicate is not one of "error", "ignore", or "replace".

        """
        if on_duplicate not in {"error", "ignore", "replace"}:
            msg = (
                f"Invalid on_duplicate value: {on_duplicate!r}. "
                "Must be 'error', 'ignore', or 'replace'"
            )
            raise ValueError(msg)

        self._checksum_fn = checksum_fn
        self._on_duplicate = on_duplicate
        self._name = name

    @abstractmethod
    def _build_key(
        self,
        chain_id: int,
        **address_args: str | bytes,
    ) -> tuple[Any, ...]:
        """Build the lookup key for the given chain and addresses."""

    @abstractmethod
    def _storage(self) -> dict[Any, T]:
        """Backing storage for this registry."""

    def _get(
        self,
        chain_id: int,
        **address_args: str | bytes,
    ) -> T | None:
        """Retrieve an item by chain and addresses.

        Args:
            chain_id: Chain ID
            **address_args: Address fields (e.g., address, token0, token1,
                pool_manager_address, pool_id)

        Returns:
            The registered item or None if not found.

        """
        key = self._build_key(chain_id, **address_args)
        return self._storage().get(key)

    def _add(
        self,
        item: T,
        chain_id: int,
        **address_args: str | bytes,
    ) -> None:
        """Register an item.

        Args:
            item: The item to register
            chain_id: Chain ID
            **address_args: Address fields for key construction

        Raises:
            DegenbotValueError: If item already exists and on_duplicate="error"

        """
        key = self._build_key(chain_id, **address_args)

        if key in self._storage():
            if self._on_duplicate == "error":
                raise DegenbotValueError(message=f"{self._name} is already registered at key {key}")
            if self._on_duplicate == "ignore":
                return
            # "replace" falls through to overwrite

        self._storage()[key] = item

    def _remove(
        self,
        chain_id: int,
        **address_args: str | bytes,
    ) -> None:
        """Remove an item from the registry.

        Args:
            chain_id: Chain ID
            **address_args: Address fields for key construction

        """
        key = self._build_key(chain_id, **address_args)
        self._storage().pop(key, None)

    def list_all(self) -> Iterable[T]:
        """Yield all registered items.

        Yields:
            Registered items.

        """
        yield from self._storage().values()

    def __len__(self) -> int:
        """Return the number of registered items.

        Returns:
            The number of registered items.

        """
        return len(self._storage())

    def reset(self) -> None:
        """Reset the registry storage (called by concrete subclasses)."""
        self._storage().clear()


class AddressRegistry[T](AbstractAddressRegistry[T]):
    """Simple address-based registry with a single primary address field.

    Key structure: (chain_id, checksummed_address)

    This class provides the _build_key and _storage implementations for
    single-address registries. Subclasses define their own typed public
    get/add/remove methods with domain-specific parameter names that
    delegate to _get/_add/_remove.

    Examples:
        - TokenRegistry (chain_id, token_address)
        - PoolRegistry for non-V4 pools (chain_id, pool_address)

    Usage:
        class TokenRegistry(AddressRegistry[Erc20Token]):
            def get(self, token_address: str, chain_id: int) -> Erc20Token | None:
                return self._get(chain_id=chain_id, address=token_address)

            def add(self, token: Erc20Token, chain_id: int) -> None:
                self._add(item=token, chain_id=chain_id, address=token.address)

    """

    def __init__(
        self,
        *,
        name: str = "AddressRegistry",
        checksum_fn: AddressFunction = get_checksum_address,
        on_duplicate: str = "error",
    ) -> None:
        """Initialize the registry.

        Args:
            name: Registry name for debugging/error messages
            checksum_fn: Function to convert addresses to checksummed form
            on_duplicate: Behavior on duplicate add operation

        """
        super().__init__(checksum_fn=checksum_fn, on_duplicate=on_duplicate, name=name)
        self._items: dict[tuple[int, ChecksumAddress], T] = {}

    def _build_key(
        self,
        chain_id: int,
        address: str | bytes = "",
        **address_args: str | bytes,
    ) -> tuple[int, ChecksumAddress]:
        """Build key (chain_id, checksummed_address).

        Returns:
            Tuple of (chain_id, checksummed_address).

        Raises:
            ValueError: If no address is provided for key construction.

        """
        first_address = next(iter(address_args.values()), address)
        if not first_address:
            msg = "No address provided for key construction"
            raise ValueError(msg)
        return (chain_id, self._checksum_fn(first_address))

    def _storage(self) -> dict[tuple[int, ChecksumAddress], T]:
        """Backing storage for this registry.

        Returns:
            The backing dictionary.

        """
        return self._items


class MultiKeyAddressRegistry[T](AbstractAddressRegistry[T]):
    """Address-based registry with multiple address fields in the key.

    Key structure: (chain_id, checksummed_address_1, checksummed_address_2, ...)

    Primary use case: V4 PoolRegistry uses (chain_id, pool_manager_address, pool_id)

    The address fields are defined at construction time and determine the
    keyword arguments accepted by get/add/remove. Subclasses should define
    their own typed get/add/remove with explicit parameter names for
    domain clarity.

    Usage:
        class ManagedPoolRegistry(MultiKeyAddressRegistry[ConcentratedLiquidityPool]):
            def __init__(self) -> None:
                super().__init__(
                    address_fields=("pool_manager_address", "pool_id"),
                    name="ManagedPool",
                )

            def get(self, chain_id, pool_manager_address, pool_id):
                return self._get(
                    chain_id=chain_id,
                    pool_manager_address=pool_manager_address,
                    pool_id=pool_id,
                )
    """

    def __init__(
        self,
        *,
        address_fields: tuple[str, ...],
        name: str = "MultiKeyAddressRegistry",
        checksum_fn: AddressFunction = get_checksum_address,
        on_duplicate: str = "error",
    ) -> None:
        """Initialize the registry.

        Args:
            address_fields: List of address field names in key order
                Example for V4: ("pool_manager_address", "pool_id")
            name: Registry name for debugging
            checksum_fn: Function to convert addresses to checksummed form
            on_duplicate: Behavior on duplicate add operation

        """
        super().__init__(checksum_fn=checksum_fn, on_duplicate=on_duplicate, name=name)
        self._address_fields = address_fields
        self._items: dict[tuple[Any, ...], T] = {}

    def _build_key(
        self,
        chain_id: int,
        **address_args: str | bytes,
    ) -> tuple[Any, ...]:
        """Build key (chain_id, checksummed_address_1, ...).

        Returns:
            Tuple of (chain_id, checksummed_address_1, ...).

        Raises:
            ValueError: If a required address field is missing.

        """
        key_parts: list[Any] = [chain_id]

        for field in self._address_fields:
            if field not in address_args:
                msg = f"Missing required address field: {field}"
                raise ValueError(msg)
            value = address_args[field]
            if field == "pool_id":
                # pool_id is kept as HexBytes, not checksummed
                key_parts.append(HexBytes(value))
            else:
                key_parts.append(self._checksum_fn(value))

        return tuple(key_parts)

    def _storage(self) -> dict[tuple[Any, ...], T]:
        """Backing storage for this registry.

        Returns:
            The backing dictionary.

        """
        return self._items
