"""AddressComparable: address wrapper with equality by checksum."""

from __future__ import annotations

from typing import TYPE_CHECKING

from degenbot.utils.bytes import to_0x_hex

if TYPE_CHECKING:
    from degenbot._ffi import ChecksummedAddress


class AddressComparable:
    """Mixin providing address-based comparison, ordering, and hashing.

    Any on-chain entity identified by a ChecksummedAddress can inherit from this
    to get consistent equality, ordering, and hashing by address.
    """

    address: ChecksummedAddress

    def __eq__(self, other: object) -> bool:
        """Check equality with another object.

        Returns:
            The computed value.

        """
        match other:
            case AddressComparable():
                return self.address == other.address
            case bytes():
                return self.address.lower() == to_0x_hex(other).lower()
            case str():
                return self.address.lower() == other.lower()
            case _:
                return NotImplemented

    def __lt__(self, other: object) -> bool:
        """Implement __lt__.

        Returns:
            The computed value.

        """
        match other:
            case AddressComparable():
                return self.address < other.address
            case bytes():
                return self.address.lower() < to_0x_hex(other).lower()
            case str():
                return self.address.lower() < other.lower()
            case _:
                return NotImplemented

    def __gt__(self, other: object) -> bool:
        """Implement __gt__.

        Returns:
            The computed value.

        """
        match other:
            case AddressComparable():
                return self.address > other.address
            case bytes():
                return self.address.lower() > to_0x_hex(other).lower()
            case str():
                return self.address.lower() > other.lower()
            case _:
                return NotImplemented

    def __hash__(self) -> int:
        """Return the hash value.

        Returns:
            The computed value.

        """
        return hash(self.address)
