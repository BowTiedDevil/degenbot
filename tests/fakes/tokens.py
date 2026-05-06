"""Consolidated fake token implementations.

Replaces the previous ad hoc token fakes:
- FakeToken (from tests/arbitrage/test_path/conftest.py)
- FakeCurveToken (from tests/arbitrage/fake_curve_pool.py)
- MockErc20Token (from tests/arbitrage/mock_pools.py)
- MockToken (from tests/arbitrage/test_optimizers/test_v2_v3_optimizer.py)

The consolidated FakeToken is a frozen dataclass with address-based equality
that is interoperable with any object having an ``address`` attribute (including
Erc20Token instances and other FakeToken variants).
"""

from dataclasses import dataclass
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress


@dataclass(frozen=True)
class FakeToken:
    """Lightweight token stand-in for testing.

    Frozen dataclass ensuring hashability and equality by address.
    Interoperable with any object exposing an ``address`` attribute
    (Erc20Token, other FakeToken variants, etc.).
    """

    address: "ChecksumAddress"
    symbol: str = "TKN"
    decimals: int = 18
    chain_id: int = 1

    def __hash__(self) -> int:
        return hash(self.address)

    def __eq__(self, other: object) -> bool:
        if hasattr(other, "address"):
            return self.address.lower() == other.address.lower()
        return NotImplemented

    def __str__(self) -> str:
        return f"{self.symbol} ({self.address[:10]}...)"

    def __repr__(self) -> str:
        return f"FakeToken({self.address}, symbol={self.symbol!r})"
