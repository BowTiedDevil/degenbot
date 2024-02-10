from eth_typing import ChecksumAddress
from typing import Any


class ArbitrageHelper:
    gas_estimate: int


class HelperManager:
    """An abstract base class for managers that generate, track and distribute various helper classes"""

    ...


class AbstractPoolUpdate:
    ...


class PoolHelper:
    address: ChecksumAddress
    name: str

    def __eq__(self, other: Any) -> bool:
        if isinstance(other, PoolHelper):
            return self.address == other.address
        elif isinstance(other, bytes):
            return self.address.lower() == other.hex().lower()
        elif isinstance(other, str):
            return self.address.lower() == other.lower()
        else:
            return NotImplemented

    def __lt__(self, other: Any) -> bool:
        if isinstance(other, PoolHelper):
            return self.address < other.address
        elif isinstance(other, bytes):
            return self.address.lower() < other.hex().lower()
        elif isinstance(other, str):
            return self.address.lower() < other.lower()
        else:
            return NotImplemented

    def __gt__(self, other: Any) -> bool:
        if isinstance(other, PoolHelper):
            return self.address > other.address
        elif isinstance(other, bytes):
            return self.address.lower() > other.hex().lower()
        elif isinstance(other, str):
            return self.address.lower() > other.lower()
        else:
            return NotImplemented

    def __hash__(self) -> int:
        return hash(self.address)

    def __str__(self) -> str:
        return self.name


class TokenHelper:
    address: ChecksumAddress

    def __eq__(self, other: Any) -> bool:
        if isinstance(other, TokenHelper):
            return self.address == other.address
        elif isinstance(other, bytes):
            return self.address.lower() == other.hex().lower()
        elif isinstance(other, str):
            return self.address.lower() == other.lower()
        else:
            return NotImplemented

    def __lt__(self, other: Any) -> bool:
        if isinstance(other, TokenHelper):
            return self.address < other.address
        elif isinstance(other, bytes):
            return self.address.lower() < other.hex().lower()
        elif isinstance(other, str):
            return self.address.lower() < other.lower()
        else:
            return NotImplemented

    def __gt__(self, other: Any) -> bool:
        if isinstance(other, TokenHelper):
            return self.address > other.address
        elif isinstance(other, bytes):
            return self.address.lower() > other.hex().lower()
        elif isinstance(other, str):
            return self.address.lower() > other.lower()
        else:
            return NotImplemented


class TransactionHelper:
    ...
