from eth_typing import ChecksumAddress

from degenbot.types.address_comparable import AddressComparable


class AbstractErc20Token(AddressComparable):
    address: ChecksumAddress
    symbol: str
    name: str
    decimals: int

    def __str__(self) -> str:
        return self.symbol
