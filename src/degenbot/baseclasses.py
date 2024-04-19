import abc
import dataclasses
from typing import TYPE_CHECKING, Any, Iterator, Protocol, Sequence, Set

from eth_typing import ChecksumAddress

if TYPE_CHECKING:
    from .erc20_token import Erc20Token


class Message:
    """
    A message sent from a `Publisher` to a `Subscriber`
    """


class Publisher(Protocol):
    """
    Can publish updates and accept subscriptions.
    """

    _subscribers: Set["Subscriber"]


class Subscriber(Protocol):
    """
    Can be notified via the `notify()` method
    """

    @abc.abstractmethod
    def notify(self, publisher: "Publisher", message: "Message") -> None:
        """
        Deliver `message` from `publisher`.
        """


class BaseArbitrage:
    id: str
    gas_estimate: int
    swap_pools: Sequence["BaseLiquidityPool"]


class BaseManager:
    """
    Base class for managers that generate, track and distribute various helper classes
    """


class BasePoolUpdate: ...


class BasePoolState:
    pool: ChecksumAddress


class BaseSimulationResult: ...


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapSimulationResult(BaseSimulationResult):
    amount0_delta: int
    amount1_delta: int
    initial_state: BasePoolState
    final_state: BasePoolState


class BaseLiquidityPool(abc.ABC, Publisher):
    address: ChecksumAddress
    name: str
    state: BasePoolState
    tokens: Sequence["Erc20Token"]
    _subscribers: Set[Subscriber]

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

    def _notify_subscribers(self: Publisher, message: Message) -> None:
        for subscriber in self._subscribers:
            subscriber.notify(self, message)

    def get_arbitrage_helpers(self: Publisher) -> Iterator[BaseArbitrage]:
        return (
            subscriber
            for subscriber in self._subscribers
            if isinstance(subscriber, (BaseArbitrage))
        )

    def subscribe(self: Publisher, subscriber: Subscriber) -> None:
        self._subscribers.add(subscriber)

    def unsubscribe(self: Publisher, subscriber: Subscriber) -> None:
        self._subscribers.discard(subscriber)


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


class BaseTransaction: ...
