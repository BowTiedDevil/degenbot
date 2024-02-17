import dataclasses
import abc
from typing import TYPE_CHECKING, Any, Iterator, Sequence

from eth_typing import ChecksumAddress

if TYPE_CHECKING:
    from .erc20_token import Erc20Token


class BaseArbitrage:
    id: str
    gas_estimate: int
    swap_pools: Sequence["BaseLiquidityPool"]


class BaseManager:
    """
    Base class for managers that generate, track and distribute various helper classes
    """

    ...


class BasePoolUpdate:
    ...


class BasePoolState:
    pool: "BaseLiquidityPool"


class BaseSimulationResult:
    ...


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapSimulationResult(BaseSimulationResult):
    amount0_delta: int
    amount1_delta: int
    current_state: BasePoolState
    future_state: BasePoolState


class BaseLiquidityPool(abc.ABC):
    address: ChecksumAddress
    name: str
    state: BasePoolState
    tokens: Sequence["Erc20Token"]

    def __eq__(self, other: Any) -> bool:
        if isinstance(other, BaseLiquidityPool):
            return self.address == other.address
        elif isinstance(other, bytes):
            return self.address.lower() == other.hex().lower()
        elif isinstance(other, str):
            return self.address.lower() == other.lower()
        else:
            return NotImplemented

    def __lt__(self, other: Any) -> bool:
        if isinstance(other, BaseLiquidityPool):
            return self.address < other.address
        elif isinstance(other, bytes):
            return self.address.lower() < other.hex().lower()
        elif isinstance(other, str):
            return self.address.lower() < other.lower()
        else:
            return NotImplemented

    def __gt__(self, other: Any) -> bool:
        if isinstance(other, BaseLiquidityPool):
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

    @abc.abstractmethod
    def subscribe(self, subscriber: Any) -> None:
        ...

    @abc.abstractmethod
    def get_arbitrage_helpers(self) -> Iterator[BaseArbitrage]:
        ...


class BaseToken:
    address: ChecksumAddress
    symbol: str
    name: str
    decimals: int

    def __eq__(self, other: Any) -> bool:
        if isinstance(other, BaseToken):
            return self.address == other.address
        elif isinstance(other, bytes):
            return self.address.lower() == other.hex().lower()
        elif isinstance(other, str):
            return self.address.lower() == other.lower()
        else:
            return NotImplemented

    def __lt__(self, other: Any) -> bool:
        if isinstance(other, BaseToken):
            return self.address < other.address
        elif isinstance(other, bytes):
            return self.address.lower() < other.hex().lower()
        elif isinstance(other, str):
            return self.address.lower() < other.lower()
        else:
            return NotImplemented

    def __gt__(self, other: Any) -> bool:
        if isinstance(other, BaseToken):
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
        return self.symbol


class BaseTransaction:
    ...
