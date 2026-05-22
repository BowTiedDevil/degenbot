"""AddressComparable: address wrapper with equality by checksum."""
from eth_typing import ChecksumAddress
from hexbytes import HexBytes


class AddressComparable:
    """Mixin providing address-based comparison, ordering, and hashing.

    Any on-chain entity identified by a ChecksumAddress can inherit from this
    to get consistent equality, ordering, and hashing by address.
    """

    address: ChecksumAddress

    def __eq__(self, other: object) -> bool:
        """Check equality with another object."""
        match other:
            case AddressComparable():
                return self.address == other.address
            case HexBytes():
                return self.address.lower() == other.to_0x_hex().lower()
            case bytes():
                return self.address.lower() == "0x" + other.hex().lower()
            case str():
                return self.address.lower() == other.lower()
            case _:
                return NotImplemented

    def __lt__(self, other: object) -> bool:
        """Implement __lt__."""
        match other:
            case AddressComparable():
                return self.address < other.address
            case HexBytes():
                return self.address.lower() < other.to_0x_hex().lower()
            case bytes():
                return self.address.lower() < "0x" + other.hex().lower()
            case str():
                return self.address.lower() < other.lower()
            case _:
                return NotImplemented

    def __gt__(self, other: object) -> bool:
        """Implement __gt__."""
        match other:
            case AddressComparable():
                return self.address > other.address
            case HexBytes():
                return self.address.lower() > other.to_0x_hex().lower()
            case bytes():
                return self.address.lower() > "0x" + other.hex().lower()
            case str():
                return self.address.lower() > other.lower()
            case _:
                return NotImplemented

    def __hash__(self) -> int:
        """Return the hash value."""
        return hash(self.address)
