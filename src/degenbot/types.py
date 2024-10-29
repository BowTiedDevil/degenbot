from collections import OrderedDict
from dataclasses import dataclass
from typing import Any, ClassVar, Protocol, TypeAlias, TypeVar

from eth_typing import ChecksumAddress
from eth_utils.address import to_checksum_address
from hexbytes import HexBytes
from typing_extensions import Self


class Message:
    """
    A message sent by a `Publisher` to a `Subscriber`
    """


class TextMessage(Message):
    def __init__(self, text: str) -> None:
        self.text = text

    def __eq__(self, other: object) -> bool:
        if not isinstance(other, TextMessage):
            return NotImplemented
        return self.text == other.text

    def __repr__(self) -> str:
        return f"PlaintextMessage(text={self.text})"

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

    instances: ClassVar[
        dict[
            tuple[int, ChecksumAddress],
            Self,
        ]
    ] = {}

    @classmethod
    def get_instance(cls, factory_address: str, chain_id: int) -> Self | None:
        return cls.instances.get((chain_id, to_checksum_address(factory_address)))


class AbstractPoolUpdate: ...


@dataclass(slots=True, frozen=True)
class AbstractPoolState:
    pool: ChecksumAddress


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


KT = TypeVar("KT")
VT = TypeVar("VT")


class BoundedCache(OrderedDict[KT, VT]):
    """
    A cache holding key-value pairs, tracked by entry order. The cache automatically removes old
    items if the number of items would exceed the maximum.
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
        if len(self) >= self.max_items:
            expired_key, expired_value = self.popitem(last=False)
            print(f"Evicted key={expired_key}, value={expired_value} from cache")

        super().__setitem__(key, value)
