import abc
import dataclasses
from collections.abc import Sequence
from typing import TYPE_CHECKING, Any, Protocol

from eth_typing import ChecksumAddress
from hexbytes import HexBytes

if TYPE_CHECKING:
    from .erc20_token import Erc20Token


class Message:
    """
    A message sent from a `Publisher` to a `Subscriber`
    """


class PlaintextMessage(Message):
    def __init__(self, text: str) -> None:
        self.text = text

    def __str__(self) -> str:
        return self.text


class Publisher(Protocol):
    """
    Can publish updates and accept subscriptions.
    """

    _subscribers: set["Subscriber"]


class Subscriber(Protocol):
    """
    Can be notified via the `notify()` method
    """

    @abc.abstractmethod
    def notify(self, publisher: "Publisher", message: "Message") -> None:
        """
        Deliver `message` from `publisher`.
        """


class AbstractArbitrage:
    id: str
    swap_pools: Sequence[Any]

    def _notify_subscribers(self: Publisher, message: Message) -> None:
        for subscriber in self._subscribers:
            subscriber.notify(publisher=self, message=message)

    def subscribe(self: Publisher, subscriber: Subscriber) -> None:
        self._subscribers.add(subscriber)

    def unsubscribe(self: Publisher, subscriber: Subscriber) -> None:
        self._subscribers.discard(subscriber)


class AbstractManager:
    """
    Base class for managers that generate, track and distribute various helper classes
    """


class AbstractPoolUpdate: ...


class AbstractPoolState:
    pool: ChecksumAddress


class AbstractSimulationResult: ...


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapSimulationResult(AbstractSimulationResult):
    amount0_delta: int
    amount1_delta: int
    initial_state: AbstractPoolState
    final_state: AbstractPoolState


class AbstractLiquidityPool(abc.ABC, Publisher):
    address: ChecksumAddress
    name: str
    tokens: Sequence["Erc20Token"]
    _subscribers: set[Subscriber]

    def __eq__(self, other: Any) -> bool:
        match other:
            case AbstractLiquidityPool():
                return self.address == other.address
            case HexBytes():
                return self.address.lower() == other.to_0x_hex().lower()
            case bytes():
                return self.address.lower() == "0x" + other.hex().lower()
            case str():
                return self.address.lower() == other.lower()
            case _:
                return NotImplemented

    def __lt__(self, other: Any) -> bool:
        match other:
            case AbstractLiquidityPool():
                return self.address < other.address
            case HexBytes():
                return self.address.lower() < other.to_0x_hex().lower()
            case bytes():
                return self.address.lower() < "0x" + other.hex().lower()
            case str():
                return self.address.lower() < other.lower()
            case _:
                return NotImplemented

    def __gt__(self, other: Any) -> bool:
        match other:
            case AbstractLiquidityPool():
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
        return hash(self.address)

    def __str__(self) -> str:
        return self.name

    def _notify_subscribers(self: Publisher, message: Message) -> None:
        for subscriber in self._subscribers:
            subscriber.notify(self, message)

    def get_arbitrage_helpers(self: Publisher) -> list[AbstractArbitrage]:
        return [
            subscriber
            for subscriber in self._subscribers
            if isinstance(subscriber, AbstractArbitrage)
        ]

    def subscribe(self: Publisher, subscriber: Subscriber) -> None:
        self._subscribers.add(subscriber)

    def unsubscribe(self: Publisher, subscriber: Subscriber) -> None:
        self._subscribers.discard(subscriber)


class AbstractErc20Token:
    address: ChecksumAddress
    symbol: str
    name: str
    decimals: int

    def __eq__(self, other: Any) -> bool:
        match other:
            case AbstractErc20Token():
                return self.address == other.address
            case HexBytes():
                return self.address.lower() == other.to_0x_hex().lower()
            case bytes():
                return self.address.lower() == "0x" + other.hex().lower()
            case str():
                return self.address.lower() == other.lower()
            case _:
                return NotImplemented

    def __lt__(self, other: Any) -> bool:
        match other:
            case AbstractErc20Token():
                return self.address < other.address
            case HexBytes():
                return self.address.lower() < other.to_0x_hex().lower()
            case bytes():
                return self.address.lower() < "0x" + other.hex().lower()
            case str():
                return self.address.lower() < other.lower()
            case _:
                return NotImplemented

    def __gt__(self, other: Any) -> bool:
        match other:
            case AbstractErc20Token():
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
        return hash(self.address)

    def __str__(self) -> str:
        return self.symbol


class AbstractTransaction: ...
