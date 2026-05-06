from __future__ import annotations

from abc import ABC, abstractmethod
from typing import TYPE_CHECKING, Any

from eth_typing import ChecksumAddress

from degenbot.types.address_comparable import AddressComparable

if TYPE_CHECKING:
    from fractions import Fraction

    from degenbot.erc20.erc20 import Erc20Token
    from degenbot.types.abstract.pool_state import AbstractPoolState
    from degenbot.types.pool_protocols import SimulationResult


class AbstractLiquidityPool(AddressComparable, ABC):
    address: ChecksumAddress
    name: str

    @property
    @abstractmethod
    def tokens(self) -> tuple[Erc20Token, ...]: ...

    @abstractmethod
    def simulate_swap(
        self,
        token_in: ChecksumAddress,
        amount_in: int,
        token_out: ChecksumAddress,
        state_override: AbstractPoolState | None = None,
    ) -> SimulationResult: ...

    def __str__(self) -> str:
        return self.name


class AbstractUniswapV2Pool(AbstractLiquidityPool, ABC):
    """
    Abstract base class for Uniswap V2-like constant product pools with directional fees.

    See abstract properties for the required interface.
    """

    @property
    @abstractmethod
    def token0(self) -> Erc20Token: ...

    @property
    @abstractmethod
    def token1(self) -> Erc20Token: ...

    @property
    @abstractmethod
    def fee_token0(self) -> Fraction: ...

    @property
    @abstractmethod
    def fee_token1(self) -> Fraction: ...

    @property
    @abstractmethod
    def state(self) -> AbstractPoolState: ...

    @property
    @abstractmethod
    def reserves_token0(self) -> int: ...

    @property
    @abstractmethod
    def reserves_token1(self) -> int: ...


class AbstractConcentratedLiquidityPool(AbstractLiquidityPool, ABC):
    """
    Abstract base class for concentrated liquidity pools (Uniswap V3/V4).

    See abstract properties for the required interface.
    """

    @property
    @abstractmethod
    def token0(self) -> Erc20Token: ...

    @property
    @abstractmethod
    def token1(self) -> Erc20Token: ...

    @property
    @abstractmethod
    def fee(self) -> int: ...

    @property
    @abstractmethod
    def liquidity(self) -> int: ...

    @property
    @abstractmethod
    def sqrt_price_x96(self) -> int: ...

    @property
    @abstractmethod
    def tick(self) -> int: ...

    @property
    @abstractmethod
    def tick_spacing(self) -> int: ...

    @property
    @abstractmethod
    def tick_bitmap(self) -> dict[int, Any]: ...

    @property
    @abstractmethod
    def tick_data(self) -> dict[int, Any]: ...

    @property
    @abstractmethod
    def sparse_liquidity_map(self) -> bool: ...

    @property
    @abstractmethod
    def state(self) -> AbstractPoolState: ...


class AbstractAerodromeV2Pool(AbstractLiquidityPool, ABC):
    """
    Abstract base class for Aerodrome V2 pools.

    See abstract properties for the required interface.
    """

    @property
    @abstractmethod
    def token0(self) -> Erc20Token: ...

    @property
    @abstractmethod
    def token1(self) -> Erc20Token: ...

    @property
    @abstractmethod
    def fee(self) -> Fraction: ...

    @property
    @abstractmethod
    def stable(self) -> bool: ...

    @property
    @abstractmethod
    def state(self) -> AbstractPoolState: ...

    @property
    @abstractmethod
    def reserves_token0(self) -> int: ...

    @property
    @abstractmethod
    def reserves_token1(self) -> int: ...
