from collections import OrderedDict
from collections.abc import Sequence
from dataclasses import dataclass
from typing import Any, ClassVar, Protocol, Self
from weakref import WeakSet

from eth_typing import BlockNumber, ChecksumAddress
from hexbytes import HexBytes

from degenbot.cache import get_checksum_address


class Message:
    """
    A message sent by a `Publisher` to a `Subscriber`
    """


class PoolStateMessage(Message):
    state: "AbstractPoolState"


class TextMessage(Message):
    def __init__(self, text: str) -> None:
        self.text = text

    def __eq__(self, other: object) -> bool:
        if not isinstance(other, TextMessage):
            return NotImplemented
        return self.text == other.text

    def __repr__(self) -> str:
        return f"{type(self).__name__}(text={self.text})"

    def __str__(self) -> str:
        return self.text


class Publisher(Protocol):
    """
    Can send a `Message` to a `Subscriber`
    """

    _subscribers: WeakSet["Subscriber"]

    def subscribe(self, subscriber: "Subscriber") -> None:
        """
        Subscribe to receive messages from this `Publisher`
        """

    def unsubscribe(self, subscriber: "Subscriber") -> None:
        """
        Stop receiving messages from this `Publisher`
        """


class PublisherMixin:
    """
    A set of default methods to accept subscribe & unsubscribe requests. Classes using this mixin
    meet the `Publisher` protocol requirements.
    """

    def subscribe(self: Publisher, subscriber: "Subscriber") -> None:
        self._subscribers.add(subscriber)

    def unsubscribe(self: Publisher, subscriber: "Subscriber") -> None:
        self._subscribers.discard(subscriber)


class Subscriber(Protocol):
    """
    Can subscribe to messages from a `Publisher`
    """

    def notify(self, publisher: Publisher, message: Message) -> None:
        """
        Deliver `message` to `Subscriber`
        """


class AbstractArbitrage:
    id: str
    swap_pools: Sequence["AbstractLiquidityPool"]


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
    type Pool = "AbstractLiquidityPool"

    instances: ClassVar[
        dict[
            tuple[int, ChecksumAddress],
            Self,
        ]
    ] = {}

    @classmethod
    def get_instance(cls, factory_address: str, chain_id: int) -> Self | None:
        return cls.instances.get((chain_id, get_checksum_address(factory_address)))


class AbstractPoolUpdate: ...


@dataclass(slots=True, frozen=True, kw_only=True)
class AbstractPoolState:
    address: ChecksumAddress
    block: BlockNumber | None


class AbstractSimulationResult: ...


class AbstractLiquidityPool:
    address: ChecksumAddress
    name: str

    def __eq__(self, other: object) -> bool:
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


class AbstractErc20Token:
    address: ChecksumAddress
    symbol: str
    name: str
    decimals: int

    def __eq__(self, other: object) -> bool:
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


class BoundedCache[KT, VT](OrderedDict[KT, VT]):
    """
    A cache holding key-value pairs, tracked by entry order. The cache automatically removes old
    items if the number of items would exceed the maximum number of entries set by `max_items`.

    Setting a value at an existing key will overwrite that value without affecting ordering.
    """

    def __init__(self, max_items: int) -> None:
        super().__init__()
        self.max_items = max_items

    def __reduce__(self) -> tuple[Any, ...]:
        state = super().__reduce__()
        return (
            state[0],
            (self.max_items,),  # max_items argument must be provided to properly unpickle
            None,
            None,
            state[4],
        )

    def __setitem__(self, key: KT, value: VT) -> None:
        super().__setitem__(key, value)
        if len(self) > self.max_items:
            self.popitem(last=False)

    def copy(self) -> Self:
        new_copy = self.__class__(max_items=self.max_items)
        for k, v in self.items():
            new_copy[k] = v
        return new_copy
