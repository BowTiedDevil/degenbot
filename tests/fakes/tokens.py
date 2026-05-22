"""
Consolidated fake token implementations.

Replaces the previous ad hoc token fakes:
- FakeToken (from tests/arbitrage/test_path/conftest.py)
- MockToken (from tests/arbitrage/test_optimizers/test_v2_v3_optimizer.py)

The consolidated FakeToken is a frozen dataclass that inherits from
AddressComparable, providing address-based equality, hashing, and
ordering that matches Erc20Token's behavior. It is interoperable with
Erc20Token instances (equality by address, hash compatibility, dict-key
interchangeability).
"""

from dataclasses import dataclass
from typing import TYPE_CHECKING

from degenbot.types.address_comparable import AddressComparable

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress


@dataclass(frozen=True, eq=False)
class FakeToken(AddressComparable):
    """
    Lightweight token stand-in for testing.

    Frozen dataclass inheriting from AddressComparable for consistent
    address-based equality, hashing, and ordering with Erc20Token.
    Production pool constructors only access .address, .chain_id,
    .decimals, .symbol, and .name — all provided here.
    """

    address: "ChecksumAddress"
    name: str = "Token"
    symbol: str = "TKN"
    decimals: int = 18
    chain_id: int = 1

    def __str__(self) -> str:
        return self.symbol

    def __repr__(self) -> str:
        return f"FakeToken({self.address}, symbol={self.symbol!r})"
