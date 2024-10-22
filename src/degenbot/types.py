from dataclasses import dataclass
from typing import Any, Protocol, TypeAlias

from eth_typing import ChecksumAddress
from eth_utils.address import to_checksum_address
from hexbytes import HexBytes
from typing_extensions import Self


class Message:
    """
    A message sent by a `Publisher`
    """


class PlaintextMessage(Message):
    def __init__(self, text: str) -> None:
        self.text = text

    def __str__(self) -> str:
        return self.text


class Publisher(Protocol):
    """
    Can send a `Message` to a `Subscriber`
    """

    _subscribers: set["Subscriber"]

    def subscribe(self, subscriber: "Subscriber") -> None:
        """
        Subscribe to receive messages from this `Publisher`
        """

    def unsubscribe(self, subscriber: "Subscriber") -> None:
        """
        Stop receiving messages from this `Publisher`
        """


class Subscriber(Protocol):
    """
    Can receive messages from a `Publisher`
    """

    def notify(self, publisher: "Publisher", message: "Message") -> None:
        """
        Deliver `message` to `Subscriber`
        """


class AbstractArbitrage(Publisher, Subscriber):
    id: str

    def _notify_subscribers(self: Publisher, message: Message) -> None:
        for subscriber in self._subscribers:
            subscriber.notify(publisher=self, message=message)

    def subscribe(self: Publisher, subscriber: Subscriber) -> None:
        self._subscribers.add(subscriber)

    def unsubscribe(self: Publisher, subscriber: Subscriber) -> None:
        self._subscribers.discard(subscriber)


@dataclass(slots=True, frozen=True)
class AbstractExchangeDeployment:
    name: str
    chain_id: int


class AbstractManager:
    """
    Base class for managers that generate, track and distribute various helper classes
    """


class AbstractPoolManager:
    """
    Base class for liquidity pool managers. The class instance dict and get_instance method are
    mechanisms for implementing a singleton strategy so only one pool manager is created for a given
    DEX factory.
    """

    # All pool managers are associated with certain types of pools. Specifying them as a class-level
    # alias allows the type hints to be generalized. Each concrete pool manager should set this
    # class attribute
    Pool: TypeAlias = "AbstractLiquidityPool"

    instances: dict[
        tuple[int, ChecksumAddress],
        Self,
    ] = dict()

    @classmethod
    def get_instance(cls, factory_address: str, chain_id: int) -> Self | None:
        return cls.instances.get((chain_id, to_checksum_address(factory_address)))


class AbstractPoolUpdate: ...


@dataclass(slots=True, frozen=True)
class AbstractPoolState:
    pool: ChecksumAddress


class AbstractSimulationResult: ...


class AbstractLiquidityPool(Publisher):
    address: ChecksumAddress
    name: str

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


class AbstractRegistry: ...


class AbstractTransaction: ...
